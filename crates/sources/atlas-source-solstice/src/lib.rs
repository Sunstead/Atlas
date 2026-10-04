//! Solstice notes, read from disk. Solstice Sync keeps every vault as plain
//! files under `data/notes/<username>/<vault>/` on the server, so Atlas
//! indexes a user's vaults in place, read-only, like OpenCloud's spaces.
//!
//! Item ids are paths relative to the user's folder, `/` separated, so the
//! first segment is the vault: `Notes/projects/atlas.md`. Files directly in
//! the user's folder belong to no vault and are skipped, as is the sync
//! server's own `.solstice/` in each vault (hidden, so never walked).
//!
//! Markdown files are notes (`kind: note`); attachments are files. Links
//! open the item in the desktop app: `solstice://open?vault=&path=`.
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

/// How much of a note a preview shows.
const PREVIEW_BYTES: usize = 64 * 1024;

pub const KIND: &str = "solstice";

pub struct SolsticeSource {
    root: PathBuf,
}

impl SolsticeSource {
    /// `root` is the user's folder, pinned when they connected; it must sit
    /// inside `notes_dir`, even after resolving symlinks.
    pub fn new(notes_dir: &Path, root: &Path) -> Result<Self> {
        let not_there = || {
            SourceError::Config(
                "Your notes aren't on the server yet. In Solstice, open Settings, Sync, and link a folder to a vault; then sync again."
                    .into(),
            )
        };
        let canon_notes =
            notes_dir.canonicalize().map_err(|e| SourceError::Unavailable(format!("{}: {e}", notes_dir.display())))?;
        let canon_root = root.canonicalize().map_err(|_| not_there())?;
        if !canon_root.starts_with(&canon_notes) || canon_root == canon_notes {
            return Err(SourceError::Config("This connection's folder is outside Solstice's notes".into()));
        }
        if !canon_root.is_dir() {
            return Err(not_there());
        }
        Ok(Self { root: root.to_path_buf() })
    }

    fn entry(&self, id: &str) -> Result<Entry> {
        if !in_vault(id) {
            return Err(SourceError::NotFound);
        }
        atlas_fs::entry(&self.root, id).map_err(SourceError::from)
    }

    fn doc(&self, entry: &Entry, with_text: bool) -> Doc {
        let (folder, name) = entry.id.rsplit_once('/').expect("ids are inside a vault");
        let note = is_note(&entry.path);
        let title = match name.rsplit_once('.') {
            Some((stem, _)) if note && !stem.is_empty() => stem.to_owned(),
            _ => name.to_owned(),
        };
        let (mime, body) = if with_text {
            match atlas_fs::extract(&entry.path) {
                Ok(x) => (x.mime, x.text),
                Err(e) => {
                    tracing::debug!(id = %entry.id, error = %e, "can't read for indexing");
                    (atlas_fs::mime_for(&entry.path), None)
                }
            }
        } else {
            (atlas_fs::mime_for(&entry.path), None)
        };
        Doc {
            external_id: entry.id.clone(),
            kind: if note { "note" } else { "file" }.into(),
            title,
            path: Some(folder.to_owned()),
            mime: Some(mime),
            size: Some(entry.size),
            mtime: Some(entry.mtime),
            fingerprint: Some(entry.fingerprint()),
            body,
        }
    }
}

/// Inside a vault: at least `<vault>/<file>`.
fn in_vault(id: &str) -> bool {
    id.contains('/')
}

fn is_note(path: &Path) -> bool {
    atlas_fs::kind_for(path) == TextKind::Markdown
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

/// `solstice://open?vault=<vault>&path=<path in the vault>`.
pub fn deep_link(id: &str) -> Option<String> {
    let (vault, path) = id.split_once('/')?;
    Some(format!("solstice://open?vault={}&path={}", encode(vault), encode(path)))
}

#[async_trait]
impl Indexed for SolsticeSource {
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
                for entry in atlas_fs::walk(&self.root).filter(|e| in_vault(&e.id)) {
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
        atlas_fs::id_for_path(&self.root, path).filter(|id| in_vault(id))
    }
}

#[async_trait]
impl Source for SolsticeSource {
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

    /// The file from disk. There are no smaller renditions here: an image's
    /// preview is the original, and nothing has a thumbnail.
    async fn blob(&self, id: &str, variant: BlobVariant) -> Result<Blob> {
        let entry = self.entry(id)?;
        let image = atlas_fs::kind_for(&entry.path) == TextKind::Image;
        match variant {
            BlobVariant::Original => {}
            BlobVariant::Preview if image => {}
            _ => return Err(SourceError::NotFound),
        }
        Ok(Blob { mime: atlas_fs::mime_for(&entry.path), len: Some(entry.size), body: BlobBody::File(entry.path) })
    }

    fn deep_link(&self, doc: &Doc) -> Option<String> {
        deep_link(&doc.external_id)
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
        notes: PathBuf,
        root: PathBuf,
    }

    /// `notes/pwb/Notes/...` with the sync server's own `.solstice/`, a stray
    /// file outside any vault, and another user's vault.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes");
        let root = notes.join("pwb");
        fs::create_dir_all(root.join("Notes/projects")).unwrap();
        fs::write(root.join("Notes/projects/Atlas plan.md"), "# Atlas\n\nIndex **notes** too.").unwrap();
        fs::write(root.join("Notes/todo.md"), "- [ ] taxes").unwrap();
        fs::write(root.join("Notes/diagram.png"), [0x89, b'P', b'N', b'G']).unwrap();
        fs::create_dir_all(root.join("Notes/.solstice/blobs")).unwrap();
        fs::write(root.join("Notes/.solstice/blobs/abc"), "cache").unwrap();
        fs::write(root.join("stray.md"), "not in a vault").unwrap();
        fs::create_dir_all(notes.join("kim/Private")).unwrap();
        fs::write(notes.join("kim/Private/secret.md"), "kim only").unwrap();
        Fixture { _dir: dir, notes, root }
    }

    fn source(f: &Fixture) -> SolsticeSource {
        SolsticeSource::new(&f.notes, &f.root).unwrap()
    }

    async fn full(s: &SolsticeSource, known: &Known) -> (SyncReport, Sink) {
        let mut sink = Sink::default();
        let r = s.sync(SyncMode::Full, known, &mut sink, &CancellationToken::new()).await.unwrap();
        (r, sink)
    }

    #[tokio::test]
    async fn indexes_the_users_vaults_only() {
        let f = fixture();
        let (r, sink) = full(&source(&f), &Known::new()).await;
        let mut ids: Vec<_> = sink.upserts.keys().cloned().collect();
        ids.sort();
        assert_eq!(ids, ["Notes/diagram.png", "Notes/projects/Atlas plan.md", "Notes/todo.md"]);
        assert_eq!(r.upserted, 3);
        let plan = &sink.upserts["Notes/projects/Atlas plan.md"];
        assert_eq!(plan.kind, "note");
        assert_eq!(plan.title, "Atlas plan");
        assert_eq!(plan.path.as_deref(), Some("Notes/projects"));
        assert!(plan.body.as_deref().unwrap().contains("Index notes too."));
        let png = &sink.upserts["Notes/diagram.png"];
        assert_eq!((png.kind.as_str(), png.title.as_str()), ("file", "diagram.png"));
    }

    #[tokio::test]
    async fn unchanged_files_are_skipped_and_gone_ones_deleted() {
        let f = fixture();
        let s = source(&f);
        let (_, first) = full(&s, &Known::new()).await;
        let mut known: Known = first.upserts.iter().map(|(id, d)| (id.clone(), d.fingerprint.clone())).collect();
        known.insert("Notes/old.md".into(), Some("1:1".into()));
        let (r, sink) = full(&s, &known).await;
        assert_eq!(r.upserted, 0);
        assert_eq!(sink.deletes, ["Notes/old.md"]);
    }

    #[tokio::test]
    async fn changed_paths_map_to_items_inside_vaults() {
        let f = fixture();
        let s = source(&f);
        assert_eq!(s.id_for_path(&f.root.join("Notes/todo.md")).as_deref(), Some("Notes/todo.md"));
        assert_eq!(s.id_for_path(&f.root.join("Notes/.solstice/blobs/abc")), None);
        assert_eq!(s.id_for_path(&f.root.join("stray.md")), None);

        fs::write(f.root.join("Notes/new.md"), "fresh").unwrap();
        fs::remove_file(f.root.join("Notes/todo.md")).unwrap();
        let known: Known = [("Notes/todo.md".to_string(), Some("1:1".to_string()))].into();
        let mut sink = Sink::default();
        let r = s
            .sync(
                SyncMode::Items(vec!["Notes/new.md".into(), "Notes/todo.md".into()]),
                &known,
                &mut sink,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!((r.upserted, r.deleted), (1, 1));
        assert_eq!(sink.deletes, ["Notes/todo.md"]);
    }

    #[test]
    fn the_root_must_be_inside_the_notes_dir() {
        let f = fixture();
        assert!(SolsticeSource::new(&f.notes, &f.notes).is_err(), "the notes dir itself");
        assert!(SolsticeSource::new(&f.notes, &f.notes.join("pwb/../..")).is_err());
        let r = SolsticeSource::new(&f.notes, &f.notes.join("nobody"));
        assert!(matches!(r, Err(SourceError::Config(_))), "no vaults yet");
    }

    #[tokio::test]
    async fn previews_and_blobs_stay_inside_the_users_vaults() {
        let f = fixture();
        let s = source(&f);
        assert!(matches!(s.preview("Notes/todo.md").await.unwrap(), Preview::Markdown { .. }));
        assert_eq!(s.preview("Notes/diagram.png").await.unwrap(), Preview::Image);
        for bad in ["../kim/Private/secret.md", "Notes/.solstice/blobs/abc", "stray.md", "Notes/../../kim/Private/secret.md"] {
            assert!(s.preview(bad).await.is_err(), "{bad}");
            assert!(s.blob(bad, BlobVariant::Original).await.is_err(), "{bad}");
        }
        assert!(s.blob("Notes/diagram.png", BlobVariant::Preview).await.is_ok());
        assert!(s.blob("Notes/diagram.png", BlobVariant::Thumbnail).await.is_err());
        assert!(s.blob("Notes/todo.md", BlobVariant::Preview).await.is_err());
        assert_eq!(s.blob("Notes/todo.md", BlobVariant::Original).await.unwrap().mime, "text/markdown");
    }

    #[test]
    fn links_open_the_note_in_its_vault() {
        assert_eq!(
            deep_link("My Notes/projects/Atlas plan.md").as_deref(),
            Some("solstice://open?vault=My%20Notes&path=projects%2FAtlas%20plan.md")
        );
        assert_eq!(deep_link("stray.md"), None);
    }
}
