//! `godterm sync init` (the first machine) and `godterm sync clone` (a
//! new one), and `sync doctor` (docs/SYNC.md 3).
//!
//! init on GitHub creates `<owner>/godterm-state` private, only after an
//! explicit typed yes on a terminal. clone never copies a machine id,
//! OAuth login or keychain item: accounts arrive with no login, each
//! logged in again from GodTerm.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use super::state_repo::{self, Report};
use super::{git, Ctx, Machine, SyncCfg};

/// Ask a yes on the terminal: only a typed "yes" counts, and there is no
/// yes without a terminal.
pub fn ask_yes(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!("{question}\n(no terminal to answer on: nothing done)");
        return false;
    }
    print!("{question} Type yes to go on: ");
    let _ = std::io::stdout().flush();
    let mut l = String::new();
    let _ = std::io::stdin().lock().read_line(&mut l);
    l.trim() == "yes"
}

/// Write `[sync]` keys into config.toml (comments kept), making the file
/// when there is none yet.
pub fn save_sync_cfg(app_home: &Path, s: &SyncCfg) -> Result<()> {
    let p = app_home.join("config.toml");
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(_) => toml::to_string_pretty(&crate::config::Config::default())?,
    };
    let mut doc: toml_edit::DocumentMut = text.parse().context("config.toml does not parse")?;
    let v: toml_edit::DocumentMut = toml::to_string_pretty(&json!({ "sync": s }))?.parse()?;
    doc["sync"] = v["sync"].clone();
    let out = doc.to_string();
    crate::config::Config::parse(&out)?;
    std::fs::create_dir_all(app_home)?;
    super::write_file(&p, out.as_bytes())?;
    Ok(())
}

/// The context again after config.toml changed.
pub fn reload(ctx: &Ctx) -> Result<Ctx> {
    let text = std::fs::read_to_string(ctx.app_home.join("config.toml"))?;
    let cfg = crate::config::Config::parse(&text)?;
    let mut c = Ctx::new(ctx.home.clone(), ctx.app_home.clone(), cfg);
    c.exe = ctx.exe.clone();
    c.now_ms = ctx.now_ms;
    // Still inside the running app: its stores keep going through it.
    c.link = ctx.link.clone();
    c.never.extend(ctx.never.iter().cloned());
    c.never.sort();
    c.never.dedup();
    Ok(c)
}

/// No owner or account set: the one given, else the first gh login
/// (`gh`, as `gh auth status` lists them).
pub fn resolve_owner(sc: &mut SyncCfg, gh: &[String]) {
    if sc.account.trim().is_empty() {
        sc.account = if sc.owner.trim().is_empty() {
            gh.first().cloned().unwrap_or_default()
        } else {
            sc.owner.clone()
        };
    }
    if sc.owner.trim().is_empty() {
        sc.owner = sc.account.clone();
    }
}

/// The first machine: create the state repo (private, after a yes) and
/// push the first collection.
pub fn init(ctx: &Ctx, sc: SyncCfg, yes: &dyn Fn(&str) -> bool) -> Result<Report> {
    let mut sc = sc;
    sc.enabled = true;
    sc.paused = false;
    resolve_owner(&mut sc, &git::gh_accounts());
    let mut c = ctx.clone();
    c.cfg.sync = sc.clone();
    let r = c.remote();
    if sc.owner.is_empty() {
        bail!("no owner for the state repo: run godterm sync init --owner NAME");
    }
    if !git::valid_account(&sc.account) || !git::valid_account(&sc.owner) {
        bail!("bad owner or account name");
    }
    if r.is_github() {
        let have = git::gh_accounts();
        if !have.contains(&sc.account) {
            bail!(
                "gh has no github.com login for {} (run: gh auth login --hostname github.com, then try again)",
                sc.account
            );
        }
    }
    if r.exists(&sc.owner, &sc.state_repo)? {
        bail!(
            "{}/{} already exists: on this machine use godterm sync clone {}/{}",
            sc.owner,
            sc.state_repo,
            sc.owner,
            sc.state_repo
        );
    }
    if r.is_github() {
        let p = super::plan::plan(&c);
        let q = format!(
            "{}\nCreate the PRIVATE repo {}/{} on GitHub (account {}), and from now on private monthly repos {}/{}-YYYY-MM for conversations? They hold your config, .env files and conversations as is.",
            p.text(),
            sc.owner, sc.state_repo, sc.account, sc.owner, sc.convos_prefix
        );
        if !yes(&q) {
            bail!("nothing created");
        }
        sc.auto_create_shards = true;
    }
    r.create_private(&sc.owner, &sc.state_repo)?;
    save_sync_cfg(&ctx.app_home, &sc)?;
    let c = reload(&c)?;
    let wc = state_repo::open_wc(&c)?;
    state_repo::write_heartbeat(&c, &wc)?;
    state_repo::sync_now(&c)
}

/// Options for `sync clone`.
#[derive(Debug, Clone, Default)]
pub struct CloneOpts {
    pub label: Option<String>,
    /// Start with that machine's tabs (its label or id; "" asks none).
    pub tabs_from: Option<String>,
    pub account: Option<String>,
    pub remote: Option<String>,
}

/// What a clone did, for the summary.
#[derive(Debug, Clone, Default)]
pub struct Cloned {
    pub report: Report,
    pub projects: Vec<String>,
    pub placed: Vec<String>,
    pub tabs: usize,
    pub logins: Vec<String>,
    pub missing_paths: Vec<String>,
}

/// A new machine: clone the state repo and restore from it.
pub fn clone(ctx: &Ctx, full: &str, o: &CloneOpts) -> Result<Cloned> {
    let (owner, name) = full
        .split_once('/')
        .filter(|(a, b)| git::valid_account(a) && !b.is_empty())
        .context("give the state repo as owner/name")?;
    let mut sc = ctx.cfg.sync.clone();
    sc.enabled = true;
    sc.owner = owner.into();
    sc.state_repo = name.into();
    sc.account = o.account.clone().unwrap_or_else(|| owner.to_string());
    if let Some(r) = &o.remote {
        sc.remote = r.clone();
    }
    // A fresh id for this machine, never a copied one.
    let mut m = Machine::load_or_new(&ctx.app_home);
    if let Some(l) = &o.label {
        m.label = l.clone();
        m.save(&ctx.app_home)?;
    }
    let mut c = ctx.clone();
    c.cfg.sync = sc.clone();
    c.machine = m;
    if c.remote().is_github() && !git::gh_accounts().contains(&sc.account) {
        bail!(
            "gh has no github.com login for {} (run: gh auth login --hostname github.com)",
            sc.account
        );
    }
    // A state repo that reads as anything but private is never used: it
    // holds .env files and conversations as is.
    if let Some(v) = c.remote().visibility(owner, name) {
        if v != "PRIVATE" {
            bail!("{owner}/{name} is {v}, not private: GodTerm only syncs with a private repo (make it private on GitHub first)");
        }
    }
    let wc = state_repo::open_wc(&c)?;
    save_sync_cfg(&ctx.app_home, &sc)?;
    let mut c = reload(&c)?;
    // The pulled config wins over this fresh machine's defaults.
    trust_remote(&c, &wc)?;
    let mut out = Cloned::default();
    // Paths: every @name the other machines use needs a folder here.
    let map = c.paths();
    let mut missing: BTreeSet<String> = BTreeSet::new();
    for m in state_repo::all_manifests(&wc) {
        if let Some(cw) = &m.cwd {
            if map.to_local(cw).is_none() {
                missing.insert(cw.split('/').next().unwrap_or(cw).to_string());
            }
        }
    }
    out.missing_paths = missing.into_iter().collect();
    // First pass: config, accounts, stores (projects listed).
    let r1 = state_repo::sync_now(&c)?;
    c = reload(&c)?;
    for a in c.accounts() {
        let _ = std::fs::create_dir_all(&a.dir);
        out.logins.push(format!(
            "{}: log in from GodTerm (its tab runs the {} login; logins are never copied)",
            a.name, a.provider
        ));
    }
    // Projects with a repo: cloned to their mapped folder (read only for
    // existing ones: fetch, never anything else).
    out.projects = clone_projects(&c, &wc);
    // Conversations: the hot set now, the rest on demand.
    let hot = hot_ids(&c, &wc);
    for m in state_repo::all_manifests(&wc) {
        let recent = chrono::DateTime::parse_from_rfc3339(&m.updated)
            .map(|t| c.now_ms - t.timestamp_millis() < c.sync().hot_days as i64 * 86_400_000)
            .unwrap_or(false);
        if !(recent || hot.contains(&m.session)) {
            continue;
        }
        match state_repo::place_session(&c, &m) {
            Ok(p) => out
                .placed
                .push(format!("{} -> {}", m.session, crate::config::tilde(&p))),
            Err(e) => out.report.errors.push(format!("{}: {e:#}", m.session)),
        }
    }
    // Tabs of another machine (fresh tab ids, so nothing clashes).
    if let Some(from) = &o.tabs_from {
        out.tabs = take_tabs(&c, &wc, from)?;
    }
    state_repo::write_heartbeat(&c, &wc)?;
    // Second pass: .env files (projects are there now), this machine.
    let mut r2 = state_repo::sync_now(&c)?;
    r2.applied.splice(0..0, r1.applied);
    r2.errors.append(&mut out.report.errors);
    out.report = r2;
    Ok(out)
}

/// Mark this machine's config as "synced as is", so every key the state
/// repo has wins over a fresh machine's defaults.
fn trust_remote(c: &Ctx, wc: &Path) -> Result<()> {
    let Ok(text) = std::fs::read_to_string(c.app_home.join("config.toml")) else {
        return Ok(());
    };
    let local = super::portable::flatten(&text, &c.paths(), &c.sync().machine_local)?;
    let remote = super::merge::load_doc(&wc.join("config/stamps.json"));
    let base: super::merge::Base = local
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                super::merge::BaseEnt {
                    h: super::merge::hash(v),
                    u: remote.get(k).map(|e| e.updated - 1).unwrap_or(0),
                    d: false,
                },
            )
        })
        .collect();
    super::merge::save_base(&c.sync_dir().join("base/config.json"), &base)?;
    Ok(())
}

/// Sessions in the other machines' tabs, the archive and jobs.
fn hot_ids(c: &Ctx, wc: &Path) -> BTreeSet<String> {
    let mut v: BTreeSet<String> = super::collect::hot_set(&c.app_home)
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    for e in std::fs::read_dir(wc.join("machines"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let st: Value = std::fs::read(e.path().join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        for s in st["slots"].as_array().into_iter().flatten() {
            for t in s["tabs"].as_array().into_iter().flatten() {
                if let Some(id) = t["session_id"].as_str() {
                    v.insert(id.to_string());
                }
            }
        }
    }
    v
}

/// Clone bound projects to their mapped folders. An existing folder with
/// the same remote is only fetched; anything else there is reported and
/// left alone. Nothing is ever committed or pushed.
fn clone_projects(c: &Ctx, wc: &Path) -> Vec<String> {
    let mut out = vec![];
    let doc = super::merge::load_doc(&wc.join("git/bindings.json"));
    let map = c.paths();
    for (port, e) in doc {
        if e.deleted {
            continue;
        }
        let v = &e.value;
        let (Some(owner), Some(repo)) = (v["owner"].as_str(), v["repo"].as_str()) else {
            out.push(format!(
                "{port}: no repo bound (not backed up: no GitHub repo)"
            ));
            continue;
        };
        if v["state"] == "exclude" {
            continue;
        }
        let Some(dst) = map.to_local(&port) else {
            out.push(format!(
                "{port}: no folder for it here (add it to [sync.paths])"
            ));
            continue;
        };
        if let Some(why) = c.refused(&dst) {
            out.push(format!("{port}: refused ({why})"));
            continue;
        }
        let account = v["account"]
            .as_str()
            .unwrap_or(&c.sync().account)
            .to_string();
        let remote = match c.remote() {
            git::Remote::GitHub { exe, .. } => git::Remote::GitHub { account, exe },
            l => l,
        };
        let url = remote.url(owner, repo);
        let cred = remote.cred();
        if dst.exists() {
            let cur = std::process::Command::new("git")
                .arg("-C")
                .arg(&dst)
                .args(["remote", "get-url", "origin"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let same = cur
                .as_deref()
                .is_some_and(|u| u.trim_end_matches(".git") == url.trim_end_matches(".git"));
            if same {
                let r = git::run(&dst, &["fetch", "-q", "origin"], cred.as_ref());
                out.push(format!(
                    "{port}: there already, fetched{}",
                    if r.is_ok() { "" } else { " (fetch failed)" }
                ));
            } else {
                out.push(format!(
                    "{port}: the folder is there with another remote or no repo: left alone"
                ));
            }
            continue;
        }
        let parent = dst
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| c.home.clone());
        let _ = std::fs::create_dir_all(&parent);
        let ds = dst.to_string_lossy().into_owned();
        match git::run(&parent, &["clone", "-q", &url, &ds], cred.as_ref()) {
            Ok(_) => out.push(format!("{port}: cloned from {owner}/{repo}")),
            Err(e) => out.push(format!("{port}: clone failed: {e:#}")),
        }
    }
    out
}

/// Start with another machine's tabs: its state.json with paths mapped
/// here and fresh tab ids. Only into a machine with no tabs yet.
pub fn take_tabs(c: &Ctx, wc: &Path, from: &str) -> Result<usize> {
    let here = c.app_home.join("state.json");
    let cur: Value = std::fs::read(&here)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if cur["slots"].as_array().is_some_and(|s| {
        s.iter()
            .any(|x| x["tabs"].as_array().is_some_and(|t| !t.is_empty()))
    }) {
        bail!("this machine has tabs already: not replaced");
    }
    let mut src: Option<PathBuf> = None;
    for e in std::fs::read_dir(wc.join("machines"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if e.file_name().to_string_lossy() == c.machine.id {
            continue;
        }
        let m: Value = std::fs::read(e.path().join("machine.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let id = e.file_name().to_string_lossy().into_owned();
        if from.is_empty()
            || from == "auto"
            || m["label"] == from
            || id == from
            || id.starts_with(from)
        {
            src = Some(e.path().join("state.json"));
            break;
        }
    }
    let src = src.context("no such machine in the state repo")?;
    let mut st: Value = serde_json::from_slice(&std::fs::read(&src)?)?;
    let map = c.paths();
    let local = |s: &str| {
        map.to_local(s)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.to_string())
    };
    let mut n = 0;
    for s in st["slots"].as_array_mut().into_iter().flatten() {
        for t in s["tabs"].as_array_mut().into_iter().flatten() {
            if let Some(cw) = t["cwd"].as_str() {
                t["cwd"] = json!(local(cw));
            }
            if let Some(o) = t.as_object_mut() {
                o.remove("uid");
                o.remove("was_working");
                o.remove("working_at");
                o.remove("finished");
                o.remove("browser");
            }
            n += 1;
        }
    }
    for x in st["closed_tabs"].as_array_mut().into_iter().flatten() {
        if let Some(cw) = x["cwd"].as_str() {
            x["cwd"] = json!(local(cw));
        }
    }
    for r in st["recents"].as_array_mut().into_iter().flatten() {
        if let Some(cw) = r["path"].as_str() {
            r["path"] = json!(local(cw));
        }
    }
    if let Some(o) = st.as_object_mut() {
        o.remove("next_uid");
        o.remove("window");
        o.remove("deliveries");
    }
    super::write_file(&here, &serde_json::to_vec_pretty(&st)?)?;
    Ok(n)
}

/// `sync doctor`: what is set up and what is missing.
pub fn doctor(c: &Ctx) -> Vec<(bool, String)> {
    let mut v = vec![];
    let have = |p: &str| {
        std::process::Command::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    v.push((have("git"), "git installed".to_string()));
    let s = c.sync();
    v.push((
        s.enabled,
        format!("sync on (state repo {}/{})", s.owner, s.state_repo),
    ));
    let r = c.remote();
    if r.is_github() {
        v.push((have("gh"), "gh installed".into()));
        let accts = git::gh_accounts();
        v.push((
            accts.contains(&s.account),
            format!(
                "gh logged in as {} (never switched; tokens read per call)",
                s.account
            ),
        ));
    } else {
        v.push((
            true,
            format!("remote: {} (a local stand-in for GitHub)", s.remote),
        ));
    }
    v.push((
        git::is_sync_repo(&c.state_dir()),
        "state repo working copy".into(),
    ));
    v.push((
        c.never.iter().any(|d| d == Path::new(super::VAULT)),
        format!("{} is never synced", super::VAULT),
    ));
    v.push((
        !c.machine.id.is_empty(),
        format!("this machine: {} ({})", c.machine.name(), c.machine.short()),
    ));
    match Report::load(c) {
        Some(rep) => {
            v.push((
                rep.ok,
                format!(
                    "last sync {}{}",
                    super::ago(rep.at_ms, c.now_ms),
                    rep.error.map(|e| format!(": {e}")).unwrap_or_default()
                ),
            ));
            for p in rep.projects.iter().take(20) {
                v.push((false, format!("{}: {}", p.path, p.warnings.join(", "))));
            }
        }
        None => v.push((false, "no sync has run yet".into())),
    }
    let map = c.paths();
    let mut unmapped = BTreeSet::new();
    for m in state_repo::all_manifests(&c.state_dir()) {
        if let Some(cw) = m.cwd {
            if map.to_local(&cw).is_none() {
                unmapped.insert(cw.split('/').next().unwrap_or("").to_string());
            }
        }
    }
    for u in unmapped {
        v.push((
            false,
            format!("{u} has no folder here: add it under [sync.paths]"),
        ));
    }
    for a in c.accounts() {
        v.push((
            a.dir.is_dir(),
            format!(
                "account {} ({}) at {}",
                a.name,
                a.provider,
                crate::config::tilde(&a.dir)
            ),
        ));
    }
    v
}
