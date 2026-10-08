//! One usage fetch per account, shared by every GodTerm process (the app,
//! `godterm status`, a second window): the last reply and when the next
//! request is allowed live in ~/.godterm/usage/<account>.json, and a lock
//! file keeps two processes from asking at the same moment. A 429 backs
//! off on a ladder (1, 2, 5, 10, 15 minutes), longer when the server's
//! Retry-After says so; Retry-After 0 or none still waits the ladder.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::usage::{parse_usage, Usage, UsageError};

/// What the endpoint said: status, Retry-After seconds, body.
#[derive(Debug, Clone, PartialEq)]
pub struct Raw {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct Entry {
    /// When it was asked (unix seconds).
    at: i64,
    status: u16,
    body: String,
    /// No request before this (unix seconds).
    until: i64,
    /// 429s in a row.
    strikes: u32,
    /// Last good body and when it came.
    good: Option<(i64, String)>,
}

/// Waits after the nth 429 in a row (1-based): 1, 2, 5, 10, 15 minutes.
const LADDER: [u64; 5] = [60, 120, 300, 600, 900];
pub const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// How long to wait after `strikes` 429s in a row, given Retry-After.
pub fn backoff(strikes: u32, retry_after: Option<u64>) -> Duration {
    let i = (strikes.max(1) as usize - 1).min(LADDER.len() - 1);
    let ladder = Duration::from_secs(LADDER[i]);
    let server = Duration::from_secs(retry_after.unwrap_or(0));
    ladder.max(server).min(MAX_BACKOFF)
}

/// Up to a tenth more, so accounts that started together drift apart.
pub fn jitter(d: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    // A fresh random seed per call (the std hasher's keys).
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.as_nanos())
            .unwrap_or(0),
    );
    let r = (h.finish() % 10_000) as f64 / 10_000.0;
    d + d.mul_f64(r / 10.0)
}

pub fn dir() -> PathBuf {
    crate::config::dirs().app_home.join("usage")
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn read(p: &Path) -> Option<Entry> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn write(p: &Path, e: &Entry) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string(e).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

/// The reply as a result.
fn outcome(status: u16, body: &str, retry: Option<u64>) -> Result<Usage, UsageError> {
    match status {
        200..=299 => parse_usage(body),
        401 | 403 => Err(UsageError::Unauthorized),
        429 => Err(UsageError::RateLimited(retry)),
        0 => Err(UsageError::Network(body.to_string())),
        s => Err(UsageError::Http(s)),
    }
}

/// What a remembered entry says now, with no request.
fn from_entry(e: &Entry) -> Result<Usage, UsageError> {
    let left = (e.until - now()).max(0) as u64;
    outcome(e.status, &e.body, Some(left))
}

/// A lock older than this is from a process that died mid request.
const STALE_LOCK: Duration = Duration::from_secs(30);

/// Usage for `account`: the shared reply when it is newer than `min` or a
/// backoff is running, otherwise one request through `net` (only one
/// process at a time asks; the others wait briefly and read its reply).
pub fn fetch(
    account: &str,
    min: Duration,
    net: impl FnOnce() -> Result<Raw, UsageError>,
) -> Result<Usage, UsageError> {
    fetch_in(&dir(), account, min, net)
}

pub fn fetch_in(
    dir: &Path,
    account: &str,
    min: Duration,
    net: impl FnOnce() -> Result<Raw, UsageError>,
) -> Result<Usage, UsageError> {
    let file = dir.join(format!("{account}.json"));
    let lock = dir.join(format!("{account}.lock"));
    let fresh = |e: &Entry| e.at + min.as_secs() as i64 > now() || e.until > now();
    if let Some(e) = read(&file).filter(fresh) {
        return from_entry(&e);
    }
    let _ = std::fs::create_dir_all(dir);
    let mut held = take_lock(&lock);
    if !held {
        // Someone else is asking: their reply, when it comes.
        let t0 = std::time::Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
            if let Some(e) = read(&file).filter(fresh) {
                return from_entry(&e);
            }
            if !lock.exists() {
                held = take_lock(&lock);
                break;
            }
        }
        if !held {
            return Err(UsageError::RateLimited(None));
        }
    }
    let prev = read(&file).unwrap_or_default();
    let raw = net();
    let t = now();
    let mut e = Entry {
        at: t,
        good: prev.good.clone(),
        ..Default::default()
    };
    let r = match raw {
        Ok(raw) => {
            e.status = raw.status;
            e.body = raw.body.clone();
            if raw.status == 429 {
                e.strikes = prev.strikes + 1;
                e.until = t + jitter(backoff(e.strikes, raw.retry_after)).as_secs() as i64;
                crate::log::info(&format!(
                    "usage {account}: 429 (Retry-After {:?}), waiting {}s",
                    raw.retry_after,
                    e.until - t
                ));
            } else if (200..300).contains(&raw.status) {
                e.good = Some((t, raw.body.clone()));
            }
            outcome(
                raw.status,
                &raw.body,
                raw.retry_after.map(|_| (e.until - t).max(0) as u64),
            )
        }
        Err(err) => {
            e.status = 0;
            e.body = err.to_string();
            Err(err)
        }
    };
    write(&file, &e);
    let _ = std::fs::remove_file(&lock);
    r
}

fn take_lock(lock: &Path) -> bool {
    let open = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock)
            .is_ok()
    };
    if open() {
        return true;
    }
    let stale = std::fs::metadata(lock)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > STALE_LOCK);
    if stale {
        let _ = std::fs::remove_file(lock);
        return open();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    /// A fake usage endpoint: answers each request with the next reply.
    fn server(replies: Vec<String>) -> (String, std::thread::JoinHandle<usize>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/oauth/usage", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut n = 0;
            for reply in replies {
                let (mut s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                while r.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                s.write_all(reply.as_bytes()).unwrap();
                n += 1;
            }
            n
        });
        (url, h)
    }

    fn reply(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    const GOOD: &str =
        r#"{"five_hour":{"utilization":20.0,"resets_at":"2099-10-09T17:29:00+00:00"}}"#;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("godterm-ushare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn ladder_and_retry_after() {
        assert_eq!(backoff(1, None), Duration::from_secs(60));
        assert_eq!(backoff(1, Some(0)), Duration::from_secs(60), "0 is not now");
        assert_eq!(backoff(2, Some(0)), Duration::from_secs(120));
        assert_eq!(backoff(3, None), Duration::from_secs(300));
        assert_eq!(backoff(9, None), Duration::from_secs(900), "capped");
        assert_eq!(
            backoff(1, Some(400)),
            Duration::from_secs(400),
            "the server's wait"
        );
        assert_eq!(backoff(1, Some(99_999)), MAX_BACKOFF);
        let j = jitter(Duration::from_secs(100));
        assert!(j >= Duration::from_secs(100) && j <= Duration::from_secs(110));
    }

    #[test]
    fn a_429_without_retry_after_backs_off_and_is_shared() {
        let d = tmp("noretry");
        let (url, h) = server(vec![reply("429 Too Many Requests", "", "")]);
        let r = fetch_in(&d, "a1", Duration::from_secs(60), || {
            crate::usage::fetch_usage_raw(&url, "tok")
        });
        assert!(matches!(r, Err(UsageError::RateLimited(_))), "{r:?}");
        assert_eq!(h.join().unwrap(), 1);
        let e = read(&d.join("a1.json")).unwrap();
        assert_eq!(e.strikes, 1);
        assert!(
            e.until - e.at >= 60,
            "at least a minute: {}",
            e.until - e.at
        );
        // Another process (or the next try) inside the backoff: no request.
        let r = fetch_in(&d, "a1", Duration::from_secs(0), || {
            panic!("asked again during the backoff")
        });
        assert!(
            matches!(r, Err(UsageError::RateLimited(Some(s))) if s >= 50),
            "{r:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn retry_after_zero_waits_the_ladder_and_longer_ones_win() {
        let d = tmp("retry");
        let (url, h) = server(vec![
            reply("429 Too Many Requests", "Retry-After: 0\r\n", ""),
            reply("429 Too Many Requests", "Retry-After: 700\r\n", ""),
            reply("200 OK", "Content-Type: application/json\r\n", GOOD),
        ]);
        let ask = |d: &Path| {
            fetch_in(d, "a2", Duration::from_secs(60), || {
                crate::usage::fetch_usage_raw(&url, "tok")
            })
        };
        assert!(matches!(ask(&d), Err(UsageError::RateLimited(_))));
        let e = read(&d.join("a2.json")).unwrap();
        assert!((60..=66).contains(&(e.until - e.at)), "{}", e.until - e.at);
        // The backoff ran out (as if): the second 429 says 700 s.
        let mut e2 = e.clone();
        e2.until = 0;
        e2.at = 0;
        write(&d.join("a2.json"), &e2);
        assert!(matches!(ask(&d), Err(UsageError::RateLimited(_))));
        let e = read(&d.join("a2.json")).unwrap();
        assert_eq!(e.strikes, 2);
        assert!(
            (700..=770).contains(&(e.until - e.at)),
            "{}",
            e.until - e.at
        );
        // Then a good reply: strikes reset, shared with the next caller.
        let mut e3 = e.clone();
        e3.until = 0;
        e3.at = 0;
        write(&d.join("a2.json"), &e3);
        let u = ask(&d).unwrap();
        assert_eq!(u.get("five_hour").unwrap().left(), 80.0);
        assert_eq!(h.join().unwrap(), 3);
        let again = fetch_in(&d, "a2", Duration::from_secs(60), || {
            panic!("fresh: no request")
        });
        assert!(again.is_ok());
        assert_eq!(read(&d.join("a2.json")).unwrap().strikes, 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn one_request_at_a_time_across_processes() {
        let d = tmp("lock");
        std::fs::create_dir_all(&d).unwrap();
        // Another process holds the lock and then writes its reply.
        std::fs::write(d.join("a3.lock"), "").unwrap();
        let d2 = d.clone();
        let other = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            write(
                &d2.join("a3.json"),
                &Entry {
                    at: now(),
                    status: 200,
                    body: GOOD.into(),
                    ..Default::default()
                },
            );
            let _ = std::fs::remove_file(d2.join("a3.lock"));
        });
        let r = fetch_in(&d, "a3", Duration::from_secs(60), || {
            panic!("it waits instead")
        });
        assert!(r.is_ok(), "{r:?}");
        other.join().unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_app_waits_and_words_it_calmly() {
        let mut st = crate::app::AccountState::default();
        st.record_usage(crate::usage::parse_usage(GOOD), 60);
        let w = st.fetch_wait();
        assert!(
            w >= Duration::from_secs(59) && w <= Duration::from_secs(67),
            "{w:?}"
        );
        // A 429 saying "retry after 0": still a minute, then two.
        st.record_usage(Err(UsageError::RateLimited(Some(0))), 60);
        assert!(st.fetch_wait() >= Duration::from_secs(59));
        st.record_usage(Err(UsageError::RateLimited(None)), 60);
        assert!(st.fetch_wait() >= Duration::from_secs(119));
        let now = chrono::Utc::now();
        // The numbers are recent: nothing to say.
        assert_eq!(st.usage_problem(Duration::from_secs(60), now), None);
        // Old: said calmly, never "retry after 0s".
        st.usage_good_at = Some(now - chrono::Duration::minutes(3));
        let p = st.usage_problem(Duration::from_secs(60), now).unwrap();
        assert_eq!(
            p,
            "usage from 3 min ago (the usage service asked us to slow down)"
        );
        // Other errors still say what happened.
        st.usage_err = Some(UsageError::Http(500));
        assert!(st
            .usage_problem(Duration::from_secs(60), now)
            .unwrap()
            .starts_with("usage API returned HTTP 500, showing data from 3 min ago"));
    }
}
