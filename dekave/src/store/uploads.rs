//! Resumable uploads. The client opens an upload, sends the file in chunks at known offsets,
//! then finishes it. A dropped connection or a closed tab loses at most one chunk: the client
//! asks how much arrived and carries on from there.

use super::{Entry, Store, index};
use home_core::{Error, Result, names, now};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::PathBuf;

/// What the client is told to send per request.
pub const CHUNK: i64 = 8 << 20;
/// What one request may carry at most.
pub const MAX_CHUNK: i64 = 16 << 20;
/// Abandoned uploads are dropped after a week.
pub const KEEP_SECONDS: i64 = 7 * 86_400;

#[derive(Debug, Clone, Serialize)]
pub struct Upload {
    pub id: String,
    pub parent_id: Option<i64>,
    pub name: String,
    pub size: i64,
    pub received: i64,
    pub chunk: i64,
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Upload> {
    Ok(Upload { id: r.get(0)?, parent_id: r.get(1)?, name: r.get(2)?, size: r.get(3)?, received: r.get(4)?, chunk: CHUNK })
}

/// Releases the busy mark when a chunk write ends, however it ends.
struct Busy<'a>(&'a Store, String);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.0.busy.lock() {
            set.remove(&self.1);
        }
    }
}

impl Store {
    fn upload_path(&self, user_id: i64, id: &str) -> Result<PathBuf> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(Error::NotFound);
        }
        Ok(self.user_dir(user_id, "tmp")?.join(format!("up-{id}")))
    }

    pub async fn upload_begin(&self, user_id: i64, parent: Option<i64>, name: &str, size: i64) -> Result<Upload> {
        let name = names::file_name(name)?.to_string();
        if size < 0 {
            return Err(Error::BadRequest("the size is negative".into()));
        }
        self.folder_path(user_id, parent).await?;
        self.check_free(user_id, parent, &name).await?;
        let id = home_core::session::random_token();
        tokio::fs::File::create(self.upload_path(user_id, &id)?).await?;
        let upload = Upload { id: id.clone(), parent_id: parent, name: name.clone(), size, received: 0, chunk: CHUNK };
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT INTO uploads (id, user_id, parent_id, name, size, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, user_id, parent, name, size, now()],
                )?;
                Ok(())
            })
            .await?;
        Ok(upload)
    }

    pub async fn upload_status(&self, user_id: i64, id: &str) -> Result<Upload> {
        let id = id.to_string();
        self.db
            .call(move |c| {
                Ok(c.query_row("SELECT id, parent_id, name, size, received FROM uploads WHERE user_id = ?1 AND id = ?2", params![user_id, id], row)
                    .optional()?)
            })
            .await?
            .ok_or(Error::NotFound)
    }

    /// Appends one chunk at `offset`, which must be exactly what has arrived so far.
    pub async fn upload_append<S, E>(&self, user_id: i64, id: &str, offset: i64, mut body: S) -> Result<Upload>
    where
        S: futures_util::Stream<Item = std::result::Result<axum::body::Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let upload = self.upload_status(user_id, id).await?;
        if offset != upload.received {
            return Err(Error::Conflict(format!("expected offset {}, got {offset}", upload.received)));
        }
        {
            let mut set = self.busy.lock().map_err(|_| Error::Internal("upload lock poisoned".into()))?;
            if !set.insert(upload.id.clone()) {
                return Err(Error::Conflict("another request is writing this upload".into()));
            }
        }
        let _busy = Busy(self, upload.id.clone());

        let path = self.upload_path(user_id, id)?;
        let mut file = tokio::fs::OpenOptions::new().write(true).open(&path).await?;
        // Whatever a broken request left behind past `received` is discarded first.
        file.set_len(upload.received as u64).await?;
        let mut file = {
            use tokio::io::AsyncSeekExt;
            file.seek(std::io::SeekFrom::End(0)).await?;
            file
        };

        let mut written: i64 = 0;
        let mut failure: Option<Error> = None;
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => {
                    written += bytes.len() as i64;
                    if written > MAX_CHUNK {
                        failure = Some(Error::BadRequest(format!("a chunk carries at most {MAX_CHUNK} bytes")));
                        break;
                    }
                    if upload.received + written > upload.size {
                        failure = Some(Error::BadRequest("more bytes than the upload announced".into()));
                        break;
                    }
                    file.write_all(&bytes).await?;
                }
                Err(e) => {
                    failure = Some(Error::BadRequest(format!("chunk interrupted: {e}")));
                    break;
                }
            }
        }
        if let Some(e) = failure {
            let _ = file.set_len(upload.received as u64).await;
            return Err(e);
        }
        file.sync_all().await?;
        drop(file);

        let received = upload.received + written;
        let id_owned = upload.id.clone();
        self.db
            .call(move |c| {
                c.execute("UPDATE uploads SET received = ?1 WHERE user_id = ?2 AND id = ?3", params![received, user_id, id_owned])?;
                Ok(())
            })
            .await?;
        Ok(Upload { received, ..upload })
    }

    /// All bytes are in: hash, move into the folder, index.
    pub async fn upload_finish(&self, user_id: i64, id: &str) -> Result<Entry> {
        let upload = self.upload_status(user_id, id).await?;
        if upload.received != upload.size {
            return Err(Error::BadRequest(format!("{} of {} bytes arrived", upload.received, upload.size)));
        }
        let folder = self.folder_path(user_id, upload.parent_id).await?;
        self.check_free(user_id, upload.parent_id, &upload.name).await?;

        let tmp = self.upload_path(user_id, id)?;
        let hash_path = tmp.clone();
        let hash = tokio::task::spawn_blocking(move || -> Result<String> {
            let mut hasher = blake3::Hasher::new();
            let mut file = std::fs::File::open(&hash_path)?;
            std::io::copy(&mut file, &mut hasher)?;
            Ok(hasher.finalize().to_hex().to_string())
        })
        .await??;

        let final_path = folder.join(&upload.name);
        tokio::fs::rename(&tmp, &final_path).await?;
        let mime = mime_guess::from_path(&upload.name).first_or_octet_stream().to_string();
        let inserted = index::insert(&self.db, index::NewEntry {
            user_id,
            parent_id: upload.parent_id,
            name: upload.name.clone(),
            is_dir: false,
            size: upload.size,
            hash: Some(hash),
            mime: Some(mime),
        })
        .await;
        let entry = match inserted {
            Ok(e) => e,
            Err(err) => {
                let _ = tokio::fs::rename(&final_path, &tmp).await;
                return Err(err);
            }
        };
        self.upload_forget(user_id, id).await?;
        Ok(entry)
    }

    async fn upload_forget(&self, user_id: i64, id: &str) -> Result<()> {
        let id = id.to_string();
        self.db
            .call(move |c| {
                c.execute("DELETE FROM uploads WHERE user_id = ?1 AND id = ?2", params![user_id, id])?;
                Ok(())
            })
            .await
    }

    pub async fn upload_cancel(&self, user_id: i64, id: &str) -> Result<()> {
        self.upload_status(user_id, id).await?;
        let _ = tokio::fs::remove_file(self.upload_path(user_id, id)?).await;
        self.upload_forget(user_id, id).await
    }

    /// Drops uploads nobody touched for `older_than` seconds.
    pub async fn uploads_expire(&self, older_than: i64) -> Result<usize> {
        let before = now() - older_than;
        let stale: Vec<(i64, String)> = self
            .db
            .call(move |c| {
                let mut stmt = c.prepare("SELECT user_id, id FROM uploads WHERE created_at < ?1")?;
                let rows = stmt.query_map([before], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        let n = stale.len();
        for (user_id, id) in stale {
            let _ = tokio::fs::remove_file(self.upload_path(user_id, &id)?).await;
            self.upload_forget(user_id, &id).await?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use home_core::{Db, accounts};

    async fn fixture() -> (tempfile::TempDir, Store, i64) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().await.unwrap();
        db.migrate("home-core", home_core::MIGRATIONS).await.unwrap();
        db.migrate("dekave", crate::MIGRATIONS).await.unwrap();
        let user = accounts::create(&db, "alice", "password1", true).await.unwrap();
        (dir, Store::new(db, std::path::PathBuf::new()), user.id)
    }

    fn one(bytes: &'static [u8]) -> impl futures_util::Stream<Item = std::result::Result<axum::body::Bytes, std::io::Error>> + Unpin {
        futures_util::stream::iter([Ok(axum::body::Bytes::from_static(bytes))])
    }

    fn broken(bytes: &'static [u8]) -> impl futures_util::Stream<Item = std::result::Result<axum::body::Bytes, std::io::Error>> + Unpin {
        futures_util::stream::iter([Ok(axum::body::Bytes::from_static(bytes)), Err(std::io::Error::other("connection reset"))])
    }

    #[tokio::test]
    async fn chunks_resume_and_finish() {
        let (dir, mut store, uid) = fixture().await;
        store = Store::new(store.db.clone(), dir.path().join("users"));
        let up = store.upload_begin(uid, None, "big.bin", 10).await.unwrap();
        assert_eq!(store.upload_append(uid, &up.id, 0, one(b"0123")).await.unwrap().received, 4);

        // A chunk that dies halfway leaves nothing behind.
        assert!(matches!(store.upload_append(uid, &up.id, 4, broken(b"45")).await, Err(Error::BadRequest(_))));
        assert_eq!(store.upload_status(uid, &up.id).await.unwrap().received, 4);

        // The wrong offset is refused; so is sending the same chunk twice.
        assert!(matches!(store.upload_append(uid, &up.id, 0, one(b"0123")).await, Err(Error::Conflict(_))));
        assert!(matches!(store.upload_finish(uid, &up.id).await, Err(Error::BadRequest(_))), "not all bytes yet");
        assert!(matches!(store.upload_append(uid, &up.id, 4, one(b"456789AB")).await, Err(Error::BadRequest(_))), "too many bytes");

        assert_eq!(store.upload_append(uid, &up.id, 4, one(b"456789")).await.unwrap().received, 10);
        let entry = store.upload_finish(uid, &up.id).await.unwrap();
        assert_eq!(entry.size, 10);
        assert_eq!(entry.hash.as_deref(), Some(blake3::hash(b"0123456789").to_hex().as_str()));
        assert_eq!(std::fs::read(store.path_of(uid, entry.id).await.unwrap()).unwrap(), b"0123456789");
        assert!(matches!(store.upload_status(uid, &up.id).await, Err(Error::NotFound)));
        assert!(store.user_dir(uid, "tmp").unwrap().read_dir().unwrap().next().is_none());
    }

    #[tokio::test]
    async fn cancel_expire_and_ownership() {
        let (dir, mut store, uid) = fixture().await;
        store = Store::new(store.db.clone(), dir.path().join("users"));
        let up = store.upload_begin(uid, None, "a.bin", 3).await.unwrap();
        assert!(matches!(store.upload_status(uid + 1, &up.id).await, Err(Error::NotFound)));
        assert!(matches!(store.upload_status(uid, "../x").await, Err(Error::NotFound)));
        store.upload_cancel(uid, &up.id).await.unwrap();
        assert!(matches!(store.upload_status(uid, &up.id).await, Err(Error::NotFound)));

        let up2 = store.upload_begin(uid, None, "b.bin", 3).await.unwrap();
        assert_eq!(store.uploads_expire(KEEP_SECONDS).await.unwrap(), 0);
        assert_eq!(store.uploads_expire(-1).await.unwrap(), 1);
        assert!(matches!(store.upload_status(uid, &up2.id).await, Err(Error::NotFound)));

        // An empty file is a valid upload.
        let up3 = store.upload_begin(uid, None, "empty.txt", 0).await.unwrap();
        assert_eq!(store.upload_finish(uid, &up3.id).await.unwrap().size, 0);
        // A name taken meanwhile is refused at begin.
        assert!(matches!(store.upload_begin(uid, None, "empty.txt", 1).await, Err(Error::Conflict(_))));
    }
}
