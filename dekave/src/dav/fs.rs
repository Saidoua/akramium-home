//! The drive seen through WebDAV. Everything goes through `Store`, so the web page, share
//! links, the trash and a mounted folder in Finder all see one truth. The credential the
//! handler passes around is the signed-in user's id.

use crate::store::{Entry, Store};
use bytes::{Buf, Bytes, BytesMut};
use dav_server::davpath::DavPath;
use dav_server::fs::{DavDirEntry, DavFile, DavMetaData, FsError, FsFuture, FsResult, FsStream, GuardedFileSystem, OpenOptions, ReadDirMeta};
use home_core::Error;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[derive(Clone)]
pub struct DavFs {
    pub store: Store,
}

fn fs_error(e: Error) -> FsError {
    match e {
        Error::NotFound | Error::Gone => FsError::NotFound,
        Error::Conflict(_) => FsError::Exists,
        Error::BadRequest(_) | Error::Forbidden | Error::Unauthorized | Error::TooMany(_) => FsError::Forbidden,
        other => {
            tracing::error!(error = %other, "webdav operation failed");
            FsError::GeneralFailure
        }
    }
}

/// The names in a request path, below `/dav`.
pub fn parts(path: &DavPath) -> FsResult<Vec<String>> {
    path.as_rel_ospath()
        .components()
        .map(|c| match c {
            std::path::Component::Normal(s) => s.to_str().map(str::to_string).ok_or(FsError::Forbidden),
            _ => Err(FsError::Forbidden),
        })
        .collect()
}

pub use crate::store::is_junk;

#[derive(Debug, Clone)]
struct Meta {
    len: u64,
    modified: SystemTime,
    is_dir: bool,
    etag: Option<String>,
}

impl Meta {
    fn root() -> Self {
        Meta { len: 0, modified: UNIX_EPOCH, is_dir: true, etag: None }
    }
    fn of(e: &Entry) -> Self {
        Meta {
            len: e.size.max(0) as u64,
            modified: UNIX_EPOCH + Duration::from_secs(e.mtime.max(0) as u64),
            is_dir: e.is_dir,
            etag: e.hash.as_ref().filter(|_| !e.is_dir).map(|h| h.chars().take(32).collect()),
        }
    }
}

impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.len
    }
    fn modified(&self) -> FsResult<SystemTime> {
        Ok(self.modified)
    }
    fn is_dir(&self) -> bool {
        self.is_dir
    }
    fn created(&self) -> FsResult<SystemTime> {
        Ok(self.modified)
    }
    fn etag(&self) -> Option<String> {
        self.etag.clone().or_else(|| {
            let t = self.modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some(format!("{t:x}"))
        })
    }
}

struct Listed(Entry);

impl DavDirEntry for Listed {
    fn name(&self) -> Vec<u8> {
        self.0.name.clone().into_bytes()
    }
    fn metadata(&'_ self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta = Meta::of(&self.0);
        Box::pin(async move { Ok(Box::new(meta) as Box<dyn DavMetaData>) })
    }
}

#[derive(Debug)]
struct ReadFile {
    file: tokio::fs::File,
    meta: Meta,
}

impl DavFile for ReadFile {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta = self.meta.clone();
        Box::pin(async move { Ok(Box::new(meta) as Box<dyn DavMetaData>) })
    }
    fn write_buf(&'_ mut self, _buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }
    fn write_bytes(&'_ mut self, _buf: Bytes) -> FsFuture<'_, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }
    fn read_bytes(&'_ mut self, count: usize) -> FsFuture<'_, Bytes> {
        Box::pin(async move {
            let mut buf = BytesMut::zeroed(count.min(1 << 20));
            let n = self.file.read(&mut buf).await.map_err(|_| FsError::GeneralFailure)?;
            buf.truncate(n);
            Ok(buf.freeze())
        })
    }
    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async move { self.file.seek(pos).await.map_err(|_| FsError::GeneralFailure) })
    }
    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A PUT in progress: bytes land in `tmp/`, and `flush` moves them into place and indexes
/// them. A PUT that dies before `flush` leaves the drive as it was.
struct WriteFile {
    store: Store,
    user_id: i64,
    parent: Option<i64>,
    name: String,
    tmp: PathBuf,
    file: Option<tokio::fs::File>,
    hasher: blake3::Hasher,
    size: u64,
    committed: bool,
}

impl std::fmt::Debug for WriteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteFile").field("name", &self.name).field("size", &self.size).finish()
    }
}

impl Drop for WriteFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

impl WriteFile {
    async fn write(&mut self, bytes: &[u8]) -> FsResult<()> {
        let file = self.file.as_mut().ok_or(FsError::GeneralFailure)?;
        file.write_all(bytes).await.map_err(|_| FsError::InsufficientStorage)?;
        self.hasher.update(bytes);
        self.size += bytes.len() as u64;
        Ok(())
    }
}

impl DavFile for WriteFile {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta = Meta { len: self.size, modified: SystemTime::now(), is_dir: false, etag: None };
        Box::pin(async move { Ok(Box::new(meta) as Box<dyn DavMetaData>) })
    }
    fn write_buf(&'_ mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async move {
            while buf.has_remaining() {
                let chunk = buf.chunk().to_vec();
                self.write(&chunk).await?;
                buf.advance(chunk.len());
            }
            Ok(())
        })
    }
    fn write_bytes(&'_ mut self, buf: Bytes) -> FsFuture<'_, ()> {
        Box::pin(async move { self.write(&buf).await })
    }
    fn read_bytes(&'_ mut self, _count: usize) -> FsFuture<'_, Bytes> {
        Box::pin(async { Err(FsError::Forbidden) })
    }
    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        // Writes are sequential; only a seek to where we already are is honoured.
        let here = self.size;
        Box::pin(async move {
            match pos {
                SeekFrom::Start(p) if p == here => Ok(here),
                SeekFrom::Current(0) | SeekFrom::End(0) => Ok(here),
                _ => Err(FsError::NotImplemented),
            }
        })
    }
    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        Box::pin(async move {
            let Some(file) = self.file.take() else { return Ok(()) };
            file.sync_all().await.map_err(|_| FsError::GeneralFailure)?;
            drop(file);
            let hash = self.hasher.finalize().to_hex().to_string();
            self.store.commit_file(self.user_id, self.parent, &self.name, &self.tmp, self.size as i64, hash).await.map_err(fs_error)?;
            self.committed = true;
            Ok(())
        })
    }
}

impl DavFs {
    async fn lookup(&self, user_id: i64, path: &DavPath) -> FsResult<Option<Entry>> {
        self.store.resolve(user_id, &parts(path)?).await.map_err(fs_error)
    }

    /// The folder a path's last name lives in, and that name.
    async fn parent_and_name(&self, user_id: i64, path: &DavPath) -> FsResult<(Option<i64>, String)> {
        let mut names = parts(path)?;
        let name = names.pop().ok_or(FsError::Forbidden)?;
        let parent = self.store.resolve(user_id, &names).await.map_err(fs_error)?;
        if parent.as_ref().is_some_and(|p| !p.is_dir) {
            return Err(FsError::Forbidden);
        }
        Ok((parent.map(|p| p.id), name))
    }

    /// To the trash; client droppings skip the trash.
    async fn discard(&self, user_id: i64, path: &DavPath) -> FsResult<()> {
        let entry = self.lookup(user_id, path).await?.ok_or(FsError::Forbidden)?;
        self.store.trash(user_id, entry.id).await.map_err(fs_error)?;
        if is_junk(&entry.name) {
            self.store.purge(user_id, entry.id).await.map_err(fs_error)?;
        }
        Ok(())
    }
}

impl GuardedFileSystem<i64> for DavFs {
    fn open<'a>(&'a self, path: &'a DavPath, options: OpenOptions, user: &'a i64) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            let user_id = *user;
            if !(options.write || options.append || options.create || options.create_new) {
                let entry = self.lookup(user_id, path).await?.ok_or(FsError::Forbidden)?;
                if entry.is_dir {
                    return Err(FsError::Forbidden);
                }
                let disk = self.store.path_of(user_id, entry.id).await.map_err(fs_error)?;
                let file = tokio::fs::File::open(disk).await.map_err(|_| FsError::NotFound)?;
                return Ok(Box::new(ReadFile { file, meta: Meta::of(&entry) }) as Box<dyn DavFile>);
            }

            // Partial updates (Content-Range, X-Update-Range) are not offered: a PUT replaces.
            if options.append || !options.truncate {
                return Err(FsError::NotImplemented);
            }
            let (parent, name) = self.parent_and_name(user_id, path).await?;
            let existing = self.store.resolve(user_id, &parts(path)?).await;
            match (&existing, options.create, options.create_new) {
                (Ok(Some(e)), _, _) if e.is_dir => return Err(FsError::Forbidden),
                (Ok(Some(_)), _, true) => return Err(FsError::Exists),
                (Err(Error::NotFound), false, false) => return Err(FsError::NotFound),
                (Err(Error::NotFound), _, _) => {
                    // A LOCK on a new name creates the file and never writes to it, so the
                    // empty file exists from the moment it is opened.
                    let tmp = self.store.tmp_path(user_id).map_err(fs_error)?;
                    tokio::fs::File::create(&tmp).await.map_err(|_| FsError::GeneralFailure)?;
                    let empty = blake3::Hasher::new().finalize().to_hex().to_string();
                    self.store.commit_file(user_id, parent, &name, &tmp, 0, empty).await.map_err(fs_error)?;
                }
                (Err(_), _, _) => return Err(FsError::Forbidden),
                _ => {}
            }
            let tmp = self.store.tmp_path(user_id).map_err(fs_error)?;
            let file = tokio::fs::File::create(&tmp).await.map_err(|_| FsError::GeneralFailure)?;
            Ok(Box::new(WriteFile { store: self.store.clone(), user_id, parent, name, tmp, file: Some(file), hasher: blake3::Hasher::new(), size: 0, committed: false })
                as Box<dyn DavFile>)
        })
    }

    fn read_dir<'a>(&'a self, path: &'a DavPath, _meta: ReadDirMeta, user: &'a i64) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            let folder = self.lookup(*user, path).await?;
            if folder.as_ref().is_some_and(|e| !e.is_dir) {
                return Err(FsError::Forbidden);
            }
            let listing = self.store.list(*user, folder.map(|e| e.id)).await.map_err(fs_error)?;
            let items = listing.entries.into_iter().map(|e| Ok(Box::new(Listed(e)) as Box<dyn DavDirEntry>));
            Ok(Box::pin(futures_util::stream::iter(items)) as FsStream<Box<dyn DavDirEntry>>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a DavPath, user: &'a i64) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let meta = match self.lookup(*user, path).await? {
                Some(e) => Meta::of(&e),
                None => Meta::root(),
            };
            Ok(Box::new(meta) as Box<dyn DavMetaData>)
        })
    }

    fn symlink_metadata<'a>(&'a self, path: &'a DavPath, user: &'a i64) -> FsFuture<'a, Box<dyn DavMetaData>> {
        self.metadata(path, user)
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath, user: &'a i64) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let (parent, name) = self.parent_and_name(*user, path).await?;
            self.store.create_folder(*user, parent, &name).await.map(|_| ()).map_err(fs_error)
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath, user: &'a i64) -> FsFuture<'a, ()> {
        Box::pin(self.discard(*user, path))
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath, user: &'a i64) -> FsFuture<'a, ()> {
        Box::pin(self.discard(*user, path))
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath, user: &'a i64) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let entry = self.lookup(*user, from).await?.ok_or(FsError::Forbidden)?;
            let (parent, name) = self.parent_and_name(*user, to).await?;
            self.store.move_rename(*user, entry.id, parent, &name).await.map(|_| ()).map_err(fs_error)
        })
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath, user: &'a i64) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let entry = self.lookup(*user, from).await?.ok_or(FsError::Forbidden)?;
            let (parent, name) = self.parent_and_name(*user, to).await?;
            self.store.copy_file(*user, entry.id, parent, &name).await.map(|_| ()).map_err(fs_error)
        })
    }
}
