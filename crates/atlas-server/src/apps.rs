//! The launcher's apps. From `ATLAS_APPS_FILE` (TOML) when it's set;
//! otherwise the sources this server knows about. Discovery from Cosmos's
//! `cosmos.service` labels comes later, through the Cosmos agent's API.
//!
//! ```toml
//! [[app]]
//! name = "Immich"
//! url = "https://immich.jupiter.sunstead.net"
//! description = "Photos"
//! icon = "photos"
//! ```

use crate::config::SourcesConfig;
use atlas_common::AppLink;
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct AppsFile {
    #[serde(default)]
    app: Vec<AppLink>,
}

pub fn parse(text: &str) -> Result<Vec<AppLink>, String> {
    let file: AppsFile = toml::from_str(text).map_err(|e| e.to_string())?;
    for a in &file.app {
        let url = url::Url::parse(&a.url).map_err(|e| format!("{}: bad url: {e}", a.name))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("{}: only http and https links", a.name));
        }
    }
    Ok(file.app)
}

pub fn load(file: Option<&Path>, sources: &SourcesConfig) -> Result<Vec<AppLink>, String> {
    match file {
        Some(path) => parse(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
            .map_err(|e| format!("{}: {e}", path.display())),
        None => Ok(defaults(sources)),
    }
}

/// The apps Atlas already has addresses for.
fn defaults(sources: &SourcesConfig) -> Vec<AppLink> {
    let mut apps = Vec::new();
    if let Some(oc) = &sources.opencloud {
        apps.push(AppLink {
            name: "OpenCloud".into(),
            url: oc.urls.public.to_string(),
            description: Some("Files".into()),
            icon: Some("files".into()),
        });
    }
    if let Some(i) = &sources.immich {
        apps.push(AppLink { name: "Immich".into(), url: i.public.to_string(), description: Some("Photos".into()), icon: Some("photos".into()) });
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_file() {
        let apps = parse(
            r#"
            [[app]]
            name = "Gitea"
            url = "https://gitea.example"
            icon = "git"

            [[app]]
            name = "Cosmos"
            url = "https://cosmos.example"
            description = "Servers"
            "#,
        )
        .unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].icon.as_deref(), Some("git"));
        assert_eq!(apps[1].description.as_deref(), Some("Servers"));
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn refuses_odd_links() {
        assert!(parse("[[app]]\nname = \"x\"\nurl = \"javascript:alert(1)\"").is_err());
        assert!(parse("[[app]]\nname = \"x\"\nurl = \"not a url\"").is_err());
        assert!(parse("[[app]]\nurl = \"https://a.example\"").is_err(), "a name is required");
    }

    #[test]
    fn defaults_to_the_configured_sources() {
        assert!(defaults(&SourcesConfig::default()).is_empty());
    }
}
