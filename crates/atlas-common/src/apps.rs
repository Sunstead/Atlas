use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// An app on the launcher, from `/v1/apps`.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[ts(export)]
pub struct AppLink {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
    /// A hint for the icon: `photos`, `files`, `notes`, `git`, `monitor`,
    /// `contacts`, `calendar`, `auth`; anything else gets a generic one.
    #[serde(default)]
    pub icon: Option<String>,
}
