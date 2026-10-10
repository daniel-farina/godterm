//! Coding agent CLIs: Claude, Grok, Codex, Cursor, Antigravity and OpenCode.
//! Launch flags and home policy live here. Claude/Grok also have usage,
//! credential and transcript adapters; native integrations let their CLIs
//! manage those features (see docs/CLI_HARNESSES.md).

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::sessions::{SessionInfo, Tokens};
use crate::usage::{Usage, UsageError, Window};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Grok,
    Codex,
    Cursor,
    Antigravity,
    OpenCode,
}

pub const NAMES: &[&str] = &[
    "claude",
    "grok",
    "codex",
    "cursor",
    "antigravity",
    "opencode",
];
pub const ALL: &[Harness] = &[
    Harness::Claude,
    Harness::Grok,
    Harness::Codex,
    Harness::Cursor,
    Harness::Antigravity,
    Harness::OpenCode,
];

impl Harness {
    pub fn of(name: &str) -> Harness {
        match name.trim().to_lowercase().as_str() {
            "grok" => Harness::Grok,
            "codex" => Harness::Codex,
            "cursor" | "cursor-agent" => Harness::Cursor,
            "antigravity" | "agy" => Harness::Antigravity,
            "opencode" => Harness::OpenCode,
            _ => Harness::Claude,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Grok => "grok",
            Harness::Codex => "codex",
            Harness::Cursor => "cursor",
            Harness::Antigravity => "antigravity",
            Harness::OpenCode => "opencode",
        }
    }

    /// The variable naming each account's isolated home.
    pub fn home_env(self) -> &'static str {
        match self {
            Harness::Claude => "CLAUDE_CONFIG_DIR",
            Harness::Grok => "GROK_HOME",
            Harness::Codex => "CODEX_HOME",
            // These CLIs use their native config and credential stores.
            // An empty key means no home override; never invent an env var
            // and claim it isolates logins.
            Harness::Cursor | Harness::Antigravity | Harness::OpenCode => "",
        }
    }

    /// The binary to run, given the configured override.
    pub fn bin(self, configured: Option<&str>) -> String {
        if let Some(c) = configured.filter(|c| !c.trim().is_empty()) {
            return crate::config::expand_tilde(c)
                .to_string_lossy()
                .into_owned();
        }
        // Tests never start the user's real claude or grok (nor anything
        // on PATH by those names): unless a test names one, a program that
        // exits at once.
        if cfg!(test) {
            return test_agent();
        }
        match self {
            Harness::Claude => "claude".into(),
            Harness::Grok => {
                let local = crate::config::home_dir().join(".local/bin/grok");
                if local.exists() {
                    local.to_string_lossy().into_owned()
                } else {
                    "grok".into()
                }
            }
            Harness::Codex => "codex".into(),
            Harness::Cursor => {
                if crate::deps::which("cursor-agent").is_some() {
                    "cursor-agent".into()
                } else {
                    "agent".into()
                }
            }
            Harness::Antigravity => "agy".into(),
            Harness::OpenCode => "opencode".into(),
        }
    }

    /// Flags for a permission mode (GodTerm's names, see config.rs).
    pub fn permission_args(self, mode: &str) -> Vec<String> {
        match self {
            Harness::Claude => crate::config::permission_args(mode),
            Harness::Grok => {
                let m = match mode {
                    "bypass" => return vec!["--always-approve".into()],
                    "accept-edits" | "acceptEdits" => "acceptEdits",
                    "auto" => "auto",
                    "plan" => "plan",
                    "dont-ask" | "dontAsk" => "dontAsk",
                    "manual" => "default",
                    _ => return vec![],
                };
                vec!["--permission-mode".into(), m.into()]
            }
            Harness::Codex => match mode {
                "bypass" => vec!["--dangerously-bypass-approvals-and-sandbox".into()],
                "accept-edits" | "acceptEdits" => vec![
                    "--sandbox".into(),
                    "workspace-write".into(),
                    "--ask-for-approval".into(),
                    "on-request".into(),
                ],
                "plan" => vec!["--sandbox".into(), "read-only".into()],
                "manual" => vec!["--ask-for-approval".into(), "on-request".into()],
                "dont-ask" | "dontAsk" => vec!["--ask-for-approval".into(), "never".into()],
                _ => vec![],
            },
            Harness::Cursor => match mode {
                "bypass" => vec!["--force".into()],
                "plan" => vec!["--mode".into(), "plan".into()],
                _ => vec![],
            },
            Harness::Antigravity => match mode {
                "bypass" => vec!["--dangerously-skip-permissions".into()],
                "accept-edits" | "acceptEdits" => vec!["--mode".into(), "accept-edits".into()],
                "plan" => vec!["--mode".into(), "plan".into()],
                _ => vec![],
            },
            // OpenCode permissions are configured in opencode.json; its
            // plan agent is the supported read-only launch option.
            Harness::OpenCode => match mode {
                "plan" => vec!["--agent".into(), "plan".into()],
                _ => vec![],
            },
        }
    }

    pub fn resume_args(self, id: &str) -> Vec<String> {
        match self {
            Harness::Claude => vec!["--resume".into(), id.into()],
            Harness::Grok => vec!["--resume".into(), id.into()],
            Harness::Codex => vec!["resume".into(), id.into()],
            Harness::Cursor => vec!["--resume".into(), id.into()],
            Harness::Antigravity => vec!["--conversation".into(), id.into()],
            Harness::OpenCode => vec!["--session".into(), id.into()],
        }
    }

    /// Always added: grok gets its own leader per account, so accounts
    /// never share one (and its login).
    pub fn base_args(self, home: &Path) -> Vec<String> {
        match self {
            Harness::Claude
            | Harness::Codex
            | Harness::Cursor
            | Harness::Antigravity
            | Harness::OpenCode => vec![],
            Harness::Grok => vec![
                "--leader-socket".into(),
                home.join("leader.sock").to_string_lossy().into_owned(),
            ],
        }
    }

    /// What a login tab runs (after the binary).
    pub fn login_args(self) -> Option<Vec<String>> {
        match self {
            Harness::Claude => None, // claude's own onboarding / /login
            Harness::Grok => Some(vec!["login".into()]),
            Harness::Codex | Harness::Cursor => Some(vec!["login".into()]),
            Harness::Antigravity | Harness::OpenCode => None,
        }
    }

    /// Short badge shown in pane headers (None for the default harness).
    pub fn badge(self) -> Option<&'static str> {
        match self {
            Harness::Claude => None,
            _ => Some(self.name()),
        }
    }

    /// The user's main (non GodTerm) home of this harness.
    pub fn main_home(self) -> PathBuf {
        match self {
            Harness::Claude => crate::session_ops::main_dir(),
            Harness::Grok => crate::config::dirs().main_grok,
            Harness::Codex => crate::config::home_dir().join(".codex"),
            Harness::Cursor => crate::config::home_dir().join(".cursor"),
            Harness::Antigravity => crate::config::home_dir().join(".gemini/antigravity-cli"),
            Harness::OpenCode => crate::config::home_dir().join(".config/opencode"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Grok => "Grok Build",
            Self::Codex => "Codex CLI",
            Self::Cursor => "Cursor CLI",
            Self::Antigravity => "Antigravity CLI",
            Self::OpenCode => "OpenCode",
        }
    }

    pub fn bin_key(self) -> &'static str {
        match self {
            Self::Claude => "claude_bin",
            Self::Grok => "grok_bin",
            Self::Codex => "codex_bin",
            Self::Cursor => "cursor_bin",
            Self::Antigravity => "antigravity_bin",
            Self::OpenCode => "opencode_bin",
        }
    }

    /// Only these harnesses have GodTerm transcript, usage and admin adapters.
    pub fn integrated(self) -> bool {
        matches!(self, Self::Claude | Self::Grok)
    }

    pub fn installed(self, cfg: &crate::config::Config) -> bool {
        crate::deps::which(&cfg.harness_bin(self)).is_some()
    }
}

#[cfg(test)]
fn test_agent() -> String {
    crate::test_stub::true_bin()
}

#[cfg(not(test))]
fn test_agent() -> String {
    unreachable!("only tests")
}

/// Variables a parent grok leaves in the environment that would make a
/// child grok act as its subagent, or use another account's login.
pub fn scrub_grok(key: &str) -> bool {
    key == "GROK_HOME"
        || key.starts_with("GROK_AGENT")
        || key.starts_with("GROK_SESSION")
        || key.starts_with("GROK_ACTIVE_")
        || key.starts_with("GROK_LEADER")
        || key == "GROK_AUTH"
        || key == "GROK_ASKPASS"
}

pub mod grok {
    use super::*;

    /// Mirror the compat switches into the slot's own config.toml
    /// ([compat.claude], comments and other keys kept), so they hold even
    /// without the environment. Only ever the GodTerm slot's file, never
    /// the user's ~/.grok/config.toml.
    pub fn write_slot_compat(home: &Path, c: &crate::config::GrokCompat) {
        if !home.starts_with(crate::config::app_home()) && !cfg!(test) {
            return;
        }
        let p = home.join("config.toml");
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>() else {
            return;
        };
        let compat = doc.entry("compat").or_insert(toml_edit::table());
        let Some(compat) = compat.as_table_mut() else {
            return;
        };
        compat.set_implicit(true);
        let claude = compat.entry("claude").or_insert(toml_edit::table());
        let Some(claude) = claude.as_table_mut() else {
            return;
        };
        for (k, v) in [
            ("hooks", c.hooks),
            ("skills", c.skills),
            ("rules", c.rules),
            ("agents", c.agents),
            ("mcps", c.mcps),
            ("sessions", c.sessions),
        ] {
            claude[k] = toml_edit::value(v);
        }
        // compat.claude.hooks covers ~/.claude/settings.json; grok still
        // loads the hooks of Claude plugins it finds in ~/.claude/plugins.
        // With hooks off, those plugins are listed as disabled for this
        // slot (the user's own entries are kept).
        // (Demo mode never looks at the user's ~/.claude.)
        if !c.hooks && !crate::demo::active() {
            let names =
                claude_plugins_with_hooks(&crate::config::home_dir().join(".claude/plugins"));
            if !names.is_empty() {
                let plugins = doc.entry("plugins").or_insert(toml_edit::table());
                if let Some(t) = plugins.as_table_mut() {
                    let mut list: Vec<String> = t
                        .get("disabled")
                        .and_then(|d| d.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    for n in names {
                        if !list.contains(&n) {
                            list.push(n);
                        }
                    }
                    let mut arr = toml_edit::Array::new();
                    for n in list {
                        arr.push(n);
                    }
                    t["disabled"] = toml_edit::value(arr);
                }
            }
        }
        let out = doc.to_string();
        if out != text {
            let _ = std::fs::write(&p, out);
        }
    }

    /// Names of Claude Code plugins that ship hooks
    /// (`cache/<market>/<name>/<version>/hooks/hooks.json`,
    /// `marketplaces/<market>/plugins/<name>/hooks/hooks.json`).
    pub fn claude_plugins_with_hooks(root: &Path) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        let mut add = |n: String| {
            if !out.contains(&n) {
                out.push(n);
            }
        };
        let dirs = |p: &Path| {
            std::fs::read_dir(p)
                .map(|r| {
                    r.flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        for market in dirs(&root.join("cache")) {
            for plugin in dirs(&market) {
                if dirs(&plugin)
                    .iter()
                    .any(|v| v.join("hooks/hooks.json").is_file())
                {
                    add(plugin
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned());
                }
            }
        }
        for market in dirs(&root.join("marketplaces")) {
            for plugin in dirs(&market.join("plugins")) {
                if plugin.join("hooks/hooks.json").is_file() {
                    add(plugin
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned());
                }
            }
        }
        out.sort();
        out
    }

    /// Grok Build is installed (the configured binary, ~/.local/bin/grok,
    /// or grok on PATH).
    pub fn installed(configured: Option<&str>) -> bool {
        let bin = Harness::Grok.bin(configured);
        Path::new(&bin).is_file()
            || std::env::var_os("PATH")
                .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(&bin).is_file()))
    }

    /// Logged in: the slot has a non empty auth.json (its contents are only
    /// read for the usage call, never shown or logged).
    pub fn logged_in(home: &Path) -> bool {
        std::fs::metadata(home.join("auth.json")).is_ok_and(|m| m.len() > 2)
    }

    /// The account email in a slot's auth.json, if it records one.
    pub fn email(home: &Path) -> Option<String> {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).ok()?).ok()?;
        fn find(v: &Value) -> Option<String> {
            match v {
                Value::Object(o) => o
                    .get("email")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| o.values().find_map(find)),
                _ => None,
            }
        }
        find(&v)
    }

    /// The bearer key in a slot's auth.json.
    pub fn token(home: &Path) -> Option<String> {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).ok()?).ok()?;
        find_key(&v)
    }

    fn find_key(v: &Value) -> Option<String> {
        const NAMES: &[&str] = &[
            "key",
            "api_key",
            "apiKey",
            "access_token",
            "accessToken",
            "token",
        ];
        if let Value::Object(o) = v {
            for n in NAMES {
                if let Some(s) = o.get(*n).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    return Some(s.to_string());
                }
            }
            for x in o.values() {
                if let Some(s) = find_key(x) {
                    return Some(s);
                }
            }
        }
        None
    }

    pub const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";

    /// The credits reply as usage buckets: the period's credits (grok's
    /// "Weekly limit") as "seven_day", then each product that reports a
    /// percent. grok 1.0.45 wraps it all in "config".
    pub fn parse_billing(v: &Value) -> Usage {
        let v = v.get("config").unwrap_or(v);
        let resets_at = v
            .pointer("/currentPeriod/end")
            .or_else(|| v.get("billingPeriodEnd"))
            .and_then(|e| {
                e.as_str()
                    .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                    .or_else(|| {
                        e.as_i64().and_then(|n| {
                            DateTime::from_timestamp(
                                if n > 10_000_000_000 { n / 1000 } else { n },
                                0,
                            )
                        })
                    })
            });
        let period = v
            .pointer("/currentPeriod/type")
            .and_then(Value::as_str)
            .unwrap_or("USAGE_PERIOD_TYPE_WEEKLY");
        let label = if period.contains("MONTH") {
            "Monthly"
        } else if period.contains("DAY") && !period.contains("WEEK") {
            "Daily"
        } else {
            "Weekly"
        };
        let mut windows = vec![];
        // proto3 JSON leaves zero values out: a reply with a period but no
        // creditUsagePercent has used none of it (100% left).
        let credit = v
            .get("creditUsagePercent")
            .and_then(Value::as_f64)
            .or_else(|| v.get("currentPeriod").map(|_| 0.0));
        if let Some(p) = credit {
            windows.push(Window {
                key: "seven_day".into(),
                label: label.into(),
                short: label.into(),
                utilization: p,
                resets_at,
            });
        }
        for item in v
            .get("productUsage")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = ["product", "name", "productName"]
                .iter()
                .find_map(|k| item.get(*k).and_then(Value::as_str))
                .unwrap_or("product");
            let pct = [
                "usagePercent",
                "creditUsagePercent",
                "percent",
                "usage_percent",
            ]
            .iter()
            .find_map(|k| item.get(*k).and_then(Value::as_f64));
            if let Some(p) = pct {
                windows.push(Window {
                    key: format!("product:{name}"),
                    label: name.to_string(),
                    short: name.trim_start_matches("Grok").to_string(),
                    utilization: p,
                    resets_at,
                });
            }
        }
        Usage {
            windows,
            absent: vec![],
            extra: None,
        }
    }

    /// grok's own status line: "Weekly limit left: 37%" (or Monthly, Daily).
    pub fn status_line_left(screen: &str) -> Option<(String, f64)> {
        let i = screen.find(" limit left:")?;
        let label = screen[..i]
            .rsplit(|c: char| !c.is_alphabetic())
            .next()
            .filter(|l| !l.is_empty())
            .unwrap_or("Weekly")
            .to_string();
        let rest = screen[i + " limit left:".len()..].trim_start();
        let num: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let left: f64 = num.parse().ok()?;
        rest[num.len()..]
            .starts_with('%')
            .then_some((label, left.clamp(0.0, 100.0)))
    }

    /// Live credits for a slot (one GET). Every attempt is logged (host,
    /// path, status, what came back), never the token.
    pub fn fetch_usage(home: &Path) -> Result<Usage, UsageError> {
        let name = home
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(tok) = token(home) else {
            crate::log::info(&format!("grok usage {name}: skipped, no key in auth.json"));
            return Err(UsageError::NotLoggedIn);
        };
        let resp = ureq::get(BILLING_URL)
            .header("Authorization", &format!("Bearer {tok}"))
            .header("X-XAI-Token-Auth", "xai-grok-cli")
            .header("Accept", "application/json")
            .call();
        let res = match resp {
            Ok(mut r) => {
                let v: Value = r
                    .body_mut()
                    .read_json()
                    .map_err(|e| UsageError::Parse(format!("bad billing reply: {e}")))?;
                let u = parse_billing(&v);
                if u.windows.is_empty() {
                    Err(UsageError::Parse("billing reply had no credits".into()))
                } else {
                    Ok(u)
                }
            }
            Err(ureq::Error::StatusCode(401 | 403)) => Err(UsageError::Unauthorized),
            Err(ureq::Error::StatusCode(429)) => Err(UsageError::RateLimited(None)),
            Err(ureq::Error::StatusCode(c)) => Err(UsageError::Http(c)),
            Err(e) => Err(UsageError::Network(format!("billing: {e}"))),
        };
        match &res {
            Ok(u) => crate::log::info(&format!(
                "grok usage {name}: GET cli-chat-proxy.grok.com/v1/billing 200, {}",
                u.windows
                    .iter()
                    .map(|w| format!("{} {:.0}% left", w.label, w.left()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(e) => crate::log::info(&format!(
                "grok usage {name}: GET cli-chat-proxy.grok.com/v1/billing failed: {}",
                e.short()
            )),
        }
        res
    }

    /// Sessions of a grok home: `sessions/<cwd>/<id>/summary.json` (+
    /// usage.json for tokens and cost). The session folder is the unit
    /// copied between slots.
    pub fn scan_sessions(home: &Path) -> Vec<SessionInfo> {
        let mut out = vec![];
        let Ok(dirs) = std::fs::read_dir(home.join("sessions")) else {
            return out;
        };
        for d in dirs.flatten() {
            let Ok(sessions) = std::fs::read_dir(d.path()) else {
                continue;
            };
            for s in sessions.flatten() {
                let p = s.path();
                if let Some(info) = parse_session(&p) {
                    out.push(info);
                }
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.modified));
        out
    }

    /// Copy (or move, into `trash`) a session folder from one grok home to
    /// another, at the same relative place (`sessions/<cwd>/<id>`).
    pub fn copy_session(
        dir: &Path,
        from_home: &Path,
        to_home: &Path,
        mv: bool,
        trash: &Path,
    ) -> anyhow::Result<(
        crate::session_ops::Copied,
        Option<crate::session_ops::Moved>,
    )> {
        use anyhow::Context;
        let rel = dir
            .strip_prefix(from_home)
            .context("session outside its home")?;
        let dst = to_home.join(rel);
        let id = dir
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut copied = crate::session_ops::Copied {
            id: id.clone(),
            files: 0,
            bytes: 0,
            skipped: false,
            jsonl: dst.join("summary.json"),
        };
        if dst.exists() {
            copied.skipped = true;
            return Ok((copied, None));
        }
        fn cp(a: &Path, b: &Path, n: &mut usize, bytes: &mut u64) -> std::io::Result<()> {
            std::fs::create_dir_all(b)?;
            for e in std::fs::read_dir(a)? {
                let e = e?;
                let t = b.join(e.file_name());
                if e.file_type()?.is_dir() {
                    cp(&e.path(), &t, n, bytes)?;
                } else {
                    *bytes += std::fs::copy(e.path(), &t)?;
                    *n += 1;
                }
            }
            Ok(())
        }
        cp(dir, &dst, &mut copied.files, &mut copied.bytes)?;
        if !mv {
            return Ok((copied, None));
        }
        let to = trash.join(format!(
            "{}-{id}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        std::fs::create_dir_all(trash)?;
        std::fs::rename(dir, &to)?;
        Ok((
            copied.clone(),
            Some(crate::session_ops::Moved {
                copied,
                trashed: vec![(dir.to_path_buf(), to)],
                dst_dir: dst,
            }),
        ))
    }

    pub fn parse_session(dir: &Path) -> Option<SessionInfo> {
        parse_session_any(dir, false)
    }

    pub fn parse_session_any(dir: &Path, include_subagents: bool) -> Option<SessionInfo> {
        let summary: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).ok()?).ok()?;
        // Subagent sessions are not resumable on their own.
        let kind_is_sub = summary
            .get("session_kind")
            .and_then(Value::as_str)
            .is_some_and(|k| k.starts_with("subagent"));
        if kind_is_sub && !include_subagents {
            return None;
        }
        let id = summary
            .pointer("/info/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| dir.file_name().map(|f| f.to_string_lossy().into_owned()))?;
        let messages = summary
            .get("num_chat_messages")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let mut tokens = Tokens::default();
        let mut cost = None;
        if let Ok(t) = std::fs::read_to_string(dir.join("usage.json")) {
            if let Ok(u) = serde_json::from_str::<Value>(&t) {
                let g = |k: &str| {
                    u.pointer(&format!("/session/{k}"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                };
                tokens.input = g("inputTokens");
                tokens.output = g("outputTokens");
                tokens.cache_read = g("cachedReadTokens");
                tokens.cache_creation = g("cacheCreationTokens");
                cost = u
                    .pointer("/session/costUsdTicks")
                    .and_then(Value::as_u64)
                    .map(|t| t as f64 / 1e10);
            }
        }
        let modified = std::fs::metadata(dir.join("summary.json"))
            .and_then(|m| m.modified())
            .ok();
        Some(SessionInfo {
            id,
            path: dir.to_path_buf(),
            cwd: summary
                .pointer("/info/cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            modified,
            messages,
            first_prompt: None,
            title: summary
                .get("generated_title")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .or_else(|| summary.get("session_summary").and_then(Value::as_str))
                .map(|t| match cost {
                    Some(c) if c > 0.0 => format!("{t} (${c:.2})"),
                    _ => t.to_string(),
                }),
            model: summary
                .get("current_model_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            tokens,
            last_prompt: summary
                .get("session_summary")
                .and_then(Value::as_str)
                .map(|t| crate::sessions::snippet(t, 160)),
            subagents: 0,
            parent: kind_is_sub.then(|| "?".to_string()),
            approx: false,
            created: summary
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(std::time::SystemTime::from),
            headless: summary.get("session_kind").and_then(Value::as_str) == Some("headless"),
            name: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn launch_flags() {
        assert_eq!(Harness::of("Grok"), Harness::Grok);
        assert_eq!(Harness::of("anything"), Harness::Claude);
        assert_eq!(
            Harness::Grok.permission_args("bypass"),
            vec!["--always-approve"]
        );
        assert_eq!(
            Harness::Grok.permission_args("accept-edits"),
            vec!["--permission-mode", "acceptEdits"]
        );
        assert_eq!(
            Harness::Grok.permission_args("manual"),
            vec!["--permission-mode", "default"]
        );
        assert!(Harness::Grok.permission_args("default").is_empty());
        assert_eq!(Harness::Grok.home_env(), "GROK_HOME");
        assert_eq!(Harness::Claude.home_env(), "CLAUDE_CONFIG_DIR");
        assert_eq!(Harness::Grok.resume_args("abc"), vec!["--resume", "abc"]);
        assert_eq!(
            Harness::Grok.base_args(Path::new("/s")),
            vec![
                "--leader-socket".to_string(),
                Path::new("/s")
                    .join("leader.sock")
                    .to_string_lossy()
                    .into_owned()
            ]
        );
        assert_eq!(Harness::Grok.login_args(), Some(vec!["login".to_string()]));
        assert!(Harness::Claude.badge().is_none());
        assert!(
            scrub_grok("GROK_AGENT_ID")
                && scrub_grok("GROK_SESSION_ID")
                && scrub_grok("GROK_HOME")
                && !scrub_grok("GROK_SANDBOX")
        );
    }

    #[test]
    fn slot_compat_config() {
        let home = std::env::temp_dir().join(format!("gt-grok-compat-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), "# mine\n[ui]\nyolo = false\n").unwrap();
        grok::write_slot_compat(&home, &crate::config::GrokCompat::default());
        let t = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(
            t.starts_with("# mine")
                && t.contains("[compat.claude]")
                && t.contains("hooks = false")
                && t.contains("skills = true")
                && t.contains("mcps = false"),
            "{t}"
        );
        assert!(crate::config::GrokCompat::default()
            .env()
            .contains(&("GROK_CLAUDE_HOOKS_ENABLED", "false")));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn grok_screens() {
        use crate::pane::{detect_activity, Activity};
        use crate::prompt::{parse_prompt, Choice, PromptKind};
        let perm = include_str!("../tests/fixtures/screens/grok_permission.txt");
        assert_eq!(detect_activity(perm), Activity::Permission);
        let p = parse_prompt(perm).expect("grok prompt");
        assert_eq!(p.kind, PromptKind::Permission);
        assert_eq!(p.options.len(), 4);
        let label = |c| p.index_for(c).map(|i| p.options[i].label.clone());
        assert_eq!(label(Choice::Approve).as_deref(), Some("Allow once"));
        assert_eq!(
            label(Choice::Always).as_deref(),
            Some("Always allow this command")
        );
        assert_eq!(label(Choice::Deny).as_deref(), Some("Reject"));
        // Unnumbered: arrows from the highlighted one, then Enter.
        assert_eq!(p.keys_for(3, false), b"\x1b[B\x1b[B\x1b[B\r".to_vec());
        assert_eq!(
            detect_activity(include_str!("../tests/fixtures/screens/grok_working.txt")),
            Activity::Working
        );
        assert_eq!(
            detect_activity(include_str!("../tests/fixtures/screens/grok_ready.txt")),
            Activity::Ready
        );
    }

    /// A reply with nothing used leaves the zero values out (proto3): it
    /// reads as 100% left, not as a bad response.
    #[test]
    fn billing_reply_with_nothing_used() {
        let v: Value = serde_json::from_str(
            r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-10-07T05:25:41Z","end":"2026-10-14T05:25:41Z"},"onDemandCap":{"val":0},"isUnifiedBillingUser":true}}"#,
        )
        .unwrap();
        let u = grok::parse_billing(&v);
        assert_eq!(u.windows.len(), 1);
        assert_eq!(
            (u.windows[0].key.as_str(), u.windows[0].left()),
            ("seven_day", 100.0)
        );
    }

    #[test]
    fn billing_reply_as_grok_sends_it() {
        // The shape grok 1.0.45 gets back (sanitized).
        let v: Value =
            serde_json::from_str(include_str!("../tests/fixtures/grok_billing.json")).unwrap();
        let u = grok::parse_billing(&v);
        let w = u.get("seven_day").unwrap();
        assert_eq!((w.label.as_str(), w.left()), ("Weekly", 0.0));
        assert_eq!(
            w.resets_at.unwrap().to_rfc3339(),
            "2099-10-09T14:16:44.739224+00:00"
        );
        assert_eq!(u.get("product:GrokBuild").unwrap().left(), 74.0);
        assert!(u.get("product:GrokChat").is_none(), "no percent, no bucket");
        assert_eq!(
            grok::status_line_left(" Weekly limit left: 0% · Grok 4.7 (high) · always-approve"),
            Some(("Weekly".into(), 0.0))
        );
        assert_eq!(
            grok::status_line_left("  Monthly limit left: 37% ·"),
            Some(("Monthly".into(), 37.0))
        );
        assert_eq!(grok::status_line_left("no limits here"), None);
    }

    #[test]
    fn billing_reply() {
        let v = json!({
            "creditUsagePercent": 87.0,
            "currentPeriod": {"start": "2026-10-02T14:16:00Z", "end": "2026-10-09T14:16:00Z"},
            "productUsage": [{"product": "GrokBuild", "usagePercent": 80.0}, {"product": "GrokChat", "usagePercent": 7.5}],
            "prepaidBalance": 0, "onDemandUsed": 0
        });
        let u = grok::parse_billing(&v);
        let w = u.get("seven_day").unwrap();
        assert_eq!((w.left(), w.label.as_str()), (13.0, "Weekly"));
        assert_eq!(
            w.resets_at.unwrap().to_rfc3339(),
            "2026-10-09T14:16:00+00:00"
        );
        assert_eq!(u.others().count(), 2);
        assert_eq!(u.get("product:GrokBuild").unwrap().short, "Build");
        assert!(grok::parse_billing(&json!({})).windows.is_empty());
    }

    #[test]
    fn auth_and_sessions() {
        let home = std::env::temp_dir().join(format!("gt-grok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        assert!(!grok::logged_in(&home));
        std::fs::write(
            home.join("auth.json"),
            r#"{"user":{"email":"x"},"key":"secret-token"}"#,
        )
        .unwrap();
        assert!(grok::logged_in(&home));
        assert_eq!(grok::token(&home).as_deref(), Some("secret-token"));
        // A session as grok 1.0.45 writes it (synthesized).
        let s = home.join("sessions/%2FUsers%2Fx%2Fproj/0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("summary.json"), json!({"info": {"id": "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee", "cwd": "/Users/x/proj"}, "num_chat_messages": 12, "generated_title": "Fix login flow", "current_model_id": "grok-4.7-build-fast", "session_kind": null}).to_string()).unwrap();
        std::fs::write(s.join("usage.json"), json!({"session": {"inputTokens": 1000, "outputTokens": 200, "cachedReadTokens": 50, "cacheCreationTokens": 0, "costUsdTicks": 123_000_000_000u64}}).to_string()).unwrap();
        let sub = home.join("sessions/%2FUsers%2Fx%2Fproj/sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("summary.json"),
            json!({"info": {"id": "sub", "cwd": "/x"}, "session_kind": "subagent"}).to_string(),
        )
        .unwrap();
        let v = grok::scan_sessions(&home);
        assert_eq!(v.len(), 1, "subagents are left out");
        // Copy to another grok home at the same place, then move back.
        let other = home.join("other");
        let trash = home.join("trash");
        let (c, m) = grok::copy_session(&s, &home, &other, false, &trash).unwrap();
        assert!(!c.skipped && m.is_none() && other.join("sessions/%2FUsers%2Fx%2Fproj/0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee/usage.json").is_file());
        assert!(
            grok::copy_session(&s, &home, &other, false, &trash)
                .unwrap()
                .0
                .skipped,
            "already there"
        );
        let copy = other.join("sessions/%2FUsers%2Fx%2Fproj/0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee");
        let third = home.join("third");
        let (_, m) = grok::copy_session(&copy, &other, &third, true, &trash).unwrap();
        assert!(m.is_some() && !copy.exists() && third.join("sessions/%2FUsers%2Fx%2Fproj/0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee/summary.json").is_file());
        let x = &v[0];
        assert_eq!(
            (x.cwd.as_str(), x.messages, x.tokens.input),
            ("/Users/x/proj", 12, 1000)
        );
        assert_eq!(x.title.as_deref(), Some("Fix login flow ($12.30)"));
        assert_eq!(x.model.as_deref(), Some("grok-4.7-build-fast"));
        let _ = std::fs::remove_dir_all(home);
    }
}
