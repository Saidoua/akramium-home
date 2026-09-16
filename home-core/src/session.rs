//! Signed-in sessions: a random token in an `HttpOnly` cookie, stored hashed.
//! `Signed` is the axum extractor handlers take when a request needs an account.

use crate::accounts::User;
use crate::{Core, Db, Error, Result, accounts, now};
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, header};
use base64::Engine;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

pub const COOKIE: &str = "home_session";

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    b64(&bytes)
}

pub fn token_hash(token: &str) -> String {
    b64(&Sha256::digest(token.as_bytes()))
}

/// Creates a session and returns the raw token for the cookie.
pub async fn create(db: &Db, user_id: i64, days: i64) -> Result<String> {
    let token = random_token();
    let hash = token_hash(&token);
    db.call(move |c| {
        let t = now();
        c.execute(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![hash, user_id, t, t + days * 86_400],
        )?;
        Ok(())
    })
    .await?;
    Ok(token)
}

pub async fn lookup(db: &Db, token: &str) -> Result<Option<User>> {
    let hash = token_hash(token);
    let user_id = db
        .call(move |c| {
            Ok(c.query_row(
                "SELECT user_id FROM sessions WHERE token_hash = ?1 AND expires_at > ?2",
                params![hash, now()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
        })
        .await?;
    match user_id {
        Some(id) => Ok(accounts::by_id(db, id).await?.filter(|u| !u.disabled)),
        None => Ok(None),
    }
}

pub async fn delete(db: &Db, token: &str) -> Result<()> {
    let hash = token_hash(token);
    db.call(move |c| {
        c.execute("DELETE FROM sessions WHERE token_hash = ?1", [hash])?;
        Ok(())
    })
    .await
}

/// The value of one cookie in a `Cookie` header, if present.
pub fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get_all(header::COOKIE).iter().find_map(|h| {
        h.to_str().ok()?.split(';').map(str::trim).find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == name).then_some(v)
        })
    })
}

pub fn set_cookie(token: &str, days: i64, secure: bool) -> HeaderValue {
    let mut v = format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}", days * 86_400);
    if secure {
        v.push_str("; Secure");
    }
    HeaderValue::from_str(&v).expect("cookie is ascii")
}

pub fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static("home_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// The signed-in user. Rejects with 401 when the cookie is missing, unknown or expired.
#[derive(Debug, Clone)]
pub struct Signed(pub User);

/// Like `Signed`, and the account is an admin (403 otherwise).
#[derive(Debug, Clone)]
pub struct Admin(pub User);

impl<S> FromRequestParts<S> for Signed
where
    S: Send + Sync,
    Core: axum::extract::FromRef<S>,
{
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        let core = Core::from_ref(state);
        let token = cookie_value(&parts.headers, COOKIE).ok_or(Error::Unauthorized)?.to_string();
        match lookup(&core.db, &token).await? {
            Some(user) => Ok(Signed(user)),
            None => Err(Error::Unauthorized),
        }
    }
}

impl<S> FromRequestParts<S> for Admin
where
    S: Send + Sync,
    Core: axum::extract::FromRef<S>,
{
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        let Signed(user) = Signed::from_request_parts(parts, state).await?;
        if user.is_admin { Ok(Admin(user)) } else { Err(Error::Forbidden) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cookies() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; home_session=abc; b=2"));
        assert_eq!(cookie_value(&h, COOKIE), Some("abc"));
        assert_eq!(cookie_value(&h, "b"), Some("2"));
        assert_eq!(cookie_value(&h, "zz"), None);
    }

    #[tokio::test]
    async fn create_lookup_delete() {
        let db = Db::open_memory().await.unwrap();
        db.migrate("home-core", crate::MIGRATIONS).await.unwrap();
        let u = accounts::create(&db, "alice", "password1", true).await.unwrap();
        let token = create(&db, u.id, 30).await.unwrap();
        assert_eq!(lookup(&db, &token).await.unwrap().unwrap().name, "alice");
        assert!(lookup(&db, "nope").await.unwrap().is_none());
        delete(&db, &token).await.unwrap();
        assert!(lookup(&db, &token).await.unwrap().is_none());
    }
}
