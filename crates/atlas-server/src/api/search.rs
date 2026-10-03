//! `GET /v1/search`: one query across the user's enabled connections. The
//! index answers for indexed sources; federated sources (Immich, from M3)
//! will be asked in parallel, each with a deadline, and merged in.

use crate::api::items::encode_id;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::state::AppState;
use atlas_common::{ItemRef, SearchHit, SearchResponse, Snippet, SourceState, SourceStatus};
use atlas_core::Doc;
use atlas_index::{DocRow, SearchQuery};
use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use std::collections::HashMap;

const DEFAULT_LIMIT: usize = 30;
const MAX_LIMIT: usize = 100;
const MAX_QUERY: usize = 500;

#[derive(Deserialize)]
pub struct SearchParams {
    q: Option<String>,
    /// Item kind: `file`, `photo`, `album`.
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

pub async fn search(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(p): Query<SearchParams>,
) -> Result<Json<SearchResponse>, AppError> {
    let text: String = p.q.unwrap_or_default().trim().chars().take(MAX_QUERY).collect();
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let connections: HashMap<i64, _> = state
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

    let mut sources: Vec<SourceStatus> = connections
        .values()
        .map(|c| SourceStatus {
            connection: c.id,
            source: c.kind.clone(),
            label: c.label.clone(),
            state: SourceState::Ok,
            message: None,
        })
        .collect();
    sources.sort_by_key(|s| s.connection);

    if text.is_empty() || connections.is_empty() {
        return Ok(Json(SearchResponse { hits: Vec::new(), sources }));
    }

    let only = (connections.len() == 1).then(|| *connections.keys().next().unwrap());
    let query = SearchQuery { text, kind: p.kind, connection: only, limit: limit * 2 };
    let index = state.indexer.index.clone();
    let user_id = user.id.get();
    let found = tokio::task::spawn_blocking(move || index.search(user_id, &query))
        .await?
        .map_err(|e| {
            tracing::error!(error = %e, "search failed");
            AppError::new(atlas_common::ErrorCode::Internal, "Search failed")
        })?;

    let hits = found
        .into_iter()
        .filter_map(|h| {
            let conn = connections.get(&h.row.connection_id)?;
            let doc = as_doc(&h.row);
            let url = state.indexer.source(conn).ok().and_then(|s| s.deep_link(&doc));
            Some(SearchHit {
                item: ItemRef { connection: conn.id, source: conn.kind.clone(), id: encode_id(&h.row.external_id) },
                title: h.row.title,
                path: h.row.path,
                kind: h.row.kind,
                mime: h.row.mime,
                size: h.row.size,
                modified: h.row.mtime,
                snippet: h.snippet.map(|(text, ranges)| Snippet { highlights: utf16_ranges(&text, &ranges), text }),
                url,
            })
        })
        .take(limit)
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
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users");
        std::fs::create_dir_all(users.join("pwb/Documents/Taxes")).unwrap();
        std::fs::write(users.join("pwb/Documents/Taxes/2025 return.txt"), "The refund arrives in March.").unwrap();
        std::fs::write(users.join("pwb/Documents/plan.md"), "# Plan\n\nShip **Atlas** search.").unwrap();
        std::fs::write(users.join("pwb/page.html"), "<script>alert(1)</script>").unwrap();
        std::fs::create_dir_all(users.join("kim")).unwrap();
        std::fs::write(users.join("kim/diary.txt"), "kim's refund secret").unwrap();

        let urls = ServiceUrls { api: Url::parse("http://opencloud:9200").unwrap(), public: Url::parse("https://oc.example").unwrap() };
        let sources = SourcesConfig { immich: None, opencloud: Some(OpenCloudConfig { urls, users_dir: users.clone() }) };
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
