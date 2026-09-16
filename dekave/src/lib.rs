//! DeKave: the household's drive. Real files under `data_dir/users/<id>/files/`, an SQLite
//! index for ids, sizes and hashes, and a web UI served at `/drive/`.

pub mod api;
pub mod store;

use axum::Router;
use home_core::Core;

pub use store::Store;

pub const MIGRATIONS: &[(&str, &str)] = &[(
    "0001-files",
    "CREATE TABLE files (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        parent_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        is_dir INTEGER NOT NULL DEFAULT 0,
        size INTEGER NOT NULL DEFAULT 0,
        mtime INTEGER NOT NULL,
        hash TEXT,
        mime TEXT,
        created_at INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX files_unique_name ON files(user_id, IFNULL(parent_id, 0), name);
    CREATE INDEX files_parent ON files(user_id, parent_id);",
)];

/// Everything DeKave's handlers reach.
#[derive(Clone)]
pub struct Drive {
    pub core: Core,
    pub store: Store,
}

impl axum::extract::FromRef<Drive> for Core {
    fn from_ref(d: &Drive) -> Core {
        d.core.clone()
    }
}

pub async fn open(core: Core) -> home_core::Result<Drive> {
    core.db.migrate("dekave", MIGRATIONS).await?;
    let store = Store::new(core.db.clone(), core.config.data_dir.join("users"));
    Ok(Drive { core, store })
}

/// Routes under `/drive` and `/api/drive`.
pub fn router(drive: Drive) -> Router<Core> {
    api::router().with_state(drive)
}
