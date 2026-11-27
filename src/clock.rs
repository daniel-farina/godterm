use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const MIN_SLEEP: Duration = Duration::from_secs(2);

struct Clocks {
    last: Option<(Instant, SystemTime)>,

    sleeps: Vec<(Instant, Duration)>,
}

static CLOCKS: Mutex<Clocks> = Mutex::new(Clocks {
    last: None,
    sleeps: Vec::new(),
});

pub fn observe() {
    let (m, w) = (Instant::now(), SystemTime::now());
    let mut c = CLOCKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((m0, w0)) = c.last {
        let dm = m.saturating_duration_since(m0);

        let dw = w.duration_since(w0).unwrap_or_default();
        if dw > dm + MIN_SLEEP {
            c.sleeps.push((m, dw - dm));

            if c.sleeps.len() > 64 {
                c.sleeps.remove(0);
            }
        }
    }
    c.last = Some((m, w));
}

pub fn slept_since(t: Instant) -> Duration {
    observe();
    let c = CLOCKS.lock().unwrap_or_else(|e| e.into_inner());
    c.sleeps
        .iter()
        .filter(|(at, _)| *at >= t)
        .map(|(_, d)| *d)
        .sum()
}

pub fn age(t: Instant) -> Duration {
    t.elapsed() + slept_since(t)
}

pub fn ago(d: Duration) -> Instant {
    let now = Instant::now();
    now.checked_sub(d).unwrap_or(now)
}

pub fn past(d: Duration) -> Instant {
    let now = Instant::now();
    let mut d = d;
    loop {
        if let Some(t) = now.checked_sub(d) {
            return t;
        }
        d /= 2;
    }
}

#[cfg(test)]
pub fn exactly_ago(d: Duration) -> Option<Instant> {
    let t = Instant::now().checked_sub(d);
    if t.is_none() {
        eprintln!(
            "this machine's clock does not reach {d:?} back; the steps that need it are skipped"
        );
    }
    t
}

#[cfg(test)]
macro_rules! ago_or_return {
    ($d:expr) => {
        match $crate::clock::exactly_ago($d) {
            Some(t) => t,
            None => return,
        }
    };
}

#[cfg(test)]
pub(crate) use ago_or_return;

#[cfg(test)]
pub fn simulate_sleep(d: Duration) {
    let mut c = CLOCKS.lock().unwrap_or_else(|e| e.into_inner());
    c.sleeps.push((Instant::now(), d));
}

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

    #[test]
    fn the_past_is_as_far_back_as_the_clock_goes() {
        let t = past(Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_secs(3600));
        let far = past(Duration::MAX);
        assert!(far <= Instant::now());
        match exactly_ago(Duration::from_secs(2)) {
            Some(t) => assert!(t.elapsed() >= Duration::from_secs(2)),
            None => assert!(past(Duration::from_secs(2)).elapsed() < Duration::from_secs(2)),
        }
    }
}
