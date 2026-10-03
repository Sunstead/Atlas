//! Sign-in with an OpenID Connect provider (Authentik): the authorization
//! code flow with PKCE, as a confidential client.
//!
//! The ID token comes straight from the token endpoint over TLS, and is
//! still checked like any JWT, with the rules Cosmos uses for its access
//! tokens (`cosmos-agent/src/auth/oidc.rs`): asymmetric algorithms only,
//! issuer, audience, expiry with a minute's leeway, plus the nonce.
//!
//! Discovery and keys are fetched on first use and every six hours. A token
//! signed with an unknown `kid` (the provider rotated keys) triggers one
//! refetch, at most once a minute. An unreachable provider is `Unavailable`
//! (503, retry), never `Invalid` (sign in again).

use crate::config::OidcConfig;
use base64::Engine;
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use url::Url;

const REFETCH_AFTER: Duration = Duration::from_secs(60);
const REFRESH_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const LEEWAY_SECS: u64 = 60;

/// Asymmetric only. HS256 would mean trusting a shared secret as a
/// signature, which must never pass.
const ALLOWED: &[Algorithm] = &[
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

#[derive(Debug)]
pub enum OidcError {
    /// Bad code, bad token, refused sign-in. Start again.
    Invalid(String),
    /// The provider can't be reached or is failing. Retry.
    Unavailable(String),
}

impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Unavailable(m) => f.write_str(m),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    end_session_endpoint: Option<String>,
}

struct Provider {
    discovery: Discovery,
    by_kid: HashMap<String, DecodingKey>,
    /// Keys published without a `kid`, tried when a token has none either.
    anonymous: Vec<DecodingKey>,
    fetched_at: Instant,
}

/// The ID token claims Atlas uses.
#[derive(Deserialize, Debug, Clone)]
pub struct IdClaims {
    pub sub: String,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    nonce: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

pub struct OidcClient {
    cfg: OidcConfig,
    discovery_url: String,
    http: reqwest::Client,
    provider: RwLock<Option<Arc<Provider>>>,
    last_attempt: RwLock<Option<Instant>>,
    /// Single-flight: concurrent loads share one fetch.
    fetching: tokio::sync::Mutex<()>,
}

fn same_issuer(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The PKCE S256 challenge for a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

impl OidcClient {
    pub fn new(cfg: OidcConfig) -> Self {
        let discovery_url = format!("{}/.well-known/openid-configuration", cfg.issuer.trim_end_matches('/'));
        Self {
            cfg,
            discovery_url,
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            provider: RwLock::new(None),
            last_attempt: RwLock::new(None),
            fetching: tokio::sync::Mutex::new(()),
        }
    }

    pub fn config(&self) -> &OidcConfig {
        &self.cfg
    }

    /// Loads discovery and keys in the background at startup, so the first
    /// sign-in doesn't wait, and logs if the provider can't be reached.
    pub fn spawn_warmup(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            match this.provider().await {
                Ok(_) => tracing::info!(issuer = %this.cfg.issuer, "identity provider loaded"),
                Err(e) => tracing::warn!(url = %this.discovery_url, error = %e, "can't load the identity provider yet"),
            }
        });
    }

    fn current(&self) -> Option<Arc<Provider>> {
        self.provider.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    async fn provider(&self) -> Result<Arc<Provider>, OidcError> {
        if let Some(p) = self.current().filter(|p| p.fetched_at.elapsed() < REFRESH_EVERY) {
            return Ok(p);
        }
        self.reload(false).await
    }

    /// Fetches discovery and keys. `only_if_due`: skip if a fetch was tried
    /// within the last minute (for unknown `kid`s, so junk tokens can't make
    /// us hammer the provider). Callers queued behind a fetch reuse it.
    async fn reload(&self, only_if_due: bool) -> Result<Arc<Provider>, OidcError> {
        let started = Instant::now();
        let _guard = self.fetching.lock().await;
        let last = *self.last_attempt.read().unwrap_or_else(|p| p.into_inner());
        if let (Some(last), Some(current)) = (last, self.current()) {
            // Someone else fetched while we waited, or it's too soon to try.
            if last >= started || (only_if_due && last.elapsed() < REFETCH_AFTER) {
                return Ok(current);
            }
        }
        *self.last_attempt.write().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());

        let result = self.fetch().await;
        match result {
            Ok(p) => {
                let p = Arc::new(p);
                *self.provider.write().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
                Ok(p)
            }
            // A failed refresh keeps the provider we have, if any.
            Err(e) => match self.current() {
                Some(p) => {
                    tracing::warn!(error = %e, "identity provider refresh failed; keeping what we have");
                    Ok(p)
                }
                None => Err(OidcError::Unavailable(e)),
            },
        }
    }

    async fn fetch(&self) -> Result<Provider, String> {
        let discovery: Discovery = self.get_json(&self.discovery_url).await?;
        if !same_issuer(&discovery.issuer, &self.cfg.issuer) {
            return Err(format!(
                "discovery says the issuer is {}, but Atlas is configured for {}",
                discovery.issuer, self.cfg.issuer
            ));
        }
        let set: JwkSet = self.get_json(&discovery.jwks_uri).await?;
        let mut by_kid = HashMap::new();
        let mut anonymous = Vec::new();
        for jwk in &set.keys {
            let Ok(key) = DecodingKey::from_jwk(jwk) else { continue };
            match &jwk.common.key_id {
                Some(kid) => {
                    by_kid.insert(kid.clone(), key);
                }
                None => anonymous.push(key),
            }
        }
        if by_kid.is_empty() && anonymous.is_empty() {
            return Err("the provider published no usable signing keys".into());
        }
        Ok(Provider { discovery, by_kid, anonymous, fetched_at: Instant::now() })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, String> {
        let res = self.http.get(url).send().await.map_err(|e| format!("{url}: {e}"))?;
        if !res.status().is_success() {
            return Err(format!("{url}: HTTP {}", res.status()));
        }
        res.json().await.map_err(|e| format!("{url}: {e}"))
    }

    /// Where to send the browser to sign in.
    pub async fn authorize_url(
        &self,
        redirect_uri: &str,
        state: &str,
        nonce: &str,
        pkce_verifier: &str,
    ) -> Result<String, OidcError> {
        let p = self.provider().await?;
        let mut url = Url::parse(&p.discovery.authorization_endpoint)
            .map_err(|e| OidcError::Unavailable(format!("bad authorization_endpoint: {e}")))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.cfg.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &self.cfg.scopes)
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", &pkce_challenge(pkce_verifier))
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    /// Trades the callback's code for a verified ID token.
    pub async fn exchange(
        &self,
        redirect_uri: &str,
        code: &str,
        pkce_verifier: &str,
        nonce: &str,
    ) -> Result<IdClaims, OidcError> {
        let p = self.provider().await?;
        let res = self
            .http
            .post(&p.discovery.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", &self.cfg.client_id),
                ("client_secret", &self.cfg.client_secret),
                ("code_verifier", pkce_verifier),
            ])
            .send()
            .await
            .map_err(|e| OidcError::Unavailable(format!("token endpoint: {e}")))?;
        let status = res.status();
        if status.is_server_error() {
            return Err(OidcError::Unavailable(format!("token endpoint: HTTP {status}")));
        }
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            return Err(OidcError::Invalid(format!("token endpoint refused the code: HTTP {status} {body}")));
        }
        let tokens: TokenResponse =
            res.json().await.map_err(|e| OidcError::Unavailable(format!("token endpoint: {e}")))?;
        let id_token = tokens.id_token.ok_or_else(|| OidcError::Invalid("no id_token in the response".into()))?;
        self.verify_id_token(&id_token, nonce).await
    }

    pub async fn verify_id_token(&self, token: &str, nonce: &str) -> Result<IdClaims, OidcError> {
        let header = decode_header(token).map_err(|e| OidcError::Invalid(format!("not a JWT: {e}")))?;
        if !ALLOWED.contains(&header.alg) {
            return Err(OidcError::Invalid(format!("{:?} tokens are not accepted", header.alg)));
        }
        let issuer = self.cfg.issuer.as_str();
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[issuer, issuer.trim_end_matches('/')]);
        validation.set_audience(&[self.cfg.client_id.as_str()]);
        validation.leeway = LEEWAY_SECS;
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);

        let kid = header.kid.as_deref();
        let mut keys = keys_for(&*self.provider().await?, kid);
        if keys.is_empty() {
            keys = keys_for(&*self.reload(true).await?, kid);
        }
        if keys.is_empty() {
            return Err(OidcError::Invalid("token signed with an unknown key".into()));
        }

        let mut last = None;
        for key in keys {
            match decode::<IdClaims>(token, &key, &validation) {
                Ok(data) => {
                    if data.claims.nonce.as_deref() != Some(nonce) {
                        return Err(OidcError::Invalid("the token's nonce doesn't match this sign-in".into()));
                    }
                    return Ok(data.claims);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(OidcError::Invalid(last.map(|e| e.to_string()).unwrap_or_else(|| "no key matched".into())))
    }

    /// The provider's end-session page, to sign out there too.
    pub async fn end_session_url(&self) -> Option<String> {
        self.provider().await.ok()?.discovery.end_session_endpoint.clone()
    }
}

fn keys_for(p: &Provider, kid: Option<&str>) -> Vec<DecodingKey> {
    match kid {
        Some(kid) => p.by_kid.get(kid).cloned().into_iter().collect(),
        None => p.anonymous.iter().chain(p.by_kid.values()).cloned().collect(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::{
        routing::{get, post},
        Form, Json, Router,
    };
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Throwaway RSA key made for Cosmos's tests; it protects nothing.
    const KEY_DER: &[u8] = include_bytes!("testdata/rsa-test-key.der");
    const KEY_N: &str = include_str!("testdata/rsa-test-key.n");

    /// A minimal OIDC provider on a random port: discovery, JWKS and a token
    /// endpoint that returns whatever ID token the test queued.
    pub struct Provider {
        pub base: String,
        pub issuer: String,
        pub jwks_hits: Arc<AtomicUsize>,
        /// The next token endpoint response's ID token.
        pub next_id_token: Arc<Mutex<Option<String>>>,
        /// The forms the token endpoint received.
        pub token_requests: Arc<Mutex<Vec<HashMap<String, String>>>>,
    }

    pub async fn provider(kid: &'static str) -> Provider {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let issuer = format!("{base}/application/o/atlas/");
        let jwks_hits = Arc::new(AtomicUsize::new(0));
        let next_id_token: Arc<Mutex<Option<String>>> = Arc::default();
        let token_requests: Arc<Mutex<Vec<HashMap<String, String>>>> = Arc::default();

        let discovery = json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{base}/application/o/authorize/"),
            "token_endpoint": format!("{base}/application/o/token/"),
            "jwks_uri": format!("{base}/jwks"),
            "end_session_endpoint": format!("{base}/application/o/atlas/end-session/"),
        });
        let hits = jwks_hits.clone();
        let (next, requests) = (next_id_token.clone(), token_requests.clone());
        let app = Router::new()
            .route(
                "/application/o/atlas/.well-known/openid-configuration",
                get(move || {
                    let d = discovery.clone();
                    async move { Json(d) }
                }),
            )
            .route(
                "/jwks",
                get(move || {
                    hits.fetch_add(1, Ordering::SeqCst);
                    async move {
                        Json(json!({
                            "keys": [{ "kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256", "n": KEY_N.trim(), "e": "AQAB" }]
                        }))
                    }
                }),
            )
            .route(
                "/application/o/token/",
                post(move |Form(form): Form<HashMap<String, String>>| {
                    requests.lock().unwrap().push(form);
                    let token = next.lock().unwrap().take();
                    async move {
                        match token {
                            Some(t) => (axum::http::StatusCode::OK, Json(json!({ "id_token": t, "access_token": "at" }))),
                            None => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" }))),
                        }
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Provider { base, issuer, jwks_hits, next_id_token, token_requests }
    }

    pub fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    pub fn token(kid: &str, claims: serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.into());
        encode(&header, &claims, &EncodingKey::from_rsa_der(KEY_DER)).unwrap()
    }

    pub fn claims(issuer: &str, nonce: &str, groups: &[&str]) -> serde_json::Value {
        json!({
            "iss": issuer,
            "aud": "atlas",
            "sub": "hashed-sub-1",
            "preferred_username": "pwb",
            "name": "Preston",
            "email": "p@example.com",
            "groups": groups,
            "nonce": nonce,
            "exp": now() + 600,
            "iat": now(),
        })
    }

    pub fn cfg(issuer: &str) -> OidcConfig {
        OidcConfig {
            issuer: issuer.into(),
            client_id: "atlas".into(),
            client_secret: "s3cret".into(),
            scopes: "openid profile email".into(),
            allowed_groups: vec![],
        }
    }

    fn invalid<T>(r: Result<T, OidcError>) -> bool {
        matches!(r, Err(OidcError::Invalid(_)))
    }

    #[tokio::test]
    async fn accepts_a_valid_id_token() {
        let p = provider("k1").await;
        let c = OidcClient::new(cfg(&p.issuer));
        let claims = c.verify_id_token(&token("k1", claims(&p.issuer, "n1", &["homelab-users"])), "n1").await.unwrap();
        assert_eq!(claims.preferred_username.as_deref(), Some("pwb"));
        assert_eq!(claims.groups, ["homelab-users"]);
    }

    #[tokio::test]
    async fn rejects_bad_tokens() {
        let p = provider("k1").await;
        let c = OidcClient::new(cfg(&p.issuer));

        assert!(invalid(c.verify_id_token(&token("k1", claims(&p.issuer, "n1", &[])), "other").await), "nonce");

        let mut x = claims(&p.issuer, "n1", &[]);
        x.as_object_mut().unwrap().remove("nonce");
        assert!(invalid(c.verify_id_token(&token("k1", x), "n1").await), "no nonce");

        let mut x = claims(&p.issuer, "n1", &[]);
        x["exp"] = json!(now() - 3600);
        assert!(invalid(c.verify_id_token(&token("k1", x), "n1").await), "expired");

        let mut x = claims(&p.issuer, "n1", &[]);
        x["aud"] = json!("cosmos");
        assert!(invalid(c.verify_id_token(&token("k1", x), "n1").await), "another app's token");

        let x = claims("https://evil.example/application/o/atlas/", "n1", &[]);
        assert!(invalid(c.verify_id_token(&token("k1", x), "n1").await), "another issuer");

        assert!(invalid(c.verify_id_token("not.a.jwt", "n1").await), "garbage");

        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("k1".into());
        let hs = encode(&header, &claims(&p.issuer, "n1", &[]), &EncodingKey::from_secret(b"s3cret")).unwrap();
        assert!(invalid(c.verify_id_token(&hs, "n1").await), "HS256");
    }

    #[tokio::test]
    async fn an_unknown_kid_refetches_at_most_once_a_minute() {
        let p = provider("k1").await;
        let c = OidcClient::new(cfg(&p.issuer));
        c.verify_id_token(&token("k1", claims(&p.issuer, "n", &[])), "n").await.unwrap();
        assert_eq!(p.jwks_hits.load(Ordering::SeqCst), 1);

        assert!(invalid(c.verify_id_token(&token("k2", claims(&p.issuer, "n", &[])), "n").await));
        assert_eq!(p.jwks_hits.load(Ordering::SeqCst), 1, "just fetched, so no refetch");

        *c.last_attempt.write().unwrap() = Some(Instant::now() - REFETCH_AFTER);
        let _ = c.verify_id_token(&token("k2", claims(&p.issuer, "n", &[])), "n").await;
        assert_eq!(p.jwks_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_unreachable_provider_is_unavailable() {
        let c = OidcClient::new(cfg("http://127.0.0.1:9/application/o/atlas/"));
        let r = c.authorize_url("http://a/cb", "s", "n", "v").await;
        assert!(matches!(r, Err(OidcError::Unavailable(_))));
    }

    #[tokio::test]
    async fn the_authorize_url_carries_pkce_state_and_nonce() {
        let p = provider("k1").await;
        let c = OidcClient::new(cfg(&p.issuer));
        let url = Url::parse(&c.authorize_url("https://atlas.example/auth/callback", "st", "no", "verifier").await.unwrap())
            .unwrap();
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "atlas");
        assert_eq!(q["redirect_uri"], "https://atlas.example/auth/callback");
        assert_eq!(q["state"], "st");
        assert_eq!(q["nonce"], "no");
        assert_eq!(q["code_challenge"], pkce_challenge("verifier"));
        assert_eq!(q["code_challenge_method"], "S256");
    }

    #[tokio::test]
    async fn exchanges_a_code_with_the_secret_and_verifier() {
        let p = provider("k1").await;
        let c = OidcClient::new(cfg(&p.issuer));
        *p.next_id_token.lock().unwrap() = Some(token("k1", claims(&p.issuer, "n", &[])));
        let claims = c.exchange("https://atlas.example/auth/callback", "code1", "ver1", "n").await.unwrap();
        assert_eq!(claims.sub, "hashed-sub-1");
        let form = p.token_requests.lock().unwrap()[0].clone();
        assert_eq!(form["code"], "code1");
        assert_eq!(form["code_verifier"], "ver1");
        assert_eq!(form["client_secret"], "s3cret");

        // No token queued: the provider refuses the code.
        assert!(invalid(c.exchange("https://atlas.example/auth/callback", "code2", "v", "n").await));
    }

    #[test]
    fn pkce_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
