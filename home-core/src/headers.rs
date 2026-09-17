//! Response headers that make the LAN threat model hold: pages run only our own scripts,
//! user files never run anything, nothing is sniffed, nothing is framed.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::LazyLock;

/// For the app's own pages.
pub static APP_CSP: LazyLock<HeaderValue> = LazyLock::new(|| {
    HeaderValue::from_static(
        "default-src 'self'; img-src 'self' blob: data:; media-src 'self' blob:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'",
    )
});

/// For anything a person uploaded: no scripts, no origin. Inline styles stay allowed only
/// because Chromium's own image and text viewers use them; HTML is never served inline.
/// PDFs take the same policy: Chromium's PDF viewer starts under `sandbox` for a top-level
/// PDF (checked on Akramium 0.1.8, Chromium 153, against the same file served both ways).
pub static USER_CONTENT_CSP: LazyLock<HeaderValue> = LazyLock::new(|| HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'"));

/// Applied to every response.
pub async fn security(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if !headers.contains_key(header::REFERRER_POLICY) {
        headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    }
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    if !headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        let is_html = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"));
        let csp = if is_html { APP_CSP.clone() } else { USER_CONTENT_CSP.clone() };
        headers.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    response
}

/// Mime types a browser may render inline from user content without running anything.
pub fn inline_safe(mime: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or("").trim();
    mime.starts_with("image/") && mime != "image/svg+xml"
        || mime.starts_with("video/")
        || mime.starts_with("audio/")
        || mime == "application/pdf"
        || mime == "text/plain"
        || mime == "text/markdown"
        || mime == "text/csv"
        || mime == "application/json"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_rules() {
        assert!(inline_safe("image/png"));
        assert!(inline_safe("application/pdf"));
        assert!(inline_safe("text/plain; charset=utf-8"));
        assert!(!inline_safe("image/svg+xml"));
        assert!(!inline_safe("text/html"));
        assert!(!inline_safe("application/xhtml+xml"));
        assert!(!inline_safe("text/xml"));
        assert!(USER_CONTENT_CSP.to_str().unwrap().starts_with("sandbox"));
    }
}
