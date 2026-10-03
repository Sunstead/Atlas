//! `ApiError` as an HTTP response: the status is for proxies and logs, the
//! code is what the client acts on.

use atlas_common::{ApiError, ErrorCode};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

pub struct AppError(pub ApiError);

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(ApiError { code, message: message.into(), detail: None })
    }

    pub fn not_found() -> Self {
        Self::new(ErrorCode::NotFound, "Not found")
    }
}

fn status(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::Forbidden => StatusCode::FORBIDDEN,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::NotEnabled => StatusCode::NOT_IMPLEMENTED,
        ErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (status(self.0.code), Json(self.0)).into_response()
    }
}
