//! The contract between Atlas and its sources.
//!
//! A source is one user's connection to one app (their OpenCloud, their
//! Immich). It can be:
//! - **indexed** ([`Indexed`]): Atlas copies its searchable text into the
//!   local index, and keeps it current;
//! - **federated** ([`Federated`]): Atlas asks it at query time (Immich's own
//!   smart search);
//! - or both.
//!
//! Every source can describe an item, preview it, hand over its bytes, and
//! link into its app. Sources never touch Atlas's storage: an indexed source
//! reports documents to an [`IndexSink`], and the server decides where they
//! go. That keeps source crates small and testable on their own.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// A source's own id for an item, stable for as long as the item exists in
/// the same place (for files, the path relative to the connection's root,
/// with `/` separators).
pub type ExternalId = String;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("not found")]
    NotFound,
    /// The connection's settings or credential are wrong. The user fixes it.
    #[error("{0}")]
    Config(String),
    /// The app or disk is failing right now. Retry later.
    #[error("{0}")]
    Unavailable(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for SourceError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound,
            _ => Self::Unavailable(e.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, SourceError>;

/// What one item looks like to the index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Doc {
    pub external_id: ExternalId,
    /// `file`, `photo`, `album`, `note`. Filters (`?type=`) match on it.
    pub kind: String,
    pub title: String,
    /// Where it lives inside the source, for display (`Documents/Taxes`).
    pub path: Option<String>,
    pub mime: Option<String>,
    pub size: Option<u64>,
    /// Unix seconds.
    pub mtime: Option<i64>,
    /// Changes when the content does. Lets a sync skip unchanged items.
    pub fingerprint: Option<String>,
    /// Searchable text, already extracted and capped.
    pub body: Option<String>,
}

/// What the index already holds for a connection: external id to
/// fingerprint. An indexed source compares against it to skip work.
pub type Known = HashMap<ExternalId, Option<String>>;

/// Where an indexed source reports changes during a sync. Buffered: nothing
/// is visible to searches until the sync commits.
pub trait IndexSink: Send {
    fn upsert(&mut self, doc: Doc) -> Result<()>;
    fn delete(&mut self, id: &str) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncMode {
    /// Everything: compare with `known`, add, change and delete.
    Full,
    /// Just these items (from a file watcher), each upserted or deleted.
    Items(Vec<ExternalId>),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub seen: u64,
    pub upserted: u64,
    pub deleted: u64,
    /// Items that couldn't be read; logged, and retried next sync.
    pub failed: u64,
}

#[async_trait]
pub trait Indexed: Send + Sync {
    async fn sync(
        &self,
        mode: SyncMode,
        known: &Known,
        sink: &mut dyn IndexSink,
        cancel: &CancellationToken,
    ) -> Result<SyncReport>;

    /// Directories to watch for changes, if the source lives on local disk.
    /// The server maps events back with [`Indexed::id_for_path`].
    fn watch_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// The item id for a changed path under a watch root, if it's one this
    /// source indexes.
    fn id_for_path(&self, _path: &std::path::Path) -> Option<ExternalId> {
        None
    }
}

/// A search Atlas forwards to the source at query time.
#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub kind: Option<String>,
    pub limit: usize,
}

/// One result from a federated source, ranked by the source.
#[derive(Debug, Clone)]
pub struct FederatedHit {
    pub doc: Doc,
    /// A thumbnail is available through [`Source::blob`].
    pub has_thumbnail: bool,
}

#[async_trait]
pub trait Federated: Send + Sync {
    async fn search(&self, query: &Query) -> Result<Vec<FederatedHit>>;
}

/// How to show an item without leaving Atlas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Preview {
    /// Plain text or code, possibly cut short.
    Text { text: String, truncated: bool },
    Markdown { text: String, truncated: bool },
    /// Shown from the blob (`original`).
    Image,
    Pdf,
    Video,
    Audio,
    /// Nothing to show inline; metadata and links only.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobVariant {
    /// The file as stored.
    Original,
    /// A large rendition for the preview pane, in a format browsers show
    /// (Immich's for HEIC and RAW). Sources without one serve the original.
    Preview,
    /// A small square-ish rendition for result lists.
    Thumbnail,
}

/// An item's bytes, for previews and downloads.
pub struct Blob {
    pub mime: String,
    pub len: Option<u64>,
    pub body: BlobBody,
}

pub enum BlobBody {
    /// A local file, streamed by the server.
    File(PathBuf),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub indexed: bool,
    pub federated: bool,
}

#[async_trait]
pub trait Source: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    /// Whether the connection works (the key is accepted, the folder is
    /// there). Run when a user connects, so mistakes show up immediately.
    async fn check(&self) -> Result<()> {
        Ok(())
    }

    /// The item as it is now, straight from the source.
    async fn get(&self, id: &str) -> Result<Doc>;

    async fn preview(&self, id: &str) -> Result<Preview>;

    async fn blob(&self, id: &str, variant: BlobVariant) -> Result<Blob>;

    /// Where "Open in app" goes.
    fn deep_link(&self, doc: &Doc) -> Option<String>;

    fn as_indexed(&self) -> Option<&dyn Indexed> {
        None
    }

    fn as_federated(&self) -> Option<&dyn Federated> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_are_tagged_on_the_wire() {
        let p = Preview::Text { text: "hi".into(), truncated: false };
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"type":"text","text":"hi","truncated":false}"#);
        assert_eq!(serde_json::to_string(&Preview::Pdf).unwrap(), r#"{"type":"pdf"}"#);
    }
}
