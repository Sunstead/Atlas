//! Sign-in flows in progress: what `/auth/callback` needs to finish what
//! `/auth/login` started. Kept server-side, so `state` is just a random key
//! and the return path never round-trips through the provider.

use crate::{now, Db, Result};
use rusqlite::{params, OptionalExtension};

/// A flow not finished within ten minutes is abandoned.
pub const FLOW_TTL_SECS: i64 = 10 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcFlow {
    pub state: String,
    pub pkce_verifier: String,
    pub nonce: String,
    /// Already checked to be a same-origin path.
    pub return_to: String,
}

impl Db {
    pub async fn put_flow(&self, f: OidcFlow) -> Result<()> {
        self.call(move |c| {
            c.execute(
                "INSERT INTO oidc_flows (state, pkce_verifier, nonce, return_to, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![f.state, f.pkce_verifier, f.nonce, f.return_to, now()],
            )?;
            Ok(())
        })
        .await
    }

    /// Takes a flow out: each `state` works once, and only while fresh.
    pub async fn take_flow(&self, state: &str) -> Result<Option<OidcFlow>> {
        let state = state.to_owned();
        self.call(move |c| {
            let found = c
                .query_row(
                    "DELETE FROM oidc_flows WHERE state = ?1 RETURNING pkce_verifier, nonce, return_to, created_at",
                    [&state],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)),
                )
                .optional()?;
            Ok(found.and_then(|(pkce_verifier, nonce, return_to, created_at)| {
                (now() - created_at < FLOW_TTL_SECS).then_some(OidcFlow { state, pkce_verifier, nonce, return_to })
            }))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(state: &str) -> OidcFlow {
        OidcFlow { state: state.into(), pkce_verifier: "v".into(), nonce: "n".into(), return_to: "/search?q=x".into() }
    }

    #[tokio::test]
    async fn a_flow_can_be_taken_once() {
        let db = Db::open_in_memory().unwrap();
        db.put_flow(flow("abc")).await.unwrap();
        assert_eq!(db.take_flow("abc").await.unwrap(), Some(flow("abc")));
        assert_eq!(db.take_flow("abc").await.unwrap(), None);
        assert_eq!(db.take_flow("other").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_stale_flow_is_refused() {
        let db = Db::open_in_memory().unwrap();
        db.put_flow(flow("old")).await.unwrap();
        db.call(|c| Ok(c.execute("UPDATE oidc_flows SET created_at = created_at - ?1", [FLOW_TTL_SECS])?))
            .await
            .unwrap();
        assert_eq!(db.take_flow("old").await.unwrap(), None);
    }
}
