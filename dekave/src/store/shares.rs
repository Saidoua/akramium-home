//! Share links: a random token that lets anyone holding it read one file or one folder
//! (and what is inside it), until it expires or is revoked. Read-only, always.

use super::{Entry, Store, index};
use home_core::{Error, Result, now};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Share {
    pub id: i64,
    pub token: String,
    pub file_id: i64,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    /// Filled when listing: the shared item's name and kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_dir: Option<bool>,
}

/// What a token opens: whose drive, which share, and the shared entry.
#[derive(Debug)]
pub struct Opened {
    pub owner: i64,
    pub share: Share,
    pub root: Entry,
}

/// Longest life a link may be given: ten years. `None` means no expiry.
const MAX_LIFE: i64 = 10 * 365 * 86_400;

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Share> {
    Ok(Share { id: r.get(0)?, token: r.get(1)?, file_id: r.get(2)?, created_at: r.get(3)?, expires_at: r.get(4)?, name: None, is_dir: None })
}

impl Store {
    pub async fn share_create(&self, user_id: i64, file_id: i64, expires_in: Option<i64>) -> Result<Share> {
        let entry = self.entry(user_id, file_id).await?;
        let expires_at = match expires_in {
            Some(s) if s <= 0 || s > MAX_LIFE => return Err(Error::BadRequest("the expiry is out of range".into())),
            Some(s) => Some(now() + s),
            None => None,
        };
        let token = home_core::session::random_token();
        let mut share = self
            .db
            .call(move |c| {
                c.execute(
                    "INSERT INTO shares (token, user_id, file_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![token, user_id, file_id, now(), expires_at],
                )?;
                let id = c.last_insert_rowid();
                Ok(c.query_row("SELECT id, token, file_id, created_at, expires_at FROM shares WHERE id = ?1", [id], row)?)
            })
            .await?;
        share.name = Some(entry.name);
        share.is_dir = Some(entry.is_dir);
        Ok(share)
    }

    /// The user's links, newest first; for one item when `file_id` is given. Links whose item
    /// sits in the trash are left out (they come back if the item is restored).
    pub async fn share_list(&self, user_id: i64, file_id: Option<i64>) -> Result<Vec<Share>> {
        let shares: Vec<Share> = self
            .db
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT s.id, s.token, s.file_id, s.created_at, s.expires_at, f.name, f.is_dir
                     FROM shares s JOIN files f ON f.id = s.file_id
                     WHERE s.user_id = ?1 AND (?2 IS NULL OR s.file_id = ?2)
                     ORDER BY s.created_at DESC, s.id DESC",
                )?;
                let rows = stmt.query_map(params![user_id, file_id], |r| {
                    let mut s = row(r)?;
                    s.name = Some(r.get(5)?);
                    s.is_dir = Some(r.get::<_, i64>(6)? != 0);
                    Ok(s)
                })?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        let mut live = Vec::with_capacity(shares.len());
        for s in shares {
            if self.live_chain(user_id, s.file_id).await.is_ok() {
                live.push(s);
            }
        }
        Ok(live)
    }

    pub async fn share_revoke(&self, user_id: i64, share_id: i64) -> Result<()> {
        let n = self
            .db
            .call(move |c| Ok(c.execute("DELETE FROM shares WHERE user_id = ?1 AND id = ?2", params![user_id, share_id])?))
            .await?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// Resolves a token. `NotFound` for an unknown token or a trashed item, `Gone` once expired.
    pub async fn share_open(&self, token: &str) -> Result<Opened> {
        let token = token.to_string();
        let found = self
            .db
            .call(move |c| {
                Ok(c.query_row("SELECT id, token, file_id, created_at, expires_at, user_id FROM shares WHERE token = ?1", [token], |r| {
                    Ok((row(r)?, r.get::<_, i64>(5)?))
                })
                .optional()?)
            })
            .await?;
        let (share, owner) = found.ok_or(Error::NotFound)?;
        if share.expires_at.is_some_and(|t| t <= now()) {
            return Err(Error::Gone);
        }
        let root = self.entry(owner, share.file_id).await?;
        Ok(Opened { owner, share, root })
    }

    /// An entry reachable through a share: the shared item itself, or anything live below it.
    pub async fn share_entry(&self, opened: &Opened, id: Option<i64>) -> Result<(Entry, Vec<Entry>)> {
        let id = id.unwrap_or(opened.root.id);
        let chain = self.live_chain(opened.owner, id).await?;
        let Some(at) = chain.iter().position(|e| e.id == opened.root.id) else {
            return Err(Error::NotFound);
        };
        let inside = chain[at..].to_vec();
        let entry = inside.last().cloned().ok_or(Error::NotFound)?;
        Ok((entry, inside))
    }

    pub async fn share_children(&self, opened: &Opened, folder: i64) -> Result<Vec<Entry>> {
        let mut children = index::children(&self.db, opened.owner, Some(folder)).await?;
        children.retain(|e| !super::is_junk(&e.name));
        Ok(children)
    }

    /// Drops links that expired more than a day ago.
    pub async fn shares_expire(&self) -> Result<usize> {
        let before = now() - 86_400;
        self.db.call(move |c| Ok(c.execute("DELETE FROM shares WHERE expires_at IS NOT NULL AND expires_at < ?1", [before])?)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use home_core::{Db, accounts};

    fn bytes(b: &'static [u8]) -> impl futures_util::Stream<Item = std::result::Result<axum::body::Bytes, std::io::Error>> + Unpin {
        futures_util::stream::iter([Ok(axum::body::Bytes::from_static(b))])
    }

    #[tokio::test]
    async fn share_scope_expiry_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().await.unwrap();
        db.migrate("home-core", home_core::MIGRATIONS).await.unwrap();
        db.migrate("dekave", crate::MIGRATIONS).await.unwrap();
        let uid = accounts::create(&db, "alice", "password1", true).await.unwrap().id;
        let other = accounts::create(&db, "bob", "password1", false).await.unwrap().id;
        let store = Store::new(db.clone(), dir.path().join("users"));

        let album = store.create_folder(uid, None, "Album").await.unwrap();
        let inner = store.create_folder(uid, Some(album.id), "Inner").await.unwrap();
        let photo = store.create_file(uid, Some(inner.id), "p.jpg", bytes(b"x")).await.unwrap();
        let secret = store.create_file(uid, None, "secret.txt", bytes(b"s")).await.unwrap();

        assert!(matches!(store.share_create(other, album.id, None).await, Err(Error::NotFound)), "only the owner shares");
        assert!(matches!(store.share_create(uid, album.id, Some(0)).await, Err(Error::BadRequest(_))));
        let share = store.share_create(uid, album.id, Some(3600)).await.unwrap();
        assert!(share.token.len() >= 40);

        let opened = store.share_open(&share.token).await.unwrap();
        assert_eq!(opened.root.id, album.id);
        let (e, crumbs) = store.share_entry(&opened, Some(photo.id)).await.unwrap();
        assert_eq!(e.name, "p.jpg");
        assert_eq!(crumbs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Album", "Inner", "p.jpg"]);
        assert!(matches!(store.share_entry(&opened, Some(secret.id)).await, Err(Error::NotFound)), "nothing outside the shared folder");
        assert_eq!(store.share_children(&opened, album.id).await.unwrap().len(), 1);

        // Trashing something inside hides it from the link; trashing the root hides the link.
        store.trash(uid, inner.id).await.unwrap();
        assert!(matches!(store.share_entry(&opened, Some(photo.id)).await, Err(Error::NotFound)));
        store.trash(uid, album.id).await.unwrap();
        assert!(matches!(store.share_open(&share.token).await, Err(Error::NotFound)));
        assert!(store.share_list(uid, None).await.unwrap().is_empty());
        store.restore(uid, album.id).await.unwrap();
        assert_eq!(store.share_list(uid, Some(album.id)).await.unwrap().len(), 1);

        assert!(matches!(store.share_open("nope").await, Err(Error::NotFound)));
        db.call(move |c| Ok(c.execute("UPDATE shares SET expires_at = ?1", [now() - 5])?)).await.unwrap();
        assert!(matches!(store.share_open(&share.token).await, Err(Error::Gone)));
        assert_eq!(store.shares_expire().await.unwrap(), 0, "kept for a day after expiry");

        let forever = store.share_create(uid, secret.id, None).await.unwrap();
        assert!(matches!(store.share_revoke(other, forever.id).await, Err(Error::NotFound)));
        store.share_revoke(uid, forever.id).await.unwrap();
        assert!(matches!(store.share_open(&forever.token).await, Err(Error::NotFound)));

        // Purging the item takes its links with it.
        let gone = store.share_create(uid, secret.id, None).await.unwrap();
        store.trash(uid, secret.id).await.unwrap();
        store.purge(uid, secret.id).await.unwrap();
        assert!(matches!(store.share_open(&gone.token).await, Err(Error::NotFound)));
    }
}
