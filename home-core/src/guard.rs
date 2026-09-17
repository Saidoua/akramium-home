//! Two checks every request passes before a handler sees it.
//!
//! **Host.** A page on the internet can point a name it controls at this machine's LAN
//! address (DNS rebinding) and then talk to the daemon as if it were its own site. Such a
//! request arrives with the attacker's name in `Host`, so only names we know are answered:
//! IP literals, `localhost`, and the configured `host_names`.
//!
//! **Cross-site writes.** A page elsewhere could make the browser send a request that
//! changes something. Browsers attach `Origin` to every such request, so a write whose
//! `Origin` is not this daemon is refused. `Sec-Fetch-Site` says the same thing more
//! directly, but browsers only send it over https, so it is a second signal, not the rule.
//! Programs that are not browsers (curl, sync tools) send neither and carry no ambient
//! cookie, so they pass.

use crate::Core;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::IpAddr;

/// `host[:port]` without the port; IPv6 literals keep their brackets off.
fn host_name(value: &str) -> &str {
    if let Some(rest) = value.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match value.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => value,
    }
}

pub fn host_allowed(value: &str, names: &[String]) -> bool {
    let host = host_name(value.trim()).trim_end_matches('.');
    if host.is_empty() {
        return false;
    }
    host.parse::<IpAddr>().is_ok() || host.eq_ignore_ascii_case("localhost") || names.iter().any(|n| n.trim_end_matches('.').eq_ignore_ascii_case(host))
}

/// Is a write with these headers safe to act on?
pub fn write_allowed(origin: Option<&str>, fetch_site: Option<&str>, host: &str) -> bool {
    if let Some(site) = fetch_site
        && !matches!(site, "same-origin" | "none")
    {
        return false;
    }
    match origin {
        None => true,
        Some(origin) => {
            let Some((_, rest)) = origin.split_once("://") else { return false };
            rest.eq_ignore_ascii_case(host)
        }
    }
}

fn text(request: &Request<Body>, name: header::HeaderName) -> Option<&str> {
    request.headers().get(name)?.to_str().ok()
}

pub async fn check(State(core): State<Core>, request: Request<Body>, next: Next) -> Response {
    // HTTP/2 carries the name in the address, HTTP/1 in the header.
    let host = request.uri().authority().map(|a| a.as_str().to_string()).or_else(|| text(&request, header::HOST).map(str::to_string));
    let Some(host) = host else {
        return (StatusCode::BAD_REQUEST, "The request names no host.").into_response();
    };
    if !host_allowed(&host, &core.config.host_names) {
        tracing::warn!(%host, "refused a request for a name this install does not answer to");
        let message = format!(
            "This Akramium Home does not answer to the name {}. If that is this machine's name, add it to host_names in home.toml.",
            host_name(&host)
        );
        return (StatusCode::MISDIRECTED_REQUEST, message).into_response();
    }

    let writes = !matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    // `/dav` takes no cookie, so there is nothing for another site to ride on, and WebDAV
    // clients send no Origin anyway.
    if writes && !request.uri().path().starts_with("/dav") {
        let origin = text(&request, header::ORIGIN);
        let fetch_site = request.headers().get("sec-fetch-site").and_then(|v| v.to_str().ok());
        if !write_allowed(origin, fetch_site, &host) {
            tracing::warn!(?origin, ?fetch_site, path = %request.uri().path(), "refused a cross-site write");
            return (StatusCode::FORBIDDEN, "This request came from another site.").into_response();
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        let names = vec!["akramium.local".to_string(), "nas.home".to_string()];
        for ok in ["localhost", "localhost:11720", "127.0.0.1:11720", "192.168.1.20", "[::1]:11720", "[fe80::1]", "akramium.local", "AKRAMIUM.LOCAL:11720", "akramium.local.", "nas.home:80"] {
            assert!(host_allowed(ok, &names), "{ok} should be answered");
        }
        for bad in ["", "evil.example", "evil.example:11720", "akramium.local.evil.example", "127.0.0.1.evil.example", "localhost.evil.example"] {
            assert!(!host_allowed(bad, &names), "{bad} should be refused");
        }
    }

    #[test]
    fn writes() {
        let host = "akramium.local:11720";
        assert!(write_allowed(None, None, host), "curl and sync tools send neither header");
        assert!(write_allowed(Some("http://akramium.local:11720"), None, host));
        assert!(write_allowed(Some("https://AKRAMIUM.local:11720"), Some("same-origin"), host));
        assert!(write_allowed(None, Some("none"), host), "typed into the address bar");
        assert!(!write_allowed(Some("http://evil.example"), None, host));
        assert!(!write_allowed(Some("http://akramium.local"), None, host), "another port is another origin");
        assert!(!write_allowed(Some("null"), None, host), "sandboxed frames and file pages");
        assert!(!write_allowed(None, Some("cross-site"), host));
        assert!(!write_allowed(Some("http://akramium.local:11720"), Some("same-site"), host));
    }
}
