//! HTTP surface of the drive: pages under `/drive`, JSON under `/api/drive`.

pub mod public;

use crate::Drive;
use crate::store::blobs;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use home_core::session::{self, Signed};
use home_core::{Result, asset, assets};
use serde::Deserialize;

pub const ASSETS: &[assets::Asset] = &[
    asset!("index.html", "dekave/ui/index.html"),
    asset!("app.css", "dekave/ui/app.css"),
    asset!("app.js", "dekave/ui/app.js"),
    asset!("lib.js", "dekave/ui/lib.js"),
    asset!("uploads.js", "dekave/ui/uploads.js"),
    asset!("viewer.js", "dekave/ui/viewer.js"),
    asset!("share.js", "dekave/ui/share.js"),
    asset!("icons.svg", "dekave/ui/icons.svg"),
    asset!("share.html", "dekave/ui/share.html"),
    asset!("share.css", "dekave/ui/share.css"),
];

pub fn router() -> Router<Drive> {
    Router::new()
        .route("/drive", get(|| async { Redirect::permanent("/drive/") }))
        .route("/dav", axum::routing::any(crate::dav::serve))
        .route("/dav/", axum::routing::any(crate::dav::serve))
        .route("/dav/{*path}", axum::routing::any(crate::dav::serve))
        .route("/drive/", get(page))
        .route("/drive/folder/{id}", get(page))
        .route("/drive/trash", get(page))
        .route("/drive/shared", get(page))
        .route("/s/{token}", get(public::root))
        .route("/s/{token}/i/{id}", get(public::item))
        .route("/s/{token}/content/{id}", get(public::content))
        .route("/api/drive/shares", get(list_shares))
        .route("/api/drive/shares/{id}", delete(revoke_share))
        .route("/api/drive/files/{id}/shares", get(list_file_shares).post(create_share))
        .route("/drive/{*path}", get(static_asset))
        .route("/api/drive/files", get(list).put(upload))
        .route("/api/drive/folders", post(create_folder))
        .route("/api/drive/files/{id}", delete(trash))
        .route("/api/drive/files/{id}/content", get(content))
        .route("/api/drive/files/{id}/thumb", get(thumb))
        .route("/api/drive/uploads", post(upload_begin))
        .route("/api/drive/uploads/{id}", get(upload_status).delete(upload_cancel))
        .route("/api/drive/uploads/{id}/finish", post(upload_finish))
        .route("/api/drive/uploads/{id}/{offset}", axum::routing::put(upload_append))
        .route("/api/drive/files/{id}/rename", post(rename))
        .route("/api/drive/files/{id}/move", post(move_to))
        .route("/api/drive/trash", get(list_trash).delete(empty_trash))
        .route("/api/drive/trash/{id}", delete(purge))
        .route("/api/drive/trash/{id}/restore", post(restore))
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
    Ok(Json(drive.store.list_visible(user.id, q.folder).await?))
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

#[derive(Deserialize)]
struct Rename {
    name: String,
}

async fn rename(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>, Json(body): Json<Rename>) -> Result<Json<crate::store::Entry>> {
    Ok(Json(drive.store.rename(user.id, id, body.name.trim()).await?))
}

#[derive(Deserialize)]
struct Move {
    parent: Option<i64>,
}

async fn move_to(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>, Json(body): Json<Move>) -> Result<Json<crate::store::Entry>> {
    Ok(Json(drive.store.move_to(user.id, id, body.parent).await?))
}

async fn trash(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>) -> Result<StatusCode> {
    drive.store.trash(user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_trash(Signed(user): Signed, State(drive): State<Drive>) -> Result<Json<Vec<crate::store::Entry>>> {
    Ok(Json(drive.store.list_trash(user.id).await?))
}

async fn empty_trash(Signed(user): Signed, State(drive): State<Drive>) -> Result<Json<serde_json::Value>> {
    let n = drive.store.empty_trash(user.id).await?;
    Ok(Json(serde_json::json!({ "purged": n })))
}

async fn purge(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>) -> Result<StatusCode> {
    drive.store.purge(user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restore(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>) -> Result<Json<crate::store::Entry>> {
    Ok(Json(drive.store.restore(user.id, id).await?))
}

async fn thumb(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>, request: Request) -> Result<Response> {
    use axum::http::{HeaderValue, header};
    use tower::ServiceExt;
    let path = drive.store.thumbnail(user.id, id).await?;
    let service = tower_http::services::ServeFile::new_with_mime(&path, &mime_guess::mime::IMAGE_JPEG);
    let mut response = match service.oneshot(request).await {
        Ok(r) => r.into_response(),
        Err(never) => match never {},
    };
    // The URL carries the content hash, so the bytes behind it never change.
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    Ok(response)
}

#[derive(Deserialize)]
struct BeginUpload {
    parent: Option<i64>,
    name: String,
    size: i64,
}

async fn upload_begin(Signed(user): Signed, State(drive): State<Drive>, Json(body): Json<BeginUpload>) -> Result<(StatusCode, Json<crate::store::uploads::Upload>)> {
    let up = drive.store.upload_begin(user.id, body.parent, &body.name, body.size).await?;
    Ok((StatusCode::CREATED, Json(up)))
}

async fn upload_status(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<String>) -> Result<Json<crate::store::uploads::Upload>> {
    Ok(Json(drive.store.upload_status(user.id, &id).await?))
}

async fn upload_append(Signed(user): Signed, State(drive): State<Drive>, Path((id, offset)): Path<(String, i64)>, body: Body) -> Result<Json<crate::store::uploads::Upload>> {
    Ok(Json(drive.store.upload_append(user.id, &id, offset, body.into_data_stream()).await?))
}

async fn upload_finish(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<String>) -> Result<(StatusCode, Json<crate::store::Entry>)> {
    Ok((StatusCode::CREATED, Json(drive.store.upload_finish(user.id, &id).await?)))
}

async fn upload_cancel(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<String>) -> Result<StatusCode> {
    drive.store.upload_cancel(user.id, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct NewShare {
    /// Seconds until the link stops working; absent or null for a link that stays.
    expires_in: Option<i64>,
}

async fn create_share(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>, Json(body): Json<NewShare>) -> Result<(StatusCode, Json<crate::store::shares::Share>)> {
    Ok((StatusCode::CREATED, Json(drive.store.share_create(user.id, id, body.expires_in).await?)))
}

async fn list_file_shares(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>) -> Result<Json<Vec<crate::store::shares::Share>>> {
    drive.store.entry(user.id, id).await?;
    Ok(Json(drive.store.share_list(user.id, Some(id)).await?))
}

async fn list_shares(Signed(user): Signed, State(drive): State<Drive>) -> Result<Json<Vec<crate::store::shares::Share>>> {
    Ok(Json(drive.store.share_list(user.id, None).await?))
}

async fn revoke_share(Signed(user): Signed, State(drive): State<Drive>, Path(id): Path<i64>) -> Result<StatusCode> {
    drive.store.share_revoke(user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
