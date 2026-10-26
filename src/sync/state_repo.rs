//! The state repo and one sync (docs/SYNC.md 2.2, 5): fetch, apply what
//! other machines changed, project the home into the working copy, push
//! the conversation chunks, then the state commit. Never forced: a push
//! someone beat fetches and runs the cycle again. Bases (what was last
//! synced) are saved only once the push went through.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::chunks::{self, Chunk, Cmp, FileMan, Manifest};
use super::collect::{self, SessionRef};
use super::files::{self, Area, Kind};
use super::git::{self, Pushed};
use super::merge::{self, Base};
use super::{shards, Ctx};

/// What one sync did, also saved as `sync/status.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub ok: bool,
    pub at_ms: i64,
    /// Last time a sync reached the remote and pushed (or had nothing).
    #[serde(default)]
    pub last_ok_ms: i64,
    #[serde(default)]
    pub error: Option<String>,
    pub committed: bool,
    pub pushed: bool,
    /// Commits made here that are not on the remote yet.
    pub waiting: usize,
    pub sessions: usize,
    pub sessions_pushed: usize,
    pub bytes_pushed: u64,
    pub extended: usize,
    pub forked: Vec<String>,
    /// Sessions open in a tab here that another machine extended or
    /// diverged from (left alone until the tab closes; Phase 3 leases).
    pub held: Vec<String>,
    pub remote_only: usize,
    pub left_out: Vec<String>,
    pub applied: Vec<String>,
    pub refused: Vec<String>,
    pub env_kept_both: Vec<String>,
    pub env_not_ignored: Vec<String>,
    pub new_jobs_paused: Vec<String>,
    pub secret_warnings: usize,
    /// Long-lived tokens were left out: the repo is not verified private.
    #[serde(default)]
    pub token_warning: Option<String>,
    /// More to upload than one sync takes (a first upload): the next
    /// sync goes on at once.
    #[serde(default)]
    pub more: bool,
    pub errors: Vec<String>,
    /// Projects with no GitHub backup or unpushed work (never acted on).
    pub projects: Vec<super::projects::Health>,
    pub machines: Vec<Value>,
    pub shards: Vec<Value>,
}

impl Report {
    pub fn path(ctx: &Ctx) -> PathBuf {
        ctx.sync_dir().join("status.json")
    }

    pub fn load(ctx: &Ctx) -> Option<Report> {
        serde_json::from_slice(&std::fs::read(Self::path(ctx)).ok()?).ok()
    }

    fn save(&self, ctx: &Ctx) {
        let _ = super::write_file(
            &Self::path(ctx),
            &serde_json::to_vec_pretty(self).unwrap_or_default(),
        );
    }
}

/// Per file: what was last pushed or placed from here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LedFile {
    pub len: u64,
    pub mtime_ms: i64,
    pub sha256: String,
    pub chunks: Vec<Chunk>,
    #[serde(default)]
    pub scanned: u64,
    #[serde(default)]
    pub vault: bool,
}

pub type Ledger = BTreeMap<String, LedFile>;

fn ledger_path(ctx: &Ctx) -> PathBuf {
    ctx.sync_dir().join("ledger.json")
}

pub fn load_ledger(ctx: &Ctx) -> Ledger {
    std::fs::read(ledger_path(ctx))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(crate) fn save_ledger(ctx: &Ctx, l: &Ledger) {
    let _ = super::write_file(
        &ledger_path(ctx),
        &serde_json::to_vec(l).unwrap_or_default(),
    );
}

/// A lock so a CLI sync and the app's worker never run at once (the
/// file, across processes), nor two threads of one process (the set of
/// sync folders held here: the lease worker shares the repos).
pub struct Lock(PathBuf);

static HELD: std::sync::Mutex<BTreeSet<PathBuf>> = std::sync::Mutex::new(BTreeSet::new());

impl Lock {
    /// Take it, waiting up to `wait` for another holder to finish.
    pub fn wait(ctx: &Ctx, wait: std::time::Duration) -> Result<Lock> {
        let t = std::time::Instant::now();
        loop {
            match Lock::take(ctx) {
                Ok(l) => return Ok(l),
                Err(e) if t.elapsed() >= wait => return Err(e),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        }
    }

    pub fn take(ctx: &Ctx) -> Result<Lock> {
        let p = ctx.sync_dir().join("lock");
        std::fs::create_dir_all(ctx.sync_dir())?;
        if !HELD
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(p.clone())
        {
            bail!("a sync is already running here");
        }
        let r = Lock::take_file(&p);
        if r.is_err() {
            HELD.lock().unwrap_or_else(|e| e.into_inner()).remove(&p);
        }
        r
    }

    fn take_file(p: &Path) -> Result<Lock> {
        let p = p.to_path_buf();
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&p)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(Lock(p));
                }
                Err(_) => {
                    let pid: i32 = std::fs::read_to_string(&p)
                        .ok()
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    if pid > 0 && pid as u32 != std::process::id() && alive(pid) {
                        bail!("a sync is already running (pid {pid})");
                    }
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        bail!("could not take the sync lock")
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        HELD.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

fn alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

pub const README: &str = "# GodTerm state\n\n\
This repo is written by GodTerm Sync: this account's portable GodTerm config, \
accounts (no credentials), jobs, archive, projects, tab layouts per machine, \
project .env files and conversation manifests. Conversations themselves are in \
the monthly godterm-convos repos.\n\n\
PRIVATE: it holds .env files and conversations as is. Never make it public.\n";

/// The working copy of the state repo, cloned the first time.
pub fn open_wc(ctx: &Ctx) -> Result<PathBuf> {
    let wc = ctx.state_dir();
    if git::is_sync_repo(&wc) {
        return Ok(wc);
    }
    let r = ctx.remote();
    let (owner, name) = (&ctx.sync().owner, &ctx.sync().state_repo);
    if !r.exists(owner, name)? {
        bail!("{owner}/{name} does not exist: run godterm sync init on the first machine");
    }
    std::fs::create_dir_all(ctx.sync_dir())?;
    let _ = std::fs::remove_dir_all(&wc);
    let ws = wc.to_string_lossy().into_owned();
    git::run(
        &ctx.sync_dir(),
        &[
            "clone",
            "-q",
            "--no-tags",
            "-b",
            "main",
            &r.url(owner, name),
            &ws,
        ],
        r.cred().as_ref(),
    )
    .or_else(|_| {
        // An empty repo has no main yet.
        git::run(
            &ctx.sync_dir(),
            &["clone", "-q", &r.url(owner, name), &ws],
            r.cred().as_ref(),
        )
    })?;
    git::mark(&wc)?;
    let _ = git::run(&wc, &["checkout", "-q", "-B", "main"], None);
    Ok(wc)
}

/// Write the leases branch layout (Phase 3 adds the leases themselves):
/// `leases/.keep` and this machine's `heartbeats/<id>.json`.
pub fn write_heartbeat(ctx: &Ctx, wc: &Path) -> Result<()> {
    let cred = ctx.remote().cred();
    let idx = wc.join(".git").join("sync-leases-index");
    let env = [("GIT_INDEX_FILE", idx.as_path())];
    let hb = serde_json::to_vec_pretty(&json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "label": ctx.machine.name(),
    }))?;
    let keep = git::hash_object(wc, b"")?;
    let hbb = git::hash_object(wc, &hb)?;
    for _ in 0..5 {
        git::fetch(wc, "leases", cred.as_ref())?;
        let base = git::rev(wc, "refs/remotes/origin/leases");
        let _ = std::fs::remove_file(&idx);
        if let Some(b) = &base {
            git::run_env(wc, &["read-tree", b], None, &env)?;
        }
        for (blob, p) in [
            (&keep, "leases/.keep".to_string()),
            (&hbb, format!("heartbeats/{}.json", ctx.machine.id)),
        ] {
            let info = format!("100644,{blob},{p}");
            git::run_env(
                wc,
                &["update-index", "--add", "--cacheinfo", &info],
                None,
                &env,
            )?;
        }
        let tree = git::run_env(wc, &["write-tree"], None, &env)?
            .trim()
            .to_string();
        let msg = format!("heartbeat {}", ctx.machine.name());
        let c = match &base {
            Some(b) => git::run(wc, &["commit-tree", &tree, "-p", b, "-m", &msg], None)?,
            None => git::run(wc, &["commit-tree", &tree, "-m", &msg], None)?,
        };
        let c = c.trim().to_string();
        if git::push(wc, "origin", &c, "leases", cred.as_ref())? == Pushed::Done {
            let _ = std::fs::remove_file(&idx);
            return Ok(());
        }
    }
    let _ = std::fs::remove_file(&idx);
    bail!("could not write the heartbeat")
}

/// Run one sync now.
pub fn sync_now(ctx: &Ctx) -> Result<Report> {
    let _lock = Lock::take(ctx)?;
    let prev = Report::load(ctx).unwrap_or_default();
    let mut rep = Report {
        at_ms: ctx.now_ms,
        last_ok_ms: prev.last_ok_ms,
        ..Default::default()
    };
    let res = (|| -> Result<()> {
        let wc = open_wc(ctx)?;
        // Who owns what (docs/SYNC.md 4): only the owner pushes a
        // session's new bytes. Offline: the board as last fetched.
        let (board, _) = super::lease::current(ctx);
        for attempt in 0..4 {
            let mut r = Report {
                at_ms: ctx.now_ms,
                last_ok_ms: prev.last_ok_ms,
                ..Default::default()
            };
            let online = git::fetch(&wc, "main", ctx.remote().cred().as_ref());
            if let Err(e) = &online {
                r.errors.push(format!("offline: {e:#}"));
            } else if git::rev(&wc, "refs/remotes/origin/main").is_some() {
                git::run(
                    &wc,
                    &["reset", "-q", "--hard", "refs/remotes/origin/main"],
                    None,
                )?;
                git::run(&wc, &["clean", "-qfd"], None)?;
            }
            let pending = cycle(ctx, &wc, board.as_ref(), &mut r)?;
            git::run(&wc, &["add", "-A"], None)?;
            let dirty = !git::run(&wc, &["status", "--porcelain"], None)?
                .trim()
                .is_empty();
            if dirty {
                let msg = format!(
                    "sync from {} ({})",
                    ctx.machine.name(),
                    chrono::Local::now().format("%Y-%m-%d %H:%M")
                );
                git::run(&wc, &["commit", "-q", "-m", &msg], None)?;
                r.committed = true;
            }
            let ahead = match git::rev(&wc, "refs/remotes/origin/main") {
                Some(o) => git::run(&wc, &["rev-list", "--count", &format!("{o}..HEAD")], None)?
                    .trim()
                    .parse()
                    .unwrap_or(0),
                None => git::run(&wc, &["rev-list", "--count", "HEAD"], None)
                    .map(|s| s.trim().parse().unwrap_or(0))
                    .unwrap_or(0),
            };
            if online.is_err() {
                r.waiting = ahead;
                rep = r;
                return Ok(());
            }
            if ahead > 0 {
                match git::push(&wc, "origin", "HEAD", "main", ctx.remote().cred().as_ref()) {
                    Ok(Pushed::Done) => r.pushed = true,
                    Ok(Pushed::Rejected) if attempt < 3 => continue,
                    Ok(Pushed::Rejected) => bail!("the state repo kept moving; will try again"),
                    Err(e) => {
                        r.waiting = ahead;
                        r.errors.push(format!("push: {e:#}"));
                        rep = r;
                        return Ok(());
                    }
                }
            }
            for (p, b) in pending {
                let _ = merge::save_base(&p, &b);
            }
            r.ok = true;
            r.last_ok_ms = ctx.now_ms;
            rep = r;
            return Ok(());
        }
        Ok(())
    })();
    if let Err(e) = res {
        rep.ok = false;
        rep.error = Some(format!("{e:#}"));
    }
    if rep.error.is_none() && !rep.ok && !rep.errors.is_empty() {
        rep.error = rep.errors.first().cloned();
    }
    rep.save(ctx);
    Ok(rep)
}

fn base_path(ctx: &Ctx, name: &str) -> PathBuf {
    ctx.sync_dir()
        .join("base")
        .join(format!("{}.json", name.replace('/', "~")))
}

fn read_json(p: &Path) -> Value {
    std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

fn write_json(p: &Path, v: &Value) -> Result<()> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    super::write_file(p, &b)?;
    Ok(())
}

/// Paths in store entries, both ways.
fn fix_paths(map: &super::portable::PathMap, to_portable: bool) -> impl Fn(&str, &mut Value) + '_ {
    move |store: &str, v: &mut Value| {
        let f = |s: &str| -> String {
            if to_portable {
                map.portable_text(s)
            } else {
                // Stores hold absolute paths.
                map.to_local(s)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| s.to_string())
            }
        };
        let fix = |x: &mut Value| {
            if let Some(s) = x.as_str() {
                *x = Value::String(f(s));
            }
        };
        match store {
            "archive" => fix(&mut v["cwd"]),
            "jobs" if v["target"].is_object() => fix(&mut v["target"]["cwd"]),
            "projects" if v["kind"] == "project" || v["kind"] == "deep_searched" => {
                fix(&mut v["key"])
            }
            "state" => {
                for s in v["slots"].as_array_mut().into_iter().flatten() {
                    for t in s["tabs"].as_array_mut().into_iter().flatten() {
                        fix(&mut t["cwd"]);
                    }
                }
                for c in v["closed_tabs"].as_array_mut().into_iter().flatten() {
                    fix(&mut c["cwd"]);
                }
                for r in v["recents"].as_array_mut().into_iter().flatten() {
                    fix(&mut r["path"]);
                }
            }
            _ => {}
        }
    }
}

/// Jobs pulled from another machine wait for a yes here: they arrive
/// paused, and run once the user turns them on.
fn jobs_pending_path(ctx: &Ctx) -> PathBuf {
    ctx.sync_dir().join("jobs-pending.json")
}

/// Apply remote, project local; returns the bases to save after the push.
fn cycle(
    ctx: &Ctx,
    wc: &Path,
    board: Option<&super::lease::Board>,
    r: &mut Report,
) -> Result<Vec<(PathBuf, Base)>> {
    let mut pending: Vec<(PathBuf, Base)> = vec![];
    let map = ctx.paths();
    let now = ctx.now_ms;
    // Fixed files.
    std::fs::write(wc.join("format.json"), "{\"version\":1}\n")?;
    std::fs::write(wc.join("README.md"), README)?;
    std::fs::write(wc.join(".gitattributes"), "* -text\n")?;

    // config.toml: portable keys, newest change per key.
    let cfg_path = ctx.app_home.join("config.toml");
    if let Ok(text) = std::fs::read_to_string(&cfg_path) {
        let local = super::portable::flatten(&text, &map, &ctx.sync().machine_local)?;
        let bp = base_path(ctx, "config");
        let base = merge::load_base(&bp);
        let remote = merge::load_doc(&wc.join("config/stamps.json"));
        let m = merge::merge(&merge::stamp(&local, &base, now), &remote, &base, now);
        let mut set = BTreeMap::new();
        let mut removed = vec![];
        let mut refused_keys: Vec<String> = vec![];
        for k in m.to_home() {
            let e = &m.doc[k];
            if e.deleted {
                removed.push(k.clone());
            } else {
                set.insert(k.clone(), e.value.clone());
            }
        }
        if !set.is_empty() || !removed.is_empty() {
            let (out, ap) = super::portable::apply(&text, &set, &removed, &map)?;
            if out != text {
                super::write_file(&cfg_path, out.as_bytes())?;
                r.applied
                    .extend(ap.changed.iter().map(|k| format!("config {k}")));
            }
            r.refused.extend(ap.refused);
            refused_keys = ap.refused_keys;
        }
        super::write_file(
            &wc.join("config/portable.toml"),
            super::portable::render(&m.live()).as_bytes(),
        )?;
        super::write_file(&wc.join("config/stamps.json"), &merge::doc_bytes(&m.doc))?;
        // A refused key never counts as synced (its absence here is not a
        // delete).
        let mut b = m.base();
        for k in refused_keys {
            b.remove(&k);
        }
        pending.push((bp, b));
    }
    // accounts.json: no config dirs, no credentials.
    let accts: Vec<Value> = ctx
        .cfg
        .accounts
        .iter()
        .map(|a| {
            json!({"name": a.name, "label": a.label, "harness": a.harness, "color": a.color,
                   "args": a.args, "permission_mode": a.permission_mode})
        })
        .collect();
    write_json(&wc.join("accounts.json"), &Value::Array(accts))?;

    // Keyed stores.
    let out_fix = fix_paths(&map, true);
    let in_fix = fix_paths(&map, false);
    let mut pend_jobs: BTreeSet<String> =
        serde_json::from_value(read_json(&jobs_pending_path(ctx))).unwrap_or_default();
    for s in merge::STORES {
        let hp = ctx.app_home.join(s.file);
        let whole = read_json(&hp);
        if s.name == "jobs" {
            // Turned on here since: approved.
            for j in whole["jobs"].as_array().into_iter().flatten() {
                if j["enabled"] == true {
                    if let Some(id) = j["id"].as_str() {
                        pend_jobs.remove(id);
                    }
                }
            }
        }
        let local = merge::entries_of(s, &whole, &out_fix);
        let bp = base_path(ctx, s.name);
        let base = merge::load_base(&bp);
        let remote = merge::load_doc(&wc.join(format!("{}.json", s.name)));
        if whole.is_null() && remote.is_empty() {
            continue;
        }
        let m = merge::merge(&merge::stamp(&local, &base, now), &remote, &base, now);
        let mut nbase = m.base();
        if !m.to_home().is_empty() {
            let mut live = m.live();
            if s.name == "jobs" {
                for k in m.to_home() {
                    let new_here = !base.contains_key(k) || pend_jobs.contains(k);
                    if let Some(v) = live.get_mut(k) {
                        if new_here && v["enabled"] == true {
                            v["enabled"] = json!(false);
                            pend_jobs.insert(k.clone());
                            r.new_jobs_paused
                                .push(v["name"].as_str().unwrap_or(k).to_string());
                            // The paused copy is the synced one here, so it
                            // is not pushed back as a change.
                            if let Some(b) = nbase.get_mut(k) {
                                b.h = merge::hash(v);
                            }
                        }
                    }
                }
            }
            let outv = merge::with_entries(s, &whole, &live, &in_fix);
            let app_owned = super::APP_STORES.contains(&s.name);
            match ctx.link.as_ref().filter(|_| app_owned) {
                // The running app holds this store: it applies the
                // change to its own copy and saves it (a write here could
                // race its next save). Not applied: the base stays, and
                // the next sync hands it over again.
                Some(link) => {
                    let ents = merge::entries_of(s, &outv, &|_, _| {});
                    let set = m
                        .to_home()
                        .iter()
                        .map(|k| ((*k).clone(), ents.get(*k).cloned()))
                        .collect();
                    let ok = (link.0.apply)(super::StoreApply { store: s.name, set });
                    if !ok {
                        r.errors
                            .push(format!("{}: the app did not take the changes yet", s.name));
                        super::write_file(
                            &wc.join(format!("{}.json", s.name)),
                            &merge::doc_bytes(&m.doc),
                        )?;
                        continue;
                    }
                }
                None => write_json(&hp, &outv)?,
            }
            r.applied
                .push(format!("{} ({} entries)", s.name, m.to_home().len()));
        }
        super::write_file(
            &wc.join(format!("{}.json", s.name)),
            &merge::doc_bytes(&m.doc),
        )?;
        pending.push((bp, nbase));
    }
    let _ = write_json(&jobs_pending_path(ctx), &json!(pend_jobs));

    // git-bindings.json (Phase 1's): keyed by portable project path.
    sync_bindings(ctx, wc, &mut pending, r)?;

    // Long-lived tokens (opt in): only into a repo checked private now.
    super::tokens::sync_tokens(ctx, wc, r, &crate::acct_tokens::store(), &|| {
        super::tokens::repo_private(ctx)
    });

    // File sets.
    let c = collect::collect(ctx);
    r.left_out
        .extend(c.skipped.iter().map(|(a, b)| format!("{a}: {b}")));
    let mut areas: BTreeMap<String, Area> = BTreeMap::new();
    let acct_dirs: BTreeMap<String, PathBuf> = ctx
        .accounts()
        .into_iter()
        .map(|a| (a.name, a.dir))
        .collect();
    let root_of = |area: &str, memory: &BTreeMap<String, String>| -> Option<PathBuf> {
        let parts: Vec<&str> = area.splitn(3, '/').collect();
        match parts.as_slice() {
            ["claude", a] => acct_dirs.get(*a).cloned(),
            ["assistant", "conversations"] => Some(ctx.app_home.join("assistant/conversations")),
            ["memory", a, _] => {
                let cwd = map.to_local(memory.get(area)?)?;
                Some(
                    acct_dirs
                        .get(*a)?
                        .join("projects")
                        .join(crate::state::encode_project_dir(&cwd))
                        .join("memory"),
                )
            }
            _ => None,
        }
    };
    let mut memory_index: BTreeMap<String, String> =
        serde_json::from_value(read_json(&wc.join("memory/index.json"))).unwrap_or_default();
    memory_index.extend(c.memory_index.clone());
    for f in &c.files {
        areas
            .entry(f.area.clone())
            .or_insert_with(|| Area {
                name: f.area.clone(),
                root: root_of(&f.area, &memory_index),
                items: BTreeMap::new(),
                kind: Kind::Plain,
            })
            .items
            .insert(f.key.clone(), f.path.clone());
    }
    let mut known: Vec<String> = files::repo_areas(wc, "assistant");
    for a in acct_dirs.keys() {
        known.extend(
            files::repo_areas(wc, "claude")
                .into_iter()
                .filter(|x| x == &format!("claude/{a}")),
        );
        known.extend(files::repo_areas(wc, &format!("memory/{a}")));
    }
    for k in known {
        let root = root_of(&k, &memory_index);
        areas.entry(k.clone()).or_insert(Area {
            name: k,
            root,
            items: BTreeMap::new(),
            kind: Kind::Plain,
        });
    }
    // .env files of every known project.
    let mut env_index: BTreeMap<String, String> =
        serde_json::from_value(read_json(&wc.join("env/index.json"))).unwrap_or_default();
    for root in super::env::project_roots(ctx) {
        let port = map.to_portable(&root);
        let key = super::portable::path_key(&port);
        let name = format!("env/{key}");
        let files_here = super::env::env_files(&root, &ctx.never);
        if files_here.is_empty() && !env_index.contains_key(&key) {
            continue;
        }
        env_index.insert(key.clone(), port.clone());
        let a = areas.entry(name.clone()).or_insert(Area {
            name,
            root: Some(root.clone()),
            items: BTreeMap::new(),
            kind: Kind::Env,
        });
        for (rel, p) in files_here {
            if super::env::ignored(&root, &rel) == Some(false) {
                r.env_not_ignored.push(format!("{port}/{rel}"));
            }
            a.items.insert(rel, p);
        }
    }
    for (key, port) in &env_index {
        let name = format!("env/{key}");
        if areas.contains_key(&name) {
            continue;
        }
        let root = map
            .to_local(port)
            .filter(|p| p.is_dir() && ctx.refused(p).is_none());
        areas.insert(
            name.clone(),
            Area {
                name,
                root,
                items: BTreeMap::new(),
                kind: Kind::Env,
            },
        );
    }
    if !env_index.is_empty() {
        write_json(&wc.join("env/index.json"), &json!(env_index))?;
    }
    if !memory_index.is_empty() {
        write_json(&wc.join("memory/index.json"), &json!(memory_index))?;
    }
    for a in areas.values() {
        let bp = base_path(ctx, &format!("files-{}", a.name));
        let base = merge::load_base(&bp);
        let o = files::sync_area(a, wc, &base, now, &ctx.machine.name(), &ctx.never);
        if !o.to_home.is_empty() {
            r.applied
                .push(format!("{} ({} files)", a.name, o.to_home.len()));
        }
        r.env_kept_both.extend(
            o.kept_both
                .iter()
                .map(|(f, keys)| format!("{f}: changed {}", keys.join(", "))),
        );
        r.errors.extend(o.errors);
        pending.push((bp, o.base));
    }

    // Prompt history: the union of the lines.
    for (acct, p) in &c.histories {
        let rp = wc.join(format!("claude/{acct}/history.jsonl"));
        let local = collect::read_lines(p);
        let remote = collect::read_lines(&rp);
        let merged = union_lines(&local, &remote);
        if merged.len() != local.len() {
            super::write_file(p, &join_lines(&merged))?;
            r.applied.push(format!("claude/{acct}/history.jsonl"));
        }
        super::write_file(&rp, &join_lines(&merged))?;
    }

    // This machine.
    let mdir = wc.join("machines").join(&ctx.machine.id);
    write_json(
        &mdir.join("machine.json"),
        &json!({"label": ctx.machine.name(), "os": std::env::consts::OS, "home": ctx.home,
                "user": std::env::var("USER").unwrap_or_default(), "first_seen": ctx.machine.first_seen,
                "version": env!("CARGO_PKG_VERSION"), "seen": chrono::Utc::now().to_rfc3339()}),
    )?;
    write_json(
        &mdir.join("paths.json"),
        &json!({"home": ctx.home, "paths": ctx.sync().paths}),
    )?;
    let mut st = read_json(&ctx.app_home.join("state.json"));
    if !st.is_null() {
        out_fix("state", &mut st);
        if let Some(o) = st.as_object_mut() {
            o.remove("window");
            o.remove("deliveries");
        }
        write_json(&mdir.join("state.json"), &st)?;
    }

    // Conversations.
    sessions(ctx, wc, &c.sessions, board, r)?;

    // Machines and shards, for the Sync screen.
    r.machines = std::fs::read_dir(wc.join("machines"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let mut v = read_json(&e.path().join("machine.json"));
            v["id"] = json!(e.file_name().to_string_lossy());
            v
        })
        .collect();
    let used = shard_bytes(wc);
    let month = shards::this_month();
    let shard_list: Vec<Value> = used
        .iter()
        .map(|(repo, b)| {
            let m = repo
                .strip_prefix(&format!("{}-", ctx.sync().convos_prefix))
                .map(|x| x.get(..7).unwrap_or(x).to_string())
                .unwrap_or_default();
            json!({"repo": repo, "month": m, "bytes": b, "sealed": m < month})
        })
        .collect();
    write_json(&wc.join("shards.json"), &Value::Array(shard_list.clone()))?;
    r.shards = shard_list;

    // Projects that are not backed up (warnings only; nothing is done to
    // them).
    for root in super::env::project_roots(ctx) {
        let w = super::projects::check(&root);
        if !w.is_empty() {
            r.projects.push(super::projects::Health {
                path: map.to_portable(&root),
                warnings: w,
            });
        }
    }
    Ok(pending)
}

fn sync_bindings(
    ctx: &Ctx,
    wc: &Path,
    pending: &mut Vec<(PathBuf, Base)>,
    r: &mut Report,
) -> Result<()> {
    let hp = ctx.app_home.join("git-bindings.json");
    let whole = read_json(&hp);
    let remote = merge::load_doc(&wc.join("git/bindings.json"));
    if whole.is_null() && remote.is_empty() {
        return Ok(());
    }
    let map = ctx.paths();
    let mut local = BTreeMap::new();
    for (k, v) in whole["projects"].as_object().into_iter().flatten() {
        local.insert(map.portable_text(k), v.clone());
    }
    let bp = base_path(ctx, "bindings");
    let base = merge::load_base(&bp);
    let m = merge::merge(
        &merge::stamp(&local, &base, ctx.now_ms),
        &remote,
        &base,
        ctx.now_ms,
    );
    if !m.to_home().is_empty() {
        let mut out = if whole.is_object() {
            whole.clone()
        } else {
            json!({"version": 1})
        };
        let mut o = serde_json::Map::new();
        for (k, v) in m.live() {
            // Phase 1 keys bindings by `~/...`; other paths placed here.
            o.insert(map.local_text(&k), v);
        }
        out["projects"] = Value::Object(o);
        write_json(&hp, &out)?;
        r.applied.push("git bindings".into());
    }
    super::write_file(&wc.join("git/bindings.json"), &merge::doc_bytes(&m.doc))?;
    pending.push((bp, m.base()));
    Ok(())
}

fn ts(l: &[u8]) -> i64 {
    serde_json::from_slice::<Value>(l)
        .ok()
        .and_then(|v| v["timestamp"].as_i64())
        .unwrap_or(0)
}

/// Every line of both, once, in time order (the local order kept for
/// equal times).
pub fn union_lines(local: &[Vec<u8>], remote: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut seen: BTreeSet<&[u8]> = BTreeSet::new();
    let mut v: Vec<Vec<u8>> = vec![];
    for l in local.iter().chain(remote) {
        if seen.insert(l.as_slice()) {
            v.push(l.clone());
        }
    }
    if v.len() > local.len() {
        v.sort_by_key(|l| ts(l));
    }
    v
}

fn join_lines(v: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![];
    for l in v {
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    out
}

/// Bytes each conversation repo holds, from the manifests.
pub fn shard_bytes(wc: &Path) -> BTreeMap<String, u64> {
    let mut used: BTreeMap<String, u64> = BTreeMap::new();
    for m in all_manifests(wc) {
        for f in m.all_files() {
            for c in f.chunks {
                *used.entry(c.repo).or_default() += c.len;
            }
        }
    }
    used
}

pub fn manifest_path(wc: &Path, acct: &str, id: &str) -> PathBuf {
    wc.join("sessions").join(acct).join(format!("{id}.json"))
}

pub fn load_manifest(wc: &Path, acct: &str, id: &str) -> Option<Manifest> {
    serde_json::from_slice(&std::fs::read(manifest_path(wc, acct, id)).ok()?).ok()
}

pub fn all_manifests(wc: &Path) -> Vec<Manifest> {
    let mut v = vec![];
    for a in std::fs::read_dir(wc.join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for f in std::fs::read_dir(a.path()).into_iter().flatten().flatten() {
            if let Ok(m) =
                serde_json::from_slice::<Manifest>(&std::fs::read(f.path()).unwrap_or_default())
            {
                v.push(m);
            }
        }
    }
    v
}

/// Sessions whose CLI may be running here (never extended, replaced or
/// forked under it): the app's own list when the sync runs inside it,
/// else every session open in a tab.
fn running_here(ctx: &Ctx) -> BTreeSet<String> {
    match &ctx.link {
        Some(l) => l.running(),
        None => open_here(ctx),
    }
}

/// Forks made outside a sync (a lost lease, a reconcile): new id ->
/// the session it came from, for its manifest's `forked_from`.
pub fn forks_path(ctx: &Ctx) -> PathBuf {
    ctx.sync_dir().join("forks.json")
}

pub fn load_forks(ctx: &Ctx) -> BTreeMap<String, String> {
    serde_json::from_value(read_json(&forks_path(ctx))).unwrap_or_default()
}

pub fn note_fork(ctx: &Ctx, new_id: &str, from: &str) {
    let mut f = load_forks(ctx);
    f.insert(new_id.to_string(), from.to_string());
    let _ = write_json(&forks_path(ctx), &json!(f));
}

/// Sessions open in a tab here.
fn open_here(ctx: &Ctx) -> BTreeSet<String> {
    let st = read_json(&ctx.app_home.join("state.json"));
    let mut v = BTreeSet::new();
    for s in st["slots"].as_array().into_iter().flatten() {
        for t in s["tabs"].as_array().into_iter().flatten() {
            if let Some(id) = t["session_id"].as_str() {
                v.insert(id.to_string());
            }
        }
    }
    v
}

fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Most new transcript bytes one sync pushes.
pub const CYCLE_BYTES: u64 = 256 << 20;

/// A new chunk waiting for its conversation repo push.
struct NewChunk {
    repo: String,
    path: String,
    bytes: Vec<u8>,
}

/// One file's plan.
enum Plan {
    /// Keep (manifest entry as given).
    Keep(FileMan),
    /// Push new chunks, then the entry.
    Push(FileMan, Vec<NewChunk>),
}

/// Plan one file against its remote entry.
#[allow(clippy::too_many_arguments)]
fn plan_file(
    ctx: &Ctx,
    acct: &str,
    id: &str,
    name: &str,
    p: &Path,
    remote: &FileMan,
    led: &LedFile,
    used: &mut BTreeMap<String, u64>,
    r: &mut Report,
) -> Result<(Cmp, Plan)> {
    let len = std::fs::metadata(p)?.len();
    let mt = mtime_ms(p);
    // What is known to be in the conversation repos: the remote entry, or
    // this machine's own pushes past it (a manifest push that failed).
    let known = if led.chunks.len() > remote.chunks.len() && led.chunks.starts_with(&remote.chunks)
    {
        FileMan {
            name: name.to_string(),
            len: led.chunks.iter().map(|c| c.len).sum(),
            sha256: led.sha256.clone(),
            chunks: led.chunks.clone(),
        }
    } else {
        remote.clone()
    };
    if led.len == len
        && led.mtime_ms == mt
        && known.len == led.len
        && known.sha256 == led.sha256
        && !known.chunks.is_empty()
    {
        return Ok((Cmp::Equal, Plan::Keep(known)));
    }
    let read = |c: &Chunk| shards::read(ctx, c);
    let cmp = if known.chunks.is_empty() && known.len == 0 {
        if len == 0 {
            Cmp::Equal
        } else {
            Cmp::LocalAhead
        }
    } else {
        chunks::compare(p, len, &known, &read)?
    };
    if cmp != Cmp::LocalAhead {
        return Ok((cmp, Plan::Keep(known)));
    }
    // One sync uploads at most CYCLE_BYTES (a first upload goes in
    // batches).
    if r.bytes_pushed >= CYCLE_BYTES {
        r.more = true;
        return Ok((Cmp::Equal, Plan::Keep(known)));
    }
    // New bytes: up to the last line end while the file is being written.
    let from = known.len;
    let mut new = chunks::read_range(p, from, len - from)?;
    let quiet = ctx.now_ms - mt > 30_000;
    if !quiet && name.ends_with(".jsonl") {
        match new.iter().rposition(|&b| b == b'\n') {
            Some(i) => new.truncate(i + 1),
            None => new.clear(),
        }
    }
    if new.is_empty() {
        return Ok((Cmp::Equal, Plan::Keep(known)));
    }
    r.secret_warnings += super::secrets::count(&new);
    let end = from + new.len() as u64;
    let mut f = known.clone();
    let max = (ctx.sync().chunk_mb.max(1) << 20) as usize;
    let mut start = 0usize;
    let mut nc = vec![];
    for cut in chunks::split_points(&new, max) {
        let b = new[start..cut].to_vec();
        let off = from + start as u64;
        let repo = shards::pick(ctx, &shards::this_month(), used);
        *used.entry(repo.clone()).or_default() += b.len() as u64;
        let path = chunks::chunk_path(acct, id, name, off);
        f.chunks.push(Chunk {
            repo: repo.clone(),
            path: path.clone(),
            off,
            len: b.len() as u64,
            sha256: super::sha_hex(&b),
        });
        nc.push(NewChunk {
            repo,
            path,
            bytes: b,
        });
        start = cut;
    }
    f.len = end;
    f.sha256 = chunks::scan(p, &[], end)?.1;
    r.bytes_pushed += new.len() as u64;
    Ok((Cmp::LocalAhead, Plan::Push(f, nc)))
}

pub(crate) fn led_key(acct: &str, id: &str, name: &str) -> String {
    format!("{acct}/{id}/{name}")
}

/// Sync every conversation: chunks first, then manifests.
fn sessions(
    ctx: &Ctx,
    wc: &Path,
    local: &[SessionRef],
    board: Option<&super::lease::Board>,
    r: &mut Report,
) -> Result<()> {
    let mut ledger = load_ledger(ctx);
    let mut used = shard_bytes(wc);
    let open = running_here(ctx);
    let forks = load_forks(ctx);
    // May this machine push a session's new bytes? Only its owner (or
    // anyone, for a session nobody leased).
    let writable = |id: &str| {
        board.is_none_or(|b| {
            b.status(id, &ctx.machine.id, ctx.now_ms, ctx.lease_ms())
                .writable()
        })
    };
    let map = ctx.paths();
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut queue: Vec<(SessionRef, Option<String>)> =
        local.iter().cloned().map(|s| (s, None)).collect();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut new_chunks: Vec<NewChunk> = vec![];
    let mut manifests: Vec<(Manifest, Vec<(String, LedFile)>)> = vec![];
    let mut qi = 0;
    while qi < queue.len() {
        let (s, forked_from) = queue[qi].clone();
        qi += 1;
        seen.insert((s.account.clone(), s.id.clone()));
        let key = |name: &str| led_key(&s.account, &s.id, name);
        let files = s.files();
        // The vault: a session naming it stays here (unless included).
        if !ctx.sync().include_sessions.contains(&s.id) {
            let mut vault = false;
            for (name, p) in &files {
                let l = ledger.entry(key(name)).or_default();
                let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                if !l.vault && len > l.scanned {
                    l.vault = collect::mentions_vault(p, l.scanned.saturating_sub(64));
                    l.scanned = len;
                }
                vault |= l.vault;
            }
            if vault {
                r.left_out
                    .push(format!("{}: mentions {}", s.id, super::VAULT));
                continue;
            }
        }
        r.sessions += 1;
        let remote = load_manifest(wc, &s.account, &s.id);
        let mut man = remote.clone().unwrap_or_else(|| Manifest {
            provider: s.provider.to_string(),
            account: s.account.clone(),
            session: s.id.clone(),
            ..Default::default()
        });
        if forked_from.is_some() {
            man.forked_from = forked_from.clone();
        } else if man.forked_from.is_none() {
            man.forked_from = forks.get(&s.id).cloned();
        }
        man.cwd = s.cwd.as_ref().map(|c| map.to_portable(c)).or(man.cwd);
        let remote_files: BTreeMap<String, FileMan> = remote
            .as_ref()
            .map(|m| {
                m.all_files()
                    .into_iter()
                    .map(|f| (f.name.clone(), f))
                    .collect()
            })
            .unwrap_or_default();
        let mut out_files = vec![];
        let mut leds = vec![];
        let mut changed = false;
        let mut diverged = false;
        let mut behind = false;
        let mut fresh_chunks = vec![];
        for (name, p) in &files {
            let rf = remote_files.get(name).cloned().unwrap_or(FileMan {
                name: name.clone(),
                ..Default::default()
            });
            let led = ledger.get(&key(name)).cloned().unwrap_or_default();
            let (cmp, plan) =
                match plan_file(ctx, &s.account, &s.id, name, p, &rf, &led, &mut used, r) {
                    Ok(x) => x,
                    Err(e) => {
                        r.errors.push(format!("{}: {e:#}", s.id));
                        (Cmp::Equal, Plan::Keep(rf.clone()))
                    }
                };
            let whole_file = !name.ends_with(".jsonl");
            match (cmp, plan) {
                (Cmp::Diverged, _)
                    if whole_file && led.sha256 == rf.sha256 && !rf.chunks.is_empty() =>
                {
                    // A summary rewritten here since our last push: replace.
                    let b = std::fs::read(p)?;
                    let path = format!(
                        "{}/{}/{name}/v{}",
                        s.account,
                        s.id,
                        &super::sha_hex(&b)[..12]
                    );
                    let repo = shards::pick(ctx, &shards::this_month(), &used);
                    let f = FileMan {
                        name: name.clone(),
                        len: b.len() as u64,
                        sha256: super::sha_hex(&b),
                        chunks: vec![Chunk {
                            repo: repo.clone(),
                            path: path.clone(),
                            off: 0,
                            len: b.len() as u64,
                            sha256: super::sha_hex(&b),
                        }],
                    };
                    fresh_chunks.push(NewChunk {
                        repo,
                        path,
                        bytes: b,
                    });
                    leds.push((key(name), led_from(&f, p)));
                    out_files.push(f);
                    changed = true;
                }
                (Cmp::Diverged | Cmp::RemoteAhead, _) if whole_file => {
                    if open.contains(&s.id) {
                        behind = true;
                        out_files.push(rf);
                        continue;
                    }
                    let b = chunks::remote_bytes(&rf, 0, &|c| shards::read(ctx, c))?;
                    chunks::place(p, &b)?;
                    leds.push((key(name), led_from(&rf, p)));
                    out_files.push(rf);
                    r.extended += 1;
                }
                (Cmp::Diverged, _) => {
                    diverged = true;
                    out_files.push(rf);
                }
                (Cmp::RemoteAhead, _) => {
                    if open.contains(&s.id) {
                        behind = true;
                        out_files.push(rf);
                        continue;
                    }
                    let len = std::fs::metadata(p)?.len();
                    let b = chunks::remote_bytes(&rf, len, &|c| shards::read(ctx, c))?;
                    chunks::extend(p, len, &b)?;
                    leds.push((key(name), led_from(&rf, p)));
                    out_files.push(rf);
                    r.extended += 1;
                }
                // New bytes of a session another machine owns (it took
                // over, or this machine was away): never pushed under its
                // id. Running here: the app's lost lease flow stops the
                // CLI first; otherwise they become a fork now.
                (_, Plan::Push(..)) if !writable(&s.id) => {
                    if open.contains(&s.id) {
                        behind = true;
                    } else {
                        diverged = true;
                    }
                    out_files.push(rf);
                }
                (_, Plan::Push(f, nc)) => {
                    fresh_chunks.extend(nc);
                    leds.push((key(name), led_from(&f, p)));
                    out_files.push(f);
                    changed = true;
                }
                (_, Plan::Keep(f)) => {
                    if f != rf {
                        changed = true;
                    }
                    leds.push((key(name), led_from(&f, p)));
                    out_files.push(f);
                }
            }
        }
        if behind {
            r.held.push(s.id.clone());
        }
        if diverged {
            if open.contains(&s.id) {
                r.held.push(format!("{} (diverged)", s.id));
                continue;
            }
            // Never merged: this machine's version becomes a new session,
            // the remote one takes the original id.
            let Some(rm) = remote.clone() else { continue };
            let (nid, np) = if s.provider == "claude" {
                let head =
                    chunks::read_range(&s.path, 0, s.bytes.min(256 << 10)).unwrap_or_default();
                let all = std::fs::read(&s.path).unwrap_or_default();
                let title = chunks::claude_title(&head, chunks::last_title(&all));
                chunks::fork_claude(&s.path, &s.id, &title, &ctx.machine.name(), &date)?
            } else {
                chunks::fork_grok(&s.path, &ctx.machine.name(), &date)?
            };
            for f in rm.all_files() {
                let dst = if s.provider == "claude" {
                    s.path.clone()
                } else {
                    s.path.join(&f.name)
                };
                let b = chunks::remote_bytes(&f, 0, &|c| shards::read(ctx, c))?;
                chunks::place(&dst, &b)?;
                ledger.insert(key(&f.name), led_from(&f, &dst));
            }
            r.forked.push(format!("{} -> {nid}", s.id));
            let fork = SessionRef {
                id: nid,
                path: np.clone(),
                bytes: std::fs::metadata(&np).map(|m| m.len()).unwrap_or(0),
                ..s.clone()
            };
            queue.push((fork, Some(s.id.clone())));
            continue;
        }
        if !changed && remote.is_some() {
            for (k, l) in leds {
                ledger.insert(k, l);
            }
            continue;
        }
        if s.provider == "claude" {
            if let Some(f) = out_files.into_iter().next() {
                man.set_main(f);
            }
            if man.title.is_empty() {
                let head =
                    chunks::read_range(&s.path, 0, s.bytes.min(256 << 10)).unwrap_or_default();
                man.title = chunks::claude_title(&head, None);
            }
        } else {
            man.files = out_files;
            if let Ok(v) = serde_json::from_slice::<Value>(
                &std::fs::read(s.path.join("summary.json")).unwrap_or_default(),
            ) {
                man.title = v["generated_title"].as_str().unwrap_or("").to_string();
            }
        }
        if !fresh_chunks.is_empty() {
            r.sessions_pushed += 1;
        }
        man.updated = chrono::Utc::now().to_rfc3339();
        man.machine = ctx.machine.id.clone();
        new_chunks.extend(fresh_chunks);
        manifests.push((man, leds));
    }
    r.remote_only = all_manifests(wc)
        .iter()
        .filter(|m| !seen.contains(&(m.account.clone(), m.session.clone())))
        .count();
    // Chunks first, per conversation repo.
    let mut by_repo: BTreeMap<String, Vec<(String, Vec<u8>)>> = BTreeMap::new();
    for c in new_chunks {
        by_repo.entry(c.repo).or_default().push((c.path, c.bytes));
    }
    let mut failed: BTreeSet<String> = BTreeSet::new();
    for (repo, files) in by_repo {
        if let Err(e) = shards::add(ctx, &repo, &files) {
            r.errors.push(format!("{repo}: {e:#}"));
            failed.insert(repo);
        }
    }
    // Then the manifests of what made it.
    for (man, leds) in manifests {
        if man
            .all_files()
            .iter()
            .any(|f| f.chunks.iter().any(|c| failed.contains(&c.repo)))
        {
            continue;
        }
        for (k, l) in leds {
            ledger.insert(k, l);
        }
        let p = manifest_path(wc, &man.account, &man.session);
        write_json(&p, &serde_json::to_value(&man)?)?;
    }
    save_ledger(ctx, &ledger);
    Ok(())
}

pub(crate) fn led_from(f: &FileMan, p: &Path) -> LedFile {
    let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    LedFile {
        len: if len == f.len { len } else { u64::MAX },
        mtime_ms: mtime_ms(p),
        sha256: f.sha256.clone(),
        chunks: f.chunks.clone(),
        scanned: len,
        vault: false,
    }
}

/// Place one remote session here (a restore, or a session opened on
/// demand). Returns its local path.
pub fn place_session(ctx: &Ctx, m: &Manifest) -> Result<PathBuf> {
    let map = ctx.paths();
    let acct = ctx
        .accounts()
        .into_iter()
        .find(|a| a.name == m.account)
        .with_context(|| format!("no account {} here", m.account))?;
    let cwd = m
        .cwd
        .as_deref()
        .and_then(|c| map.to_local(c))
        .with_context(|| {
            format!(
                "{}: its folder has no place on this machine (add it to [sync.paths])",
                m.session
            )
        })?;
    if let Some(why) = ctx.refused(&cwd) {
        bail!("{why}");
    }
    let mut ledger = load_ledger(ctx);
    let out = if m.provider == "claude" {
        let p = acct
            .dir
            .join("projects")
            .join(crate::state::encode_project_dir(&cwd))
            .join(format!("{}.jsonl", m.session));
        let f = m.main();
        if !p.exists() {
            let b = chunks::remote_bytes(&f, 0, &|c| shards::read(ctx, c))?;
            chunks::place(&p, &b)?;
        }
        ledger.insert(led_key(&m.account, &m.session, &f.name), led_from(&f, &p));
        p
    } else {
        let d = acct
            .dir
            .join("sessions")
            .join(crate::demo_agent::grok_dir_name(&cwd))
            .join(&m.session);
        for f in &m.files {
            let p = d.join(&f.name);
            if !p.exists() {
                let b = chunks::remote_bytes(f, 0, &|c| shards::read(ctx, c))?;
                chunks::place(&p, &b)?;
            }
            ledger.insert(led_key(&m.account, &m.session, &f.name), led_from(f, &p));
        }
        d
    };
    save_ledger(ctx, &ledger);
    Ok(out)
}
