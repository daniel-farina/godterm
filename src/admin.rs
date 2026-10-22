//! Account administration for the assistant, the pure part: the catalog
//! of well known MCP servers, what each account has (read from its config
//! dir, never its credentials), the exact commands an install runs, and
//! parsing what `claude mcp list` / `grok mcp list` print.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::harness::Harness;

// ---------- the catalog ----------

const CATALOG: &str = include_str!("../data/mcp_catalog.json");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OAuthClient {
    pub client_id: String,
    pub callback_port: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// A Claude Code plugin (`name@marketplace`) that brings the server.
    #[serde(default)]
    pub plugin: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// http, sse, stdio or connector.
    pub transport: String,
    /// oauth, token or none.
    pub auth: String,
    #[serde(default)]
    pub oauth: Option<OAuthClient>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub connector_url: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

pub fn catalog() -> Vec<Entry> {
    serde_json::from_str::<Value>(CATALOG)
        .ok()
        .and_then(|v| serde_json::from_value(v["entries"].clone()).ok())
        .unwrap_or_default()
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// A catalog entry by id, name or alias ("Slack", "slack connector", "jira").
pub fn find(q: &str) -> Option<Entry> {
    let n = norm(q.trim_end_matches(" mcp").trim_end_matches(" connector"));
    if n.is_empty() {
        return None;
    }
    catalog().into_iter().find(|e| {
        norm(&e.id) == n
            || norm(&e.name) == n
            || e.aliases.iter().any(|a| norm(a) == n)
            || norm(&e.name).starts_with(&n) && n.len() >= 4
    })
}

// ---------- what an account has ----------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Server {
    pub name: String,
    /// http, sse, stdio, ws, or "connector" (claude.ai).
    pub transport: String,
    /// The URL without its query, or the command.
    pub target: String,
    /// user, local, project, plugin, claude.ai
    pub scope: String,
    /// connected, needs-auth, failed, pending, disabled or unknown.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plugin {
    pub id: String,
    pub enabled: bool,
    /// MCP servers it brings (by name).
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Caps {
    pub servers: Vec<Server>,
    pub plugins: Vec<Plugin>,
    pub marketplaces: Vec<String>,
    pub skills: Vec<String>,
    /// Hook events configured (PreToolUse, Stop, ...).
    pub hooks: Vec<String>,
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// A URL shown or logged: no query (it can carry tokens) and no userinfo.
pub fn safe_target(t: &str) -> String {
    let t = t.split(['?', '#']).next().unwrap_or("").to_string();
    match t.split_once("://") {
        Some((s, rest)) => match rest.split_once('@') {
            Some((_, host)) if !host.contains('/') || rest.find('@') < rest.find('/') => {
                format!("{s}://{host}")
            }
            _ => t,
        },
        None => t,
    }
}

fn server_of(name: &str, v: &Value, scope: &str) -> Server {
    let transport = v["type"].as_str().unwrap_or(if v.get("url").is_some() {
        "http"
    } else {
        "stdio"
    });
    let target = match v["url"].as_str() {
        Some(u) => safe_target(u),
        None => {
            let mut c = vec![v["command"].as_str().unwrap_or("").to_string()];
            if let Some(a) = v["args"].as_array() {
                c.extend(a.iter().filter_map(|x| x.as_str().map(str::to_string)));
            }
            c.join(" ").trim().to_string()
        }
    };
    Server {
        name: name.to_string(),
        transport: transport.to_string(),
        target,
        scope: scope.to_string(),
        status: "unknown".into(),
    }
}

/// What a Claude Code config dir has: user and local MCP servers, plugins
/// (and the servers they bring), marketplaces, skills and hooks. Only
/// these keys are read; credentials, headers and env values never are.
pub fn claude_caps(dir: &Path) -> Caps {
    let mut c = Caps::default();
    if let Some(cj) = read_json(&dir.join(".claude.json")) {
        if let Some(m) = cj["mcpServers"].as_object() {
            for (n, v) in m {
                c.servers.push(server_of(n, v, "user"));
            }
        }
        if let Some(ps) = cj["projects"].as_object() {
            for p in ps.values() {
                if let Some(m) = p["mcpServers"].as_object() {
                    for (n, v) in m {
                        if !c.servers.iter().any(|s| s.name == *n) {
                            c.servers.push(server_of(n, v, "local"));
                        }
                    }
                }
            }
        }
    }
    let settings = read_json(&dir.join("settings.json")).unwrap_or(Value::Null);
    let enabled = settings["enabledPlugins"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(h) = settings["hooks"].as_object() {
        c.hooks = h.keys().cloned().collect();
    }
    if let Some(ip) = read_json(&dir.join("plugins/installed_plugins.json")) {
        if let Some(m) = ip["plugins"].as_object() {
            for (id, installs) in m {
                let path = installs
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|i| i["installPath"].as_str())
                    .map(PathBuf::from);
                let mut servers = vec![];
                if let Some(mj) = path.and_then(|p| read_json(&p.join(".mcp.json"))) {
                    let m = mj["mcpServers"].as_object().or(mj.as_object()).cloned();
                    let short = id.split('@').next().unwrap_or(id);
                    for (n, v) in m.unwrap_or_default() {
                        if v.is_object() {
                            let full = format!("plugin:{short}:{n}");
                            let mut s = server_of(&full, &v, "plugin");
                            s.scope = "plugin".into();
                            c.servers.push(s);
                            servers.push(full);
                        }
                    }
                }
                c.plugins.push(Plugin {
                    id: id.clone(),
                    enabled: enabled.get(id).and_then(Value::as_bool).unwrap_or(false),
                    servers,
                });
            }
        }
    }
    if let Some(km) = read_json(&dir.join("plugins/known_marketplaces.json")) {
        if let Some(m) = km.as_object() {
            c.marketplaces = m.keys().cloned().collect();
        }
    }
    c.skills = list_dirs(&dir.join("skills"));
    c
}

/// What a Grok home has: `[mcp_servers]`, plugins, skills.
pub fn grok_caps(dir: &Path) -> Caps {
    let mut c = Caps::default();
    let text = std::fs::read_to_string(dir.join("config.toml")).unwrap_or_default();
    let t: toml::Value = text
        .parse()
        .unwrap_or(toml::Value::Table(Default::default()));
    let disabled: Vec<String> = t
        .get("disabled_mcp_servers")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(m) = t.get("mcp_servers").and_then(|v| v.as_table()) {
        for (n, v) in m {
            let j = serde_json::to_value(v).unwrap_or(Value::Null);
            let mut s = server_of(n, &j, "user");
            if j["enabled"].as_bool() == Some(false) || disabled.contains(n) {
                s.status = "disabled".into();
            }
            c.servers.push(s);
        }
    }
    let enabled: Vec<String> = t
        .get("plugins")
        .and_then(|p| p.get("enabled"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for p in list_dirs(&dir.join("installed-plugins")) {
        c.plugins.push(Plugin {
            enabled: enabled
                .iter()
                .any(|e| e == &p || e.ends_with(&format!("/{p}"))),
            id: p,
            servers: vec![],
        });
    }
    c.skills = list_dirs(&dir.join("skills"));
    c
}

fn list_dirs(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn caps(h: Harness, dir: &Path) -> Caps {
    match h {
        Harness::Claude => claude_caps(dir),
        Harness::Grok => grok_caps(dir),
        _ => Caps::default(),
    }
}

// ---------- mcp list ----------

/// `claude mcp list` lines: "name: target (HTTP) - ✓ Connected",
/// "plugin:slack:slack: https://mcp.slack.com/mcp (HTTP) - ! Needs
/// authentication", "claude.ai Gmail: https://... - ✓ Connected".
pub fn parse_mcp_list(out: &str) -> Vec<Server> {
    let mut v = vec![];
    for l in out.lines() {
        let l = l.trim();
        let Some((left, status)) = l.rsplit_once(" - ") else {
            continue;
        };
        let Some((name, target)) = left.split_once(": ") else {
            continue;
        };
        if name.is_empty() || name.contains("  ") {
            continue;
        }
        let s = status.to_lowercase();
        let status =
            if s.contains("connected") && !s.contains("not connected") && !s.contains("failed") {
                "connected"
            } else if s.contains("auth") {
                "needs-auth"
            } else if s.contains("pending") {
                "pending"
            } else if s.contains("fail") || s.contains("error") {
                "failed"
            } else {
                "unknown"
            };
        let (target, transport) = match target.rsplit_once(" (") {
            Some((t, k)) => (t.to_string(), k.trim_end_matches(')').to_lowercase()),
            None => (
                target.to_string(),
                if target.starts_with("http") {
                    "http".into()
                } else {
                    "stdio".into()
                },
            ),
        };
        let (scope, transport) = if name.starts_with("claude.ai ") {
            ("claude.ai", "connector".to_string())
        } else if name.starts_with("plugin:") {
            ("plugin", transport)
        } else {
            ("", transport)
        };
        v.push(Server {
            name: name.to_string(),
            transport,
            target: safe_target(&target),
            scope: scope.to_string(),
            status: status.to_string(),
        });
    }
    v
}

/// `grok mcp list --json`: an array (or {servers: [...]}) of objects with
/// name, url/command, enabled, and a status when it has one.
pub fn parse_grok_list(out: &str) -> Vec<Server> {
    let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
    let arr = v
        .as_array()
        .cloned()
        .or_else(|| v["servers"].as_array().cloned())
        .unwrap_or_default();
    arr.iter()
        .filter_map(|s| {
            let name = s["name"].as_str()?;
            let mut x = server_of(name, s, "user");
            x.status = match (s["enabled"].as_bool(), s["status"].as_str()) {
                (Some(false), _) => "disabled".into(),
                (_, Some(st)) => {
                    let st = st.to_lowercase();
                    if st.contains("auth") {
                        "needs-auth".into()
                    } else if st.contains("connect") || st == "ok" {
                        "connected".into()
                    } else if st.contains("fail") || st.contains("error") {
                        "failed".into()
                    } else {
                        st
                    }
                }
                _ => "unknown".into(),
            };
            Some(x)
        })
        .collect()
}

// ---------- commands ----------

/// What an install runs on one account: argv after the program.
#[derive(Debug, Clone, PartialEq)]
pub struct Install {
    /// The server name to check afterwards ("linear", "plugin:slack:slack").
    pub check: String,
    pub steps: Vec<Vec<String>>,
    pub auth: String,
}

/// The source of an install, as the model gives it.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Catalog(Entry),
    Url { url: String, transport: String },
    Command(Vec<String>),
}

pub const OFFICIAL_MARKETPLACE: &str = "claude-plugins-official";
pub const OFFICIAL_MARKETPLACE_REPO: &str = "anthropics/claude-plugins-official";

/// The steps for `name` from `src` on a harness. `via_plugin`: a catalog
/// entry with a plugin installs that (Claude only); `has_market`: the
/// official marketplace is already known to the account.
pub fn install_steps(
    h: Harness,
    name: &str,
    src: &Source,
    via_plugin: bool,
    has_market: bool,
    env: &[(String, String)],
) -> Result<Install, String> {
    if !h.integrated() {
        return Err(format!(
            "Manage {} MCP servers in its CLI; no GodTerm admin adapter yet",
            h.label()
        ));
    }
    let s = |x: &str| x.to_string();
    let env_flags: Vec<String> = env
        .iter()
        .flat_map(|(k, v)| vec![s("-e"), format!("{k}={v}")])
        .collect();
    match (h, src) {
        (_, Source::Catalog(e)) if e.transport == "connector" => Err(format!(
            "{} is a claude.ai connector: it is turned on at {} while signed in to that account, not installed here",
            e.name,
            e.connector_url.clone().unwrap_or_else(|| "claude.ai/settings/connectors".into())
        )),
        (Harness::Claude, Source::Catalog(e)) if via_plugin && e.plugin.is_some() => {
            let plugin = e.plugin.clone().unwrap_or_default();
            let short = plugin.split('@').next().unwrap_or(&plugin).to_string();
            let mut steps = vec![];
            if plugin.ends_with(&format!("@{OFFICIAL_MARKETPLACE}")) && !has_market {
                steps.push(vec![s("plugin"), s("marketplace"), s("add"), s(OFFICIAL_MARKETPLACE_REPO)]);
            }
            steps.push(vec![s("plugin"), s("install"), plugin.clone()]);
            Ok(Install {
                check: format!("plugin:{short}:{short}"),
                steps,
                auth: e.auth.clone(),
            })
        }
        (_, Source::Catalog(e)) => {
            let inner = match (&e.url, &e.command) {
                (Some(u), _) => Source::Url {
                    url: u.clone(),
                    transport: e.transport.clone(),
                },
                (None, Some(c)) => Source::Command(c.clone()),
                _ => return Err(format!("{} has no URL or command", e.name)),
            };
            let mut i = install_steps(h, name, &inner, false, has_market, env)?;
            if let (Harness::Claude, Some(o)) = (h, &e.oauth) {
                // The provider's registered OAuth client (Slack needs it).
                let st = &mut i.steps[0];
                let at = st.iter().position(|x| x == name).unwrap_or(st.len());
                st.splice(
                    at..at,
                    [s("--client-id"), o.client_id.clone(), s("--callback-port"), o.callback_port.to_string()],
                );
            }
            i.auth = e.auth.clone();
            Ok(i)
        }
        (_, Source::Url { url, transport }) => {
            if !url.starts_with("https://") && !url.starts_with("http://localhost") && !url.starts_with("http://127.0.0.1") {
                return Err("a remote MCP server URL must be https".into());
            }
            let t = if transport == "sse" { "sse" } else { "http" };
            let mut a = vec![s("mcp"), s("add"), s("--transport"), s(t), s("--scope"), s("user")];
            a.extend(env_flags);
            a.push(name.to_string());
            a.push(url.clone());
            Ok(Install {
                check: name.to_string(),
                steps: vec![a],
                auth: "oauth".into(),
            })
        }
        (_, Source::Command(cmd)) => {
            if cmd.is_empty() {
                return Err("no command".into());
            }
            let mut a = vec![s("mcp"), s("add"), s("--scope"), s("user")];
            a.extend(env_flags);
            a.push(name.to_string());
            a.push(s("--"));
            a.extend(cmd.iter().cloned());
            Ok(Install {
                check: name.to_string(),
                steps: vec![a],
                auth: "none".into(),
            })
        }
    }
}

/// `mcp remove`: user scope, where installs go.
pub fn remove_steps(h: Harness, name: &str) -> Vec<Vec<String>> {
    match h {
        Harness::Claude => vec![vec![
            "mcp".into(),
            "remove".into(),
            "--scope".into(),
            "user".into(),
            name.into(),
        ]],
        Harness::Grok => vec![vec!["mcp".into(), "remove".into(), name.into()]],
        _ => vec![],
    }
}

/// `plugin install|uninstall|enable|disable`.
pub fn plugin_steps(
    h: Harness,
    action: &str,
    id: &str,
    marketplace: Option<&str>,
) -> Vec<Vec<String>> {
    if !h.integrated() {
        return vec![];
    }
    let s = |x: &str| x.to_string();
    let mut v = vec![];
    if let Some(m) = marketplace {
        v.push(vec![s("plugin"), s("marketplace"), s("add"), s(m)]);
    }
    let mut a = vec![s("plugin"), s(action), s(id)];
    match (h, action) {
        (Harness::Grok, "install") => a.push(s("--trust")),
        (Harness::Grok, "uninstall") => a.push(s("--confirm")),
        _ => {}
    }
    v.push(a);
    v
}

pub fn list_args(h: Harness) -> Vec<String> {
    match h {
        Harness::Claude => vec!["mcp".into(), "list".into()],
        Harness::Grok => vec!["mcp".into(), "list".into(), "--json".into()],
        _ => vec![],
    }
}

/// One command as it reads in a confirmation: "claude mcp add ...".
pub fn shown(program: &str, argv: &[String]) -> String {
    let p = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| program.to_string());
    let p = p.trim_end_matches(".exe").to_string();
    let args: Vec<String> = argv
        .iter()
        .map(|a| {
            // Values of -e KEY=VALUE are masked.
            let a = match a.split_once('=') {
                Some((k, _))
                    if k.chars().all(|c| c.is_ascii_uppercase() || c == '_') && !k.is_empty() =>
                {
                    format!("{k}=***")
                }
                _ => a.clone(),
            };
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a
            }
        })
        .collect();
    format!("{p} {}", args.join(" "))
}

/// Run `program argv` for an account: its home in the harness's env var,
/// the environment scrubbed as for tabs. (exit ok, stdout + stderr)
pub fn run(
    program: &str,
    argv: &[String],
    h: Harness,
    home: &Path,
    pass_env: &[String],
    limit: Duration,
) -> (bool, String) {
    let mut c = std::process::Command::new(program);
    c.args(argv)
        .env(h.home_env(), home)
        .stdin(std::process::Stdio::null());
    for (k, _) in std::env::vars() {
        if crate::pane::should_scrub(&k, pass_env) {
            c.env_remove(&k);
        }
    }
    match crate::procs::output_timeout(&mut c, limit) {
        Ok(o) => {
            let mut t = String::from_utf8_lossy(&o.stdout).to_string();
            let e = String::from_utf8_lossy(&o.stderr);
            if !e.trim().is_empty() {
                t.push('\n');
                t.push_str(e.trim());
            }
            (o.status.success(), t)
        }
        Err(e) => (false, e),
    }
}

/// The first https URL in some text (a login or OAuth page).
pub fn first_url(text: &str) -> Option<String> {
    let i = text.find("https://")?;
    let u: String = text[i..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | ')'))
        .collect();
    (u.len() > 10).then_some(u)
}

/// A login URL on a terminal screen: lines are joined first, since a long
/// URL wraps across them.
pub fn url_on_screen(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    for (i, l) in lines.iter().enumerate() {
        if let Some(at) = l.find("https://") {
            let mut u = l[at..].trim().to_string();
            // Continue while the next lines are unbroken URL text.
            for next in lines.iter().skip(i + 1) {
                let n = next.trim();
                if n.is_empty() || n.contains(' ') || n.len() < 8 && !u.ends_with('=') {
                    break;
                }
                u.push_str(n);
            }
            return first_url(&u);
        }
    }
    None
}

/// Open a URL in the browser (tests record it instead).
pub fn open_url(url: &str) {
    #[cfg(test)]
    {
        OPENED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(url.to_string());
    }
    if cfg!(test) || std::env::var_os("GODTERM_NO_OPEN").is_some() {
        return;
    }
    let mut c = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let _ = c
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
pub static OPENED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The clipboard, read only for a login code on the user's command.
pub fn read_clipboard() -> Option<String> {
    #[cfg(test)]
    {
        return TEST_CLIPBOARD
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
    }
    #[cfg(not(test))]
    {
        let mut c = if cfg!(target_os = "macos") {
            std::process::Command::new("pbpaste")
        } else if cfg!(windows) {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoProfile", "-Command", "Get-Clipboard"]);
            c
        } else {
            let mut c = std::process::Command::new("xclip");
            c.args(["-selection", "clipboard", "-o"]);
            c
        };
        let o = crate::procs::output_timeout(&mut c, Duration::from_secs(3)).ok()?;
        let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!t.is_empty()).then_some(t)
    }
}

#[cfg(test)]
pub static TEST_CLIPBOARD: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// A short summary of an account's capabilities for the state block.
pub fn summary(c: &Caps) -> String {
    let mut parts = vec![];
    let servers: Vec<String> = c
        .servers
        .iter()
        .map(|s| {
            let n = s.name.trim_start_matches("plugin:");
            let n = n.split(':').next_back().unwrap_or(n);
            match s.status.as_str() {
                "unknown" => n.to_string(),
                st => format!("{n} ({st})"),
            }
        })
        .collect();
    if !servers.is_empty() {
        parts.push(format!("mcp {}", servers.join(", ")));
    }
    let plugins: Vec<String> = c
        .plugins
        .iter()
        .map(|p| {
            let n = p.id.split('@').next().unwrap_or(&p.id).to_string();
            if p.enabled {
                n
            } else {
                format!("{n} (off)")
            }
        })
        .collect();
    if !plugins.is_empty() {
        parts.push(format!("plugins {}", plugins.join(", ")));
    }
    if !c.skills.is_empty() {
        parts.push(format!("{} skills", c.skills.len()));
    }
    if parts.is_empty() {
        "no MCP servers or plugins".into()
    } else {
        parts.join("; ")
    }
}

pub fn caps_json(c: &Caps) -> Value {
    json!({
        "mcp_servers": c.servers,
        "plugins": c.plugins,
        "marketplaces": c.marketplaces,
        "skills": c.skills,
        "hooks": c.hooks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_finds_by_name_and_alias() {
        assert!(catalog().len() >= 15);
        assert_eq!(find("Slack").unwrap().id, "slack");
        assert_eq!(find("the slack connector").map(|e| e.id), None);
        assert_eq!(find("slack connector").unwrap().id, "slack");
        assert_eq!(find("Jira").unwrap().id, "atlassian");
        assert_eq!(find("google drive").unwrap().transport, "connector");
        assert!(find("kubernetes").is_none());
        for e in catalog() {
            assert!(
                e.url.is_some()
                    || e.command.is_some()
                    || e.plugin.is_some()
                    || e.transport == "connector",
                "{}",
                e.id
            );
            if let Some(u) = &e.url {
                assert!(u.starts_with("https://"), "{u}");
            }
        }
    }

    #[test]
    fn parses_mcp_list_lines() {
        let out = "Checking MCP server health...\n\nlinear: https://mcp.linear.app/mcp (HTTP) - ✓ Connected\nplugin:slack:slack: https://mcp.slack.com/mcp (HTTP) - ! Needs authentication\nclaude.ai Gmail: https://gmail.mcp.claude.com/mcp - ✓ Connected\nplaywright: npx @playwright/mcp@latest - ✗ Failed to connect\n";
        let v = parse_mcp_list(out);
        assert_eq!(v.len(), 4, "{v:?}");
        assert_eq!(
            (
                v[0].name.as_str(),
                v[0].status.as_str(),
                v[0].transport.as_str()
            ),
            ("linear", "connected", "http")
        );
        assert_eq!(
            (
                v[1].name.as_str(),
                v[1].status.as_str(),
                v[1].scope.as_str()
            ),
            ("plugin:slack:slack", "needs-auth", "plugin")
        );
        assert_eq!(
            (v[2].scope.as_str(), v[2].transport.as_str()),
            ("claude.ai", "connector")
        );
        assert_eq!(v[3].status, "failed");
    }

    #[test]
    fn install_commands() {
        let slack = find("slack").unwrap();
        let i = install_steps(
            Harness::Claude,
            "slack",
            &Source::Catalog(slack.clone()),
            true,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(i.steps.len(), 2);
        assert_eq!(
            shown("/x/claude", &i.steps[1]),
            "claude plugin install slack@claude-plugins-official"
        );
        assert_eq!(i.check, "plugin:slack:slack");
        let d = install_steps(
            Harness::Claude,
            "slack",
            &Source::Catalog(slack),
            false,
            true,
            &[],
        )
        .unwrap();
        assert_eq!(
            shown("claude", &d.steps[0]),
            "claude mcp add --transport http --scope user --client-id 1601185624273.8899143856786 --callback-port 3118 slack https://mcp.slack.com/mcp"
        );
        let u = install_steps(
            Harness::Grok,
            "x",
            &Source::Url {
                url: "https://mcp.example.com/mcp".into(),
                transport: "sse".into(),
            },
            false,
            false,
            &[("API_KEY".into(), "secret".into())],
        )
        .unwrap();
        assert_eq!(shown("grok", &u.steps[0]), "grok mcp add --transport sse --scope user -e API_KEY=*** x https://mcp.example.com/mcp");
        assert!(install_steps(
            Harness::Claude,
            "x",
            &Source::Url {
                url: "http://evil.com".into(),
                transport: "http".into()
            },
            false,
            false,
            &[]
        )
        .is_err());
        assert!(install_steps(
            Harness::Claude,
            "d",
            &Source::Catalog(find("drive").unwrap()),
            true,
            true,
            &[]
        )
        .unwrap_err()
        .contains("connector"));
        let c = install_steps(
            Harness::Claude,
            "fs",
            &Source::Command(vec!["npx".into(), "-y".into(), "pkg".into()]),
            false,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(
            shown("claude", &c.steps[0]),
            "claude mcp add --scope user fs -- npx -y pkg"
        );
    }

    #[test]
    fn reads_capabilities_without_secrets() {
        let d = std::env::temp_dir().join(format!("godterm-caps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let plug = d.join("plugins/cache/x/slack/1");
        std::fs::create_dir_all(&plug).unwrap();
        std::fs::create_dir_all(d.join("skills/review")).unwrap();
        std::fs::write(d.join(".claude.json"), r#"{"oauthAccount": {"emailAddress": "secret@example.com"}, "mcpServers": {"linear": {"type": "http", "url": "https://mcp.linear.app/mcp?token=abc", "headers": {"Authorization": "Bearer SECRET"}}}}"#).unwrap();
        std::fs::write(
            d.join("settings.json"),
            r#"{"enabledPlugins": {"slack@claude-plugins-official": true}, "hooks": {"Stop": []}}"#,
        )
        .unwrap();
        std::fs::write(d.join("plugins/installed_plugins.json"), format!(r#"{{"version": 2, "plugins": {{"slack@claude-plugins-official": [{{"installPath": "{}"}}]}}}}"#, plug.display().to_string().replace('\\', "\\\\"))).unwrap();
        std::fs::write(
            plug.join(".mcp.json"),
            r#"{"mcpServers": {"slack": {"type": "http", "url": "https://mcp.slack.com/mcp"}}}"#,
        )
        .unwrap();
        let c = claude_caps(&d);
        let j = caps_json(&c).to_string();
        assert!(
            !j.contains("SECRET") && !j.contains("secret@") && !j.contains("token=abc"),
            "{j}"
        );
        assert_eq!(c.servers.len(), 2);
        assert_eq!(c.servers[1].name, "plugin:slack:slack");
        assert!(c.plugins[0].enabled);
        assert_eq!(c.skills, vec!["review"]);
        assert_eq!(c.hooks, vec!["Stop"]);
        assert_eq!(summary(&c), "mcp linear, slack; plugins slack; 1 skills");
        // Grok.
        let g = d.join("grok");
        std::fs::create_dir_all(g.join("installed-plugins/gdrive")).unwrap();
        std::fs::write(g.join("config.toml"), "disabled_mcp_servers = [\"sentry\"]\n[mcp_servers.sentry]\nurl = \"https://mcp.sentry.dev/mcp\"\n[plugins]\nenabled = [\"gdrive\"]\n").unwrap();
        let gc = grok_caps(&g);
        assert_eq!(gc.servers[0].status, "disabled");
        assert!(gc.plugins[0].enabled);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn finds_wrapped_login_urls() {
        let screen = "Browser didn't open? Use the url below to sign in:\n\nhttps://claude.ai/oauth/authorize?code=true&client_id=9d1c\n250a-e61b-44d9-88ed-5944d1962f5e&state=xyz\n\nPaste code here if prompted >";
        assert_eq!(
            url_on_screen(screen).unwrap(),
            "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&state=xyz"
        );
        assert_eq!(safe_target("https://u:p@h.com/x?k=1"), "https://h.com/x");
    }
}
