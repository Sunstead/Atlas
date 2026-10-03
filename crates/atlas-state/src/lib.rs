//! Atlas's state database: users, sessions, sign-in flows and source
//! connections. Small, written rarely, and backed up (it lives in the
//! `atlas-state` volume). The search index is elsewhere (`atlas-index`):
//! derived, rebuildable, never backed up.
//!
//! One SQLite connection behind a mutex, used from `spawn_blocking`, is
//! plenty at this size; it's the pattern Cosmos uses for its settings store.
//!
//! Every per-user query takes a [`UserId`], which only comes from a session
//! lookup or a sign-in. Never build one from request input.

pub mod connections;
pub mod crypto;
pub mod flows;
pub mod sessions;
pub mod users;

use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub use connections::{ConnectionPatch, ConnectionRow, NewConnection};
pub use crypto::{MasterKey, Sealed};
pub use flows::OidcFlow;
pub use sessions::{Session, SESSION_TTL_SECS};
pub use users::{NewUser, User};

/// A user's database id. Constructed only by this crate, from a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserId(i64);

impl UserId {
    pub fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("not found")]
    NotFound,
    /// A uniqueness rule, e.g. one connection per kind per user.
    #[error("{0}")]
    Conflict(String),
    #[error("credentials: {0}")]
    Crypto(String),
    #[error("database task failed: {0}")]
    Task(String),
}

pub type Result<T> = std::result::Result<T, StateError>;

/// Each entry runs once, in order, tracked by `PRAGMA user_version`. Append
/// only: never edit a migration that has shipped.
const MIGRATIONS: &[&str] = &[
    // 0.1: users, sessions, sign-in flows, per-user settings and connections.
    "CREATE TABLE users (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        oidc_issuer   TEXT    NOT NULL,
        oidc_sub      TEXT    NOT NULL,
        username      TEXT    NOT NULL,
        display_name  TEXT,
        email         TEXT,
        created_at    INTEGER NOT NULL,
        last_login_at INTEGER NOT NULL,
        UNIQUE (oidc_issuer, oidc_sub)
    );
    CREATE TABLE sessions (
        token_hash   BLOB    PRIMARY KEY,
        user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
        created_at   INTEGER NOT NULL,
        expires_at   INTEGER NOT NULL,
        last_seen_at INTEGER NOT NULL
    ) WITHOUT ROWID;
    CREATE INDEX sessions_user ON sessions (user_id);
    CREATE TABLE oidc_flows (
        state         TEXT    PRIMARY KEY,
        pkce_verifier TEXT    NOT NULL,
        nonce         TEXT    NOT NULL,
        return_to     TEXT    NOT NULL,
        created_at    INTEGER NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE user_settings (
        user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
        key        TEXT    NOT NULL,
        value_json TEXT    NOT NULL,
        PRIMARY KEY (user_id, key)
    ) WITHOUT ROWID;
    CREATE TABLE source_connections (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
        kind        TEXT    NOT NULL,
        label       TEXT    NOT NULL,
        config_json TEXT    NOT NULL,
        cred_nonce  BLOB,
        cred_cipher BLOB,
        key_version INTEGER,
        enabled     INTEGER NOT NULL,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        UNIQUE (user_id, kind)
    );",
];

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Opens (creating if needed) `dir/atlas.db` and brings it up to date.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| StateError::Task(format!("{}: {e}", dir.display())))?;
        let conn = Connection::open(dir.join("atlas.db"))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
        migrate(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Runs `f` on the connection, off the async runtime.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut guard)
        })
        .await
        .map_err(|e| StateError::Task(e.to_string()))?
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: usize = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
    if current > MIGRATIONS.len() {
        return Err(StateError::Task(format!(
            "database is at version {current}, newer than this build ({}); refusing to run",
            MIGRATIONS.len()
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
        tracing::info!(version = i + 1, "state database migrated");
    }
    Ok(())
}

pub(crate) fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `n` random bytes from the OS.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("the OS random number generator failed");
    buf
}

/// A random URL-safe token with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        drop(Db::open(dir.path()).unwrap());
        let db = Db::open(dir.path()).unwrap();
        let v: i64 = db.conn.lock().unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
    }

    #[test]
    fn refuses_a_newer_database() {
        let dir = tempfile::tempdir().unwrap();
        drop(Db::open(dir.path()).unwrap());
        let conn = Connection::open(dir.path().join("atlas.db")).unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
        drop(conn);
        assert!(Db::open(dir.path()).is_err());
    }

    #[test]
    fn tokens_are_random_and_url_safe() {
        let a = random_token(32);
        assert_ne!(a, random_token(32));
        assert_eq!(a.len(), 43);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }
}
