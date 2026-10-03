//! The few pages the server renders itself: sign-in problems, shown before
//! the app (or a session) exists. Plain and self-contained.

use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
};

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A message page with one way forward.
pub fn message(status: StatusCode, title: &str, body: &str, link: Option<(&str, &str)>) -> Response {
    let link = link
        .map(|(href, text)| format!(r#"<p><a href="{}">{}</a></p>"#, escape(href), escape(text)))
        .unwrap_or_default();
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} - Atlas</title>
<style>
  :root {{ color-scheme: light dark; font-family: system-ui, sans-serif; }}
  body {{ max-width: 32rem; margin: 20vh auto 0; padding: 0 1.5rem; line-height: 1.5; }}
  h1 {{ font-size: 1.25rem; }}
  a {{ color: inherit; }}
</style>
</head>
<body>
<h1>{title}</h1>
<p>{body}</p>
{link}
</body>
</html>
"#,
        title = escape(title),
        body = escape(body),
    );
    (status, [(header::CACHE_CONTROL, "no-store")], Html(html)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_everything_it_is_given() {
        assert_eq!(escape(r#"<a href="x">'&'</a>"#), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }
}
