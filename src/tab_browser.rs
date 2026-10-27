use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};

use crate::app::{App, Modal};
use crate::browser::{self, Browser, ChromeProfile, Env, ProfileExt, EXTENSION_ID};
use crate::hits::UiAction;

pub const STORE_URL: &str =
    "https://chromewebstore.google.com/detail/hhcmgoofomhgciiibhipgmgkgnoenaoi";

pub const EXTENSIONS_URL: &str = "chrome://extensions";

const POLL_EVERY: Duration = Duration::from_secs(3);

const PENDING_FOR: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtState {
    Missing,
    Disabled,
    Enabled,
}

pub fn ext_state(p: &ProfileExt) -> ExtState {
    match (p.installed, p.enabled) {
        (_, Some(true)) => ExtState::Enabled,
        (_, Some(false)) | (true, None) => ExtState::Disabled,
        (false, None) => ExtState::Missing,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtnState {
    None,

    Chosen,

    NeedsEnable,

    Ready,
}

impl BtnState {
    pub fn word(self) -> &'static str {
        match self {
            BtnState::None => "none",
            BtnState::Chosen => "needs_install",
            BtnState::NeedsEnable => "needs_enable",
            BtnState::Ready => "ready",
        }
    }
}

pub fn button_state(bound: Option<&str>, profiles: &[ProfileExt]) -> BtnState {
    let Some(dir) = bound else {
        return BtnState::None;
    };
    match profiles.iter().find(|p| p.profile.dir == dir) {
        None => BtnState::Chosen,
        Some(p) => match ext_state(p) {
            ExtState::Enabled => BtnState::Ready,
            ExtState::Disabled => BtnState::NeedsEnable,
            ExtState::Missing => BtnState::Chosen,
        },
    }
}

pub fn button_label(state: BtnState, name: &str) -> String {
    let n = crate::sessions::snippet(name, 14);
    match state {
        BtnState::None => " browser ".into(),
        BtnState::Chosen => format!(" browser: {n} (add) "),
        BtnState::NeedsEnable => format!(" browser: {n} (turn on) "),
        BtnState::Ready => format!(" ✓ browser: {n} "),
    }
}

pub fn button_labels(state: BtnState, name: &str) -> Vec<String> {
    let mid = match state {
        BtnState::None => " browser ",
        BtnState::Chosen => " browser (add) ",
        BtnState::NeedsEnable => " browser (turn on) ",
        BtnState::Ready => " ✓ browser ",
    };
    let tiny = if state == BtnState::Ready {
        " ✓ browser "
    } else {
        " browser "
    };
    let mut v = vec![button_label(state, name), mid.to_string(), tiny.to_string()];
    v.dedup();
    v
}

pub fn compact_label(state: BtnState) -> &'static str {
    match state {
        BtnState::Ready => " ✓ web ",
        BtnState::None => " web ",
        BtnState::Chosen | BtnState::NeedsEnable => " web! ",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPlan {
    Store,
    Extensions,
}

impl OpenPlan {
    pub fn url(self) -> &'static str {
        match self {
            OpenPlan::Store => STORE_URL,
            OpenPlan::Extensions => EXTENSIONS_URL,
        }
    }
}

pub fn plan_for(s: ExtState) -> Option<OpenPlan> {
    match s {
        ExtState::Missing => Some(OpenPlan::Store),
        ExtState::Disabled => Some(OpenPlan::Extensions),
        ExtState::Enabled => None,
    }
}

pub type Links = std::collections::BTreeMap<String, String>;

pub fn linked_dir<'a>(links: &'a Links, label: &str) -> Option<&'a str> {
    let l = label.trim();
    if l.is_empty() {
        return None;
    }
    links
        .iter()
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(l))
        .map(|(_, v)| v.as_str())
}

fn label_names(label: &str, p: &ChromeProfile) -> bool {
    let same = |a: &str, b: &str| !b.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim());
    [&p.name, &p.dir, &p.user_name, &p.gaia_name]
        .iter()
        .any(|f| same(label, f))
}

pub fn match_browsers(
    all: &[Browser],
    p: &ChromeProfile,
    enabled_profiles: usize,
    this_enabled: bool,
    links: &Links,
) -> (Vec<Browser>, &'static str) {
    let linked = |b: &Browser| linked_dir(links, &b.label);
    let by_link: Vec<Browser> = all
        .iter()
        .filter(|b| linked(b) == Some(p.dir.as_str()))
        .cloned()
        .collect();
    let by_label: Vec<Browser> = all
        .iter()
        .filter(|b| linked(b).is_none() && label_names(&b.label, p))
        .cloned()
        .collect();
    if !by_link.is_empty() {
        let mut v = by_link;
        v.extend(by_label);
        return (v, "linked");
    }
    if !by_label.is_empty() {
        return (by_label, "label");
    }
    if all.len() == 1 && linked(&all[0]).is_none() && enabled_profiles == 1 && this_enabled {
        return (all.to_vec(), "only browser");
    }
    (vec![], "unknown")
}

pub fn assign_browsers(
    all: &[Browser],
    profiles: &[ProfileExt],
    links: &Links,
) -> Vec<(Browser, Option<String>, &'static str)> {
    let enabled: Vec<&ProfileExt> = profiles
        .iter()
        .filter(|p| ext_state(p) == ExtState::Enabled)
        .collect();
    all.iter()
        .map(|b| {
            if let Some(d) = linked_dir(links, &b.label) {
                return (b.clone(), Some(d.to_string()), "linked");
            }
            let named: Vec<&ProfileExt> = profiles
                .iter()
                .filter(|p| label_names(&b.label, &p.profile))
                .collect();
            match named.len() {
                1 => (b.clone(), Some(named[0].profile.dir.clone()), "label"),
                0 if all.len() == 1 && enabled.len() == 1 => (
                    b.clone(),
                    Some(enabled[0].profile.dir.clone()),
                    "only browser",
                ),
                0 => (b.clone(), None, "unknown"),
                _ => (b.clone(), None, "ambiguous"),
            }
        })
        .collect()
}

pub fn clean_also(primary: Option<&str>, also: &[String]) -> Vec<String> {
    let Some(primary) = primary else {
        return vec![];
    };
    let mut out: Vec<String> = vec![];
    for d in also {
        if valid_profile_dir(d) && d != primary && !out.contains(d) {
            out.push(d.clone());
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOp {
    Set(String),

    Add(String),

    Remove(String),

    Primary(String),

    Clear,
}

impl BindOp {
    pub fn parse(op: &str, dir: Option<&str>) -> Result<BindOp, String> {
        let need = || -> Result<String, String> {
            let d = dir.map(str::trim).filter(|d| !d.is_empty()).ok_or(format!(
                "{op} needs profile (a Chrome profile folder, e.g. \"Profile 1\")"
            ))?;
            if !valid_profile_dir(d) {
                return Err(format!("\"{d}\" is not a Chrome profile folder name"));
            }
            Ok(d.to_string())
        };
        Ok(match op {
            "set" | "use" => BindOp::Set(need()?),
            "add" => BindOp::Add(need()?),
            "remove" => BindOp::Remove(need()?),
            "primary" => BindOp::Primary(need()?),
            "clear" => BindOp::Clear,
            o => return Err(format!("unknown op \"{o}\"")),
        })
    }
}

pub fn apply_bind(primary: &mut Option<String>, also: &mut Vec<String>, op: &BindOp) -> bool {
    let before = (primary.clone(), also.clone());
    match op {
        BindOp::Set(d) => *primary = Some(d.clone()),
        BindOp::Add(d) => {
            if primary.is_none() {
                *primary = Some(d.clone());
            } else {
                also.push(d.clone());
            }
        }
        BindOp::Remove(d) => {
            if primary.as_deref() == Some(d.as_str()) {
                *primary = if also.is_empty() {
                    None
                } else {
                    Some(also.remove(0))
                };
            } else {
                also.retain(|x| x != d);
            }
        }
        BindOp::Primary(d) => {
            if primary.as_deref() != Some(d.as_str()) {
                if let Some(old) = primary.take() {
                    also.insert(0, old);
                }
                *primary = Some(d.clone());
            }
        }
        BindOp::Clear => {
            *primary = None;
            also.clear();
        }
    }
    *primary = primary.take().filter(|d| valid_profile_dir(d));
    *also = clean_also(primary.as_deref(), also);
    before != (primary.clone(), also.clone())
}

const BSK_CMDS: &[&str] = &[
    "navigate",
    "navigate-back",
    "navigate-forward",
    "reload",
    "click",
    "hover",
    "wheel",
    "scroll-to",
    "focus",
    "blur",
    "fill",
    "press",
    "select",
    "upload",
    "download",
    "evaluate",
    "screenshot",
    "snapshot",
    "observe",
    "console",
    "network",
    "get-html",
    "extract",
    "emulate",
    "session",
    "tab",
    "window",
    "browsers",
    "status",
    "doctor",
    "debug",
    "daemon",
    "logs",
];

const BSK_GROUPS: &[&str] = &["session", "tab", "window", "debug", "daemon"];

#[derive(Debug, Clone, PartialEq)]
pub struct BskAction {
    pub what: String,
    pub at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(crate) fn shell_words(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            None => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn short_arg(a: &str) -> String {
    let low = a.to_lowercase();
    let s = if low.starts_with("https://") || low.starts_with("http://") {
        let rest = &a[a.find("://").map_or(0, |i| i + 3)..];
        rest.strip_prefix("www.")
            .unwrap_or(rest)
            .trim_end_matches('/')
    } else {
        a
    };
    crate::sessions::snippet(s.trim(), 40)
}

pub fn bsk_summary(cmd: &str) -> Option<String> {
    let mut segs: Vec<&str> = vec![cmd];
    for sep in ["&&", "||", ";", "|", "\n"] {
        segs = segs.into_iter().flat_map(|s| s.split(sep)).collect();
    }
    segs.into_iter().rev().find_map(|seg| {
        let words = shell_words(seg);

        let mut it = words
            .iter()
            .skip_while(|w| w.contains('=') && !w.starts_with('-') && !w.contains('/'));
        let prog = it.next()?;
        if prog.rsplit('/').next() != Some("bsk") {
            return None;
        }
        let rest: Vec<&String> = it.collect();
        let at = rest.iter().position(|w| BSK_CMDS.contains(&w.as_str()))?;
        let mut what = rest[at].to_string();
        let mut args = rest[at + 1..].iter().filter(|w| !w.starts_with('-'));
        if BSK_GROUPS.contains(&what.as_str()) {
            if let Some(sub) = args.next() {
                what = format!("{what} {sub}");
            }
        }
        let args: Vec<&&String> = args.collect();

        let arg = args
            .iter()
            .find(|a| a.contains("://") || a.starts_with('@'))
            .or_else(|| args.first());
        if let Some(a) = arg {
            what = format!("{what} {}", short_arg(a));
        }
        Some(what)
    })
}

pub fn last_bsk_action(tail: &str) -> Option<BskAction> {
    tail.lines()
        .rev()
        .filter(|l| l.contains("bsk"))
        .find_map(|l| {
            let v: Value = serde_json::from_str(l).ok()?;
            if v.get("type").and_then(Value::as_str) != Some("assistant") {
                return None;
            }
            let at = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&chrono::Utc));
            let content = v.pointer("/message/content")?.as_array()?;
            content.iter().rev().find_map(|c| {
                if c.get("type").and_then(Value::as_str) != Some("tool_use") {
                    return None;
                }
                let cmd = c.pointer("/input/command")?.as_str()?;
                bsk_summary(cmd).map(|what| BskAction { what, at })
            })
        })
}

pub fn activity_words(a: &BskAction, now: chrono::DateTime<chrono::Utc>) -> String {
    match a.at {
        Some(t) => format!("{} · {} ago", a.what, crate::usage::ago_words(t, now)),
        None => a.what.clone(),
    }
}

pub(crate) fn read_tail(p: &std::path::Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(max);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = vec![];
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if from > 0 {
        text.split_once('\n')
            .map(|x| x.1.to_string())
            .unwrap_or_default()
    } else {
        text
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub prog: String,
    pub args: Vec<String>,
}

pub fn valid_profile_dir(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 64
        && d != "."
        && d != ".."
        && d.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

pub fn chrome_cmd(os: Os, url: &str, profile: Option<&str>) -> Option<Cmd> {
    if profile.is_some_and(|p| !valid_profile_dir(p)) || url.trim().is_empty() {
        return None;
    }
    let flag = profile.map(|p| format!("--profile-directory={p}"));
    let cmd = match os {
        Os::Mac => {
            let mut args = vec!["-na".into(), "Google Chrome".into(), "--args".into()];
            args.extend(flag);
            args.push(url.into());
            Cmd {
                prog: "open".into(),
                args,
            }
        }
        Os::Windows => {
            if url.contains(['&', '^', '|', '<', '>', '"', '%', '\n', '\r']) {
                return None;
            }
            let mut args: Vec<String> =
                vec!["/C".into(), "start".into(), "".into(), "chrome".into()];
            args.extend(flag);
            args.push(url.into());
            Cmd {
                prog: "cmd".into(),
                args,
            }
        }
        Os::Linux => {
            let mut args: Vec<String> = flag.into_iter().collect();
            args.push(url.into());
            Cmd {
                prog: "google-chrome".into(),
                args,
            }
        }
    };
    Some(cmd)
}

#[allow(dead_code)]
const LINUX_CHROME: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
];

#[cfg(test)]
pub static LAUNCHED: std::sync::Mutex<Vec<Cmd>> = std::sync::Mutex::new(Vec::new());

pub fn open_chrome(url: &str, profile: Option<&str>) -> bool {
    let Some(cmd) = chrome_cmd(Os::current(), url, profile) else {
        return false;
    };
    #[cfg(test)]
    {
        LAUNCHED.lock().unwrap_or_else(|e| e.into_inner()).push(cmd);
        return true;
    }
    #[cfg(not(test))]
    {
        if std::env::var_os("GODTERM_NO_OPEN").is_some() {
            return true;
        }

        let shown: Vec<String> = cmd
            .args
            .iter()
            .map(|a| {
                if a.contains("://") {
                    crate::admin::safe_target(a)
                } else {
                    a.clone()
                }
            })
            .collect();
        crate::log::info(&format!("browser: {} {}", cmd.prog, shown.join(" ")));
        let run = |prog: &str| {
            std::process::Command::new(prog)
                .args(&cmd.args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
        };
        if Os::current() == Os::Linux {
            LINUX_CHROME.iter().any(|p| run(p))
        } else {
            run(&cmd.prog)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Quick {
    pub profiles: Vec<ProfileExt>,

    pub browsers: Option<Vec<Browser>>,
}

pub fn quick(env: &Env, want_bsk: bool) -> Quick {
    let profiles = browser::chrome_extensions(&env.chrome_root);
    let browsers = if want_bsk {
        browser::find_bsk(&env.bin_dirs).and_then(|bin| {
            match browser::run_bsk(&bin, &["browsers", "--json"], env.timeout) {
                Ok((0, out)) => Some(browser::parse_browsers(&out)),
                _ => None,
            }
        })
    } else {
        None
    };
    Quick { profiles, browsers }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Pick,

    Confirm(OpenPlan),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrowserDlg {
    pub uid: u64,
    pub sel: usize,
    pub step: Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Dim,
    Good,
    Warn,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DlgLine {
    pub text: String,
    pub tone: Tone,
    pub action: Option<UiAction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DlgView {
    pub title: String,
    pub lines: Vec<DlgLine>,
    pub buttons: Vec<(String, UiAction, String)>,
}

fn line(text: impl Into<String>, tone: Tone) -> DlgLine {
    DlgLine {
        text: text.into(),
        tone,
        action: None,
    }
}

fn clean(dir: &str) -> Option<String> {
    valid_profile_dir(dir).then(|| dir.to_string())
}

impl App {
    fn tab_env(&self) -> Option<Env> {
        self.browser_env_pub()
    }

    pub fn browser_profiles(&self) -> Vec<ProfileExt> {
        if let Some(q) = self.browser.quick_last_result() {
            return q.profiles;
        }
        self.browser
            .snapshot()
            .map(|s| s.profiles)
            .unwrap_or_default()
    }

    pub fn browser_connected(&self) -> Option<Vec<Browser>> {
        if let Some(b) = self.browser.quick_last_result().and_then(|q| q.browsers) {
            return Some(b);
        }
        self.browser
            .snapshot()
            .and_then(|s| matches!(s.daemon, browser::Daemon::Running(_)).then_some(s.browsers))
    }

    pub fn tab_browser_dir(&self, uid: u64) -> Option<String> {
        let (s, t) = self.find_tab(uid)?;
        self.panes[s].tabs[t].browser.clone()
    }

    pub fn tab_browser_dirs(&self, uid: u64) -> Vec<String> {
        let Some((s, t)) = self.find_tab(uid) else {
            return vec![];
        };
        let tab = &self.panes[s].tabs[t];
        tab.browser
            .iter()
            .chain(tab.browser_also.iter())
            .cloned()
            .collect()
    }

    pub fn profile_of(&self, dir: &str) -> ChromeProfile {
        self.browser_profiles()
            .into_iter()
            .find(|p| p.profile.dir == dir)
            .map(|p| p.profile)
            .unwrap_or(ChromeProfile {
                dir: dir.to_string(),
                name: dir.to_string(),
                ..Default::default()
            })
    }

    pub fn tab_browser_profile(&self, uid: u64) -> Option<ChromeProfile> {
        let dir = self.tab_browser_dir(uid)?;
        Some(
            self.browser_profiles()
                .into_iter()
                .find(|p| p.profile.dir == dir)
                .map(|p| p.profile)
                .unwrap_or(ChromeProfile {
                    dir: dir.clone(),
                    name: dir,
                    ..Default::default()
                }),
        )
    }

    pub fn tab_browser_state(&self, uid: u64) -> BtnState {
        button_state(
            self.tab_browser_dir(uid).as_deref(),
            &self.browser_profiles(),
        )
    }

    pub fn browser_button(&self, s: usize) -> (BtnState, Vec<String>) {
        let uid = self.panes[s].cur().uid;
        let st = self.tab_browser_state(uid);
        let name = self
            .tab_browser_profile(uid)
            .map(|p| if p.name.is_empty() { p.dir } else { p.name })
            .unwrap_or_default();
        (st, button_labels(st, &name))
    }

    pub fn tab_browser_json(&self, s: usize, t: usize) -> Option<Value> {
        let tab = &self.panes[s].tabs[t];
        tab.browser.as_ref()?;
        let p = self.tab_browser_profile(tab.uid)?;
        let mut v = json!({
            "profile_dir": p.dir, "profile": p.name, "email": p.user_name,
            "state": self.tab_browser_state(tab.uid).word(),
        });
        if !tab.browser_also.is_empty() {
            let profiles = self.browser_profiles();
            v["also"] = json!(tab
                .browser_also
                .iter()
                .map(|d| {
                    let q = self.profile_of(d);
                    json!({"profile_dir": d, "profile": q.name,
                        "state": button_state(Some(d), &profiles).word()})
                })
                .collect::<Vec<_>>());
        }
        if let Some(a) = self.tab_browser_activity(tab.uid) {
            v["last_action"] = json!(activity_words(&a, chrono::Utc::now()));
        }
        Some(v)
    }

    pub fn tab_browser_activity(&self, uid: u64) -> Option<BskAction> {
        let (s, t) = self.find_tab(uid)?;
        self.panes[s].tabs[t].browser.as_ref()?;
        let mut cache = self.browser.activity.borrow_mut();
        if let Some((at, v)) = cache.get(&uid) {
            if at.elapsed() < Duration::from_secs(3) {
                return v.clone();
            }
        }
        let v = self
            .transcript_of(s, t)
            .and_then(|(p, _)| read_tail(&p, 256 * 1024))
            .and_then(|tail| last_bsk_action(&tail));
        cache.insert(uid, (Instant::now(), v.clone()));
        v
    }

    pub fn open_tab_chrome(&mut self, uid: u64) {
        let Some(dir) = self.tab_browser_dir(uid) else {
            if let Some((s, _)) = self.find_tab(uid) {
                self.open_browser_dialog(s);
            }
            return;
        };
        let who = self.profile_name(&dir).unwrap_or_else(|| dir.clone());
        let name = self
            .find_tab(uid)
            .map(|(s, t)| self.panes[s].tabs[t].name())
            .unwrap_or_default();
        if open_chrome("chrome://newtab", Some(&dir)) {
            self.flash(format!("Opened Chrome profile {who} for {name}"));
        } else {
            self.flash("Could not open Chrome");
        }
    }

    pub fn bind_tab_browser(&mut self, uid: u64, dir: Option<String>) -> bool {
        let op = match dir {
            Some(d) => match clean(&d) {
                Some(d) => BindOp::Set(d),
                None => return false,
            },
            None => BindOp::Clear,
        };
        self.bind_tab_browsers(uid, &op)
    }

    pub fn bind_tab_browsers(&mut self, uid: u64, op: &BindOp) -> bool {
        let Some((s, t)) = self.find_tab(uid) else {
            return false;
        };
        let tab = &mut self.panes[s].tabs[t];
        let (mut p, mut a) = (tab.browser.clone(), tab.browser_also.clone());
        if !apply_bind(&mut p, &mut a, op) {
            return false;
        }
        tab.browser = p;
        tab.browser_also = a;
        if tab.browser.is_none() {
            self.browser.pending.retain(|(u, _)| *u != uid);
        }
        self.state_dirty = true;
        true
    }

    pub fn link_browser_label(&mut self, label: &str, dir: Option<&str>) -> Result<bool, String> {
        let label = label.trim();
        if label.is_empty() {
            return Err("this browser has no name in its extension popup to link; set its Browser name there first".into());
        }
        if label.chars().count() > 120 || label.chars().any(char::is_control) {
            return Err("that browser name is too long or has control characters".into());
        }
        let dir = match dir.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => {
                Some(clean(d).ok_or(format!("\"{d}\" is not a Chrome profile folder name"))?)
            }
            None => None,
        };
        let mut links = self.cfg.browser.links.clone();
        links.retain(|k, _| !k.trim().eq_ignore_ascii_case(label));
        if let Some(d) = &dir {
            links.insert(label.to_string(), d.clone());
        }
        if links == self.cfg.browser.links {
            return Ok(false);
        }
        let item = (!links.is_empty()).then(|| {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in &links {
                t.insert(k, v.as_str().into());
            }
            toml_edit::Item::Value(toml_edit::Value::InlineTable(t))
        });
        crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("browser.links"),
            item,
        )
        .map_err(|e| format!("could not write config.toml: {e:#}"))?;
        self.cfg.browser.links = links;
        self.config_mtime = crate::app::config_mtime();
        Ok(true)
    }

    pub fn set_default_profile(&mut self, dir: &str) {
        let dir = clean(dir).unwrap_or_default();
        if self.cfg.browser.default_profile == dir {
            return;
        }
        self.cfg.browser.default_profile = dir.clone();
        let item = (!dir.is_empty()).then(|| toml_edit::value(dir.as_str()));
        if let Err(e) = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("browser.default_profile"),
            item,
        ) {
            crate::log::error(&format!("browser.default_profile: {e:#}"));
        }
        self.config_mtime = crate::app::config_mtime();
    }

    pub fn browser_default_cycle(&mut self) {
        let mut dirs: Vec<String> = vec![String::new()];
        dirs.extend(self.browser_profiles().into_iter().map(|p| p.profile.dir));
        let cur = &self.cfg.browser.default_profile;
        let i = dirs.iter().position(|d| d == cur).unwrap_or(0);
        let next = dirs[(i + 1) % dirs.len()].clone();
        self.set_default_profile(&next);
        let say = match self.profile_name(&next) {
            Some(n) => format!("Links open in Chrome profile {n}"),
            None => "Links open in the system browser".to_string(),
        };
        self.flash(say);
    }

    pub fn profile_name(&self, dir: &str) -> Option<String> {
        if dir.is_empty() {
            return None;
        }
        Some(
            self.browser_profiles()
                .into_iter()
                .find(|p| p.profile.dir == dir)
                .map(|p| p.profile.display())
                .unwrap_or_else(|| dir.to_string()),
        )
    }

    pub fn link_profile(&self, tab_uid: Option<u64>) -> Option<String> {
        let own = tab_uid.and_then(|u| self.tab_browser_dir(u));
        let dir = own
            .or_else(|| Some(self.cfg.browser.default_profile.clone()))
            .and_then(|d| clean(&d))?;
        match self.tab_env() {
            Some(env) => {
                let text = std::fs::read_to_string(env.chrome_root.join("Local State"))
                    .unwrap_or_default();
                browser::parse_local_state(&text)
                    .iter()
                    .any(|p| p.dir == dir)
                    .then_some(dir)
            }
            None => Some(dir),
        }
    }

    pub fn open_link_in_chrome(&self, url: &str, tab_uid: Option<u64>) -> bool {
        match self.link_profile(tab_uid) {
            Some(p) => open_chrome(url, Some(&p)),
            None => false,
        }
    }

    pub fn open_service_link(&self, url: &str) {
        let low = url.trim().to_lowercase();
        let web = low.starts_with("https://") || low.starts_with("http://");
        if web && self.cfg.browser.login_links && self.open_link_in_chrome(url, None) {
            return;
        }
        crate::admin::open_url(url);
    }

    pub fn toggle_login_links(&mut self) {
        let on = !self.cfg.browser.login_links;
        self.cfg.browser.login_links = on;
        if let Err(e) = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("browser.login_links"),
            Some(toml_edit::value(on)),
        ) {
            crate::log::error(&format!("browser.login_links: {e:#}"));
        }
        self.config_mtime = crate::app::config_mtime();
        self.flash(if on {
            "Sign in and Remote Control links open in the default Chrome profile"
        } else {
            "Sign in and Remote Control links open in the system browser"
        });
    }

    pub fn open_browser_dialog(&mut self, s: usize) {
        let Some(slot) = self.panes.get(s) else {
            return;
        };
        if let Some(why) = self.pane_harness(s).and_then(|h| h.no_tab_tools_reason()) {
            self.flash(why);
            return;
        }
        let uid = slot.cur().uid;
        self.focus = s;
        self.browser.note = None;
        let profiles = self.browser_profiles();
        let want = self
            .tab_browser_dir(uid)
            .or_else(|| Some(self.cfg.browser.default_profile.clone()));
        let sel = want
            .and_then(|d| profiles.iter().position(|p| p.profile.dir == d))
            .unwrap_or(0);
        self.modal = Modal::Browser(BrowserDlg {
            uid,
            sel,
            step: Step::Pick,
        });
        self.browser_quick(true);
        self.browser_refresh(false);
    }

    pub fn browser_quick(&mut self, force: bool) {
        let Some(env) = self.tab_env() else {
            return;
        };
        if self.browser.quick_busy.load(Ordering::SeqCst) {
            return;
        }
        if !force
            && self
                .browser
                .quick_at
                .is_some_and(|t| t.elapsed() < POLL_EVERY)
        {
            return;
        }
        self.browser.quick_at = Some(Instant::now());
        self.browser.quick_busy.store(true, Ordering::SeqCst);

        let want_bsk = self
            .browser_profiles()
            .iter()
            .any(|p| ext_state(p) == ExtState::Enabled);
        let (slot, flag) = (self.browser.quick.clone(), self.browser.quick_busy.clone());
        std::thread::spawn(move || {
            let q = quick(&env, want_bsk);
            if let Ok(mut g) = slot.lock() {
                *g = Some(q);
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    fn dialog_open(&self) -> bool {
        matches!(self.modal, Modal::Browser(_))
    }

    pub fn browser_poll_tick(&mut self) {
        let waiting = !self.browser.pending.is_empty();

        let bound = self
            .panes
            .iter()
            .any(|p| p.tabs.iter().any(|t| t.browser.is_some()));
        if bound
            && !waiting
            && !self.dialog_open()
            && self
                .browser
                .quick_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(60))
        {
            self.browser_quick(true);
        }
        if !self.dialog_open() && !waiting {
            return;
        }
        self.browser_quick(false);
        let pending = std::mem::take(&mut self.browser.pending);
        for (uid, since) in pending {
            let Some((s, t)) = self.find_tab(uid) else {
                continue;
            };
            if self.panes[s].tabs[t].browser.is_none() || since.elapsed() > PENDING_FOR {
                continue;
            }
            if self.tab_browser_state(uid) != BtnState::Ready {
                self.browser.pending.push((uid, since));
                continue;
            }
            let name = self.panes[s].tabs[t].name();
            let who = self
                .tab_browser_profile(uid)
                .map(|p| p.display())
                .unwrap_or_default();
            let more = match self.browser_connected() {
                Some(b) if !b.is_empty() => "",
                _ => " Open the extension popup and turn on the local connection.",
            };
            self.browser.note = Some(format!("Ready: the extension is on in {who}.{more}"));
            self.flash(format!(
                "Browser ready for {name}: Chrome profile {who}.{more}"
            ));
        }
    }

    pub fn on_browser_key(&mut self, k: KeyEvent) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        match d.step {
            Step::Confirm(plan) => match k.code {
                KeyCode::Enter | KeyCode::Char('y') => self.browser_open_run(plan),
                _ => self.modal = Modal::None,
            },
            Step::Pick => {
                let n = self.browser_profiles().len();
                let set = |app: &mut App, sel: usize| {
                    app.modal = Modal::Browser(BrowserDlg { sel, ..d.clone() })
                };
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Up | KeyCode::Char('k') => set(self, d.sel.saturating_sub(1)),
                    KeyCode::Down | KeyCode::Char('j') => {
                        set(self, (d.sel + 1).min(n.saturating_sub(1)))
                    }
                    KeyCode::Enter => self.browser_choose(d.sel),
                    KeyCode::Char('a') => self.browser_toggle_also(d.sel),
                    KeyCode::Char('c') => self.browser_clear(),
                    KeyCode::Char('r') => self.browser_quick(true),
                    KeyCode::Char('o') => self.open_tab_chrome(d.uid),
                    _ => {}
                }
            }
        }
    }

    pub fn browser_choose(&mut self, i: usize) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        let profiles = self.browser_profiles();
        let Some(p) = profiles.get(i) else {
            return;
        };
        let dir = p.profile.dir.clone();
        let who = p.profile.display();
        self.bind_tab_browser(d.uid, Some(dir.clone()));
        self.set_default_profile(&dir);
        self.browser.note = None;
        match plan_for(ext_state(p)) {
            None => {
                self.modal = Modal::Browser(BrowserDlg {
                    sel: i,
                    step: Step::Pick,
                    ..d
                });
                self.browser.note = Some(match self.browser_connected() {
                    Some(b) if !b.is_empty() => format!("Ready: this tab uses {who}."),
                    _ => format!(
                        "This tab uses {who}. Open the extension popup there and turn on the local connection."
                    ),
                });
                self.browser_quick(true);
                self.flash(format!("Browser for this tab: Chrome profile {who}"));
            }
            Some(plan) => {
                self.modal = Modal::Browser(BrowserDlg {
                    sel: i,
                    step: Step::Confirm(plan),
                    ..d
                });
            }
        }
    }

    pub fn browser_toggle_also(&mut self, i: usize) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        let Some(p) = self.browser_profiles().get(i).cloned() else {
            return;
        };
        let dir = p.profile.dir.clone();
        let who = p.profile.display();
        let had = self.tab_browser_dirs(d.uid).contains(&dir);
        let op = if had {
            BindOp::Remove(dir)
        } else {
            BindOp::Add(dir)
        };
        self.modal = Modal::Browser(BrowserDlg {
            sel: i,
            ..d.clone()
        });
        if self.bind_tab_browsers(d.uid, &op) {
            let say = if had {
                format!("This tab no longer uses {who}.")
            } else {
                format!("This tab also uses {who}.")
            };
            self.browser.note = Some(say.clone());
            self.flash(say);
            self.browser_quick(true);
        }
    }

    pub fn browser_clear(&mut self) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        if self.bind_tab_browser(d.uid, None) {
            self.browser.pending.retain(|(u, _)| *u != d.uid);
            self.browser.note = Some("This tab has no browser now.".into());
            self.flash("Browser cleared for this tab");
        }
    }

    pub fn browser_open_run(&mut self, plan: OpenPlan) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        let Some(dir) = self.tab_browser_dir(d.uid) else {
            self.modal = Modal::None;
            return;
        };
        let who = self.profile_name(&dir).unwrap_or_else(|| dir.clone());
        let ok = open_chrome(plan.url(), Some(&dir));
        self.modal = Modal::Browser(BrowserDlg {
            step: Step::Pick,
            ..d.clone()
        });
        if !ok {
            self.browser.note = Some("Could not open Chrome.".into());
            return;
        }
        self.browser.pending.retain(|(u, _)| *u != d.uid);
        self.browser.pending.push((d.uid, Instant::now()));
        self.browser.note = Some(match plan {
            OpenPlan::Store => format!(
                "Opened the Chrome Web Store in {who}. Press Add to Chrome there (Chrome needs your click), then turn the extension on. GodTerm looks every few seconds."
            ),
            OpenPlan::Extensions => format!(
                "Opened chrome://extensions in {who}. Turn BrowserSkill on there. GodTerm looks every few seconds."
            ),
        });
        self.browser_quick(true);
    }

    pub fn browser_dialog_view(&self, d: &BrowserDlg) -> Option<DlgView> {
        let (s, t) = self.find_tab(d.uid)?;
        let tab = self.panes[s].tabs[t].name();
        let title = format!("browser for {}", crate::sessions::snippet(&tab, 30));
        let who = self
            .tab_browser_dir(d.uid)
            .and_then(|dir| self.profile_name(&dir));
        if let Step::Confirm(plan) = d.step {
            let who = who.unwrap_or_default();
            let mut lines = match plan {
                OpenPlan::Store => vec![
                    line(format!("Open the Chrome Web Store page of BrowserSkill in {who}?"), Tone::Normal),
                    line("Chrome makes you press Add to Chrome yourself; GodTerm cannot do that for you.", Tone::Warn),
                ],
                OpenPlan::Extensions => vec![
                    line(format!("Open chrome://extensions in {who}?"), Tone::Normal),
                    line("Turn BrowserSkill on there; GodTerm cannot do that for you.", Tone::Warn),
                ],
            };
            lines.push(line(
                "Then open the extension's popup and turn on its local connection. GodTerm looks every few seconds and marks this tab ready.",
                Tone::Dim,
            ));
            return Some(DlgView {
                title,
                lines,
                buttons: vec![
                    (
                        "Open Chrome".into(),
                        UiAction::ModalOk,
                        "Open it in that profile (Enter)".into(),
                    ),
                    (
                        "Not now".into(),
                        UiAction::ModalCancel,
                        "The profile stays chosen (Esc)".into(),
                    ),
                ],
            });
        }
        let profiles = self.browser_profiles();
        let mut lines = vec![line(
            "Chrome profile for this tab (a: also use another). Links open in the first.",
            Tone::Dim,
        )];
        if profiles.is_empty() {
            lines.push(line(
                "No Chrome profiles found. Is Chrome installed and used once?",
                Tone::Warn,
            ));
        }
        let bound = self.tab_browser_dir(d.uid);
        let dirs = self.tab_browser_dirs(d.uid);
        let many = dirs.len() > 1;
        for (i, p) in profiles.iter().enumerate() {
            let this = bound.as_deref() == Some(p.profile.dir.as_str());
            let also = !this && dirs.contains(&p.profile.dir);
            let text = format!(
                "{} {:<24} {}{}",
                if i == d.sel { "›" } else { " " },
                crate::sessions::snippet(&p.profile.display(), 24),
                p.word(),
                match (this, also, many) {
                    (true, _, true) => "  (this tab, primary)",
                    (true, _, false) => "  (this tab)",
                    (_, true, _) => "  (this tab too)",
                    _ => "",
                }
            );
            lines.push(DlgLine {
                text,
                tone: match ext_state(p) {
                    ExtState::Enabled => Tone::Good,
                    ExtState::Disabled => Tone::Warn,
                    ExtState::Missing => Tone::Normal,
                },
                action: Some(UiAction::BrowserRow(i)),
            });
        }
        let st = self.tab_browser_state(d.uid);
        let w = who.unwrap_or_default();
        let (msg, tone) = match st {
            BtnState::None => ("Pick a profile.".to_string(), Tone::Dim),
            BtnState::Chosen => (format!("BrowserSkill is not in {w} yet."), Tone::Warn),
            BtnState::NeedsEnable => (
                format!("BrowserSkill is in {w} but turned off."),
                Tone::Warn,
            ),
            BtnState::Ready => (format!("Ready: this tab's agent can use {w}."), Tone::Good),
        };
        lines.push(line("", Tone::Dim));
        lines.push(line(msg, tone));
        if many {
            let more: Vec<String> = dirs[1..]
                .iter()
                .map(|d| self.profile_name(d).unwrap_or_else(|| d.clone()))
                .collect();
            lines.push(line(
                format!(
                    "Also: {}. Its agent uses the primary one unless asked for another.",
                    more.join(", ")
                ),
                Tone::Normal,
            ));
        }
        if st == BtnState::Ready {
            let conn = match self.browser_connected() {
                Some(b) if !b.is_empty() => format!("bsk sees {} connected.", crate::control::plural(b.len(), "browser")),
                Some(_) => "bsk sees no connected browser: turn on the local connection in the extension popup.".to_string(),
                None => "bsk is not running or not installed (Settings > Browser).".to_string(),
            };
            lines.push(line(conn, Tone::Dim));
            lines.push(line(
                "Tip: set the extension's Browser name to the profile name so agents can tell browsers apart.",
                Tone::Dim,
            ));
        }
        if bound.is_some() {
            lines.push(match self.tab_browser_activity(d.uid) {
                Some(a) => line(
                    format!(
                        "Last in the browser: {}",
                        activity_words(&a, chrono::Utc::now())
                    ),
                    Tone::Normal,
                ),
                None => line(
                    "Its agent has not used the browser in this session.",
                    Tone::Dim,
                ),
            });
        }
        if let Some(n) = &self.browser.note {
            lines.push(line(n.clone(), Tone::Normal));
        }
        let sel_in = profiles
            .get(d.sel)
            .is_some_and(|p| dirs.contains(&p.profile.dir));
        let mut buttons = vec![
            (
                "Use profile".into(),
                UiAction::ModalOk,
                "Choose the selected profile for this tab (Enter)".into(),
            ),
            (
                if sel_in { "Remove" } else { "Also use" }.into(),
                UiAction::BrowserAlso,
                if sel_in {
                    "Take the selected profile off this tab (a)"
                } else {
                    "Add the selected profile to this tab's browsers (a)"
                }
                .into(),
            ),
        ];
        if bound.is_some() {
            buttons.push((
                "Open Chrome".into(),
                UiAction::BrowserOpenTab,
                "Bring up this tab's Chrome profile (o)".into(),
            ));
        }
        buttons.push((
            "No browser".into(),
            UiAction::BrowserClear,
            "Clear this tab's browser (c)".into(),
        ));
        buttons.push(("Close".into(), UiAction::ModalCancel, "Esc".into()));
        Some(DlgView {
            title,
            lines,
            buttons,
        })
    }

    pub(crate) fn browser_tool_for(&mut self, uid: u64, tool: &str) -> Result<Value, String> {
        let base = self.browser_tool(tool)?;
        let dirs = self.tab_browser_dirs(uid);
        if dirs.is_empty() {
            let mut v = base;
            if v["result"].is_object() {
                v["result"]["bound_profile"] = Value::Null;
                v["result"]["bound_profiles"] = json!([]);
                v["result"]["say"] = json!("No browser is set for this tab. Ask the user to choose one: Browser on this tab's bottom bar (or Ctrl-a i), or the Browser section of this tab in the web app. Do not pick one yourself.");
            }
            return Ok(v);
        }
        let Some(snap) = self.browser.snapshot() else {
            return Ok(base);
        };
        let profiles = self.browser_profiles();
        let profiles = if profiles.is_empty() {
            snap.profiles.clone()
        } else {
            profiles
        };
        let enabled = profiles
            .iter()
            .filter(|p| ext_state(p) == ExtState::Enabled)
            .count();
        let all = self
            .browser_connected()
            .unwrap_or_else(|| snap.browsers.clone());
        let links = self.cfg.browser.links.clone();
        struct One<'a> {
            profile: ChromeProfile,
            ext: Option<&'a ProfileExt>,
            state: BtnState,
            matched: Vec<Browser>,
            how: &'static str,
        }
        let ones: Vec<One> = dirs
            .iter()
            .map(|dir| {
                let ext = profiles.iter().find(|p| &p.profile.dir == dir);
                let profile = ext.map(|p| p.profile.clone()).unwrap_or(ChromeProfile {
                    dir: dir.clone(),
                    name: dir.clone(),
                    ..Default::default()
                });
                let state = button_state(Some(dir), &profiles);
                let (matched, how) =
                    match_browsers(&all, &profile, enabled, state == BtnState::Ready, &links);
                One {
                    profile,
                    ext,
                    state,
                    matched,
                    how,
                }
            })
            .collect();
        let entry = |o: &One, primary: bool| {
            json!({
                "profile_dir": o.profile.dir, "name": o.profile.name, "email": o.profile.user_name,
                "extension": o.ext.map(ProfileExt::word).unwrap_or("not found"),
                "state": o.state.word(),
                "primary": primary,
                "browsers": o.matched.iter().map(|b| b.instance_id.clone()).collect::<Vec<_>>(),
                "match": o.how,
            })
        };
        let bound_profiles: Vec<Value> = ones
            .iter()
            .enumerate()
            .map(|(i, o)| entry(o, i == 0))
            .collect();

        let mut browsers: Vec<Value> = vec![];
        let mut seen: Vec<String> = vec![];
        for (i, o) in ones.iter().enumerate() {
            for b in &o.matched {
                if seen.contains(&b.instance_id) {
                    continue;
                }
                seen.push(b.instance_id.clone());
                let mut v = b.to_json();
                v["profile_dir"] = json!(o.profile.dir);
                v["profile"] = json!(o.profile.name);
                v["primary"] = json!(i == 0);
                browsers.push(v);
            }
        }
        let say = if ones.len() == 1 {
            one_say(
                ones[0].state,
                &ones[0].matched,
                all.is_empty(),
                &ones[0].profile.name,
            )
        } else {
            let mut use_: Vec<String> = vec![];
            let mut problems: Vec<String> = vec![];
            for (i, o) in ones.iter().enumerate() {
                let who = if i == 0 {
                    format!("{} (primary)", o.profile.name)
                } else {
                    o.profile.name.clone()
                };
                match (o.state, o.matched.first()) {
                    (BtnState::Ready, Some(b)) => {
                        use_.push(format!("{who}: `bsk session start --browser {}`", b.instance_id))
                    }
                    (BtnState::Ready, None) if all.is_empty() => problems.push(format!(
                        "{who} is not connected (the user turns on the local connection in its extension popup)"
                    )),
                    (BtnState::Ready, None) => problems.push(format!(
                        "bsk cannot tell which connected browser is {who}: ask the user to link it in this tab's Browser section, or to set the extension's Browser name to \"{}\"",
                        o.profile.name
                    )),
                    _ => problems.push(format!(
                        "{who} is not ready (the extension is missing or off there; the user finishes in this tab's Browser dialog)"
                    )),
                }
            }
            let mut say = format!(
                "This tab has {} browsers (Chrome profiles); use only these and no other. ",
                ones.len()
            );
            if !use_.is_empty() {
                say += &format!("{}. ", use_.join("; "));
            }
            say += "Use the primary one unless the request is clearly about another one's account or site; when it is unclear which, ask the user.";
            if !problems.is_empty() {
                say += &format!(" Not usable now: {}.", problems.join("; "));
            }
            say
        };
        let first = &ones[0];
        let bound = bound_profiles[0].clone();
        let mut result = if tool == "list_browsers" {
            json!({
                "daemon": snap.daemon.word(),
                "bound_profile": bound,
                "bound_profiles": bound_profiles,
                "browsers": browsers,
                "count": browsers.len(),
                "match": first.how,
                "checked_secs_ago": snap.age_secs(),
            })
        } else {
            let mut v = snap.to_json();
            v["chrome_profiles"] = json!(ones
                .iter()
                .filter_map(|o| o.ext.map(ProfileExt::to_json))
                .collect::<Vec<_>>());
            v["browsers"] = json!(browsers);
            v["bound_profile"] = bound;
            v["bound_profiles"] = json!(bound_profiles);
            v["match"] = json!(first.how);
            v
        };
        result["say"] = json!(say);
        result["extension_id"] = json!(EXTENSION_ID);
        Ok(json!({"ok": true, "result": result}))
    }

    pub(crate) fn open_desktop_tool(&mut self) -> Result<Value, String> {
        if !self.web_caller {
            return Err("open_desktop is the user's own button in the web app".into());
        }
        if !cfg!(target_os = "macos") {
            return Err("only on a Mac".into());
        }
        let r = std::process::Command::new("open")
            .args(["-a", "GodTerm"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match r {
            Ok(st) if st.success() => Ok(json!({"ok": true, "say": "GodTerm is at the front on the Mac."})),
            Ok(_) => Err("the GodTerm app was not found in Applications (this GodTerm runs from a terminal?)".into()),
            Err(e) => Err(format!("open failed: {e}")),
        }
    }

    pub(crate) fn tab_browser_tool(&mut self, args: &Value) -> Result<Value, String> {
        if !self.web_caller {
            return Err("tab_browser is the user's own buttons in the web app's Browser section; an agent never picks a browser: ask the user to choose one there".into());
        }
        let (s, t) = self.one_tab(args)?;
        let uid = self.panes[s].tabs[t].uid;
        let op = args["op"].as_str().unwrap_or("view").trim().to_lowercase();
        let dir = args["profile"].as_str();
        let tab = self.panes[s].tabs[t].name();
        let say = match op.as_str() {
            "view" => return Ok(json!({"ok": true, "result": self.tab_browser_view(uid)})),
            "open" => {
                let dir = match dir.map(str::trim).filter(|d| !d.is_empty()) {
                    Some(d) => clean(d).ok_or("not a Chrome profile folder name")?,
                    None => self
                        .tab_browser_dir(uid)
                        .ok_or("this tab has no browser yet: choose a Chrome profile first")?,
                };
                if !self.tab_browser_dirs(uid).contains(&dir) {
                    return Err("that profile is not one of this tab's browsers".into());
                }
                let who = self.profile_name(&dir).unwrap_or_else(|| dir.clone());
                if !open_chrome("chrome://newtab", Some(&dir)) {
                    return Err("could not open Chrome".into());
                }
                format!("Opened Chrome profile {who} for {tab}")
            }
            _ => {
                let op = BindOp::parse(&op, dir)?;
                let changed = self.bind_tab_browsers(uid, &op);

                if let BindOp::Set(d) | BindOp::Primary(d) = &op {
                    self.set_default_profile(d);
                }
                if changed {
                    self.browser_quick(true);
                    self.browser_refresh(false);
                }
                let name = |d: &str| self.profile_name(d).unwrap_or_else(|| d.to_string());
                match (&op, changed) {
                    (_, false) => "Nothing changed".to_string(),
                    (BindOp::Set(d), _) => format!("{tab} uses {}", name(d)),
                    (BindOp::Add(d), _) => format!("{tab} also uses {}", name(d)),
                    (BindOp::Remove(d), _) => format!("{tab} no longer uses {}", name(d)),
                    (BindOp::Primary(d), _) => {
                        format!("{} is now {tab}'s primary browser", name(d))
                    }
                    (BindOp::Clear, _) => format!("{tab} has no browser now"),
                }
            }
        };
        Ok(json!({"ok": true, "result": {"say": say, "view": self.tab_browser_view(uid)}}))
    }

    pub(crate) fn link_browser_tool(&mut self, args: &Value) -> Result<Value, String> {
        if !self.web_caller {
            return Err("link_browser is the user's own button in the web app's Browser section; ask the user which profile a browser is".into());
        }
        let label = args["label"].as_str().unwrap_or("");
        let dir = args["profile"].as_str();
        let changed = self.link_browser_label(label, dir)?;
        let label = label.trim();
        let say = match (dir.filter(|d| !d.trim().is_empty()), changed) {
            (_, false) => "Nothing changed".to_string(),
            (Some(d), _) => format!(
                "\"{label}\" is {}",
                self.profile_name(d.trim()).unwrap_or_else(|| d.to_string())
            ),
            (None, _) => format!("\"{label}\" is not linked to a profile now"),
        };
        let mut r = json!({"say": say, "links": self.cfg.browser.links});
        if let Ok((s, t)) = self.one_tab(args) {
            r["view"] = self.tab_browser_view(self.panes[s].tabs[t].uid);
        }
        Ok(json!({"ok": true, "result": r}))
    }

    pub fn tab_browser_view(&mut self, uid: u64) -> Value {
        let snap = self.browser.snapshot();
        if snap
            .as_ref()
            .is_none_or(|s| Duration::from_secs(s.age_secs()) > Duration::from_secs(60))
        {
            self.browser_refresh(snap.is_none());
        }

        let first = self.browser.quick_last_result().is_none();
        self.browser_quick(first);
        let mut profiles = self.browser_profiles();
        if profiles.is_empty() {
            if let Some(s) = &snap {
                profiles = s.profiles.clone();
            }
        }
        let dirs = self.tab_browser_dirs(uid);
        let links = self.cfg.browser.links.clone();
        let all = self
            .browser_connected()
            .or_else(|| snap.as_ref().map(|s| s.browsers.clone()))
            .unwrap_or_default();
        let assigned = assign_browsers(&all, &profiles, &links);
        let name_of = |d: &str| {
            profiles
                .iter()
                .find(|p| p.profile.dir == d)
                .map(|p| p.profile.name.clone())
                .unwrap_or_else(|| d.to_string())
        };
        let enabled = profiles
            .iter()
            .filter(|p| ext_state(p) == ExtState::Enabled)
            .count();
        let bound: Vec<Value> = dirs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let ext = profiles.iter().find(|p| &p.profile.dir == d);
                let prof = ext.map(|p| p.profile.clone()).unwrap_or(ChromeProfile {
                    dir: d.clone(),
                    name: d.clone(),
                    ..Default::default()
                });
                let state = button_state(Some(d), &profiles);
                let (m, how) =
                    match_browsers(&all, &prof, enabled, state == BtnState::Ready, &links);
                json!({
                    "profile_dir": d, "name": prof.name, "email": prof.user_name,
                    "extension": ext.map(ProfileExt::word).unwrap_or("not found"),
                    "state": state.word(), "primary": i == 0,
                    "browsers": m.iter().map(|b| b.instance_id.clone()).collect::<Vec<_>>(),
                    "match": how,
                })
            })
            .collect();
        let profiles_json: Vec<Value> = profiles
            .iter()
            .map(|p| {
                let mut v = p.to_json();
                v["state"] = json!(match ext_state(p) {
                    ExtState::Enabled => "on",
                    ExtState::Disabled => "off",
                    ExtState::Missing => "missing",
                });
                v["bound"] = json!(dirs.contains(&p.profile.dir));
                v["primary"] = json!(dirs.first() == Some(&p.profile.dir));
                v
            })
            .collect();
        let browsers: Vec<Value> = assigned
            .iter()
            .map(|(b, d, how)| {
                let mut v = b.to_json();
                v["profile_dir"] = json!(d);
                v["profile"] = json!(d.as_deref().map(name_of));
                v["match"] = json!(how);
                v["this_tab"] = json!(d.as_ref().is_some_and(|d| dirs.contains(d)));
                v
            })
            .collect();
        let unmatched = assigned.iter().filter(|x| x.1.is_none()).count();
        let mut v = snap
            .as_ref()
            .map(|s| s.to_json())
            .unwrap_or(json!({"state": "checking"}));
        v["tab"] = json!(crate::control::tab_id(uid));
        v["bound"] = json!(bound);
        v["chrome_profiles"] = json!(profiles_json);
        v["browsers"] = json!(browsers);
        v["unmatched"] = json!(unmatched);
        v["links"] = json!(links);
        if let Some(s) = &snap {
            v["checked_secs_ago"] = json!(s.age_secs());
        }
        v
    }
}

fn one_say(state: BtnState, matched: &[Browser], none_connected: bool, name: &str) -> String {
    match (state, matched.first()) {
        (BtnState::Ready, Some(b)) => format!(
            "Use only this browser: `bsk session start --browser {}`.",
            b.instance_id
        ),
        (BtnState::Ready, None) if none_connected => "The extension is on in this tab's profile but bsk sees no connected browser. Ask the user to turn on the local connection in the extension popup.".to_string(),
        (BtnState::Ready, None) => format!(
            "bsk cannot tell which connected browser is this tab's profile. Ask the user to link it in this tab's Browser section (\"This is {name}\" on the connected browser), or to set the extension's Browser name to \"{name}\", and call again. Do not use another browser."
        ),
        _ => "This tab's profile is not ready: the extension is missing or off there. Ask the user to finish in the Browser dialog of this tab. Do not use another browser.".to_string(),
    }
}
