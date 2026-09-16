//! First run: no accounts exist, so the daemon prints a one-time link that creates the admin.
//! The link is also written to `data_dir/setup-link` for installs where nobody reads the log.

use crate::{Config, Db, Result, accounts};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Setup {
    token: Arc<Mutex<Option<String>>>,
}

impl Setup {
    pub async fn prepare(db: &Db, config: &Config) -> Result<Self> {
        let link_file = config.data_dir.join("setup-link");
        if accounts::count(db).await? > 0 {
            let _ = std::fs::remove_file(&link_file);
            return Ok(Self { token: Arc::new(Mutex::new(None)) });
        }
        let token = crate::session::random_token();
        let host = if config.listen.ip().is_unspecified() { "localhost".to_string() } else { config.listen.ip().to_string() };
        let link = format!("http://{host}:{}/setup?token={token}", config.listen.port());
        std::fs::write(&link_file, format!("{link}\n"))?;
        tracing::info!("no accounts yet. Open this link once to create the admin:\n\n    {link}\n");
        Ok(Self { token: Arc::new(Mutex::new(Some(token))) })
    }

    /// True while the admin has not been created.
    pub fn pending(&self) -> bool {
        self.token.lock().map(|t| t.is_some()).unwrap_or(false)
    }

    /// Consumes the token if it matches. A second call with the same token fails.
    pub fn claim(&self, token: &str) -> bool {
        let mut guard = match self.token.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let ok = guard.as_deref().is_some_and(|t| constant_eq(t, token));
        if ok {
            *guard = None;
        }
        ok
    }

    /// Checks without consuming (for showing the form).
    pub fn matches(&self, token: &str) -> bool {
        self.token.lock().map(|g| g.as_deref().is_some_and(|t| constant_eq(t, token))).unwrap_or(false)
    }

    pub fn finish(&self, data_dir: &std::path::Path) {
        let _ = std::fs::remove_file(data_dir.join("setup-link"));
    }
}

fn constant_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_is_single_use() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().await.unwrap();
        db.migrate("home-core", crate::MIGRATIONS).await.unwrap();
        let mut config = Config::default();
        config.data_dir = dir.path().to_path_buf();
        let setup = Setup::prepare(&db, &config).await.unwrap();
        assert!(setup.pending());
        let link = std::fs::read_to_string(dir.path().join("setup-link")).unwrap();
        let token = link.trim().rsplit("token=").next().unwrap().to_string();
        assert!(!setup.claim("wrong"));
        assert!(setup.claim(&token));
        assert!(!setup.claim(&token));
        assert!(!setup.pending());
    }
}
