//! A user's connections to sources (their Immich, their OpenCloud), with
//! credentials sealed by the master key. At most one connection per kind per
//! user. Credentials are write-only through the API: nothing here returns
//! one in the clear except [`Db::open_credential`], for the adapters.

use crate::crypto::{credential_aad, MasterKey, Sealed};
use crate::{now, Db, Result, StateError, UserId};
use rusqlite::{params, OptionalExtension, Transaction};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ConnectionRow {
    pub id: i64,
    pub user: UserId,
    pub kind: String,
    pub label: String,
    /// Kind-specific settings, e.g. a pinned root path.
    pub config: serde_json::Value,
    pub credential: Option<Sealed>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewConnection {
    pub kind: String,
    pub label: String,
    pub config: serde_json::Value,
    pub credential: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionPatch {
    pub label: Option<String>,
    pub enabled: Option<bool>,
    pub config: Option<serde_json::Value>,
    /// `Some(None)` removes the credential, `Some(Some(_))` replaces it.
    pub credential: Option<Option<Vec<u8>>>,
}

const COLUMNS: &str =
    "id, user_id, kind, label, config_json, cred_nonce, cred_cipher, key_version, enabled, created_at, updated_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<ConnectionRow> {
    let config: String = r.get(4)?;
    let nonce: Option<Vec<u8>> = r.get(5)?;
    let cipher: Option<Vec<u8>> = r.get(6)?;
    let key_version: Option<i64> = r.get(7)?;
    let credential = match (nonce, cipher, key_version) {
        (Some(nonce), Some(cipher), Some(key_version)) => Some(Sealed { nonce, cipher, key_version }),
        _ => None,
    };
    Ok(ConnectionRow {
        id: r.get(0)?,
        user: UserId(r.get(1)?),
        kind: r.get(2)?,
        label: r.get(3)?,
        config: serde_json::from_str(&config).unwrap_or(serde_json::Value::Null),
        credential,
        enabled: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

fn get(tx: &rusqlite::Connection, user: UserId, id: i64) -> Result<ConnectionRow> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM source_connections WHERE id = ?1 AND user_id = ?2"),
        params![id, user.0],
        row,
    )
    .optional()?
    .ok_or(StateError::NotFound)
}

fn set_credential(
    tx: &Transaction,
    key: Option<&MasterKey>,
    user: UserId,
    id: i64,
    kind: &str,
    secret: Option<&[u8]>,
) -> Result<()> {
    let sealed = match secret {
        None => None,
        Some(secret) => {
            let key = key.ok_or_else(|| StateError::Crypto("no master key is configured".into()))?;
            Some(key.seal(secret, &credential_aad(user, id, kind))?)
        }
    };
    tx.execute(
        "UPDATE source_connections SET cred_nonce = ?2, cred_cipher = ?3, key_version = ?4 WHERE id = ?1",
        params![
            id,
            sealed.as_ref().map(|s| &s.nonce),
            sealed.as_ref().map(|s| &s.cipher),
            sealed.as_ref().map(|s| s.key_version)
        ],
    )?;
    Ok(())
}

impl Db {
    pub async fn connections(&self, user: UserId) -> Result<Vec<ConnectionRow>> {
        self.call(move |c| {
            let mut stmt =
                c.prepare(&format!("SELECT {COLUMNS} FROM source_connections WHERE user_id = ?1 ORDER BY id"))?;
            let rows = stmt.query_map([user.0], row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn connection(&self, user: UserId, id: i64) -> Result<ConnectionRow> {
        self.call(move |c| get(c, user, id)).await
    }

    /// Every user's connections. For the indexer, which works for all users
    /// in the background; request handlers use [`Db::connections`].
    pub async fn all_connections(&self) -> Result<Vec<ConnectionRow>> {
        self.call(|c| {
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM source_connections ORDER BY id"))?;
            let rows = stmt.query_map([], row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// One connection by id, whoever owns it. For the indexer only.
    pub async fn connection_by_id(&self, id: i64) -> Result<Option<ConnectionRow>> {
        self.call(move |c| {
            Ok(c.query_row(&format!("SELECT {COLUMNS} FROM source_connections WHERE id = ?1"), [id], row).optional()?)
        })
        .await
    }

    /// `key` is needed only when there's a credential to seal.
    pub async fn create_connection(
        &self,
        user: UserId,
        new: NewConnection,
        key: Option<Arc<MasterKey>>,
    ) -> Result<ConnectionRow> {
        self.call(move |c| {
            let tx = c.transaction()?;
            let t = now();
            let inserted = tx.execute(
                "INSERT INTO source_connections (user_id, kind, label, config_json, enabled, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)
                 ON CONFLICT (user_id, kind) DO NOTHING",
                params![user.0, new.kind, new.label, new.config.to_string(), t],
            )?;
            if inserted == 0 {
                return Err(StateError::Conflict(format!("a {} connection already exists", new.kind)));
            }
            let id = tx.last_insert_rowid();
            // Sealed after the insert: the row id is part of what the
            // ciphertext is bound to.
            set_credential(&tx, key.as_deref(), user, id, &new.kind, new.credential.as_deref())?;
            let created = get(&tx, user, id)?;
            tx.commit()?;
            Ok(created)
        })
        .await
    }

    pub async fn update_connection(
        &self,
        user: UserId,
        id: i64,
        patch: ConnectionPatch,
        key: Option<Arc<MasterKey>>,
    ) -> Result<ConnectionRow> {
        self.call(move |c| {
            let tx = c.transaction()?;
            let current = get(&tx, user, id)?;
            tx.execute(
                "UPDATE source_connections SET label = ?2, enabled = ?3, config_json = ?4, updated_at = ?5 WHERE id = ?1",
                params![
                    id,
                    patch.label.unwrap_or(current.label),
                    patch.enabled.unwrap_or(current.enabled),
                    patch.config.unwrap_or(current.config).to_string(),
                    now()
                ],
            )?;
            if let Some(credential) = patch.credential {
                set_credential(&tx, key.as_deref(), user, id, &current.kind, credential.as_deref())?;
            }
            let updated = get(&tx, user, id)?;
            tx.commit()?;
            Ok(updated)
        })
        .await
    }

    pub async fn delete_connection(&self, user: UserId, id: i64) -> Result<()> {
        self.call(move |c| {
            let n = c.execute("DELETE FROM source_connections WHERE id = ?1 AND user_id = ?2", params![id, user.0])?;
            if n == 0 {
                return Err(StateError::NotFound);
            }
            Ok(())
        })
        .await
    }

    /// The connection's credential in the clear, for an adapter to use.
    pub fn open_credential(&self, conn: &ConnectionRow, key: &MasterKey) -> Result<Option<Vec<u8>>> {
        conn.credential
            .as_ref()
            .map(|s| key.open(s, &credential_aad(conn.user, conn.id, &conn.kind)))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::sample;

    fn key() -> Arc<MasterKey> {
        Arc::new(MasterKey::from_bytes(&[9; 32]).unwrap())
    }

    fn immich(credential: Option<&str>) -> NewConnection {
        NewConnection {
            kind: "immich".into(),
            label: "Photos".into(),
            config: serde_json::json!({}),
            credential: credential.map(|s| s.as_bytes().to_vec()),
        }
    }

    async fn setup() -> (Db, UserId, UserId) {
        let db = Db::open_in_memory().unwrap();
        let a = db.upsert_user(sample("a", "alice")).await.unwrap().id;
        let b = db.upsert_user(sample("b", "bob")).await.unwrap().id;
        (db, a, b)
    }

    #[tokio::test]
    async fn creates_seals_and_opens() {
        let (db, a, _) = setup().await;
        let k = key();
        let c = db.create_connection(a, immich(Some("secret-key")), Some(k.clone())).await.unwrap();
        assert_eq!(c.kind, "immich");
        let sealed = c.credential.as_ref().unwrap();
        assert_ne!(sealed.cipher, b"secret-key");
        assert_eq!(db.open_credential(&c, &k).unwrap().unwrap(), b"secret-key");
    }

    #[tokio::test]
    async fn one_per_kind_per_user() {
        let (db, a, b) = setup().await;
        db.create_connection(a, immich(None), None).await.unwrap();
        assert!(matches!(db.create_connection(a, immich(None), None).await, Err(StateError::Conflict(_))));
        db.create_connection(b, immich(None), None).await.unwrap();
    }

    #[tokio::test]
    async fn users_only_see_their_own() {
        let (db, a, b) = setup().await;
        let c = db.create_connection(a, immich(None), None).await.unwrap();
        assert_eq!(db.connections(a).await.unwrap().len(), 1);
        assert!(db.connections(b).await.unwrap().is_empty());
        assert!(matches!(db.connection(b, c.id).await, Err(StateError::NotFound)));
        assert!(matches!(
            db.update_connection(b, c.id, ConnectionPatch::default(), None).await,
            Err(StateError::NotFound)
        ));
        assert!(matches!(db.delete_connection(b, c.id).await, Err(StateError::NotFound)));
        assert_eq!(db.connections(a).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_credential_moved_to_another_row_wont_open() {
        let (db, a, b) = setup().await;
        let k = key();
        let mine = db.create_connection(a, immich(Some("alice-key")), Some(k.clone())).await.unwrap();
        let theirs = db.create_connection(b, immich(None), None).await.unwrap();
        let (nonce, cipher) = {
            let s = mine.credential.clone().unwrap();
            (s.nonce, s.cipher)
        };
        let id = theirs.id;
        db.call(move |c| {
            Ok(c.execute(
                "UPDATE source_connections SET cred_nonce = ?2, cred_cipher = ?3, key_version = 1 WHERE id = ?1",
                params![id, nonce, cipher],
            )?)
        })
        .await
        .unwrap();
        let stolen = db.connection(b, theirs.id).await.unwrap();
        assert!(db.open_credential(&stolen, &k).is_err());
    }

    #[tokio::test]
    async fn patches_replace_and_remove_credentials() {
        let (db, a, _) = setup().await;
        let k = key();
        let c = db.create_connection(a, immich(Some("one")), Some(k.clone())).await.unwrap();
        let patch = ConnectionPatch {
            label: Some("Family photos".into()),
            enabled: Some(false),
            credential: Some(Some(b"two".to_vec())),
            ..Default::default()
        };
        let c = db.update_connection(a, c.id, patch, Some(k.clone())).await.unwrap();
        assert_eq!(c.label, "Family photos");
        assert!(!c.enabled);
        assert_eq!(db.open_credential(&c, &k).unwrap().unwrap(), b"two");

        let patch = ConnectionPatch { credential: Some(None), ..Default::default() };
        let c = db.update_connection(a, c.id, patch, None).await.unwrap();
        assert!(c.credential.is_none());
        assert_eq!(c.label, "Family photos", "untouched fields stay");
    }

    #[tokio::test]
    async fn sealing_without_a_key_fails_and_writes_nothing() {
        let (db, a, _) = setup().await;
        assert!(matches!(db.create_connection(a, immich(Some("x")), None).await, Err(StateError::Crypto(_))));
        assert!(db.connections(a).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_a_user_deletes_their_connections() {
        let (db, a, _) = setup().await;
        db.create_connection(a, immich(None), None).await.unwrap();
        let id = a.get();
        db.call(move |c| Ok(c.execute("DELETE FROM users WHERE id = ?1", [id])?)).await.unwrap();
        assert!(db.connections(a).await.unwrap().is_empty());
    }
}
