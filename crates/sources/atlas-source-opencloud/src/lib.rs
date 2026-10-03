//! OpenCloud, read from disk. OpenCloud stores personal spaces as plain
//! files (PosixFS) under `data/files/users/<username>`, so Atlas indexes a
//! user's space in place, read-only, without OpenCloud's API or a token.
//! The API (app tokens) comes later, for file-id links, thumbnails and shared
//! spaces.
//!
//! Item ids are paths relative to the user's root, `/` separated. They change
//! when a file is renamed or moved, which a sync sees as a delete and an add.
//!
//! The sync runs on a blocking thread (the server's indexer uses
//! `spawn_blocking`), so walking and reading files here is fine.

use async_trait::async_trait;
use atlas_core::{
    Blob, BlobBody, BlobVariant, Capabilities, Doc, Indexed, IndexSink, Known, Preview, Result, Source, SourceError,
    SyncMode, SyncReport,
};
use atlas_fs::{Entry, TextKind};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// How much of a text file a preview shows.
const PREVIEW_BYTES: usize = 64 * 1024;

pub const KIND: &str = "opencloud";

pub struct OpenCloudSource {
    root: PathBuf,
    username: String,
    public_url: String,
}

impl OpenCloudSource {
    /// `root` is the user's space, pinned when they connected; it must sit
    /// inside `users_dir`, even after resolving symlinks.
    pub fn new(users_dir: &Path, root: &Path, public_url: &str) -> Result<Self> {
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
        Ok(Self { root: root.to_path_buf(), username, public_url: public_url.trim_end_matches('/').to_owned() })
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
        // No thumbnails from disk: images stand in for their own, anything
        // else has none until the API layer.
        if variant == BlobVariant::Thumbnail && atlas_fs::kind_for(&entry.path) != TextKind::Image {
            return Err(SourceError::NotFound);
        }
        Ok(Blob { mime: mime_of(&entry.path), len: Some(entry.size), body: BlobBody::File(entry.path) })
    }

    /// The containing folder in OpenCloud's web app. File-id links (straight
    /// to the file) need the API and come with it.
    fn deep_link(&self, doc: &Doc) -> Option<String> {
        let folder = doc.path.as_deref().map(|p| format!("/{}", encode_path(p))).unwrap_or_default();
        Some(format!("{}/files/spaces/personal/{}{}", self.public_url, encode_path(&self.username), folder))
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
        OpenCloudSource::new(&f.users, &f.root, "https://opencloud.example/").unwrap()
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
        assert!(OpenCloudSource::new(&f.users, &f.users, "https://x").is_err(), "the users dir itself");
        assert!(OpenCloudSource::new(&f.users, &f.users.join("../users/pwb/../.."), "https://x").is_err());
        let r = OpenCloudSource::new(&f.users, &f.users.join("nobody"), "https://x");
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
