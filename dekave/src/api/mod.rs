//! HTTP surface of the drive: pages under `/drive`, JSON under `/api/drive`.

use crate::Drive;
use crate::store::blobs;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use home_core::session::{self, Signed};
use home_core::{Result, asset, assets};
use serde::Deserialize;

pub const ASSETS: &[assets::Asset] = &[
    asset!("index.html", "dekave/ui/index.html"),
    asset!("app.css", "dekave/ui/app.css"),
    asset!("app.js", "dekave/ui/app.js"),
    asset!("icons.svg", "dekave/ui/icons.svg"),
];

pub fn router() -> Router<Drive> {
    Router::new()
        .route("/drive", get(|| async { Redirect::permanent("/drive/") }))
        .route("/drive/", get(page))
        .route("/drive/folder/{id}", get(page))
        .route("/drive/{*path}", get(static_asset))
        .route("/api/drive/files", get(list).put(upload))
        .route("/api/drive/folders", post(create_folder))
        .route("/api/drive/files/{id}/content", get(content))
}

/// The app shell; a signed-out visitor goes to the sign-in page and comes back here.
async fn page(State(drive): State<Drive>, uri: Uri, headers: axum::http::HeaderMap) -> Response {
    let signed = match session::cookie_value(&headers, session::COOKIE) {
        Some(token) => session::lookup(&drive.core.db, token).await.ok().flatten().is_some(),
        None => false,
    };
    if !signed {
        let next = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/drive/");
        return Redirect::to(&format!("/login?next={}", percent(next))).into_response();
    }
    assets::respond(&ASSETS[0])
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

async fn static_asset(Path(path): Path<String>) -> Response {
    assets::serve(ASSETS, &path)
}

#[derive(Deserialize)]
struct FolderQuery {
    folder: Option<i64>,
}

async fn list(Signed(user): Signed, State(drive): State<Drive>, Query(q): Query<FolderQuery>) -> Result<Json<crate::store::Listing>> {
    Ok(Json(drive.store.list(user.id, q.folder).await?))
}

#[derive(Deserialize)]
struct NewFolder {
    parent: Option<i64>,
    name: String,
}

async fn create_folder(Signed(user): Signed, State(drive): State<Drive>, Json(body): Json<NewFolder>) -> Result<(StatusCode, Json<crate::store::Entry>)> {
    let e = drive.store.create_folder(user.id, body.parent, &body.name).await?;
    Ok((StatusCode::CREATED, Json(e)))
}

#[derive(Deserialize)]
struct UploadQuery {
    parent: Option<i64>,
    name: String,
}

/// Whole-file upload: `PUT /api/drive/files?name=…&parent=…` with the bytes as the body.
async fn upload(Signed(user): Signed, State(drive): State<Drive>, Query(q): Query<UploadQuery>, body: Body) -> Result<(StatusCode, Json<crate::store::Entry>)> {
    let stream = body.into_data_stream();
    let e = drive.store.create_file(user.id, q.parent, &q.name, stream).await?;
    Ok((StatusCode::CREATED, Json(e)))
}

#[derive(Deserialize)]
struct ContentQuery {
    #[serde(default)]
    download: bool,
}

async fn content(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>, Query(q): Query<ContentQuery>, request: Request) -> Result<Response> {
    let entry = drive.store.entry(user.id, id).await?;
    if entry.is_dir {
        return Err(home_core::Error::BadRequest("that is a folder".into()));
    }
    let path = drive.store.path_of(user.id, id).await?;
    let mime = entry.mime.clone().unwrap_or_else(|| "application/octet-stream".into());
    Ok(blobs::serve(&path, &entry.name, &mime, q.download, request).await)
}
