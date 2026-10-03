//! The HTTP API: `/healthz` (public, for Cosmos uptime checks) and `/v1`.
//! Everything under `/v1` except `info` needs a session ([`CurrentUser`]),
//! and every state-changing request passes the CSRF guard.

mod connections;
pub mod csrf;
mod discovery;
pub mod items;
mod search;

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::state::AppState;
use atlas_common::{Me, ServerInfo};
use axum::{
    middleware,
    routing::{get, patch, post},
    Json, Router,
};

/// Bumped when `/v1` changes incompatibly.
pub const API_VERSION: u32 = 1;

pub fn router(state: AppState) -> Router<AppState> {
    let v1 = Router::new()
        .route("/info", get(info))
        .route("/me", get(me))
        .route("/source-kinds", get(connections::kinds))
        .route("/connections", get(connections::list).post(connections::create))
        .route("/connections/{id}", patch(connections::update).delete(connections::remove))
        .route("/connections/{id}/sync", post(connections::sync))
        .route("/search", get(search::search))
        .route("/suggest", get(discovery::suggest))
        .route("/apps", get(discovery::apps))
        .route("/items/{conn}/{id}", get(items::item))
        .route("/items/{conn}/{id}/preview", get(items::preview))
        .route("/items/{conn}/{id}/blob", get(items::blob))
        // Unknown API paths are JSON 404s, never the app shell.
        .fallback(|| async { AppError::not_found() })
        .layer(middleware::from_fn_with_state(state, csrf::guard));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/opensearch.xml", get(discovery::opensearch))
        .nest("/v1", v1)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn info() -> Json<ServerInfo> {
    Json(ServerInfo { version: env!("CARGO_PKG_VERSION").to_owned(), api_version: API_VERSION })
}

async fn me(CurrentUser(user): CurrentUser) -> Json<Me> {
    Json(Me { username: user.username, display_name: user.display_name, email: user.email })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::dev_state;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    async fn get(path: &str) -> (StatusCode, String) {
        let app = crate::app(dev_state(), None);
        let res = app.oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
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

    #[tokio::test]
    async fn me_needs_a_session() {
        let (status, body) = get("/v1/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains(r#""code":"unauthorized""#), "{body}");
    }
}
