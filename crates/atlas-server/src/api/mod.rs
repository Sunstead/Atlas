//! The HTTP API: `/healthz` (public, for Cosmos uptime checks) and `/v1`.

use crate::error::AppError;
use atlas_common::ServerInfo;
use axum::{routing::get, Json, Router};

/// Bumped when `/v1` changes incompatibly.
pub const API_VERSION: u32 = 1;

pub fn router() -> Router {
    let v1 = Router::new()
        .route("/info", get(info))
        // Unknown API paths are JSON 404s, never the app shell.
        .fallback(|| async { AppError::not_found() });

    Router::new().route("/healthz", get(healthz)).nest("/v1", v1)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn info() -> Json<ServerInfo> {
    Json(ServerInfo { version: env!("CARGO_PKG_VERSION").to_owned(), api_version: API_VERSION })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    async fn get(path: &str) -> (StatusCode, String) {
        let res = router().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn healthz_is_ok() {
        assert_eq!(get("/healthz").await, (StatusCode::OK, "ok".into()));
    }

    #[tokio::test]
    async fn info_reports_the_version() {
        let (status, body) = get("/v1/info").await;
        assert_eq!(status, StatusCode::OK);
        let info: ServerInfo = serde_json::from_str(&body).unwrap();
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.api_version, API_VERSION);
    }

    #[tokio::test]
    async fn unknown_api_paths_are_json_404s() {
        let (status, body) = get("/v1/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains(r#""code":"not_found""#), "{body}");
    }
}
