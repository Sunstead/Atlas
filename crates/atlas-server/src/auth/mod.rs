//! Who is asking. Sign-in is Authentik OIDC (`oidc`); the result is a
//! server-side session in a cookie (`cookie`), looked up per request by the
//! [`CurrentUser`] extractor. Sign-in routes are in `routes`.

pub mod cookie;
pub mod oidc;
mod page;
pub mod return_to;
mod routes;

pub use routes::router;

use crate::config::AuthMode as ConfigAuthMode;
use crate::error::AppError;
use crate::state::AppState;
use atlas_common::ErrorCode;
use atlas_state::User;
use axum::{extract::FromRequestParts, http::request::Parts, http::HeaderMap};
use cookie::CookieSpec;
use oidc::OidcClient;
use std::sync::Arc;
use url::Url;

pub enum Mode {
    Oidc(Arc<OidcClient>),
    Dev { username: String },
    Disabled,
}

pub struct Auth {
    pub mode: Mode,
    pub cookie: CookieSpec,
    /// `<public url>/auth/callback`, registered with the provider.
    pub redirect_uri: String,
}

impl Auth {
    pub fn new(mode: ConfigAuthMode, public_url: &Url) -> Self {
        let mode = match mode {
            ConfigAuthMode::Oidc(cfg) => Mode::Oidc(Arc::new(OidcClient::new(cfg))),
            ConfigAuthMode::Dev { username } => Mode::Dev { username },
            ConfigAuthMode::Disabled => Mode::Disabled,
        };
        Self {
            mode,
            cookie: CookieSpec::for_url(public_url),
            redirect_uri: public_url.join("/auth/callback").expect("a valid public URL").into(),
        }
    }
}

/// The signed-in user for a request's cookie, if any.
pub async fn session_user(state: &AppState, headers: &HeaderMap) -> Result<Option<User>, AppError> {
    let Some(token) = state.auth.cookie.read(headers) else { return Ok(None) };
    let Some(session) = state.db.session(&token).await? else { return Ok(None) };
    match state.db.user(session.user).await {
        Ok(u) => Ok(Some(u)),
        Err(atlas_state::StateError::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Extracts the signed-in user, or answers 401. Every per-user handler takes
/// this, and scopes its queries by `user.id`.
pub struct CurrentUser(pub User);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        session_user(state, &parts.headers)
            .await?
            .map(CurrentUser)
            .ok_or_else(|| AppError::new(ErrorCode::Unauthorized, "Sign in to continue"))
    }
}
