//! Routes every install has: first-run setup, sign in and out, who am I, and the admin's
//! account list. Modules mount their own routers beside these.

use crate::session::{Admin, Signed};
use crate::{Core, Error, Result, accounts, asset, assets, names, session};
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

pub const ASSETS: &[assets::Asset] = &[
    asset!("setup.html", "home-core/ui/setup.html"),
    asset!("login.html", "home-core/ui/login.html"),
    asset!("home.css", "home-core/ui/home.css"),
    asset!("auth.js", "home-core/ui/auth.js"),
    asset!("mark.svg", "home-core/ui/mark.svg"),
];

pub fn router() -> Router<Core> {
    Router::new()
        .route("/", get(root))
        .route("/setup", get(setup_page))
        .route("/login", get(login_page))
        .route("/api/setup", post(setup_submit))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/{id}/disabled", post(set_disabled))
        .route("/api/users/{id}/password", post(set_password))
        .route("/home/{*path}", get(static_asset))
}

async fn root(State(core): State<Core>) -> Redirect {
    if core.setup.pending() { Redirect::to("/setup") } else { Redirect::to("/drive/") }
}

async fn static_asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    assets::serve(ASSETS, &path)
}

#[derive(Deserialize)]
struct SetupQuery {
    #[serde(default)]
    token: String,
}

async fn setup_page(State(core): State<Core>, Query(q): Query<SetupQuery>) -> Response {
    if !core.setup.pending() {
        return (StatusCode::GONE, "Setup is done. Sign in instead.").into_response();
    }
    if !core.setup.matches(&q.token) {
        return (StatusCode::FORBIDDEN, "That setup link is not the one this install printed.").into_response();
    }
    assets::page(&ASSETS[0], &[("token", &q.token)])
}

#[derive(Deserialize)]
struct SetupBody {
    token: String,
    name: String,
    password: String,
}

async fn setup_submit(State(core): State<Core>, Json(body): Json<SetupBody>) -> Result<Response> {
    if !core.setup.pending() {
        return Err(Error::Gone);
    }
    names::user_name(&body.name)?;
    names::password(&body.password)?;
    if !core.setup.claim(&body.token) {
        return Err(Error::Forbidden);
    }
    let user = accounts::create(&core.db, &body.name, &body.password, true).await?;
    core.setup.finish(&core.config.data_dir);
    tracing::info!(name = %user.name, "admin created");
    signed_in(&core, user).await
}

async fn login_page() -> Response {
    assets::respond(&ASSETS[1])
}

#[derive(Deserialize)]
struct LoginBody {
    name: String,
    password: String,
}

async fn login(State(core): State<Core>, Json(body): Json<LoginBody>) -> Result<Response> {
    match accounts::authenticate(&core.db, &body.name, &body.password).await? {
        Some(user) => signed_in(&core, user).await,
        None => Err(Error::BadRequest("wrong name or password".into())),
    }
}

async fn signed_in(core: &Core, user: accounts::User) -> Result<Response> {
    let token = session::create(&core.db, user.id, core.config.limits.session_days).await?;
    let mut response = Json(user).into_response();
    response.headers_mut().insert(header::SET_COOKIE, session::set_cookie(&token, core.config.limits.session_days, false));
    Ok(response)
}

async fn logout(State(core): State<Core>, headers: header::HeaderMap) -> Result<Response> {
    if let Some(token) = session::cookie_value(&headers, session::COOKIE) {
        session::delete(&core.db, token).await?;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(header::SET_COOKIE, session::clear_cookie());
    Ok(response)
}

async fn me(Signed(user): Signed) -> Json<accounts::User> {
    Json(user)
}

async fn list_users(Admin(_): Admin, State(core): State<Core>) -> Result<Json<Vec<accounts::User>>> {
    Ok(Json(accounts::list(&core.db).await?))
}

#[derive(Deserialize)]
struct NewUser {
    name: String,
    password: String,
    #[serde(default)]
    is_admin: bool,
}

async fn create_user(Admin(_): Admin, State(core): State<Core>, Json(body): Json<NewUser>) -> Result<(StatusCode, Json<accounts::User>)> {
    let user = accounts::create(&core.db, &body.name, &body.password, body.is_admin).await?;
    Ok((StatusCode::CREATED, Json(user)))
}

#[derive(Deserialize)]
struct Disabled {
    disabled: bool,
}

async fn set_disabled(
    Admin(admin): Admin,
    State(core): State<Core>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(body): Json<Disabled>,
) -> Result<StatusCode> {
    if id == admin.id {
        return Err(Error::BadRequest("you cannot disable your own account".into()));
    }
    accounts::set_disabled(&core.db, id, body.disabled).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct NewPassword {
    password: String,
}

async fn set_password(
    Admin(_): Admin,
    State(core): State<Core>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(body): Json<NewPassword>,
) -> Result<StatusCode> {
    accounts::set_password(&core.db, id, &body.password).await?;
    Ok(StatusCode::NO_CONTENT)
}
