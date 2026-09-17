//! Slows password guessing. Failures are counted per client address and per account name;
//! past a threshold each further failure doubles the wait, up to fifteen minutes. A success
//! clears the address's count.
//!
//! The name threshold is deliberately high: a mounted drive retrying an old password, or
//! someone in the house typing another person's name wrong, must not lock that person out.
//! It only blunts guessing spread over many addresses.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FREE_FAILURES_PER_ADDRESS: u32 = 5;
const FREE_FAILURES_PER_NAME: u32 = 20;
const FIRST_WAIT: Duration = Duration::from_secs(30);
const LONGEST_WAIT: Duration = Duration::from_secs(900);
const FORGET_AFTER: Duration = Duration::from_secs(3600);
const MAX_BUCKETS: usize = 10_000;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    failures: u32,
    blocked_until: Option<Instant>,
    last: Instant,
}

#[derive(Clone, Default)]
pub struct LoginLimit {
    buckets: Arc<Mutex<HashMap<String, Bucket>>>,
}

fn keys(address: Option<IpAddr>, name: &str) -> [(String, u32); 2] {
    let address = address.map(|a| a.to_string()).unwrap_or_else(|| "unknown".into());
    [(format!("address:{address}"), FREE_FAILURES_PER_ADDRESS), (format!("name:{}", name.to_lowercase()), FREE_FAILURES_PER_NAME)]
}

fn wait_after(failures: u32, free: u32) -> Option<Duration> {
    if failures < free {
        return None;
    }
    let doublings = (failures - free).min(10);
    Some((FIRST_WAIT * 2u32.pow(doublings)).min(LONGEST_WAIT))
}

impl LoginLimit {
    /// `Err(seconds)` while this address or this name has to wait.
    pub fn check(&self, address: Option<IpAddr>, name: &str) -> Result<(), u64> {
        self.check_at(address, name, Instant::now())
    }

    pub fn failed(&self, address: Option<IpAddr>, name: &str) {
        self.failed_at(address, name, Instant::now());
    }

    pub fn succeeded(&self, address: Option<IpAddr>, name: &str) {
        if let Ok(mut map) = self.buckets.lock() {
            // Only the address: a success from one place says nothing about guesses from others.
            map.remove(&keys(address, name)[0].0);
        }
    }

    fn check_at(&self, address: Option<IpAddr>, name: &str, now: Instant) -> Result<(), u64> {
        let Ok(map) = self.buckets.lock() else { return Ok(()) };
        let wait = keys(address, name)
            .iter()
            .filter_map(|(k, _)| map.get(k)?.blocked_until)
            .filter(|until| *until > now)
            .map(|until| until - now)
            .max();
        match wait {
            Some(w) => Err(w.as_secs().max(1)),
            None => Ok(()),
        }
    }

    fn failed_at(&self, address: Option<IpAddr>, name: &str, now: Instant) {
        let Ok(mut map) = self.buckets.lock() else { return };
        if map.len() >= MAX_BUCKETS {
            map.retain(|_, b| now.duration_since(b.last) < FORGET_AFTER);
        }
        for (key, free) in keys(address, name) {
            let bucket = map.entry(key).or_insert(Bucket { failures: 0, blocked_until: None, last: now });
            if now.duration_since(bucket.last) >= FORGET_AFTER {
                bucket.failures = 0;
            }
            bucket.failures += 1;
            bucket.last = now;
            bucket.blocked_until = wait_after(bucket.failures, free).map(|w| now + w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn locks_after_five_and_doubles() {
        let limit = LoginLimit::default();
        let t = Instant::now();
        for _ in 0..4 {
            limit.failed_at(ip("10.0.0.9"), "alice", t);
        }
        assert!(limit.check_at(ip("10.0.0.9"), "alice", t).is_ok(), "four failures are free");
        limit.failed_at(ip("10.0.0.9"), "alice", t);
        assert_eq!(limit.check_at(ip("10.0.0.9"), "alice", t), Err(30));
        assert!(limit.check_at(ip("10.0.0.9"), "bob", t).is_err(), "the address waits whatever name it tries");
        assert!(limit.check_at(ip("10.0.0.7"), "alice", t).is_ok(), "alice can still sign in from elsewhere");

        let later = t + Duration::from_secs(31);
        assert!(limit.check_at(ip("10.0.0.9"), "alice", later).is_ok());
        limit.failed_at(ip("10.0.0.9"), "alice", later);
        assert_eq!(limit.check_at(ip("10.0.0.9"), "alice", later), Err(60));
        for _ in 0..20 {
            limit.failed_at(ip("10.0.0.9"), "x", later);
        }
        assert_eq!(limit.check_at(ip("10.0.0.9"), "x", later), Err(900), "never longer than fifteen minutes");
    }

    #[test]
    fn success_clears_the_address() {
        let limit = LoginLimit::default();
        for _ in 0..5 {
            limit.failed(ip("10.0.0.9"), "alice");
        }
        assert!(limit.check(ip("10.0.0.9"), "alice").is_err());
        limit.succeeded(ip("10.0.0.9"), "alice");
        assert!(limit.check(ip("10.0.0.9"), "alice").is_ok());
    }

    #[test]
    fn a_name_guessed_from_many_addresses_waits_too() {
        let limit = LoginLimit::default();
        let t = Instant::now();
        for i in 0..20 {
            limit.failed_at(ip(&format!("10.0.1.{i}")), "Alice", t);
        }
        assert!(limit.check_at(ip("10.0.2.1"), "alice", t).is_err(), "names compare without case");
        assert!(limit.check_at(ip("10.0.2.1"), "bob", t).is_ok());
    }

    #[test]
    fn old_failures_are_forgotten() {
        let limit = LoginLimit::default();
        let t = Instant::now();
        for _ in 0..4 {
            limit.failed_at(ip("10.0.0.9"), "alice", t);
        }
        limit.failed_at(ip("10.0.0.9"), "alice", t + FORGET_AFTER);
        assert!(limit.check_at(ip("10.0.0.9"), "alice", t + FORGET_AFTER).is_ok(), "an hour later the count starts again");
    }
}
