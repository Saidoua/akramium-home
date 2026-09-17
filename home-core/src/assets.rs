//! Pages and scripts embedded in the binary. During development `HOME_ASSETS_DIR` points at
//! the repository root and files are read from disk instead, so a CSS change needs no rebuild.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mime_guess::mime;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct Asset {
    /// URL path below the module's prefix, e.g. `app.js`.
    pub path: &'static str,
    /// Path from the repository root, for the development override.
    pub source: &'static str,
    pub bytes: &'static [u8],
}

#[macro_export]
macro_rules! asset {
    ($path:literal, $source:literal) => {
        $crate::assets::Asset { path: $path, source: $source, bytes: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../", $source)) }
    };
}

pub fn content_type(path: &str) -> HeaderValue {
    let guess = mime_guess::from_path(path).first_or_octet_stream();
    let mut value = guess.to_string();
    if guess.type_() == mime::TEXT || guess.subtype() == mime::JAVASCRIPT || guess.subtype() == mime::JSON {
        value.push_str("; charset=utf-8");
    }
    HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("application/octet-stream"))
}

fn dev_dir() -> Option<PathBuf> {
    std::env::var_os("HOME_ASSETS_DIR").map(PathBuf::from)
}

/// The asset's bytes, from disk when the override is set.
pub fn bytes(asset: &Asset) -> std::borrow::Cow<'static, [u8]> {
    if let Some(dir) = dev_dir()
        && let Ok(b) = std::fs::read(dir.join(asset.source))
    {
        return std::borrow::Cow::Owned(b);
    }
    std::borrow::Cow::Borrowed(asset.bytes)
}

/// A response for an asset: right content type, cached briefly, app-page CSP for HTML.
pub fn respond(asset: &Asset) -> Response {
    let body = bytes(asset);
    let mut response = (StatusCode::OK, body.into_owned()).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type(asset.path));
    let cache = if dev_dir().is_some() { "no-store" } else { "private, max-age=300" };
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if asset.path.ends_with(".html") {
        headers.insert(header::CONTENT_SECURITY_POLICY, crate::headers::APP_CSP.clone());
    }
    response
}

/// Finds `path` in a table and serves it, or 404.
pub fn serve(table: &[Asset], path: &str) -> Response {
    match table.iter().find(|a| a.path == path) {
        Some(a) => respond(a),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serves an HTML page with `{{key}}` placeholders replaced (values HTML-escaped).
pub fn page(asset: &Asset, values: &[(&str, &str)]) -> Response {
    let mut text = String::from_utf8_lossy(&bytes(asset)).into_owned();
    for (k, v) in values {
        text = text.replace(&format!("{{{{{k}}}}}"), &escape(v));
    }
    let mut response = respond(asset);
    *response.body_mut() = text.into();
    response
}

/// Like `page`, plus `[[key]]` placeholders replaced as they are: for HTML the caller built
/// from escaped pieces. Text values go in first, so the HTML (which may quote a file called
/// `{{title}}`) is never scanned for text placeholders.
pub fn page_with_html(asset: &Asset, status: StatusCode, html: &[(&str, &str)], values: &[(&str, &str)]) -> Response {
    let mut text = String::from_utf8_lossy(&bytes(asset)).into_owned();
    for (k, v) in values {
        text = text.replace(&format!("{{{{{k}}}}}"), &escape(v));
    }
    for (k, v) in html {
        text = text.replace(&format!("[[{k}]]"), v);
    }
    let mut response = respond(asset);
    *response.status_mut() = status;
    *response.body_mut() = text.into();
    response
}

pub fn escape(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type("a.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("a.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("a.png"), "image/png");
    }
}
