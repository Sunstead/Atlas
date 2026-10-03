//! Immich, asked at query time. Immich already has the best photo search
//! there is for its library (CLIP embeddings from its ML container), so
//! Atlas forwards each search rather than copying the library:
//! - smart search (`POST /api/search/smart`) for "beach at sunset";
//! - file name search (`POST /api/search/metadata`) for "IMG_2041";
//! - albums by name, from `GET /api/albums`, cached for a few minutes.
//!
//! Requests use the user's own API key (`x-api-key`), so results are exactly
//! what that user can see in Immich. The key needs `asset.read`, `asset.view`
//! and `album.read`; `asset.download` adds downloads of originals.
//!
//! Item ids are `asset:<uuid>` and `album:<uuid>`.
//!
//! Written against Immich 3.2: `query`/`size` and the older top-level
//! `originalFileName` field, which 3.2 deprecated in favour of `filter` but
//! still accepts.

use async_trait::async_trait;
use atlas_core::{
    Blob, BlobBody, BlobVariant, Capabilities, Doc, Federated, FederatedHit, Preview, Query, Result, Source, SourceError,
};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const KIND: &str = "immich";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const ALBUMS_FOR: Duration = Duration::from_secs(5 * 60);

pub struct ImmichSource {
    api: String,
    public: String,
    http: reqwest::Client,
    albums: Mutex<Option<(Instant, Vec<Album>)>>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct Asset {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    original_file_name: String,
    #[serde(default)]
    original_mime_type: Option<String>,
    #[serde(default)]
    local_date_time: Option<String>,
    #[serde(default)]
    file_created_at: Option<String>,
    #[serde(default)]
    exif_info: Option<Exif>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Exif {
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    file_size_in_byte: Option<u64>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct Album {
    id: String,
    album_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    asset_count: Option<u64>,
    #[serde(default)]
    album_thumbnail_asset_id: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse {
    assets: AssetPage,
}

#[derive(Deserialize)]
struct AssetPage {
    items: Vec<Asset>,
}

enum Id<'a> {
    Asset(&'a str),
    Album(&'a str),
}

fn parse_id(id: &str) -> Result<Id<'_>> {
    let valid = |u: &str| u.len() == 36 && u.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    match id.split_once(':') {
        Some(("asset", u)) if valid(u) => Ok(Id::Asset(u)),
        Some(("album", u)) if valid(u) => Ok(Id::Album(u)),
        _ => Err(SourceError::NotFound),
    }
}

/// Unix seconds for an ISO 8601 timestamp's date and time (the zone is
/// ignored: Immich's `localDateTime` is wall-clock time marked `Z`).
fn unix_secs(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

fn place(exif: &Exif) -> Option<String> {
    let parts: Vec<&str> = [exif.city.as_deref(), exif.country.as_deref()].into_iter().flatten().filter(|s| !s.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

impl Asset {
    fn doc(&self) -> Doc {
        let exif = self.exif_info.clone().unwrap_or_default();
        Doc {
            external_id: format!("asset:{}", self.id),
            kind: if self.kind == "VIDEO" { "video" } else { "photo" }.into(),
            title: self.original_file_name.clone(),
            path: place(&exif),
            mime: self.original_mime_type.clone(),
            size: exif.file_size_in_byte,
            mtime: self.local_date_time.as_deref().or(self.file_created_at.as_deref()).and_then(unix_secs),
            fingerprint: None,
            body: exif.description.filter(|d| !d.is_empty()),
        }
    }
}

impl Album {
    fn doc(&self) -> Doc {
        let count = self.asset_count.map(|n| format!("{n} {}", if n == 1 { "item" } else { "items" }));
        Doc {
            external_id: format!("album:{}", self.id),
            kind: "album".into(),
            title: self.album_name.clone(),
            path: count,
            mime: None,
            size: None,
            mtime: self.updated_at.as_deref().and_then(unix_secs),
            fingerprint: None,
            body: self.description.clone().filter(|d| !d.is_empty()),
        }
    }
}

fn http_error(what: &str, status: reqwest::StatusCode) -> SourceError {
    match status.as_u16() {
        401 => SourceError::Config("Immich didn't accept the API key. Make a new one and save it in Atlas.".into()),
        403 => SourceError::Config(format!("The Immich API key isn't allowed to {what}. Give it more permissions in Immich.")),
        404 => SourceError::NotFound,
        _ => SourceError::Unavailable(format!("Immich: {what}: HTTP {status}")),
    }
}

impl ImmichSource {
    /// `api` is where Atlas reaches Immich (`http://immich-server:2283`);
    /// `public` is where links point.
    pub fn new(api: &str, public: &str, api_key: &str) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut key = reqwest::header::HeaderValue::from_str(api_key.trim())
            .map_err(|_| SourceError::Config("The Immich API key has characters it can't have".into()))?;
        key.set_sensitive(true);
        headers.insert("x-api-key", key);
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .default_headers(headers)
            .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| SourceError::Other(e.to_string()))?;
        Ok(Self {
            api: api.trim_end_matches('/').to_owned(),
            public: public.trim_end_matches('/').to_owned(),
            http,
            albums: Mutex::new(None),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api{path}", self.api)
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, what: &str, body: serde_json::Value) -> Result<T> {
        let res = self.http.post(self.url(path)).json(&body).send().await.map_err(|e| SourceError::Unavailable(format!("Immich: {e}")))?;
        if !res.status().is_success() {
            return Err(http_error(what, res.status()));
        }
        res.json().await.map_err(|e| SourceError::Unavailable(format!("Immich: {what}: {e}")))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str, what: &str) -> Result<T> {
        let res = self.http.get(self.url(path)).send().await.map_err(|e| SourceError::Unavailable(format!("Immich: {e}")))?;
        if !res.status().is_success() {
            return Err(http_error(what, res.status()));
        }
        res.json().await.map_err(|e| SourceError::Unavailable(format!("Immich: {what}: {e}")))
    }

    async fn bytes(&self, path: &str, what: &str) -> Result<Blob> {
        let res = self.http.get(self.url(path)).send().await.map_err(|e| SourceError::Unavailable(format!("Immich: {e}")))?;
        if !res.status().is_success() {
            return Err(http_error(what, res.status()));
        }
        let mime = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let bytes = res.bytes().await.map_err(|e| SourceError::Unavailable(format!("Immich: {e}")))?;
        Ok(Blob { mime, len: Some(bytes.len() as u64), body: BlobBody::Bytes(bytes.to_vec()) })
    }

    async fn albums(&self) -> Result<Vec<Album>> {
        if let Some((at, albums)) = self.albums.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            if at.elapsed() < ALBUMS_FOR {
                return Ok(albums.clone());
            }
        }
        let albums: Vec<Album> = self.get_json("/albums", "list albums").await?;
        *self.albums.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), albums.clone()));
        Ok(albums)
    }
}

#[async_trait]
impl Federated for ImmichSource {
    /// File name matches first (exact intent), then smart search, then
    /// albums by name. Smart search ranks by similarity, so a weak match
    /// still comes back; that's Immich's call, and the merge ranks it low.
    async fn search(&self, q: &Query) -> Result<Vec<FederatedHit>> {
        let wants = |k: &str| q.kind.as_deref().is_none_or(|want| want == k);
        let size = q.limit.clamp(1, 100);
        let media = wants("photo") || wants("video");

        let by_name = async {
            if !media {
                return Ok(Vec::new());
            }
            let r: SearchResponse = self
                .post("/search/metadata", "search", serde_json::json!({ "originalFileName": q.text, "size": size, "withExif": true }))
                .await?;
            Ok::<_, SourceError>(r.assets.items)
        };
        let smart = async {
            if !media {
                return Ok(Vec::new());
            }
            let r: SearchResponse =
                self.post("/search/smart", "search", serde_json::json!({ "query": q.text, "size": size, "withExif": true })).await?;
            Ok::<_, SourceError>(r.assets.items)
        };
        let albums = async {
            if !wants("album") {
                return Ok(Vec::new());
            }
            let needle = q.text.to_lowercase();
            Ok::<_, SourceError>(self.albums().await?.into_iter().filter(|a| a.album_name.to_lowercase().contains(&needle)).collect::<Vec<_>>())
        };
        let (by_name, smart, albums) = tokio::join!(by_name, smart, albums);
        // Albums failing (no album.read) shouldn't hide photos.
        let albums = albums.unwrap_or_else(|e| {
            tracing::debug!(error = %e, "Immich albums unavailable");
            Vec::new()
        });
        let (by_name, smart) = match (by_name, smart) {
            (Err(e), Err(_)) => return Err(e),
            (a, b) => (a.unwrap_or_default(), b.unwrap_or_default()),
        };

        let mut seen = std::collections::HashSet::new();
        let mut hits = Vec::new();
        for a in by_name.iter().chain(smart.iter()) {
            let doc = a.doc();
            if !wants(&doc.kind) || !seen.insert(a.id.clone()) {
                continue;
            }
            hits.push(FederatedHit { doc, has_thumbnail: true });
        }
        for a in &albums {
            hits.push(FederatedHit { doc: a.doc(), has_thumbnail: a.album_thumbnail_asset_id.is_some() });
        }
        hits.truncate(q.limit);
        Ok(hits)
    }
}

#[async_trait]
impl Source for ImmichSource {
    fn capabilities(&self) -> Capabilities {
        Capabilities { indexed: false, federated: true }
    }

    /// The cheapest call that proves the key can search.
    async fn check(&self) -> Result<()> {
        let _: SearchResponse =
            self.post("/search/metadata", "search", serde_json::json!({ "size": 1, "withExif": false })).await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Doc> {
        match parse_id(id)? {
            Id::Asset(u) => Ok(self.get_json::<Asset>(&format!("/assets/{u}"), "read photos").await?.doc()),
            Id::Album(u) => Ok(self.get_json::<Album>(&format!("/albums/{u}?withoutAssets=true"), "read albums").await?.doc()),
        }
    }

    /// Photos and videos show their preview rendition (browsers can't show
    /// HEIC or RAW originals, and Immich already made one); albums their
    /// cover.
    async fn preview(&self, id: &str) -> Result<Preview> {
        match parse_id(id)? {
            Id::Asset(_) => Ok(Preview::Image),
            Id::Album(u) => {
                let album: Album = self.get_json(&format!("/albums/{u}?withoutAssets=true"), "read albums").await?;
                Ok(if album.album_thumbnail_asset_id.is_some() { Preview::Image } else { Preview::None })
            }
        }
    }

    async fn blob(&self, id: &str, variant: BlobVariant) -> Result<Blob> {
        let asset = match parse_id(id)? {
            Id::Asset(u) => u.to_owned(),
            Id::Album(u) => {
                let album: Album = self.get_json(&format!("/albums/{u}?withoutAssets=true"), "read albums").await?;
                if variant == BlobVariant::Original {
                    return Err(SourceError::NotFound);
                }
                album.album_thumbnail_asset_id.ok_or(SourceError::NotFound)?
            }
        };
        match variant {
            BlobVariant::Original => self.bytes(&format!("/assets/{asset}/original"), "download originals").await,
            BlobVariant::Preview => self.bytes(&format!("/assets/{asset}/thumbnail?size=preview"), "view photos").await,
            BlobVariant::Thumbnail => self.bytes(&format!("/assets/{asset}/thumbnail?size=thumbnail"), "view photos").await,
        }
    }

    fn deep_link(&self, doc: &Doc) -> Option<String> {
        match parse_id(&doc.external_id).ok()? {
            Id::Asset(u) => Some(format!("{}/photos/{u}", self.public)),
            Id::Album(u) => Some(format!("{}/albums/{u}", self.public)),
        }
    }

    fn as_federated(&self) -> Option<&dyn Federated> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path, Query as AxQuery},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::Arc;

    const A1: &str = "11111111-1111-4111-8111-111111111111";
    const A2: &str = "22222222-2222-4222-8222-222222222222";
    const AL: &str = "33333333-3333-4333-8333-333333333333";

    fn asset(id: &str, name: &str, kind: &str) -> Value {
        json!({
            "id": id, "type": kind, "originalFileName": name, "originalMimeType": "image/jpeg",
            "localDateTime": "2024-07-14T18:30:00.000Z",
            "exifInfo": { "city": "Lisbon", "country": "Portugal", "fileSizeInByte": 2048, "description": "Sunset" }
        })
    }

    fn ok_key(h: &HeaderMap) -> bool {
        h.get("x-api-key").and_then(|v| v.to_str().ok()) == Some("good-key")
    }

    /// A fake Immich: one beach photo by name, one by smart search, one album.
    async fn immich(bodies: Arc<std::sync::Mutex<Vec<(String, Value)>>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (b1, b2) = (bodies.clone(), bodies);
        let app = Router::new()
            .route(
                "/api/search/metadata",
                post(move |h: HeaderMap, Json(body): Json<Value>| async move {
                    if !ok_key(&h) {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    b1.lock().unwrap().push(("metadata".into(), body.clone()));
                    let items = if body["originalFileName"] == "IMG" { vec![asset(A1, "IMG_2041.jpg", "IMAGE")] } else { vec![] };
                    Json(json!({ "assets": { "items": items, "count": 1, "total": 1, "facets": [], "nextPage": null, "nextCursor": null }, "albums": { "items": [], "count": 0, "total": 0, "facets": [] } })).into_response()
                }),
            )
            .route(
                "/api/search/smart",
                post(move |h: HeaderMap, Json(body): Json<Value>| async move {
                    if !ok_key(&h) {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    b2.lock().unwrap().push(("smart".into(), body));
                    Json(json!({ "assets": { "items": [asset(A1, "IMG_2041.jpg", "IMAGE"), asset(A2, "clip.mp4", "VIDEO")], "count": 2, "total": 2, "facets": [], "nextPage": null, "nextCursor": null }, "albums": { "items": [], "count": 0, "total": 0, "facets": [] } })).into_response()
                }),
            )
            .route(
                "/api/albums",
                get(|| async { Json(json!([{ "id": AL, "albumName": "Beach trip", "assetCount": 12, "albumThumbnailAssetId": A1 }])) }),
            )
            .route(
                "/api/albums/{id}",
                get(|| async { Json(json!({ "id": AL, "albumName": "Beach trip", "assetCount": 12, "albumThumbnailAssetId": A1 })) }),
            )
            .route("/api/assets/{id}", get(|Path(id): Path<String>| async move { Json(asset(&id, "IMG_2041.jpg", "IMAGE")) }))
            .route(
                "/api/assets/{id}/thumbnail",
                get(|Path(id): Path<String>, AxQuery(q): AxQuery<HashMap<String, String>>| async move {
                    ([("content-type", "image/webp")], format!("{id}:{}", q.get("size").cloned().unwrap_or_default()))
                }),
            )
            .route("/api/assets/{id}/original", get(|| async { StatusCode::FORBIDDEN }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn query(text: &str, kind: Option<&str>) -> Query {
        Query { text: text.into(), kind: kind.map(Into::into), limit: 20 }
    }

    #[tokio::test]
    async fn merges_name_smart_and_album_matches() {
        let bodies = Arc::default();
        let base = immich(Arc::clone(&bodies)).await;
        let s = ImmichSource::new(&base, "https://immich.example/", "good-key").unwrap();
        let hits = s.search(&query("IMG", None)).await.unwrap();
        let ids: Vec<_> = hits.iter().map(|h| h.doc.external_id.as_str()).collect();
        assert_eq!(ids, [format!("asset:{A1}"), format!("asset:{A2}")], "deduplicated, name match first; no album named IMG");

        let photo = &hits[0].doc;
        assert_eq!(photo.kind, "photo");
        assert_eq!(photo.path.as_deref(), Some("Lisbon, Portugal"));
        assert_eq!(photo.mtime, Some(1_720_981_800));
        assert_eq!(photo.size, Some(2048));
        assert_eq!(hits[1].doc.kind, "video");

        let sent = bodies.lock().unwrap().clone();
        assert!(sent.iter().any(|(k, b)| k == "smart" && b["query"] == "IMG" && b["size"] == 20));

        let hits = s.search(&query("beach", None)).await.unwrap();
        assert!(hits.iter().any(|h| h.doc.kind == "album" && h.doc.title == "Beach trip" && h.doc.path.as_deref() == Some("12 items")));
    }

    #[tokio::test]
    async fn kind_filters_skip_whats_not_wanted() {
        let base = immich(Arc::default()).await;
        let s = ImmichSource::new(&base, "https://immich.example", "good-key").unwrap();
        let albums = s.search(&query("beach", Some("album"))).await.unwrap();
        assert!(albums.iter().all(|h| h.doc.kind == "album") && !albums.is_empty());
        let videos = s.search(&query("anything", Some("video"))).await.unwrap();
        assert!(videos.iter().all(|h| h.doc.kind == "video") && !videos.is_empty());
        assert!(s.search(&query("anything", Some("file"))).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_bad_key_says_so() {
        let base = immich(Arc::default()).await;
        let s = ImmichSource::new(&base, "https://immich.example", "wrong").unwrap();
        assert!(matches!(s.search(&query("beach", Some("photo"))).await, Err(SourceError::Config(_))));
        assert!(matches!(s.check().await, Err(SourceError::Config(_))));
        let good = ImmichSource::new(&base, "https://immich.example", "good-key").unwrap();
        good.check().await.unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_immich_is_unavailable() {
        let s = ImmichSource::new("http://127.0.0.1:9", "https://immich.example", "k").unwrap();
        assert!(matches!(s.search(&query("beach", Some("photo"))).await, Err(SourceError::Unavailable(_))));
    }

    #[tokio::test]
    async fn items_previews_bytes_and_links() {
        let base = immich(Arc::default()).await;
        let s = ImmichSource::new(&base, "https://immich.example", "good-key").unwrap();
        let photo = format!("asset:{A1}");
        assert_eq!(s.get(&photo).await.unwrap().title, "IMG_2041.jpg");
        assert_eq!(s.preview(&photo).await.unwrap(), Preview::Image);

        let blob = s.blob(&photo, BlobVariant::Preview).await.unwrap();
        assert_eq!(blob.mime, "image/webp");
        let BlobBody::Bytes(b) = blob.body else { panic!() };
        assert_eq!(b, format!("{A1}:preview").as_bytes());

        // No asset.download on this key.
        assert!(matches!(s.blob(&photo, BlobVariant::Original).await, Err(SourceError::Config(_))));

        let album = format!("album:{AL}");
        let BlobBody::Bytes(b) = s.blob(&album, BlobVariant::Thumbnail).await.unwrap().body else { panic!() };
        assert_eq!(b, format!("{A1}:thumbnail").as_bytes(), "an album's cover");

        let doc = s.get(&photo).await.unwrap();
        assert_eq!(s.deep_link(&doc).unwrap(), format!("https://immich.example/photos/{A1}"));
        assert_eq!(s.deep_link(&s.get(&album).await.unwrap()).unwrap(), format!("https://immich.example/albums/{AL}"));
    }

    #[test]
    fn ids_must_be_well_formed() {
        assert!(parse_id(&format!("asset:{A1}")).is_ok());
        for bad in ["asset:../../admin", "asset:", "people:x", A1, "album:1234"] {
            assert!(parse_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_timestamps() {
        assert_eq!(unix_secs("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(unix_secs("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(unix_secs("nope"), None);
    }
}
