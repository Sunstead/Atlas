//! The kinds of source a user can connect, and whether this server is set up
//! for each. The adapters themselves arrive with their milestones (M2
//! OpenCloud, M3 Immich); this is what the settings page offers meanwhile.

use crate::config::SourcesConfig;
use atlas_common::{CredentialInfo, SourceKindInfo};

pub const OPENCLOUD: &str = "opencloud";
pub const IMMICH: &str = "immich";

pub fn kinds(cfg: &SourcesConfig) -> Vec<SourceKindInfo> {
    vec![
        SourceKindInfo {
            kind: OPENCLOUD.into(),
            name: "OpenCloud".into(),
            description: "Your files. Indexed from the server's disk, so search works without a token.".into(),
            credential: Some(CredentialInfo {
                label: "App token".into(),
                help: "Optional for now. Used later for links into OpenCloud, thumbnails and shared spaces.".into(),
                url: None,
                required: false,
            }),
            enabled: cfg.opencloud.is_some(),
            disabled_reason: cfg
                .opencloud
                .is_none()
                .then(|| "Not configured on this server (ATLAS_OPENCLOUD_URL and ATLAS_OPENCLOUD_USERS_DIR).".into()),
        },
        SourceKindInfo {
            kind: IMMICH.into(),
            name: "Immich".into(),
            description: "Your photos and videos, searched with Immich's own smart search.".into(),
            credential: Some(CredentialInfo {
                label: "API key".into(),
                help: "In Immich: Account settings, API keys, New API key. Give it read access to assets, albums and search."
                    .into(),
                url: cfg.immich.as_ref().map(|i| format!("{}user-settings?isOpen=api-keys", i.public)),
                required: true,
            }),
            enabled: cfg.immich.is_some(),
            disabled_reason: cfg.immich.is_none().then(|| "Not configured on this server (ATLAS_IMMICH_URL).".into()),
        },
    ]
}

pub fn kind(cfg: &SourcesConfig, kind: &str) -> Option<SourceKindInfo> {
    kinds(cfg).into_iter().find(|k| k.kind == kind)
}
