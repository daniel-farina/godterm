use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const EXTENSION_ID: &str = "hhcmgoofomhgciiibhipgmgkgnoenaoi";

#[derive(Debug, Clone)]
pub struct Env {
    pub bin_dirs: Vec<PathBuf>,

    pub chrome_root: PathBuf,

    pub timeout: Duration,

    pub skill_sources: Vec<PathBuf>,
}

impl Env {
    pub fn real() -> Env {
        let home = crate::config::home_dir();
        let mut bin_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        bin_dirs.push(home.join(".local").join("bin"));
        if let Some(d) = std::env::var_os("BSK_INSTALL_DIR").filter(|d| !d.is_empty()) {
            bin_dirs.push(PathBuf::from(d));
        }
        Env {
            bin_dirs,
            chrome_root: chrome_root(),
            timeout: Duration::from_secs(8),
            skill_sources: vec![
                home.join(".claude/skills/browser-skill"),
                home.join(".agents/skills/browser-skill"),
            ],
        }
    }
}

pub fn chrome_root() -> PathBuf {
    let home = crate::config::home_dir();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Google/Chrome")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("Google/Chrome/User Data")
    } else {
        home.join(".config/google-chrome")
    }
}

pub fn find_bsk(dirs: &[PathBuf]) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["bsk.exe", "bsk.cmd", "bsk"]
    } else {
        &["bsk"]
    };
    for d in dirs {
        for n in names {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[derive(Debug, PartialEq)]
pub enum RunErr {
    Spawn(String),

    Timeout,
}

pub fn run_bsk(bin: &Path, args: &[&str], timeout: Duration) -> Result<(i32, String), RunErr> {
    let mut child = Command::new(bin)
        .args(args)
        .env("BSK_AUTO_START", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| RunErr::Spawn(e.to_string()))?;
    let mut out = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let mut buf = Vec::new();
            let _ = o.take(1 << 20).read_to_end(&mut buf);
            s = String::from_utf8_lossy(&buf).into_owned();
        }
        let _ = tx.send(s);
    });
    let t0 = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) if t0.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunErr::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(e) => return Err(RunErr::Spawn(e.to_string())),
        }
    };
    let text = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    Ok((code, text))
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Browser {
    pub instance_id: String,

    pub label: String,
    pub browser_name: String,
    pub browser_version: String,
    pub extension_version: String,
    pub session_count: u64,
    pub connected_at_ms: u64,
    pub version_skew: bool,
    pub unresponsive: bool,
}

impl Browser {
    pub fn to_json(&self) -> Value {
        json!({
            "instance_id": self.instance_id,
            "label": self.label,
            "browser": self.browser_name,
            "browser_version": self.browser_version,
            "extension_version": self.extension_version,
            "sessions": self.session_count,
            "connected_at_ms": self.connected_at_ms,
            "version_skew": self.version_skew,
            "unresponsive": self.unresponsive,
        })
    }
}

fn browsers_of(v: &Value) -> Vec<Browser> {
    let Some(a) = v.as_array() else {
        return vec![];
    };
    let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    a.iter()
        .filter(|x| x.is_object())
        .map(|x| Browser {
            instance_id: s(x, "instance_id"),
            label: s(x, "label"),
            browser_name: s(x, "browser_name"),
            browser_version: s(x, "browser_version"),
            extension_version: s(x, "extension_version"),
            session_count: x.get("session_count").and_then(Value::as_u64).unwrap_or(0),
            connected_at_ms: x
                .get("connected_at_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            version_skew: x
                .get("version_skew")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            unresponsive: x
                .get("unresponsive")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .filter(|b| !b.instance_id.is_empty())
        .collect()
}

pub fn parse_browsers(text: &str) -> Vec<Browser> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    match v.get("browsers") {
        Some(b) => browsers_of(b),
        None => browsers_of(&v),
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DaemonInfo {
    pub version: String,
    pub pid: u64,
    pub uptime_secs: u64,
    pub ws_port: u64,
    pub sessions: usize,
    pub browsers: Vec<Browser>,
}

pub fn parse_status(text: &str) -> Option<DaemonInfo> {
    let v: Value = serde_json::from_str(text).ok()?;
    let o = v.as_object()?;

    if !["daemon_version", "pid", "ws_port", "browsers", "sessions"]
        .iter()
        .any(|k| o.contains_key(*k))
    {
        return None;
    }
    let u = |k: &str| o.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(DaemonInfo {
        version: o
            .get("daemon_version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        pid: u("pid"),
        uptime_secs: u("uptime_secs"),
        ws_port: u("ws_port"),
        sessions: o
            .get("sessions")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        browsers: o.get("browsers").map(browsers_of).unwrap_or_default(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct DoctorLine {
    pub status: String,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Doctor {
    pub lines: Vec<DoctorLine>,
}

impl Doctor {
    fn count(&self, pred: impl Fn(&str) -> bool) -> usize {
        self.lines.iter().filter(|l| pred(&l.status)).count()
    }
    pub fn ok(&self) -> usize {
        self.count(|s| s == "ok")
    }
    pub fn warn(&self) -> usize {
        self.count(|s| s.starts_with("warn"))
    }
    pub fn fail(&self) -> usize {
        self.count(|s| s.starts_with("fail") || s == "error")
    }

    pub fn summary(&self) -> String {
        if self.lines.is_empty() {
            return "no result".into();
        }
        let mut s = format!("{} ok", self.ok());
        if self.warn() > 0 {
            s += &format!(
                ", {} warning{}",
                self.warn(),
                if self.warn() == 1 { "" } else { "s" }
            );
        }
        if self.fail() > 0 {
            s += &format!(", {} failed", self.fail());
        }
        s
    }
    pub fn problems(&self) -> Vec<&DoctorLine> {
        self.lines
            .iter()
            .filter(|l| l.status != "ok" && l.status != "info")
            .collect()
    }
}

pub fn parse_doctor(text: &str) -> Doctor {
    const WORDS: &[&str] = &[
        "ok", "warn", "warning", "fail", "failed", "error", "info", "skip",
    ];
    let mut lines = vec![];
    for l in text.lines() {
        let l = l.trim();
        let Some(first) = l.split_whitespace().next() else {
            continue;
        };
        let status = first.to_lowercase();
        if !WORDS.contains(&status.as_str()) {
            continue;
        }
        let rest = l[first.len()..].trim_start();
        let (name, detail) = match rest.find("  ") {
            Some(i) => (rest[..i].trim(), rest[i..].trim()),
            None => (rest.trim(), ""),
        };
        lines.push(DoctorLine {
            status,
            name: name.to_string(),
            detail: detail.to_string(),
        });
    }
    Doctor { lines }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HarnessSkill {
    pub id: String,
    pub skills_dir: String,
    pub detected: bool,
    pub installed: bool,
}

pub fn parse_harnesses(text: &str) -> Vec<HarnessSkill> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    let arr = v.get("harnesses").unwrap_or(&v);
    let Some(a) = arr.as_array() else {
        return vec![];
    };
    a.iter()
        .filter_map(|h| {
            let id = h.get("id")?.as_str()?.to_string();
            Some(HarnessSkill {
                id,
                skills_dir: h
                    .get("skills_dir")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                detected: h.get("detected").and_then(Value::as_bool).unwrap_or(false),
                installed: h.get("installed").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChromeProfile {
    pub dir: String,
    pub name: String,
    pub user_name: String,
    pub gaia_name: String,
}

impl ChromeProfile {
    pub fn display(&self) -> String {
        let mut s = if self.name.is_empty() {
            self.dir.clone()
        } else {
            self.name.clone()
        };
        if !self.user_name.is_empty() {
            s += &format!(" ({})", self.user_name);
        }
        s
    }
}

pub fn parse_local_state(text: &str) -> Vec<ChromeProfile> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    let Some(m) = v
        .get("profile")
        .and_then(|p| p.get("info_cache"))
        .and_then(Value::as_object)
    else {
        return vec![];
    };
    let mut out: Vec<ChromeProfile> = m
        .iter()
        .filter(|(_, p)| p.is_object())
        .map(|(dir, p)| {
            let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            ChromeProfile {
                dir: dir.clone(),
                name: s("name"),
                user_name: s("user_name"),
                gaia_name: s("gaia_name"),
            }
        })
        .collect();

    out.sort_by_key(|p| {
        let n = p
            .dir
            .strip_prefix("Profile ")
            .and_then(|n| n.parse::<u32>().ok());
        (p.dir != "Default", n.unwrap_or(u32::MAX), p.dir.clone())
    });
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExtPref {
    Missing,
    Enabled,
    Disabled,
}

pub fn parse_ext_pref(text: &str, id: &str) -> ExtPref {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return ExtPref::Missing;
    };
    let Some(e) = v
        .get("extensions")
        .and_then(|e| e.get("settings"))
        .and_then(|s| s.get(id))
    else {
        return ExtPref::Missing;
    };
    let reasons = match e.get("disable_reasons") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) != 0,
        Some(Value::Array(a)) => !a.is_empty(),
        _ => false,
    };
    let state_off = e.get("state").and_then(Value::as_u64) == Some(0);
    if reasons || state_off {
        ExtPref::Disabled
    } else {
        ExtPref::Enabled
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileExt {
    pub profile: ChromeProfile,

    pub installed: bool,

    pub enabled: Option<bool>,
}

impl ProfileExt {
    pub fn word(&self) -> &'static str {
        match (self.installed, self.enabled) {
            (false, None) => "not installed",
            (_, Some(false)) => "installed, disabled",
            (true, Some(true)) => "installed, enabled",
            (true, None) => "installed",
            (false, Some(true)) => "enabled (files not found)",
        }
    }
    pub fn present(&self) -> bool {
        self.installed || self.enabled.is_some()
    }
    pub fn to_json(&self) -> Value {
        json!({
            "profile_dir": self.profile.dir,
            "name": self.profile.name,
            "email": self.profile.user_name,
            "extension": self.word(),
            "installed": self.installed,
            "enabled": self.enabled,
        })
    }
}

pub fn chrome_extensions(root: &Path) -> Vec<ProfileExt> {
    let text = std::fs::read_to_string(root.join("Local State")).unwrap_or_default();
    parse_local_state(&text)
        .into_iter()
        .map(|p| {
            let d = root.join(&p.dir);
            let installed = d.join("Extensions").join(EXTENSION_ID).is_dir();
            let enabled = match std::fs::read_to_string(d.join("Secure Preferences")) {
                Ok(t) => match parse_ext_pref(&t, EXTENSION_ID) {
                    ExtPref::Missing => None,
                    ExtPref::Enabled => Some(true),
                    ExtPref::Disabled => Some(false),
                },
                Err(_) => None,
            };
            ProfileExt {
                profile: p,
                installed,
                enabled,
            }
        })
        .collect()
}

pub fn skill_dir(home: &Path) -> PathBuf {
    home.join("skills").join("browser-skill")
}

pub fn skill_present(home: &Path) -> bool {
    skill_dir(home).join("SKILL.md").is_file()
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountRef {
    pub number: usize,
    pub label: String,
    pub harness: String,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountSkill {
    pub account: AccountRef,
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Daemon {
    NotInstalled,

    Stopped,
    Running(DaemonInfo),

    Unresponsive,
    Unknown(String),
}

impl Daemon {
    pub fn word(&self) -> &'static str {
        match self {
            Daemon::NotInstalled => "bsk not installed",
            Daemon::Stopped => "not running",
            Daemon::Running(_) => "running",
            Daemon::Unresponsive => "not answering",
            Daemon::Unknown(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub at: std::time::SystemTime,
    pub bsk_path: Option<PathBuf>,
    pub version: Option<String>,
    pub daemon: Daemon,
    pub browsers: Vec<Browser>,
    pub doctor: Option<Doctor>,
    pub harnesses: Vec<HarnessSkill>,
    pub profiles: Vec<ProfileExt>,
    pub accounts: Vec<AccountSkill>,
}

impl Snapshot {
    pub fn age_secs(&self) -> u64 {
        self.at.elapsed().map(|d| d.as_secs()).unwrap_or(0)
    }

    pub fn browsers_json(&self) -> Value {
        json!({
            "daemon": self.daemon.word(),
            "browsers": self.browsers.iter().map(Browser::to_json).collect::<Vec<_>>(),
            "count": self.browsers.len(),
            "note": "label is free text the user typed in the extension popup; it is not the Chrome profile name and the mapping cannot be detected: ask the user which browser when unclear. Use the instance_id with `bsk session start --browser <id>`.",
            "checked_secs_ago": self.age_secs(),
        })
    }

    pub fn to_json(&self) -> Value {
        let daemon = match &self.daemon {
            Daemon::Running(i) => json!({
                "state": "running", "version": i.version, "pid": i.pid,
                "uptime_secs": i.uptime_secs, "ws_port": i.ws_port, "sessions": i.sessions,
            }),
            Daemon::Unknown(why) => json!({"state": "unknown", "why": why}),
            d => json!({"state": d.word()}),
        };
        json!({
            "bsk": {
                "installed": self.bsk_path.is_some(),
                "path": self.bsk_path.as_ref().map(|p| crate::config::tilde(p)),
                "version": self.version,
            },
            "daemon": daemon,
            "browsers": self.browsers.iter().map(Browser::to_json).collect::<Vec<_>>(),
            "doctor": self.doctor.as_ref().map(|d| json!({
                "summary": d.summary(),
                "problems": d.problems().iter().map(|l| json!({"status": l.status, "check": l.name, "detail": l.detail})).collect::<Vec<_>>(),
            })),
            "skill_harnesses": self.harnesses.iter().map(|h| json!({
                "id": h.id, "skills_dir": h.skills_dir, "detected": h.detected, "installed": h.installed,
            })).collect::<Vec<_>>(),
            "godterm_accounts": self.accounts.iter().map(|a| json!({
                "account": a.account.number, "label": a.account.label,
                "harness": a.account.harness, "skill_installed": a.installed,
            })).collect::<Vec<_>>(),
            "chrome_profiles": self.profiles.iter().map(ProfileExt::to_json).collect::<Vec<_>>(),
            "extension_id": EXTENSION_ID,
            "checked_secs_ago": self.age_secs(),
        })
    }
}

pub fn collect(env: &Env, accounts: &[AccountRef]) -> Snapshot {
    let bsk_path = find_bsk(&env.bin_dirs);
    let mut snap = Snapshot {
        at: std::time::SystemTime::now(),
        bsk_path: bsk_path.clone(),
        version: None,
        daemon: Daemon::NotInstalled,
        browsers: vec![],
        doctor: None,
        harnesses: vec![],
        profiles: chrome_extensions(&env.chrome_root),
        accounts: accounts
            .iter()
            .map(|a| AccountSkill {
                installed: skill_present(&a.dir),
                account: a.clone(),
            })
            .collect(),
    };
    let Some(bin) = bsk_path else {
        return snap;
    };
    let t = env.timeout;
    if let Ok((0, out)) = run_bsk(&bin, &["--version"], t) {
        snap.version = out
            .split_whitespace()
            .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(str::to_string);
    }
    snap.daemon = match run_bsk(&bin, &["status", "--json"], t) {
        Ok((0, out)) => match parse_status(&out) {
            Some(i) => Daemon::Running(i),
            None => Daemon::Unknown("status was not readable".into()),
        },
        Ok(_) => Daemon::Stopped,
        Err(RunErr::Timeout) => Daemon::Unresponsive,
        Err(RunErr::Spawn(e)) => Daemon::Unknown(e),
    };
    if let Daemon::Running(info) = &snap.daemon {
        snap.browsers = match run_bsk(&bin, &["browsers", "--json"], t) {
            Ok((0, out)) => parse_browsers(&out),
            _ => info.browsers.clone(),
        };
    }
    if let Ok((_, out)) = run_bsk(&bin, &["doctor"], t * 2) {
        let d = parse_doctor(&out);
        if !d.lines.is_empty() {
            snap.doctor = Some(d);
        }
    }
    if let Ok((0, out)) = run_bsk(&bin, &["install-skill", "--list", "--json"], t) {
        snap.harnesses = parse_harnesses(&out);
    }
    snap
}
