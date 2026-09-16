//! Shared pieces of Akramium Home. Every module (DeKave, Doks, Komail) builds on these:
//! one config file, one SQLite database, one set of household accounts and sessions,
//! one way of serving embedded pages with the right headers.

pub mod accounts;
pub mod assets;
pub mod config;
pub mod db;
pub mod error;
pub mod headers;
pub mod log;
pub mod names;
pub mod routes;
pub mod session;
pub mod setup;

pub use config::Config;
pub use db::Db;
pub use error::{Error, Result};

use std::sync::Arc;

/// What every request handler can reach. Cheap to clone.
#[derive(Clone)]
pub struct Core {
    pub config: Arc<Config>,
    pub db: Db,
    pub setup: setup::Setup,
}

impl Core {
    pub async fn open(config: Config) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;
        let db = Db::open(&config.data_dir.join("home.db")).await?;
        db.migrate("home-core", MIGRATIONS).await?;
        let setup = setup::Setup::prepare(&db, &config).await?;
        Ok(Self { config: Arc::new(config), db, setup })
    }
}

/// Tables shared by every module.
pub const MIGRATIONS: &[(&str, &str)] = &[(
    "0001-accounts",
    "CREATE TABLE users (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE COLLATE NOCASE,
        password_hash TEXT NOT NULL,
        is_admin INTEGER NOT NULL DEFAULT 0,
        disabled INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE sessions (
        token_hash TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
    );
    CREATE INDEX sessions_user ON sessions(user_id);",
)];

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
