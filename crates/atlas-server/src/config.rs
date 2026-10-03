//! Configuration from `ATLAS_*` environment variables. Secrets also read
//! from `<NAME>_FILE`, for Docker secrets or a mounted file.

use std::net::SocketAddr;
use std::path::PathBuf;
use url::Url;

#[derive(Debug, Clone)]
pub struct Config {
    /// `ATLAS_BIND`, default `0.0.0.0:8080`.
    pub bind: SocketAddr,
    /// `ATLAS_PUBLIC_URL`: the origin users reach Atlas at. Sign-in redirects
    /// and the same-origin checks use it. Default `http://localhost:1420`
    /// (the Vite dev server); required with OIDC.
    pub public_url: Url,
    /// `ATLAS_WEB_DIR`: the web app build to serve. Unset means API only,
    /// which is how development runs (Vite serves the app and proxies here).
    pub web_dir: Option<PathBuf>,
    /// `ATLAS_STATE_DIR`: where `atlas.db` lives. Default `.data/state`; the
    /// image sets `/state`, the backed-up volume.
    pub state_dir: PathBuf,
    /// `ATLAS_INDEX_DIR`: the search index. Default `.data/index`; the image
    /// sets `/index` (derived data, not backed up).
    pub index_dir: PathBuf,
    pub auth: AuthMode,
    /// `ATLAS_MASTER_KEY[_FILE]`: base64 of 32 bytes, sealing credentials.
    pub master_key: Option<String>,
    pub sources: SourcesConfig,
}

#[derive(Debug, Clone)]
pub enum AuthMode {
    /// `ATLAS_OIDC_*`: the real thing.
    Oidc(OidcConfig),
    /// `ATLAS_DEV_USER` without OIDC: every sign-in is this user. For local
    /// development only; logged loudly.
    Dev { username: String },
    /// Neither: nobody can sign in. The server still runs (health checks,
    /// the app shell), and sign-in says it isn't configured.
    Disabled,
}

#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// `ATLAS_OIDC_ISSUER`, e.g. `https://auth.jupiter.sunstead.net/application/o/atlas/`.
    pub issuer: String,
    /// `ATLAS_OIDC_CLIENT_ID`.
    pub client_id: String,
    /// `ATLAS_OIDC_CLIENT_SECRET[_FILE]`.
    pub client_secret: String,
    /// `ATLAS_OIDC_SCOPES`, default `openid profile email`.
    pub scopes: String,
    /// `ATLAS_OIDC_ALLOWED_GROUPS`, comma separated. Empty means anyone the
    /// provider lets through (Authentik's policy binding is the first gate).
    pub allowed_groups: Vec<String>,
}

/// Where each kind of source lives. A kind is offered only when configured.
#[derive(Debug, Clone, Default)]
pub struct SourcesConfig {
    pub immich: Option<ServiceUrls>,
    pub opencloud: Option<OpenCloudConfig>,
}

#[derive(Debug, Clone)]
pub struct ServiceUrls {
    /// Where Atlas calls the API, e.g. `http://immich-server:2283`.
    pub api: Url,
    /// What links in results point at, e.g. `https://immich.jupiter.sunstead.net`.
    pub public: Url,
}

#[derive(Debug, Clone)]
pub struct OpenCloudConfig {
    /// `ATLAS_OPENCLOUD_URL` / `ATLAS_OPENCLOUD_PUBLIC_URL`.
    pub urls: ServiceUrls,
    /// `ATLAS_OPENCLOUD_USERS_DIR`: the PosixFS personal spaces, mounted
    /// read-only (`/data/files/users`). Each user's root is `<dir>/<username>`.
    pub users_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{name} is invalid: {reason}")]
    Invalid { name: &'static str, reason: String },
    #[error("{name} is required {when}")]
    Missing { name: &'static str, when: &'static str },
    #[error("{name}: can't read {path}: {reason}")]
    File { name: String, path: String, reason: String },
}

struct Env<F>(F);

impl<F: Fn(&str) -> Option<String>> Env<F> {
    fn get(&self, name: &str) -> Option<String> {
        (self.0)(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
    }

    /// `NAME`, or the contents of the file `NAME_FILE` names.
    fn secret(&self, name: &str) -> Result<Option<String>, ConfigError> {
        if let Some(v) = self.get(name) {
            return Ok(Some(v));
        }
        let file_var = format!("{name}_FILE");
        match self.get(&file_var) {
            None => Ok(None),
            Some(path) => std::fs::read_to_string(&path)
                .map(|s| Some(s.trim().to_owned()).filter(|s| !s.is_empty()))
                .map_err(|e| ConfigError::File { name: file_var, path, reason: e.to_string() }),
        }
    }

    fn url(&self, name: &'static str) -> Result<Option<Url>, ConfigError> {
        self.get(name)
            .map(|v| Url::parse(&v).map_err(|e| ConfigError::Invalid { name, reason: e.to_string() }))
            .transpose()
    }

    fn service(&self, api: &'static str, public: &'static str) -> Result<Option<ServiceUrls>, ConfigError> {
        let Some(api_url) = self.url(api)? else { return Ok(None) };
        let public = self.url(public)?.unwrap_or_else(|| api_url.clone());
        Ok(Some(ServiceUrls { api: api_url, public }))
    }
}

impl Config {
    /// `get` looks a variable up; tests pass a map instead of the process env.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let env = Env(get);

        let bind = match env.get("ATLAS_BIND") {
            Some(v) => v.parse().map_err(|e: std::net::AddrParseError| ConfigError::Invalid {
                name: "ATLAS_BIND",
                reason: e.to_string(),
            })?,
            None => SocketAddr::from(([0, 0, 0, 0], 8080)),
        };

        let explicit_public = env.url("ATLAS_PUBLIC_URL")?;
        let public_url = explicit_public.clone().unwrap_or_else(|| Url::parse("http://localhost:1420").unwrap());

        let auth = match env.get("ATLAS_OIDC_ISSUER") {
            Some(issuer) => {
                if explicit_public.is_none() {
                    return Err(ConfigError::Missing { name: "ATLAS_PUBLIC_URL", when: "with ATLAS_OIDC_ISSUER" });
                }
                let client_id = env
                    .get("ATLAS_OIDC_CLIENT_ID")
                    .ok_or(ConfigError::Missing { name: "ATLAS_OIDC_CLIENT_ID", when: "with ATLAS_OIDC_ISSUER" })?;
                let client_secret = env.secret("ATLAS_OIDC_CLIENT_SECRET")?.ok_or(ConfigError::Missing {
                    name: "ATLAS_OIDC_CLIENT_SECRET",
                    when: "with ATLAS_OIDC_ISSUER",
                })?;
                AuthMode::Oidc(OidcConfig {
                    issuer,
                    client_id,
                    client_secret,
                    scopes: env.get("ATLAS_OIDC_SCOPES").unwrap_or_else(|| "openid profile email".into()),
                    allowed_groups: env
                        .get("ATLAS_OIDC_ALLOWED_GROUPS")
                        .map(|v| v.split(',').map(|g| g.trim().to_owned()).filter(|g| !g.is_empty()).collect())
                        .unwrap_or_default(),
                })
            }
            None => match env.get("ATLAS_DEV_USER") {
                Some(username) => AuthMode::Dev { username },
                None => AuthMode::Disabled,
            },
        };

        let sources = SourcesConfig {
            immich: env.service("ATLAS_IMMICH_URL", "ATLAS_IMMICH_PUBLIC_URL")?,
            opencloud: match env.service("ATLAS_OPENCLOUD_URL", "ATLAS_OPENCLOUD_PUBLIC_URL")? {
                None => None,
                Some(urls) => Some(OpenCloudConfig {
                    urls,
                    users_dir: env.get("ATLAS_OPENCLOUD_USERS_DIR").map(PathBuf::from).ok_or(ConfigError::Missing {
                        name: "ATLAS_OPENCLOUD_USERS_DIR",
                        when: "with ATLAS_OPENCLOUD_URL",
                    })?,
                }),
            },
        };

        Ok(Self {
            bind,
            public_url,
            web_dir: env.get("ATLAS_WEB_DIR").map(PathBuf::from),
            state_dir: env.get("ATLAS_STATE_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".data/state")),
            index_dir: env.get("ATLAS_INDEX_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".data/index")),
            auth,
            master_key: env.secret("ATLAS_MASTER_KEY")?,
            sources,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn from(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_env(|k| map.get(k).cloned())
    }

    const OIDC: &[(&str, &str)] = &[
        ("ATLAS_PUBLIC_URL", "https://atlas.example"),
        ("ATLAS_OIDC_ISSUER", "https://auth.example/application/o/atlas/"),
        ("ATLAS_OIDC_CLIENT_ID", "atlas"),
        ("ATLAS_OIDC_CLIENT_SECRET", "s3cret"),
        ("ATLAS_OIDC_ALLOWED_GROUPS", "homelab-users, family ,"),
    ];

    #[test]
    fn defaults() {
        let c = from(&[]).unwrap();
        assert_eq!(c.bind.to_string(), "0.0.0.0:8080");
        assert_eq!(c.public_url.as_str(), "http://localhost:1420/");
        assert!(c.web_dir.is_none());
        assert_eq!(c.state_dir, PathBuf::from(".data/state"));
        assert_eq!(c.index_dir, PathBuf::from(".data/index"));
        assert!(matches!(c.auth, AuthMode::Disabled));
        assert!(c.master_key.is_none());
        assert!(c.sources.immich.is_none() && c.sources.opencloud.is_none());
    }

    #[test]
    fn reads_bind_and_web_dir() {
        let c = from(&[("ATLAS_BIND", "127.0.0.1:9000"), ("ATLAS_WEB_DIR", "/web")]).unwrap();
        assert_eq!(c.bind.to_string(), "127.0.0.1:9000");
        assert_eq!(c.web_dir.unwrap(), PathBuf::from("/web"));
    }

    #[test]
    fn rejects_a_bad_bind() {
        assert!(matches!(from(&[("ATLAS_BIND", "nope")]), Err(ConfigError::Invalid { name: "ATLAS_BIND", .. })));
    }

    #[test]
    fn reads_oidc() {
        let AuthMode::Oidc(o) = from(OIDC).unwrap().auth else { panic!("not oidc") };
        assert_eq!(o.client_id, "atlas");
        assert_eq!(o.client_secret, "s3cret");
        assert_eq!(o.scopes, "openid profile email");
        assert_eq!(o.allowed_groups, ["homelab-users", "family"]);
    }

    #[test]
    fn oidc_needs_a_public_url_and_a_secret() {
        let without = |name: &str| OIDC.iter().copied().filter(|(k, _)| *k != name).collect::<Vec<_>>();
        assert!(matches!(from(&without("ATLAS_PUBLIC_URL")), Err(ConfigError::Missing { name: "ATLAS_PUBLIC_URL", .. })));
        assert!(matches!(
            from(&without("ATLAS_OIDC_CLIENT_SECRET")),
            Err(ConfigError::Missing { name: "ATLAS_OIDC_CLIENT_SECRET", .. })
        ));
    }

    #[test]
    fn secrets_come_from_files_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        std::fs::write(&path, "from-a-file\n").unwrap();
        let p = path.to_str().unwrap();
        let mut vars: Vec<_> = OIDC.iter().copied().filter(|(k, _)| *k != "ATLAS_OIDC_CLIENT_SECRET").collect();
        vars.push(("ATLAS_OIDC_CLIENT_SECRET_FILE", p));
        vars.push(("ATLAS_MASTER_KEY_FILE", p));
        let c = from(&vars).unwrap();
        let AuthMode::Oidc(o) = c.auth else { panic!() };
        assert_eq!(o.client_secret, "from-a-file");
        assert_eq!(c.master_key.as_deref(), Some("from-a-file"));

        let missing = from(&[("ATLAS_MASTER_KEY_FILE", "/nope/not/here")]);
        assert!(matches!(missing, Err(ConfigError::File { .. })));
    }

    #[test]
    fn dev_user_only_without_oidc() {
        assert!(matches!(from(&[("ATLAS_DEV_USER", "pwb")]).unwrap().auth, AuthMode::Dev { .. }));
        let mut vars = OIDC.to_vec();
        vars.push(("ATLAS_DEV_USER", "pwb"));
        assert!(matches!(from(&vars).unwrap().auth, AuthMode::Oidc(_)));
    }

    #[test]
    fn sources_need_their_settings() {
        let c = from(&[("ATLAS_IMMICH_URL", "http://immich-server:2283")]).unwrap();
        let immich = c.sources.immich.unwrap();
        assert_eq!(immich.public, immich.api, "public defaults to the API URL");

        assert!(matches!(
            from(&[("ATLAS_OPENCLOUD_URL", "http://opencloud:9200")]),
            Err(ConfigError::Missing { name: "ATLAS_OPENCLOUD_USERS_DIR", .. })
        ));
        let c = from(&[
            ("ATLAS_OPENCLOUD_URL", "http://opencloud:9200"),
            ("ATLAS_OPENCLOUD_PUBLIC_URL", "https://opencloud.example"),
            ("ATLAS_OPENCLOUD_USERS_DIR", "/data/files/users"),
        ])
        .unwrap();
        assert_eq!(c.sources.opencloud.unwrap().urls.public.as_str(), "https://opencloud.example/");
    }
}
