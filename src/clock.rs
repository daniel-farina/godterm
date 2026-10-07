//! Time spent asleep. `Instant` on macOS does not advance while the Mac
//! sleeps, so a window measured with it alone ("answerable for two
//! minutes", "paused for five minutes") survives a night with the lid
//! closed and resumes where it left off. The wall clock does advance: when
//! it runs ahead of `Instant` by more than a little, the difference is a
//! sleep, recorded at the moment it was noticed. `age` adds the sleeps
//! after an instant to its elapsed time.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// A drift smaller than this is clock noise, not a sleep.
const MIN_SLEEP: Duration = Duration::from_secs(2);

struct Clocks {
    last: Option<(Instant, SystemTime)>,
    /// (noticed at, how long it slept), oldest first.
    sleeps: Vec<(Instant, Duration)>,
}

static CLOCKS: Mutex<Clocks> = Mutex::new(Clocks {
    last: None,
    sleeps: Vec::new(),
});

/// Compare the two clocks and record a sleep if the wall clock ran ahead.
/// Called every tick (and by `age`), so a sleep is noticed right after
/// the wake.
pub fn observe() {
    let (m, w) = (Instant::now(), SystemTime::now());
    let mut c = CLOCKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((m0, w0)) = c.last {
        let dm = m.saturating_duration_since(m0);
        // A wall clock set back is not a sleep.
        let dw = w.duration_since(w0).unwrap_or_default();
        if dw > dm + MIN_SLEEP {
            c.sleeps.push((m, dw - dm));
            // Old sleeps matter only to old instants; keep a few.
            if c.sleeps.len() > 64 {
                c.sleeps.remove(0);
            }
        }
    }
    c.last = Some((m, w));
}

/// Time slept since `t`.
pub fn slept_since(t: Instant) -> Duration {
    observe();
    let c = CLOCKS.lock().unwrap_or_else(|e| e.into_inner());
    c.sleeps
        .iter()
        .filter(|(at, _)| *at >= t)
        .map(|(_, d)| *d)
        .sum()
}

/// How long ago `t` was, sleep included.
pub fn age(t: Instant) -> Duration {
    t.elapsed() + slept_since(t)
}

/// Pretend the Mac just slept for `d` (tests).
#[cfg(test)]
pub fn simulate_sleep(d: Duration) {
    let mut c = CLOCKS.lock().unwrap_or_else(|e| e.into_inner());
    c.sleeps.push((Instant::now(), d));
}

/// Forget simulated sleeps (tests).
#[cfg(test)]
pub fn reset() {
    let mut c = CLOCKS.lock().unwrap_or_else(|e| e.into_inner());
    c.sleeps.clear();
    c.last = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_counts_only_after_the_instant() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset();
        let before = Instant::now();
        assert!(age(before) < Duration::from_secs(1));
        simulate_sleep(Duration::from_secs(3600));
        std::thread::sleep(Duration::from_millis(5));
        let after = Instant::now();
        assert!(age(before) >= Duration::from_secs(3600));
        assert!(age(after) < Duration::from_secs(1));
        reset();
    }
}
