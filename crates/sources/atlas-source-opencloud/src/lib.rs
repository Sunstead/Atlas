//! OpenCloud, read from disk. OpenCloud stores personal spaces as plain
//! files (PosixFS) under `data/files/users/<username>`, so Atlas indexes a
//! user's space in place, read-only, without OpenCloud's API or a token.
//!
//! With the user's app token (optional; Basic auth `username:token`, which
//! OpenCloud accepts when `PROXY_ENABLE_APP_AUTH` is on), Atlas also uses the
//! API: thumbnails and previews from OpenCloud's own renderer
//! (`/dav/spaces/<space>/<path>?preview=1`), and the space's ids from Graph
//! (`/graph/v1.0/me/drives`) when they aren't on disk.
//!
//! Item ids are paths relative to the user's root, `/` separated. They change
//! when a file is renamed or moved, which a sync sees as a delete and an add.
//!
//! Links go straight to the file, like OpenCloud's own permalinks
//! (`/f/<storage>$<space>!<file>`): PosixFS keeps each file's id in its
//! `user.oc.id` attribute and the space's in the root's `user.oc.space.id`,
//! so no API call is needed. The storage id is the same for every file on a
//! server (`ATLAS_OPENCLOUD_STORAGE_ID`, from any permalink, or from Graph
//! with a token). Without those, links go to the containing folder.
//!
//! The sync runs on a blocking thread (the server's indexer uses
//! `spawn_blocking`), so walking and reading files here is fine.

use async_trait::async_trait;
use atlas_core::{
    Blob, BlobBody, BlobVariant, Capabilities, Doc, Indexed, IndexSink, Known, Preview, Result, Source, SourceError,
    SyncMode, SyncReport,
};
use atlas_fs::{Entry, TextKind};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How much of a text file a preview shows.
const PREVIEW_BYTES: usize = 64 * 1024;

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Rendition sizes asked of OpenCloud's thumbnail service.
const THUMB_PX: u32 = 256;
const PREVIEW_PX: u32 = 1600;

pub const KIND: &str = "opencloud";

pub struct OpenCloudSource {
    root: PathBuf,
    username: String,
    public_url: String,
    /// `<storage id>$<space id>`, when both are known: the prefix of every
    /// permalink in this space. Filled from Graph later if a token allows.
    space_ref: RwLock<Option<String>>,
    /// The space's own id, for WebDAV paths.
    space_id: RwLock<Option<String>>,
    api: Option<Api>,
    token: Option<String>,
}

/// OpenCloud's API, as the user (their app token).
struct Api {
    base: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct Drives {
    value: Vec<Drive>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Drive {
    id: String,
    drive_type: String,
}

/// Whether OpenCloud at `base` accepts `username:token`, before a connection
/// is saved.
pub async fn check_token(base: &str, username: &str, token: &str) -> Result<()> {
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build().map_err(|e| SourceError::Other(e.to_string()))?;
    let res = http
        .get(format!("{}/graph/v1.0/me", base.trim_end_matches('/')))
        .basic_auth(username, Some(token.trim()))
        .send()
        .await
        .map_err(|e| SourceError::Unavailable(format!("OpenCloud: {e}")))?;
    if !res.status().is_success() {
        return Err(http_error("check the token", res.status()));
    }
    Ok(())
}

fn http_error(what: &str, status: reqwest::StatusCode) -> SourceError {
    match status.as_u16() {
        401 => SourceError::Config(
            "OpenCloud didn't accept the app token. It may have expired; make a new one and save it in Atlas.".into(),
        ),
        404 => SourceError::NotFound,
        _ => SourceError::Unavailable(format!("OpenCloud: {what}: HTTP {status}")),
    }
}

/// OpenCloud's ids are UUIDs; anything else isn't put in a link.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// An OpenCloud permalink: `/f/<storage>$<space>!<file>`, with `!` encoded
/// as OpenCloud does.
fn permalink(public_url: &str, space_ref: &str, file_id: &str) -> String {
    format!("{public_url}/f/{space_ref}%21{file_id}")
}

impl OpenCloudSource {
    /// `root` is the user's space, pinned when they connected; it must sit
    /// inside `users_dir`, even after resolving symlinks.
    /// `storage_id` is OpenCloud's storage provider id, for file links.
    pub fn new(users_dir: &Path, root: &Path, public_url: &str, storage_id: Option<&str>) -> Result<Self> {
        let not_there = || {
            SourceError::Config(
                "Your OpenCloud folder isn't on the server yet. Sign in to OpenCloud once, then sync again.".into(),
            )
        };
        let canon_users = users_dir.canonicalize().map_err(|e| SourceError::Unavailable(format!("{}: {e}", users_dir.display())))?;
        let canon_root = root.canonicalize().map_err(|_| not_there())?;
        if !canon_root.starts_with(&canon_users) || canon_root == canon_users {
            return Err(SourceError::Config("This connection's folder is outside OpenCloud's spaces".into()));
        }
        if !canon_root.is_dir() {
            return Err(not_there());
        }
        let username = root.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let space_id = atlas_fs::xattr(root, "user.oc.space.id").filter(|s| is_uuid(s));
        let space_ref = match (storage_id.filter(|s| is_uuid(s)), &space_id) {
            (Some(storage), Some(space)) => Some(format!("{storage}${space}")),
            _ => None,
        };
        Ok(Self {
            root: root.to_path_buf(),
            username,
            public_url: public_url.trim_end_matches('/').to_owned(),
            space_ref: RwLock::new(space_ref),
            space_id: RwLock::new(space_id),
            api: None,
            token: None,
        })
    }

    /// Uses OpenCloud's API at `base` (`http://opencloud:9200`) with the
    /// user's app token.
    pub fn with_api(mut self, base: &str, token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| SourceError::Other(e.to_string()))?;
        // reqwest's per-request basic_auth marks the header sensitive.
        self.api = Some(Api { base: base.trim_end_matches('/').to_owned(), http });
        self.token = Some(token.trim().to_owned());
        Ok(self)
    }

    fn get(&self, url: String) -> Option<reqwest::RequestBuilder> {
        let api = self.api.as_ref()?;
        Some(api.http.get(url).basic_auth(&self.username, self.token.as_deref()))
    }

    /// Fills in the space's ids from Graph, if they weren't on disk.
    async fn resolve_space(&self) -> Result<()> {
        let known = self.space_ref.read().unwrap_or_else(|p| p.into_inner()).is_some()
            && self.space_id.read().unwrap_or_else(|p| p.into_inner()).is_some();
        let Some(api) = &self.api else { return Ok(()) };
        if known {
            return Ok(());
        }
        let req = self.get(format!("{}/graph/v1.0/me/drives", api.base)).expect("api is set");
        let res = req.send().await.map_err(|e| SourceError::Unavailable(format!("OpenCloud: {e}")))?;
        if !res.status().is_success() {
            return Err(http_error("list spaces", res.status()));
        }
        let drives: Drives = res.json().await.map_err(|e| SourceError::Unavailable(format!("OpenCloud: {e}")))?;
        let personal = drives.value.into_iter().find(|d| d.drive_type == "personal");
        if let Some((storage, space)) = personal.as_ref().and_then(|d| d.id.split_once('$')) {
            if is_uuid(storage) && is_uuid(space) {
                let mut r = self.space_ref.write().unwrap_or_else(|p| p.into_inner());
                if r.is_none() {
                    *r = Some(format!("{storage}${space}"));
                }
                let mut s = self.space_id.write().unwrap_or_else(|p| p.into_inner());
                if s.is_none() {
                    *s = Some(space.to_owned());
                }
            }
        }
        Ok(())
    }

    /// OpenCloud's rendition of an image, `px` on its longest side.
    async fn rendition(&self, entry: &Entry, px: u32) -> Result<Blob> {
        let Some(api) = &self.api else { return Err(SourceError::NotFound) };
        self.resolve_space().await?;
        let space = self.space_id.read().unwrap_or_else(|p| p.into_inner()).clone().ok_or(SourceError::NotFound)?;
        let url = format!("{}/dav/spaces/{space}/{}?preview=1&x={px}&y={px}&a=1", api.base, encode_path(&entry.id));
        let res = self.get(url).expect("api is set").send().await.map_err(|e| SourceError::Unavailable(format!("OpenCloud: {e}")))?;
        if !res.status().is_success() {
            return Err(http_error("thumbnail", res.status()));
        }
        let mime = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .filter(|m| m.starts_with("image/"))
            .ok_or_else(|| SourceError::Unavailable("OpenCloud: the thumbnail isn't an image".into()))?
            .to_owned();
        let bytes = res.bytes().await.map_err(|e| SourceError::Unavailable(format!("OpenCloud: {e}")))?;
        Ok(Blob { mime, len: Some(bytes.len() as u64), body: BlobBody::Bytes(bytes.to_vec()) })
    }

    fn doc(&self, entry: &Entry, with_text: bool) -> Doc {
        let (title, folder) = match entry.id.rsplit_once('/') {
            Some((folder, name)) => (name.to_owned(), Some(folder.to_owned())),
            None => (entry.id.clone(), None),
        };
        let (mime, body) = if with_text {
            match atlas_fs::extract(&entry.path) {
                Ok(x) => (x.mime, x.text),
                Err(e) => {
                    tracing::debug!(id = %entry.id, error = %e, "can't read for indexing");
                    (mime_of(&entry.path), None)
                }
            }
        } else {
            (mime_of(&entry.path), None)
        };
        Doc {
            external_id: entry.id.clone(),
            kind: "file".into(),
            title,
            path: folder,
            mime: Some(mime),
            size: Some(entry.size),
            mtime: Some(entry.mtime),
            fingerprint: Some(entry.fingerprint()),
            body,
        }
    }

    fn entry(&self, id: &str) -> Result<Entry> {
        atlas_fs::entry(&self.root, id).map_err(SourceError::from)
    }
}

fn mime_of(path: &Path) -> String {
    atlas_fs::mime_for(path)
}

/// URL-encodes each segment of a `/` path.
fn encode_path(path: &str) -> String {
    path.split('/').map(|s| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")).collect::<Vec<_>>().join("/")
}

#[async_trait]
impl Indexed for OpenCloudSource {
    async fn sync(
        &self,
        mode: SyncMode,
        known: &Known,
        sink: &mut dyn IndexSink,
        cancel: &CancellationToken,
    ) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        if let Err(e) = self.resolve_space().await {
            tracing::debug!(error = %e, "can't read the space's ids from OpenCloud; links go to folders");
        }
        match mode {
            SyncMode::Full => {
                if !self.root.is_dir() {
                    return Err(SourceError::Unavailable(format!("{} is missing", self.root.display())));
                }
                let mut seen = std::collections::HashSet::new();
                for entry in atlas_fs::walk(&self.root) {
                    if cancel.is_cancelled() {
                        return Err(SourceError::Cancelled);
                    }
                    report.seen += 1;
                    let fingerprint = entry.fingerprint();
                    let unchanged = known.get(&entry.id).is_some_and(|f| f.as_deref() == Some(fingerprint.as_str()));
                    seen.insert(entry.id.clone());
                    if !unchanged {
                        sink.upsert(self.doc(&entry, true))?;
                        report.upserted += 1;
                    }
                }
                for id in known.keys().filter(|id| !seen.contains(*id)) {
                    sink.delete(id)?;
                    report.deleted += 1;
                }
            }
            SyncMode::Items(ids) => {
                for id in ids {
                    if cancel.is_cancelled() {
                        return Err(SourceError::Cancelled);
                    }
                    match self.entry(&id) {
                        Ok(entry) => {
                            report.seen += 1;
                            let fingerprint = entry.fingerprint();
                            if known.get(&id).is_none_or(|f| f.as_deref() != Some(fingerprint.as_str())) {
                                sink.upsert(self.doc(&entry, true))?;
                                report.upserted += 1;
                            }
                        }
                        Err(SourceError::NotFound) => {
                            if known.contains_key(&id) {
                                sink.delete(&id)?;
                                report.deleted += 1;
                            }
                        }
                        Err(e) => {
                            tracing::debug!(%id, error = %e, "can't read a changed file");
                            report.failed += 1;
                        }
                    }
                }
            }
        }
        Ok(report)
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }

    fn id_for_path(&self, path: &Path) -> Option<String> {
        atlas_fs::id_for_path(&self.root, path)
    }
}

#[async_trait]
impl Source for OpenCloudSource {
    fn capabilities(&self) -> Capabilities {
        Capabilities { indexed: true, federated: false }
    }

    /// With a token, that OpenCloud accepts it.
    async fn check(&self) -> Result<()> {
        match (&self.api, &self.token) {
            (Some(api), Some(token)) => check_token(&api.base, &self.username, token).await,
            _ => Ok(()),
        }
    }

    async fn get(&self, id: &str) -> Result<Doc> {
        Ok(self.doc(&self.entry(id)?, false))
    }

    async fn preview(&self, id: &str) -> Result<Preview> {
        let entry = self.entry(id)?;
        let read = |entry: &Entry| -> Result<(String, bool)> {
            use std::io::Read;
            let mut buf = Vec::new();
            std::fs::File::open(&entry.path)?.take(PREVIEW_BYTES as u64 + 1).read_to_end(&mut buf)?;
            let truncated = buf.len() > PREVIEW_BYTES;
            buf.truncate(PREVIEW_BYTES);
            Ok((String::from_utf8_lossy(&buf).into_owned(), truncated))
        };
        Ok(match atlas_fs::kind_for(&entry.path) {
            TextKind::Markdown => {
                let (text, truncated) = read(&entry)?;
                Preview::Markdown { text, truncated }
            }
            TextKind::Plain => {
                let (text, truncated) = read(&entry)?;
                Preview::Text { text, truncated }
            }
            TextKind::Image => Preview::Image,
            TextKind::Pdf => Preview::Pdf,
            TextKind::Video => Preview::Video,
            TextKind::Audio => Preview::Audio,
            TextKind::Opaque => Preview::None,
        })
    }

    async fn blob(&self, id: &str, variant: BlobVariant) -> Result<Blob> {
        let entry = self.entry(id)?;
        let image = atlas_fs::kind_for(&entry.path) == TextKind::Image;
        let px = match variant {
            BlobVariant::Original => None,
            BlobVariant::Preview => Some(PREVIEW_PX),
            BlobVariant::Thumbnail => Some(THUMB_PX),
        };
        if let Some(px) = px {
            if !image {
                return Err(SourceError::NotFound);
            }
            // OpenCloud's rendition when there's a token; the original from
            // disk otherwise, or if that fails.
            if self.api.is_some() {
                match self.rendition(&entry, px).await {
                    Ok(b) => return Ok(b),
                    Err(e) => tracing::debug!(id = %entry.id, error = %e, "no OpenCloud rendition; serving the original"),
                }
            }
        }
        Ok(Blob { mime: mime_of(&entry.path), len: Some(entry.size), body: BlobBody::File(entry.path) })
    }

    /// Straight to the file when its id is on disk; otherwise its folder.
    fn deep_link(&self, doc: &Doc) -> Option<String> {
        if let Some(space_ref) = self.space_ref.read().unwrap_or_else(|p| p.into_inner()).as_deref() {
            let file_id = atlas_fs::resolve(&self.root, &doc.external_id)
                .ok()
                .and_then(|p| atlas_fs::xattr(&p, "user.oc.id"))
                .filter(|id| is_uuid(id));
            if let Some(id) = file_id {
                return Some(permalink(&self.public_url, space_ref, &id));
            }
        }
        let folder = doc.path.as_deref().map(|p| format!("/{}", encode_path(p))).unwrap_or_default();
        Some(format!("{}/files/spaces/personal/{}{}", self.public_url, encode_path(&self.username), folder))
    }

    /// Only with a token: without one, a "thumbnail" would be the original.
    fn has_thumbnail(&self, doc: &Doc) -> bool {
        self.api.is_some() && doc.mime.as_deref().is_some_and(|m| m.starts_with("image/"))
    }

    fn as_indexed(&self) -> Option<&dyn Indexed> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    #[derive(Default)]
    struct Sink {
        upserts: HashMap<String, Doc>,
        deletes: Vec<String>,
    }

    impl IndexSink for Sink {
        fn upsert(&mut self, doc: Doc) -> Result<()> {
            self.upserts.insert(doc.external_id.clone(), doc);
            Ok(())
        }
        fn delete(&mut self, id: &str) -> Result<()> {
            self.deletes.push(id.into());
            Ok(())
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        users: PathBuf,
        root: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users");
        let root = users.join("pwb");
        fs::create_dir_all(root.join("Documents/Taxes")).unwrap();
        fs::write(root.join("Documents/Taxes/2025.txt"), "refund in march").unwrap();
        fs::write(root.join("Documents/plan.md"), "# Plan\n\nBuild **Atlas**.").unwrap();
        fs::write(root.join("photo.jpg"), [0xff, 0xd8, 0xff]).unwrap();
        fs::create_dir_all(users.join("kim")).unwrap();
        fs::write(users.join("kim/private.txt"), "kim only").unwrap();
        Fixture { _dir: dir, users, root }
    }

    fn source(f: &Fixture) -> OpenCloudSource {
        OpenCloudSource::new(&f.users, &f.root, "https://opencloud.example/", None).unwrap()
    }

    async fn full(s: &OpenCloudSource, known: &Known) -> (SyncReport, Sink) {
        let mut sink = Sink::default();
        let r = s.sync(SyncMode::Full, known, &mut sink, &CancellationToken::new()).await.unwrap();
        (r, sink)
    }

    #[tokio::test]
    async fn a_full_sync_indexes_the_users_files_only() {
        let f = fixture();
        let (r, sink) = full(&source(&f), &Known::new()).await;
        assert_eq!(r.upserted, 3);
        let mut ids: Vec<_> = sink.upserts.keys().cloned().collect();
        ids.sort();
        assert_eq!(ids, ["Documents/Taxes/2025.txt", "Documents/plan.md", "photo.jpg"]);
        let tax = &sink.upserts["Documents/Taxes/2025.txt"];
        assert_eq!(tax.title, "2025.txt");
        assert_eq!(tax.path.as_deref(), Some("Documents/Taxes"));
        assert_eq!(tax.body.as_deref(), Some("refund in march"));
        assert!(sink.upserts["Documents/plan.md"].body.as_deref().unwrap().contains("Build Atlas."));
        assert_eq!(sink.upserts["photo.jpg"].body, None);
    }

    #[tokio::test]
    async fn unchanged_files_are_skipped_and_gone_ones_deleted() {
        let f = fixture();
        let s = source(&f);
        let (_, first) = full(&s, &Known::new()).await;
        let mut known: Known = first.upserts.iter().map(|(id, d)| (id.clone(), d.fingerprint.clone())).collect();
        known.insert("old/removed.txt".into(), Some("1:1".into()));
        let (r, sink) = full(&s, &known).await;
        assert_eq!(r.upserted, 0);
        assert_eq!(sink.deletes, ["old/removed.txt"]);
    }

    #[tokio::test]
    async fn item_syncs_upsert_changes_and_delete_removals() {
        let f = fixture();
        let s = source(&f);
        fs::write(f.root.join("new.txt"), "fresh").unwrap();
        fs::remove_file(f.root.join("photo.jpg")).unwrap();
        let known: Known = [("photo.jpg".to_string(), Some("3:1".to_string()))].into();
        let mut sink = Sink::default();
        let r = s
            .sync(SyncMode::Items(vec!["new.txt".into(), "photo.jpg".into()]), &known, &mut sink, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!((r.upserted, r.deleted), (1, 1));
        assert!(sink.upserts.contains_key("new.txt"));
        assert_eq!(sink.deletes, ["photo.jpg"]);
    }

    #[tokio::test]
    async fn a_cancelled_sync_stops() {
        let f = fixture();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let r = source(&f).sync(SyncMode::Full, &Known::new(), &mut Sink::default(), &cancel).await;
        assert!(matches!(r, Err(SourceError::Cancelled)));
    }

    #[test]
    fn the_root_must_be_inside_the_users_dir() {
        let f = fixture();
        assert!(OpenCloudSource::new(&f.users, &f.users, "https://x", None).is_err(), "the users dir itself");
        assert!(OpenCloudSource::new(&f.users, &f.users.join("../users/pwb/../.."), "https://x", None).is_err());
        let r = OpenCloudSource::new(&f.users, &f.users.join("nobody"), "https://x", None);
        assert!(matches!(r, Err(SourceError::Config(_))), "not there yet");
    }

    #[tokio::test]
    async fn previews_by_type_and_stays_inside_the_root() {
        let f = fixture();
        let s = source(&f);
        assert!(matches!(s.preview("Documents/plan.md").await.unwrap(), Preview::Markdown { .. }));
        assert_eq!(s.preview("Documents/Taxes/2025.txt").await.unwrap(), Preview::Text { text: "refund in march".into(), truncated: false });
        assert_eq!(s.preview("photo.jpg").await.unwrap(), Preview::Image);
        assert!(matches!(s.preview("../kim/private.txt").await, Err(SourceError::NotFound)));
        assert!(s.blob("../kim/private.txt", BlobVariant::Original).await.is_err());
        let blob = s.blob("photo.jpg", BlobVariant::Original).await.unwrap();
        assert_eq!(blob.mime, "image/jpeg");
        assert!(s.blob("Documents/plan.md", BlobVariant::Thumbnail).await.is_err());
    }

    // From a real Jupiter permalink.
    const STORAGE: &str = "8184153f-ca77-4fe9-8090-01063bcdbc65";
    const SPACE: &str = "854f8139-f074-409b-9b9e-6ed557c70bc7";
    const FILE: &str = "a1f06204-84e1-479a-842b-9ec785a663d2";

    #[test]
    fn permalinks_match_opencloud() {
        assert_eq!(
            permalink("https://opencloud.jupiter.sunstead.net", &format!("{STORAGE}${SPACE}"), FILE),
            "https://opencloud.jupiter.sunstead.net/f/8184153f-ca77-4fe9-8090-01063bcdbc65$854f8139-f074-409b-9b9e-6ed557c70bc7%21a1f06204-84e1-479a-842b-9ec785a663d2"
        );
        assert!(is_uuid(FILE));
        for bad in ["", "not-a-uuid", "8184153f-ca77-4fe9-8090-01063bcdbc6", "8184153f$ca77-4fe9-8090-01063bcdbc65z"] {
            assert!(!is_uuid(bad), "{bad}");
        }
    }

    /// Links straight to the file when the attributes are there. Skipped
    /// where user xattrs aren't supported (Windows, some tmpfs).
    #[cfg(unix)]
    #[test]
    fn links_to_the_file_from_its_attributes() {
        let f = fixture();
        let file = f.root.join("Documents/plan.md");
        if xattr::set(&f.root, "user.oc.space.id", SPACE.as_bytes()).is_err() {
            eprintln!("no user xattrs here; skipping");
            return;
        }
        xattr::set(&file, "user.oc.id", FILE.as_bytes()).unwrap();
        let s = OpenCloudSource::new(&f.users, &f.root, "https://oc.example", Some(STORAGE)).unwrap();
        let doc = s.doc(&atlas_fs::entry(&f.root, "Documents/plan.md").unwrap(), false);
        assert_eq!(s.deep_link(&doc).unwrap(), format!("https://oc.example/f/{STORAGE}${SPACE}%21{FILE}"));
        // A file without an id falls back to its folder.
        let doc = s.doc(&atlas_fs::entry(&f.root, "Documents/Taxes/2025.txt").unwrap(), false);
        assert_eq!(s.deep_link(&doc).unwrap(), "https://oc.example/files/spaces/personal/pwb/Documents/Taxes");
        // A junk id isn't put in a link.
        xattr::set(&file, "user.oc.id", b"../../evil").unwrap();
        let doc = s.doc(&atlas_fs::entry(&f.root, "Documents/plan.md").unwrap(), false);
        assert!(s.deep_link(&doc).unwrap().contains("/files/spaces/personal/pwb/Documents"));
    }

    #[test]
    fn without_a_storage_id_links_go_to_folders() {
        let f = fixture();
        let s = OpenCloudSource::new(&f.users, &f.root, "https://oc.example", None).unwrap();
        assert!(s.space_ref.read().unwrap().is_none());
    }

    #[test]
    fn links_to_the_containing_folder() {
        let f = fixture();
        let s = source(&f);
        let mut d = s.doc(&atlas_fs::entry(&f.root, "Documents/Taxes/2025.txt").unwrap(), false);
        assert_eq!(s.deep_link(&d).unwrap(), "https://opencloud.example/files/spaces/personal/pwb/Documents/Taxes");
        d.path = Some("My Files/a&b".into());
        assert_eq!(s.deep_link(&d).unwrap(), "https://opencloud.example/files/spaces/personal/pwb/My%20Files/a%26b");
        d.path = None;
        assert_eq!(s.deep_link(&d).unwrap(), "https://opencloud.example/files/spaces/personal/pwb");
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use axum::{
        extract::{Path as AxPath, RawQuery},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

    const STORAGE: &str = "8184153f-ca77-4fe9-8090-01063bcdbc65";
    const SPACE: &str = "854f8139-f074-409b-9b9e-6ed557c70bc7";

    /// `pwb:good token` in Basic auth.
    fn authorized(h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Basic cHdiOmdvb2QgdG9rZW4=")
    }

    /// A fake OpenCloud: Graph's me and drives, and WebDAV previews. Records
    /// each preview request's path and query.
    async fn opencloud(up: Arc<Mutex<bool>>, seen: Arc<Mutex<Vec<String>>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let up2 = up.clone();
        let app = Router::new()
            .route(
                "/graph/v1.0/me",
                get(|h: HeaderMap| async move {
                    if !authorized(&h) {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    Json(serde_json::json!({ "onPremisesSamAccountName": "pwb" })).into_response()
                }),
            )
            .route(
                "/graph/v1.0/me/drives",
                get(|h: HeaderMap| async move {
                    if !authorized(&h) {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    Json(serde_json::json!({ "value": [
                        { "id": "a0ca6a90-a365-4782-871e-d44447bbc668$a0ca6a90-a365-4782-871e-d44447bbc668", "driveType": "virtual" },
                        { "id": format!("{STORAGE}${SPACE}"), "driveType": "personal" }
                    ] }))
                    .into_response()
                }),
            )
            .route(
                "/dav/spaces/{*rest}",
                get(move |h: HeaderMap, AxPath(rest): AxPath<String>, RawQuery(q): RawQuery| {
                    let (up, seen) = (up2.clone(), seen.clone());
                    async move {
                        if !*up.lock().unwrap() {
                            return StatusCode::BAD_GATEWAY.into_response();
                        }
                        if !authorized(&h) {
                            return StatusCode::UNAUTHORIZED.into_response();
                        }
                        seen.lock().unwrap().push(format!("{rest}?{}", q.unwrap_or_default()));
                        ([("content-type", "image/png")], b"rendition".to_vec()).into_response()
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let _ = up;
        base
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        users: PathBuf,
        root: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users");
        let root = users.join("pwb");
        std::fs::create_dir_all(root.join("School/Chemistry")).unwrap();
        std::fs::write(root.join("School/Chemistry/Top Hat.png"), [0x89, b'P', b'N', b'G']).unwrap();
        std::fs::write(root.join("notes.txt"), "text").unwrap();
        Fixture { _dir: dir, users, root }
    }

    fn source(f: &Fixture, base: &str, token: &str) -> OpenCloudSource {
        OpenCloudSource::new(&f.users, &f.root, "https://oc.example", None).unwrap().with_api(base, token).unwrap()
    }

    #[tokio::test]
    async fn checks_the_token() {
        let f = fixture();
        let base = opencloud(Arc::new(Mutex::new(true)), Arc::default()).await;
        source(&f, &base, "good token").check().await.unwrap();
        assert!(matches!(source(&f, &base, "wrong").check().await, Err(SourceError::Config(_))));
        // Without a token there's nothing to check.
        OpenCloudSource::new(&f.users, &f.root, "https://oc.example", None).unwrap().check().await.unwrap();
    }

    #[tokio::test]
    async fn learns_the_space_from_graph() {
        let f = fixture();
        let base = opencloud(Arc::new(Mutex::new(true)), Arc::default()).await;
        let s = source(&f, &base, "good token");
        assert!(s.space_ref.read().unwrap().is_none());
        s.resolve_space().await.unwrap();
        assert_eq!(s.space_ref.read().unwrap().as_deref(), Some(format!("{STORAGE}${SPACE}").as_str()));
        assert_eq!(s.space_id.read().unwrap().as_deref(), Some(SPACE));
    }

    #[tokio::test]
    async fn thumbnails_come_from_opencloud_and_fall_back_to_the_original() {
        let f = fixture();
        let up = Arc::new(Mutex::new(true));
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let base = opencloud(up.clone(), seen.clone()).await;
        let s = source(&f, &base, "good token");
        let id = "School/Chemistry/Top Hat.png";

        let thumb = s.blob(id, BlobVariant::Thumbnail).await.unwrap();
        assert_eq!(thumb.mime, "image/png");
        let BlobBody::Bytes(b) = thumb.body else { panic!("expected the rendition") };
        assert_eq!(b, b"rendition");
        let asked = seen.lock().unwrap()[0].clone();
        // The fake decodes the path once, so a single encoding shows as the plain name.
        assert_eq!(asked, format!("{SPACE}/School/Chemistry/Top Hat.png?preview=1&x=256&y=256&a=1"));

        let _ = s.blob(id, BlobVariant::Preview).await.unwrap();
        assert!(seen.lock().unwrap()[1].contains("x=1600"));

        // OpenCloud failing: the original from disk instead.
        *up.lock().unwrap() = false;
        let thumb = s.blob(id, BlobVariant::Thumbnail).await.unwrap();
        assert!(matches!(thumb.body, BlobBody::File(_)));

        // Not an image: no thumbnail at all.
        assert!(matches!(s.blob("notes.txt", BlobVariant::Thumbnail).await, Err(SourceError::NotFound)));
        // Originals always come from disk.
        assert!(matches!(s.blob(id, BlobVariant::Original).await.unwrap().body, BlobBody::File(_)));
    }

    #[tokio::test]
    async fn result_lists_get_thumbnails_only_with_a_token() {
        let f = fixture();
        let base = opencloud(Arc::new(Mutex::new(true)), Arc::default()).await;
        let with = source(&f, &base, "good token");
        let without = OpenCloudSource::new(&f.users, &f.root, "https://oc.example", None).unwrap();
        let image = with.doc(&atlas_fs::entry(&f.root, "School/Chemistry/Top Hat.png").unwrap(), false);
        let text = with.doc(&atlas_fs::entry(&f.root, "notes.txt").unwrap(), false);
        assert!(with.has_thumbnail(&image));
        assert!(!with.has_thumbnail(&text));
        assert!(!without.has_thumbnail(&image));
    }
}
