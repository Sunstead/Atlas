//! Cross-site request forgery guard for state-changing requests.
//!
//! The session cookie is `SameSite=Lax`, so a cross-site form POST wouldn't
//! carry it in current browsers anyway. This is the belt to that braces:
//! every non-GET request must send `X-Atlas-Request: 1` (a header a plain
//! form can't set, and a cross-site `fetch` can't without a CORS preflight
//! Atlas never answers), and an `Origin`, when present, must be Atlas's own.

use crate::error::AppError;
use crate::state::AppState;
use atlas_common::ErrorCode;
use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};

pub const HEADER: &str = "x-atlas-request";

pub async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    if req.headers().get(HEADER).and_then(|v| v.to_str().ok()) != Some("1") {
        return AppError::new(ErrorCode::Forbidden, "Missing X-Atlas-Request header").into_response();
    }
    if let Some(origin) = req.headers().get("origin").and_then(|v| v.to_str().ok()) {
        if origin != state.public_url.origin().ascii_serialization() {
            return AppError::new(ErrorCode::Forbidden, "Cross-origin request refused").into_response();
        }
    }
    next.run(req).await
}
