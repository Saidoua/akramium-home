//! `home.toml`: one file for the daemon and all its modules. Environment variables
//! `HOME_CONFIG`, `HOME_LISTEN` and `HOME_DATA_DIR` override the file (Docker needs that).

use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Address to listen on. Loopback by default; `0.0.0.0:11720` to serve the household.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Where everything lives: the database, the files, the caches.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Host names the daemon answers to besides IP literals and localhost (DNS rebinding guard).
    #[serde(default = "default_host_names")]
    pub host_names: Vec<String>,
    #[serde(default)]
    pub modules: Modules,
    #[serde(default)]
    pub limits: Limits,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Modules {
    #[serde(default = "yes")]
    pub dekave: bool,
    #[serde(default)]
    pub doks: bool,
    #[serde(default)]
    pub komail: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Largest single request body outside uploads, in bytes.
    #[serde(default = "default_body_limit")]
    pub body_bytes: usize,
    /// Days a session cookie stays valid.
    #[serde(default = "default_session_days")]
    pub session_days: i64,
}

fn yes() -> bool {
    true
}
fn default_listen() -> SocketAddr {
    "127.0.0.1:11720".parse().unwrap()
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}
fn default_host_names() -> Vec<String> {
    vec!["akramium.local".to_string()]
}
fn default_body_limit() -> usize {
    1 << 20
}
fn default_session_days() -> i64 {
    30
}

impl Default for Modules {
    fn default() -> Self {
        Self { dekave: true, doks: false, komail: false }
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self { body_bytes: default_body_limit(), session_days: default_session_days() }
    }
}
impl Default for Config {
    fn default() -> Self {
        toml::from_str("").expect("empty config is valid")
    }
}

impl Config {
    /// Reads the file when it exists, then applies the environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let path = path
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("HOME_CONFIG").map(PathBuf::from));
        let mut config = match path {
            Some(p) if p.exists() => {
                let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                toml::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?
            }
            Some(p) => return Err(format!("{}: no such file", p.display())),
            None => Config::default(),
        };
        if let Ok(v) = std::env::var("HOME_LISTEN") {
            config.listen = v.parse().map_err(|e| format!("HOME_LISTEN: {e}"))?;
        }
        if let Some(v) = std::env::var_os("HOME_DATA_DIR") {
            config.data_dir = PathBuf::from(v);
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.listen.port(), 11720);
        assert!(c.modules.dekave);
        assert!(!c.modules.komail);
    }

    #[test]
    fn parses_modules() {
        let c: Config = toml::from_str("listen = \"0.0.0.0:80\"\n[modules]\ndoks = true\n").unwrap();
        assert_eq!(c.listen.port(), 80);
        assert!(c.modules.doks);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("colour = 1").is_err());
    }
}
