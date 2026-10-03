//! `/v1/items/{connection}/{id}`: one item, its preview and its bytes.
//! `id` is the source's id, base64url encoded. Every route checks that the
//! connection belongs to the signed-in user before asking the source.
//!
//! Bytes are served from Atlas's own origin, so a user's HTML or SVG file
//! must never run as Atlas: every blob is sent with `CSP: sandbox` (no
//! scripts, an opaque origin) and `nosniff`. PDFs are the exception, because
//! browsers won't show them in a sandbox; their viewers don't run page script
//! against the origin.

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::state::AppState;
use atlas_common::{ItemInfo, ItemRef, PreviewInfo};
use atlas_core::{BlobBody, BlobVariant, Preview, Source};
use atlas_state::ConnectionRow;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use serde::Deserialize;
use std::sync::Arc;

pub fn encode_id(id: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id)
}

fn decode_id(encoded: &str) -> Result<String, AppError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(AppError::not_found)
}

/// The user's connection and its source, or 404 (never "someone else's").
async fn open(state: &AppState, user: &atlas_state::User, conn: i64) -> Result<(ConnectionRow, Arc<dyn Source>), AppError> {
    let row = state.db.connection(user.id, conn).await?;
    if !row.enabled {
        return Err(AppError::not_found());
    }
    let source = state.indexer.source(&row)?;
    Ok((row, source))
}

pub async fn item(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path((conn, id)): Path<(i64, String)>,
) -> Result<Json<ItemInfo>, AppError> {
    let external = decode_id(&id)?;
    let (row, source) = open(&state, &user, conn).await?;
    let doc = source.get(&external).await?;
    Ok(Json(ItemInfo {
        item: ItemRef { connection: row.id, source: row.kind.clone(), id },
        url: source.deep_link(&doc),
        title: doc.title,
        path: doc.path,
        kind: doc.kind,
        mime: doc.mime,
        size: doc.size,
        modified: doc.mtime,
        source_label: row.label,
    }))
}

pub async fn preview(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path((conn, id)): Path<(i64, String)>,
) -> Result<Json<PreviewInfo>, AppError> {
    let external = decode_id(&id)?;
    let (_, source) = open(&state, &user, conn).await?;
    Ok(Json(match source.preview(&external).await? {
        Preview::Text { text, truncated } => PreviewInfo::Text { text, truncated },
        Preview::Markdown { text, truncated } => PreviewInfo::Markdown { text, truncated },
        Preview::Image => PreviewInfo::Image,
        Preview::Pdf => PreviewInfo::Pdf,
        Preview::Video => PreviewInfo::Video,
        Preview::Audio => PreviewInfo::Audio,
        Preview::None => PreviewInfo::None,
    }))
}

#[derive(Deserialize)]
pub struct BlobParams {
    variant: Option<String>,
    /// `download=1`: save it rather than show it.
    download: Option<String>,
}

pub async fn blob(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path((conn, id)): Path<(i64, String)>,
    Query(p): Query<BlobParams>,
) -> Result<Response, AppError> {
    let external = decode_id(&id)?;
    let (_, source) = open(&state, &user, conn).await?;
    let variant = match p.variant.as_deref() {
        Some("thumbnail") => BlobVariant::Thumbnail,
        Some("preview") => BlobVariant::Preview,
        _ => BlobVariant::Original,
    };
    let blob = source.blob(&external, variant).await?;

    let body = match blob.body {
        BlobBody::Bytes(b) => Body::from(b),
        BlobBody::File(path) => {
            let file = tokio::fs::File::open(&path).await.map_err(|e| AppError::from(atlas_core::SourceError::from(e)))?;
            Body::from_stream(tokio_util::io::ReaderStream::new(file))
        }
    };

    let mut res = body.into_response();
    let h = res.headers_mut();
    let mime = HeaderValue::from_str(&blob.mime).unwrap_or(HeaderValue::from_static("application/octet-stream"));
    let is_pdf = blob.mime == "application/pdf";
    h.insert(header::CONTENT_TYPE, mime);
    if let Some(len) = blob.len {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=300"));
    if !is_pdf {
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'"));
    }
    if p.download.is_some() {
        let name = external.rsplit('/').next().unwrap_or("download");
        let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' { c } else { '_' }).collect();
        let encoded: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>().replace('+', "%20");
        if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_urls() {
        for id in ["Documents/Taxes/2025.txt", "Meeting 10:30 ✓.md", "a/b?c#d"] {
            let e = encode_id(id);
            assert!(e.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{e}");
            assert_eq!(decode_id(&e).ok().unwrap(), id);
        }
        assert!(decode_id("!!").is_err());
    }
}
