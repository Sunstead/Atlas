//! Sign-in sessions. The cookie holds a random token; the database holds
//! only its SHA-256, so a copy of the database can't be used to sign in.

use crate::{now, random_token, Db, Result, UserId};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

/// Thirty days, sliding: each use within the window extends it.
pub const SESSION_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// How stale `last_seen_at` may get before a request rewrites it, so a busy
/// session doesn't write on every request.
const TOUCH_AFTER_SECS: i64 = 60 * 60;

#[derive(Debug, Clone)]
pub struct Session {
    pub user: UserId,
    pub expires_at: i64,
}

fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

impl Db {
    /// Starts a session. Returns the token for the cookie.
    pub async fn create_session(&self, user: UserId) -> Result<String> {
        let token = random_token(32);
        let h = hash(&token);
        self.call(move |c| {
            let t = now();
            c.execute(
                "INSERT INTO sessions (token_hash, user_id, created_at, expires_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?3)",
                params![h, user.get(), t, t + SESSION_TTL_SECS],
            )?;
            Ok(())
        })
        .await?;
        Ok(token)
    }

    /// The session for a cookie token, if it exists and hasn't expired.
    pub async fn session(&self, token: &str) -> Result<Option<Session>> {
        let h = hash(token);
        self.call(move |c| {
            let t = now();
            let found = c
                .query_row(
                    "SELECT user_id, expires_at, last_seen_at FROM sessions WHERE token_hash = ?1",
                    [&h],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
                )
                .optional()?;
            let Some((user, expires_at, last_seen)) = found else { return Ok(None) };
            if expires_at <= t {
                c.execute("DELETE FROM sessions WHERE token_hash = ?1", [&h])?;
                return Ok(None);
            }
            let mut expires_at = expires_at;
            if t - last_seen >= TOUCH_AFTER_SECS {
                expires_at = t + SESSION_TTL_SECS;
                c.execute(
                    "UPDATE sessions SET last_seen_at = ?2, expires_at = ?3 WHERE token_hash = ?1",
                    params![h, t, expires_at],
                )?;
            }
            Ok(Some(Session { user: UserId(user), expires_at }))
        })
        .await
    }

    pub async fn delete_session(&self, token: &str) -> Result<()> {
        let h = hash(token);
        self.call(move |c| {
            c.execute("DELETE FROM sessions WHERE token_hash = ?1", [h])?;
            Ok(())
        })
        .await
    }

    /// Drops expired sessions and abandoned sign-in flows. Run periodically.
    pub async fn purge_expired(&self) -> Result<usize> {
        self.call(|c| {
            let t = now();
            let s = c.execute("DELETE FROM sessions WHERE expires_at <= ?1", [t])?;
            let f = c.execute("DELETE FROM oidc_flows WHERE created_at <= ?1", [t - crate::flows::FLOW_TTL_SECS])?;
            Ok(s + f)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::sample;

    #[tokio::test]
    async fn a_session_finds_its_user_until_deleted() {
        let db = Db::open_in_memory().unwrap();
        let user = db.upsert_user(sample("s", "pwb")).await.unwrap();
        let token = db.create_session(user.id).await.unwrap();
        assert_eq!(db.session(&token).await.unwrap().unwrap().user, user.id);
        assert!(db.session("not-a-token").await.unwrap().is_none());
        db.delete_session(&token).await.unwrap();
        assert!(db.session(&token).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn only_the_hash_is_stored() {
        let db = Db::open_in_memory().unwrap();
        let user = db.upsert_user(sample("s", "pwb")).await.unwrap();
        let token = db.create_session(user.id).await.unwrap();
        let stored: Vec<u8> =
            db.call(|c| Ok(c.query_row("SELECT token_hash FROM sessions", [], |r| r.get(0))?)).await.unwrap();
        assert_ne!(stored, token.as_bytes());
        assert_eq!(stored, hash(&token));
    }

    #[tokio::test]
    async fn expired_sessions_are_gone() {
        let db = Db::open_in_memory().unwrap();
        let user = db.upsert_user(sample("s", "pwb")).await.unwrap();
        let token = db.create_session(user.id).await.unwrap();
        db.call(|c| Ok(c.execute("UPDATE sessions SET expires_at = 0", [])?)).await.unwrap();
        assert!(db.session(&token).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn stale_sessions_slide_forward() {
        let db = Db::open_in_memory().unwrap();
        let user = db.upsert_user(sample("s", "pwb")).await.unwrap();
        let token = db.create_session(user.id).await.unwrap();
        let t = now();
        db.call(move |c| {
            Ok(c.execute(
                "UPDATE sessions SET last_seen_at = ?1, expires_at = ?2",
                params![t - TOUCH_AFTER_SECS - 1, t + 100],
            )?)
        })
        .await
        .unwrap();
        let s = db.session(&token).await.unwrap().unwrap();
        assert!(s.expires_at >= t + SESSION_TTL_SECS);
    }
}
