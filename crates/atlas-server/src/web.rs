//! Serves the web app build from Atlas's own origin, so the browser needs no
//! CORS and the session cookie covers both. Adapted from Cosmos
//! (`cosmos-agent/src/api/web.rs`).
//!
//! The app shell is gated: a page load without a session goes to sign-in,
//! carrying the full path and query. That's what lets a search started from
//! the browser's address bar (`/search?q=...`) survive the round trip.
//! Static files (hashed assets, the favicon) are public; they hold no data.

use crate::auth::session_user;
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Request, State},
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
pub fn router(dir: &Path) -> Option<Router<AppState>> {
    if !dir.join("index.html").is_file() {
        tracing::warn!(dir = %dir.display(), "ATLAS_WEB_DIR has no index.html; not serving the web app");
        return None;
    }
    tracing::info!(dir = %dir.display(), "serving web app");

    let files = ServeDir::new(dir);
    Some(
        Router::new()
            .fallback(move |State(state): State<AppState>, req: Request| serve(state, files.clone(), req))
            .layer(CompressionLayer::new().gzip(true)),
    )
}

fn is_server_path(path: &str) -> bool {
    SERVER_PREFIXES
        .iter()
        .any(|p| path == *p || path.strip_prefix(p).is_some_and(|rest| rest.starts_with('/')))
}

async fn serve(state: AppState, mut files: ServeDir, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    if is_server_path(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path_and_query = req.uri().path_and_query().map(|p| p.as_str().to_owned()).unwrap_or_else(|| path.clone());
    let headers = req.headers().clone();

    // Hashed build output never changes; everything else must revalidate so a
    // deploy is picked up on the next load.
    let asset = path.starts_with("/assets/");
    let mut res = match files.try_call(req).await {
        Ok(res) => res.map(Body::new),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Client-side routes (`/search?q=`) get the app shell. Missing assets stay
    // 404 so a stale chunk isn't cached as HTML.
    let fallback = res.status() == StatusCode::NOT_FOUND && !asset;
    let shell = fallback || path == "/" || path == "/index.html";
    if shell {
        match session_user(&state, &headers).await {
            Ok(Some(_)) => {}
            Ok(None) => return sign_in(&path_and_query),
            Err(e) => return e.into_response(),
        }
    }
    if fallback {
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

fn sign_in(return_to: &str) -> Response {
    let to = format!(
        "/auth/login?return_to={}",
        url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect::<String>()
    );
    (StatusCode::FOUND, [(header::LOCATION, to.as_str()), (header::CACHE_CONTROL, "no-store")]).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::dev_state;
    use tower::ServiceExt;

    struct Site {
        _dir: tempfile::TempDir,
        app: Router,
        cookie: String,
    }

    async fn site() -> Site {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<!doctype html>app").unwrap();
        std::fs::write(dir.path().join("favicon.svg"), "<svg/>").unwrap();
        std::fs::create_dir(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("assets/app-abc.js"), "js").unwrap();
        let app = crate::app(dev_state(), Some(dir.path()));
        let res = app.clone().oneshot(Request::get("/auth/login").body(Body::empty()).unwrap()).await.unwrap();
        let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
        Site { _dir: dir, app, cookie }
    }

    async fn get(s: &Site, path: &str, signed_in: bool) -> Response {
        let mut req = Request::get(path);
        if signed_in {
            req = req.header(header::COOKIE, &s.cookie);
        }
        s.app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
    }

    async fn text(res: Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn serves_index_and_client_routes_when_signed_in() {
        let s = site().await;
        for path in ["/", "/search?q=notes", "/item/abc/def"] {
            let res = get(&s, path, true).await;
            assert_eq!(res.status(), StatusCode::OK, "{path}");
            assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
            assert!(text(res).await.contains("app"));
        }
    }

    #[tokio::test]
    async fn signed_out_page_loads_go_to_sign_in_with_the_query() {
        let s = site().await;
        let res = get(&s, "/search?q=tax%202025&type=file", false).await;
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(
            res.headers()[header::LOCATION],
            "/auth/login?return_to=%2Fsearch%3Fq%3Dtax%25202025%26type%3Dfile"
        );
        for path in ["/", "/index.html"] {
            assert_eq!(get(&s, path, false).await.status(), StatusCode::FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn static_files_are_public() {
        let s = site().await;
        assert_eq!(get(&s, "/favicon.svg", false).await.status(), StatusCode::OK);
        let res = get(&s, "/assets/app-abc.js", false).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers()[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
        assert_eq!(get(&s, "/assets/gone.js", false).await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn server_paths_are_not_the_app_shell() {
        let s = site().await;
        for path in ["/v1", "/v1/nope", "/auth/nope"] {
            assert_eq!(get(&s, path, true).await.status(), StatusCode::NOT_FOUND, "{path}");
        }
        // Only whole segments count.
        assert_eq!(get(&s, "/v1beta", true).await.status(), StatusCode::OK);
    }

    #[test]
    fn missing_index_disables_the_app() {
        let dir = tempfile::tempdir().unwrap();
        assert!(router(dir.path()).is_none());
    }
}
