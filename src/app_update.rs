//! Updates in the app: a background checker (at start and every few
//! hours), the status bar chip, the toast, Settings > About, the
//! assistant's mention, and Ctrl-a N: restart into the new version with
//! every tab resumed.

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::App;
use crate::pane::Activity;
use crate::update::{self, Cache, Layout, Method, Release, Staged, Version};

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// Newer, but this install updates another way (or downloads are off).
    Available,
    Downloading(u8),
    /// Verified and waiting for a restart.
    Ready,
    Failed(String),
}

/// What the checker thread found.
#[derive(Debug, Clone, Default)]
pub struct Shared {
    pub phase: Phase,
    pub latest: Option<Release>,
    pub checked_at: Option<chrono::DateTime<chrono::Local>>,
    pub method: Option<Method>,
    pub staged: Option<Staged>,
    /// The releases newer than this build, newest first, with notes.
    pub between: Vec<Release>,
}

#[derive(Default)]
pub struct UpdateUi {
    pub shared: Arc<Mutex<Shared>>,
    pub(crate) kick: Option<mpsc::Sender<()>>,
    /// The version the toast was shown for.
    pub toasted: Option<String>,
    /// A second Ctrl-a N within this window restarts despite the warning.
    pub confirm_until: Option<Instant>,
    /// Restart once the assistant's turn is over.
    pub after_turn: bool,
    /// "Later": no chip for this version until the next launch.
    pub later: Option<String>,
    /// Download now even with auto_download off (the Updates "Download").
    pub force_download: Arc<std::sync::atomic::AtomicBool>,
}

impl UpdateUi {
    pub fn snapshot(&self) -> Shared {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The newer version that is ready to restart into.
    pub fn ready(&self) -> Option<String> {
        let s = self.snapshot();
        (s.phase == Phase::Ready)
            .then(|| s.staged.map(|x| x.version))
            .flatten()
    }
}

/// One check (and download), as the thread runs it (tests).
#[cfg(test)]
pub fn run_once(
    shared: &Arc<Mutex<Shared>>,
    cfg: &crate::config::UpdateCfg,
    src: &update::Source,
    l: &Layout,
    method: Method,
) {
    run_once_with(shared, cfg, src, l, method, false)
}

/// `force`: download even with auto_download off.
pub fn run_once_with(
    shared: &Arc<Mutex<Shared>>,
    cfg: &crate::config::UpdateCfg,
    src: &update::Source,
    l: &Layout,
    method: Method,
    force: bool,
) {
    let set = |f: &dyn Fn(&mut Shared)| f(&mut shared.lock().unwrap_or_else(|e| e.into_inner()));
    set(&|s| {
        s.phase = Phase::Checking;
        s.method = Some(method.clone());
    });
    let mut cache = Cache::load(l);
    let res = update::check(src, &mut cache, &cfg.channel);
    let skipped = cache.skipped.clone();
    if let Err(e) = &res {
        cache.last_error = Some(format!("{e:#}"));
    }
    cache.save(l);
    let rel = match res {
        Ok(r) => r,
        Err(e) => {
            crate::log::info(&format!("update: check failed: {e:#}"));
            set(&|s| {
                s.phase = Phase::Failed(format!("{e:#}"));
                s.checked_at = Some(chrono::Local::now());
            });
            return;
        }
    };
    let cur = Version::current();
    let newer = rel.clone().filter(|r| r.version() > cur);
    set(&|s| {
        s.latest = rel.clone();
        s.checked_at = Some(chrono::Local::now());
    });
    let Some(rel) = newer else {
        crate::log::info("update: up to date");
        set(&|s| s.phase = Phase::UpToDate);
        return;
    };
    let v = rel.version().to_string();
    // The notes of every version between, for the Updates window.
    let mut cache = Cache::load(l);
    let between = update::notes_between(src, &mut cache, &rel, &cur);
    cache.save(l);
    set(&|s| s.between = between.clone());
    if skipped.as_deref() == Some(v.as_str()) {
        crate::log::info(&format!("update: {v} skipped by the user"));
        set(&|s| s.phase = Phase::UpToDate);
        return;
    }
    if shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .staged
        .as_ref()
        .is_some_and(|x| x.version == v)
    {
        set(&|s| s.phase = Phase::Ready);
        return;
    }
    if method.hint().is_some() || (!cfg.auto_download && !force) {
        crate::log::info(&format!("update: {v} available ({})", method.describe()));
        set(&|s| s.phase = Phase::Available);
        return;
    }
    let sh = shared.clone();
    let progress = move |got: u64, total: u64| {
        if let Some(p) = (got * 100).checked_div(total) {
            let pct = p.min(99) as u8;
            sh.lock().unwrap_or_else(|e| e.into_inner()).phase = Phase::Downloading(pct);
        }
    };
    match update::stage(src, &rel, &method, l, &progress) {
        Ok(st) => {
            crate::log::info(&format!("update: {v} downloaded and verified"));
            set(&|s| {
                s.staged = Some(st.clone());
                s.phase = Phase::Ready;
            });
        }
        Err(e) => {
            crate::log::info(&format!("update: download of {v} failed: {e:#}"));
            set(&|s| s.phase = Phase::Failed(format!("{e:#}")));
        }
    }
}

impl App {
    /// Start the checker once (never in tests: no network there).
    pub fn start_update_checker(&mut self) {
        if self.update.kick.is_some() || cfg!(test) || !self.cfg.update.enabled {
            return;
        }
        let (tx, rx) = mpsc::channel::<()>();
        self.update.kick = Some(tx);
        let shared = self.update.shared.clone();
        let force = self.update.force_download.clone();
        let cfg = self.cfg.update.clone();
        let every = Duration::from_secs(cfg.check_hours.max(1) * 3600);
        let _ = std::thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                let l = Layout::default();
                let src = update::Source::github(cfg.require_signature);
                // Let the start settle first.
                let mut wait = Duration::from_secs(15);
                loop {
                    match rx.recv_timeout(wait) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    let now = force.swap(false, std::sync::atomic::Ordering::SeqCst);
                    run_once_with(&shared, &cfg, &src, &l, update::detect_self(&l), now);
                    // A failed check (offline, rate limited) tries again sooner.
                    let failed = matches!(
                        shared.lock().unwrap_or_else(|e| e.into_inner()).phase,
                        Phase::Failed(_)
                    );
                    wait = if failed {
                        every.min(Duration::from_secs(3600))
                    } else {
                        every
                    };
                }
            });
    }

    /// "Check now".
    pub fn check_for_update(&mut self) {
        if !self.cfg.update.enabled {
            self.flash("Update checks are off (Settings > General > Updates)");
            return;
        }
        self.start_update_checker();
        if let Some(k) = &self.update.kick {
            let _ = k.send(());
            self.flash("Checking for updates...");
        }
    }

    /// Every second: the one time toast when a version becomes ready.
    pub fn update_tick(&mut self) {
        let s = self.update.snapshot();
        // A development build hears about releases in Settings > About only.
        let dev = matches!(s.method, Some(Method::Dev { .. }));
        let v = s.latest.as_ref().map(|r| r.version().to_string());
        let note = match (&s.phase, &v) {
            (Phase::Ready, Some(v)) => Some(format!(
                "Update v{v} is ready: Ctrl-a N restarts into it (your tabs resume)"
            )),
            (Phase::Available, Some(v)) => Some(match s.method.as_ref().and_then(Method::hint) {
                Some(h) => format!("GodTerm v{v} is out: {h}"),
                None => format!("GodTerm v{v} is out: Ctrl-a N downloads it"),
            }),
            _ => None,
        };
        if let (Some(n), Some(v), false) = (note, v, dev) {
            if self.update.toasted.as_deref() != Some(v.as_str()) {
                self.update.toasted = Some(v);
                self.flash(n);
            }
        }
        if self.update.after_turn && !self.assistant.busy {
            self.update.after_turn = false;
            self.restart_to_update(true);
        }
    }

    /// The status bar chip: "UPDATE v0.2.2 · RESTART".
    pub fn update_chip(&self) -> Option<String> {
        let s = self.update.snapshot();
        if matches!(s.method, Some(Method::Dev { .. })) {
            return None;
        }
        let v = s.latest.as_ref()?.version().to_string();
        match s.phase {
            Phase::Ready => Some(format!("UPDATE v{v} · RESTART")),
            Phase::Available => Some(format!("UPDATE v{v}")),
            _ => None,
        }
    }

    /// What stands in the way of restarting now.
    pub fn restart_blockers(&self) -> Vec<String> {
        let mut why = vec![];
        if !self.pending_confirms.is_empty() {
            why.push("a confirmation is waiting for your answer".to_string());
        }
        if self.assistant.busy {
            why.push("the assistant is answering".into());
        }
        if self.voice.partial.is_some() || self.voice.holding.is_some() {
            why.push("you are talking".into());
        }
        let working = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| matches!(t.activity, Activity::Working | Activity::Permission))
            .count();
        if working > 0 {
            why.push(format!(
                "{working} tab{} working (interrupted, then resumed)",
                if working == 1 { " is" } else { "s are" }
            ));
        }
        why
    }

    /// Ctrl-a N and the chip: restart into the ready version; ask first
    /// when something is in progress (a second press within 10 s goes on).
    /// Without a ready version it checks (or says how this install updates).
    pub fn restart_to_update(&mut self, force: bool) {
        let s = self.update.snapshot();
        let Some(staged) = s.staged.clone().filter(|_| s.phase == Phase::Ready) else {
            match (&s.phase, s.method.as_ref().and_then(Method::hint)) {
                (Phase::Available, Some(h)) => self.flash(format!("Update with: {h}")),
                (Phase::Downloading(p), _) => self.flash(format!("Downloading the update ({p}%)")),
                _ => self.check_for_update(),
            }
            return;
        };
        let again = self
            .update
            .confirm_until
            .is_some_and(|t| Instant::now() < t);
        let blockers = self.restart_blockers();
        if !blockers.is_empty() && !force && !again {
            self.update.confirm_until = Some(Instant::now() + Duration::from_secs(10));
            self.flash(format!(
                "Not yet: {}. Press Ctrl-a N again within 10 s to restart anyway",
                blockers.join(", ")
            ));
            return;
        }
        self.update.confirm_until = None;
        let method = s
            .method
            .clone()
            .unwrap_or_else(|| update::detect_self(&Layout::default()));
        match update::install(
            &staged,
            &method,
            &Version::current(),
            false,
            &Layout::default(),
        ) {
            Ok(next) => {
                crate::log::info(&format!(
                    "update: restarting into {} ({})",
                    staged.version,
                    next.program.display()
                ));
                self.save_state();
                update::request_restart(next);
                self.quit = true;
            }
            Err(e) => {
                crate::log::info(&format!("update: install failed: {e:#}"));
                self.update
                    .shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .phase = Phase::Failed(format!("{e:#}"));
                self.flash(format!("Update failed: {e:#}"));
            }
        }
    }

    /// "Skip this version".
    pub fn skip_update(&mut self) {
        let s = self.update.snapshot();
        let Some(v) = s.latest.map(|r| r.version().to_string()) else {
            return;
        };
        let l = Layout::default();
        let mut c = Cache::load(&l);
        c.skipped = Some(v.clone());
        c.save(&l);
        let mut sh = self.update.shared.lock().unwrap_or_else(|e| e.into_inner());
        sh.phase = Phase::UpToDate;
        sh.staged = None;
        drop(sh);
        self.flash(format!("Skipping v{v}; a newer release will show again"));
    }

    /// Settings > About: the update lines.
    pub fn update_lines(&self) -> Vec<String> {
        let s = self.update.snapshot();
        let cur = Version::current();
        let checked = s
            .checked_at
            .map(|t| format!(" (checked {})", t.format("%H:%M")))
            .unwrap_or_default();
        let mut v = vec![];
        let latest = s.latest.as_ref().map(|r| r.version().to_string());
        v.push(match (&s.phase, &latest) {
            _ if !self.cfg.update.enabled => "updates    checks are off".to_string(),
            (Phase::Ready, Some(l)) => {
                format!("updates    v{l} downloaded and verified, ready to restart{checked}")
            }
            (Phase::Available, Some(l)) => format!(
                "updates    v{l} is out{}{checked}",
                s.method
                    .as_ref()
                    .and_then(Method::hint)
                    .map(|h| format!(": {h}"))
                    .unwrap_or_default()
            ),
            (Phase::Downloading(p), Some(l)) => format!("updates    downloading v{l} ({p}%)"),
            (Phase::Checking, _) => "updates    checking...".into(),
            (Phase::Failed(e), _) => format!(
                "updates    last check failed: {}",
                crate::sessions::snippet(e, 90)
            ),
            (Phase::UpToDate, _) => format!("updates    v{cur} is the latest{checked}"),
            _ => format!("updates    v{cur}, not checked yet"),
        });
        if let Some(m) = &s.method {
            v.push(format!("installed  {}", m.describe()));
        }
        if let Some(r) = s.latest.as_ref().filter(|r| r.version() > cur) {
            v.push(format!("notes      {}", r.page));
            for n in r.notes(6) {
                v.push(format!("           {}", crate::sessions::snippet(&n, 100)));
            }
        }
        v
    }

    /// The state block line for the assistant.
    pub fn update_state_line(&self) -> String {
        if let Some(v) = self.update.ready() {
            return format!("update_ready: v{v} (mention it once, briefly; \"update now\" is restart_to_update; what's new: update_info)\n");
        }
        let s = self.update.snapshot();
        let Some(v) = s
            .latest
            .as_ref()
            .map(|r| r.version())
            .filter(|v| *v > Version::current())
        else {
            return String::new();
        };
        let how = match (&s.phase, s.method.as_ref().and_then(Method::hint)) {
            (_, Some(h)) => format!("updates with: {h}"),
            (Phase::Downloading(p), _) => format!("downloading, {p}%"),
            _ => "not downloaded yet (restart_to_update downloads it)".into(),
        };
        format!("update_available: v{v}, {how}; what's new: update_info\n")
    }

    /// For the assistant's update_info.
    pub fn update_info_json(&self) -> serde_json::Value {
        let s = self.update.snapshot();
        let cur = Version::current();
        let latest = s.latest.as_ref().map(|r| r.version().to_string());
        let mut rels = s.between.clone();
        if rels.is_empty() {
            rels.extend(s.latest.clone().filter(|r| r.version() > cur));
        }
        serde_json::json!({
            "current": cur.to_string(),
            "latest": latest,
            "newer": rels.iter().any(|r| r.version() > cur),
            "state": match &s.phase {
                Phase::Ready => "downloaded and verified, ready to restart".to_string(),
                Phase::Downloading(p) => format!("downloading {p}%"),
                Phase::Available => "available".into(),
                Phase::Checking => "checking".into(),
                Phase::UpToDate => "up to date".into(),
                Phase::Failed(e) => format!("last check failed: {e}"),
                Phase::Idle => "not checked yet".into(),
            },
            "install": s.method.as_ref().map(Method::describe),
            "update_with": s.method.as_ref().and_then(Method::hint),
            "versions": rels.iter().map(|r| serde_json::json!({
                "version": r.version().to_string(),
                "published": r.published,
                "notes": r.notes(20),
                "page": r.page,
            })).collect::<Vec<_>>(),
        })
    }
}
