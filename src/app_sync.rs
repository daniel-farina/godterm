//! GodTerm Sync in the app: the background worker (started once sync is
//! on; never on the UI thread), the reload after a sync wrote into
//! GodTerm's home, the Sync section's lines, and the assistant's
//! `sync_status` (read only) and `sync_now` (no confirmation: it only
//! touches GodTerm's own state repos, never a project repo).

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::app::App;
use crate::sync::worker::Worker;

pub const TOOL_NAMES: &[&str] = &["sync_status", "sync_now", "sync_lease"];

static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();
static SEEN: AtomicU64 = AtomicU64::new(0);

fn worker() -> &'static Mutex<Option<Worker>> {
    WORKER.get_or_init(|| Mutex::new(None))
}

/// What the worker saw last, if it runs.
fn snapshot() -> Option<crate::sync::worker::Shared> {
    worker()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(Worker::snapshot)
}

/// The running app as the sync sees it (made once, with the worker).
static LINK: OnceLock<crate::sync::Link> = OnceLock::new();

/// A fresh context for a sync or a lease step, linked to the app.
fn make_ctx() -> anyhow::Result<crate::sync::Ctx> {
    let mut c = crate::sync::Ctx::from_env()?;
    c.link = LINK.get().cloned();
    Ok(c)
}

impl App {
    /// Each tick: start the workers once sync is on (the sync and the
    /// lease worker, each on its own thread).
    pub fn sync_tick(&mut self) {
        if cfg!(test) || crate::demo::active() || !self.cfg.sync.enabled {
            return;
        }
        self.lease_start();
        let mut w = worker().lock().unwrap_or_else(|e| e.into_inner());
        if w.is_none() {
            *w = Some(Worker::start(Arc::new(make_ctx)));
            return;
        }
        let applied = w.as_ref().map(|x| x.snapshot().applied).unwrap_or(0);
        drop(w);
        // The archive and jobs come in through AppEvent::SyncApply (the
        // app saves them itself); config.toml reloads on its own.
        SEEN.store(applied, Ordering::Relaxed);
    }

    /// The link and the lease worker, once.
    pub(crate) fn lease_start(&mut self) {
        if cfg!(test) || self.lease.cmd.is_some() || !self.cfg.sync.enabled {
            return;
        }
        let tx = self.tx.clone();
        let link = crate::sync::Link(Arc::new(crate::sync::AppLink {
            running: Mutex::default(),
            apply: Box::new(move |a| {
                let (rtx, rrx) = std::sync::mpsc::channel();
                if tx.send(crate::app::AppEvent::SyncApply(a, rtx)).is_err() {
                    return false;
                }
                rrx.recv_timeout(std::time::Duration::from_secs(20))
                    .unwrap_or(false)
            }),
        }));
        let _ = LINK.set(link);
        self.lease.link = LINK.get().cloned();
        self.lease.me = crate::sync::Machine::load_or_new(&crate::config::app_home()).id;
        let tx =
            crate::app_lease::spawn(Arc::new(make_ctx), self.tx.clone(), self.lease.held.clone());
        for c in std::mem::take(&mut self.lease.sent) {
            let _ = tx.send(c);
        }
        self.lease.cmd = Some(tx);
    }

    /// A turn ended or something changed: sync soon.
    pub fn sync_poke(&self, why: &'static str) {
        if let Some(w) = worker().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            w.poke(why);
        }
    }

    /// The Sync section's lines.
    pub fn sync_lines(&self) -> Vec<String> {
        let s = &self.cfg.sync;
        if !s.enabled {
            return vec![
                "Sync is off on this machine.".into(),
                String::new(),
                "First machine: godterm sync init (creates a PRIVATE state repo after a typed yes).".into(),
                "New machine:   godterm sync clone OWNER/godterm-state".into(),
                "Dry run:       godterm sync plan".into(),
                String::new(),
                "Project repos are never committed to or pushed by the sync.".into(),
            ];
        }
        let Ok(ctx) = crate::sync::Ctx::from_env() else {
            return vec!["Sync: config.toml does not load".into()];
        };
        let snap = snapshot();
        let mut v = vec![];
        if let Some(sn) = &snap {
            if sn.running {
                v.push(format!("Syncing now ({})", sn.last_reason));
            }
            if let Some(r) = sn.retry_in {
                v.push(format!("Retrying in {r}s"));
            }
        }
        let rep = snap
            .and_then(|s| s.last)
            .or_else(|| crate::sync::state_repo::Report::load(&ctx));
        match rep {
            Some(r) => v.extend(crate::sync::cli::report_lines(&ctx, &r)),
            None => v.push("No sync has run yet.".into()),
        }
        v.push(format!(
            "This machine: {} ({})",
            ctx.machine.name(),
            ctx.machine.short()
        ));
        v.extend(self.lease_lines());
        v
    }

    /// The sync tools; None when `tool` is not one of them.
    pub fn sync_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        if !TOOL_NAMES.contains(&tool) {
            return None;
        }
        Some(match tool {
            // The user's own press (the web's buttons), never the
            // assistant's.
            "sync_lease" => {
                let user = self.web_caller;
                self.sync_lease_tool(args, user)
            }
            "sync_status" => {
                let lines = self.sync_lines();
                let rep = crate::sync::Ctx::from_env()
                    .ok()
                    .and_then(|c| crate::sync::state_repo::Report::load(&c));
                let ctx = crate::sync::Ctx::from_env().ok();
                Ok(json!({"ok": true, "result": {
                    "enabled": self.cfg.sync.enabled,
                    "lines": lines,
                    "state_repo": format!("{}/{}", self.cfg.sync.owner, self.cfg.sync.state_repo),
                    "remote": self.cfg.sync.remote,
                    "machine": ctx.as_ref().map(|c| json!({"label": c.machine.name(), "id": c.machine.short()})),
                    "last": rep.as_ref().map(|r| json!({
                        "ok": r.ok, "at_ms": r.at_ms, "last_ok_ms": r.last_ok_ms, "pushed": r.pushed,
                        "waiting": r.waiting, "error": r.error, "forked": r.forked, "held": r.held,
                        "new_jobs_paused": r.new_jobs_paused, "secret_warnings": r.secret_warnings,
                        "token_warning": r.token_warning,
                    })),
                    "leases": self.lease_status_json(),
                    "projects_not_backed_up": rep.as_ref().map(|r| json!(r.projects)).unwrap_or(json!([])),
                    "say": lines.first().cloned().unwrap_or_default(),
                }}))
            }
            _ => {
                if !self.cfg.sync.enabled {
                    Err(
                        "sync is off on this machine (the user runs godterm sync init or clone)"
                            .into(),
                    )
                } else {
                    self.sync_poke("asked");
                    self.lease_send(crate::sync::takeover::Cmd::Poll);
                    Ok(json!({"ok": true, "result": {"say": "Syncing GodTerm's state now."}}))
                }
            }
        })
    }
}
