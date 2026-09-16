//! One SQLite connection behind a mutex, used from async code through `spawn_blocking`.
//! A household daemon never has enough concurrent writers for a pool to matter, and one
//! connection in WAL mode keeps every read consistent with the last write.

use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub async fn open(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
            let conn = Connection::open(&path)?;
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE IF NOT EXISTS migrations (
                     module TEXT NOT NULL,
                     name TEXT NOT NULL,
                     applied_at INTEGER NOT NULL,
                     PRIMARY KEY (module, name)
                 );",
            )?;
            Ok(conn)
        })
        .await??;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// In-memory database for tests.
    pub async fn open_memory() -> Result<Self> {
        Self::open(Path::new(":memory:")).await
    }

    /// Runs `f` with the connection on a blocking thread.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| Error::Internal("database lock poisoned".into()))?;
            f(&mut guard)
        })
        .await?
    }

    /// Applies each `(name, sql)` pair once, in order, recording it under `module`.
    pub async fn migrate(&self, module: &'static str, migrations: &'static [(&'static str, &'static str)]) -> Result<()> {
        self.call(move |conn| {
            for (name, sql) in migrations {
                let done: bool = conn.query_row(
                    "SELECT COUNT(*) FROM migrations WHERE module = ?1 AND name = ?2",
                    (module, name),
                    |r| r.get::<_, i64>(0).map(|n| n > 0),
                )?;
                if done {
                    continue;
                }
                let tx = conn.transaction()?;
                tx.execute_batch(sql)?;
                tx.execute(
                    "INSERT INTO migrations (module, name, applied_at) VALUES (?1, ?2, ?3)",
                    (module, name, crate::now()),
                )?;
                tx.commit()?;
                tracing::info!(module, name, "migration applied");
            }
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_once() {
        let db = Db::open_memory().await.unwrap();
        const M: &[(&str, &str)] = &[("0001", "CREATE TABLE t (x INTEGER);")];
        db.migrate("test", M).await.unwrap();
        db.migrate("test", M).await.unwrap();
        let n: i64 = db
            .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM migrations", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 1);
    }
}
