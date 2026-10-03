//! `GET /v1/search`: one query across the user's enabled connections.
//!
//! The index answers for indexed sources (OpenCloud); federated sources
//! (Immich) are asked at the same time, each with [`FEDERATED_DEADLINE`].
//! A source that fails or runs out of time is reported in `sources`, and
//! everyone else's results still come back.
//!
//! Ranked lists from different engines can't be compared by score (BM25
//! against CLIP similarity), so they're merged by rank: Reciprocal Rank
//! Fusion, plus a small nudge for titles that contain the query.

use crate::api::items::encode_id;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::sources;
use crate::state::AppState;
use atlas_common::{ItemRef, SearchHit, SearchResponse, Snippet, SourceState, SourceStatus};
use atlas_core::{Doc, Query as SourceQuery, SourceError};
use atlas_index::{DocRow, SearchQuery};
use atlas_state::ConnectionRow;
use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

const DEFAULT_LIMIT: usize = 30;
const MAX_LIMIT: usize = 100;
const MAX_QUERY: usize = 500;

/// How long a federated source gets. Immich's smart search runs a CLIP model
/// on the query, which takes a moment on a CPU.
#[cfg(not(test))]
pub const FEDERATED_DEADLINE: Duration = Duration::from_millis(2500);
#[cfg(test)]
pub const FEDERATED_DEADLINE: Duration = Duration::from_millis(400);

/// RRF's `k`: higher flattens the advantage of the top few ranks.
const RRF_K: f64 = 60.0;

#[derive(Deserialize)]
pub struct SearchParams {
    q: Option<String>,
    /// Item kind: `file`, `photo`, `video`, `album`.
    #[serde(rename = "type")]
    kind: Option<String>,
    /// A connection id, or a source kind (`opencloud`).
    source: Option<String>,
    limit: Option<usize>,
}

/// UTF-16 offsets for byte ranges in `text`, as JavaScript string indexes.
fn utf16_ranges(text: &str, ranges: &[(usize, usize)]) -> Vec<[u32; 2]> {
    let at = |byte: usize| text.get(..byte).map(|s| s.encode_utf16().count() as u32);
    ranges.iter().filter_map(|&(s, e)| Some([at(s)?, at(e)?])).collect()
}

fn as_doc(row: &DocRow) -> Doc {
    Doc {
        external_id: row.external_id.clone(),
        kind: row.kind.clone(),
        title: row.title.clone(),
        path: row.path.clone(),
        mime: row.mime.clone(),
        size: row.size,
        mtime: row.mtime,
        fingerprint: row.fingerprint.clone(),
        body: None,
    }
}

/// One result before merging.
#[derive(Debug, Clone)]
struct Candidate {
    connection: i64,
    doc: Doc,
    snippet: Option<Snippet>,
    thumbnail: bool,
}

/// Merges ranked lists by Reciprocal Rank Fusion. Each list contributes
/// `1 / (k + rank)` per item; a title containing the whole query gets half
/// a top rank's worth on top. Ties keep list order.
fn fuse(lists: Vec<Vec<Candidate>>, query: &str, limit: usize) -> Vec<Candidate> {
    let q = query.to_lowercase();
    let mut scored: Vec<(f64, usize, usize, Candidate)> = Vec::new();
    for (li, list) in lists.into_iter().enumerate() {
        for (rank, c) in list.into_iter().enumerate() {
            let mut score = 1.0 / (RRF_K + rank as f64 + 1.0);
            if !q.is_empty() && c.doc.title.to_lowercase().contains(&q) {
                score += 0.5 / (RRF_K + 1.0);
            }
            scored.push((score, rank, li, c));
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().take(limit).map(|(_, _, _, c)| c).collect()
}

/// Which kinds a source can return, to skip asking it for others.
fn offers(kind: &str, wanted: Option<&str>) -> bool {
    let Some(w) = wanted else { return true };
    match kind {
        sources::OPENCLOUD => w == "file",
        sources::IMMICH => matches!(w, "photo" | "video" | "album"),
        _ => true,
    }
}

fn status(c: &ConnectionRow, state: SourceState, message: Option<String>) -> SourceStatus {
    SourceStatus { connection: c.id, source: c.kind.clone(), label: c.label.clone(), state, message }
}

/// Asks one federated source, within the deadline.
async fn ask(state: &AppState, conn: &ConnectionRow, q: &SourceQuery) -> (SourceStatus, Vec<Candidate>) {
    let source = match state.indexer.source(conn) {
        Ok(s) => s,
        Err(e) => return (status(conn, SourceState::Error, Some(e.to_string())), Vec::new()),
    };
    let Some(federated) = source.as_federated() else { return (status(conn, SourceState::Ok, None), Vec::new()) };
    match tokio::time::timeout(FEDERATED_DEADLINE, federated.search(q)).await {
        Err(_) => (status(conn, SourceState::Timeout, None), Vec::new()),
        Ok(Err(e)) => {
            if !matches!(e, SourceError::Config(_)) {
                tracing::info!(connection = conn.id, error = %e, "federated search failed");
            }
            (status(conn, SourceState::Error, Some(e.to_string())), Vec::new())
        }
        Ok(Ok(hits)) => (
            status(conn, SourceState::Ok, None),
            hits.into_iter()
                .map(|h| Candidate { connection: conn.id, doc: h.doc, snippet: None, thumbnail: h.has_thumbnail })
                .collect(),
        ),
    }
}

pub async fn search(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(p): Query<SearchParams>,
) -> Result<Json<SearchResponse>, AppError> {
    let text: String = p.q.unwrap_or_default().trim().chars().take(MAX_QUERY).collect();
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let kind = p.kind.filter(|k| !k.is_empty());

    let connections: HashMap<i64, ConnectionRow> = state
        .db
        .connections(user.id)
        .await?
        .into_iter()
        .filter(|c| c.enabled)
        .filter(|c| match p.source.as_deref() {
            None => true,
            Some(s) => c.kind == s || c.id.to_string() == s,
        })
        .map(|c| (c.id, c))
        .collect();

    let mut ordered: Vec<&ConnectionRow> = connections.values().collect();
    ordered.sort_by_key(|c| c.id);

    if text.is_empty() || connections.is_empty() {
        let sources = ordered.iter().map(|c| status(c, SourceState::Ok, None)).collect();
        return Ok(Json(SearchResponse { hits: Vec::new(), sources }));
    }

    // The index, for every indexed connection at once.
    let indexed: Vec<i64> = ordered
        .iter()
        .filter(|c| sources::is_indexed(&c.kind) && offers(&c.kind, kind.as_deref()))
        .map(|c| c.id)
        .collect();
    let local = async {
        if indexed.is_empty() {
            return Ok(Vec::new());
        }
        let only = (indexed.len() == 1).then(|| indexed[0]);
        let query = SearchQuery { text: text.clone(), kind: kind.clone(), connection: only, limit: limit * 2 };
        let index = state.indexer.index.clone();
        let user_id = user.id.get();
        let found = tokio::task::spawn_blocking(move || index.search(user_id, &query)).await?.map_err(|e| {
            tracing::error!(error = %e, "search failed");
            AppError::new(atlas_common::ErrorCode::Internal, "Search failed")
        })?;
        Ok::<_, AppError>(
            found
                .into_iter()
                .filter(|h| indexed.contains(&h.row.connection_id))
                .map(|h| Candidate {
                    connection: h.row.connection_id,
                    doc: as_doc(&h.row),
                    snippet: h.snippet.map(|(text, ranges)| Snippet { highlights: utf16_ranges(&text, &ranges), text }),
                    thumbnail: false,
                })
                .collect(),
        )
    };

    // Federated connections, all at once.
    let squery = SourceQuery { text: text.clone(), kind: kind.clone(), limit };
    let federated = futures_util::future::join_all(
        ordered
            .iter()
            .filter(|c| !sources::is_indexed(&c.kind) && offers(&c.kind, kind.as_deref()))
            .map(|c| ask(&state, c, &squery)),
    );

    let (local, federated) = tokio::join!(local, federated);
    let mut lists = vec![local?];
    let mut statuses: HashMap<i64, SourceStatus> = HashMap::new();
    for (status, hits) in federated {
        statuses.insert(status.connection, status);
        lists.push(hits);
    }
    let sources = ordered
        .iter()
        .map(|c| statuses.remove(&c.id).unwrap_or_else(|| status(c, SourceState::Ok, None)))
        .collect();

    let hits = fuse(lists, &text, limit)
        .into_iter()
        .filter_map(|c| {
            let conn = connections.get(&c.connection)?;
            let source = state.indexer.source(conn).ok();
            let url = source.as_ref().and_then(|s| s.deep_link(&c.doc));
            let thumbnail = c.thumbnail || source.as_ref().is_some_and(|s| s.has_thumbnail(&c.doc));
            Some(SearchHit {
                item: ItemRef { connection: conn.id, source: conn.kind.clone(), id: encode_id(&c.doc.external_id) },
                title: c.doc.title,
                path: c.doc.path,
                kind: c.doc.kind,
                mime: c.doc.mime,
                size: c.doc.size,
                modified: c.doc.mtime,
                snippet: c.snippet,
                thumbnail,
                url,
            })
        })
        .collect();

    Ok(Json(SearchResponse { hits, sources }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthMode, OpenCloudConfig, ServiceUrls, SourcesConfig};
    use crate::state::tests::state;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use std::path::PathBuf;
    use std::time::Duration;
    use tower::ServiceExt;
    use url::Url;

    struct World {
        _dir: tempfile::TempDir,
        users: PathBuf,
        app: Router,
        state: AppState,
        cookie: String,
    }

    /// Two users' OpenCloud spaces on disk; signed in (dev mode) as pwb.
    async fn world() -> World {
        world_with(None).await
    }

    async fn world_with(immich: Option<&str>) -> World {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users");
        std::fs::create_dir_all(users.join("pwb/Documents/Taxes")).unwrap();
        std::fs::write(users.join("pwb/Documents/Taxes/2025 return.txt"), "The refund arrives in March.").unwrap();
        std::fs::write(users.join("pwb/Documents/plan.md"), "# Plan\n\nShip **Atlas** search.").unwrap();
        std::fs::write(users.join("pwb/page.html"), "<script>alert(1)</script>").unwrap();
        std::fs::create_dir_all(users.join("kim")).unwrap();
        std::fs::write(users.join("kim/diary.txt"), "kim's refund secret").unwrap();

        let urls = ServiceUrls { api: Url::parse("http://opencloud:9200").unwrap(), public: Url::parse("https://oc.example").unwrap() };
        let immich = immich.map(|u| ServiceUrls { api: Url::parse(u).unwrap(), public: Url::parse("https://immich.example").unwrap() });
        let sources = SourcesConfig { immich, opencloud: Some(OpenCloudConfig { urls, users_dir: users.clone(), storage_id: None }) };
        let state = state(AuthMode::Dev { username: "pwb".into() }, sources);
        state.indexer.start();
        let app = crate::app(state.clone(), None);
        let res = app.clone().oneshot(Request::get("/auth/login").body(Body::empty()).unwrap()).await.unwrap();
        let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
        World { _dir: dir, users, app, state, cookie }
    }

    impl World {
        async fn call(&self, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
            let mut req = Request::builder().method(method).uri(uri).header(header::COOKIE, &self.cookie).header("x-atlas-request", "1");
            if body.is_some() {
                req = req.header(header::CONTENT_TYPE, "application/json");
            }
            let res = self.app.clone().oneshot(req.body(body.map(|b| Body::from(b.to_owned())).unwrap_or_default()).unwrap()).await.unwrap();
            let (status, headers) = (res.status(), res.headers().clone());
            (status, headers, axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec())
        }

        async fn json(&self, uri: &str) -> serde_json::Value {
            let (status, _, body) = self.call("GET", uri, None).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {}", String::from_utf8_lossy(&body));
            serde_json::from_slice(&body).unwrap()
        }

        /// Waits for a search to return `n` hits.
        async fn hits(&self, q: &str, n: usize) -> serde_json::Value {
            for _ in 0..100 {
                let r = self.json(&format!("/v1/search?q={q}")).await;
                if r["hits"].as_array().unwrap().len() == n {
                    return r;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("no {n} hits for {q}: {}", self.json(&format!("/v1/search?q={q}")).await);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_sync_search_preview() {
        let w = world().await;
        let (status, _, _) = w.call("POST", "/v1/connections", Some(r#"{"kind":"opencloud"}"#)).await;
        assert_eq!(status, StatusCode::CREATED);

        let r = w.hits("refund", 1).await;
        let hit = &r["hits"][0];
        assert_eq!(hit["title"], "2025 return.txt");
        assert_eq!(hit["path"], "Documents/Taxes");
        assert_eq!(hit["url"], "https://oc.example/files/spaces/personal/pwb/Documents/Taxes");
        let snippet = hit["snippet"]["text"].as_str().unwrap();
        assert!(snippet.contains("refund"));
        assert_eq!(r["sources"][0]["state"], "ok");

        // kim's files exist on disk, but they aren't pwb's.
        assert!(!r.to_string().contains("kim"));

        // Item, preview and bytes.
        let conn = hit["item"]["connection"].as_i64().unwrap();
        let id = hit["item"]["id"].as_str().unwrap();
        let item = w.json(&format!("/v1/items/{conn}/{id}")).await;
        assert_eq!(item["source_label"], "OpenCloud");
        let preview = w.json(&format!("/v1/items/{conn}/{id}/preview")).await;
        assert_eq!(preview["type"], "text");
        let (status, headers, bytes) = w.call("GET", &format!("/v1/items/{conn}/{id}/blob"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes, b"The refund arrives in March.");
        assert!(headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap().starts_with("sandbox"));

        // A user's HTML is served sandboxed, never as an Atlas page.
        let html = encode_id("page.html");
        let (_, headers, _) = w.call("GET", &format!("/v1/items/{conn}/{html}/blob"), None).await;
        assert!(headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("sandbox"));
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

        // Ids can't climb out of the user's folder.
        let escape = encode_id("../kim/diary.txt");
        let (status, _, _) = w.call("GET", &format!("/v1/items/{conn}/{escape}/blob"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Suggestions match titles as they're typed.
        let (_, _, body) = w.call("GET", "/v1/suggest?q=pla", None).await;
        assert_eq!(String::from_utf8(body).unwrap(), r#"["pla",["plan.md"]]"#);

        // Sync status shows on the connection.
        let list = w.json("/v1/connections").await;
        assert_eq!(list[0]["sync"]["items"], 3);
        assert!(list[0]["sync"]["last_synced_at"].is_number());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn another_users_connection_is_invisible() {
        let w = world().await;
        w.call("POST", "/v1/connections", Some(r#"{"kind":"opencloud"}"#)).await;
        w.hits("refund", 1).await;

        // kim connects too (directly, since dev sign-in is always pwb).
        let kim = w
            .state
            .db
            .upsert_user(atlas_state::NewUser {
                issuer: "dev".into(),
                subject: "kim".into(),
                username: "kim".into(),
                display_name: None,
                email: None,
            })
            .await
            .unwrap();
        let row = w
            .state
            .db
            .create_connection(
                kim.id,
                atlas_state::NewConnection {
                    kind: "opencloud".into(),
                    label: "OpenCloud".into(),
                    config: serde_json::json!({ "root": w.users.join("kim") }),
                    credential: None,
                },
                None,
            )
            .await
            .unwrap();
        w.state.indexer.changed(&row);
        for _ in 0..100 {
            if w.state.indexer.status(row.id).is_some_and(|s| s.items == 1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(w.state.indexer.status(row.id).unwrap().items, 1, "kim's space is indexed");

        // pwb still sees one refund, and can't open kim's items.
        let r = w.hits("refund", 1).await;
        assert_eq!(r["hits"][0]["title"], "2025 return.txt");
        let (status, _, _) = w.call("GET", &format!("/v1/items/{}/{}", row.id, encode_id("diary.txt")), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_files_appear_and_disconnecting_forgets() {
        let w = world().await;
        let (_, _, created) = w.call("POST", "/v1/connections", Some(r#"{"kind":"opencloud"}"#)).await;
        let id = serde_json::from_slice::<serde_json::Value>(&created).unwrap()["id"].as_i64().unwrap();
        w.hits("refund", 1).await;

        // The watcher picks up a new file without a manual sync.
        std::fs::write(w.users.join("pwb/Documents/receipt.txt"), "another refund, for April").unwrap();
        w.hits("refund", 2).await;

        // Pausing hides it from search; disconnecting removes its items.
        w.call("PATCH", &format!("/v1/connections/{id}"), Some(r#"{"enabled":false}"#)).await;
        w.hits("refund", 0).await;
        w.call("DELETE", &format!("/v1/connections/{id}"), None).await;
        assert_eq!(w.state.indexer.index.count(id).unwrap(), 0);
    }

    fn cand(conn: i64, title: &str) -> Candidate {
        Candidate {
            connection: conn,
            doc: Doc {
                external_id: title.into(),
                kind: "file".into(),
                title: title.into(),
                path: None,
                mime: None,
                size: None,
                mtime: None,
                fingerprint: None,
                body: None,
            },
            snippet: None,
            thumbnail: false,
        }
    }

    /// How the fake Immich behaves.
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Up,
        Slow,
        Down,
    }

    /// A fake Immich: one beach photo, any key but "bad-key" accepted.
    async fn fake_immich(mode: std::sync::Arc<std::sync::Mutex<Mode>>) -> String {
        use axum::{http::HeaderMap, response::IntoResponse, routing::{get, post}, Json};
        use serde_json::json;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let reply = move |h: HeaderMap, mode: std::sync::Arc<std::sync::Mutex<Mode>>| async move {
            if h.get("x-api-key").and_then(|v| v.to_str().ok()) == Some("bad-key") {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let m = *mode.lock().unwrap();
            if m == Mode::Down {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            if m == Mode::Slow {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let photo = json!({ "id": "11111111-1111-4111-8111-111111111111", "type": "IMAGE", "originalFileName": "beach refund.jpg",
                "originalMimeType": "image/jpeg", "localDateTime": "2024-07-14T18:30:00.000Z" });
            Json(json!({ "assets": { "items": [photo], "count": 1 }, "albums": { "items": [], "count": 0 } })).into_response()
        };
        let (m1, m2) = (mode.clone(), mode.clone());
        let app = Router::new()
            .route("/api/search/metadata", post(move |h: HeaderMap| reply(h, m1.clone())))
            .route("/api/search/smart", post(move |h: HeaderMap| reply(h, m2.clone())))
            .route("/api/albums", get(|| async { Json(json!([])) }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn photos_and_files_in_one_search_and_trouble_is_reported() {
        let mode = std::sync::Arc::new(std::sync::Mutex::new(Mode::Up));
        let immich = fake_immich(mode.clone()).await;
        let w = world_with(Some(&immich)).await;
        w.call("POST", "/v1/connections", Some(r#"{"kind":"opencloud"}"#)).await;

        // A key Immich refuses is refused here, before it's saved.
        let (status, _, body) = w.call("POST", "/v1/connections", Some(r#"{"kind":"immich","credential":"bad-key"}"#)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}", String::from_utf8_lossy(&body));
        let (status, _, _) = w.call("POST", "/v1/connections", Some(r#"{"kind":"immich","credential":"good-key"}"#)).await;
        assert_eq!(status, StatusCode::CREATED);

        // One search, both sources: the file from the index, the photo from Immich.
        let r = w.hits("refund", 2).await;
        let kinds: Vec<&str> = r["hits"].as_array().unwrap().iter().map(|h| h["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"file") && kinds.contains(&"photo"), "{r}");
        let photo = r["hits"].as_array().unwrap().iter().find(|h| h["kind"] == "photo").unwrap();
        assert_eq!(photo["url"], "https://immich.example/photos/11111111-1111-4111-8111-111111111111");
        assert_eq!(photo["thumbnail"], true);
        assert!(r["sources"].as_array().unwrap().iter().all(|s| s["state"] == "ok"));

        // type=photo asks only Immich.
        let r = w.json("/v1/search?q=refund&type=photo").await;
        assert!(r["hits"].as_array().unwrap().iter().all(|h| h["kind"] == "photo"));

        // Immich failing: files still come back, and the status says why.
        *mode.lock().unwrap() = Mode::Down;
        let r = w.json("/v1/search?q=refund").await;
        assert_eq!(r["hits"].as_array().unwrap().len(), 1);
        let immich_status = r["sources"].as_array().unwrap().iter().find(|s| s["source"] == "immich").unwrap().clone();
        assert_eq!(immich_status["state"], "error");

        // Immich slow: the search doesn't wait past the deadline.
        *mode.lock().unwrap() = Mode::Slow;
        let started = std::time::Instant::now();
        let r = w.json("/v1/search?q=refund").await;
        assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
        assert_eq!(r["hits"].as_array().unwrap().len(), 1);
        let immich_status = r["sources"].as_array().unwrap().iter().find(|s| s["source"] == "immich").unwrap().clone();
        assert_eq!(immich_status["state"], "timeout");
    }

    #[test]
    fn fusion_interleaves_by_rank_and_favours_title_matches() {
        let files = vec![cand(1, "a"), cand(1, "b"), cand(1, "c")];
        let photos = vec![cand(2, "x"), cand(2, "y")];
        let titles = |v: Vec<Candidate>| v.into_iter().map(|c| c.doc.title).collect::<Vec<_>>();
        assert_eq!(titles(fuse(vec![files.clone(), photos], "zzz", 10)), ["a", "x", "b", "y", "c"]);
        // A title containing the query climbs past a rank above it.
        let photos = vec![cand(2, "x"), cand(2, "beach day")];
        let out = titles(fuse(vec![files, photos], "beach", 10));
        assert_eq!(&out[..3], ["beach day", "a", "x"]);
        assert_eq!(fuse(vec![vec![cand(1, "a"), cand(1, "b")]], "", 1).len(), 1);
    }

    #[test]
    fn kinds_go_to_the_sources_that_have_them() {
        assert!(offers("opencloud", Some("file")) && !offers("opencloud", Some("photo")));
        assert!(offers("immich", Some("album")) && !offers("immich", Some("file")));
        assert!(offers("immich", None));
    }

    #[test]
    fn converts_byte_ranges_to_utf16() {
        let text = "café refund 🎉 refund";
        let first = text.find("refund").unwrap();
        let second = text.rfind("refund").unwrap();
        let r = utf16_ranges(text, &[(first, first + 6), (second, second + 6)]);
        let js: Vec<u16> = text.encode_utf16().collect();
        for [s, e] in r {
            assert_eq!(String::from_utf16(&js[s as usize..e as usize]).unwrap(), "refund");
        }
        assert!(utf16_ranges("abc", &[(1, 99)]).is_empty(), "out of range is dropped");
    }
}
