//! The kinds of source a user can connect, whether this server is set up for
//! each, and building a connection's adapter. Immich's adapter arrives with
//! M3; until then its connections can be saved but aren't searched.

use crate::config::SourcesConfig;
use atlas_common::{CredentialInfo, SourceKindInfo};
use atlas_core::{Source, SourceError};
use atlas_source_opencloud::OpenCloudSource;
use atlas_state::ConnectionRow;
use std::path::PathBuf;
use std::sync::Arc;

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

/// Whether Atlas keeps this kind in its own index (and so syncs it).
pub fn is_indexed(kind: &str) -> bool {
    kind == OPENCLOUD
}

/// The adapter for a connection.
pub fn build(cfg: &SourcesConfig, row: &ConnectionRow) -> Result<Arc<dyn Source>, SourceError> {
    match row.kind.as_str() {
        OPENCLOUD => {
            let oc = cfg.opencloud.as_ref().ok_or_else(|| SourceError::Config("OpenCloud isn't configured on this server".into()))?;
            let root = row
                .config
                .get("root")
                .and_then(|r| r.as_str())
                .map(PathBuf::from)
                .ok_or_else(|| SourceError::Config("This connection has no folder; disconnect and connect again".into()))?;
            Ok(Arc::new(OpenCloudSource::new(&oc.users_dir, &root, oc.urls.public.as_str())?))
        }
        IMMICH => Err(SourceError::Config("Immich search isn't available yet".into())),
        other => Err(SourceError::Config(format!("Unknown source kind {other:?}"))),
    }
}
