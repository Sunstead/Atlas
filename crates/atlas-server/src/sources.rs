//! The kinds of source a user can connect, whether this server is set up for
//! each, and building a connection's adapter.

use crate::config::SourcesConfig;
use atlas_common::{CredentialInfo, SourceKindInfo};
use atlas_core::{Source, SourceError};
use atlas_source_immich::ImmichSource;
use atlas_source_opencloud::OpenCloudSource;
use atlas_state::{ConnectionRow, Db, MasterKey};
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

/// The adapter for a connection. Credentials are opened here, only for as
/// long as it takes to hand them to the adapter.
pub fn build(cfg: &SourcesConfig, db: &Db, key: Option<&MasterKey>, row: &ConnectionRow) -> Result<Arc<dyn Source>, SourceError> {
    match row.kind.as_str() {
        OPENCLOUD => {
            let oc = cfg.opencloud.as_ref().ok_or_else(|| SourceError::Config("OpenCloud isn't configured on this server".into()))?;
            let root = row
                .config
                .get("root")
                .and_then(|r| r.as_str())
                .map(PathBuf::from)
                .ok_or_else(|| SourceError::Config("This connection has no folder; disconnect and connect again".into()))?;
            Ok(Arc::new(OpenCloudSource::new(&oc.users_dir, &root, oc.urls.public.as_str(), oc.storage_id.as_deref())?))
        }
        IMMICH => {
            let immich = cfg.immich.as_ref().ok_or_else(|| SourceError::Config("Immich isn't configured on this server".into()))?;
            let key = key.ok_or_else(|| SourceError::Config("This server can't open saved keys: ATLAS_MASTER_KEY isn't set".into()))?;
            let secret = db
                .open_credential(row, key)
                .map_err(|_| SourceError::Config("The saved API key can't be read. Save it again.".into()))?
                .ok_or_else(|| SourceError::Config("Add an Immich API key to search your photos".into()))?;
            let secret = String::from_utf8(secret).map_err(|_| SourceError::Config("The saved API key is damaged. Save it again.".into()))?;
            Ok(Arc::new(ImmichSource::new(immich.api.as_str(), immich.public.as_str(), &secret)?))
        }
        other => Err(SourceError::Config(format!("Unknown source kind {other:?}"))),
    }
}

/// Tries a credential before it's saved, so a wrong key is caught on the
/// settings page rather than at the next search. Only a refusal counts: if
/// the app is down right now, the key is saved anyway (and the next search
/// says the source is unavailable).
pub async fn check_credential(cfg: &SourcesConfig, kind: &str, credential: &str) -> Result<(), SourceError> {
    let result = match (kind, &cfg.immich) {
        (IMMICH, Some(immich)) => ImmichSource::new(immich.api.as_str(), immich.public.as_str(), credential)?.check().await,
        // OpenCloud's token isn't used until the API layer.
        _ => Ok(()),
    };
    match result {
        Err(SourceError::Config(m)) => Err(SourceError::Config(m)),
        Err(e) => {
            tracing::warn!(%kind, error = %e, "can't check the credential now; saving it anyway");
            Ok(())
        }
        Ok(()) => Ok(()),
    }
}
