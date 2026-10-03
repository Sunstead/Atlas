use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The signed-in user, from `/v1/me`.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct Me {
    /// The Authentik username. Data paths are keyed by it.
    pub username: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
}

/// From `POST /auth/logout`: where to send the browser next (the provider's
/// end-session page, so its session ends too, or `/`).
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct LogoutResponse {
    pub redirect: String,
}
