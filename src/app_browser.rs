//! BrowserSkill in the app: a passive, rate limited status poll on a
//! background thread, the `list_browsers` / `browser_status` tools, and
//! `install_browser_skill` (the skill folder copied into chosen accounts,
//! `Bash(bsk:*)` allowed in their settings.json, both after one yes).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::browser::{self, AccountRef, Env, Snapshot};
use crate::browser_skill::{self as skill, Merge, BSK_RULE};
use crate::harness::Harness;

/// A poll is not started again sooner than this (the screen being open or
/// the assistant asking over and over does not matter).
const MIN_GAP: Duration = Duration::from_secs(30);
/// A cached answer older than this is refreshed in the background.
const STALE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct BrowserState {
    snap: Arc<Mutex<Option<Snapshot>>>,
    inflight: Arc<AtomicBool>,
    last_start: Option<Instant>,
    /// Settings: the first click of "Install the browser skill" waits for
    /// its second.
    pub confirm: Option<Instant>,
    /// Tests (and only tests) say where things are; the real machine's
    /// folders are never looked at in a test.
    pub env: Option<Env>,
    /// The Browser dialog's quick look (tab_browser) and when it started.
    pub quick: Arc<Mutex<Option<crate::tab_browser::Quick>>>,
    pub quick_busy: Arc<AtomicBool>,
    pub quick_at: Option<Instant>,
    /// Tabs waiting for the user to add or turn on the extension: (tab
    /// id, since).
    pub pending: Vec<(u64, Instant)>,
    /// A line the Browser dialog shows under the profiles.
    pub note: Option<String>,
    /// Each bound tab's last bsk call, and when it was looked at.
    pub activity: std::cell::RefCell<
        std::collections::HashMap<u64, (Instant, Option<crate::tab_browser::BskAction>)>,
    >,
}

impl BrowserState {
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.snap.lock().ok().and_then(|g| g.clone())
    }
    pub fn quick_last_result(&self) -> Option<crate::tab_browser::Quick> {
        self.quick.lock().ok().and_then(|g| g.clone())
    }
    #[cfg(test)]
    pub fn set_quick(&self, q: crate::tab_browser::Quick) {
        *self.quick.lock().unwrap() = Some(q);
    }
    #[cfg(test)]
    pub fn set(&self, s: Snapshot) {
        *self.snap.lock().unwrap() = Some(s);
    }
}

impl App {
    pub(crate) fn browser_env_pub(&self) -> Option<Env> {
        self.browser_env()
    }

    fn browser_env(&self) -> Option<Env> {
        match &self.browser.env {
            Some(e) => Some(e.clone()),
            None if cfg!(test) => None,
            None if crate::demo::active() => Some(crate::demo::browser_env()),
            None => Some(Env::real()),
        }
    }

    fn browser_accounts(&self) -> Vec<AccountRef> {
        self.cfg
            .accounts
            .iter()
            .enumerate()
            .map(|(i, a)| AccountRef {
                number: i + 1,
                label: a.display().to_string(),
                harness: a.harness().name().to_string(),
                dir: a.config_dir(),
            })
            .collect()
    }

    /// Start a background look unless one runs or the last one was lately.
    pub fn browser_refresh(&mut self, force: bool) {
        let Some(env) = self.browser_env() else {
            return;
        };
        if self.browser.inflight.load(Ordering::SeqCst) {
            return;
        }
        if !force
            && self
                .browser
                .last_start
                .is_some_and(|t| t.elapsed() < MIN_GAP)
        {
            return;
        }
        self.browser.last_start = Some(Instant::now());
        self.browser.inflight.store(true, Ordering::SeqCst);
        let accounts = self.browser_accounts();
        let (snap, flag) = (self.browser.snap.clone(), self.browser.inflight.clone());
        std::thread::spawn(move || {
            let s = browser::collect(&env, &accounts);
            if let Ok(mut g) = snap.lock() {
                *g = Some(s);
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Per tick: while the Browser section of Settings is on screen, keep
    /// the picture fresh (at most one look per 30 s).
    pub fn browser_tick(&mut self) {
        if self.view == crate::app::View::Settings
            && self.settings_section_kind() == crate::settings::Section::Browser
        {
            self.browser_refresh(false);
        }
        if self
            .browser
            .confirm
            .is_some_and(|t| t.elapsed() > Duration::from_secs(15))
        {
            self.browser.confirm = None;
        }
        self.browser_poll_tick();
    }

    /// Whether a look is running now.
    pub fn browser_busy(&self) -> bool {
        self.browser.inflight.load(Ordering::SeqCst)
    }

    /// `list_browsers` and `browser_status`: the last look (never waits for
    /// a new one); a stale or missing one starts a refresh.
    pub fn browser_tool(&mut self, tool: &str) -> Result<Value, String> {
        if self.browser_env().is_none() {
            return Err("browser status is not available here".into());
        }
        let snap = self.browser.snapshot();
        let stale = snap
            .as_ref()
            .is_none_or(|s| Duration::from_secs(s.age_secs()) > STALE);
        if stale {
            self.browser_refresh(snap.is_none());
        }
        let Some(s) = snap else {
            return Ok(json!({"ok": true, "result": {
                "state": "checking",
                "say": "GodTerm is looking at BrowserSkill now; call again in a few seconds.",
            }}));
        };
        let mut v = if tool == "list_browsers" {
            s.browsers_json()
        } else {
            s.to_json()
        };
        // Whose each browser is, when the user linked it or its label
        // names one profile.
        let who =
            crate::tab_browser::assign_browsers(&s.browsers, &s.profiles, &self.cfg.browser.links);
        if let Some(arr) = v["browsers"].as_array_mut() {
            for (b, (_, dir, how)) in arr.iter_mut().zip(who.iter()) {
                b["profile_dir"] = json!(dir);
                b["match"] = json!(how);
            }
        }
        if stale {
            v["refreshing"] = json!(true);
        }
        Ok(json!({"ok": true, "result": v}))
    }

    // ---------- install_browser_skill ----------

    fn browser_source(env: &Env) -> Option<PathBuf> {
        env.skill_sources
            .iter()
            .find(|p| skill::source_ok(p))
            .cloned()
    }

    /// (plan, question, needs a yes). Without an `account` argument: every
    /// account.
    pub(crate) fn browser_plan(&mut self, args: &Value) -> Result<(Value, String, bool), String> {
        let env = self
            .browser_env()
            .ok_or("browser skill install is not available here")?;
        let src = Self::browser_source(&env).ok_or(
            "the BrowserSkill skill is not installed on this Mac yet (no browser-skill folder in ~/.claude/skills). GodTerm does not install BrowserSkill itself: the user installs bsk and its Chrome extension first",
        )?;
        let accts = match args.get("account") {
            None | Some(Value::Null) => (0..self.cfg.accounts.len()).collect(),
            v => self.admin_accounts(v)?,
        };
        if accts.is_empty() {
            return Err("there are no accounts".into());
        }
        let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
        let allow = args
            .get("allow_bsk")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mut targets = vec![];
        for a in accts {
            let acfg = &self.cfg.accounts[a];
            let dir = acfg.config_dir();
            let label = acfg.display().to_string();
            let claude = acfg.harness() == Harness::Claude;
            // GodTerm copies the skill into the home its own account dir
            // stands for: only Claude's and Grok's. The other CLIs read
            // their native stores (bsk install-skill lists the ones it
            // knows: Codex, Cursor), so they get the tab tools only.
            let why = acfg.harness().no_tab_tools_reason().or_else(|| {
                (!acfg.harness().integrated()).then(|| {
                    format!(
                        "{} reads skills from its native store, not the slot's folder: its tabs have the Browser binding and list_browsers, and `bsk install-skill` installs the skill for the CLIs bsk knows",
                        acfg.harness().label()
                    )
                })
            });
            if let Some(why) = why {
                targets.push(json!({
                    "account": a + 1, "label": label, "harness": acfg.harness().name(),
                    "dir": dir, "dest": Value::Null, "action": "skip", "settings": "skip",
                    "note": why,
                }));
                continue;
            }
            let dest = browser::skill_dir(&dir);
            let (action, note) = if !dir.is_dir() {
                (
                    None,
                    Some("the account folder does not exist yet".to_string()),
                )
            } else {
                match skill::plan(&src, &dest, force) {
                    Ok(x) => (Some(x), None),
                    Err(e) => (None, Some(e)),
                }
            };
            let settings = if !claude || !allow {
                "skip"
            } else {
                match std::fs::read_to_string(dir.join("settings.json")) {
                    Err(_) => "add",
                    Ok(t) if t.trim().is_empty() => "add",
                    Ok(t) => match serde_json::from_str::<Value>(&t) {
                        Ok(v) if v.get("permissions").is_some_and(|p| !p.is_object()) => "refused",
                        Ok(v)
                            if v["permissions"]["allow"].as_array().is_some_and(|l| {
                                l.iter().any(|x| x.as_str() == Some(BSK_RULE))
                            }) =>
                        {
                            "present"
                        }
                        Ok(_) => "add",
                        Err(_) => "refused",
                    },
                }
            };
            targets.push(json!({
                "account": a + 1, "label": label, "harness": acfg.harness().name(),
                "dir": dir, "dest": dest,
                "action": action.as_ref().map(|x| x.word()).unwrap_or("skip"),
                "settings": settings,
                "note": note,
            }));
        }
        let changes = |t: &Value| {
            matches!(t["action"].as_str(), Some("install" | "update")) || t["settings"] == "add"
        };
        let n = targets.iter().filter(|t| changes(t)).count();
        let plan = json!({"source": src, "force": force, "allow_bsk": allow, "targets": targets});
        if n == 0 {
            return Ok((plan, "Nothing to change.".into(), false));
        }
        let names = |f: &dyn Fn(&Value) -> bool| -> Vec<String> {
            plan["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| f(t))
                .map(|t| t["label"].as_str().unwrap_or("").to_string())
                .collect()
        };
        let join = |v: Vec<String>| match v.as_slice() {
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
            [] => String::new(),
        };
        let copy = names(&|t| matches!(t["action"].as_str(), Some("install" | "update")));
        let perm = names(&|t| t["settings"] == "add");
        let kept = names(&|t| t["action"] == "kept (newer copy)");
        let mut q = String::new();
        if !copy.is_empty() {
            q += &format!("Copy the browser skill into {}", join(copy));
        }
        if !perm.is_empty() {
            q += &if q.is_empty() {
                format!("Allow {BSK_RULE} in the settings of {}", join(perm))
            } else {
                format!(" and allow {BSK_RULE} in their settings.json (a backup is kept)")
            };
        }
        if !kept.is_empty() {
            q += &format!(" ({} keep a newer copy)", join(kept));
        }
        q += "? Their agents can then run bsk to drive the browser you connect.";
        Ok((plan, q, true))
    }

    pub(crate) fn browser_run(&mut self, plan: &Value) -> Result<Value, String> {
        let src = PathBuf::from(plan["source"].as_str().ok_or("no source")?);
        let mut rows = vec![];
        let mut done = 0;
        let mut failed = 0;
        for t in plan["targets"].as_array().into_iter().flatten() {
            let dest = PathBuf::from(t["dest"].as_str().unwrap_or(""));
            let dir = PathBuf::from(t["dir"].as_str().unwrap_or(""));
            let mut row = json!({"account": t["account"], "label": t["label"]});
            row["skill"] = match t["action"].as_str() {
                Some("install" | "update") => match skill::install(&src, &dest) {
                    Ok(()) => {
                        done += 1;
                        json!(if t["action"] == "install" {
                            "installed"
                        } else {
                            "updated"
                        })
                    }
                    Err(e) => {
                        failed += 1;
                        json!(format!("failed: {e}"))
                    }
                },
                Some(other) => json!(other),
                None => json!("skipped"),
            };
            row["permission"] = match t["settings"].as_str() {
                Some("add") => match skill::allow_rule(&dir.join("settings.json"), BSK_RULE) {
                    Ok(Merge::Added { backup }) => {
                        done += 1;
                        json!(match backup {
                            Some(b) => format!("added (backup {})", b.display()),
                            None => "added (settings.json created)".into(),
                        })
                    }
                    Ok(Merge::AlreadyAllowed) => json!("already allowed"),
                    Err(e) => {
                        failed += 1;
                        json!(format!("failed: {e}"))
                    }
                },
                Some("present") => json!("already allowed"),
                Some("refused") => json!("not changed: settings.json has an unexpected shape"),
                _ => json!("not requested"),
            };
            rows.push(row);
        }
        self.admin_record(&format!(
            "browser skill: {done} change(s), {failed} failure(s) across {} account(s)",
            rows.len()
        ));
        self.browser_refresh(true);
        let say = if failed > 0 {
            format!("Done with {failed} problem(s); the result has each account.")
        } else if done == 0 {
            "Nothing needed changing.".to_string()
        } else {
            "Done: the browser skill is in place. Already running tabs pick it up in a new session."
                .to_string()
        };
        Ok(json!({"ok": failed == 0, "result": {"accounts": rows, "say": say}}))
    }

    /// Settings > Browser, "Install the browser skill": the first click
    /// shows what it will do, the second (within 15 s) does it.
    pub fn browser_skill_click(&mut self) {
        let (plan, q, needs) = match self.browser_plan(&json!({})) {
            Ok(x) => x,
            Err(e) => return self.flash(e),
        };
        if !needs {
            self.browser.confirm = None;
            return self.flash("The browser skill is already in every account.");
        }
        if self
            .browser
            .confirm
            .is_some_and(|t| t.elapsed() < Duration::from_secs(15))
        {
            self.browser.confirm = None;
            let say = match self.browser_run(&plan) {
                Ok(v) => v["result"]["say"].as_str().unwrap_or("Done.").to_string(),
                Err(e) => e,
            };
            return self.flash(say);
        }
        self.browser.confirm = Some(Instant::now());
        self.flash(format!("{q} Click again to go ahead."));
    }
}
