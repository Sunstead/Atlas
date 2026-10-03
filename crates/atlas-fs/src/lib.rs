//! Files on local disk, for sources whose originals Atlas reads in place
//! (OpenCloud's PosixFS spaces now, Solstice vaults later):
//! - [`walk`]: every file under a root, skipping hidden files and folders and
//!   never following symlinks;
//! - [`resolve`]: an item id back to a path, refusing anything outside the root;
//! - [`extract`]: searchable text from a file, capped;
//! - [`watch`]: change events for a root, debounced.

mod extract;
mod watch;

pub use extract::{extract, kind_for, mime_for, Extracted, TextKind, MAX_TEXT_BYTES};
pub use watch::{watch, Watcher};

use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the root, `/` separated: the item's id.
    pub id: String,
    pub path: PathBuf,
    pub size: u64,
    /// Unix seconds.
    pub mtime: i64,
}

impl Entry {
    /// Changes when the file does, without reading it. Content hashing would
    /// mean reading every file on every full sync.
    pub fn fingerprint(&self) -> String {
        format!("{}:{}", self.size, self.mtime)
    }
}

/// Whether a name is hidden: dot-files and dot-folders (`.solstice/`,
/// `.oc-nodes`), which hold app state, not the user's files.
pub fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn to_id(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?.to_owned()),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The id for a path under `root`, if it's a visible file Atlas would index.
pub fn id_for_path(root: &Path, path: &Path) -> Option<String> {
    let id = to_id(root, path)?;
    if id.split('/').any(is_hidden) {
        return None;
    }
    Some(id)
}

/// A file's entry, if it's a regular file (not a symlink).
pub fn entry(root: &Path, id: &str) -> std::io::Result<Entry> {
    let path = resolve(root, id)?;
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not a file"));
    }
    Ok(make_entry(id.to_owned(), path, &meta))
}

fn make_entry(id: String, path: PathBuf, meta: &std::fs::Metadata) -> Entry {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Entry { id, path, size: meta.len(), mtime }
}

/// Every visible regular file under `root`. Symlinks are skipped, not
/// followed, so a link can't pull in files from outside the root. Unreadable
/// entries are logged and skipped.
pub fn walk(root: &Path) -> impl Iterator<Item = Entry> + '_ {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .follow_links(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .build()
        .filter_map(move |r| match r {
            Ok(e) => Some(e),
            Err(err) => {
                tracing::debug!(error = %err, "skipping an unreadable entry");
                None
            }
        })
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(move |e| {
            let id = to_id(root, e.path())?;
            let meta = e.metadata().ok()?;
            Some(make_entry(id, e.into_path(), &meta))
        })
}

/// The path for an item id, if it stays inside `root`. Refuses `..`,
/// absolute paths, hidden segments and anything that resolves (through a
/// symlink) outside the root.
pub fn resolve(root: &Path, id: &str) -> std::io::Result<PathBuf> {
    let denied = || std::io::Error::new(std::io::ErrorKind::NotFound, "not inside this source");
    if id.is_empty() || id.contains('\\') || id.contains('\0') {
        return Err(denied());
    }
    let mut path = root.to_path_buf();
    for part in id.split('/') {
        if part.is_empty() || part == "." || part == ".." || is_hidden(part) {
            return Err(denied());
        }
        path.push(part);
    }
    let canon_root = root.canonicalize()?;
    let canon = path.canonicalize()?;
    if !canon.starts_with(&canon_root) {
        return Err(denied());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("Documents/Taxes")).unwrap();
        fs::create_dir_all(r.join(".solstice")).unwrap();
        fs::write(r.join("Documents/Taxes/2025.txt"), "refund").unwrap();
        fs::write(r.join("Documents/notes.md"), "# Hi").unwrap();
        fs::write(r.join(".solstice/state.json"), "{}").unwrap();
        fs::write(r.join(".hidden"), "x").unwrap();
        d
    }

    #[test]
    fn walks_visible_files_with_slash_ids() {
        let d = tree();
        let mut ids: Vec<_> = walk(d.path()).map(|e| e.id).collect();
        ids.sort();
        assert_eq!(ids, ["Documents/Taxes/2025.txt", "Documents/notes.md"]);
    }

    #[test]
    fn resolves_ids_inside_the_root_only() {
        let d = tree();
        assert!(resolve(d.path(), "Documents/notes.md").is_ok());
        for bad in ["", "../x", "Documents/../../x", "/etc/passwd", "Documents\\notes.md", ".solstice/state.json", "a//b", "C:/x"] {
            assert!(resolve(d.path(), bad).is_err(), "{bad:?}");
        }
        assert!(resolve(d.path(), "Documents/missing.md").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn colons_are_ordinary_on_unix() {
        let d = tree();
        fs::write(d.path().join("Documents/Meeting 10:30.txt"), "x").unwrap();
        assert!(resolve(d.path(), "Documents/Meeting 10:30.txt").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_root_are_refused_and_not_walked() {
        let d = tree();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), d.path().join("Documents/link.txt")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("Documents/linkdir")).unwrap();
        assert!(resolve(d.path(), "Documents/link.txt").is_err());
        assert!(resolve(d.path(), "Documents/linkdir/secret.txt").is_err());
        assert!(walk(d.path()).all(|e| !e.id.contains("link")));
    }

    #[test]
    fn maps_paths_back_to_ids() {
        let d = tree();
        let r = d.path();
        assert_eq!(id_for_path(r, &r.join("Documents/notes.md")).as_deref(), Some("Documents/notes.md"));
        assert_eq!(id_for_path(r, &r.join(".solstice/state.json")), None);
        assert_eq!(id_for_path(r, r), None);
        assert_eq!(id_for_path(r, Path::new("/elsewhere/x")), None);
    }

    #[test]
    fn entries_carry_a_fingerprint() {
        let d = tree();
        let e = entry(d.path(), "Documents/Taxes/2025.txt").unwrap();
        assert_eq!(e.size, 6);
        assert!(e.fingerprint().starts_with("6:"));
        assert!(entry(d.path(), "Documents").is_err(), "folders aren't entries");
    }
}
