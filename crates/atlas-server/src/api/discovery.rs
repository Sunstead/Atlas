//! Ways into Atlas from outside its own pages:
//! - `/opensearch.xml` (public): lets a browser add Atlas as a search engine,
//!   so typing in the address bar searches it (`/search?q=...`);
//! - `/v1/suggest`: OpenSearch suggestions, matching titles as you type;
//! - `/v1/apps`: the launcher's list of apps.

use crate::auth::session_user;
use crate::state::AppState;
use atlas_common::AppLink;
use atlas_index::SearchQuery;
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The OpenSearch description. Firefox offers "Add Atlas" once a page links
/// it (`<link rel="search">` in index.html).
pub async fn opensearch(State(state): State<AppState>) -> Response {
    let base = state.public_url.as_str().trim_end_matches('/');
    let base = xml_escape(base);
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<OpenSearchDescription xmlns="http://a9.com/-/spec/opensearch/1.1/" xmlns:moz="http://www.mozilla.org/2006/browser/search/">
  <ShortName>Atlas</ShortName>
  <Description>Search your files and photos</Description>
  <InputEncoding>UTF-8</InputEncoding>
  <Image type="image/svg+xml">{base}/favicon.svg</Image>
  <Url type="text/html" method="get" template="{base}/search?q={{searchTerms}}"/>
  <Url type="application/x-suggestions+json" method="get" template="{base}/v1/suggest?q={{searchTerms}}"/>
  <moz:SearchForm>{base}/search</moz:SearchForm>
</OpenSearchDescription>
"#
    );
    ([(header::CONTENT_TYPE, "application/opensearchdescription+xml"), (header::CACHE_CONTROL, "public, max-age=3600")], xml)
        .into_response()
}

#[derive(Deserialize)]
pub struct SuggestParams {
    q: Option<String>,
}

const SUGGESTIONS: usize = 8;

/// `["query", ["title", ...]]`, the OpenSearch suggestions format. Signed
/// out (browsers may not send cookies with suggestion requests), it's just
/// empty: suggestions are a nicety, never a reason to sign in.
pub async fn suggest(State(state): State<AppState>, headers: HeaderMap, Query(p): Query<SuggestParams>) -> Response {
    let q: String = p.q.unwrap_or_default().trim().chars().take(200).collect();
    let empty = || Json(serde_json::json!([q.clone(), Vec::<String>::new()])).into_response();
    if q.is_empty() {
        return empty();
    }
    let Ok(Some(user)) = session_user(&state, &headers).await else { return empty() };

    let index = state.indexer.index.clone();
    let query = SearchQuery { text: q.clone(), kind: None, connection: None, limit: SUGGESTIONS * 2 };
    let hits = tokio::task::spawn_blocking(move || index.search(user.id.get(), &query)).await;
    let Ok(Ok(hits)) = hits else { return empty() };

    let enabled: std::collections::HashSet<i64> = match state.db.connections(user.id).await {
        Ok(c) => c.into_iter().filter(|c| c.enabled).map(|c| c.id).collect(),
        Err(_) => return empty(),
    };
    let mut titles: Vec<String> = Vec::new();
    for h in hits.into_iter().filter(|h| enabled.contains(&h.row.connection_id)) {
        if !titles.contains(&h.row.title) {
            titles.push(h.row.title);
        }
        if titles.len() == SUGGESTIONS {
            break;
        }
    }
    let mut res = Json(serde_json::json!([q, titles])).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("private, no-store"));
    res
}

pub async fn apps(State(state): State<AppState>, _: crate::auth::CurrentUser) -> Json<Vec<AppLink>> {
    Json(state.apps.as_ref().clone())
}

#[cfg(test)]
mod tests {
    use crate::state::tests::dev_state;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn opensearch_points_at_search_and_is_public() {
        let app = crate::app(dev_state(), None);
        let res = app.oneshot(Request::get("/opensearch.xml").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/opensearchdescription+xml");
        let body = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
        assert!(body.contains(r#"template="http://localhost:1420/search?q={searchTerms}""#), "{body}");
        assert!(body.contains("/v1/suggest?q={searchTerms}"));
    }

    #[tokio::test]
    async fn suggestions_signed_out_are_empty_not_an_error() {
        let app = crate::app(dev_state(), None);
        let res = app.oneshot(Request::get("/v1/suggest?q=tax").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], br#"["tax",[]]"#);
    }
}
