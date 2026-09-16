//! Household accounts. The admin creates them; nobody registers.

use crate::{Db, Error, Result, names, now};
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use std::sync::LazyLock;
use argon2::Argon2;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub created_at: i64,
}

fn row_to_user(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        name: r.get(1)?,
        is_admin: r.get::<_, i64>(2)? != 0,
        disabled: r.get::<_, i64>(3)? != 0,
        created_at: r.get(4)?,
    })
}

const USER_COLUMNS: &str = "id, name, is_admin, disabled, created_at";

/// Argon2id with the crate's defaults (19 MiB, two passes), in a PHC string.
pub fn hash_password(password: &str) -> Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| Error::Internal(e.to_string()))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    Argon2::default().verify_password(password.as_bytes(), hash).is_ok()
}

pub async fn count(db: &Db) -> Result<i64> {
    db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)).await
}

pub async fn create(db: &Db, name: &str, password: &str, is_admin: bool) -> Result<User> {
    let name = names::user_name(name)?.to_string();
    names::password(password)?;
    let password = password.to_string();
    db.call(move |c| {
        let hash = hash_password(&password)?;
        let inserted = c.execute(
            "INSERT INTO users (name, password_hash, is_admin, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, hash, is_admin as i64, now()],
        );
        match inserted {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                return Err(Error::Conflict(format!("there is already an account called {name}")));
            }
            Err(e) => return Err(e.into()),
        }
        let id = c.last_insert_rowid();
        Ok(c.query_row(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"), [id], row_to_user)?)
    })
    .await
}

pub async fn by_id(db: &Db, id: i64) -> Result<Option<User>> {
    db.call(move |c| {
        Ok(c.query_row(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"), [id], row_to_user).optional()?)
    })
    .await
}

pub async fn list(db: &Db) -> Result<Vec<User>> {
    db.call(|c| {
        let mut stmt = c.prepare(&format!("SELECT {USER_COLUMNS} FROM users ORDER BY name"))?;
        let rows = stmt.query_map([], row_to_user)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await
}

/// The user when the name and password match and the account is enabled.
/// Hashing runs on the blocking pool; a wrong name costs a hash too, so timing says nothing.
pub async fn authenticate(db: &Db, name: &str, password: &str) -> Result<Option<User>> {
    let name = name.to_string();
    let password = password.to_string();
    db.call(move |c| {
        let found = c
            .query_row(
                &format!("SELECT {USER_COLUMNS}, password_hash FROM users WHERE name = ?1"),
                [&name],
                |r| Ok((row_to_user(r)?, r.get::<_, String>(5)?)),
            )
            .optional()?;
        match found {
            Some((user, hash)) if !user.disabled && verify_password(&password, &hash) => Ok(Some(user)),
            Some(_) => Ok(None),
            None => {
                // Burn the same time as a real check.
                let _ = verify_password(&password, &DUMMY_HASH);
                Ok(None)
            }
        }
    })
    .await
}

pub async fn set_password(db: &Db, id: i64, password: &str) -> Result<()> {
    names::password(password)?;
    let password = password.to_string();
    db.call(move |c| {
        let hash = hash_password(&password)?;
        let n = c.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![hash, id])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    })
    .await
}

pub async fn set_disabled(db: &Db, id: i64, disabled: bool) -> Result<()> {
    db.call(move |c| {
        let n = c.execute("UPDATE users SET disabled = ?1 WHERE id = ?2", params![disabled as i64, id])?;
        if disabled {
            c.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
        }
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    })
    .await
}

// A real hash of an unknown password, verified when the name does not exist so a wrong name
// takes as long as a wrong password.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| hash_password("no such account").unwrap_or_default());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trip() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not a hash"));
    }

    #[tokio::test]
    async fn create_and_authenticate() {
        let db = Db::open_memory().await.unwrap();
        db.migrate("home-core", crate::MIGRATIONS).await.unwrap();
        assert_eq!(count(&db).await.unwrap(), 0);
        let u = create(&db, "alice", "password1", true).await.unwrap();
        assert!(u.is_admin);
        assert!(matches!(create(&db, "Alice", "password2", false).await, Err(Error::Conflict(_))));
        assert!(matches!(create(&db, "bob", "short", false).await, Err(Error::BadRequest(_))));
        assert!(authenticate(&db, "alice", "password1").await.unwrap().is_some());
        assert!(authenticate(&db, "alice", "password2").await.unwrap().is_none());
        assert!(authenticate(&db, "nobody", "password1").await.unwrap().is_none());
        set_disabled(&db, u.id, true).await.unwrap();
        assert!(authenticate(&db, "alice", "password1").await.unwrap().is_none());
    }
}
