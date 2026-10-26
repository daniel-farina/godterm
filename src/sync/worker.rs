//! The background sync (docs/SYNC.md 5): two threads, never the UI
//! thread. A watcher looks at what the sync covers every two seconds
//! (GodTerm's state files, the transcripts of open tabs, project .env
//! files every minute) and, after `debounce_s` of quiet following a
//! change (a turn ending is its transcript going quiet), hands a sync to
//! the runner. The runner syncs; it retries with backoff from 30 s up to
//! 10 min while offline or failing, and fetches every two minutes
//! anyway. Only GodTerm's own state and conversation repos are touched.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::state_repo::Report;
use super::Ctx;

/// What the app and the tools see.
#[derive(Debug, Clone, Default)]
pub struct Shared {
    pub running: bool,
    pub last: Option<Report>,
    /// Bumped when a sync wrote into GodTerm's home (the app reloads).
    pub applied: u64,
    /// Seconds until the next retry, when failing.
    pub retry_in: Option<u64>,
    pub last_reason: String,
    pub changes_seen: u64,
}

pub struct Worker {
    tx: Sender<&'static str>,
    pub shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    /// Start both threads. `make` builds a fresh context per sync (the
    /// config may have changed).
    pub fn start(make: Arc<dyn Fn() -> anyhow::Result<Ctx> + Send + Sync>) -> Worker {
        let (tx, rx) = channel::<&'static str>();
        let shared = Arc::new(Mutex::new(Shared {
            last: make().ok().and_then(|c| Report::load(&c)),
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let w = Worker {
            tx: tx.clone(),
            shared: shared.clone(),
            stop: stop.clone(),
        };
        let (m2, s2, st2) = (make.clone(), shared.clone(), stop.clone());
        let _ = std::thread::Builder::new()
            .name("sync-run".into())
            .spawn(move || run_loop(m2, rx, s2, st2));
        let _ = std::thread::Builder::new()
            .name("sync-watch".into())
            .spawn(move || watch_loop(make, tx, shared, stop));
        w
    }

    /// Sync soon (a turn ended, a change, the user asked).
    pub fn poke(&self, why: &'static str) {
        let _ = self.tx.send(why);
    }

    pub fn snapshot(&self) -> Shared {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send("quit");
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_loop(
    make: Arc<dyn Fn() -> anyhow::Result<Ctx> + Send + Sync>,
    rx: Receiver<&'static str>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) {
    let mut backoff = 0u64;
    let mut next_periodic = Instant::now() + Duration::from_secs(10);
    loop {
        let wait = next_periodic.saturating_duration_since(Instant::now());
        let why = match rx.recv_timeout(wait) {
            Ok("quit") => return,
            Ok(w) => w,
            Err(RecvTimeoutError::Timeout) => "periodic",
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // Pokes that piled up while syncing count once.
        while rx.try_recv().is_ok() {}
        let Ok(ctx) = make() else {
            next_periodic = Instant::now() + Duration::from_secs(60);
            continue;
        };
        if !ctx.sync().enabled {
            next_periodic = Instant::now() + Duration::from_secs(60);
            continue;
        }
        {
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            s.running = true;
            s.last_reason = why.to_string();
        }
        let rep = super::state_repo::sync_now(&ctx).unwrap_or_else(|e| Report {
            error: Some(format!("{e:#}")),
            at_ms: ctx.now_ms,
            ..Default::default()
        });
        let failed = !rep.ok;
        let more = rep.more;
        if !rep.applied.is_empty() || !rep.forked.is_empty() || rep.extended > 0 {
            crate::log::info(&format!("sync: applied {}", rep.applied.join(", ")));
        }
        if let Some(e) = &rep.error {
            crate::log::info(&format!("sync: {e}"));
        }
        let wrote = !rep.applied.is_empty();
        backoff = if failed {
            (backoff * 2).clamp(30, 600)
        } else {
            0
        };
        {
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            s.running = false;
            if wrote {
                s.applied += 1;
            }
            s.retry_in = failed.then_some(backoff);
            s.last = Some(rep);
        }
        let hb = ctx.sync().heartbeat_s.clamp(30, 3600);
        next_periodic = Instant::now()
            + Duration::from_secs(if more {
                1
            } else if failed {
                backoff
            } else {
                hb
            });
    }
}

/// (path, len, mtime) of everything watched.
fn signature(ctx: &Ctx, env_roots: &[PathBuf]) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut v = vec![];
    let mut add = |p: PathBuf| {
        if let Ok(m) = std::fs::metadata(&p) {
            v.push((p, m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
        }
    };
    for f in [
        "config.toml",
        "state.json",
        "archive.json",
        "jobs.json",
        "projects.json",
        "assistant/learned.json",
        "git-bindings.json",
    ] {
        add(ctx.app_home.join(f));
    }
    // The transcripts of open tabs.
    let st: serde_json::Value = std::fs::read(ctx.app_home.join("state.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let accts = ctx.accounts();
    for s in st["slots"].as_array().into_iter().flatten() {
        let Some(a) = accts
            .iter()
            .find(|a| Some(a.name.as_str()) == s["account"].as_str())
        else {
            continue;
        };
        for t in s["tabs"].as_array().into_iter().flatten() {
            let (Some(cwd), Some(id)) = (t["cwd"].as_str(), t["session_id"].as_str()) else {
                continue;
            };
            if a.provider == "claude" {
                add(a
                    .dir
                    .join("projects")
                    .join(crate::state::encode_project_dir(std::path::Path::new(cwd)))
                    .join(format!("{id}.jsonl")));
            }
        }
    }
    for r in env_roots {
        for (_, p) in super::env::env_files(r, &ctx.never) {
            add(p);
        }
    }
    v
}

fn watch_loop(
    make: Arc<dyn Fn() -> anyhow::Result<Ctx> + Send + Sync>,
    tx: Sender<&'static str>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) {
    let mut last: Option<Vec<(PathBuf, u64, SystemTime)>> = None;
    let mut dirty_at: Option<Instant> = None;
    let mut env_roots: Vec<PathBuf> = vec![];
    let mut roots_at = Instant::now() - Duration::from_secs(3600);
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(2));
        let Ok(ctx) = make() else { continue };
        if !ctx.sync().enabled {
            continue;
        }
        if roots_at.elapsed() > Duration::from_secs(60) {
            env_roots = super::env::project_roots(&ctx);
            roots_at = Instant::now();
        }
        let sig = signature(&ctx, &env_roots);
        if last.as_ref().is_some_and(|l| *l != sig) {
            dirty_at = Some(Instant::now());
            shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .changes_seen += 1;
        }
        last = Some(sig);
        let quiet = Duration::from_secs(ctx.sync().debounce_s.max(1));
        if dirty_at.is_some_and(|t| t.elapsed() >= quiet) {
            dirty_at = None;
            let _ = tx.send("change");
        }
    }
}
