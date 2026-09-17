//! Serving a file's bytes: Range requests, ETag, HEAD, all from tower-http's `ServeFile`,
//! with the headers that keep uploaded content inert.

use axum::body::Body;
use axum::http::{HeaderValue, Request, header};
use axum::response::{IntoResponse, Response};
use home_core::headers;
use std::path::Path;
use tower::ServiceExt;
use tower_http::services::ServeFile;

/// `inline` for types a browser can show safely, `attachment` for everything else.
pub fn disposition(name: &str, mime: &str, force_download: bool) -> HeaderValue {
    let kind = if force_download || !headers::inline_safe(mime) { "attachment" } else { "inline" };
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '_' }).collect::<String>().replace('"', "_");
    let utf8 = percent(name);
    HeaderValue::from_str(&format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{utf8}"))
        .unwrap_or(HeaderValue::from_static("attachment"))
}

fn percent(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub async fn serve(path: &Path, name: &str, mime: &str, force_download: bool, request: Request<Body>) -> Response {
    let mime_value = HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream"));
    let service = ServeFile::new_with_mime(path, &mime.parse().unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM));
    let mut response = match service.oneshot(request).await {
        Ok(r) => r.into_response(),
        Err(never) => match never {},
    };
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, mime_value);
    h.insert(header::CONTENT_DISPOSITION, disposition(name, mime, force_download));
    h.insert(header::CONTENT_SECURITY_POLICY, headers::user_content_csp(mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispositions() {
        assert!(disposition("a.png", "image/png", false).to_str().unwrap().starts_with("inline;"));
        assert!(disposition("a.html", "text/html", false).to_str().unwrap().starts_with("attachment;"));
        assert!(disposition("a.png", "image/png", true).to_str().unwrap().starts_with("attachment;"));
        let v = disposition("été \"x\".txt", "text/plain", false);
        let s = v.to_str().unwrap();
        assert!(s.contains("filename=\"_t_ _x_.txt\""), "{s}");
        assert!(s.contains("filename*=UTF-8''%C3%A9t%C3%A9%20%22x%22.txt"), "{s}");
    }
}
