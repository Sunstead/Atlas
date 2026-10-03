use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Served unauthenticated from `/v1/info`, so the web app can tell which
/// server it's talking to before anyone signs in. Nothing here is secret.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct ServerInfo {
    /// The `atlas-server` crate version.
    pub version: String,
    /// Bumped when `/v1` changes incompatibly.
    pub api_version: u32,
}
