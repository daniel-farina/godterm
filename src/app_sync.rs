use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::app::App;
use crate::sync::worker::Worker;

pub const TOOL_NAMES: &[&str] = &[
    "sync_status",
    "sync_now",
    "sync_lease",
    "sync_plan",
    "sync_pause",
];

pub const PLANNED: &[&str] = &["sync_init", "sync_clone"];

const PLAN_FOR: Duration = Duration::from_secs(600);

const VIS_FOR: Duration = Duration::from_secs(300);

static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();
static SEEN: AtomicU64 = AtomicU64::new(0);

fn worker() -> &'static Mutex<Option<Worker>> {
    WORKER.get_or_init(|| Mutex::new(None))
}

fn snapshot() -> Option<crate::sync::worker::Shared> {
    worker()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(Worker::snapshot)
}

static LINK: OnceLock<crate::sync::Link> = OnceLock::new();

fn make_ctx() -> anyhow::Result<crate::sync::Ctx> {
    let mut c = crate::sync::Ctx::from_env()?;
    c.link = LINK.get().cloned();
    Ok(c)
}

#[derive(Debug, Clone)]
pub struct PlanCache {
    pub at: Instant,
    pub owner: String,
    pub account: String,

    pub repo: Option<String>,
    pub value: Value,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncOp {
    pub kind: String,

    pub target: String,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,

    pub ok: Option<bool>,
    pub error: Option<String>,
    pub summary: Value,
}

#[derive(Debug, Default)]
pub struct VisCache {
    at: Option<Instant>,
    checking: bool,

    repos: Vec<(String, Option<String>)>,
}

#[derive(Debug, Default)]
pub struct SyncUi {
    pub plan: Arc<Mutex<Option<PlanCache>>>,
    pub op: Arc<Mutex<Option<SyncOp>>>,
    pub vis: Arc<Mutex<VisCache>>,

    pub test_home: Option<(PathBuf, PathBuf)>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn in_words(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0 => "now".into(),
        1..=59 => format!("in {s} s"),
        _ => format!("in {} min", (s + 59) / 60),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl App {
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

        SEEN.store(applied, Ordering::Relaxed);
    }

    fn sync_link(&mut self) -> Option<crate::sync::Link> {
        if cfg!(test) {
            return self.lease.link.clone();
        }
        if LINK.get().is_none() {
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
        }
        self.lease.link = LINK.get().cloned();
        self.lease.link.clone()
    }

    pub(crate) fn lease_start(&mut self) {
        if cfg!(test) || self.lease.cmd.is_some() || !self.cfg.sync.enabled {
            return;
        }
        self.sync_link();
        self.lease.me = crate::sync::Machine::load_or_new(&crate::config::app_home()).id;
        let tx =
            crate::app_lease::spawn(Arc::new(make_ctx), self.tx.clone(), self.lease.held.clone());
        for c in std::mem::take(&mut self.lease.sent) {
            let _ = tx.send(c);
        }
        self.lease.cmd = Some(tx);
    }

    pub fn sync_poke(&self, why: &'static str) {
        if let Some(w) = worker().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            w.poke(why);
        }
    }

    pub(crate) fn sync_ctx(&self) -> anyhow::Result<crate::sync::Ctx> {
        if let Some((home, app)) = &self.sync_ui.test_home {
            let text = std::fs::read_to_string(app.join("config.toml")).unwrap_or_default();
            let cfg = crate::config::Config::parse(&text)?;
            let mut c = crate::sync::Ctx::new(home.clone(), app.clone(), cfg);
            c.never.push(crate::sync::paths::real(&home.join("vault")));
            c.link = self.lease.link.clone();
            return Ok(c);
        }
        if cfg!(test) {
            anyhow::bail!("tests give the sync a fixture home (sync_ui.test_home)");
        }
        make_ctx()
    }

    pub fn sync_lines(&self) -> Vec<String> {
        let s = &self.cfg.sync;
        if !s.enabled {
            return vec![
                "Sync is off on this machine.".into(),
                String::new(),
                "Turn it on from the web app (Settings > Sync: Preview, Enable or Join), ask the assistant (\"enable sync\"), or in a shell:".into(),
                "First machine: godterm sync init (creates a PRIVATE state repo after a typed yes).".into(),
                "New machine:   godterm sync clone OWNER/godterm-state".into(),
                "Dry run:       godterm sync plan".into(),
                String::new(),
                "Project repos are never committed to or pushed by the sync.".into(),
            ];
        }
        let Ok(ctx) = self.sync_ctx() else {
            return vec!["Sync: config.toml does not load".into()];
        };
        let snap = snapshot();
        let mut v = vec![];
        if s.paused {
            v.push("Paused: the background sync waits (Sync now still works)".into());
        }
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

    pub fn sync_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        if PLANNED.contains(&tool) {
            return Some(self.planned(tool, args));
        }
        if !TOOL_NAMES.contains(&tool) {
            return None;
        }
        Some(match tool {
            "sync_lease" => {
                let user = self.web_caller;
                self.sync_lease_tool(args, user)
            }
            "sync_status" => Ok(self.sync_status_json()),
            "sync_plan" => self.sync_plan_tool(args),
            "sync_pause" => self.sync_pause_tool(args),
            _ => {
                if !self.cfg.sync.enabled {
                    Err(
                        "sync is off on this machine: sync_plan, then sync_init (first machine) or sync_clone (join), each with the user's yes"
                            .into(),
                    )
                } else {
                    self.sync_poke("asked");
                    self.lease_send(crate::sync::takeover::Cmd::Poll);
                    let say = if self.cfg.sync.paused {
                        "Syncing GodTerm's state now (once: the background sync stays paused)."
                    } else {
                        "Syncing GodTerm's state now."
                    };
                    Ok(json!({"ok": true, "result": {"say": say}}))
                }
            }
        })
    }

    pub fn sync_op(&self) -> Option<SyncOp> {
        lock(&self.sync_ui.op).clone()
    }

    fn sync_op_running(&self) -> bool {
        self.sync_op().is_some_and(|o| o.ok.is_none())
    }

    pub fn sync_status_json(&self) -> Value {
        let s = self.cfg.sync.clone();
        let lines = self.sync_lines();
        let ctx = self.sync_ctx().ok();
        let snap = snapshot();
        let rep = snap
            .as_ref()
            .and_then(|s| s.last.clone())
            .or_else(|| ctx.as_ref().and_then(crate::sync::state_repo::Report::load));
        let state_repo = format!("{}/{}", s.owner, s.state_repo);
        let now = now_ms();
        let mut warnings: Vec<String> = vec![];
        let mut repos: Vec<Value> = vec![];
        let mut next: Option<Value> = None;
        if s.enabled {
            let mut names = vec![(state_repo.clone(), "state", None, false)];
            for sh in rep.as_ref().map(|r| r.shards.clone()).unwrap_or_default() {
                names.push((
                    format!("{}/{}", s.owner, sh["repo"].as_str().unwrap_or("")),
                    "conversations",
                    sh["bytes"].as_u64(),
                    sh["sealed"] == true,
                ));
            }
            let vis = ctx
                .as_ref()
                .map(|c| self.sync_visibility(c, names.iter().map(|n| n.0.clone()).collect()))
                .unwrap_or_default();
            for (name, role, bytes, sealed) in names {
                let v = vis
                    .iter()
                    .find(|(n, _)| *n == name)
                    .and_then(|x| x.1.clone());
                if let Some(x) = v
                    .as_ref()
                    .filter(|x| x.as_str() != "PRIVATE" && x.as_str() != "CHECKING")
                {
                    warnings.push(format!(
                        "{name} reads as {x}, not private: make it private on GitHub now"
                    ));
                }
                repos.push(json!({"name": name, "role": role, "bytes": bytes,
                    "size": bytes.map(crate::sync::plan::mb), "sealed": sealed,
                    "visibility": v}));
            }

            let hb = s.heartbeat_s.clamp(30, 3600) as i64 * 1000;
            next = Some(if s.paused {
                json!({"paused": true, "words": "paused"})
            } else if snap.as_ref().is_some_and(|x| x.running) {
                json!({"words": "running now"})
            } else if let Some(r) = snap.as_ref().and_then(|x| x.retry_in) {
                json!({"at_ms": now + r as i64 * 1000, "words": format!("retry in {r} s")})
            } else if let Some(r) = rep.as_ref().filter(|r| r.more) {
                json!({"at_ms": r.at_ms, "words": "goes on at once (more to upload)"})
            } else if let Some(r) = rep.as_ref() {
                let at = (r.at_ms + hb).max(now);
                json!({"at_ms": at, "words": in_words(at - now), "note": "at the latest; a few seconds after a change"})
            } else {
                json!({"words": "soon (the first sync)"})
            });
            if let Some(r) = &rep {
                if let Some(e) = &r.error {
                    warnings.push(format!("last sync failed: {e}"));
                }
                if let Some(w) = &r.token_warning {
                    warnings.push(w.clone());
                }
                if r.secret_warnings > 0 {
                    warnings.push(format!(
                        "{} likely secrets in pushed transcripts (the repos are private; warning only)",
                        r.secret_warnings
                    ));
                }
                for e in &r.env_not_ignored {
                    warnings.push(format!("{e} is not ignored by its project's git"));
                }
                for f in &r.forked {
                    warnings.push(format!("forked (diverged, never merged): {f}"));
                }
            }
        }
        let prefs = crate::acct_tokens::Prefs::load(
            &ctx.as_ref()
                .map(|c| c.app_home.clone())
                .unwrap_or_else(crate::config::app_home),
        );
        let say = if !s.enabled {
            "Sync is off on this machine.".to_string()
        } else {
            lines
                .iter()
                .find(|l| l.starts_with("State repo") || l.starts_with("Paused"))
                .cloned()
                .unwrap_or_else(|| lines.first().cloned().unwrap_or_default())
        };
        json!({"ok": true, "result": {
            "enabled": s.enabled,
            "paused": s.paused,
            "lines": lines,
            "state_repo": state_repo,
            "owner": s.owner,
            "account": s.account,
            "remote": s.remote,
            "machine": ctx.as_ref().map(|c| json!({"label": c.machine.name(), "id": c.machine.short()})),
            "worker": snap.as_ref().map(|x| json!({"running": x.running, "reason": x.last_reason,
                "retry_in": x.retry_in, "changes_seen": x.changes_seen})),
            "last": rep.as_ref().map(|r| json!({
                "ok": r.ok, "at_ms": r.at_ms, "last_ok_ms": r.last_ok_ms, "pushed": r.pushed,
                "waiting": r.waiting, "error": r.error, "errors": r.errors.iter().take(10).collect::<Vec<_>>(),
                "forked": r.forked, "held": r.held, "sessions": r.sessions,
                "sessions_pushed": r.sessions_pushed, "bytes_pushed": r.bytes_pushed,
                "remote_only": r.remote_only, "more": r.more, "extended": r.extended,
                "new_jobs_paused": r.new_jobs_paused, "secret_warnings": r.secret_warnings,
                "token_warning": r.token_warning,
            })),
            "next": next,
            "repos": repos,
            "machines": rep.as_ref().map(|r| json!(r.machines)).unwrap_or(json!([])),
            "leases": self.lease_status_json(),
            "projects_not_backed_up": rep.as_ref().map(|r| json!(r.projects)).unwrap_or(json!([])),
            "token_sync": {"on": prefs.sync_all, "warning": rep.as_ref().and_then(|r| r.token_warning.clone())},
            "op": self.sync_op(),
            "warnings": warnings,
            "never": "Project repos are never committed to, pushed or created by the sync.",
            "say": say,
        }})
    }

    fn sync_visibility(
        &self,
        ctx: &crate::sync::Ctx,
        names: Vec<String>,
    ) -> Vec<(String, Option<String>)> {
        let remote = ctx.remote();
        let split = |n: &str| {
            let (o, r) = n.split_once('/').unwrap_or(("", n));
            (o.to_string(), r.to_string())
        };
        if !remote.is_github() {
            return names
                .into_iter()
                .map(|n| {
                    let (o, r) = split(&n);
                    let v = remote.visibility(&o, &r);
                    (n, v)
                })
                .collect();
        }
        let mut c = lock(&self.sync_ui.vis);
        let fresh = c.at.is_some_and(|t| t.elapsed() < VIS_FOR)
            && names.iter().all(|n| c.repos.iter().any(|(x, _)| x == n));
        if !fresh && !c.checking && !cfg!(test) {
            c.checking = true;
            let vis = self.sync_ui.vis.clone();
            let names2 = names.clone();
            let _ = std::thread::Builder::new()
                .name("sync-vis".into())
                .spawn(move || {
                    let out: Vec<(String, Option<String>)> = names2
                        .into_iter()
                        .map(|n| {
                            let (o, r) = split(&n);
                            let v = remote.visibility(&o, &r);
                            (n, v)
                        })
                        .collect();
                    let mut c = lock(&vis);
                    c.repos = out;
                    c.at = Some(Instant::now());
                    c.checking = false;
                });
        }
        names
            .into_iter()
            .map(|n| {
                let v = c
                    .repos
                    .iter()
                    .find(|(x, _)| *x == n)
                    .map(|x| x.1.clone())
                    .unwrap_or_else(|| Some("CHECKING".into()));
                (n, v)
            })
            .collect()
    }

    fn sync_plan_tool(&mut self, args: &Value) -> Result<Value, String> {
        let ctx = self.sync_ctx().map_err(|e| format!("{e:#}"))?;
        let s = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_string)
        };
        let repo = s("repo");
        if let Some(r) = &repo {
            r.split_once('/')
                .filter(|(a, b)| crate::sync::git::valid_account(a) && !b.is_empty())
                .ok_or("repo: the state repo as OWNER/NAME (godterm-state, usually)")?;
        }
        let (owner, account) = (s("owner"), s("account"));
        let enabled = self.cfg.sync.enabled;
        let cache = self.sync_ui.plan.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("sync-plan".into())
            .spawn(move || {
                let v = build_plan(&ctx, enabled, owner, account, repo, &cache);
                let _ = tx.send(v);
            })
            .map_err(|e| e.to_string())?;
        self.pending_job = Some(rx);
        Ok(
            json!({"_wait": {"job": true, "deliveries": [], "max_ms": 55_000,
            "timeout_error": "the plan is still being built (a large first sync): call sync_plan again in a moment"}}),
        )
    }

    fn sync_pause_tool(&mut self, args: &Value) -> Result<Value, String> {
        if !self.cfg.sync.enabled {
            return Err("sync is off on this machine: nothing to pause".into());
        }
        let pause = args.get("pause").and_then(Value::as_bool).unwrap_or(true);
        let ctx = self.sync_ctx().map_err(|e| format!("{e:#}"))?;
        let mut sc = ctx.sync().clone();
        sc.paused = pause;
        crate::sync::restore::save_sync_cfg(&ctx.app_home, &sc).map_err(|e| format!("{e:#}"))?;
        self.cfg.sync.paused = pause;
        self.reload_config_now();
        if !pause {
            self.sync_poke("asked");
        }
        let say = if pause {
            "Paused GodTerm Sync on this machine: the background sync waits until you resume. Sync now still runs once; conversation ownership keeps working."
        } else {
            "Resumed GodTerm Sync on this machine: it syncs now and after each change."
        };
        Ok(json!({"ok": true, "result": {"paused": pause, "say": say}}))
    }

    pub(crate) fn sync_plan_for(
        &mut self,
        tool: &str,
        args: &Value,
    ) -> Option<Result<(Value, String, bool), String>> {
        if !PLANNED.contains(&tool) {
            return None;
        }
        Some((|| {
            if self.cfg.sync.enabled {
                return Err(format!(
                    "sync is already on here (state repo {}/{}): sync_status shows it",
                    self.cfg.sync.owner, self.cfg.sync.state_repo
                ));
            }
            if self.sync_op_running() {
                return Err("a sync setup is running already: sync_status shows it".into());
            }
            let s = |k: &str| {
                args.get(k)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
            };
            let cache = lock(&self.sync_ui.plan).clone();
            if tool == "sync_init" {
                let c = cache
                    .filter(|c| c.repo.is_none() && c.at.elapsed() < PLAN_FOR)
                    .ok_or("run sync_plan first (it checks gh and the repo, and shows what would be synced); tell the user what it says, then call sync_init")?;
                let (owner, account) = (
                    s("owner").unwrap_or_else(|| c.owner.clone()),
                    s("account")
                        .or_else(|| s("owner"))
                        .unwrap_or_else(|| c.account.clone()),
                );
                if owner != c.owner || account != c.account {
                    return Err(format!(
                        "the plan was for {}/{} with gh account {}: run sync_plan with owner {owner} and account {account} first",
                        c.owner,
                        self.cfg.sync.state_repo,
                        c.account
                    ));
                }
                let pf = &c.value["preflight"];
                if pf["can_init"] != true {
                    let w: Vec<&str> = pf["warnings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    return Err(format!("cannot enable sync: {}", w.join("; ")));
                }
                let repo = pf["state_repo"].as_str().unwrap_or("").to_string();
                let convos = pf["convos"].as_str().unwrap_or("").to_string();
                let p = &c.value["plan"];
                let what = format!(
                    "{} conversations ({}), {} .env files, config and accounts (no logins)",
                    p["sessions"].as_u64().unwrap_or(0),
                    crate::sync::plan::mb(p["session_bytes"].as_u64().unwrap_or(0)),
                    p["env_files"].as_u64().unwrap_or(0),
                );
                let q = if pf["github"] == true {
                    format!(
                        "Create the PRIVATE GitHub repo {repo} with the gh account {account}, push this machine's first backup ({what}) and from now on private monthly repos {convos}? They hold your config, .env files and conversations as is; project repos are never touched."
                    )
                } else {
                    format!(
                        "Create the private state repo {repo} in {} and push this machine's first backup ({what})? Project repos are never touched.",
                        pf["remote"].as_str().unwrap_or("")
                    )
                };
                return Ok((
                    json!({"owner": owner, "account": account, "repo": repo}),
                    q,
                    true,
                ));
            }

            let repo = s("repo").ok_or("repo: the state repo to join, as OWNER/NAME")?;
            let (owner, _) = repo
                .split_once('/')
                .filter(|(a, b)| crate::sync::git::valid_account(a) && !b.is_empty())
                .ok_or("repo: the state repo as OWNER/NAME (godterm-state, usually)")?;
            let account = s("account").unwrap_or_else(|| owner.to_string());
            if !crate::sync::git::valid_account(&account) {
                return Err("bad gh account name".into());
            }

            if let Some(c) = cache.filter(|c| {
                c.repo.as_deref() == Some(&repo)
                    && c.account == account
                    && c.at.elapsed() < PLAN_FOR
            }) {
                if c.value["preflight"]["can_clone"] != true {
                    let w: Vec<&str> = c.value["preflight"]["warnings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    return Err(format!("cannot join {repo}: {}", w.join("; ")));
                }
            }
            let label = s("label");
            let q = format!(
                "Join {repo} on this machine{} with the gh account {account}? GodTerm copies its config, accounts (you log in again: logins are never copied), jobs (paused until you turn them on), archive, .env files and the last {} days of conversations here, and clones the projects it lists into their folders (only cloned or fetched, never committed to or pushed). Nothing on GitHub changes.",
                label.as_deref().map(|l| format!(" as \"{l}\"")).unwrap_or_default(),
                self.cfg.sync.hot_days
            );
            Ok((
                json!({"repo": repo, "account": account, "label": label}),
                q,
                true,
            ))
        })())
    }

    pub(crate) fn sync_run_plan(
        &mut self,
        tool: &str,
        plan: &Value,
    ) -> Option<Result<Value, String>> {
        if !PLANNED.contains(&tool) {
            return None;
        }
        Some((|| {
            if self.cfg.sync.enabled {
                return Err("sync is already on here".into());
            }
            if self.sync_op_running() {
                return Err("a sync setup is running already".into());
            }
            let mut ctx = self.sync_ctx().map_err(|e| format!("{e:#}"))?;

            ctx.link = self.sync_link().or(ctx.link);
            if let Some(l) = &ctx.link {
                let running: std::collections::BTreeSet<String> = self
                    .panes
                    .iter()
                    .flat_map(|p| p.tabs.iter())
                    .filter(|t| t.is_running())
                    .filter_map(|t| t.session_id.clone())
                    .collect();
                *lock(&l.0.running) = running;
            }
            let str_of = |k: &str| plan[k].as_str().unwrap_or("").to_string();
            let target = str_of("repo");
            let op = SyncOp {
                kind: if tool == "sync_init" { "init" } else { "clone" }.into(),
                target: target.clone(),
                started_ms: now_ms(),
                ..Default::default()
            };
            *lock(&self.sync_ui.op) = Some(op);
            let slot = self.sync_ui.op.clone();
            let init = tool == "sync_init";
            let (owner, account, label) = (
                str_of("owner"),
                str_of("account"),
                plan["label"].as_str().map(str::to_string),
            );
            let t2 = target.clone();
            std::thread::Builder::new()
                .name("sync-setup".into())
                .spawn(move || {
                    let r = if init {
                        run_init(&ctx, owner, account)
                    } else {
                        run_clone(&ctx, &t2, account, label)
                    };
                    let mut g = lock(&slot);
                    if let Some(o) = g.as_mut() {
                        o.finished_ms = Some(now_ms());
                        match r {
                            Ok(v) => {
                                o.ok = Some(true);
                                o.summary = v;
                            }
                            Err(e) => {
                                crate::log::info(&format!("sync: {} failed: {e}", o.kind));
                                o.ok = Some(false);
                                o.error = Some(e);
                            }
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            let say = if init {
                format!("Creating the private repo {target} and pushing the first backup in the background. Settings > Sync (or sync_status) shows when it is done.")
            } else {
                format!("Joining {target} in the background: restoring the config, accounts, jobs, archive, .env files and recent conversations. Settings > Sync (or sync_status) shows when it is done.")
            };
            Ok(json!({"ok": true, "result": {"started": true, "say": say}}))
        })())
    }
}

fn build_plan(
    ctx: &crate::sync::Ctx,
    enabled: bool,
    owner: Option<String>,
    account: Option<String>,
    repo: Option<String>,
    cache: &Mutex<Option<PlanCache>>,
) -> Value {
    use crate::sync::plan;
    let gh = if ctx.remote().is_github() {
        crate::sync::git::gh_accounts()
    } else {
        vec![]
    };
    let (o, name, a) = match &repo {
        Some(full) => {
            let (o, n) = full.split_once('/').unwrap_or((full, ""));
            (
                o.to_string(),
                n.to_string(),
                account.clone().unwrap_or_else(|| o.to_string()),
            )
        }
        None => {
            let mut sc = ctx.sync().clone();
            if let Some(o) = &owner {
                sc.owner = o.clone();
                sc.account = o.clone();
            }
            if let Some(a) = &account {
                sc.account = a.clone();
            }
            if !enabled {
                crate::sync::restore::resolve_owner(&mut sc, &gh);
            }
            (sc.owner, sc.state_repo, sc.account)
        }
    };
    let mut pf = plan::preflight(ctx, &o, &name, &a, &gh);
    if repo.is_some() {
        pf.warnings.retain(|w| !w.contains("is there already"));
    }
    if enabled {
        pf.warnings.retain(|w| !w.contains("is there already"));
        pf.can_init = false;
        pf.can_clone = false;
    }

    let p = repo.is_none().then(|| plan::plan(ctx));
    let mut warnings = pf.warnings.clone();
    if let Some(p) = &p {
        warnings.extend(p.warnings());
    }
    let say = match (&p, &repo) {
        (Some(p), _) => format!(
            "Dry run, nothing written: {} conversations ({}, {} to upload in {} sync{}), {} .env files, config and {} stores would go to the private repo {} with the gh account {}{}.",
            p.sessions,
            plan::mb(p.session_bytes),
            plan::mb(p.new_bytes),
            p.batches,
            if p.batches == 1 { "" } else { "s" },
            p.env_files,
            p.stores.len(),
            pf.state_repo,
            if pf.account.is_empty() { "(none)" } else { &pf.account },
            if warnings.is_empty() { String::new() } else { format!("; {} warnings", warnings.len()) }
        ),
        (None, Some(r)) => match (pf.exists, pf.can_clone) {
            (Some(true), true) => format!("{r} is there and can be joined with the gh account {}.", pf.account),
            (Some(false), _) => format!("{r} does not exist (or the gh account {} cannot see it).", pf.account),
            _ => format!("{r} cannot be joined: {}", pf.warnings.join("; ")),
        },
        _ => String::new(),
    };
    let next = if enabled {
        "Sync is on already: this shows what it covers."
    } else if repo.is_some() {
        if pf.can_clone {
            "Tell the user; to join it, call sync_clone with this repo (it asks for their yes)."
        } else {
            "Tell the user what blocks it; nothing was done."
        }
    } else if pf.can_init {
        "Tell the user this summary and the warnings in a few words. To turn sync on, call sync_init: it asks the user once (needs_confirmation); only their clear yes in a later message runs it."
    } else {
        "Tell the user what blocks it (the warnings); nothing was done."
    };
    let v = json!({
        "plan": p,
        "text": p.as_ref().map(|p| p.text()),
        "preflight": pf,
        "warnings": warnings,
        "enabled": enabled,
        "say": say,
        "next": next,
    });
    *lock(cache) = Some(PlanCache {
        at: Instant::now(),
        owner: pf.owner.clone(),
        account: pf.account.clone(),
        repo,
        value: v.clone(),
    });
    v
}

fn run_init(ctx: &crate::sync::Ctx, owner: String, account: String) -> Result<Value, String> {
    let mut sc = ctx.sync().clone();
    sc.owner = owner;
    sc.account = account;

    let rep = crate::sync::restore::init(ctx, sc, &|_| true).map_err(|e| format!("{e:#}"))?;
    let c = crate::sync::restore::reload(ctx).map_err(|e| format!("{e:#}"))?;
    Ok(
        json!({"lines": crate::sync::cli::report_lines(&c, &rep), "ok": rep.ok,
        "say": if rep.ok { "Sync is on: the first backup is pushed." } else { "Sync is on, but the first sync failed: it retries on its own." }}),
    )
}

fn run_clone(
    ctx: &crate::sync::Ctx,
    full: &str,
    account: String,
    label: Option<String>,
) -> Result<Value, String> {
    let o = crate::sync::restore::CloneOpts {
        label,
        tabs_from: None,
        account: Some(account),
        remote: None,
    };
    let c = crate::sync::restore::clone(ctx, full, &o).map_err(|e| format!("{e:#}"))?;
    let ctx2 = crate::sync::restore::reload(ctx).map_err(|e| format!("{e:#}"))?;
    Ok(json!({
        "projects": c.projects, "placed": c.placed.len(), "logins": c.logins,
        "missing_paths": c.missing_paths,
        "lines": crate::sync::cli::report_lines(&ctx2, &c.report),
        "say": format!("Joined {full}: {} conversations placed here, {} projects; log in to each account again.", c.placed.len(), c.projects.len()),
    }))
}
