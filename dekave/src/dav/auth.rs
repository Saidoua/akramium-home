//! HTTP Basic for WebDAV clients. `/dav` takes nothing else: a page in a browser cannot ride
//! the session cookie into it. A verified name and password is remembered for five minutes,
//! because clients authenticate every request and one Argon2 check costs 19 MiB.

use axum::http::HeaderValue;
use base64::Engine;
use home_core::accounts::{self, User};
use home_core::{Core, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const REMEMBER: Duration = Duration::from_secs(300);
const MAX_REMEMBERED: usize = 1024;

#[derive(Clone, Default)]
pub struct BasicAuth {
    seen: Arc<Mutex<HashMap<[u8; 32], (i64, Instant)>>>,
}

/// `name` and `password` from an `Authorization: Basic …` header.
pub fn parse(header: &HeaderValue) -> Option<(String, String)> {
    let value = header.to_str().ok()?;
    let (scheme, rest) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(rest.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (name, password) = text.split_once(':')?;
    Some((name.to_string(), password.to_string()))
}

impl BasicAuth {
    /// `Ok(None)`: no or wrong credentials. `Err(TooMany)`: this address or name must wait.
    pub async fn check(&self, core: &Core, header: Option<&HeaderValue>, address: Option<std::net::IpAddr>) -> Result<Option<User>> {
        let Some((name, password)) = header.and_then(parse) else {
            return Ok(None);
        };
        let key: [u8; 32] = Sha256::new().chain_update(name.as_bytes()).chain_update([0]).chain_update(password.as_bytes()).finalize().into();

        let remembered = self.seen.lock().ok().and_then(|m| m.get(&key).copied()).filter(|(_, at)| at.elapsed() < REMEMBER);
        if let Some((user_id, _)) = remembered {
            // The password is known good; the account may have been disabled since.
            return Ok(accounts::by_id(&core.db, user_id).await?.filter(|u| !u.disabled));
        }
        // Only guesses wait. A mounted drive with a remembered good password keeps working
        // while someone else at the same address is locked out.
        core.limit.check(address, &name).map_err(home_core::Error::TooMany)?;
        let user = accounts::authenticate(&core.db, &name, &password).await?;
        match &user {
            Some(_) => core.limit.succeeded(address, &name),
            None => core.limit.failed(address, &name),
        }
        if let (Some(u), Ok(mut map)) = (&user, self.seen.lock()) {
            if map.len() >= MAX_REMEMBERED {
                map.clear();
            }
            map.insert(key, (u.id, Instant::now()));
        }
        Ok(user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic() {
        let h = HeaderValue::from_static("Basic YWxpY2U6cGE6c3M="); // alice:pa:ss
        assert_eq!(parse(&h), Some(("alice".into(), "pa:ss".into())));
        assert_eq!(parse(&HeaderValue::from_static("Bearer abc")), None);
        assert_eq!(parse(&HeaderValue::from_static("Basic !!!")), None);
    }

    #[tokio::test]
    async fn remembers_and_respects_disable() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = home_core::Config::default();
        config.data_dir = dir.path().to_path_buf();
        let core = Core::open(config).await.unwrap();
        let alice = accounts::create(&core.db, "alice", "password1", true).await.unwrap();
        let auth = BasicAuth::default();
        let good = HeaderValue::from_static("Basic YWxpY2U6cGFzc3dvcmQx"); // alice:password1
        let bad = HeaderValue::from_static("Basic YWxpY2U6d3Jvbmc="); // alice:wrong

        assert!(auth.check(&core, None, None).await.unwrap().is_none());
        assert!(auth.check(&core, Some(&bad), None).await.unwrap().is_none());
        assert_eq!(auth.check(&core, Some(&good), None).await.unwrap().unwrap().id, alice.id);
        let started = Instant::now();
        assert!(auth.check(&core, Some(&good), None).await.unwrap().is_some());
        assert!(started.elapsed() < Duration::from_millis(15), "the second check skips Argon2");
        accounts::set_disabled(&core.db, alice.id, true).await.unwrap();
        assert!(auth.check(&core, Some(&good), None).await.unwrap().is_none(), "a remembered password does not outlive a disable");
    }
}
