//! What a share link opens, without an account: a page for the shared file or folder, and
//! the bytes behind it. Rendered on the server from escaped pieces; the page runs no script.

use super::ASSETS;
use crate::Drive;
use crate::store::shares::Opened;
use crate::store::{Entry, blobs};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use home_core::assets::{self, escape};
use home_core::Error;
use serde::Deserialize;

const TEXT_PREVIEW: u64 = 64 * 1024;

fn share_page() -> &'static assets::Asset {
    ASSETS.iter().find(|a| a.path == "share.html").expect("share.html is embedded")
}

fn finish(mut response: Response) -> Response {
    let h = response.headers_mut();
    h.insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
    h.insert(axum::http::header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(axum::http::header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response
}

fn message(status: StatusCode, title: &str, text: &str) -> Response {
    let body = format!("<div class=\"notice\"><h2>{}</h2><p>{}</p></div>", escape(title), escape(text));
    finish(assets::page_with_html(share_page(), status, &[("body", &body)], &[("title", title)]))
}

fn failure(e: Error) -> Response {
    match e {
        Error::Gone => message(StatusCode::GONE, "This link has expired", "Ask the person who sent it for a new one."),
        Error::NotFound => message(StatusCode::NOT_FOUND, "Nothing here", "The link is wrong, or what it pointed to was removed."),
        other => {
            tracing::error!(error = %other, "share page failed");
            message(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", "Try again in a moment.")
        }
    }
}

fn size(n: i64) -> String {
    let mut v = n as f64;
    if n < 1024 {
        return format!("{n} B");
    }
    let mut unit = "B";
    for u in ["KB", "MB", "GB", "TB"] {
        v /= 1024.0;
        unit = u;
        if v < 1024.0 {
            break;
        }
    }
    if v < 10.0 { format!("{v:.1} {unit}") } else { format!("{v:.0} {unit}") }
}

fn crumbs_html(token: &str, inside: &[Entry]) -> String {
    let mut out = String::from("<nav class=\"crumbs\">");
    for (i, e) in inside.iter().enumerate() {
        if i > 0 {
            out.push_str("<span class=\"sep\">/</span>");
        }
        if i + 1 == inside.len() {
            out.push_str(&format!("<span class=\"here\">{}</span>", escape(&e.name)));
        } else {
            out.push_str(&format!("<a href=\"/s/{}/i/{}\">{}</a>", escape(token), e.id, escape(&e.name)));
        }
    }
    out.push_str("</nav>");
    out
}

async fn render(drive: &Drive, opened: &Opened, id: Option<i64>) -> Result<Response, Error> {
    let token = &opened.share.token;
    let (entry, inside) = drive.store.share_entry(opened, id).await?;
    let mut body = String::new();
    if inside.len() > 1 {
        body.push_str(&crumbs_html(token, &inside));
    }

    if entry.is_dir {
        let children = drive.store.share_children(opened, entry.id).await?;
        body.push_str("<div class=\"files\">");
        if children.is_empty() {
            body.push_str("<div class=\"notice\"><p>This folder is empty.</p></div>");
        }
        for c in children {
            let icon = if c.is_dir { "folder" } else if c.mime.as_deref().is_some_and(|m| m.starts_with("image/")) { "image" } else { "file" };
            body.push_str(&format!(
                "<a class=\"row{}\" href=\"/s/{}/i/{}\"><svg class=\"icon\" aria-hidden=\"true\"><use href=\"/drive/icons.svg#{}\"></use></svg><span class=\"name\">{}</span><span class=\"size\">{}</span></a>",
                if c.is_dir { " dir" } else { "" },
                escape(token),
                c.id,
                icon,
                escape(&c.name),
                if c.is_dir { String::new() } else { size(c.size) },
            ));
        }
        body.push_str("</div>");
    } else {
        let url = format!("/s/{}/content/{}", escape(token), entry.id);
        let mime = entry.mime.clone().unwrap_or_default();
        body.push_str(&format!(
            "<div class=\"file-head\"><div><h2>{}</h2><p>{}</p></div><a class=\"primary slim\" href=\"{url}?download=1\">Download</a></div>",
            escape(&entry.name),
            size(entry.size),
        ));
        if mime.starts_with("image/") && mime != "image/svg+xml" {
            body.push_str(&format!("<div class=\"stage\"><img src=\"{url}\" alt=\"{}\"></div>", escape(&entry.name)));
        } else if matches!(mime.as_str(), "text/plain" | "text/markdown" | "text/csv" | "application/json") {
            use tokio::io::AsyncReadExt;
            let path = drive.store.path_of(opened.owner, entry.id).await?;
            let mut buf = Vec::new();
            tokio::fs::File::open(&path).await?.take(TEXT_PREVIEW).read_to_end(&mut buf).await?;
            body.push_str(&format!("<pre class=\"text\">{}</pre>", escape(&String::from_utf8_lossy(&buf))));
            if entry.size as u64 > TEXT_PREVIEW {
                body.push_str("<p class=\"more\">This is the beginning of the file. Download it to read the rest.</p>");
            }
        } else if mime == "application/pdf" {
            body.push_str(&format!("<p class=\"more\"><a href=\"{url}\">Open the PDF</a></p>"));
        }
    }
    let expiry = match opened.share.expires_at {
        Some(t) => format!("<p class=\"expiry\" data-expires=\"{t}\">This link stops working in {}.</p>", remaining(t - home_core::now())),
        None => String::new(),
    };
    body.push_str(&expiry);
    Ok(finish(assets::page_with_html(share_page(), StatusCode::OK, &[("body", &body)], &[("title", &entry.name)])))
}

fn remaining(seconds: i64) -> String {
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    match seconds {
        s if s >= 2 * 86_400 => plural(s / 86_400, "day"),
        s if s >= 2 * 3600 => plural(s / 3600, "hour"),
        s => plural((s / 60).max(1), "minute"),
    }
}

pub async fn root(State(drive): State<Drive>, Path(token): Path<String>) -> Response {
    match drive.store.share_open(&token).await {
        Ok(opened) => render(&drive, &opened, None).await.unwrap_or_else(failure),
        Err(e) => failure(e),
    }
}

pub async fn item(State(drive): State<Drive>, Path((token, id)): Path<(String, i64)>) -> Response {
    match drive.store.share_open(&token).await {
        Ok(opened) => render(&drive, &opened, Some(id)).await.unwrap_or_else(failure),
        Err(e) => failure(e),
    }
}

#[derive(Deserialize)]
pub struct ContentQuery {
    #[serde(default)]
    download: Option<String>,
}

pub async fn content(State(drive): State<Drive>, Path((token, id)): Path<(String, i64)>, Query(q): Query<ContentQuery>, request: Request) -> Response {
    let served = async {
        let opened = drive.store.share_open(&token).await?;
        let (entry, _) = drive.store.share_entry(&opened, Some(id)).await?;
        if entry.is_dir {
            return Err(Error::NotFound);
        }
        let path = drive.store.path_of(opened.owner, entry.id).await?;
        let mime = entry.mime.clone().unwrap_or_else(|| "application/octet-stream".into());
        Ok::<_, Error>(blobs::serve(&path, &entry.name, &mime, q.download.is_some(), request).await)
    };
    match served.await {
        Ok(mut response) => {
            response.headers_mut().insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
            response
        }
        Err(e) => failure(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_durations() {
        assert_eq!(size(18), "18 B");
        assert_eq!(size(78_585), "77 KB");
        assert_eq!(size(5 * 1024 * 1024 + 300_000), "5.3 MB");
        assert_eq!(remaining(30), "1 minute");
        assert_eq!(remaining(3 * 3600), "3 hours");
        assert_eq!(remaining(7 * 86_400), "7 days");
    }
}
