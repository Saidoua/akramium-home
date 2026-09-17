//! `/dav/`: the drive for Finder, Windows Explorer, phones and sync tools.

pub mod auth;
pub mod fs;

use crate::Drive;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use dav_server::davpath::DavPath;
use dav_server::{DavHandler, fakels::FakeLs};

pub const PREFIX: &str = "/dav";

pub fn handler(store: crate::Store) -> DavHandler<i64> {
    DavHandler::builder()
        .filesystem(Box::new(fs::DavFs { store }))
        .locksystem(FakeLs::new())
        .strip_prefix(PREFIX)
        .build_handler()
}

fn challenge() -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, "Sign in with your Akramium Home name and password.").into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"DeKave\", charset=\"UTF-8\""));
    r
}

pub async fn serve(State(drive): State<Drive>, request: Request) -> Response {
    let address = home_core::ClientIp::of(request.extensions()).0;
    let user = match drive.dav_auth.check(&drive.core, request.headers().get(header::AUTHORIZATION), address).await {
        Ok(Some(user)) => user,
        Ok(None) => return challenge(),
        Err(e) => return e.into_response(),
    };

    // One item to the trash per request. The library would walk a folder and discard its
    // files one by one, which would scatter them across the trash.
    if request.method() == Method::DELETE {
        // Only the address is needed; a borrowed request body could not cross an await.
        let uri = request.uri().clone();
        return to_trash(&drive, user.id, &uri).await;
    }

    let response = drive.dav.handle_guarded(request, user.name.clone(), user.id).await;
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, Body::new(body))
}

async fn to_trash(drive: &Drive, user_id: i64, uri: &axum::http::Uri) -> Response {
    let names = DavPath::from_uri(uri).ok().and_then(|mut p| {
        p.set_prefix(PREFIX).ok()?;
        fs::parts(&p).ok()
    });
    let Some(names) = names else { return StatusCode::BAD_REQUEST.into_response() };
    let entry = match drive.store.resolve(user_id, &names).await {
        Ok(Some(e)) => e,
        Ok(None) => return StatusCode::FORBIDDEN.into_response(),
        Err(home_core::Error::NotFound) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return e.into_response(),
    };
    if let Err(e) = drive.store.trash(user_id, entry.id).await {
        return e.into_response();
    }
    if fs::is_junk(&entry.name) {
        let _ = drive.store.purge(user_id, entry.id).await;
    }
    StatusCode::NO_CONTENT.into_response()
}
