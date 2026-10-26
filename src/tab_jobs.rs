//! Jobs for the agents inside tabs. Each tab GodTerm starts (Claude, Grok,
//! Codex, Cursor, OpenCode; not Antigravity, which has no per-launch MCP
//! config) gets a `godterm mcp` server of its own (GODTERM_CLIENT=tab, the
//! tab's uid and a token derived from the control token and that uid) with
//! only create_job, list_jobs, update_job, delete_job, adopt_loop and the
//! two browser tools, all limited to that tab, and (where the CLI has a
//! channel for it) a hint to prefer the job tools over CronCreate and /loop
//! (which die with the session).
//!
//! The scoping lives in one place, `App::tab_scoped` (control.rs). A job a
//! tab's agent asks for is never confirmed by the agent: GodTerm shows the
//! question to the user in a dialog against that tab (the same plan and
//! question the assistant's ask-once path builds) and runs the plan only
//! on the user's yes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::app::{App, Modal};
use crate::control::tab_id;
use crate::harness::Harness;

/// The only tools a tab's agent has.
pub const TAB_TOOLS: &[&str] = &[
    "create_job",
    "list_jobs",
    "update_job",
    "delete_job",
    "adopt_loop",
    "list_browsers",
    "browser_status",
];

/// Appended to the agent's system prompt.
pub const HINT: &str = "Prefer the godterm create_job tool over CronCreate or /loop for anything recurring or scheduled; it survives restarts and shows in GodTerm's Loops & schedules. To use the browser, run `bsk browsers --json`, ask the user which browser if unclear, and use `bsk session start --browser <id>`; GodTerm's list_browsers shows what is connected. If list_browsers shows a bound_profile, this tab has its own Chrome profile: use only that browser with bsk and no other (with several bound_profiles, only those: the primary unless the user means another, and ask when unclear); if none is set, ask the user to click Browser on this tab's bottom bar. Driving it: follow the BrowserSkill skill and `bsk <command> --help` (they track the installed bsk). Pass --tab-id on every page command so it never lands on whichever tab is active. For slow pages use wait-for-navigation, wait-for-element or wait-ms, not reloads. When the user must act in the page (a login, a captcha, a confirm), use `bsk request-help --prompt \"...\"` and wait for it; never ask them to click in the Agent Window, which stops the session. `tab borrow` asks the user to confirm first: say why before you call it. On \"Chrome blocked CDP access to another extension's content\" (a password manager's frame), stay in the page's main frame and leave that frame alone; if the step needs it (autofill), use request-help.";

/// How long a job request waits for the user's answer.
const ASK_FOR: Duration = Duration::from_secs(600);
/// Requests from one tab waiting for the user at once.
const MAX_WAITING: usize = 5;

/// The token of tab `uid`: derived from the launch's read-write control
/// token, so it dies with the launch and names exactly one tab.
pub fn tab_token(shared: &str, uid: u64) -> String {
    let mut h = Sha256::new();
    h.update(b"godterm-tab-token-v1:");
    h.update(shared.as_bytes());
    h.update(b":");
    h.update(uid.to_string().as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `token` is tab `uid`'s token (no early exit on a differing byte).
pub fn token_ok(shared: &str, uid: u64, token: &str) -> bool {
    let want = tab_token(shared, uid);
    want.len() == token.len()
        && want
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |a, (x, y)| a | (x ^ y))
            == 0
}

/// A socket request from a tab client: which tab it is, if the request is
/// well formed, its token is that tab's and the tool is one a tab has.
pub fn authorize(shared: &str, req: &Value) -> Result<u64, String> {
    if req.get("user_says").is_some() {
        return Err("user confirmations come from the user".into());
    }
    let tool = req.get("tool").and_then(Value::as_str).unwrap_or("");
    let uid = req.get("tab").and_then(Value::as_u64);
    let token = req.get("token").and_then(Value::as_str).unwrap_or("");
    let Some(uid) = uid.filter(|u| token_ok(shared, *u, token)) else {
        return Err("bad token".into());
    };
    if !TAB_TOOLS.contains(&tool) {
        return Err(format!(
            "{tool} is not available to a tab's agent (it has {})",
            TAB_TOOLS.join(", ")
        ));
    }
    Ok(uid)
}

/// The `godterm mcp` server entry of tab `uid`.
pub fn mcp_server(exe: &Path, shared: &str, uid: u64) -> Value {
    let mut v = json!({
        "command": exe,
        "args": ["mcp"],
        "env": {
            "GODTERM_HOME": crate::config::app_home(),
            "GODTERM_CLIENT": "tab",
            "GODTERM_TAB": uid.to_string(),
            "GODTERM_TOKEN": tab_token(shared, uid),
        },
    });
    if cfg!(test) {
        v["env"][crate::test_guard::ENV] = json!("tab-mcp");
    }
    v
}

/// Claude's `--mcp-config` file for tab `uid`.
pub fn write_mcp_config(exe: &Path, shared: &str, uid: u64) -> Result<PathBuf> {
    let d = crate::config::app_home().join("tab-mcp");
    std::fs::create_dir_all(&d)?;
    let p = d.join(format!("mcp-t{uid}.json"));
    let cfg = json!({"mcpServers": {"godterm": mcp_server(exe, shared, uid)}});
    crate::config::write_private(&p, serde_json::to_string_pretty(&cfg)?)?;
    Ok(p)
}

/// Add `hint` to the system prompt text a flag already carries
/// (`--append-system-prompt x` or `--append-system-prompt=x`), else add
/// the flag.
fn add_hint(args: &mut Vec<String>, flag: &str, hint: &str) {
    let eq = format!("{flag}=");
    for i in 0..args.len() {
        if args[i] == flag {
            if let Some(v) = args.get_mut(i + 1) {
                *v = format!("{v}\n\n{hint}");
                return;
            }
        } else if args[i].starts_with(&eq) {
            args[i] = format!("{}\n\n{hint}", args[i]);
            return;
        }
    }
    args.push(flag.into());
    args.push(hint.into());
}

/// What a tab's launch gets besides the harness' own flags.
#[derive(Debug, Default, PartialEq)]
pub struct Extras {
    pub env: Vec<(String, String)>,
}

/// Add the tab's job tools and the hint to a launch: Claude gets
/// `--mcp-config` (next to the user's own, never `--strict-mcp-config`),
/// `--allowedTools` for these tools and `--append-system-prompt`; Grok gets
/// `--rules` and the tab's identity in its environment (its `godterm`
/// server is in the account's config.toml, see `harness::grok`); Codex gets
/// `-c` overrides, Cursor `--plugin-dir`, OpenCode a config file and
/// `--standalone` (see the functions below).
#[cfg(test)]
pub fn extend_launch(
    h: Harness,
    exe: &Path,
    shared: &str,
    uid: u64,
    args: &mut Vec<String>,
) -> Result<Extras> {
    extend_launch_for(h, exe, shared, uid, args, Path::new("/nonexistent"), false)
}

/// A TOML basic string (also what `codex -c key=value` parses).
fn toml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Codex: the tab's server as `-c` overrides (nothing is written to its
/// config.toml), its identity forwarded from the launch environment with
/// `env_vars` so the token never shows on a command line, the server's
/// tools approved up front (GodTerm asks the user itself, as for Claude's
/// --allowedTools), and the hint as `developer_instructions` appended to
/// the slot's own, if its config.toml has one.
fn codex_launch(exe: &Path, shared: &str, uid: u64, home: &Path, args: &mut Vec<String>) -> Extras {
    let srv = mcp_server(exe, shared, uid);
    let mut env = vec![format!(
        "GODTERM_HOME={}",
        toml_str(&crate::config::app_home().to_string_lossy())
    )];
    env.push(format!("GODTERM_CLIENT={}", toml_str("tab")));
    if let Some(g) = srv["env"]
        .get(crate::test_guard::ENV)
        .and_then(Value::as_str)
    {
        env.push(format!("{}={}", crate::test_guard::ENV, toml_str(g)));
    }
    let mut c = |kv: String| {
        args.push("-c".into());
        args.push(kv);
    };
    c(format!(
        "mcp_servers.godterm.command={}",
        toml_str(&exe.to_string_lossy())
    ));
    c("mcp_servers.godterm.args=[\"mcp\"]".into());
    c(format!("mcp_servers.godterm.env={{{}}}", env.join(",")));
    c("mcp_servers.godterm.env_vars=[\"GODTERM_TAB\",\"GODTERM_TOKEN\"]".into());
    c("mcp_servers.godterm.default_tools_approval_mode=\"approve\"".into());
    let own = std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|d| {
            d.get("developer_instructions")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    let hint = match own {
        Some(o) if !o.trim().is_empty() => format!("{o}\n\n{HINT}"),
        _ => HINT.to_string(),
    };
    c(format!("developer_instructions={}", toml_str(&hint)));
    Extras {
        env: vec![
            ("GODTERM_TAB".into(), uid.to_string()),
            ("GODTERM_TOKEN".into(), tab_token(shared, uid)),
        ],
    }
}

/// Cursor: a plugin directory per tab (`--plugin-dir`), holding only the
/// tab's MCP server. The server shows in the CLI as "plugin-<dir>-godterm"
/// and needs no entry in ~/.cursor/mcp.json.
pub fn write_cursor_plugin(exe: &Path, shared: &str, uid: u64) -> Result<PathBuf> {
    let d = crate::config::app_home()
        .join("tab-mcp")
        .join(format!("cursor-t{uid}"));
    std::fs::create_dir_all(d.join(".cursor-plugin"))?;
    let manifest = json!({
        "name": format!("godterm-tab-{uid}"),
        "description": "GodTerm job and browser tools for this tab",
        "version": "1.0.0",
    });
    crate::config::write_private(
        &d.join(".cursor-plugin").join("plugin.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    let cfg = json!({"mcpServers": {"godterm": mcp_server(exe, shared, uid)}});
    crate::config::write_private(&d.join("mcp.json"), serde_json::to_string_pretty(&cfg)?)?;
    Ok(d)
}

/// OpenCode: the config file `OPENCODE_CONFIG` names for one launch. It
/// layers over the user's own config (nothing there is edited): the tab's
/// MCP server when `server` is given, and the plan agent as the default
/// when `plan` (2.x has no flag for an agent). Only a `--standalone`
/// process reads it; the shared background service does not.
pub fn write_opencode_config(
    uid: u64,
    server: Option<(&Path, &str)>,
    plan: bool,
) -> Result<PathBuf> {
    let d = crate::config::app_home().join("tab-mcp");
    std::fs::create_dir_all(&d)?;
    let p = d.join(format!("opencode-t{uid}.json"));
    let mut cfg = json!({});
    if plan {
        cfg["default_agent"] = json!("plan");
    }
    if let Some((exe, shared)) = server {
        let s = mcp_server(exe, shared, uid);
        cfg["mcp"] = json!({"servers": {"godterm": {
            "type": "local",
            "command": [s["command"].clone(), "mcp"],
            "environment": s["env"].clone(),
        }}});
    }
    crate::config::write_private(&p, serde_json::to_string_pretty(&cfg)?)?;
    Ok(p)
}

/// What OpenCode gets besides its flags: the config file (when its tools
/// or plan mode need one) and `--standalone`. Not when the user's own
/// environment already names an OPENCODE_CONFIG: that is theirs.
pub fn opencode_launch(
    uid: u64,
    server: Option<(&Path, &str)>,
    plan: bool,
    args: &mut Vec<String>,
) -> Vec<(String, String)> {
    if server.is_none() && !plan {
        return vec![];
    }
    if std::env::var_os("OPENCODE_CONFIG").is_some() {
        crate::log::info("OPENCODE_CONFIG is set: not adding GodTerm's tab config to opencode");
        return vec![];
    }
    match write_opencode_config(uid, server, plan) {
        Ok(p) => {
            args.insert(0, "--standalone".into());
            vec![("OPENCODE_CONFIG".into(), p.to_string_lossy().into_owned())]
        }
        Err(e) => {
            crate::log::error(&format!("opencode config for t{uid}: {e:#}"));
            vec![]
        }
    }
}

pub fn extend_launch_for(
    h: Harness,
    exe: &Path,
    shared: &str,
    uid: u64,
    args: &mut Vec<String>,
    home: &Path,
    plan: bool,
) -> Result<Extras> {
    match h {
        Harness::Codex => Ok(codex_launch(exe, shared, uid, home, args)),
        Harness::Cursor => {
            let d = write_cursor_plugin(exe, shared, uid)?;
            args.push("--plugin-dir".into());
            args.push(d.to_string_lossy().into_owned());
            Ok(Extras::default())
        }
        Harness::OpenCode => Ok(Extras {
            env: opencode_launch(uid, Some((exe, shared)), plan, args),
        }),
        Harness::Claude => {
            let p = write_mcp_config(exe, shared, uid)?;
            args.push("--mcp-config".into());
            args.push(p.to_string_lossy().into_owned());
            args.push("--allowedTools".into());
            args.extend(TAB_TOOLS.iter().map(|t| format!("mcp__godterm__{t}")));
            // After the variadic --allowedTools a flag must follow.
            add_hint(args, "--append-system-prompt", HINT);
            Ok(Extras::default())
        }
        Harness::Grok => {
            add_hint(args, "--rules", HINT);
            Ok(Extras {
                env: vec![
                    ("GODTERM_TAB".into(), uid.to_string()),
                    ("GODTERM_TOKEN".into(), tab_token(shared, uid)),
                ],
            })
        }
        // No tab server and no hint: GodTerm cannot scope tools for these
        // CLIs (see Harness::has_tab_tools).
        _ => Ok(Extras::default()),
    }
}

/// The `godterm` server in a Grok account's config.toml (Grok has no flag
/// for one): present when `on`, else removed. A server of that name that
/// is not ours (no GODTERM_CLIENT = tab) is the user's and left alone.
/// The tab's identity comes from the environment `extend_launch` sets.
pub fn write_grok_server(home: &Path, exe: &Path, on: bool) {
    // Only GodTerm's own account homes, never the user's ~/.grok.
    if !home.starts_with(crate::config::app_home()) && !cfg!(test) {
        return;
    }
    let p = home.join("config.toml");
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>() else {
        return;
    };
    let ours = |doc: &toml_edit::DocumentMut| {
        doc.get("mcp_servers")
            .and_then(|s| s.get("godterm"))
            .and_then(|g| g.get("env"))
            .and_then(|e| e.get("GODTERM_CLIENT"))
            .and_then(|c| c.as_str())
            == Some("tab")
    };
    let exists = doc
        .get("mcp_servers")
        .and_then(|s| s.get("godterm"))
        .is_some();
    if exists && !ours(&doc) {
        return;
    }
    if !on {
        if exists {
            if let Some(s) = doc["mcp_servers"].as_table_mut() {
                s.remove("godterm");
            }
            let _ = crate::config::write_private(&p, doc.to_string());
        }
        return;
    }
    let mut env = toml_edit::InlineTable::new();
    env.insert(
        "GODTERM_HOME",
        crate::config::app_home().to_string_lossy().as_ref().into(),
    );
    env.insert("GODTERM_CLIENT", "tab".into());
    if cfg!(test) {
        env.insert(crate::test_guard::ENV, "tab-mcp".into());
    }
    let mut args = toml_edit::Array::new();
    args.push("mcp");
    let mut s = toml_edit::Table::new();
    s.insert("command", toml_edit::value(exe.to_string_lossy().as_ref()));
    s.insert("args", toml_edit::value(args));
    s.insert("env", toml_edit::value(env));
    let servers = doc.entry("mcp_servers").or_insert(toml_edit::table());
    let Some(servers) = servers.as_table_mut() else {
        return;
    };
    servers.set_implicit(true);
    servers.insert("godterm", toml_edit::Item::Table(s));
    let new = doc.to_string();
    if new != text {
        let _ = std::fs::create_dir_all(home);
        let _ = crate::config::write_private(&p, new);
    }
}

/// What a tab's `godterm mcp` says about each tool: the same tools, for
/// this tab only.
pub fn tab_description(tool: &str) -> &'static str {
    match tool {
        "create_job" => "Create a GodTerm job for THIS tab: a prompt GodTerm sends to this tab on a schedule. Prefer it over CronCreate or /loop for anything recurring or scheduled: unlike those (they live in this session's memory, die when it exits and expire after 7 days) a job is kept by GodTerm, survives restarts, shows in GodTerm's Loops & schedules, and still fires when the tab was closed (GodTerm resumes this session first). prompt: exactly what you will be asked each time. Schedule, exactly one of: cron (five fields, local time: \"0 9 * * 1-5\"), every (\"30 minutes\", \"2 hours\", \"1 day\") or at (one run: \"in 2 hours\", \"2026-10-15 09:00\"). catch_up: once (default) or skip. until: an optional end date. name: a short label. GodTerm asks the USER to approve it in its own dialog: the reply is state waiting_for_user, not done; do not ask again or repeat the call, tell the user it is waiting and check list_jobs later.",
        "list_jobs" => "This tab's GodTerm jobs (id, name, prompt, schedule, next and last run, how it went) and its session loops (a /loop or CronCreate job that dies with this session; adopt_loop turns one into a job), and what the user answered to your recent job requests. Filters: status (active, paused, finished, failing), text.",
        "update_job" => "Change one of this tab's GodTerm jobs (job: id or name): action pause or resume; or name, prompt, cron / every / at, catch_up, until (an end date, or \"none\"). Pause, resume, name and catch_up apply at once; the rest waits for the user's approval in GodTerm (state waiting_for_user).",
        "delete_job" => "Delete one of this tab's GodTerm jobs for good (job: id or name). Waits for the user's approval in GodTerm (state waiting_for_user). To stop it for a while use update_job with action pause.",
        "adopt_loop" => "Turn this tab's own session loop (a /loop or CronCreate job; see session_loops in list_jobs) into a GodTerm job so it survives restarts: copies its prompt and schedule (a dynamic /loop has none: pass cron or every). By default you should then cancel the session loop yourself so it does not fire twice (CronDelete), unless keep_session_loop is true. loop: the loop id (optional when there is one). Also name, catch_up, until. Waits for the user's approval in GodTerm (state waiting_for_user).",
        "list_browsers" => "THIS tab's browsers for BrowserSkill (bsk): the Chrome profiles the user bound to this tab (bound_profiles, the primary first; bound_profile is the primary) and the connected browsers that are them (browsers: instance_id, label, versions, open sessions, and profile_dir and primary saying whose each is). Use only those browsers, never another: the primary one unless the request is clearly about another bound profile's account or site, and when it is unclear which, ask the user. The instance_id goes to `bsk session start --browser <id>`. You never choose or change a tab's browser yourself. No bound_profile: the user has not set one; ask them to choose one (Browser on this tab's bottom bar, or the tab's Browser section in the web app). Read only.",
        "browser_status" => "BrowserSkill (bsk) status for THIS tab, read only: installed and version, daemon state, doctor summary, this tab's bound Chrome profiles (bound_profiles, the primary first) with their extension state, and the connected browsers that are them. Use only those; ask the user which when unclear.",
        _ => "",
    }
}

/// A tool's input schema for a tab: the control API's, without the
/// arguments that name another tab or confirm.
pub fn tab_schema(schema: Value) -> Value {
    let mut s = schema;
    if let Some(p) = s.get_mut("properties").and_then(Value::as_object_mut) {
        for k in [
            "tab",
            "archived",
            "account",
            "confirm_token",
            "reissue_token",
        ] {
            p.remove(k);
        }
    }
    s
}

/// The tools a tab's `godterm mcp` lists.
pub fn tab_tool_list() -> Vec<Value> {
    crate::control::TOOLS
        .iter()
        .filter(|t| TAB_TOOLS.contains(&t.name))
        .map(|t| {
            json!({
                "name": t.name,
                "description": tab_description(t.name),
                "inputSchema": tab_schema((t.schema)()),
            })
        })
        .collect()
}

/// A job request from a tab's agent waiting for the user.
#[derive(Debug, Clone)]
pub struct TabAsk {
    pub token: String,
    pub uid: u64,
    pub tool: String,
    pub plan: Value,
    pub question: String,
    pub at: Instant,
}

/// The app's state for tab job requests.
#[derive(Debug, Default)]
pub struct TabJobs {
    pub asks: Vec<TabAsk>,
    /// The request the dialog on screen is about.
    pub shown: Option<String>,
    /// What the user answered, per tab (newest last): list_jobs says it.
    pub answers: Vec<(u64, String)>,
}

impl App {
    /// One call from the agent of tab `uid` (already authorized at the
    /// socket): scoped to that tab, then run; what needs a yes waits for
    /// the user's.
    pub(crate) fn tab_call(&mut self, uid: u64, tool: &str, args: &Value) -> Value {
        match self.tab_call_inner(uid, tool, args) {
            Ok(v) => v,
            Err(e) => json!({"ok": false, "error": e}),
        }
    }

    fn tab_call_inner(&mut self, uid: u64, tool: &str, args: &Value) -> Result<Value, String> {
        let a = self.tab_scoped(uid, tool, args)?;
        if matches!(tool, "list_browsers" | "browser_status") {
            return self.browser_tool_for(uid, tool);
        }
        if tool == "list_jobs" {
            let mut v = self.list_jobs_json(&a)?;
            let said: Vec<&str> = self
                .tab_jobs
                .answers
                .iter()
                .filter(|(u, _)| *u == uid)
                .map(|(_, s)| s.as_str())
                .collect();
            if !said.is_empty() {
                v["result"]["your_requests"] = json!(said);
            }
            let waiting: Vec<&str> = self
                .tab_jobs
                .asks
                .iter()
                .filter(|k| k.uid == uid)
                .map(|k| k.question.as_str())
                .collect();
            if !waiting.is_empty() {
                v["result"]["waiting_for_user"] = json!(waiting);
            }
            return Ok(v);
        }
        let (mut plan, question, needs) = self.plan_for(tool, &a)?;
        // Jobs made here say so.
        if matches!(tool, "create_job" | "adopt_loop") {
            if let Some(j) = plan.get_mut("job") {
                j["created_by"] = json!("tab");
            }
        }
        if !needs {
            return self.run_plan(tool, &plan);
        }
        let label = self
            .find_tab(uid)
            .map(|(s, t)| self.panes[s].tabs[t].name())
            .unwrap_or_default();
        let question = format!("The agent in the \"{label}\" tab asks: {question}");
        let mine = self.tab_jobs.asks.iter().filter(|k| k.uid == uid).count();
        if !self
            .tab_jobs
            .asks
            .iter()
            .any(|k| k.uid == uid && k.question == question)
        {
            if mine >= MAX_WAITING {
                return Err(format!(
                    "{MAX_WAITING} job requests from this tab are already waiting for the user: wait for their answer"
                ));
            }
            self.tab_jobs.asks.push(TabAsk {
                token: crate::session_ops::new_uuid()[..8].to_string(),
                uid,
                tool: tool.into(),
                plan,
                question: question.clone(),
                at: Instant::now(),
            });
            crate::log::info(&format!("tab job: t{uid} {tool} asks: {question}"));
            self.flash(format!(
                "The {label} tab asks to change its jobs: answer in the dialog"
            ));
        }
        Ok(json!({"ok": true, "result": {
            "state": "waiting_for_user",
            "question": question,
            "say": "GodTerm is asking the user to approve this; it is not done yet. Do not repeat the call. Tell the user it is waiting for their yes in GodTerm; list_jobs shows the outcome.",
        }}))
    }

    /// The user's answer to the request `token`.
    pub fn tab_ask_answer(&mut self, token: &str, yes: bool) {
        let Some(i) = self.tab_jobs.asks.iter().position(|k| k.token == token) else {
            return;
        };
        let k = self.tab_jobs.asks.remove(i);
        if self.tab_jobs.shown.as_deref() == Some(token) {
            self.tab_jobs.shown = None;
        }
        let note = if !yes {
            format!("declined by the user: {}", k.question)
        } else if self.find_tab(k.uid).is_none() {
            format!("not done, the tab is gone: {}", k.question)
        } else {
            match self.run_plan(&k.tool, &k.plan) {
                Ok(v) => format!(
                    "approved and done: {}",
                    v["result"]["say"]
                        .as_str()
                        .or(v["say"].as_str())
                        .unwrap_or(&k.question)
                ),
                Err(e) => format!("approved but failed ({e}): {}", k.question),
            }
        };
        crate::log::info(&format!("tab job: t{} {note}", k.uid));
        self.flash(note.chars().take(100).collect::<String>());
        self.tab_jobs.answers.push((k.uid, note));
        if self.tab_jobs.answers.len() > 20 {
            self.tab_jobs.answers.remove(0);
        }
    }

    /// Show the next waiting request when no other dialog is open; one
    /// whose dialog was closed another way counts as declined; old ones
    /// expire.
    pub fn tab_jobs_tick(&mut self) {
        if self.tab_jobs.asks.is_empty() && self.tab_jobs.shown.is_none() {
            return;
        }
        if let Some(tok) = self.tab_jobs.shown.clone() {
            let still = matches!(&self.modal, Modal::ConfirmTabJob(t) if *t == tok);
            if !still {
                self.tab_ask_answer(&tok, false);
            }
        }
        let old: Vec<String> = self
            .tab_jobs
            .asks
            .iter()
            .filter(|k| crate::clock::age(k.at) > ASK_FOR)
            .map(|k| k.token.clone())
            .collect();
        for t in old {
            if let Some(k) = self.tab_jobs.asks.iter().find(|k| k.token == t) {
                let note = format!("expired, the user did not answer: {}", k.question);
                self.tab_jobs.answers.push((k.uid, note));
            }
            self.tab_jobs.asks.retain(|k| k.token != t);
            if matches!(&self.modal, Modal::ConfirmTabJob(x) if *x == t) {
                self.modal = Modal::None;
                self.tab_jobs.shown = None;
            }
        }
        if matches!(self.modal, Modal::None) && self.tab_jobs.shown.is_none() {
            if let Some(k) = self.tab_jobs.asks.first() {
                self.tab_jobs.shown = Some(k.token.clone());
                self.modal = Modal::ConfirmTabJob(k.token.clone());
            }
        }
    }

    /// The question of request `token`, and the tab it is about.
    pub fn tab_ask_text(&self, token: &str) -> Option<(String, String)> {
        let k = self.tab_jobs.asks.iter().find(|k| k.token == token)?;
        let tab = self
            .find_tab(k.uid)
            .map(|(s, t)| {
                format!(
                    "{} on {}",
                    self.panes[s].tabs[t].name(),
                    self.describe_tab(s)
                )
            })
            .unwrap_or_else(|| tab_id(k.uid));
        Some((tab, k.question.clone()))
    }

    /// What to add to the launch of tab `uid` (pane `pane`) on harness
    /// `h`: its job tools and hint, unless `tabs.jobs_tools` is off or the
    /// control socket is not up. Never fails a launch.
    pub fn tab_jobs_launch(
        &self,
        h: Harness,
        uid: u64,
        config_dir: &Path,
        args: &mut Vec<String>,
        plan: bool,
    ) -> Vec<(String, String)> {
        if !h.has_tab_tools() {
            return vec![];
        }
        // OpenCode has no plan flag: plan mode is a config file, with or
        // without the tab's tools.
        let off = |args: &mut Vec<String>| {
            if h == Harness::OpenCode && plan {
                opencode_launch(uid, None, true, args)
            } else {
                vec![]
            }
        };
        if h == Harness::Grok {
            // Switched off: the server entry we added goes again.
            write_grok_server(
                config_dir,
                &std::env::current_exe().unwrap_or_else(|_| "godterm".into()),
                self.cfg.tabs.jobs_tools && shared_token().is_some(),
            );
        }
        if !self.cfg.tabs.jobs_tools {
            return off(args);
        }
        let Some(shared) = shared_token() else {
            return off(args);
        };
        let exe = std::env::current_exe().unwrap_or_else(|_| "godterm".into());
        match extend_launch_for(h, &exe, &shared, uid, args, config_dir, plan) {
            Ok(x) => x.env,
            Err(e) => {
                crate::log::error(&format!("tab job tools for t{uid}: {e:#}"));
                off(args)
            }
        }
    }
}

/// The launch's read-write control token (a fixed one in tests).
pub fn shared_token() -> Option<String> {
    crate::control::brain_token().or_else(|| cfg!(test).then(|| "test-shared-token".to_string()))
}
