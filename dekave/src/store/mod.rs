//! One `Store` behind the API, later WebDAV and Doks. Every operation is a filesystem step
//! plus an index step, in that order for writes and the reverse for deletes, so a crash
//! leaves at worst an unindexed file (found by a rescan), never a dangling row.

pub mod blobs;
pub mod index;

use home_core::{Db, Error, Result, names, now};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub use index::Entry;

#[derive(Clone)]
pub struct Store {
    db: Db,
    root: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Listing {
    pub folder: Option<Entry>,
    pub crumbs: Vec<Entry>,
    pub entries: Vec<Entry>,
}

impl Store {
    pub fn new(db: Db, root: PathBuf) -> Self {
        Self { db, root }
    }

    /// `data_dir/users/<id>/files`, created on first use.
    pub fn user_root(&self, user_id: i64) -> Result<PathBuf> {
        let dir = self.root.join(user_id.to_string()).join("files");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn tmp_dir(&self, user_id: i64) -> Result<PathBuf> {
        let dir = self.root.join(user_id.to_string()).join("tmp");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// The absolute path of an entry, from its chain of parents. Asserts it stays under the root.
    pub async fn path_of(&self, user_id: i64, id: i64) -> Result<PathBuf> {
        let chain = index::chain(&self.db, user_id, id).await?;
        let root = self.user_root(user_id)?;
        let mut path = root.clone();
        for e in &chain {
            path.push(names::file_name(&e.name)?);
        }
        if !path.starts_with(&root) {
            return Err(Error::Internal("path escaped the user root".into()));
        }
        Ok(path)
    }

    pub async fn entry(&self, user_id: i64, id: i64) -> Result<Entry> {
        index::get(&self.db, user_id, id).await?.ok_or(Error::NotFound)
    }

    /// The contents of a folder (`None` = the root) with its breadcrumb chain.
    pub async fn list(&self, user_id: i64, folder: Option<i64>) -> Result<Listing> {
        let (folder_entry, crumbs) = match folder {
            Some(id) => {
                let chain = index::chain(&self.db, user_id, id).await?;
                let last = chain.last().cloned().ok_or(Error::NotFound)?;
                if !last.is_dir {
                    return Err(Error::BadRequest("that is a file, not a folder".into()));
                }
                (Some(last), chain)
            }
            None => (None, Vec::new()),
        };
        let entries = index::children(&self.db, user_id, folder).await?;
        Ok(Listing { folder: folder_entry, crumbs, entries })
    }

    /// Writes a new file from a stream of chunks. Lands in `tmp/`, then renames into place.
    pub async fn create_file<S, E>(&self, user_id: i64, parent: Option<i64>, name: &str, mut body: S) -> Result<Entry>
    where
        S: futures_util::Stream<Item = std::result::Result<axum::body::Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let name = names::file_name(name)?.to_string();
        let parent_path = match parent {
            Some(id) => {
                let e = self.entry(user_id, id).await?;
                if !e.is_dir {
                    return Err(Error::BadRequest("the parent is not a folder".into()));
                }
                self.path_of(user_id, id).await?
            }
            None => self.user_root(user_id)?,
        };
        if index::find_child(&self.db, user_id, parent, &name).await?.is_some() {
            return Err(Error::Conflict(format!("there is already something called {name} here")));
        }

        let tmp = self.tmp_dir(user_id)?.join(format!("{}-{}", now(), home_core::session::random_token()));
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut hasher = blake3::Hasher::new();
        let mut size: i64 = 0;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| Error::BadRequest(format!("upload interrupted: {e}")))?;
            hasher.update(&chunk);
            size += chunk.len() as i64;
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        drop(file);

        let final_path = parent_path.join(&name);
        tokio::fs::rename(&tmp, &final_path).await?;
        let mime = mime_guess::from_path(&name).first_or_octet_stream().to_string();
        let entry = index::insert(&self.db, index::NewEntry {
            user_id,
            parent_id: parent,
            name,
            is_dir: false,
            size,
            hash: Some(hasher.finalize().to_hex().to_string()),
            mime: Some(mime),
        })
        .await;
        match entry {
            Ok(e) => Ok(e),
            Err(err) => {
                let _ = tokio::fs::remove_file(&final_path).await;
                Err(err)
            }
        }
    }

    pub async fn create_folder(&self, user_id: i64, parent: Option<i64>, name: &str) -> Result<Entry> {
        let name = names::file_name(name)?.to_string();
        let parent_path = match parent {
            Some(id) => {
                let e = self.entry(user_id, id).await?;
                if !e.is_dir {
                    return Err(Error::BadRequest("the parent is not a folder".into()));
                }
                self.path_of(user_id, id).await?
            }
            None => self.user_root(user_id)?,
        };
        if index::find_child(&self.db, user_id, parent, &name).await?.is_some() {
            return Err(Error::Conflict(format!("there is already something called {name} here")));
        }
        tokio::fs::create_dir(parent_path.join(&name)).await?;
        index::insert(&self.db, index::NewEntry { user_id, parent_id: parent, name, is_dir: true, size: 0, hash: None, mime: None }).await
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use home_core::accounts;

    async fn fixture() -> (tempfile::TempDir, Store, i64) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().await.unwrap();
        db.migrate("home-core", home_core::MIGRATIONS).await.unwrap();
        db.migrate("dekave", crate::MIGRATIONS).await.unwrap();
        let user = accounts::create(&db, "alice", "password1", true).await.unwrap();
        let store = Store::new(db, dir.path().join("users"));
        (dir, store, user.id)
    }

    fn stream(bytes: &'static [u8]) -> impl futures_util::Stream<Item = std::result::Result<axum::body::Bytes, std::io::Error>> + Unpin {
        futures_util::stream::iter(bytes.chunks(3).map(|c| Ok(axum::body::Bytes::copy_from_slice(c))))
    }

    #[tokio::test]
    async fn upload_list_and_path() {
        let (_dir, store, uid) = fixture().await;
        let folder = store.create_folder(uid, None, "Photos").await.unwrap();
        let file = store.create_file(uid, Some(folder.id), "cat.jpg", stream(b"hello world")).await.unwrap();
        assert_eq!(file.size, 11);
        assert_eq!(file.mime.as_deref(), Some("image/jpeg"));
        let path = store.path_of(uid, file.id).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
        assert!(path.ends_with("files/Photos/cat.jpg"));
        let listing = store.list(uid, Some(folder.id)).await.unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.crumbs.len(), 1);
        let root = store.list(uid, None).await.unwrap();
        assert_eq!(root.entries[0].name, "Photos");
        assert!(store.tmp_dir(uid).unwrap().read_dir().unwrap().next().is_none(), "tmp is empty after upload");
    }

    #[tokio::test]
    async fn rejects_duplicates_and_bad_names() {
        let (_dir, store, uid) = fixture().await;
        store.create_file(uid, None, "a.txt", stream(b"1")).await.unwrap();
        assert!(matches!(store.create_file(uid, None, "a.txt", stream(b"2")).await, Err(Error::Conflict(_))));
        assert!(matches!(store.create_file(uid, None, "../x", stream(b"2")).await, Err(Error::BadRequest(_))));
        assert!(matches!(store.create_folder(uid, Some(999), "x").await, Err(Error::NotFound)));
    }

    #[tokio::test]
    async fn users_do_not_see_each_other() {
        let (_dir, store, uid) = fixture().await;
        let f = store.create_file(uid, None, "a.txt", stream(b"1")).await.unwrap();
        assert!(matches!(store.entry(uid + 1, f.id).await, Err(Error::NotFound)));
        assert!(matches!(store.path_of(uid + 1, f.id).await, Err(Error::NotFound)));
    }
}
