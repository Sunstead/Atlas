//! The session cookie. `__Host-` prefixed and `Secure` when Atlas is served
//! over HTTPS (production); plain `atlas_session` over HTTP (local
//! development), where browsers won't keep a `Secure` cookie reliably.

use axum::http::{header, HeaderMap, HeaderValue};
use url::Url;

#[derive(Debug, Clone)]
pub struct CookieSpec {
    pub name: &'static str,
    pub secure: bool,
}

impl CookieSpec {
    pub fn for_url(public_url: &Url) -> Self {
        if public_url.scheme() == "https" {
            Self { name: "__Host-atlas_session", secure: true }
        } else {
            Self { name: "atlas_session", secure: false }
        }
    }

    /// The session token from the request's `Cookie` headers.
    pub fn read(&self, headers: &HeaderMap) -> Option<String> {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(k, _)| *k == self.name)
            .map(|(_, v)| v.trim_matches('"').to_owned())
            .filter(|v| !v.is_empty())
    }

    /// `Lax`, so following a link or a browser search into Atlas carries the
    /// session; state-changing requests are guarded separately (`csrf`).
    pub fn set(&self, token: &str, max_age_secs: i64) -> HeaderValue {
        self.header(token, max_age_secs)
    }

    pub fn clear(&self) -> HeaderValue {
        self.header("", 0)
    }

    fn header(&self, value: &str, max_age: i64) -> HeaderValue {
        let secure = if self.secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!(
            "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}",
            self.name
        ))
        .expect("cookie tokens are URL-safe base64")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url: &str) -> CookieSpec {
        CookieSpec::for_url(&Url::parse(url).unwrap())
    }

    #[test]
    fn https_gets_the_host_prefix_and_secure() {
        let s = spec("https://atlas.example");
        assert_eq!(s.name, "__Host-atlas_session");
        let v = s.set("tok", 60);
        let v = v.to_str().unwrap();
        assert!(v.starts_with("__Host-atlas_session=tok; Path=/; HttpOnly; SameSite=Lax; Max-Age=60"));
        assert!(v.ends_with("; Secure"));
    }

    #[test]
    fn http_is_plain() {
        let s = spec("http://localhost:1420");
        assert_eq!(s.name, "atlas_session");
        assert!(!s.set("tok", 60).to_str().unwrap().contains("Secure"));
    }

    #[test]
    fn reads_its_cookie_among_others() {
        let s = spec("http://localhost:1420");
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, "theme=dark; atlas_session=abc".parse().unwrap());
        h.append(header::COOKIE, "other=1".parse().unwrap());
        assert_eq!(s.read(&h).as_deref(), Some("abc"));

        let mut h = HeaderMap::new();
        h.append(header::COOKIE, "xatlas_session=no; atlas_session=".parse().unwrap());
        assert_eq!(s.read(&h), None);
    }

    #[test]
    fn clearing_expires_it() {
        assert!(spec("https://a.example").clear().to_str().unwrap().contains("Max-Age=0"));
    }
}
