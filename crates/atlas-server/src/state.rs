//! What every handler can reach.

use crate::auth::Auth;
use crate::config::SourcesConfig;
use crate::indexer::Indexer;
use atlas_state::{Db, MasterKey};
use std::sync::Arc;
use url::Url;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub auth: Arc<Auth>,
    /// `None` when `ATLAS_MASTER_KEY` isn't set: connections that need a
    /// credential can't be saved.
    pub master_key: Option<Arc<MasterKey>>,
    pub sources: Arc<SourcesConfig>,
    pub public_url: Url,
    pub indexer: Arc<Indexer>,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::AuthMode;

    /// In-memory state with dev sign-in as `pwb` and a test master key.
    pub fn dev_state() -> AppState {
        state(AuthMode::Dev { username: "pwb".into() }, SourcesConfig::default())
    }

    pub fn state(mode: AuthMode, sources: SourcesConfig) -> AppState {
        let public_url = Url::parse("http://localhost:1420").unwrap();
        let db = Db::open_in_memory().unwrap();
        let sources = Arc::new(sources);
        // A fresh index per test, under the OS temp dir.
        let dir = std::env::temp_dir().join(format!("atlas-test-index-{}", atlas_state::random_token(9)));
        let index = Arc::new(atlas_index::Index::open(&dir).unwrap());
        let master_key = Some(Arc::new(MasterKey::from_bytes(&[7; 32]).unwrap()));
        AppState {
            indexer: Indexer::new(index, db.clone(), sources.clone(), master_key.clone()),
            db,
            auth: Arc::new(Auth::new(mode, &public_url)),
            master_key,
            sources,
            public_url,
        }
    }
}
