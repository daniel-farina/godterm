use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app_tabmodel::{BarModel, MAX_MODELS};
use crate::config::Config;
use crate::harness::Harness;

const TTL: Duration = Duration::from_secs(12 * 3600);

const RETRY: Duration = Duration::from_secs(30 * 60);
pub const MODELS_URL: &str = "https://api.anthropic.com/v1/models?limit=100";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,

    pub name: String,

    pub line: String,
}

const CLAUDE_LINES: &[(&str, &str)] = &[
    ("haiku", "quickest"),
    ("sonnet", "balanced"),
    ("opus", "thorough"),
    ("fable", "most capable"),
];

const CLAUDE_ALIASES: &[&str] = &["haiku", "sonnet", "opus"];

pub fn parse_anthropic(json: &str) -> Vec<Model> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return vec![];
    };
    let mut rows: Vec<(String, Model)> = vec![];
    for m in v["data"].as_array().into_iter().flatten() {
        let Some(id) = m["id"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        if m["lifecycle"].as_str().is_some_and(|l| l != "active") {
            continue;
        }
        let shown = m["display_name"].as_str().unwrap_or(id);
        let name = shown.strip_prefix("Claude ").unwrap_or(shown).to_string();
        let line = m["line"]
            .as_str()
            .map(str::to_lowercase)
            .unwrap_or_else(|| first_word(&name));
        let at = m["created_at"].as_str().unwrap_or("").to_string();
        rows.push((
            at,
            Model {
                id: id.to_string(),
                name,
                line,
            },
        ));
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows.into_iter().map(|r| r.1).collect()
}

fn first_word(s: &str) -> String {
    s.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

pub fn parse_grok_models(text: &str) -> Vec<Model> {
    if text.to_lowercase().contains("not authenticated") {
        return vec![];
    }
    let mut out: Vec<Model> = vec![];
    let mut on = false;
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("Available models") {
            on = true;
            continue;
        }
        if !on {
            continue;
        }
        let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) else {
            continue;
        };
        let id = rest.split_whitespace().next().unwrap_or("");
        if !id.is_empty() && !out.iter().any(|m| m.id == id) {
            out.push(Model {
                id: id.to_string(),
                name: id.to_string(),
                line: String::new(),
            });
        }
    }
    out
}

pub fn parse_claude_menu(screen: &str) -> Vec<Model> {
    let mut out: Vec<Model> = vec![];
    for l in screen.lines() {
        let t = l.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '❯' | '>' | '›'));
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Some(rest) = t[digits.len()..].strip_prefix(". ") else {
            continue;
        };
        if digits.is_empty() {
            continue;
        }
        let name = rest
            .split("  ")
            .next()
            .unwrap_or("")
            .trim()
            .trim_end_matches('✔')
            .trim();
        let line = first_word(name);
        let known = CLAUDE_LINES.iter().any(|(k, _)| *k == line);
        let versioned = name
            .split_whitespace()
            .nth(1)
            .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_digit()));
        if name.is_empty() || name.starts_with("Default") || !(known || versioned) {
            continue;
        }
        let ver: String = name
            .split_whitespace()
            .skip(1)
            .collect::<Vec<_>>()
            .join("-")
            .replace('.', "-");
        let id = if ver.is_empty() {
            line.clone()
        } else {
            format!("claude-{line}-{ver}")
        };
        if !out.iter().any(|m| m.id == id) {
            out.push(Model {
                id,
                name: name.to_string(),
                line,
            });
        }
    }
    out
}

pub fn parse_grok_menu(screen: &str) -> Vec<Model> {
    let mut out: Vec<Model> = vec![];
    for w in screen.split(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '.' | '_'))) {
        let ok = w
            .strip_prefix("grok-")
            .is_some_and(|r| r.chars().next().is_some_and(|c| c.is_ascii_digit()));
        if ok && !out.iter().any(|m| m.id == w) {
            out.push(Model {
                id: w.to_string(),
                name: w.to_string(),
                line: String::new(),
            });
        }
    }
    out
}

fn claude_newest(list: &[Model]) -> Vec<&Model> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut v: Vec<&Model> = list
        .iter()
        .filter(|m| !m.line.is_empty() && seen.insert(m.line.as_str()))
        .collect();
    let rank = |m: &Model| {
        CLAUDE_LINES
            .iter()
            .position(|(k, _)| *k == m.line)
            .unwrap_or(CLAUDE_LINES.len())
    };
    v.sort_by_key(|m| rank(m));
    v
}

pub fn claude_bar(list: &[Model]) -> Vec<BarModel> {
    claude_newest(list)
        .into_iter()
        .take(MAX_MODELS)
        .map(|m| BarModel {
            arg: if CLAUDE_ALIASES.contains(&m.line.as_str()) {
                m.line.clone()
            } else {
                m.id.clone()
            },
            label: m.name.clone(),
            desc: CLAUDE_LINES
                .iter()
                .find(|(k, _)| *k == m.line)
                .map(|(_, d)| d.to_string())
                .unwrap_or_default(),
            matches: vec![m.line.clone()],
        })
        .collect()
}

pub fn grok_bar(list: &[Model]) -> Vec<BarModel> {
    let known = crate::providers::tab_models(Harness::Grok);
    list.iter()
        .filter(|m| m.id.starts_with("grok-"))
        .take(MAX_MODELS)
        .map(|m| {
            let short = m.id.trim_start_matches("grok-");
            let fast = short.contains("build-fast");
            let label = if fast {
                format!("{} fast", short.replace("-build-fast", ""))
            } else {
                short.to_string()
            };
            let desc = known
                .iter()
                .find(|k| k.arg == m.id)
                .map(|k| k.desc.to_string())
                .unwrap_or_else(|| {
                    if fast {
                        "quickest".into()
                    } else {
                        String::new()
                    }
                });
            BarModel {
                arg: m.id.clone(),
                label,
                desc,
                matches: vec![m.id.to_lowercase(), m.id.to_lowercase().replace('-', " ")],
            }
        })
        .collect()
}

pub fn assistant_ids(id: &str, list: &[Model]) -> Vec<String> {
    match id {
        "claude" => claude_newest(list).iter().map(|m| m.id.clone()).collect(),
        _ => list
            .iter()
            .filter(|m| m.id.starts_with("grok-"))
            .map(|m| m.id.clone())
            .collect(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cached {
    at: u64,
    models: Vec<Model>,
}

#[derive(Default)]
struct State {
    disk_read: bool,
    lists: HashMap<String, Cached>,
    inflight: HashSet<String>,
    failed: HashMap<String, Instant>,
}

static STATE: std::sync::LazyLock<Mutex<State>> = std::sync::LazyLock::new(Mutex::default);

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_path() -> PathBuf {
    crate::config::app_home().join("model-lists.json")
}

fn live() -> bool {
    !cfg!(test) && !crate::demo::active()
}

fn save(lists: &HashMap<String, Cached>) {
    let p = cache_path();
    let tmp = p.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string(lists) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

pub fn get(key: &str, cfg: &Config) -> Option<Vec<Model>> {
    let mut st = lock();
    if live() && !st.disk_read {
        st.disk_read = true;
        if let Some(m) = std::fs::read_to_string(cache_path())
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, Cached>>(&t).ok())
        {
            for (k, v) in m {
                st.lists.entry(k).or_insert(v);
            }
        }
    }
    let have = st.lists.get(key).cloned();
    let fresh = have
        .as_ref()
        .is_some_and(|c| now_secs().saturating_sub(c.at) < TTL.as_secs());
    let waiting = st.failed.get(key).is_some_and(|t| t.elapsed() < RETRY);
    if live() && !fresh && !waiting && st.inflight.insert(key.to_string()) {
        let key = key.to_string();
        let src = Source::of(cfg);
        std::thread::spawn(move || {
            let res = fetch(&key, &src);
            let mut st = lock();
            st.inflight.remove(&key);
            match res {
                Some(models) => {
                    st.failed.remove(&key);
                    st.lists.insert(
                        key,
                        Cached {
                            at: now_secs(),
                            models,
                        },
                    );
                    save(&st.lists);
                }
                None => {
                    st.failed.insert(key, Instant::now());
                }
            }
        });
    }
    have.map(|c| c.models).filter(|m| !m.is_empty())
}

pub fn bar(h: Harness, cfg: &Config) -> Option<Vec<BarModel>> {
    let (key, f): (&str, fn(&[Model]) -> Vec<BarModel>) = match h {
        Harness::Claude => ("claude", claude_bar),
        Harness::Grok => ("grok", grok_bar),
        _ => return None,
    };
    let v = f(&get(key, cfg)?);
    (!v.is_empty()).then_some(v)
}

pub fn assistant_models(id: &str, cfg: &Config) -> Option<Vec<String>> {
    let key = if id == "grok" { "grok" } else { "claude" };
    let v = assistant_ids(key, &get(key, cfg)?);
    (!v.is_empty()).then_some(v)
}

struct Source {
    claude_bin: String,
    grok_bin: String,
    claude_dirs: Vec<PathBuf>,
    grok_dirs: Vec<PathBuf>,
}

impl Source {
    fn of(cfg: &Config) -> Source {
        let dirs = |h: Harness| -> Vec<PathBuf> {
            cfg.accounts
                .iter()
                .filter(|a| a.harness() == h)
                .map(|a| a.config_dir())
                .collect()
        };
        Source {
            claude_bin: cfg.claude_bin(),
            grok_bin: Harness::Grok.bin(cfg.grok_bin.as_deref()),
            claude_dirs: dirs(Harness::Claude),
            grok_dirs: dirs(Harness::Grok),
        }
    }
}

fn fetch(key: &str, src: &Source) -> Option<Vec<Model>> {
    let res = match key {
        "claude" => fetch_claude(src),
        _ => fetch_grok(src),
    };
    crate::log::info(&format!(
        "model list {key}: {}",
        match &res {
            Some(m) => format!("{} models", m.len()),
            None => "none, keeping the built in list".into(),
        }
    ));
    res
}

fn fetch_claude(src: &Source) -> Option<Vec<Model>> {
    for dir in &src.claude_dirs {
        let Some((c, _)) = crate::creds::load_creds(dir) else {
            continue;
        };
        let expired = c
            .expires_at
            .is_some_and(|e| e <= chrono::Utc::now().timestamp_millis());
        if c.access_token.is_empty() || expired {
            continue;
        }
        if let Some(body) = http_models(&c.access_token) {
            let v = parse_anthropic(&body);
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    let dir = src.claude_dirs.first()?;
    let screen = menu_screen(&src.claude_bin, &[], "CLAUDE_CONFIG_DIR", dir)?;
    let v = parse_claude_menu(&screen);
    (!v.is_empty()).then_some(v)
}

fn http_models(token: &str) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let mut resp = agent
        .get(MODELS_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .header("Accept", "application/json")
        .header("User-Agent", concat!("godterm/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?;
    resp.body_mut().read_to_string().ok()
}

fn fetch_grok(src: &Source) -> Option<Vec<Model>> {
    let home = src.grok_dirs.iter().find(|d| d.join("auth.json").exists());
    let mut cmd = std::process::Command::new(&src.grok_bin);
    cmd.arg("models");
    if let Some(h) = home {
        cmd.env(Harness::Grok.home_env(), h);
    }
    if let Some(o) = crate::creds::output_with_timeout(cmd, Duration::from_secs(20)) {
        let v = parse_grok_models(&String::from_utf8_lossy(&o.stdout));
        if !v.is_empty() {
            return Some(v);
        }
    }
    let home = home?;
    let screen = menu_screen(&src.grok_bin, &[], Harness::Grok.home_env(), home)?;
    let v = parse_grok_menu(&screen);
    (!v.is_empty()).then_some(v)
}

fn menu_screen(bin: &str, args: &[&str], home_env: &str, home: &std::path::Path) -> Option<String> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex as M};
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok()?;
    let mut cmd = CommandBuilder::new(bin);
    cmd.args(args);
    cmd.cwd(std::env::temp_dir());
    if !home_env.is_empty() {
        cmd.env(home_env, home);
    }
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).ok()?;
    drop(pair.slave);
    let parser = Arc::new(M::new(vt100::Parser::new(40, 120, 0)));
    let mut reader = pair.master.try_clone_reader().ok()?;
    let mut writer = pair.master.take_writer().ok()?;
    let p2 = parser.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            p2.lock()
                .unwrap_or_else(|e| e.into_inner())
                .process(&buf[..n]);
        }
    });
    let screen = || {
        parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .contents()
    };
    std::thread::sleep(Duration::from_secs(6));
    let _ = writer.write_all(b"/model");
    std::thread::sleep(Duration::from_millis(800));
    let _ = writer.write_all(b"\r");
    let t0 = Instant::now();
    let mut text = String::new();
    while t0.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(500));
        text = screen();
        if parse_claude_menu(&text).len() > 1 || parse_grok_menu(&text).len() > 1 {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Some(text)
}

#[cfg(test)]
pub mod testing {
    use super::*;

    pub fn set(key: &str, models: Vec<Model>) {
        lock().lists.insert(
            key.to_string(),
            Cached {
                at: now_secs(),
                models,
            },
        );
    }

    pub fn clear() {
        lock().lists.clear();
    }
}
