//! Where to send the browser after sign-in. It comes from the URL
//! (`/auth/login?return_to=`), so anyone can craft it: accept only a path on
//! Atlas's own origin, or sign-in becomes an open redirect.

use url::Url;

pub fn safe_return_to(public_url: &Url, raw: Option<&str>) -> String {
    raw.and_then(|r| check(public_url, r)).unwrap_or_else(|| "/".to_owned())
}

fn check(public_url: &Url, raw: &str) -> Option<String> {
    // A path, and only a path: not `//host` (scheme-relative) or `/\host`
    // (which browsers also read as scheme-relative).
    if !raw.starts_with('/') || raw.starts_with("//") || raw.starts_with("/\\") {
        return None;
    }
    if raw.chars().any(|c| c.is_control() || c == '\\') {
        return None;
    }
    let joined = public_url.join(raw).ok()?;
    if joined.origin() != public_url.origin() {
        return None;
    }
    // Back into sign-in would loop.
    if joined.path() == "/auth" || joined.path().starts_with("/auth/") {
        return None;
    }
    let mut out = joined.path().to_owned();
    if let Some(q) = joined.query() {
        out.push('?');
        out.push_str(q);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(raw: &str) -> String {
        safe_return_to(&Url::parse("https://atlas.example").unwrap(), Some(raw))
    }

    #[test]
    fn keeps_paths_and_queries() {
        assert_eq!(rt("/search?q=tax%202025&type=file"), "/search?q=tax%202025&type=file");
        assert_eq!(rt("/item/3/abc"), "/item/3/abc");
        assert_eq!(rt("/"), "/");
    }

    #[test]
    fn refuses_other_origins() {
        for bad in [
            "//evil.com",
            "//evil.com/search",
            "/\\evil.com",
            "/\\/evil.com",
            "https://evil.com",
            "http://atlas.example/",
            "evil.com",
            "javascript:alert(1)",
            "/\r\nSet-Cookie:x",
            "/\tfoo",
            "",
        ] {
            assert_eq!(rt(bad), "/", "{bad:?}");
        }
    }

    #[test]
    fn refuses_sign_in_itself() {
        assert_eq!(rt("/auth/login?return_to=/x"), "/");
        assert_eq!(rt("/auth/callback"), "/");
    }

    #[test]
    fn missing_is_home() {
        assert_eq!(safe_return_to(&Url::parse("https://atlas.example").unwrap(), None), "/");
    }
}
