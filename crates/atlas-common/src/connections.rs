use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A kind of source a user can connect, from `/v1/source-kinds`. The
/// settings page renders its form from this.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct SourceKindInfo {
    /// Stable id: `opencloud`, `immich`.
    pub kind: String,
    pub name: String,
    pub description: String,
    /// What the user pastes in, if anything.
    pub credential: Option<CredentialInfo>,
    /// False when the server isn't configured for this kind; the reason says
    /// what's missing. Hide or grey out rather than offer it.
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct CredentialInfo {
    /// The field label, e.g. "API key".
    pub label: String,
    /// One line on where to get it.
    pub help: String,
    /// Where the user creates one, if there's a page for it.
    pub url: Option<String>,
    pub required: bool,
}

/// A user's connection, from `/v1/connections`. The credential itself is
/// write-only and never comes back; `has_credential` says whether one is set.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct ConnectionInfo {
    #[ts(type = "number")]
    pub id: i64,
    pub kind: String,
    pub label: String,
    pub enabled: bool,
    pub has_credential: bool,
    /// Unix seconds.
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    /// Indexing progress, for sources Atlas indexes.
    pub sync: Option<SyncInfo>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct SyncInfo {
    /// A sync is under way.
    pub running: bool,
    /// Items in the index.
    #[ts(type = "number")]
    pub items: u64,
    /// Unix seconds of the last full sync that finished.
    #[ts(type = "number | null")]
    pub last_synced_at: Option<i64>,
    /// Why the last sync failed, if it did.
    pub error: Option<String>,
}

/// `POST /v1/connections`.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct CreateConnection {
    pub kind: String,
    /// Defaults to the kind's name.
    #[serde(default)]
    #[ts(optional)]
    pub label: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub credential: Option<String>,
}

/// `PATCH /v1/connections/{id}`. Absent fields stay as they are.
#[derive(Serialize, Deserialize, TS, Debug, Clone, Default)]
#[ts(export)]
pub struct UpdateConnection {
    #[serde(default)]
    #[ts(optional)]
    pub label: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub enabled: Option<bool>,
    /// A new credential, replacing the old one.
    #[serde(default)]
    #[ts(optional)]
    pub credential: Option<String>,
}
