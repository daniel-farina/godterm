//! macOS notifications and a small rate limiter shared with spoken alerts.

use std::collections::HashMap;
use std::hash::Hash;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Allows an event at most once per `per_key` for the same key and at most
/// once per `gap` overall.
pub struct Limiter<K> {
    per_key: Duration,
    gap: Duration,
    seen: HashMap<K, Instant>,
    last: Option<Instant>,
}

impl<K: Hash + Eq> Limiter<K> {
    pub fn new(per_key: Duration, gap: Duration) -> Self {
        Limiter {
            per_key,
            gap,
            seen: HashMap::new(),
            last: None,
        }
    }

    pub fn set_per_key(&mut self, d: Duration) {
        self.per_key = d;
    }

    pub fn allow_at(&mut self, key: K, now: Instant) -> bool {
        if self.last.is_some_and(|t| now.duration_since(t) < self.gap) {
            return false;
        }
        if self
            .seen
            .get(&key)
            .is_some_and(|t| now.duration_since(*t) < self.per_key)
        {
            return false;
        }
        self.seen.insert(key, now);
        self.last = Some(now);
        true
    }

    pub fn allow(&mut self, key: K) -> bool {
        self.allow_at(key, Instant::now())
    }
}

/// AppleScript string literal body.
pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Post a notification through `osascript` without blocking.
pub fn post(title: &str, message: &str) {
    if cfg!(test) || !cfg!(target_os = "macos") {
        return;
    }
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escape(message),
        escape(title)
    );
    std::thread::spawn(move || {
        let _ = Command::new("osascript")
            .args(["-e", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter() {
        let mut l = Limiter::new(Duration::from_secs(60), Duration::from_secs(5));
        let t0 = Instant::now();
        assert!(l.allow_at("a", t0));
        assert!(!l.allow_at("b", t0 + Duration::from_secs(1)), "global gap");
        assert!(l.allow_at("b", t0 + Duration::from_secs(6)));
        assert!(
            !l.allow_at("a", t0 + Duration::from_secs(30)),
            "same key too soon"
        );
        assert!(l.allow_at("a", t0 + Duration::from_secs(61)));
    }

    #[test]
    fn escapes() {
        assert_eq!(escape(r#"say "hi" \ ok"#), r#"say \"hi\" \\ ok"#);
    }
}
