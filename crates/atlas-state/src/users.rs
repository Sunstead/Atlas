//! Users, created on first sign-in. Identity is the provider's
//! `(issuer, sub)`; the username comes from `preferred_username` and is what
//! data paths are keyed by (`data/files/users/<username>`).

use crate::{now, Db, Result, StateError, UserId};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
}

/// Claims from a verified ID token.
#[derive(Debug, Clone)]
pub struct NewUser {
    pub issuer: String,
    pub subject: String,
    pub username: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User { id: UserId(r.get(0)?), username: r.get(1)?, display_name: r.get(2)?, email: r.get(3)? })
}

impl Db {
    /// Creates the user on first sign-in, refreshes their profile after.
    pub async fn upsert_user(&self, u: NewUser) -> Result<User> {
        self.call(move |c| {
            let t = now();
            c.query_row(
                "INSERT INTO users (oidc_issuer, oidc_sub, username, display_name, email, created_at, last_login_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT (oidc_issuer, oidc_sub) DO UPDATE SET
                    username = excluded.username,
                    display_name = excluded.display_name,
                    email = excluded.email,
                    last_login_at = excluded.last_login_at
                 RETURNING id, username, display_name, email",
                params![u.issuer, u.subject, u.username, u.display_name, u.email, t],
                row,
            )
            .map_err(Into::into)
        })
        .await
    }

    pub async fn user(&self, id: UserId) -> Result<User> {
        self.call(move |c| {
            c.query_row("SELECT id, username, display_name, email FROM users WHERE id = ?1", [id.0], row)
                .optional()?
                .ok_or(StateError::NotFound)
        })
        .await
    }
}

#[cfg(test)]
pub(crate) fn sample(subject: &str, username: &str) -> NewUser {
    NewUser {
        issuer: "https://auth.example/application/o/atlas/".into(),
        subject: subject.into(),
        username: username.into(),
        display_name: Some(format!("{username} name")),
        email: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_sign_in_creates_later_ones_update() {
        let db = Db::open_in_memory().unwrap();
        let a = db.upsert_user(sample("sub-1", "pwb")).await.unwrap();
        let mut again = sample("sub-1", "preston");
        again.email = Some("p@example.com".into());
        let b = db.upsert_user(again).await.unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(b.username, "preston");
        assert_eq!(db.user(a.id).await.unwrap().email.as_deref(), Some("p@example.com"));

        let other = db.upsert_user(sample("sub-2", "kim")).await.unwrap();
        assert_ne!(other.id, a.id);
    }
}
