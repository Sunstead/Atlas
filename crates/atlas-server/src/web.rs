//! Serves the web app build from Atlas's own origin, so the browser needs no
//! CORS and the session cookie covers both. Adapted from Cosmos
//! (`cosmos-agent/src/api/web.rs`).

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use std::path::Path;
use tower_http::{compression::CompressionLayer, services::ServeDir};

/// Path prefixes owned by the server. Anything under them that no route
/// matched is a 404, never the app shell.
const SERVER_PREFIXES: &[&str] = &["/v1", "/auth"];

/// `None` when `dir` holds no `index.html`, so a bad path degrades to API only.
pub fn router(dir: &Path) -> Option<Router> {
    if !dir.join("index.html").is_file() {
        tracing::warn!(dir = %dir.display(), "ATLAS_WEB_DIR has no index.html; not serving the web app");
        return None;
    }
    tracing::info!(dir = %dir.display(), "serving web app");

    let files = ServeDir::new(dir);
    Some(
        Router::new()
            .fallback(move |req: Request| serve(files.clone(), req))
            .layer(CompressionLayer::new().gzip(true)),
    )
}

fn is_server_path(path: &str) -> bool {
    SERVER_PREFIXES
        .iter()
        .any(|p| path == *p || path.strip_prefix(p).is_some_and(|rest| rest.starts_with('/')))
}

async fn serve(mut files: ServeDir, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    if is_server_path(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    // Hashed build output never changes; everything else must revalidate so a
    // deploy is picked up on the next load.
    let asset = path.starts_with("/assets/");
    let mut res = match files.try_call(req).await {
        Ok(res) => res.map(Body::new),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Client-side routes (`/search?q=`) get the app shell. Missing assets stay
    // 404 so a stale chunk isn't cached as HTML.
    if res.status() == StatusCode::NOT_FOUND && !asset {
        let index = Request::builder().uri("/index.html").body(Body::empty()).unwrap_or_default();
        res = match files.try_call(index).await {
            Ok(res) => res.map(Body::new),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
    }

    let cache = if asset && res.status().is_success() {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let headers = res.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    fn site() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<!doctype html>app").unwrap();
        std::fs::create_dir(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("assets/app-abc.js"), "js").unwrap();
        let router = router(dir.path()).unwrap();
        (dir, router)
    }

    async fn get(router: &Router, path: &str) -> Response {
        router.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap()
    }

    async fn text(res: Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn serves_index_and_client_routes() {
        let (_dir, r) = site();
        for path in ["/", "/search?q=notes", "/item/abc/def"] {
            let res = get(&r, path).await;
            assert_eq!(res.status(), StatusCode::OK, "{path}");
            assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
            assert!(text(res).await.contains("app"));
        }
    }

    #[tokio::test]
    async fn assets_are_immutable_and_missing_ones_404() {
        let (_dir, r) = site();
        let res = get(&r, "/assets/app-abc.js").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers()[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
        assert_eq!(get(&r, "/assets/gone.js").await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn server_paths_are_not_the_app_shell() {
        let (_dir, r) = site();
        for path in ["/v1", "/v1/nope", "/auth/nope"] {
            assert_eq!(get(&r, path).await.status(), StatusCode::NOT_FOUND, "{path}");
        }
        // Only whole segments count.
        assert_eq!(get(&r, "/v1beta").await.status(), StatusCode::OK);
    }

    #[test]
    fn missing_index_disables_the_app() {
        let dir = tempfile::tempdir().unwrap();
        assert!(router(dir.path()).is_none());
    }
}
