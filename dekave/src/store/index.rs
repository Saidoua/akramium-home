//! The SQLite side of the store: rows in `files`.

use home_core::{Db, Error, Result, now};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub is_dir: bool,
    pub size: i64,
    pub mtime: i64,
    pub hash: Option<String>,
    pub mime: Option<String>,
}

pub struct NewEntry {
    pub user_id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub is_dir: bool,
    pub size: i64,
    pub hash: Option<String>,
    pub mime: Option<String>,
}

const COLUMNS: &str = "id, parent_id, name, is_dir, size, mtime, hash, mime";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        name: r.get(2)?,
        is_dir: r.get::<_, i64>(3)? != 0,
        size: r.get(4)?,
        mtime: r.get(5)?,
        hash: r.get(6)?,
        mime: r.get(7)?,
    })
}

pub async fn get(db: &Db, user_id: i64, id: i64) -> Result<Option<Entry>> {
    db.call(move |c| {
        Ok(c.query_row(&format!("SELECT {COLUMNS} FROM files WHERE user_id = ?1 AND id = ?2"), params![user_id, id], row).optional()?)
    })
    .await
}

pub async fn find_child(db: &Db, user_id: i64, parent: Option<i64>, name: &str) -> Result<Option<Entry>> {
    let name = name.to_string();
    db.call(move |c| {
        Ok(c.query_row(
            &format!("SELECT {COLUMNS} FROM files WHERE user_id = ?1 AND IFNULL(parent_id, 0) = IFNULL(?2, 0) AND name = ?3"),
            params![user_id, parent, name],
            row,
        )
        .optional()?)
    })
    .await
}

/// Folders first, then files, both by name.
pub async fn children(db: &Db, user_id: i64, parent: Option<i64>) -> Result<Vec<Entry>> {
    db.call(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {COLUMNS} FROM files WHERE user_id = ?1 AND IFNULL(parent_id, 0) = IFNULL(?2, 0) ORDER BY is_dir DESC, name COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map(params![user_id, parent], row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await
}

/// The entry and all its ancestors, root first. `NotFound` if the id is not the user's.
pub async fn chain(db: &Db, user_id: i64, id: i64) -> Result<Vec<Entry>> {
    db.call(move |c| {
        let mut out = Vec::new();
        let mut cursor = Some(id);
        while let Some(cur) = cursor {
            let e = c
                .query_row(&format!("SELECT {COLUMNS} FROM files WHERE user_id = ?1 AND id = ?2"), params![user_id, cur], row)
                .optional()?
                .ok_or(Error::NotFound)?;
            cursor = e.parent_id;
            out.push(e);
            if out.len() > 512 {
                return Err(Error::Internal("folder nesting too deep".into()));
            }
        }
        out.reverse();
        Ok(out)
    })
    .await
}

pub async fn insert(db: &Db, e: NewEntry) -> Result<Entry> {
    db.call(move |c| {
        let t = now();
        let r = c.execute(
            "INSERT INTO files (user_id, parent_id, name, is_dir, size, mtime, hash, mime, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![e.user_id, e.parent_id, e.name, e.is_dir as i64, e.size, t, e.hash, e.mime, t],
        );
        match r {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(err, _)) if err.code == rusqlite::ErrorCode::ConstraintViolation => {
                return Err(Error::Conflict(format!("there is already something called {} here", e.name)));
            }
            Err(err) => return Err(err.into()),
        }
        let id = c.last_insert_rowid();
        Ok(c.query_row(&format!("SELECT {COLUMNS} FROM files WHERE id = ?1"), [id], row)?)
    })
    .await
}
