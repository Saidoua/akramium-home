//! The SQLite side of the store: rows in `files`. A trashed item keeps its row, its
//! children and its original parent; only `trashed_at` marks it.

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trashed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orig_parent_id: Option<i64>,
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

const COLUMNS: &str = "id, parent_id, name, is_dir, size, mtime, hash, mime, trashed_at, orig_parent_id";

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
        trashed_at: r.get(8)?,
        orig_parent_id: r.get(9)?,
    })
}

fn conflict(name: &str) -> Error {
    Error::Conflict(format!("there is already something called {name} here"))
}

fn map_insert(r: rusqlite::Result<usize>, name: &str) -> Result<()> {
    match r {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(err, _)) if err.code == rusqlite::ErrorCode::ConstraintViolation => Err(conflict(name)),
        Err(err) => Err(err.into()),
    }
}

pub async fn get(db: &Db, user_id: i64, id: i64) -> Result<Option<Entry>> {
    db.call(move |c| {
        Ok(c.query_row(&format!("SELECT {COLUMNS} FROM files WHERE user_id = ?1 AND id = ?2"), params![user_id, id], row).optional()?)
    })
    .await
}

/// A live (not trashed) child by name.
pub async fn find_child(db: &Db, user_id: i64, parent: Option<i64>, name: &str) -> Result<Option<Entry>> {
    let name = name.to_string();
    db.call(move |c| {
        Ok(c.query_row(
            &format!("SELECT {COLUMNS} FROM files WHERE user_id = ?1 AND IFNULL(parent_id, 0) = IFNULL(?2, 0) AND name = ?3 AND trashed_at IS NULL"),
            params![user_id, parent, name],
            row,
        )
        .optional()?)
    })
    .await
}

/// Live children of a folder: folders first, then files, both by name.
pub async fn children(db: &Db, user_id: i64, parent: Option<i64>) -> Result<Vec<Entry>> {
    db.call(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {COLUMNS} FROM files WHERE user_id = ?1 AND IFNULL(parent_id, 0) = IFNULL(?2, 0) AND trashed_at IS NULL
             ORDER BY is_dir DESC, name COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map(params![user_id, parent], row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await
}

/// Everything in the trash, newest first.
pub async fn trashed(db: &Db, user_id: i64) -> Result<Vec<Entry>> {
    db.call(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {COLUMNS} FROM files WHERE user_id = ?1 AND trashed_at IS NOT NULL ORDER BY trashed_at DESC, name COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map([user_id], row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await
}

/// Trashed items older than `before`, across all users: `(user_id, entry)`.
pub async fn trashed_before(db: &Db, before: i64) -> Result<Vec<(i64, Entry)>> {
    db.call(move |c| {
        let mut stmt = c.prepare(&format!("SELECT user_id, {COLUMNS} FROM files WHERE trashed_at IS NOT NULL AND trashed_at < ?1"))?;
        let rows = stmt.query_map([before], |r| {
            let user_id: i64 = r.get(0)?;
            let inner = Entry {
                id: r.get(1)?,
                parent_id: r.get(2)?,
                name: r.get(3)?,
                is_dir: r.get::<_, i64>(4)? != 0,
                size: r.get(5)?,
                mtime: r.get(6)?,
                hash: r.get(7)?,
                mime: r.get(8)?,
                trashed_at: r.get(9)?,
                orig_parent_id: r.get(10)?,
            };
            Ok((user_id, inner))
        })?;
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
        map_insert(r, &e.name)?;
        let id = c.last_insert_rowid();
        Ok(c.query_row(&format!("SELECT {COLUMNS} FROM files WHERE id = ?1"), [id], row)?)
    })
    .await
}

/// A file's type follows its name, so a rename to `.pdf` makes it a PDF.
fn mime_for(name: &str) -> String {
    mime_guess::from_path(name).first_or_octet_stream().to_string()
}

pub async fn rename(db: &Db, user_id: i64, id: i64, name: String) -> Result<()> {
    db.call(move |c| {
        let r = c.execute(
            "UPDATE files SET name = ?1, mtime = ?2, mime = CASE WHEN is_dir = 1 THEN NULL ELSE ?3 END WHERE user_id = ?4 AND id = ?5",
            params![name, now(), mime_for(&name), user_id, id],
        );
        map_insert(r, &name)
    })
    .await
}

pub async fn set_parent(db: &Db, user_id: i64, id: i64, parent: Option<i64>) -> Result<()> {
    db.call(move |c| {
        let name: String = c.query_row("SELECT name FROM files WHERE user_id = ?1 AND id = ?2", params![user_id, id], |r| r.get(0))?;
        let r = c.execute("UPDATE files SET parent_id = ?1, mtime = ?2 WHERE user_id = ?3 AND id = ?4", params![parent, now(), user_id, id]);
        map_insert(r, &name)
    })
    .await
}

/// New place and name in one step (WebDAV MOVE).
pub async fn set_parent_and_name(db: &Db, user_id: i64, id: i64, parent: Option<i64>, name: String) -> Result<()> {
    db.call(move |c| {
        let r = c.execute(
            "UPDATE files SET parent_id = ?1, name = ?2, mtime = ?3, mime = CASE WHEN is_dir = 1 THEN NULL ELSE ?4 END WHERE user_id = ?5 AND id = ?6",
            params![parent, name, now(), mime_for(&name), user_id, id],
        );
        map_insert(r, &name)
    })
    .await
}

/// New bytes behind an existing file.
pub async fn set_content(db: &Db, user_id: i64, id: i64, size: i64, hash: String, mime: String) -> Result<()> {
    db.call(move |c| {
        c.execute(
            "UPDATE files SET size = ?1, hash = ?2, mime = ?3, mtime = ?4 WHERE user_id = ?5 AND id = ?6",
            params![size, hash, mime, now(), user_id, id],
        )?;
        Ok(())
    })
    .await
}

pub async fn set_trashed(db: &Db, user_id: i64, id: i64) -> Result<()> {
    db.call(move |c| {
        c.execute(
            "UPDATE files SET trashed_at = ?1, orig_parent_id = parent_id, parent_id = NULL WHERE user_id = ?2 AND id = ?3 AND trashed_at IS NULL",
            params![now(), user_id, id],
        )?;
        Ok(())
    })
    .await
}

pub async fn set_restored(db: &Db, user_id: i64, id: i64, parent: Option<i64>) -> Result<()> {
    db.call(move |c| {
        let name: String = c.query_row("SELECT name FROM files WHERE user_id = ?1 AND id = ?2", params![user_id, id], |r| r.get(0))?;
        let r = c.execute(
            "UPDATE files SET trashed_at = NULL, orig_parent_id = NULL, parent_id = ?1, mtime = ?2 WHERE user_id = ?3 AND id = ?4",
            params![parent, now(), user_id, id],
        );
        map_insert(r, &name)
    })
    .await
}

/// Deletes the row; children go with it (cascade).
pub async fn delete(db: &Db, user_id: i64, id: i64) -> Result<()> {
    db.call(move |c| {
        c.execute("DELETE FROM files WHERE user_id = ?1 AND id = ?2", params![user_id, id])?;
        Ok(())
    })
    .await
}
