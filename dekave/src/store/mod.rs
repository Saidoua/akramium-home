//! One `Store` behind the API, later WebDAV and Doks. Every operation is a filesystem step
//! plus an index step, in that order for writes and the reverse for deletes, so a crash
//! leaves at worst an unindexed file (found by a rescan), never a dangling row.
//!
//! Layout under `data_dir/users/<id>/`: `files/` mirrors the live tree by name; `trash/<entry id>`
//! holds each trashed item (with its subtree); `tmp/` holds uploads in flight.

pub mod blobs;
pub mod index;
pub mod shares;
pub mod thumbs;
pub mod uploads;

use home_core::{Db, Error, Result, names, now};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub use index::Entry;

#[derive(Clone)]
pub struct Store {
    db: Db,
    root: PathBuf,
    /// Uploads with a chunk being written right now; a second writer is turned away.
    busy: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

#[derive(Debug, Serialize)]
pub struct Listing {
    pub folder: Option<Entry>,
    pub crumbs: Vec<Entry>,
    pub entries: Vec<Entry>,
}

impl Store {
    pub fn new(db: Db, root: PathBuf) -> Self {
        Self { db, root, busy: Default::default() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn user_dir(&self, user_id: i64, sub: &str) -> Result<PathBuf> {
        let dir = self.root.join(user_id.to_string()).join(sub);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// `data_dir/users/<id>/files`, created on first use.
    pub fn user_root(&self, user_id: i64) -> Result<PathBuf> {
        self.user_dir(user_id, "files")
    }

    /// The chain of an entry that is live: no ancestor in the trash. `NotFound` otherwise.
    pub(crate) async fn live_chain(&self, user_id: i64, id: i64) -> Result<Vec<Entry>> {
        let chain = index::chain(&self.db, user_id, id).await?;
        if chain.iter().any(|e| e.trashed_at.is_some()) {
            return Err(Error::NotFound);
        }
        Ok(chain)
    }

    /// Where an entry's bytes are, live or trashed. Asserts the result stays under the user's dir.
    pub async fn path_of(&self, user_id: i64, id: i64) -> Result<PathBuf> {
        let chain = index::chain(&self.db, user_id, id).await?;
        self.path_of_chain(user_id, &chain)
    }

    pub(crate) fn path_of_chain(&self, user_id: i64, chain: &[Entry]) -> Result<PathBuf> {
        let base = self.root.join(user_id.to_string());
        let mut path = self.user_root(user_id)?;
        for e in chain {
            if e.trashed_at.is_some() {
                path = self.user_dir(user_id, "trash")?.join(e.id.to_string());
            } else {
                path.push(names::file_name(&e.name)?);
            }
        }
        if !path.starts_with(&base) {
            return Err(Error::Internal("path escaped the user directory".into()));
        }
        Ok(path)
    }

    /// A live entry.
    pub async fn entry(&self, user_id: i64, id: i64) -> Result<Entry> {
        let chain = self.live_chain(user_id, id).await?;
        chain.into_iter().last().ok_or(Error::NotFound)
    }

    /// A live folder's path, or the root for `None`.
    pub(crate) async fn folder_path(&self, user_id: i64, folder: Option<i64>) -> Result<PathBuf> {
        match folder {
            Some(id) => {
                let chain = self.live_chain(user_id, id).await?;
                if !chain.last().is_some_and(|e| e.is_dir) {
                    return Err(Error::BadRequest("that is a file, not a folder".into()));
                }
                self.path_of_chain(user_id, &chain)
            }
            None => self.user_root(user_id),
        }
    }

    /// The contents of a folder (`None` = the root) with its breadcrumb chain.
    pub async fn list(&self, user_id: i64, folder: Option<i64>) -> Result<Listing> {
        let (folder_entry, crumbs) = match folder {
            Some(id) => {
                let chain = self.live_chain(user_id, id).await?;
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

    pub(crate) async fn check_free(&self, user_id: i64, parent: Option<i64>, name: &str) -> Result<()> {
        if index::find_child(&self.db, user_id, parent, name).await?.is_some() {
            return Err(Error::Conflict(format!("there is already something called {name} here")));
        }
        Ok(())
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
        let parent_path = self.folder_path(user_id, parent).await?;
        self.check_free(user_id, parent, &name).await?;

        let tmp = self.user_dir(user_id, "tmp")?.join(format!("{}-{}", now(), home_core::session::random_token()));
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut hasher = blake3::Hasher::new();
        let mut size: i64 = 0;
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    drop(file);
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(Error::BadRequest(format!("upload interrupted: {e}")));
                }
            };
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
        let parent_path = self.folder_path(user_id, parent).await?;
        self.check_free(user_id, parent, &name).await?;
        tokio::fs::create_dir(parent_path.join(&name)).await?;
        index::insert(&self.db, index::NewEntry { user_id, parent_id: parent, name, is_dir: true, size: 0, hash: None, mime: None }).await
    }

    pub async fn rename(&self, user_id: i64, id: i64, new_name: &str) -> Result<Entry> {
        let new_name = names::file_name(new_name)?.to_string();
        let chain = self.live_chain(user_id, id).await?;
        let entry = chain.last().cloned().ok_or(Error::NotFound)?;
        if entry.name == new_name {
            return Ok(entry);
        }
        self.check_free(user_id, entry.parent_id, &new_name).await?;
        let from = self.path_of_chain(user_id, &chain)?;
        let to = from.with_file_name(&new_name);
        tokio::fs::rename(&from, &to).await?;
        if let Err(e) = index::rename(&self.db, user_id, id, new_name).await {
            let _ = tokio::fs::rename(&to, &from).await;
            return Err(e);
        }
        self.entry(user_id, id).await
    }

    /// Moves an entry into another live folder (`None` = the root).
    pub async fn move_to(&self, user_id: i64, id: i64, new_parent: Option<i64>) -> Result<Entry> {
        let chain = self.live_chain(user_id, id).await?;
        let entry = chain.last().cloned().ok_or(Error::NotFound)?;
        if entry.parent_id == new_parent {
            return Ok(entry);
        }
        let target_dir = match new_parent {
            Some(target) => {
                let target_chain = self.live_chain(user_id, target).await?;
                if !target_chain.last().is_some_and(|e| e.is_dir) {
                    return Err(Error::BadRequest("the target is not a folder".into()));
                }
                if target_chain.iter().any(|e| e.id == id) {
                    return Err(Error::BadRequest("a folder cannot move into itself".into()));
                }
                self.path_of_chain(user_id, &target_chain)?
            }
            None => self.user_root(user_id)?,
        };
        self.check_free(user_id, new_parent, &entry.name).await?;
        let from = self.path_of_chain(user_id, &chain)?;
        let to = target_dir.join(&entry.name);
        tokio::fs::rename(&from, &to).await?;
        if let Err(e) = index::set_parent(&self.db, user_id, id, new_parent).await {
            let _ = tokio::fs::rename(&to, &from).await;
            return Err(e);
        }
        self.entry(user_id, id).await
    }

    /// Moves an entry (with its subtree) into the trash.
    pub async fn trash(&self, user_id: i64, id: i64) -> Result<()> {
        let chain = self.live_chain(user_id, id).await?;
        let from = self.path_of_chain(user_id, &chain)?;
        let to = self.user_dir(user_id, "trash")?.join(id.to_string());
        tokio::fs::rename(&from, &to).await?;
        if let Err(e) = index::set_trashed(&self.db, user_id, id).await {
            let _ = tokio::fs::rename(&to, &from).await;
            return Err(e);
        }
        Ok(())
    }

    pub async fn list_trash(&self, user_id: i64) -> Result<Vec<Entry>> {
        index::trashed(&self.db, user_id).await
    }

    /// Puts a trashed entry back where it was, or in the root if that folder is gone.
    pub async fn restore(&self, user_id: i64, id: i64) -> Result<Entry> {
        let entry = index::get(&self.db, user_id, id).await?.ok_or(Error::NotFound)?;
        if entry.trashed_at.is_none() {
            return Err(Error::BadRequest("that item is not in the trash".into()));
        }
        let parent = match entry.orig_parent_id {
            Some(p) if self.live_chain(user_id, p).await.is_ok() => Some(p),
            _ => None,
        };
        self.check_free(user_id, parent, &entry.name).await?;
        let from = self.user_dir(user_id, "trash")?.join(id.to_string());
        let to = self.folder_path(user_id, parent).await?.join(&entry.name);
        tokio::fs::rename(&from, &to).await?;
        if let Err(e) = index::set_restored(&self.db, user_id, id, parent).await {
            let _ = tokio::fs::rename(&to, &from).await;
            return Err(e);
        }
        self.entry(user_id, id).await
    }

    /// Deletes a trashed entry for good.
    pub async fn purge(&self, user_id: i64, id: i64) -> Result<()> {
        let entry = index::get(&self.db, user_id, id).await?.ok_or(Error::NotFound)?;
        if entry.trashed_at.is_none() {
            return Err(Error::BadRequest("only items in the trash can be deleted for good".into()));
        }
        let path = self.user_dir(user_id, "trash")?.join(id.to_string());
        let removed = if entry.is_dir { tokio::fs::remove_dir_all(&path).await } else { tokio::fs::remove_file(&path).await };
        match removed {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        index::delete(&self.db, user_id, id).await
    }

    pub async fn empty_trash(&self, user_id: i64) -> Result<usize> {
        let items = self.list_trash(user_id).await?;
        let n = items.len();
        for e in items {
            self.purge(user_id, e.id).await?;
        }
        Ok(n)
    }

    /// Purges every trashed item older than `keep_days`. Returns how many went.
    pub async fn purge_expired(&self, keep_days: i64) -> Result<usize> {
        let items = index::trashed_before(&self.db, now() - keep_days * 86_400).await?;
        let mut n = 0;
        for (user_id, e) in items {
            match self.purge(user_id, e.id).await {
                Ok(()) => n += 1,
                Err(err) => tracing::warn!(user_id, id = e.id, error = %err, "purge failed"),
            }
        }
        Ok(n)
    }

    /// Hourly purge of expired trash.
    pub fn spawn_purge_task(&self, keep_days: i64) {
        let store = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tick.tick().await;
                match store.purge_expired(keep_days).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(n, "purged expired trash"),
                    Err(e) => tracing::warn!(error = %e, "trash purge failed"),
                }
                if let Err(e) = store.shares_expire().await {
                    tracing::warn!(error = %e, "share cleanup failed");
                }
                match store.uploads_expire(uploads::KEEP_SECONDS).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(n, "dropped abandoned uploads"),
                    Err(e) => tracing::warn!(error = %e, "upload cleanup failed"),
                }
            }
        });
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
        assert!(store.user_dir(uid, "tmp").unwrap().read_dir().unwrap().next().is_none(), "tmp is empty after upload");
    }

    #[tokio::test]
    async fn rejects_duplicates_and_bad_names() {
        let (_dir, store, uid) = fixture().await;
        store.create_file(uid, None, "a.txt", stream(b"1")).await.unwrap();
        assert!(matches!(store.create_file(uid, None, "a.txt", stream(b"2")).await, Err(Error::Conflict(_))));
        assert!(matches!(store.create_file(uid, None, "../x", stream(b"2")).await, Err(Error::BadRequest(_))));
        assert!(matches!(store.create_folder(uid, Some(999), "x").await, Err(Error::NotFound)));
        assert!(store.user_dir(uid, "tmp").unwrap().read_dir().unwrap().next().is_none(), "failed uploads leave no tmp file");
    }

    #[tokio::test]
    async fn users_do_not_see_each_other() {
        let (_dir, store, uid) = fixture().await;
        let f = store.create_file(uid, None, "a.txt", stream(b"1")).await.unwrap();
        assert!(matches!(store.entry(uid + 1, f.id).await, Err(Error::NotFound)));
        assert!(matches!(store.path_of(uid + 1, f.id).await, Err(Error::NotFound)));
    }

    #[tokio::test]
    async fn rename_and_move() {
        let (_dir, store, uid) = fixture().await;
        let a = store.create_folder(uid, None, "A").await.unwrap();
        let b = store.create_folder(uid, Some(a.id), "B").await.unwrap();
        let f = store.create_file(uid, Some(b.id), "f.txt", stream(b"x")).await.unwrap();
        let root = store.user_root(uid).unwrap();

        let f2 = store.rename(uid, f.id, "g.txt").await.unwrap();
        assert_eq!(f2.name, "g.txt");
        assert!(root.join("A/B/g.txt").exists());
        assert!(!root.join("A/B/f.txt").exists());

        store.move_to(uid, f.id, None).await.unwrap();
        assert!(root.join("g.txt").exists());
        assert!(matches!(store.move_to(uid, a.id, Some(b.id)).await, Err(Error::BadRequest(_))), "folder into its child");
        assert!(matches!(store.move_to(uid, f.id, Some(f.id)).await, Err(Error::BadRequest(_))), "file as target");

        store.create_file(uid, Some(a.id), "g.txt", stream(b"y")).await.unwrap();
        assert!(matches!(store.move_to(uid, f.id, Some(a.id)).await, Err(Error::Conflict(_))));
        assert!(root.join("g.txt").exists(), "a failed move leaves the file in place");

        let a2 = store.rename(uid, a.id, "Archive").await.unwrap();
        assert_eq!(a2.name, "Archive");
        assert_eq!(store.path_of(uid, b.id).await.unwrap(), root.join("Archive/B"));
    }

    #[tokio::test]
    async fn trash_restore_purge() {
        let (_dir, store, uid) = fixture().await;
        let a = store.create_folder(uid, None, "A").await.unwrap();
        let f = store.create_file(uid, Some(a.id), "f.txt", stream(b"x")).await.unwrap();
        let root = store.user_root(uid).unwrap();
        let trash_dir = root.with_file_name("trash");

        store.trash(uid, a.id).await.unwrap();
        assert!(!root.join("A").exists());
        assert!(trash_dir.join(a.id.to_string()).join("f.txt").exists());
        assert!(store.list(uid, None).await.unwrap().entries.is_empty());
        assert!(matches!(store.entry(uid, f.id).await, Err(Error::NotFound)), "a child of a trashed folder is not live");
        assert!(matches!(store.list(uid, Some(a.id)).await, Err(Error::NotFound)));
        assert_eq!(store.path_of(uid, f.id).await.unwrap(), trash_dir.join(a.id.to_string()).join("f.txt"));
        assert_eq!(store.list_trash(uid).await.unwrap().len(), 1);

        // A new "A" appears meanwhile; restore then conflicts.
        store.create_folder(uid, None, "A").await.unwrap();
        assert!(matches!(store.restore(uid, a.id).await, Err(Error::Conflict(_))));
        store.trash(uid, store.list(uid, None).await.unwrap().entries[0].id).await.unwrap();
        let back = store.restore(uid, a.id).await.unwrap();
        assert_eq!(back.parent_id, None);
        assert!(root.join("A/f.txt").exists());
        assert!(store.entry(uid, f.id).await.is_ok());

        // Restore into a parent that was trashed lands in the root.
        let b = store.create_folder(uid, Some(a.id), "B").await.unwrap();
        store.trash(uid, b.id).await.unwrap();
        store.trash(uid, a.id).await.unwrap();
        let b2 = store.restore(uid, b.id).await.unwrap();
        assert_eq!(b2.parent_id, None);
        assert!(root.join("B").exists());

        assert!(matches!(store.purge(uid, b.id).await, Err(Error::BadRequest(_))), "live items cannot be purged");
        assert_eq!(store.empty_trash(uid).await.unwrap(), 2);
        assert!(store.list_trash(uid).await.unwrap().is_empty());
        assert!(matches!(store.path_of(uid, f.id).await, Err(Error::NotFound)), "children rows went with the folder");
        assert!(trash_dir.read_dir().unwrap().next().is_none());
    }

    #[tokio::test]
    async fn purge_expired_only_old_items() {
        let (_dir, store, uid) = fixture().await;
        let f = store.create_file(uid, None, "old.txt", stream(b"x")).await.unwrap();
        store.trash(uid, f.id).await.unwrap();
        assert_eq!(store.purge_expired(30).await.unwrap(), 0);
        assert_eq!(store.purge_expired(0).await.unwrap(), 0, "trashed just now is not before now");
        assert_eq!(store.purge_expired(-1).await.unwrap(), 1);
    }
}
