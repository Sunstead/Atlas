use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// `GET /v1/search?q=&type=&source=&limit=`.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// How each connection did. A failing or slow source shows up here,
    /// and the others' results still come back.
    pub sources: Vec<SourceStatus>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct SearchHit {
    pub item: ItemRef,
    pub title: String,
    /// Where it lives in its source, e.g. `Documents/Taxes`.
    pub path: Option<String>,
    /// `file`, `photo`, `album`.
    pub kind: String,
    pub mime: Option<String>,
    #[ts(type = "number | null")]
    pub size: Option<u64>,
    /// Unix seconds.
    #[ts(type = "number | null")]
    pub modified: Option<i64>,
    pub snippet: Option<Snippet>,
    /// A thumbnail is available (`/blob?variant=thumbnail`).
    pub thumbnail: bool,
    /// "Open in app".
    pub url: Option<String>,
}

/// Which item, in which connection. `id` is the source's id, base64url
/// encoded, so it's safe in a URL path segment.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[ts(export)]
pub struct ItemRef {
    #[ts(type = "number")]
    pub connection: i64,
    /// The connection's source kind: `opencloud`, `immich`.
    pub source: String,
    pub id: String,
}

/// A piece of the item's text around the match.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct Snippet {
    pub text: String,
    /// `[start, end)` character offsets (UTF-16 code units, as JavaScript
    /// counts) to highlight.
    pub highlights: Vec<[u32; 2]>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct SourceStatus {
    #[ts(type = "number")]
    pub connection: i64,
    pub source: String,
    pub label: String,
    pub state: SourceState,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Ok,
    /// Answered with an error; `message` says what.
    Error,
    /// Didn't answer in time; its results are missing.
    Timeout,
}

/// `GET /v1/items/{connection}/{id}`.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct ItemInfo {
    pub item: ItemRef,
    pub title: String,
    pub path: Option<String>,
    pub kind: String,
    pub mime: Option<String>,
    #[ts(type = "number | null")]
    pub size: Option<u64>,
    #[ts(type = "number | null")]
    pub modified: Option<i64>,
    pub url: Option<String>,
    /// The connection's label, e.g. "OpenCloud".
    pub source_label: String,
}

/// `GET /v1/items/{connection}/{id}/preview`: how to show it in Atlas.
/// Media types load their bytes from `/blob`.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[ts(export)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PreviewInfo {
    Text { text: String, truncated: bool },
    Markdown { text: String, truncated: bool },
    Image,
    Pdf,
    Video,
    Audio,
    None,
}
