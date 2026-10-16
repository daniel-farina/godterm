//! The model bar: one row in each pane, between its tab and the usage,
//! with the harness's top models (providers::Provider::tab_models, or
//! `[tab_models]` in config.toml). The current one is highlighted; a
//! click switches the tab with the harness's own in-session command,
//! `/model <arg>` (Claude Code and Grok Build both have it), typed through
//! the paste then Enter path, only while the tab is idle at its prompt
//! with an empty input box. A busy tab gets it after its turn: never an
//! interrupt.
//!
//! The current model comes from the tab itself: for Claude Code its
//! transcript (the latest assistant message's model, or a newer "Set
//! model to" from /model, the user's own included), else its screen's
//! banner, else the account's settings.json; for Grok its status line or
//! header, else the session's summary.json.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::harness::Harness;
use crate::pane::{Activity, LaunchKind};

/// At most this many models in a bar.
pub const MAX_MODELS: usize = 4;
/// Panes with fewer interior rows get no bar.
pub const MIN_ROWS: u16 = 12;
/// How often a shown tab's model is looked up.
const POLL: Duration = Duration::from_secs(3);
/// A /model just sent shows as current this long, unless the tab names a
/// model (its own word wins once it does).
const TRUST_SENT: Duration = Duration::from_secs(20);
/// A queued switch is given up after this long.
const QUEUE_MAX: Duration = Duration::from_secs(6 * 3600);
/// The transcript tail read for the model.
const TAIL: u64 = 1 << 20;

pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// A model in a pane's bar.
#[derive(Debug, Clone, PartialEq)]
pub struct BarModel {
    pub arg: String,
    pub label: String,
    pub desc: String,
    pub matches: Vec<String>,
}

/// A switch waiting for its tab to be idle.
#[derive(Debug, Clone)]
pub struct ModelSwitch {
    pub uid: u64,
    pub arg: String,
    pub label: String,
    pub at: Instant,
}

/// What a switch did.
#[derive(Debug, Clone, PartialEq)]
pub enum Switched {
    /// Typed in now.
    Sent,
    /// Waits; the reason ("after this turn").
    Queued(&'static str),
    /// It already runs that model.
    Already,
}

impl Switched {
    pub fn word(&self) -> &'static str {
        match self {
            Switched::Sent => "applied",
            Switched::Queued(_) => "queued",
            Switched::Already => "already",
        }
    }
}

/// The bar's models for harness `h`: the config's list when it has one
/// ("haiku", "fable=Fable"), else the registry's; at most MAX_MODELS.
pub fn models_for(h: Harness, cfg: &crate::config::Config) -> Vec<BarModel> {
    let reg = crate::providers::tab_models(h);
    let of = |m: &crate::providers::TabModel| BarModel {
        arg: m.arg.to_string(),
        label: m.label.to_string(),
        desc: m.desc.to_string(),
        matches: m.matches.iter().map(|s| s.to_string()).collect(),
    };
    let over: Vec<&str> = cfg
        .tab_models
        .of(h)
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let v: Vec<BarModel> = if over.is_empty() {
        reg.iter().map(of).collect()
    } else {
        over.into_iter()
            .filter_map(|e| {
                let (arg, label) = match e.split_once('=') {
                    Some((a, l)) => (a.trim(), Some(l.trim())),
                    None => (e, None),
                };
                if arg.is_empty() {
                    return None;
                }
                let known = reg
                    .iter()
                    .find(|m| m.arg.eq_ignore_ascii_case(arg) || m.label.eq_ignore_ascii_case(arg));
                let mut b = match known {
                    Some(m) => of(m),
                    None => BarModel {
                        arg: arg.to_string(),
                        label: arg.to_string(),
                        desc: String::new(),
                        matches: vec![arg.to_lowercase()],
                    },
                };
                if let Some(l) = label.filter(|l| !l.is_empty()) {
                    b.label = l.to_string();
                }
                Some(b)
            })
            .collect()
    };
    v.into_iter().take(MAX_MODELS).collect()
}

/// Which of `models` a model id or display name is ("claude-opus-5-5",
/// "Sonnet 5.5", "grok-4.7-build-fast"): the longest piece found wins.
pub fn index_of(models: &[BarModel], model: &str) -> Option<usize> {
    let m = model.to_lowercase();
    let mut best: Option<(usize, usize)> = None;
    for (i, b) in models.iter().enumerate() {
        let arg = b.arg.to_lowercase();
        for k in b.matches.iter().map(String::as_str).chain([arg.as_str()]) {
            if !k.is_empty() && m.contains(k) && best.is_none_or(|(_, n)| k.len() > n) {
                best = Some((i, k.len()));
            }
        }
    }
    best.map(|(i, _)| i)
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | ',' | '/'))
        .filter(|w| !w.is_empty())
        .map(|w| match w {
            "quick" | "quicker" | "fast" | "faster" | "fastest" | "cheap" | "cheaper"
            | "cheapest" | "small" | "smallest" | "light" | "lightest" => "quickest".into(),
            "smart" | "smarter" | "smartest" | "best" | "strongest" | "strong" | "biggest"
            | "powerful" | "capable" => "capable".into(),
            "middle" | "mid" => "balanced".into(),
            w => w.to_string(),
        })
        .collect()
}

/// A model named loosely ("haiku", "use opus", "claude sonnet 5.5", "the
/// fast one", "grok 4.7", "2"): its index, or None when nothing or more
/// than one fits.
pub fn resolve(models: &[BarModel], name: &str) -> Option<usize> {
    let n = name.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    if let Ok(k) = n.parse::<usize>() {
        return (1..=models.len()).contains(&k).then(|| k - 1);
    }
    if let Some(i) = models
        .iter()
        .position(|m| m.arg.to_lowercase() == n || m.label.to_lowercase() == n)
    {
        return Some(i);
    }
    // Every piece of the name counts; a tie goes to the plainer model
    // ("grok 4.7" is 4.7, not 4.7 fast), a real tie to nobody.
    const STOP: &[&str] = &[
        "the", "a", "an", "one", "to", "use", "model", "on", "that", "this", "tab", "please",
        "switch", "it", "claude", "most",
    ];
    let said: Vec<String> = words(&n)
        .into_iter()
        .filter(|w| !STOP.contains(&w.as_str()))
        .collect();
    let mut scored: Vec<(usize, usize, usize)> = models
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let own: Vec<String> = words(&format!("{} {}", m.arg, m.label));
            let mut all = own.clone();
            all.extend(words(&m.desc));
            all.extend(m.matches.iter().flat_map(|k| words(k)));
            let score = said.iter().filter(|w| all.contains(w)).count();
            (i, score, own.len())
        })
        .filter(|(_, s, _)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    match scored.as_slice() {
        [] => index_of(models, &n),
        [one] => Some(one.0),
        [a, b, ..] if a.1 > b.1 || a.2 < b.2 => Some(a.0),
        _ => None,
    }
}

/// Text without ANSI color codes.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// "Set model to `Sonnet 5.5` and saved ..." (Claude Code's /model): the
/// model it names.
pub fn set_model_text(s: &str) -> Option<String> {
    let s = plain(s);
    for pre in ["Set model to ", "Kept model as "] {
        let Some(i) = s.find(pre) else { continue };
        let rest = &s[i + pre.len()..];
        let name = if let Some(r) = rest.strip_prefix('`') {
            r.split('`').next().unwrap_or("")
        } else {
            let end = [" and ", " (", "<", "\n", " for "]
                .iter()
                .filter_map(|e| rest.find(e))
                .min()
                .unwrap_or(rest.len());
            &rest[..end]
        };
        let name = name.trim().trim_end_matches('.');
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

/// The model a Claude Code transcript names last: the newest assistant
/// message's model, or a newer "Set model to" from /model (subagents'
/// and synthetic messages left out).
pub fn claude_transcript_model(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = vec![];
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines().rev() {
        if !line.contains("\"model\"") && !line.contains("model to") && !line.contains("model as") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        match v["type"].as_str() {
            Some("assistant") => {
                if let Some(m) = v["message"]["model"]
                    .as_str()
                    .filter(|m| !m.is_empty() && !m.starts_with('<'))
                {
                    return Some(m.to_string());
                }
            }
            Some("user") | Some("system") => {
                let c = &v["message"]["content"];
                let texts: Vec<String> = match c {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a
                        .iter()
                        .filter_map(|b| b["text"].as_str().map(str::to_string))
                        .collect(),
                    _ => v["content"]
                        .as_str()
                        .map(str::to_string)
                        .into_iter()
                        .collect(),
                };
                for t in texts {
                    if t.contains("<local-command-stdout>") || v["type"] == "system" {
                        if let Some(m) = set_model_text(&t) {
                            return Some(m);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// A Grok session's model (summary.json's current_model_id).
pub fn grok_session_model(dir: &Path) -> Option<String> {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).ok()?).ok()?;
    v["current_model_id"]
        .as_str()
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// The model the account's settings.json starts sessions with.
fn settings_model(home: &Path) -> Option<String> {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("settings.json")).ok()?).ok()?;
    v["model"]
        .as_str()
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// Grok's status line segment "Grok 4.7 (high)".
fn grok_status_segment(screen: &str) -> Option<String> {
    screen.lines().find_map(|l| {
        if !l.contains('·') {
            return None;
        }
        l.split('·')
            .map(str::trim)
            .find(|s| s.starts_with("Grok ") && !s.starts_with("Grok Build"))
            .map(str::to_string)
    })
}

/// The model a tab's screen shows: Claude Code's banner ("Opus 5.5 ·
/// Claude Max" under "Claude Code v..."), Grok's status line or header.
pub fn screen_model(h: Harness, screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().collect();
    match h {
        Harness::Claude => {
            let i = lines.iter().position(|l| l.contains("Claude Code v"))?;
            let next = lines.get(i + 1)?;
            let head = next.split('·').next()?;
            let name = head.trim_start_matches(|c: char| !c.is_ascii_alphabetic());
            let name = name.trim();
            (!name.is_empty() && name.split_whitespace().count() <= 3).then(|| name.to_string())
        }
        Harness::Grok => {
            if let Some(s) = grok_status_segment(screen) {
                let name = s.split(" (").next().unwrap_or(&s).trim();
                return Some(name.to_string());
            }
            lines.iter().find_map(|l| {
                let t = l.trim();
                t.starts_with("Grok Build")
                    .then(|| t.split_whitespace().last())
                    .flatten()
                    .filter(|w| w.starts_with("grok-"))
                    .map(str::to_string)
            })
        }
    }
}

/// The effort a tab's screen shows ("◐ medium · /effort"; "Grok 4.7
/// (high)").
pub fn screen_effort(h: Harness, screen: &str) -> Option<String> {
    let known = |w: &str| EFFORTS.contains(&w).then(|| w.to_string());
    match h {
        Harness::Claude => screen.lines().rev().find_map(|l| {
            let i = l.find("· /effort")?;
            known(l[..i].split_whitespace().last()?)
        }),
        Harness::Grok => {
            let s = grok_status_segment(screen)?;
            let inner = s
                .split('(')
                .nth(1)?
                .split(')')
                .next()?
                .trim()
                .to_lowercase();
            known(&inner)
        }
    }
}

/// Whether the tab's input box is empty (None: no box found). Dim text
/// is a placeholder, not a draft.
pub fn input_empty(h: Harness, screen: &vt100::Screen) -> Option<bool> {
    let (rows, cols) = screen.size();
    let row_text = |r: u16| -> String { screen.contents_between(r, 0, r, cols) };
    // Whether row r has text that is not dim from column c on.
    let typed = |r: u16, from: u16, to: u16| -> bool {
        (from..to).any(|c| {
            screen.cell(r, c).is_some_and(|cell| {
                let s = cell.contents();
                !s.trim().is_empty() && !cell.dim()
            })
        })
    };
    match h {
        Harness::Claude => {
            for r in (1..rows).rev() {
                let t = row_text(r);
                let Some(col) = t.find('❯') else { continue };
                if !t[..col].trim().is_empty() {
                    continue;
                }
                if !row_text(r - 1).trim_start().starts_with('─') {
                    continue;
                }
                let c0 = t[..col].chars().count() as u16 + 1;
                return Some(!typed(r, c0, cols));
            }
            None
        }
        Harness::Grok => {
            for r in 0..rows.saturating_sub(1) {
                if !row_text(r).trim_start().starts_with('╭') {
                    continue;
                }
                let t = row_text(r + 1);
                if t.contains("Type a message") {
                    return Some(true);
                }
                let Some(a) = t.find('│') else { return None };
                let b = t.rfind('│').filter(|b| *b > a).unwrap_or(t.len());
                let (c0, c1) = (
                    t[..a].chars().count() as u16 + 1,
                    t[..b].chars().count() as u16,
                );
                return Some(!typed(r + 1, c0, c1));
            }
            None
        }
    }
}

impl App {
    /// The harness of a pane's account.
    pub fn pane_harness(&self, s: usize) -> Option<Harness> {
        let a = self.panes.get(s)?.account?;
        Some(self.cfg.accounts.get(a)?.harness())
    }

    /// The model bar's models for pane `s` (empty: no bar).
    pub fn bar_models(&self, s: usize) -> Vec<BarModel> {
        match self.pane_harness(s) {
            Some(h) => models_for(h, &self.cfg),
            None => vec![],
        }
    }

    /// Whether pane `i` (interior `inner_h` rows) shows the bar now.
    pub fn model_bar_on(&self, i: usize, inner_h: u16) -> bool {
        if !self.cfg.tab_model_bar || inner_h < MIN_ROWS {
            return false;
        }
        let Some(slot) = self.panes.get(i) else {
            return false;
        };
        slot.account.is_some()
            && slot.cur().kind != LaunchKind::Login
            && !self.bar_models(i).is_empty()
    }

    /// Which bar model tab (s, t) runs (a switch just sent counts until
    /// the tab names one).
    pub fn tab_model_index(&self, s: usize, t: usize, models: &[BarModel]) -> Option<usize> {
        let tab = &self.panes[s].tabs[t];
        if let Some((arg, at)) = &tab.model_set {
            if at.elapsed() < TRUST_SENT {
                if let Some(i) = models.iter().position(|m| &m.arg == arg) {
                    return Some(i);
                }
            }
        }
        index_of(models, tab.model.as_deref()?)
    }

    /// The model's short name for the state and get_state ("Haiku", or
    /// what the tab says when it is not in the bar).
    pub fn tab_model_label(&self, s: usize, t: usize) -> Option<String> {
        let models = self.bar_models(s);
        match self.tab_model_index(s, t, &models) {
            Some(i) => Some(models[i].label.clone()),
            None => self.panes[s].tabs[t].model.clone(),
        }
    }

    /// The switch waiting for tab `uid`.
    pub fn queued_model(&self, uid: u64) -> Option<&ModelSwitch> {
        self.model_switches.iter().find(|m| m.uid == uid)
    }

    /// Why tab (s, t) cannot take a /model now (None: it can).
    fn not_ready_for_model(&self, s: usize, t: usize) -> Option<&'static str> {
        let tab = &self.panes[s].tabs[t];
        if !tab.is_running() {
            return Some("once it starts");
        }
        match tab.activity {
            Activity::Ready => {}
            Activity::Working | Activity::Permission => return Some("after this turn"),
            _ => return Some("once it is ready"),
        }
        let uid = tab.uid;
        if self.pending_enters.iter().any(|p| p.uid == uid)
            || self
                .deliveries
                .iter()
                .any(|d| d.uid == uid && d.in_flight())
        {
            return Some("after the prompt being sent");
        }
        let h = self.pane_harness(s)?;
        let parser = tab.parser.lock().unwrap_or_else(|e| e.into_inner());
        if input_empty(h, parser.screen()) == Some(false) {
            return Some("once its input box is empty");
        }
        None
    }

    /// Type `/model <arg>` into tab (s, t) (paste, then Enter once it shows).
    fn send_model(&mut self, s: usize, t: usize, arg: &str, label: &str) {
        let text = format!("/model {arg}");
        crate::log::info(&format!(
            "model bar: {} gets {text}",
            crate::control::tab_id(self.panes[s].tabs[t].uid)
        ));
        self.paste_then_enter(s, t, &text);
        let tab = &mut self.panes[s].tabs[t];
        tab.model_set = Some((arg.to_string(), Instant::now()));
        tab.model_polled = None;
        let _ = label;
        self.state_dirty = true;
    }

    /// Switch tab (s, t) to bar model `k`: now when it is idle, else
    /// queued (a newer pick replaces a queued one).
    pub fn switch_tab_model(&mut self, s: usize, t: usize, k: usize) -> Result<Switched, String> {
        let models = self.bar_models(s);
        let m = models
            .get(k)
            .cloned()
            .ok_or_else(|| "no such model in this tab's bar".to_string())?;
        let uid = self.panes[s].tabs[t].uid;
        let had = self.model_switches.iter().any(|x| x.uid == uid);
        self.model_switches.retain(|x| x.uid != uid);
        if !had && self.tab_model_index(s, t, &models) == Some(k) {
            return Ok(Switched::Already);
        }
        match self.not_ready_for_model(s, t) {
            None => {
                self.send_model(s, t, &m.arg, &m.label);
                Ok(Switched::Sent)
            }
            Some(why) => {
                crate::log::info(&format!(
                    "model bar: {} switches to {} {why}",
                    crate::control::tab_id(uid),
                    m.arg
                ));
                self.model_switches.push(ModelSwitch {
                    uid,
                    arg: m.arg,
                    label: m.label,
                    at: Instant::now(),
                });
                self.state_dirty = true;
                Ok(Switched::Queued(why))
            }
        }
    }

    /// What a switch says (flash, tool result).
    pub fn switch_say(&self, s: usize, t: usize, k: usize, r: &Switched) -> String {
        let models = self.bar_models(s);
        let label = models.get(k).map(|m| m.label.as_str()).unwrap_or("?");
        let name = self.panes[s].tabs[t].name();
        let grok = self.pane_harness(s) == Some(Harness::Grok);
        match r {
            Switched::Sent if grok => format!("Switching {name} to {label}."),
            Switched::Sent => format!(
                "Switching {name} to {label}. Claude Code also makes it this account's default for new sessions."
            ),
            Switched::Queued(why) => format!("{name} switches to {label} {why}."),
            Switched::Already => format!("{name} already runs {label}."),
        }
    }

    /// A click on bar button `k` of pane `i`.
    pub fn click_tab_model(&mut self, i: usize, k: usize) {
        self.focus = i;
        self.view = crate::app::View::Grid;
        self.assistant.focused = false;
        let t = self.panes[i].active;
        match self.switch_tab_model(i, t, k) {
            Ok(r) => {
                let say = self.switch_say(i, t, k, &r);
                self.flash(say);
            }
            Err(e) => self.flash(e),
        }
    }

    /// Ctrl-a #: pick the focused tab's model with a number next.
    pub fn start_model_entry(&mut self) {
        let p = self.focus;
        let models = self.bar_models(p);
        if models.is_empty() {
            self.flash("No models to pick for this pane (it needs an account)");
            return;
        }
        self.model_entry = Some(p);
        let list: Vec<String> = models
            .iter()
            .enumerate()
            .map(|(k, m)| format!("{} {}", k + 1, m.label))
            .collect();
        self.flash(format!(
            "Model for this tab: {} (# cycles, Esc cancels)",
            list.join(", ")
        ));
    }

    /// The key after Ctrl-a #: true when it was taken.
    pub fn model_entry_key(&mut self, k: &crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        let Some(p) = self.model_entry.take() else {
            return false;
        };
        if p >= self.panes.len() {
            return false;
        }
        let t = self.panes[p].active;
        let n = self.bar_models(p).len();
        let pick = match k.code {
            KeyCode::Char(c @ '1'..='9') => {
                let i = (c as u8 - b'1') as usize;
                if i >= n {
                    self.flash(format!("This bar has {n} models"));
                    return true;
                }
                i
            }
            KeyCode::Char('#') => {
                let models = self.bar_models(p);
                let cur = self
                    .queued_model(self.panes[p].tabs[t].uid)
                    .and_then(|q| models.iter().position(|m| m.arg == q.arg))
                    .or_else(|| self.tab_model_index(p, t, &models));
                cur.map(|c| (c + 1) % n).unwrap_or(0)
            }
            KeyCode::Esc => return true,
            _ => return false,
        };
        self.click_tab_model(p, pick);
        true
    }

    /// A Grok tab's session folder.
    pub(crate) fn grok_session_dir(&self, s: usize, t: usize) -> Option<PathBuf> {
        let a = self.panes[s].account?;
        let id = self.panes[s].tabs[t].session_id.clone()?;
        let root = self.cfg.accounts.get(a)?.config_dir().join("sessions");
        std::fs::read_dir(root)
            .ok()?
            .flatten()
            .map(|d| d.path().join(&id))
            .find(|p| p.join("summary.json").is_file())
    }

    /// Look up tab (s, t)'s model and effort (at most every POLL unless
    /// `now`).
    pub fn refresh_tab_model(&mut self, s: usize, t: usize, now: bool) {
        let Some(h) = self.pane_harness(s) else {
            return;
        };
        if !now
            && self.panes[s].tabs[t]
                .model_polled
                .is_some_and(|p| p.elapsed() < POLL)
        {
            return;
        }
        self.panes[s].tabs[t].model_polled = Some(Instant::now());
        let screen = self.tab_screen(s, t);
        let effort = screen_effort(h, &screen);
        let from_file = match h {
            Harness::Claude => match self.transcript_path(s, t) {
                Some(p) => {
                    let stamp = crate::takeover::stamp(&p);
                    if self.panes[s].tabs[t].model_stamp == Some(stamp) {
                        None
                    } else {
                        self.panes[s].tabs[t].model_stamp = Some(stamp);
                        claude_transcript_model(&p)
                    }
                }
                None => None,
            },
            Harness::Grok => None,
        };
        let tab_model = self.panes[s].tabs[t].model.clone();
        let found = match h {
            // A new transcript line wins; else what was found, else the
            // screen, else the account's default.
            Harness::Claude => from_file.clone().or(tab_model).or_else(|| {
                screen_model(h, &screen).or_else(|| {
                    let a = self.panes[s].account?;
                    settings_model(&self.cfg.accounts.get(a)?.config_dir())
                })
            }),
            // Its status line is live; the session file is written later.
            Harness::Grok => screen_model(h, &screen).or_else(|| {
                self.grok_session_dir(s, t)
                    .and_then(|d| grok_session_model(&d))
                    .or(tab_model)
            }),
        };
        let models = self.bar_models(s);
        let tab = &mut self.panes[s].tabs[t];
        if let Some((arg, at)) = tab.model_set.clone() {
            let says = found
                .as_deref()
                .and_then(|m| index_of(&models, m))
                .map(|i| models[i].arg == arg);
            if says == Some(true) || at.elapsed() >= TRUST_SENT {
                tab.model_set = None;
            } else if from_file.is_none() && h == Harness::Claude {
                // Not written yet: look again soon.
                tab.model_stamp = None;
            }
        }
        if found.is_some() && tab.model != found {
            tab.model = found;
            self.state_dirty = true;
        }
        if effort.is_some() {
            tab.effort = effort;
        }
    }

    /// Queued switches go in once their tab is idle; shown tabs' models
    /// are looked up.
    pub fn tab_model_tick(&mut self) {
        let sw = std::mem::take(&mut self.model_switches);
        for x in sw {
            let Some((s, t)) = self.find_tab(self.current_uid(x.uid)) else {
                continue;
            };
            if x.at.elapsed() > QUEUE_MAX {
                crate::log::info(&format!(
                    "model bar: gave up switching {} to {}",
                    crate::control::tab_id(x.uid),
                    x.arg
                ));
                continue;
            }
            if self.not_ready_for_model(s, t).is_none() {
                self.send_model(s, t, &x.arg, &x.label);
            } else {
                self.model_switches.push(x);
            }
        }
        if !self.cfg.tab_model_bar {
            return;
        }
        for s in 0..self.panes.len() {
            if self.panes[s].hidden || self.pane_rects.get(s).is_none_or(|r| r.width == 0) {
                continue;
            }
            let t = self.panes[s].active;
            if self.panes[s].tabs[t].is_running() {
                self.refresh_tab_model(s, t, false);
            }
        }
    }

    /// The set_tab_model tool: {tab, model}; at once, no confirmation.
    pub fn set_tab_model_tool(&mut self, args: &Value) -> Result<Value, String> {
        let (s, t) = self.one_tab(args)?;
        let name = args["model"]
            .as_str()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .ok_or("model is required")?;
        let models = self.bar_models(s);
        if models.is_empty() {
            return Err("that tab has no account, so no models".into());
        }
        let list = models
            .iter()
            .map(|m| format!("{} ({})", m.label, m.arg))
            .collect::<Vec<_>>()
            .join(", ");
        let k = resolve(&models, name)
            .ok_or_else(|| format!("\"{name}\" is not one of this tab's models: {list}"))?;
        let r = self.switch_tab_model(s, t, k)?;
        let say = self.switch_say(s, t, k, &r);
        let mut v = json!({
            "tab": crate::control::tab_id(self.panes[s].tabs[t].uid),
            "model": models[k].label,
            "arg": models[k].arg,
            "status": r.word(),
            "say": say,
        });
        if let Switched::Queued(why) = r {
            v["when"] = json!(why);
        }
        if r == Switched::Sent {
            v["command"] = json!(format!("/model {}", models[k].arg));
        }
        Ok(json!({"ok": true, "result": v}))
    }
}
