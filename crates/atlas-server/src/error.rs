//! `ApiError` as an HTTP response: the status is for proxies and logs, the
//! code is what the client acts on.

use atlas_common::{ApiError, ErrorCode};
use atlas_state::StateError;
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

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }
}

impl From<StateError> for AppError {
    fn from(e: StateError) -> Self {
        match e {
            StateError::NotFound => Self::not_found(),
            StateError::Conflict(m) => Self::new(ErrorCode::Conflict, m),
            e => {
                // Details stay in the log; they can name paths and keys.
                tracing::error!(error = %e, "state database");
                Self::new(ErrorCode::Internal, "Something went wrong on the server")
            }
        }
    }
}

fn status(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::Forbidden => StatusCode::FORBIDDEN,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Conflict => StatusCode::CONFLICT,
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
