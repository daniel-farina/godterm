//! Accounts and logins from the web app's Accounts screen (`accounts`
//! frames, not a control tool: the assistant cannot send them), and the
//! long-lived token flow (`claude setup-token`), shared with the terminal
//! UI's palette.
//!
//! - A login runs the CLI's own login in a tab of that account, as the
//!   terminal UI does. Its screen is read (login_screen.rs): the sign in
//!   URL goes to the browser as an "Open sign-in page" button, a code the
//!   CLI asks for comes back from the phone and is typed into that tab
//!   (never logged), and the account's login state is followed until it
//!   shows logged in.
//! - A token runs `claude setup-token` in a secret tab (pane.rs `secret`):
//!   the token is taken out of the output before any screen sees it and
//!   stored only in the keychain (acct_tokens.rs). The browser gets its
//!   last four characters and creation date, never the token.
//! - Log out, remove, copy and delete a token ask first (a one time
//!   confirm id, 5 minutes); removing never deletes the account's folder
//!   unless the user also ticked that, which asks a second time.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::acct_tokens::{self, Meta, Scrub};
use crate::app::App;
use crate::login_screen::{self, LoginScreen};
use crate::pane::{LaunchKind, LaunchSpec};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlowKind {
    Login,
    Token,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    Starting,
    /// The sign in page is known.
    SignIn,
    /// The CLI waits for the code from the page.
    Code,
    /// A code was typed in; waiting for the CLI.
    Checking,
    Done(String),
    Failed(String),
}

pub struct Flow {
    pub account: usize,
    /// The account's name (indexes can move when config.toml changes).
    pub name: String,
    pub uid: u64,
    pub kind: FlowKind,
    pub stage: Stage,
    pub started: Instant,
    pub screen: LoginScreen,
    /// Error lines already seen when the code was last sent.
    errors_seen: usize,
    /// Onboarding screens answered (and when), so each gets one Enter.
    pressed: Vec<(&'static str, Instant)>,
    /// The token tab's filter.
    catch: Option<Arc<Mutex<Scrub>>>,
    /// Started from the terminal UI: the sign in page opens on the Mac.
    open_on_mac: bool,
    opened: bool,
    ended_at: Option<Instant>,
    enter_after_done: bool,
}

impl Flow {
    fn done(&self) -> bool {
        matches!(self.stage, Stage::Done(_) | Stage::Failed(_))
    }

    fn json(&self) -> Value {
        let (stage, note) = match &self.stage {
            Stage::Starting => ("starting", None),
            Stage::SignIn => ("sign_in", None),
            Stage::Code => ("code", None),
            Stage::Checking => ("checking", None),
            Stage::Done(m) => ("done", Some(m.clone())),
            Stage::Failed(m) => ("failed", Some(m.clone())),
        };
        json!({
            "kind": match self.kind { FlowKind::Login => "login", FlowKind::Token => "token" },
            "stage": stage,
            "note": note,
            "tab": self.uid,
            "url": self.screen.url,
            "open_on_mac": self.screen.local_redirect,
            "user_code": self.screen.user_code,
            "wants_code": self.screen.wants_code && !self.done(),
            "error": self.screen.error.as_ref().filter(|_| self.screen.errors > 0).map(|e| crate::acct_tokens::mask_all(e)),
            "retry": self.screen.retry && !self.done(),
            "seconds": self.started.elapsed().as_secs(),
        })
    }
}

/// A destructive action waiting for the user's yes.
struct Confirm {
    id: String,
    op: String,
    args: Value,
    at: Instant,
}

#[derive(Default)]
pub struct AcctState {
    pub flows: Vec<Flow>,
    confirms: Vec<Confirm>,
    /// Browsers with the Accounts screen open (they get `view` pushes).
    pub watchers: Vec<u64>,
    last_view: String,
    last_tick: Option<Instant>,
    last_refresh: Option<Instant>,
    /// Token facts by account name, read off the main thread.
    meta: Arc<Mutex<HashMap<String, Option<Meta>>>>,
    meta_at: Option<Instant>,
    meta_busy: Arc<std::sync::atomic::AtomicBool>,
    /// Removed this run (they stay listed until GodTerm restarts).
    pub removed: Vec<String>,
}

const CONFIRM_FOR: Duration = Duration::from_secs(300);
const FLOW_LIMIT: Duration = Duration::from_secs(900);

fn hex(c: ratatui::style::Color) -> String {
    match c {
        ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "#8a8f87".into(),
    }
}

fn new_id() -> String {
    let mut b = [0u8; 12];
    crate::platform::fill_random(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl App {
    fn acct_index(&self, v: &Value) -> Result<usize, String> {
        let n = v["account"]
            .as_u64()
            .or_else(|| v["account"].as_str().and_then(|s| s.parse().ok()))
            .ok_or("which account?")? as usize;
        if n == 0 || n > self.cfg.accounts.len() {
            return Err("no such account".into());
        }
        let a = n - 1;
        if self.acct.removed.contains(&self.cfg.accounts[a].name) {
            return Err("that account was removed".into());
        }
        Ok(a)
    }

    fn acct_flow(&self, a: usize) -> Option<&Flow> {
        let name = &self.cfg.accounts.get(a)?.name;
        self.acct.flows.iter().rev().find(|f| &f.name == name)
    }

    /// Token facts (cached; read again in the background every minute or
    /// after a change).
    fn token_meta(&mut self, name: &str) -> Option<Meta> {
        let stale = self
            .acct
            .meta_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(60));
        if stale
            && !self
                .acct
                .meta_busy
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.acct.meta_at = Some(Instant::now());
            let names: Vec<String> = self
                .cfg
                .accounts
                .iter()
                .filter(|a| a.harness() == crate::harness::Harness::Claude)
                .map(|a| a.name.clone())
                .collect();
            let (map, busy) = (
                Arc::clone(&self.acct.meta),
                Arc::clone(&self.acct.meta_busy),
            );
            let work = move || {
                let st = acct_tokens::store();
                let got: Vec<(String, Option<Meta>)> = names
                    .into_iter()
                    .map(|n| (n.clone(), st.meta(&n)))
                    .collect();
                let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
                m.clear();
                m.extend(got);
                busy.store(false, std::sync::atomic::Ordering::Release);
            };
            if cfg!(test) {
                work();
            } else {
                std::thread::spawn(work);
            }
        }
        self.acct
            .meta
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
            .flatten()
    }

    fn token_changed(&mut self) {
        self.acct.meta_at = None;
    }

    /// The Accounts screen's data. No secret is in it: login state, email
    /// (unless privacy is on), plan, usage, token date and last four.
    pub fn accounts_view(&mut self) -> Value {
        let deps = self.deps();
        let brain = self.assistant.brain.as_ref().and_then(|b| b.account);
        let want = self.assistant_account();
        let prefs = acct_tokens::prefs();
        let mut list = vec![];
        for a in 0..self.cfg.accounts.len() {
            let c = self.cfg.accounts[a].clone();
            let st = &self.accounts[a];
            let h = c.harness();
            let login = &st.login;
            let state = login_state(st);
            let buckets: Vec<Value> = st
                .usage
                .as_ref()
                .map(|u| {
                    u.windows
                        .iter()
                        .map(|w| json!({"bucket": w.label, "left_pct": w.left_now().round(),
                            "resets_at": w.resets_at.map(|t| t.with_timezone(&chrono::Local).format("%a %H:%M").to_string())}))
                        .collect()
                })
                .unwrap_or_default();
            let usage_error = st.usage_err.as_ref().map(|e| match e {
                crate::usage::UsageError::Unauthorized => {
                    "the login expired or was revoked".to_string()
                }
                crate::usage::UsageError::RateLimited(_) => {
                    "the usage API asked to wait".to_string()
                }
                crate::usage::UsageError::NotLoggedIn => "not logged in".to_string(),
                other => format!("{other:?}"),
            });
            let email = (!self.privacy())
                .then(|| st.profile.email.clone())
                .flatten();
            let plan = login
                .subscription
                .clone()
                .or_else(|| st.profile.billing_type.clone());
            let org = (!self.privacy())
                .then(|| st.profile.org_name.clone())
                .flatten();
            let tabs: usize = self
                .panes
                .iter()
                .filter(|p| p.account == Some(a))
                .map(|p| p.tabs.iter().filter(|t| t.is_running()).count())
                .sum();
            let claude = h == crate::harness::Harness::Claude;
            let token = if claude {
                self.token_meta(&c.name).map(|m| {
                    let p = &prefs;
                    json!({"created": m.created, "last4": m.last4,
                        "use": p.use_for.contains(&c.name),
                        "sync": p.sync.contains(&c.name),
                        "from": p.from.get(&c.name)})
                })
            } else {
                None
            };
            let installed = deps
                .iter()
                .find(|d| d.id == h.name())
                .is_none_or(|d| d.status.ok());
            list.push(json!({
                "account": a + 1,
                "name": c.name,
                "label": c.display(),
                "harness": h.name(),
                "harness_label": h.label(),
                "color": hex(crate::theme::parse_color(&c.color)),
                "color_name": c.color,
                "state": state,
                "email": email,
                "plan": plan,
                "org": org,
                "folder": c.cwd.clone().unwrap_or_else(|| "~".into()),
                "config_dir": crate::config::tilde(&c.config_dir()),
                "tabs": tabs,
                "assistant": brain == Some(a),
                "assistant_next": brain.is_none() && want == Some(a),
                "buckets": buckets,
                "usage_error": usage_error,
                "integrated": h.integrated(),
                "installed": installed,
                "can_token": claude,
                "token": token,
                "flow": self.acct_flow(a).map(Flow::json),
                "removed": self.acct.removed.contains(&c.name),
            }));
        }
        let agents: Vec<Value> = crate::add_account::AGENTS
            .iter()
            .map(|(id, name, sub)| {
                let d = deps.iter().find(|d| d.id == *id);
                json!({"id": id, "name": name, "sub": sub,
                    "installed": d.is_none_or(|d| d.status.ok()),
                    "how": d.filter(|d| !d.status.ok()).map(crate::first_run::how_to_install)})
            })
            .collect();
        let swatches: Vec<Value> = crate::theme::SWATCHES
            .iter()
            .map(|s| json!({"name": s, "hex": hex(crate::theme::parse_color(s))}))
            .collect();
        json!({
            "accounts": list,
            "agents": agents,
            "swatches": swatches,
            "assistant_provider": self.cfg.assistant.provider,
            "privacy": self.privacy(),
            // Syncing tokens: off unless turned on here, and only into a
            // state repo checked private before each push.
            "token_sync": {
                "on": prefs.sync_all,
                "sync_enabled": self.cfg.sync.enabled,
                "repo": format!("{}/{}", self.cfg.sync.owner, self.cfg.sync.state_repo),
                "warning": std::fs::read(crate::config::app_home().join("sync/status.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .and_then(|v| v["token_warning"].as_str().map(str::to_string)),
            },
        })
    }

    /// An `accounts` frame from a browser (src/web/ws.rs).
    pub fn web_accounts(&mut self, client: u64, v: Value) {
        let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) else {
            return;
        };
        let id = v["id"].clone();
        let op = v["op"].as_str().unwrap_or("view").to_string();
        if op == "watch" {
            let on = v["on"].as_bool().unwrap_or(true);
            self.acct.watchers.retain(|w| *w != client);
            if on {
                self.acct.watchers.push(client);
            }
        }
        let r = self.accounts_op(&op, &v);
        let mut out = match r {
            Ok(mut o) => {
                o["ok"] = json!(true);
                o
            }
            Err(e) => json!({"ok": false, "error": e}),
        };
        out["t"] = json!("accounts");
        out["id"] = id;
        out["view"] = self.accounts_view();
        self.acct.last_view = out["view"].to_string();
        c.out.text(out.to_string());
    }

    /// One Accounts operation. Destructive ones answer `confirm` first.
    pub fn accounts_op(&mut self, op: &str, v: &Value) -> Result<Value, String> {
        self.acct.confirms.retain(|c| c.at.elapsed() < CONFIRM_FOR);
        match op {
            "view" | "watch" => Ok(json!({})),
            "login" => {
                let a = self.acct_index(v)?;
                let uid = self.acct_start_login(a, false)?;
                Ok(json!({"tab": uid}))
            }
            "code" => self.acct_code(v),
            "open_mac" => {
                // A sign in that comes back to localhost: on the Mac.
                let a = self.acct_index(v)?;
                let url = self
                    .acct_flow(a)
                    .and_then(|f| f.screen.url.clone())
                    .ok_or("no sign in page yet")?;
                self.open_service_link(&url);
                Ok(json!({"said": "Opened on the Mac"}))
            }
            "retry" => {
                let a = self.acct_index(v)?;
                let f = self.acct_flow(a).ok_or("no login is running for it")?;
                let uid = f.uid;
                let (s, t) = self.find_tab(uid).ok_or("its login tab is gone")?;
                self.panes[s].tabs[t].write(b"\r");
                Ok(json!({}))
            }
            "cancel" => {
                let a = self.acct_index(v)?;
                let name = self.cfg.accounts[a].name.clone();
                let uids: Vec<u64> = self
                    .acct
                    .flows
                    .iter()
                    .filter(|f| f.name == name)
                    .map(|f| f.uid)
                    .collect();
                for u in uids {
                    if let Some((s, t)) = self.find_tab(u) {
                        if self.panes[s].tabs[t].is_running() {
                            self.panes[s].tabs[t].kill();
                        }
                    }
                }
                self.acct.flows.retain(|f| f.name != name);
                Ok(json!({"said": "Stopped"}))
            }
            "add" => self.acct_add(v),
            "rename" => {
                let a = self.acct_index(v)?;
                let label = v["label"].as_str().unwrap_or("").trim().to_string();
                if label.is_empty() || label.chars().count() > 40 {
                    return Err("a name of 1 to 40 characters".into());
                }
                let name = self.cfg.accounts[a].name.clone();
                set_account_field(&name, "label", toml_edit::value(label.clone()))?;
                self.reload_config_now();
                Ok(json!({"said": format!("Renamed to {label}")}))
            }
            "color" => {
                let a = self.acct_index(v)?;
                let col = v["color"].as_str().unwrap_or("");
                if !crate::theme::SWATCHES.contains(&col) {
                    return Err("pick one of the colors".into());
                }
                let name = self.cfg.accounts[a].name.clone();
                set_account_field(&name, "color", toml_edit::value(col))?;
                self.reload_config_now();
                Ok(json!({"said": "Color changed"}))
            }
            "assistant" => {
                let a = self.acct_index(v)?;
                let label = self.cfg.accounts[a].display().to_string();
                let h = self.cfg.accounts[a].harness();
                let provider = crate::providers::PROVIDERS
                    .iter()
                    .find(|p| p.harness == Some(h))
                    .ok_or_else(|| format!("the assistant cannot run on {}", h.label()))?;
                let say = self.switch_assistant(Some(provider.id), Some(&json!(a + 1)), None)?;
                crate::log::info(&format!("web: assistant account set to {label}"));
                Ok(json!({"said": say}))
            }
            "logout" | "remove" | "token_copy" | "token_delete" => self.acct_ask(op, v),
            "token_sync" => {
                let a = self.acct_index(v)?;
                let name = self.cfg.accounts[a].name.clone();
                let on = v["on"].as_bool().unwrap_or(false);
                if on && self.token_meta(&name).is_none() {
                    return Err("it has no token yet".into());
                }
                acct_tokens::edit_prefs(|p| {
                    p.sync.retain(|x| x != &name);
                    if on {
                        p.sync.push(name.clone());
                    }
                })?;
                let all = acct_tokens::prefs().sync_all;
                Ok(json!({"said": match (on, all) {
                    (true, true) => "It syncs to your other machines at the next sync (private state repo only)",
                    (true, false) => "Marked; turn on Sync tokens to my other machines too",
                    _ => "It stays on this Mac (a copy already synced stays on the other machines)",
                }}))
            }
            "token_sync_all" => {
                let on = v["on"].as_bool().unwrap_or(false);
                if !on {
                    acct_tokens::edit_prefs(|p| p.sync_all = false)?;
                    return Ok(json!({"said": "Tokens no longer sync from or to this Mac"}));
                }
                let repo = format!("{}/{}", self.cfg.sync.owner, self.cfg.sync.state_repo);
                let id = new_id();
                self.acct.confirms.push(Confirm {
                    id: id.clone(),
                    op: "token_sync_all".into(),
                    args: json!({}),
                    at: Instant::now(),
                });
                Ok(json!({"confirm": {"id": id, "yes": "Sync tokens",
                    "question": format!("Sync the long-lived tokens you mark to your other machines? They go into the state repo {repo} on GitHub, only while GitHub says it is private (checked before every push); anyone who can read that repo can use them. Other machines put them in their own keychain.")}}))
            }
            "token_create" => {
                let a = self.acct_index(v)?;
                let uid = self.acct_start_token(a, false)?;
                Ok(json!({"tab": uid}))
            }
            "token_use" => {
                let a = self.acct_index(v)?;
                let name = self.cfg.accounts[a].name.clone();
                let on = v["on"].as_bool().unwrap_or(false);
                if on && self.token_meta(&name).is_none() {
                    return Err("it has no token yet".into());
                }
                acct_tokens::set_uses_token(&name, on)?;
                // The assistant on this account starts again with (or
                // without) it.
                if self.assistant.brain.as_ref().and_then(|b| b.account) == Some(a) {
                    self.assistant.brain = None;
                }
                Ok(json!({"said": if on {
                    "The assistant on this account now runs with its token"
                } else {
                    "The assistant on this account uses its normal login again"
                }}))
            }
            "confirm" => {
                let cid = v["confirm_id"].as_str().unwrap_or("");
                let i = self
                    .acct
                    .confirms
                    .iter()
                    .position(|c| c.id == cid)
                    .ok_or("that question expired; try again")?;
                let c = self.acct.confirms.remove(i);
                if v["answer"].as_str() == Some("no") {
                    return Ok(json!({"said": "Nothing changed"}));
                }
                self.acct_confirmed(&c.op, &c.args)
            }
            _ => Err(format!("unknown accounts op {op:?}")),
        }
    }

    /// Ask before a destructive action: a one time id and the question.
    fn acct_ask(&mut self, op: &str, v: &Value) -> Result<Value, String> {
        let a = self.acct_index(v)?;
        let c = &self.cfg.accounts[a];
        let label = c.display().to_string();
        let name = c.name.clone();
        let dir = crate::config::tilde(&c.config_dir());
        let mut args = json!({"account": a + 1, "name": name});
        let (question, danger) = match op {
            "logout" => {
                if !c.harness().integrated() {
                    return Err(format!(
                        "{} keeps its own login; log out in its CLI",
                        c.harness().label()
                    ));
                }
                (
                    format!("Log out of {label}? Its tabs stop."),
                    "Log out".to_string(),
                )
            }
            "remove" => {
                let delete = v["delete_data"].as_bool() == Some(true);
                args["delete_data"] = json!(delete);
                let managed = crate::config::app_home().join("accounts");
                if delete && !c.config_dir().starts_with(&managed) {
                    return Err(format!(
                        "{dir} is not a folder GodTerm made; it is never deleted from here"
                    ));
                }
                (
                    format!(
                        "Remove {label} from GodTerm? Its tabs stop and it goes away at the next start.{}",
                        if delete {
                            " Deleting its folder is asked next, on its own."
                        } else {
                            " Its folder is kept."
                        }
                    ),
                    "Remove".to_string(),
                )
            }
            "token_copy" => {
                if self.token_meta(&name).is_none() {
                    return Err("it has no token".into());
                }
                (format!("Copy {label}'s long-lived token to the Mac's clipboard? Anything on the Mac that reads the clipboard can see it."), "Copy".to_string())
            }
            _ => {
                if self.token_meta(&name).is_none() {
                    return Err("it has no token".into());
                }
                (format!("Delete {label}'s long-lived token from the keychain? This does not revoke it: sign in at claude.ai to revoke it for good."), "Delete".to_string())
            }
        };
        let id = new_id();
        self.acct.confirms.push(Confirm {
            id: id.clone(),
            op: op.to_string(),
            args,
            at: Instant::now(),
        });
        Ok(json!({"confirm": {"id": id, "question": question, "yes": danger}}))
    }

    fn acct_confirmed(&mut self, op: &str, args: &Value) -> Result<Value, String> {
        if op == "token_sync_all" {
            acct_tokens::edit_prefs(|p| p.sync_all = true)?;
            crate::log::info("accounts: token sync turned on on this Mac");
            return Ok(
                json!({"said": "Marked tokens sync at the next sync, while the state repo is private"}),
            );
        }
        let name = args["name"].as_str().unwrap_or("").to_string();
        let a = self
            .cfg
            .accounts
            .iter()
            .position(|c| c.name == name)
            .ok_or("the accounts changed; try again")?;
        if op != "delete_data" && self.acct.removed.contains(&name) {
            return Err("that account was removed".into());
        }
        let label = self.cfg.accounts[a].display().to_string();
        match op {
            "logout" => {
                self.logout_account(a);
                Ok(json!({"said": format!("Logging out {label}")}))
            }
            "remove" => {
                for s in self.panes.iter_mut() {
                    if s.account == Some(a) {
                        s.kill_all();
                    }
                }
                self.remove_account(a);
                self.acct.removed.push(name.clone());
                self.acct.flows.retain(|f| f.name != name);
                crate::log::info(&format!("web: removed account {name} (folder kept)"));
                if args["delete_data"].as_bool() != Some(true) {
                    return Ok(json!({"said": format!("Removed {label}; its folder is kept")}));
                }
                // The folder is a second, separate yes.
                let dir = crate::config::tilde(&self.cfg.accounts[a].config_dir());
                let id = new_id();
                self.acct.confirms.push(Confirm {
                    id: id.clone(),
                    op: "delete_data".into(),
                    args: json!({"name": name}),
                    at: Instant::now(),
                });
                Ok(json!({
                    "said": format!("Removed {label}; its folder is kept for now"),
                    "confirm": {"id": id, "yes": "Delete the folder",
                        "question": format!("Also delete {dir}? It holds {label}'s login, conversations and settings for its CLI. It goes to the Trash (log out first if you want the login revoked).")},
                }))
            }
            "delete_data" => {
                let dir = self.cfg.accounts[a].config_dir();
                let managed = crate::config::app_home().join("accounts");
                if !dir.starts_with(&managed) {
                    return Err("that folder is not one GodTerm made".into());
                }
                let p = to_trash(&dir).map_err(|e| format!("Its folder stayed: {e}"))?;
                crate::log::info(&format!(
                    "web: the folder of {name} went to {}",
                    crate::config::tilde(&p)
                ));
                Ok(json!({"said": format!("{label}'s folder is in the Trash")}))
            }
            "token_copy" => {
                let t = acct_tokens::store()
                    .get(&name)
                    .ok_or("the keychain did not give it")?;
                acct_tokens::to_clipboard(&t)?;
                crate::log::info(&format!("token of {name} copied to the clipboard"));
                Ok(
                    json!({"said": "Copied to the Mac's clipboard. Paste it where it goes, then copy something else over it."}),
                )
            }
            "token_delete" => {
                acct_tokens::store().delete(&name)?;
                let synced = acct_tokens::prefs().syncs(&name);
                let now = chrono::Utc::now().timestamp_millis();
                let _ = acct_tokens::edit_prefs(|p| p.deleted(&name, now));
                self.token_changed();
                if self.assistant.brain.as_ref().and_then(|b| b.account) == Some(a) {
                    self.assistant.brain = None;
                }
                crate::log::info(&format!("token of {name} deleted from the keychain"));
                Ok(json!({
                    "said": if synced {
                        "Deleted here, and from your other machines at the next sync. It still works until revoked: sign in at claude.ai, open Settings, and revoke it there (or let it expire after a year)."
                    } else {
                        "Deleted from the keychain. It still works until revoked: sign in at claude.ai, open Settings, and revoke it there (or let it expire after a year)."
                    },
                    "revoke": "https://claude.ai/settings",
                }))
            }
            _ => Err("unknown".into()),
        }
    }

    /// The code from the sign in page, typed into the login tab. Never
    /// logged or echoed back.
    fn acct_code(&mut self, v: &Value) -> Result<Value, String> {
        let a = self.acct_index(v)?;
        let code = v["code"].as_str().unwrap_or("").trim().to_string();
        if code.is_empty() || code.len() > 400 || code.chars().any(|c| c.is_control()) {
            return Err("that does not look like a login code".into());
        }
        let name = self.cfg.accounts[a].name.clone();
        let i = self
            .acct
            .flows
            .iter()
            .rposition(|f| f.name == name && !f.done())
            .ok_or("no login of this account waits for a code")?;
        let uid = self.acct.flows[i].uid;
        let (s, t) = self.find_tab(uid).ok_or("its login tab is gone")?;
        self.paste_then_enter(s, t, &code);
        let f = &mut self.acct.flows[i];
        f.errors_seen = f.screen.errors;
        f.stage = Stage::Checking;
        crate::log::info(&format!(
            "accounts: a login code was typed for {name} (not logged)"
        ));
        Ok(json!({}))
    }

    fn acct_add(&mut self, v: &Value) -> Result<Value, String> {
        let label = v["label"].as_str().unwrap_or("").trim().to_string();
        if label.is_empty() || label.chars().count() > 40 {
            return Err("give the account a name (up to 40 characters)".into());
        }
        let harness = v["harness"].as_str().unwrap_or("claude");
        if !crate::harness::NAMES.contains(&harness) {
            return Err("unknown agent".into());
        }
        let deps = self.deps();
        if let Some(d) = deps.iter().find(|d| d.id == harness && !d.status.ok()) {
            return Err(format!(
                "{} is not installed. {}",
                d.name,
                crate::first_run::how_to_install(d)
            ));
        }
        let color = v["color"].as_str().unwrap_or("");
        let color = if crate::theme::SWATCHES.contains(&color) {
            color.to_string()
        } else {
            crate::theme::SWATCHES[self.cfg.accounts.len() % crate::theme::SWATCHES.len()]
                .to_string()
        };
        let folder = v["folder"].as_str().unwrap_or("~").trim();
        let folder = if folder.is_empty() { "~" } else { folder };
        if !crate::config::expand_tilde(folder).is_dir() {
            return Err(format!("{folder} is not a folder"));
        }
        let n0 = self.cfg.accounts.len();
        self.add_account(crate::app::NewAccount {
            label: label.clone(),
            harness: harness.to_string(),
            color,
            cwd: folder.to_string(),
            permission_mode: None,
        });
        if self.cfg.accounts.len() == n0 {
            return Err("the account could not be added".into());
        }
        let a = self.cfg.accounts.len() - 1;
        // add_account started its login in the account's pane: watch it.
        if let Some(p) = self.panes.iter().position(|p| p.account == Some(a)) {
            let uid = self.panes[p].cur().uid;
            self.acct_watch(a, uid, FlowKind::Login, None, false);
        }
        crate::log::info(&format!("web: added account {label} ({harness})"));
        Ok(json!({"said": format!("Added {label}; its login started"), "account": a + 1}))
    }

    fn acct_watch(
        &mut self,
        a: usize,
        uid: u64,
        kind: FlowKind,
        catch: Option<Arc<Mutex<Scrub>>>,
        open_on_mac: bool,
    ) {
        let name = self.cfg.accounts[a].name.clone();
        self.acct.flows.retain(|f| f.name != name || f.kind != kind);
        self.acct.flows.push(Flow {
            account: a,
            name,
            uid,
            kind,
            stage: Stage::Starting,
            started: Instant::now(),
            screen: LoginScreen::default(),
            errors_seen: 0,
            pressed: vec![],
            catch,
            open_on_mac,
            opened: false,
            ended_at: None,
            enter_after_done: false,
        });
    }

    /// Start account `a`'s own login in a tab of its pane (a new tab when
    /// the shown one is in use), watched for the Accounts screen.
    pub fn acct_start_login(&mut self, a: usize, open_on_mac: bool) -> Result<u64, String> {
        if self
            .acct_flow(a)
            .is_some_and(|f| !f.done() && f.kind == FlowKind::Login)
        {
            return self.acct_flow(a).map(|f| f.uid).ok_or_else(String::new);
        }
        let pane = self.pane_for_account(a);
        self.assign(pane, a);
        let cur = self.panes[pane].cur();
        if cur.is_running() || cur.session_id.is_some() {
            let cwd = self.cfg.accounts[a].work_dir();
            self.panes[pane].add_tab(cwd);
        }
        self.launch(pane, LaunchKind::Login);
        let tab = self.panes[pane].cur();
        if !tab.is_running() {
            return Err("the login did not start (see the toast)".into());
        }
        let uid = tab.uid;
        self.acct_watch(a, uid, FlowKind::Login, None, open_on_mac);
        crate::log::info(&format!(
            "accounts: login started for {} in {}",
            self.cfg.accounts[a].name,
            crate::control::tab_id(uid)
        ));
        Ok(uid)
    }

    /// `claude setup-token` for account `a` in a secret tab of its pane:
    /// the token it prints is caught before any screen sees it.
    pub fn acct_start_token(&mut self, a: usize, open_on_mac: bool) -> Result<u64, String> {
        let c = self.cfg.accounts[a].clone();
        if c.harness() != crate::harness::Harness::Claude {
            return Err("long-lived tokens are for Claude accounts".into());
        }
        let dir = c.config_dir();
        if crate::creds::is_default_dir(&dir) {
            return Err("refusing the default ~/.claude".into());
        }
        if let Some(f) = self
            .acct
            .flows
            .iter()
            .find(|f| f.name == c.name && f.kind == FlowKind::Token && !f.done())
        {
            return Ok(f.uid);
        }
        let pane = self.pane_for_account(a);
        self.assign(pane, a);
        let cwd = c.work_dir();
        let i = self.panes[pane].add_tab(cwd.clone());
        let size = self.panes[pane].size;
        let catch = Arc::new(Mutex::new(Scrub::new()));
        {
            let tab = &mut self.panes[pane].tabs[i];
            tab.secret = Some(Arc::clone(&catch));
            tab.transient = true;
            tab.custom_name = Some("setup-token".into());
            tab.resize(size.0, size.1);
        }
        let spec = LaunchSpec {
            program: self.cfg.claude_bin(),
            home_env: "CLAUDE_CONFIG_DIR",
            extra_env: vec![],
            args: vec!["setup-token".into()],
            cwd,
            config_dir: dir,
            kind: LaunchKind::Login,
            pass_env: self.cfg.pass_env.clone(),
            type_when_idle: None,
            harness: crate::harness::Harness::Claude,
        };
        let tx = self.tx.clone();
        let tab = &mut self.panes[pane].tabs[i];
        tab.spawn(spec, tx)
            .map_err(|e| format!("could not start claude setup-token: {e:#}"))?;
        let uid = tab.uid;
        self.state_dirty = true;
        self.acct_watch(a, uid, FlowKind::Token, Some(catch), open_on_mac);
        crate::log::info(&format!(
            "accounts: setup-token started for {} in {}",
            c.name,
            crate::control::tab_id(uid)
        ));
        Ok(uid)
    }

    /// From the palette: a long-lived token for the focused pane's account.
    pub fn palette_create_token(&mut self) {
        let Some(a) = self.panes.get(self.focus).and_then(|p| p.account) else {
            self.flash("No account in this pane");
            return;
        };
        match self.acct_start_token(a, true) {
            Ok(_) => self.flash(format!(
                "setup-token started for {}: sign in, paste the code; the token goes to the keychain only",
                self.cfg.accounts[a].display()
            )),
            Err(e) => self.flash(e),
        }
    }

    /// Every tick: follow the login and token tabs; push the Accounts
    /// view to browsers that have it open when it changed.
    pub fn acct_tick(&mut self) {
        if self.acct.flows.is_empty() && self.acct.watchers.is_empty() {
            return;
        }
        if self
            .acct
            .last_tick
            .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.acct.last_tick = Some(Instant::now());
        let mut flows = std::mem::take(&mut self.acct.flows);
        let mut refresh = vec![];
        for f in &mut flows {
            if f.done() {
                continue;
            }
            let Some(a) = self.cfg.accounts.iter().position(|c| c.name == f.name) else {
                f.stage = Stage::Failed("the account is gone".into());
                continue;
            };
            f.account = a;
            if f.started.elapsed() > FLOW_LIMIT {
                f.stage = Stage::Failed("it timed out; start it again".into());
                continue;
            }
            // A token first: it may be caught in the last output.
            if let Some(tok) = f
                .catch
                .as_ref()
                .and_then(|c| c.lock().unwrap_or_else(|e| e.into_inner()).take())
            {
                let shown = acct_tokens::mask(&tok);
                f.stage = match acct_tokens::store().put(&f.name, &tok) {
                    Ok(()) => {
                        let now = chrono::Utc::now().timestamp_millis();
                        let _ = acct_tokens::edit_prefs(|p| p.saved(&f.name, &tok, now, None));
                        crate::log::info(&format!(
                            "token for {} saved in the keychain ({shown})",
                            f.name
                        ));
                        Stage::Done(format!("Saved in the keychain ({shown})"))
                    }
                    Err(e) => {
                        crate::log::error(&format!("token for {}: not saved: {e}", f.name));
                        Stage::Failed(format!("Not saved: {e}"))
                    }
                };
                drop(tok);
                self.acct.meta_at = None;
                continue;
            }
            let Some((s, t)) = self.find_tab(f.uid) else {
                f.stage = Stage::Failed("its tab closed".into());
                continue;
            };
            let screen = self.tab_screen(s, t);
            let running = self.panes[s].tabs[t].is_running();
            f.screen = login_screen::read(&screen);
            if let Some(k) = f.screen.onboarding {
                if !f
                    .pressed
                    .iter()
                    .any(|(x, at)| *x == k && at.elapsed() < Duration::from_secs(5))
                {
                    f.pressed.push((k, Instant::now()));
                    self.panes[s].tabs[t].write(b"\r");
                }
            }
            if f.open_on_mac && !f.opened {
                if let Some(u) = f.screen.url.clone() {
                    f.opened = true;
                    self.open_service_link(&u);
                }
            }
            let logged = self.accounts.get(a).is_some_and(|x| x.login.logged_in());
            match f.kind {
                FlowKind::Token => {
                    if f.screen.token_created || !running {
                        // Printed but never caught (or the CLI ended).
                        f.stage = Stage::Failed(if f.screen.token_created {
                            "the token was printed in a form GodTerm did not recognize; nothing was saved. Close that tab".into()
                        } else {
                            "setup-token ended without a token".into()
                        });
                        continue;
                    }
                }
                FlowKind::Login => {
                    let after_code = matches!(f.stage, Stage::Checking);
                    if f.screen.success || (logged && (after_code || !running)) {
                        f.stage =
                            Stage::Done(format!("{} is signed in", self.cfg.accounts[a].display()));
                        f.enter_after_done = screen.to_lowercase().contains("press enter");
                        if f.enter_after_done {
                            self.panes[s].tabs[t].write(b"\r");
                        }
                        refresh.push(a);
                        continue;
                    }
                    if !running {
                        f.stage = Stage::Failed("the login ended; start it again".into());
                        refresh.push(a);
                        continue;
                    }
                    if f.started.elapsed() > Duration::from_secs(2) {
                        refresh.push(a);
                    }
                }
            }
            if matches!(f.stage, Stage::Checking) {
                if f.screen.errors > f.errors_seen {
                    f.stage = Stage::Code;
                }
                continue;
            }
            f.stage = if f.screen.wants_code {
                Stage::Code
            } else if f.screen.url.is_some() {
                Stage::SignIn
            } else {
                Stage::Starting
            };
        }
        // Finished ones stay listed a while ("signed in"), then go.
        for f in &mut flows {
            if f.done() && f.ended_at.is_none() {
                f.ended_at = Some(Instant::now());
            }
        }
        flows.retain(|f| {
            f.ended_at
                .is_none_or(|t| t.elapsed() < Duration::from_secs(180))
        });
        // A flow started meanwhile (by a tick's action) is kept too.
        flows.append(&mut self.acct.flows);
        self.acct.flows = flows;
        // Login state, at most every 2 s while a login runs.
        if !refresh.is_empty()
            && self
                .acct
                .last_refresh
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
        {
            self.acct.last_refresh = Some(Instant::now());
            refresh.sort_unstable();
            refresh.dedup();
            for a in refresh {
                self.refresh_account(a, false);
            }
        }
        self.acct_push();
    }

    /// The view to the browsers that watch it, when it changed.
    fn acct_push(&mut self) {
        let Some(hub) = self.web.as_ref().map(|w| Arc::clone(&w.hub)) else {
            return;
        };
        self.acct.watchers.retain(|c| hub.client(*c).is_some());
        if self.acct.watchers.is_empty() {
            return;
        }
        let view = self.accounts_view();
        let s = view.to_string();
        if s == self.acct.last_view {
            return;
        }
        self.acct.last_view = s;
        let frame = json!({"t": "accounts", "op": "view", "ok": true, "view": view}).to_string();
        for c in &self.acct.watchers {
            if let Some(cl) = hub.client(*c) {
                cl.out.text(frame.clone());
            }
        }
    }

    /// For the assistant's state: login flows in words (never a token).
    pub fn acct_state_lines(&self) -> String {
        let mut s = String::new();
        for f in &self.acct.flows {
            let what = match f.kind {
                FlowKind::Login => "login",
                FlowKind::Token => "long-lived token",
            };
            let st = match &f.stage {
                Stage::Starting => "starting".to_string(),
                Stage::SignIn => "waiting for the sign in page".into(),
                Stage::Code => {
                    "waiting for the code (the user pastes it in the web app's Accounts screen)"
                        .into()
                }
                Stage::Checking => "checking the code".into(),
                Stage::Done(m) => format!("done: {m}"),
                Stage::Failed(m) => format!("failed: {m}"),
            };
            s.push_str(&format!("{what} a{} (web): {st}\n", f.account + 1));
        }
        s
    }
}

/// An account's login in one word: logged_in, logged_out, expired (the
/// usage API refused it, or its token ran out with no refresh) or cli.
pub fn login_state(st: &crate::app::AccountState) -> &'static str {
    let login = &st.login;
    if login.cli_managed {
        "cli"
    } else if !login.logged_in() {
        "logged_out"
    } else if matches!(st.usage_err, Some(crate::usage::UsageError::Unauthorized))
        || (login.expired() && !login.has_refresh)
    {
        "expired"
    } else {
        "logged_in"
    }
}

/// Set one key of the `[[account]]` named `name` in config.toml (found by
/// name, as remove_account does), keeping the file's comments.
fn set_account_field(name: &str, key: &str, value: toml_edit::Item) -> Result<(), String> {
    use crate::config::Config;
    let path = Config::path();
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let arr = doc
        .get_mut("account")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .ok_or("no accounts in config.toml")?;
    let t = arr
        .iter_mut()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(name))
        .ok_or_else(|| format!("{name} is not in config.toml"))?;
    t.insert(key, value);
    let out = doc.to_string();
    Config::parse(&out).map_err(|e| format!("{e:#}"))?;
    crate::config::write_atomic(&path, out.as_bytes()).map_err(|e| e.to_string())
}

/// Move a folder to the Trash (~/.Trash), named so it is found again.
fn to_trash(dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    // Only an account folder GodTerm made (under its own accounts/),
    // never the main ~/.claude or ~/.grok.
    let managed = crate::config::app_home().join("accounts");
    let h = crate::config::home_dir();
    if crate::creds::is_default_dir(dir)
        || !dir.starts_with(&managed)
        || dir == managed
        || dir.starts_with(h.join(".grok"))
        || dir.starts_with(h.join(".claude"))
    {
        return Err("refusing that folder".into());
    }
    if !dir.is_dir() {
        return Err("it is not there".into());
    }
    let trash = crate::config::home_dir().join(".Trash");
    std::fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    let base = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "account".into());
    let to = trash.join(format!(
        "{base} (GodTerm {})",
        chrono::Local::now().format("%Y-%m-%d %H.%M.%S")
    ));
    std::fs::rename(dir, &to).map_err(|e| e.to_string())?;
    Ok(to)
}

#[cfg(test)]
#[path = "app_accounts_tests.rs"]
mod tests;
