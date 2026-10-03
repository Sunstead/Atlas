//! Configuration from `ATLAS_*` environment variables.
//!
//! Later milestones add the OIDC client, the master key and source roots,
//! each secret with a `_FILE` variant.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// `ATLAS_BIND`, default `0.0.0.0:8080`.
    pub bind: SocketAddr,
    /// `ATLAS_WEB_DIR`: the web app build to serve. Unset means API only,
    /// which is how development runs (Vite serves the app and proxies here).
    pub web_dir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{name} is invalid: {reason}")]
    Invalid { name: &'static str, reason: String },
}

impl Config {
    /// `get` looks a variable up; tests pass a map instead of the process env.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let bind = match get("ATLAS_BIND").filter(|v| !v.is_empty()) {
            Some(v) => v.parse().map_err(|e: std::net::AddrParseError| ConfigError::Invalid {
                name: "ATLAS_BIND",
                reason: e.to_string(),
            })?,
            None => SocketAddr::from(([0, 0, 0, 0], 8080)),
        };
        let web_dir = get("ATLAS_WEB_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
        Ok(Self { bind, web_dir })
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

    #[test]
    fn defaults() {
        let c = from(&[]).unwrap();
        assert_eq!(c.bind.to_string(), "0.0.0.0:8080");
        assert!(c.web_dir.is_none());
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
}
