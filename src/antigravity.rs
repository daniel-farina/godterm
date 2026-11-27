use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::pane::Activity;
use crate::sessions::{lossy_lines, snippet, SessionInfo};
use crate::usage::{Usage, Window};

pub const TOKEN_FILE: &str = "antigravity-oauth-token";
pub const STATE_ENV: &str = "GODTERM_AGY_STATE";
pub const FILE_TOKENS_ENV: &str = "AGY_CLI_FORCE_FILE_TOKEN_STORAGE";
pub const OWN_DIR: &str = ".godterm-agy";
pub const NATIVE_LABEL: &str = "Antigravity (plain terminal)";
pub const DEFAULT_PROJECT: &str = "default-cli-project";

pub const STATE_CMD: &str = r#"[ -n "$GODTERM_AGY_STATE" ] && cat > "$GODTERM_AGY_STATE.tmp" && mv -f "$GODTERM_AGY_STATE.tmp" "$GODTERM_AGY_STATE""#;

const STALE: Duration = Duration::from_secs(10);

pub fn data_dir(home: &Path) -> PathBuf {
    home.join(".gemini").join("antigravity-cli")
}

pub fn config_dir(home: &Path) -> PathBuf {
    home.join(".gemini").join("config")
}

pub fn own_dir(home: &Path) -> PathBuf {
    home.join(OWN_DIR)
}

fn same(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

pub fn isolated(home: &Path) -> bool {
    cfg!(unix) && !crate::demo::active() && !same(home, &crate::config::home_dir())
}

pub fn prepare_home(home: &Path) {
    if !isolated(home) {
        return;
    }
    for d in [data_dir(home), config_dir(home), own_dir(home)] {
        let _ = std::fs::create_dir_all(d);
    }
    #[cfg(unix)]
    if !cfg!(test) {
        link_home(&crate::config::home_dir(), home);
    }
}

#[cfg(unix)]
pub fn link_home(real: &Path, home: &Path) -> usize {
    let app = crate::config::app_home();
    let app_name = (app.parent() == Some(real))
        .then(|| app.file_name().map(|n| n.to_os_string()))
        .flatten();
    let mut made = 0;
    if let Ok(rd) = std::fs::read_dir(home) {
        for e in rd.flatten() {
            let p = e.path();
            let Ok(target) = std::fs::read_link(&p) else {
                continue;
            };
            if target.parent() == Some(real) && !target.exists() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let Ok(rd) = std::fs::read_dir(real) else {
        return 0;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        if name == ".gemini"
            || name == ".Trash"
            || name == OWN_DIR
            || app_name.as_ref().is_some_and(|a| *a == name)
        {
            continue;
        }
        let to = home.join(&name);
        if to.symlink_metadata().is_ok() {
            continue;
        }
        if std::os::unix::fs::symlink(e.path(), &to).is_ok() {
            made += 1;
        }
    }
    made
}

pub fn token_path(home: &Path) -> PathBuf {
    data_dir(home).join(TOKEN_FILE)
}

pub fn logged_in(home: &Path) -> bool {
    std::fs::metadata(token_path(home)).is_ok_and(|m| m.len() > 2)
}

pub fn logout(home: &Path) -> bool {
    if !isolated(home) {
        return false;
    }
    let _ = std::fs::remove_file(own_dir(home).join("account.json"));
    let p = token_path(home);
    !p.exists() || std::fs::remove_file(p).is_ok()
}

pub fn state_path(uid: u64) -> PathBuf {
    crate::config::app_home()
        .join("run")
        .join("agy")
        .join(format!("t{uid}.json"))
}

pub fn has_settings(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--settings" || a.starts_with("--settings="))
}

pub fn settings_arg() -> String {
    json!({"title": {"type": "command", "command": STATE_CMD}}).to_string()
}

pub fn launch(home: &Path, uid: u64, args: &mut Vec<String>) -> Vec<(String, String)> {
    if crate::demo::active() {
        return vec![];
    }
    prepare_home(home);
    let mut env = vec![];
    if isolated(home) {
        env.push((FILE_TOKENS_ENV.to_string(), "1".to_string()));
    }
    let p = state_path(uid);
    let _ = std::fs::remove_file(&p);
    forget(uid);
    if cfg!(unix) && !has_settings(args) {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        args.push("--settings".into());
        args.push(settings_arg());
        env.push((STATE_ENV.to_string(), p.to_string_lossy().into_owned()));
    }
    env
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub agent_state: String,
    pub conversation_id: String,
    pub title: Option<String>,
    pub confirm: bool,
    pub queued: u64,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub model: Option<String>,
    pub cwd: String,
    pub quota: Value,
}

pub fn parse_state(v: &Value) -> State {
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|x| !x.trim().is_empty())
            .map(str::to_string)
    };
    State {
        agent_state: s("agent_state").unwrap_or_default(),
        conversation_id: s("conversation_id")
            .or_else(|| s("session_id"))
            .filter(|id| crate::harness::Harness::safe_session_id(id))
            .unwrap_or_default(),
        title: s("conversation_title"),
        confirm: v["tool_confirmation_pending"] == true,
        queued: v["pending_input_count"].as_u64().unwrap_or(0),
        email: s("email"),
        plan: s("plan_tier"),
        model: v
            .pointer("/model/display_name")
            .or_else(|| v.pointer("/model/id"))
            .and_then(Value::as_str)
            .map(str::to_string),
        cwd: v
            .pointer("/workspace/project_dir")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| s("cwd"))
            .unwrap_or_default(),
        quota: v.get("quota").cloned().unwrap_or(Value::Null),
    }
}

type Cached = (SystemTime, State);

fn cache() -> &'static Mutex<HashMap<u64, Cached>> {
    static C: std::sync::OnceLock<Mutex<HashMap<u64, Cached>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

pub fn forget(uid: u64) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&uid);
}

pub fn read_state(uid: u64) -> Option<(SystemTime, State)> {
    let p = state_path(uid);
    let at = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, s)) = c.get(&uid) {
        if *t == at {
            return Some((at, s.clone()));
        }
    }
    let v: Value = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
    let s = parse_state(&v);
    c.insert(uid, (at, s.clone()));
    Some((at, s))
}

pub fn activity(s: &State) -> Option<Activity> {
    if s.confirm {
        return Some(Activity::Permission);
    }
    match s.agent_state.as_str() {
        "working" | "tool_use" => Some(Activity::Working),
        "idle" => Some(Activity::Ready),
        _ => None,
    }
}

pub fn blend_with(
    state: Option<(SystemTime, State)>,
    started: SystemTime,
    now: SystemTime,
    screen: Activity,
) -> Activity {
    if screen == Activity::Permission {
        return screen;
    }
    let Some((at, s)) = state.filter(|(at, _)| *at >= started) else {
        return screen;
    };
    let Some(a) = activity(&s) else {
        return screen;
    };
    let fresh = now.duration_since(at).unwrap_or_default() < STALE;
    if a == screen || a == Activity::Permission || fresh {
        a
    } else {
        screen
    }
}

pub fn blend(uid: u64, started: SystemTime, screen: Activity) -> Activity {
    blend_with(read_state(uid), started, SystemTime::now(), screen)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Account {
    pub email: Option<String>,
    pub plan: Option<String>,
    pub quota: Value,
}

pub fn account(home: &Path) -> Account {
    let v: Value = std::fs::read(own_dir(home).join("account.json"))
        .ok()
        .and_then(|t| serde_json::from_slice(&t).ok())
        .unwrap_or(Value::Null);
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Account {
        email: s("email"),
        plan: s("plan"),
        quota: v.get("quota").cloned().unwrap_or(Value::Null),
    }
}

fn sessions_file(home: &Path) -> PathBuf {
    own_dir(home).join("sessions.json")
}

pub fn known_sessions(home: &Path) -> serde_json::Map<String, Value> {
    std::fs::read(sessions_file(home))
        .ok()
        .and_then(|t| serde_json::from_slice::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn note(home: &Path, s: &State) -> bool {
    if !isolated(home) {
        return false;
    }
    let mut changed = false;
    if s.email.is_some() || s.plan.is_some() || !s.quota.is_null() {
        let old = account(home);
        let new = Account {
            email: s.email.clone().or(old.email.clone()),
            plan: s.plan.clone().or(old.plan.clone()),
            quota: if s.quota.is_null() {
                old.quota.clone()
            } else {
                s.quota.clone()
            },
        };
        if new != old {
            let _ = std::fs::create_dir_all(own_dir(home));
            let body = json!({"email": new.email, "plan": new.plan, "quota": new.quota});
            changed |=
                crate::config::write_private(&own_dir(home).join("account.json"), body.to_string())
                    .is_ok();
        }
    }
    if !s.conversation_id.is_empty() {
        let mut all = known_sessions(home);
        let entry = json!({"cwd": s.cwd, "title": s.title, "model": s.model});
        if all.get(&s.conversation_id) != Some(&entry) {
            all.insert(s.conversation_id.clone(), entry);
            let _ = std::fs::create_dir_all(own_dir(home));
            let _ =
                crate::config::write_private(&sessions_file(home), Value::Object(all).to_string());
        }
    }
    changed
}

fn bucket_label(id: &str) -> (String, String) {
    let long = id.replace(['-', '_'], " ");
    let short = if id.contains("week") {
        "wk".to_string()
    } else if id.contains("five") || id.contains("5h") || id.contains("hour") {
        "5h".to_string()
    } else {
        long.clone()
    };
    (short, long)
}

pub fn usage(quota: &Value) -> Option<Usage> {
    let mut windows = vec![];
    for (id, b) in quota.as_object()? {
        if b["disabled"] == true {
            continue;
        }
        let Some(left) = b["remaining_fraction"].as_f64() else {
            continue;
        };
        let (short, label) = bucket_label(id);
        windows.push(Window {
            key: format!("quota:{id}"),
            label,
            short,
            utilization: ((1.0 - left) * 100.0).clamp(0.0, 100.0),
            resets_at: b["reset_time"]
                .as_str()
                .and_then(|t| t.parse::<chrono::DateTime<chrono::Utc>>().ok()),
        });
    }
    (!windows.is_empty()).then_some(Usage {
        windows,
        absent: vec![],
        extra: None,
    })
}

pub const QUOTA_KEYS: &[(&str, &str, &str)] = &[("quota:*", "", "")];

pub fn is_transcript(p: &Path) -> bool {
    p.file_name().is_some_and(|f| f == "transcript.jsonl")
        && p.parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|f| f == ".system_generated")
}

pub fn transcript_of(home: &Path, id: &str) -> PathBuf {
    data_dir(home)
        .join("brain")
        .join(id)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl")
}

pub fn list_transcripts(home: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(data_dir(home).join("brain")) else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let p = transcript_of(home, &id);
            p.is_file().then_some(p)
        })
        .collect()
}

pub fn user_text(content: &str) -> Option<String> {
    let t = match content.find("<USER_REQUEST>") {
        Some(i) => {
            let rest = &content[i + "<USER_REQUEST>".len()..];
            rest.split("</USER_REQUEST>").next().unwrap_or(rest)
        }
        None => content,
    }
    .trim();
    (!t.is_empty()).then(|| t.to_string())
}

pub fn annotation_title(text: &str) -> Option<String> {
    let rest = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("title:"))?
        .trim();
    let inner = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push(' '),
                Some(x) => out.push(x),
                None => break,
            },
            c => out.push(c),
        }
    }
    (!out.trim().is_empty()).then(|| out.trim().to_string())
}

pub fn parse_transcript(path: &Path) -> Option<SessionInfo> {
    let brain = path.parent()?.parent()?.parent()?;
    let id = brain.file_name()?.to_string_lossy().into_owned();
    let data = brain.parent()?.parent()?;
    let home = data.parent()?.parent()?;
    let f = std::fs::File::open(path).ok()?;
    let mut info = SessionInfo {
        id: id.clone(),
        path: path.to_path_buf(),
        modified: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
        ..Default::default()
    };
    let mut asked: Vec<String> = vec![];
    let mut asked_at: Vec<(String, i64)> = vec![];
    for line in lossy_lines(BufReader::new(f)) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let at = v["created_at"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
        if info.created.is_none() {
            info.created = at.map(SystemTime::from);
        }
        match v["type"].as_str() {
            Some("USER_INPUT") => {
                if let Some(t) = v["content"].as_str().and_then(user_text) {
                    if let Some(at) = at {
                        asked_at.push((t.clone(), at.timestamp_millis()));
                    }
                    asked.push(t);
                }
            }
            Some("PLANNER_RESPONSE") => {
                info.messages += 1;
                if let Some(m) = v["model"].as_str() {
                    info.model = Some(m.to_string());
                }
                let cached = v["cache_read_tokens"].as_u64().unwrap_or(0);
                info.tokens.input += v["input_tokens"].as_u64().unwrap_or(0);
                info.tokens.cache_read += cached;
                info.tokens.output += v["output_tokens"].as_u64().unwrap_or(0);
            }
            _ => {}
        }
    }
    info.messages += asked.len();
    info.first_prompt = asked.first().cloned();
    info.last_prompt = asked.last().map(|t| snippet(t, 160));
    let known = known_sessions(home);
    let rec = known.get(&id);
    info.cwd = rec
        .and_then(|r| r["cwd"].as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .or_else(|| last_cwd(data, &id))
        .or_else(|| history_cwd(data, &asked_at))
        .unwrap_or_default();
    let named = std::fs::read_to_string(data.join("annotations").join(format!("{id}.pbtxt")))
        .ok()
        .and_then(|t| annotation_title(&t))
        .or_else(|| {
            rec.and_then(|r| r["title"].as_str())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        });
    info.title = named
        .map(|t| snippet(&t, 80))
        .or_else(|| info.first_prompt.as_deref().map(|t| snippet(t, 80)));
    Some(info)
}

fn last_cwd(data: &Path, id: &str) -> Option<String> {
    let v: Value =
        serde_json::from_slice(&std::fs::read(data.join("cache/last_conversations.json")).ok()?)
            .ok()?;
    v.as_object()?
        .iter()
        .find(|(_, x)| x.as_str() == Some(id))
        .map(|(cwd, _)| cwd.clone())
}

const HISTORY_SLACK_MS: i64 = 120_000;

fn history_cwd(data: &Path, asked: &[(String, i64)]) -> Option<String> {
    if asked.is_empty() {
        return None;
    }
    let f = std::fs::File::open(data.join("history.jsonl")).ok()?;
    let mut best: Option<(i64, String)> = None;
    for line in lossy_lines(BufReader::new(f)) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(text), Some(at), Some(dir)) = (
            v["display"].as_str(),
            v["timestamp"].as_i64(),
            v["workspace"].as_str().filter(|w| !w.is_empty()),
        ) else {
            continue;
        };
        for (t, when) in asked {
            let off = (at - when).abs();
            if off <= HISTORY_SLACK_MS
                && t.trim() == text.trim()
                && best.as_ref().is_none_or(|(b, _)| off < *b)
            {
                best = Some((off, dir.to_string()));
            }
        }
    }
    best.map(|(_, d)| d)
}

#[cfg(not(test))]
pub fn native_home() -> Option<PathBuf> {
    (!crate::demo::active()).then(crate::config::home_dir)
}

#[cfg(test)]
pub static TEST_NATIVE_HOME: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(test)]
pub fn native_home() -> Option<PathBuf> {
    TEST_NATIVE_HOME
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub fn is_native(home: &Path) -> bool {
    native_home().is_some_and(|n| same(home, &n))
}

pub fn has_conversations(home: &Path) -> bool {
    data_dir(home).join("brain").is_dir()
}

pub fn has_conversation(home: &Path, id: &str) -> bool {
    data_dir(home)
        .join("conversations")
        .join(format!("{id}.db"))
        .is_file()
}

fn copy_tree(from: &Path, to: &Path, n: &mut usize) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in std::fs::read_dir(from)
        .map_err(|e| format!("{}: {e}", from.display()))?
        .flatten()
    {
        let Ok(t) = e.file_type() else { continue };
        let name = e.file_name();
        if t.is_symlink() || name == TOKEN_FILE {
            continue;
        }
        if t.is_dir() {
            copy_tree(&e.path(), &to.join(&name), n)?;
        } else if t.is_file() {
            std::fs::copy(e.path(), to.join(&name))
                .map_err(|e| format!("{}: {e}", to.join(&name).display()))?;
            *n += 1;
        }
    }
    Ok(())
}

pub fn copy_conversation(from: &Path, to: &Path, id: &str) -> Result<usize, String> {
    if !crate::harness::Harness::safe_session_id(id) || id.contains("..") {
        return Err("that is not a conversation id".into());
    }
    crate::guard::resolve_outside_vault(from)?;
    crate::guard::resolve_outside_vault(to)?;
    if same(from, to) {
        return Err("the conversation is already in that home".into());
    }
    if !isolated(to) || is_native(to) {
        return Err("a conversation is only copied into an account's own home".into());
    }
    let (src, dst) = (data_dir(from), data_dir(to));
    let db = format!("{id}.db");
    if !src.join("conversations").join(&db).is_file() {
        return Err(
            "that conversation has no saved state the CLI could resume (conversations/<id>.db is missing)"
                .into(),
        );
    }
    if dst.join("conversations").join(&db).exists() {
        return Err("that account already has a conversation with this id".into());
    }
    crate::guard::resolve_outside_vault(&dst)?;
    let mut n = 0;
    let conv = dst.join("conversations");
    std::fs::create_dir_all(&conv).map_err(|e| format!("{}: {e}", conv.display()))?;
    for e in std::fs::read_dir(src.join("conversations"))
        .map_err(|e| format!("{e}"))?
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(&db) && e.file_type().is_ok_and(|t| t.is_file()) {
            std::fs::copy(e.path(), conv.join(&name)).map_err(|e| format!("{name}: {e}"))?;
            n += 1;
        }
    }
    let brain = src.join("brain").join(id);
    if brain.is_dir() && !brain.is_symlink() {
        copy_tree(&brain, &dst.join("brain").join(id), &mut n)?;
    }
    let note = format!("{id}.pbtxt");
    let title = src.join("annotations").join(&note);
    if title.is_file() && !title.is_symlink() {
        let d = dst.join("annotations");
        std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        std::fs::copy(&title, d.join(&note)).map_err(|e| format!("{note}: {e}"))?;
        n += 1;
    }
    let projects = config_dir(to).join("projects");
    let project = projects.join(format!("{DEFAULT_PROJECT}.json"));
    if !project.exists() {
        let _ = std::fs::create_dir_all(&projects);
        let body = json!({"id": DEFAULT_PROJECT, "name": "CLI Project", "projectResources": {}});
        let _ = std::fs::write(
            &project,
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        );
    }
    Ok(n)
}

pub fn remember(home: &Path, id: &str, cwd: &str, title: Option<&str>) {
    if !isolated(home) {
        return;
    }
    let mut all = known_sessions(home);
    if all.contains_key(id) {
        return;
    }
    all.insert(
        id.to_string(),
        json!({"cwd": cwd, "title": title, "model": null}),
    );
    let _ = std::fs::create_dir_all(own_dir(home));
    let _ = crate::config::write_private(&sessions_file(home), Value::Object(all).to_string());
}

pub fn write_mcp(home: &Path, exe: &Path, on: bool) -> bool {
    if !isolated(home) || (!home.starts_with(crate::config::app_home()) && !cfg!(test)) {
        return false;
    }
    let p = config_dir(home).join("mcp_config.json");
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let mut doc: Value = if text.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&text) {
            Ok(v @ Value::Object(_)) => v,
            _ => return false,
        }
    };
    let ours = doc
        .pointer("/mcpServers/godterm/env/GODTERM_CLIENT")
        .and_then(Value::as_str)
        == Some("tab");
    let exists = doc.pointer("/mcpServers/godterm").is_some();
    if exists && !ours {
        return false;
    }
    if on {
        let mut env = serde_json::Map::new();
        env.insert(
            "GODTERM_HOME".into(),
            json!(crate::config::app_home().to_string_lossy()),
        );
        env.insert("GODTERM_CLIENT".into(), json!("tab"));
        if cfg!(test) {
            env.insert(crate::test_guard::ENV.into(), json!("tab-mcp"));
        }
        if !doc["mcpServers"].is_object() {
            doc["mcpServers"] = json!({});
        }
        doc["mcpServers"]["godterm"] =
            json!({"command": exe.to_string_lossy(), "args": ["mcp"], "env": env});
    } else if exists {
        if let Some(m) = doc["mcpServers"].as_object_mut() {
            m.remove("godterm");
        }
    } else {
        return false;
    }
    let new = serde_json::to_string_pretty(&doc).unwrap_or_default();
    if new != text {
        let _ = std::fs::create_dir_all(config_dir(home));
        let _ = crate::config::write_private(&p, new);
    }
    on
}

impl crate::app::App {
    pub(crate) fn antigravity_tick(&mut self) {
        let mut seen: Vec<(usize, State)> = vec![];
        for si in 0..self.panes.len() {
            let Some(a) = self.panes[si].account else {
                continue;
            };
            for t in self.panes[si].tabs.iter_mut() {
                if t.harness != crate::harness::Harness::Antigravity || !t.is_running() {
                    continue;
                }
                let Some((at, s)) = read_state(t.uid) else {
                    continue;
                };
                if at < t.started {
                    continue;
                }
                if !s.conversation_id.is_empty()
                    && t.set_session(
                        Some(s.conversation_id.clone()),
                        "the CLI reports it in its state",
                    )
                {
                    self.state_dirty = true;
                }
                seen.push((a, s));
            }
        }
        for (a, s) in seen {
            let Some(cfg) = self.cfg.accounts.get(a) else {
                continue;
            };
            if cfg.harness() != crate::harness::Harness::Antigravity {
                continue;
            }
            let cfg = cfg.clone();
            if note(&cfg.config_dir(), &s) {
                let snap = crate::app::snapshot_of(&cfg, true);
                let _ = self
                    .tx
                    .send(crate::app::AppEvent::Status(a, Box::new(snap)));
            }
        }
    }
}

#[cfg(test)]
#[path = "antigravity_tests.rs"]
mod tests;
