//! The Settings screen model: what can be set, how it is shown, and how a
//! change is written to config.toml (with toml_edit, so comments and layout
//! survive).

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use toml_edit::{value, Array, DocumentMut, Item};

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    General,
    Layout,
    Accounts,
    Voice,
    Assistant,
    Memory,
    Setup,
    Keys,
    About,
}

pub const SECTIONS: &[(Section, &str)] = &[
    (Section::General, "General"),
    (Section::Layout, "Layout"),
    (Section::Accounts, "Accounts"),
    (Section::Voice, "Voice and Audio"),
    (Section::Assistant, "Assistant"),
    (Section::Memory, "Memory"),
    (Section::Setup, "Setup"),
    (Section::Keys, "Keys"),
    (Section::About, "About"),
];

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Toggle,
    /// Cycles through these values.
    Choice(&'static [&'static str]),
    /// Integer with bounds and step.
    Number {
        min: i64,
        max: i64,
        step: i64,
    },
    /// Float with bounds and step.
    Float {
        min: f64,
        max: f64,
        step: f64,
    },
    /// Cycles through values found at runtime (devices, models, voices).
    Pick(Vec<String>),
    /// Free text; lists are comma separated.
    Text,
    /// Comma separated list of strings.
    List,
    /// A secret kept outside config.toml (the Keychain): typed masked,
    /// shown only as saved or not.
    Secret,
}

/// The xAI API key row: never written to config.toml.
pub const XAI_KEY: &str = "voice.grok.api_key";

/// Where a setting lives in config.toml.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    /// A top level key, or `voice.x`.
    Global(&'static str),
    /// A key inside the `[[account]]` table at this index.
    Account(usize, &'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub key: Key,
    pub label: String,
    pub kind: Kind,
    pub help: &'static str,
    /// Wired to config but the feature is not built yet.
    pub soon: bool,
    /// Per account overrides may be unset ("inherit").
    pub inherit: bool,
    /// The header it sits under in its section ("" for none).
    pub group: &'static str,
    /// Set in a search for a row the current choices hide: why.
    pub hidden: Option<String>,
}

fn g(key: &'static str, label: &str, kind: Kind, help: &'static str) -> Setting {
    Setting {
        key: Key::Global(key),
        label: label.into(),
        kind,
        help,
        soon: false,
        inherit: false,
        group: "",
        hidden: None,
    }
}

pub const MODES: &[&str] = &[
    "bypass",
    "default",
    "auto",
    "accept-edits",
    "plan",
    "manual",
    "dont-ask",
];

/// Every row of `sec`, the hidden ones too (marked with why), for search.
pub fn all_settings(sec: Section, cfg: &Config) -> Vec<Setting> {
    match sec {
        Section::Voice => voice_rows(cfg, true),
        Section::Assistant => assistant_rows(cfg, true),
        s => settings_for(s, cfg),
    }
}

/// Rows matching a search across every section (hidden ones included):
/// each with its section and header as the group.
pub fn search(cfg: &Config, query: &str) -> Vec<(Section, Setting)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    for (sec, _) in SECTIONS {
        for s in all_settings(*sec, cfg) {
            let key = match &s.key {
                Key::Global(k) => k.to_string(),
                Key::Account(_, k) => k.to_string(),
            };
            let hay = format!("{} {} {}", s.label, key, s.help).to_lowercase();
            if q.split_whitespace().all(|w| hay.contains(w)) {
                out.push((*sec, s));
            }
        }
    }
    out
}

/// Settings shown in `sec`. Account rows are generated per account.
pub fn settings_for(sec: Section, cfg: &Config) -> Vec<Setting> {
    match sec {
        Section::General => vec![
            g("permission_mode", "Permission mode", Kind::Choice(MODES), "How new tabs start: bypass skips every permission check (--dangerously-skip-permissions); default lets the tool decide."),
            g("auto_trust", "Auto trust folders", Kind::Toggle, "Trust each tab's folder automatically so the folder trust dialog does not come back."),
            g("trusted_dirs", "Trusted folders", Kind::List, "Limit auto trust to these folders, comma separated. Empty: any folder."),
            g("new_tab_base", "New tab base folder", Kind::Text, "New tabs get a dated folder inside this one (accounts can override)."),
            g("new_tab_name_pattern", "New tab folder name", Kind::Text, "strftime pattern; -1, -2 ... is added. Default %Y-%m-%d."),
            g("new_tab_create_folder", "Create the new tab folder", Kind::Toggle, "Create the dated folder; off means it must already exist."),
            g("recent_paths_limit", "Recent folders kept", Kind::Number { min: 1, max: 50, step: 1 }, "Recent folders listed in the New Tab dialog."),
            g("remember_window", "Remember window position", Kind::Toggle, "Reopen the window (from GodTerm.app) where it was; moves to the main screen if that monitor is gone."),
            g("splash", "Show splash at start", Kind::Choice(&["once", "never"]), "once: the GodTerm splash plays on the first launch and once after an update (any key skips it). never: skip it. godterm splash shows it any time."),
            g("splash_motion", "Splash animation", Kind::Choice(&["on", "auto", "off"]), "on: always animate; auto: a still frame when macOS Reduce Motion is on; off: always a still frame."),
            g("restore", "Restore tabs", Kind::Choice(&["eager", "lazy"]), "eager starts every restored tab at launch; lazy when it is first shown."),
            g("auto_restart", "Auto restart crashed tabs", Kind::Toggle, "Restart a tab (resuming its session) when it exits with an error."),
            g("notifications", "Desktop notifications", Kind::Toggle, "Notify when a background tab needs approval or finishes, while the terminal is not in front."),
            g("autostart", "Start tabs at launch", Kind::Toggle, "Start sessions in logged in panes when godterm opens."),
            g("update.enabled", "Check for updates", Kind::Toggle, "Look for a new version on GitHub at start and every few hours (Settings > About shows it; Ctrl-a N restarts into it)."),
            g("update.channel", "Update channel", Kind::Choice(&["stable", "prerelease"]), "stable: releases only; prerelease: also release candidates."),
            g("update.auto_download", "Download updates", Kind::Toggle, "Download and verify a new version in the background, so a restart is all it takes. Homebrew and system packages only get a notice."),
            g("update.require_signature", "Require signed updates", Kind::Toggle, "On (recommended): an update is installed only if its SHA256SUMS carries a valid signature from GodTerm's release key. Turning this off is not recommended: an unsigned release would then be trusted on its checksums alone."),
            g("grok_claude_compat.hooks", "Grok: Claude hooks", Kind::Toggle, "Let grok tabs run the hooks of your Claude Code setup (~/.claude plugins and settings). Off by default: grok fails on them (command not found .../hooks/node)."),
            g("grok_claude_compat.skills", "Grok: Claude skills", Kind::Toggle, "Let grok tabs use your Claude Code skills."),
            g("grok_claude_compat.rules", "Grok: Claude rules", Kind::Toggle, "Let grok tabs read Claude Code rules."),
            g("grok_claude_compat.agents", "Grok: CLAUDE.md", Kind::Toggle, "Let grok tabs read your CLAUDE.md instructions."),
            g("grok_claude_compat.mcps", "Grok: Claude MCP servers", Kind::Toggle, "Let grok tabs start the MCP servers from your ~/.claude.json (off by default)."),
            g("grok_claude_compat.sessions", "Grok: Claude sessions", Kind::Toggle, "grok's (staged) access to Claude Code sessions."),
            g("show_path", "Show the folder", Kind::Toggle, "Show the active tab's folder in each pane header (label · folder · email) and under each tab. Click it to copy, double click for Finder, right click for more."),
            g("show_email", "Show account email", Kind::Toggle, "Show each account's email in the pane header (Ctrl-a e). Accounts can override it."),
            g("privacy", "Privacy mode", Kind::Toggle, "Hide emails everywhere, doctor included, for screen sharing (Ctrl-a E)."),
            g("suggest_move_below", "Suggest moving tabs below (% 5h left)", Kind::Float { min: 0.0, max: 50.0, step: 5.0 }, "When a tab's account has less than this left, its header offers a one click move to the account with the most left. 0 turns it off."),
            g("refresh_secs", "Usage refresh (seconds)", Kind::Number { min: 60, max: 3600, step: 30 }, "How often usage is fetched per account (never more than once a minute)."),
            g("usage.auto_failover", "When an account runs out", Kind::Choice(&["ask", "auto", "off"]), "ask: a notice offers to move the tab to the account of the same provider with the most left, one yes (Ctrl-a F, a click, or \"move it\") moves it; auto: idle tabs move by themselves and it says so, busy ones are offered; off: nothing."),
            g("usage.failover_pct", "Runs out below (% left)", Kind::Float { min: 0.0, max: 50.0, step: 1.0 }, "An account counts as running out under this much left (its binding limit, 5 hour or weekly), or when a tab says it hit its limit."),
        ],
        Section::Layout => vec![
            g("confirm_close", "Confirm closing a tab", Kind::Choice(&["busy", "always", "never"]), "busy: ask when the tab is working, waiting for an answer or has loops; always: whenever it runs; never. Closed tabs can be reopened (Ctrl-a W)."),
            g("tab_position", "Tab list position", Kind::Choice(&["left", "right", "top"]), "Where each pane lists its tabs. A pane can override it (Ctrl-a S)."),
            g("layout", "Pane layout", Kind::Choice(&["auto", "grid", "columns", "rows", "focus"]), "auto: 1, 2, 2+1, 2x2, 3x2, 3x3, paging past what fits at 40x10 per pane. grid uses Grid below; focus makes the focused pane large. Ctrl-a L cycles; drag pane borders to resize."),
            g("grid", "Grid (e.g. 3x2)", Kind::Text, "Columns x rows for layout = grid."),
            g("ungrouped_tabs", "Ungrouped tabs", Kind::Choice(&["bottom", "top"]), "Where tabs outside any group sit in a tab list: under the groups, or above them (pinned tabs are always first)."),
            g("usage_colors", "Usage colors", Kind::Choice(&["gradient", "bands", "mono"]), "By percent left: gradient blends sage, sand and clay; bands uses five steps (75+, 50, 25, 10, under 10); mono is gray, bright under 10%. Under 10% is bold, under 5% gets a ▼."),
        ],
        Section::Accounts => {
            let mut v = vec![];
            for (i, a) in cfg.accounts.iter().enumerate() {
                let n = a.display().to_string();
                let acc = |k: &'static str, label: &str, kind: Kind, help: &'static str, inherit: bool, is_soon: bool| Setting {
                    key: Key::Account(i, k),
                    label: format!("{n}: {label}"),
                    kind,
                    help,
                    soon: is_soon,
                    inherit,
                    group: "",
                    hidden: None,
                };
                v.push(acc("label", "label", Kind::Text, "Name shown in the pane header and status bar.", false, false));
                v.push(acc("harness", "harness", Kind::Choice(crate::harness::NAMES), "The coding agent it runs: claude (Claude Code) or grok (Grok Build, isolated with its own GROK_HOME). Restart its tabs after a change.", false, false));
                v.push(acc("color", "color", Kind::Choice(&crate::theme::ROTATION), "Accent color of the account.", false, false));
                v.push(acc("cwd", "default folder", Kind::Text, "Folder new tabs of this account start in.", false, false));
                v.push(acc("new_tab_base", "new tab base", Kind::Text, "Base folder for this account's new tabs.", true, false));
                v.push(acc("args", "extra args", Kind::List, "Extra command line arguments for this account, comma separated.", false, false));
                v.push(acc("permission_mode", "permission mode", Kind::Choice(MODES), "Overrides the global permission mode for this account.", true, false));
                v.push(acc("auto_trust", "auto trust", Kind::Toggle, "Overrides auto trust for this account.", true, false));
                v.push(acc("show_email", "show email", Kind::Toggle, "Overrides show email for this account.", true, false));
                v.push(acc("scrollback_lines", "history lines", Kind::Number { min: 0, max: 100_000, step: 500 }, "Overrides the history kept per tab for this account.", true, false));
            }
            v
        }
        Section::Voice => voice_rows(cfg, false),
        Section::Assistant => assistant_rows(cfg, false),
        Section::Memory => vec![
            g("memory_saver", "Memory saver", Kind::Toggle, "Low memory profile (Ctrl-a Z, the Mem button): 200 lines of history on screen, none in the background, lazy restore, idle background tabs paused, caches dropped, voice helpers stopped when idle, small queues."),
            g("suspend_idle_after", "Pause idle background tabs after", Kind::Text, "With the memory saver: e.g. 10m, 30m, off. A paused tab (zz) stops its claude and resumes the conversation when you open it. Never a working, waiting or looping tab."),
            g("scrollback_lines", "History lines per tab", Kind::Number { min: 0, max: 100_000, step: 500 }, "Scrollback kept for the tab on screen; 0 keeps none. Accounts can override it."),
            g("background_scrollback_lines", "History for background tabs", Kind::Number { min: 0, max: 100_000, step: 100 }, "Tabs not on screen keep at most this much; trimmed when they go to the background."),
            g("writer_queue_kb", "Input queue per tab", Kind::Number { min: 16, max: 65536, step: 64 }, "Writes queued to a tab before more input is dropped (new tabs)."),
            g("pty_read_buffer_kb", "Read buffer per tab (KB)", Kind::Number { min: 1, max: 256, step: 4 }, "Bytes read from claude at a time (new tabs)."),
            g("transcript_cache", "Parsed sessions cached", Kind::Number { min: 0, max: 1000, step: 10 }, "Sessions kept parsed in memory per account; older ones are read again when needed."),
            g("usage_history", "Usage samples kept", Kind::Number { min: 0, max: 1000, step: 10 }, "5 hour usage samples kept per account (the dashboard trend)."),
            g("voice_idle_stop_min", "Stop voice helpers when idle (min)", Kind::Number { min: 0, max: 240, step: 5 }, "In push to talk, stop whisper-server and Kokoro after this many idle minutes (0: keep them). The memory saver uses 5."),
            g("viz.reduced_motion", "Reduced motion (live map)", Kind::Toggle, "The live map draws 8 frames a second with fewer particles (it does with the memory saver too)."),
        ],
        Section::Keys | Section::About | Section::Setup => vec![],
    }
}

/// The current value of a setting as config.toml would hold it.
pub fn current(cfg: &Config, s: &Setting) -> Option<toml::Value> {
    let root = toml::Value::try_from(cfg).ok()?;
    match &s.key {
        Key::Global(k) => {
            let mut v = &root;
            for part in k.split('.') {
                v = v.get(part)?;
            }
            Some(v.clone())
        }
        Key::Account(i, k) => root.get("account")?.get(*i)?.get(*k).cloned(),
    }
}

/// How the value reads on screen.
pub fn display(cfg: &Config, s: &Setting) -> String {
    if s.kind == Kind::Secret {
        return match crate::voice::grok_tts::key_saved() {
            Some(true) => format!("saved ({})", crate::voice::grok_tts::key_store().place()),
            Some(false) => "not set".into(),
            None => "checking...".into(),
        };
    }
    match current(cfg, s) {
        None if s.inherit => "inherit".into(),
        None => String::new(),
        Some(toml::Value::Boolean(b)) => if b { "on" } else { "off" }.into(),
        Some(toml::Value::String(t)) => t,
        Some(toml::Value::Integer(n)) => n.to_string(),
        Some(toml::Value::Float(f)) => fmt_float(f),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => other.to_string(),
    }
}

/// Floats as typed: f32 settings come back as 0.800000011920929.
pub fn fmt_float(f: f64) -> String {
    let s = format!("{f:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// The value after one step of a click or Right (`dir` +1) / Left (-1).
pub fn stepped(cfg: &Config, s: &Setting, dir: i64) -> Option<Item> {
    let cur = current(cfg, s);
    Some(match &s.kind {
        Kind::Toggle => {
            // Inherited overrides start from the global value.
            let b = match &cur {
                Some(toml::Value::Boolean(b)) => *b,
                _ => match &s.key {
                    Key::Account(_, "auto_trust") => cfg.auto_trust,
                    Key::Account(_, "show_email") => cfg.show_email,
                    _ => false,
                },
            };
            value(!b)
        }
        Kind::Choice(opts) => {
            let opts: Vec<String> = opts.iter().map(|o| o.to_string()).collect();
            value(cycle(&opts, cur.as_ref(), dir)?)
        }
        Kind::Pick(opts) => value(cycle(opts, cur.as_ref(), dir)?),
        Kind::Number { min, max, step } => {
            let n = cur.as_ref().and_then(|v| v.as_integer()).unwrap_or(*min);
            value((n + dir * step).clamp(*min, *max))
        }
        Kind::Float { min, max, step } => {
            let f = cur.as_ref().and_then(|v| v.as_float()).unwrap_or(*min);
            let f = ((f + dir as f64 * step).clamp(*min, *max) * 100.0).round() / 100.0;
            value(f)
        }
        Kind::Text | Kind::List | Kind::Secret => return None,
    })
}

fn cycle(opts: &[String], cur: Option<&toml::Value>, dir: i64) -> Option<String> {
    if opts.is_empty() {
        return None;
    }
    let now = match cur {
        Some(toml::Value::String(s)) => s.clone(),
        Some(toml::Value::Integer(n)) => n.to_string(),
        Some(toml::Value::Float(f)) => fmt_float(*f),
        _ => String::new(),
    };
    let n = opts.len() as i64;
    let next = match opts.iter().position(|o| *o == now) {
        Some(i) => (i as i64 + dir).rem_euclid(n) as usize,
        None => 0,
    };
    Some(opts[next].clone())
}

/// Device, model and voice lists for the Voice and Audio section, found in
/// the background when Settings opens (listing devices takes a moment).
#[derive(Debug, Clone, Default)]
pub struct AudioLists {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub models: Vec<String>,
    pub kokoro_voices: Vec<String>,
    pub say_voices: Vec<String>,
    /// xAI voices (fetched when a Grok credential works, else the known
    /// list).
    pub grok_voices: Vec<String>,
}

pub static AUDIO_LISTS: std::sync::Mutex<Option<AudioLists>> = std::sync::Mutex::new(None);

/// Where the device lists stand, for the Settings screen.
#[derive(Debug, Clone, PartialEq)]
pub enum ListState {
    NotStarted,
    Detecting,
    Done,
    /// Something did not answer in time (it was stopped).
    Failed(String),
}

pub static LIST_STATE: std::sync::Mutex<ListState> = std::sync::Mutex::new(ListState::NotStarted);

pub fn list_state() -> ListState {
    LIST_STATE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Detect the device and voice lists in the background, once (Settings
/// opened); `retry` runs it again. Never blocks the caller.
pub fn start_audio_lists(vc: &crate::config::VoiceCfg, retry: bool) {
    {
        let mut st = LIST_STATE.lock().unwrap_or_else(|e| e.into_inner());
        match &*st {
            ListState::Detecting => return,
            ListState::Done | ListState::Failed(_) if !retry => return,
            _ => {}
        }
        *st = ListState::Detecting;
    }
    let vc = vc.clone();
    std::thread::spawn(move || detect_audio_lists(&vc));
}

/// Something that lists devices, at most LIST_TIMEOUT (it runs on a
/// thread of its own; a call that never returns is left behind).
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(crate::voice::LIST_TIMEOUT).ok()
}

/// Tests: a device lister that never answers.
#[cfg(test)]
pub static HANG_LISTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Fill AUDIO_LISTS (blocking, with timeouts; call from a thread).
pub fn detect_audio_lists(vc: &crate::config::VoiceCfg) {
    #[cfg(test)]
    if HANG_LISTS.load(std::sync::atomic::Ordering::SeqCst) {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
    let mut failed: Vec<&str> = vec![];
    let inputs = match crate::voice::input_devices_timed(&vc.ffmpeg) {
        Ok(v) => v,
        Err(_) => {
            failed.push("microphones");
            vec![]
        }
    };
    let outputs = within(crate::voice::player::output_devices).unwrap_or_else(|| {
        failed.push("speakers");
        vec![]
    });
    let dir = crate::config::expand_tilde(&vc.model)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let mut models: Vec<String> = std::fs::read_dir(&dir)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("ggml-") && n.ends_with(".bin"))
                })
                .map(|p| crate::config::tilde(&p))
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    let kokoro_voices =
        crate::voice::tts::kokoro_voices(&crate::config::expand_tilde(&vc.kokoro_voices));
    let say_voices = crate::voice::say_voices();
    crate::voice::grok_tts::refresh_key_status();
    let grok_voices = if crate::voice::engines::chain(vc).contains(&"grok") {
        crate::voice::grok_tts::live_voices(&vc.grok)
    } else {
        vec![]
    };
    *AUDIO_LISTS.lock().unwrap_or_else(|e| e.into_inner()) = Some(AudioLists {
        inputs,
        outputs,
        models,
        kokoro_voices,
        say_voices,
        grok_voices,
    });
    *LIST_STATE.lock().unwrap_or_else(|e| e.into_inner()) = if failed.is_empty() {
        ListState::Done
    } else {
        ListState::Failed(format!("{} did not answer in time", failed.join(" and ")))
    };
}

fn with_default(first: &str, rest: &[String]) -> Vec<String> {
    let mut v = vec![first.to_string()];
    v.extend(rest.iter().filter(|r| r.as_str() != first).cloned());
    v
}

/// A picker when the list is known, free text until then.
fn pick_or_text(list: Option<Vec<String>>) -> Kind {
    match list {
        Some(v) if v.len() > 1 => Kind::Pick(v),
        _ => Kind::Text,
    }
}

const NOISE_FLOORS: &[&str] = &[
    "auto", "-75", "-70", "-65", "-60", "-55", "-50", "-45", "-40",
];

/// Rows of one section, grouped under headers. Rows that do not apply
/// to the current choices are left out, or with `all` (search) kept and
/// marked with why they are hidden.
struct Rows {
    v: Vec<Setting>,
    all: bool,
    group: &'static str,
}

impl Rows {
    fn new(all: bool) -> Rows {
        Rows {
            v: vec![],
            all,
            group: "",
        }
    }

    fn group(&mut self, g: &'static str) {
        self.group = g;
    }

    fn add(&mut self, mut s: Setting) {
        s.group = self.group;
        self.v.push(s);
    }

    /// Shown when `cond`; in a search, kept as "hidden: applies when ...".
    fn when(&mut self, cond: bool, applies: &str, s: Setting) {
        if cond {
            self.add(s);
        } else if self.all {
            let mut s = s;
            s.hidden = Some(format!("applies when {applies}"));
            self.add(s);
        }
    }
}

/// The section headers that start collapsed.
pub const COLLAPSED_BY_DEFAULT: &[&str] = &["Advanced and debugging"];

/// Settings > Voice and Audio > Talk back: the engine (local or cloud),
/// its fallbacks, and only the chosen engine's panel.
fn talk_back_rows(r: &mut Rows, cfg: &Config, l: Option<&AudioLists>) {
    let vc = &cfg.voice;
    let on = vc.tts && vc.tts_engine != "off";
    let eng = vc.tts_engine.as_str();
    r.group("Talk back");
    r.add(g(
        "voice.tts",
        "Talk back",
        Kind::Toggle,
        "Spoken confirmations, read backs and announcements.",
    ));
    r.when(vc.tts, "talk back is on", g("voice.tts_engine", "Engine", Kind::Choice(&["kokoro", "say", "grok", "off"]), "kokoro: a neural voice, local on this Mac. say: the macOS voice, local. grok: xAI's cloud voices (streamed; the spoken text, the assistant's replies, is sent to xAI). off: silent."));
    let others: Vec<&str> = ["kokoro", "say", "grok"]
        .into_iter()
        .filter(|e| *e != eng)
        .collect();
    let _ = &others;
    r.when(on, "talk back is on", g("voice.tts_fallback", "Fallback engines", Kind::List, "Tried in order when the engine fails or gives no audio in time, comma separated: any of the engines other than the one above (kokoro, say, grok)."));
    r.when(
        on,
        "talk back is on",
        g(
            "voice.tts_volume",
            "Volume",
            Kind::Float {
                min: 0.0,
                max: 1.0,
                step: 0.1,
            },
            "0 to 1, every engine.",
        ),
    );
    r.when(
        on,
        "talk back is on",
        g(
            "voice.output_device",
            "Output device",
            pick_or_text(l.map(|l| with_default("default", &l.outputs))),
            "Where talk back plays.",
        ),
    );
    // The chosen engine's panel only.
    let grok = on && eng == "grok";
    let sources = crate::voice::grok_tts::sources(cfg);
    let mut ids: Vec<String> = vec!["auto".into()];
    ids.extend(sources.iter().map(|s| s.id.clone()));
    r.when(grok, "engine = grok", g("voice.grok.auth", "Grok: sign in with", Kind::Choice(&["oauth", "api_key"]), "oauth: a grok login (the Grok Build login of a grok account here, or the main ~/.grok); api_key: an xAI API key from console.x.ai, kept in the Keychain. Shared with Grok speech recognition."));
    r.when(grok && vc.grok.auth == "api_key", "engine = grok and sign in = api_key", g(XAI_KEY, "Grok: xAI API key", Kind::Secret, "Typed masked and kept in the Keychain (service GodTerm-xai-api-key), never in config.toml. Test checks it; Remove deletes it."));
    r.when(grok && vc.grok.auth != "api_key", "engine = grok and sign in = oauth", Setting {
        label: "Grok login".to_string(),
        ..g("voice.grok.source", "", Kind::Pick(ids), "auto: the first logged in grok account, then the main ~/.grok. Or one account, or main. The token is read when speaking and never logged.")
    });
    let voices = l
        .map(|l| l.grok_voices.clone())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            crate::voice::grok_tts::KNOWN_VOICES
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    r.when(
        grok,
        "engine = grok",
        g(
            "voice.grok.voice",
            "Grok: voice",
            Kind::Pick(voices),
            "An xAI voice (eve, ara, leo, rex, sal, ...). Preview plays a sample.",
        ),
    );
    r.when(
        grok,
        "engine = grok",
        g(
            "voice.grok.speed",
            "Grok: speed",
            Kind::Float {
                min: 0.7,
                max: 1.5,
                step: 0.05,
            },
            "Speaking rate, 0.7 to 1.5; 1.0 is normal.",
        ),
    );
    r.when(
        grok,
        "engine = grok",
        g(
            "voice.grok.language",
            "Grok: language",
            Kind::Text,
            "BCP-47 code (en, es-ES, fr, ...) or auto.",
        ),
    );
    r.when(
        grok,
        "engine = grok",
        g(
            "voice.grok.timeout_ms",
            "Grok: timeout (ms)",
            Kind::Number {
                min: 1000,
                max: 15000,
                step: 500,
            },
            "No audio within this many milliseconds: the sentence goes to the fallback.",
        ),
    );
    let kokoro = on && eng == "kokoro";
    r.when(
        kokoro,
        "engine = kokoro",
        g(
            "voice.kokoro_voice",
            "Kokoro: voice",
            pick_or_text(l.map(|l| l.kokoro_voices.clone())),
            "a = American, b = British; f = female, m = male. Preview plays a sample.",
        ),
    );
    r.when(
        on && (eng == "kokoro" || eng == "say"),
        "engine = kokoro or say",
        g(
            "voice.tts_speed",
            "Speed",
            Kind::Float {
                min: 0.5,
                max: 2.0,
                step: 0.1,
            },
            "Speaking rate, 0.5 to 2; 1.0 is normal.",
        ),
    );
    r.when(kokoro, "engine = kokoro", g("voice.tts_provider", "Kokoro: runs on", Kind::Choice(&["cpu", "coreml"]), "cpu loads in a fraction of a second; CoreML loads slower and is not faster for this model."));
    r.when(kokoro, "engine = kokoro", g("voice.tts_unload_after_s", "Kokoro: unload after (s)", Kind::Number { min: 0, max: 3600, step: 60 }, "Free the model (about 800 MB) after this many idle seconds; it reloads in a fraction of a second. 0 keeps it loaded."));
    r.when(
        on && eng == "say",
        "engine = say",
        g(
            "voice.tts_voice",
            "say: voice",
            pick_or_text(l.map(|l| with_default("system", &l.say_voices))),
            "The macOS voice. Preview plays a sample.",
        ),
    );
}

fn voice_rows(cfg: &Config, all: bool) -> Vec<Setting> {
    let lists = AUDIO_LISTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let l = lists.as_ref();
    let vc = &cfg.voice;
    let mut r = Rows::new(all);
    r.group("Listening");
    r.add(g("voice.mode", "Mode", Kind::Choice(&["push", "wake", "open"]), "push: Ctrl-a space or the Mic button. wake: listening for the wake word. open: open mic, every utterance is a command (a red OPEN MIC shows while on)."));
    let hands_free = vc.mode != "push";
    r.when(
        hands_free,
        "mode = wake or open",
        g(
            "voice.wake_words",
            "Wake words",
            Kind::List,
            "Comma separated, e.g. hey god, god term (hey go still works).",
        ),
    );
    r.when(hands_free, "mode = wake or open", g("voice.wake_sensitivity", "Wake sensitivity", Kind::Float { min: 0.5, max: 1.0, step: 0.05 }, "0.5 loose to 1.0 exact: how close a heard phrase must be to a wake word or a learned alias."));
    r.when(
        vc.mode == "open",
        "mode = open",
        g(
            "voice.open_mic_sleep_min",
            "Open mic sleeps after (min)",
            Kind::Number {
                min: 0,
                max: 240,
                step: 5,
            },
            "Silent minutes before open mic goes back to waiting for the wake word. 0: never.",
        ),
    );
    r.add(g("voice.wake_follow_up_s", "After the wake word alone (s)", Kind::Number { min: 3, max: 30, step: 1 }, "Say the wake word alone and GodTerm answers \"Yes?\"; what you say within this many seconds is the command."));
    r.add(g("voice.pause_default_s", "Pause: default (s)", Kind::Number { min: 10, max: 3600, step: 30 }, "\"Hold on\" without a duration stops acting on speech this many seconds; the wake word resumes sooner."));
    r.add(g(
        "voice.pause_max_s",
        "Pause: longest (s)",
        Kind::Number {
            min: 60,
            max: 14400,
            step: 300,
        },
        "The longest pause the assistant may set, in seconds (\"give me an hour\").",
    ));
    r.add(g(
        "voice.enabled",
        "Start at launch",
        Kind::Toggle,
        "Start listening when GodTerm opens.",
    ));
    r.add(g(
        "voice.mute_on_start",
        "Start muted",
        Kind::Toggle,
        "Start with the mic muted (the red MUTED button or Ctrl-a X unmutes).",
    ));
    r.add(g("voice.instant_commands", "Instant one-word commands", Kind::Toggle, "stop, yes, no, approve, deny, sleep, stop talking, next tab, previous tab run at once when they are the whole utterance; everything else goes to the assistant."));

    r.group("Microphone");
    r.add(g(
        "voice.device",
        "Microphone",
        pick_or_text(l.map(|l| with_default("default", &l.inputs))),
        "The input device. Test mic shows the live level and what was heard.",
    ));
    r.add(g("voice.capture", "Capture", Kind::Choice(&["auto", "apple", "ffmpeg"]), "apple: Apple voice processing through godterm-speech (echo cancellation, noise suppression, gain, Voice Isolation); ffmpeg: plain capture; auto: apple on macOS with the default mic, else ffmpeg."));
    let apple_cap = vc.capture != "ffmpeg";
    r.when(apple_cap, "capture = apple or auto", g("voice.voice_processing", "Apple voice processing", Kind::Toggle, "Echo cancellation, noise suppression and gain control. Pick Voice Isolation with Open macOS mic modes."));
    r.when(
        !(apple_cap && vc.voice_processing),
        "Apple voice processing is off (it cleans the signal itself)",
        g(
            "voice.denoise",
            "Noise suppression",
            Kind::Choice(&["auto", "on", "off"]),
            "RNNoise before the endpointer for fans, hum and keys (not voices).",
        ),
    );
    r.add(g(
        "voice.gain_db",
        "Gain (dB)",
        Kind::Float {
            min: -12.0,
            max: 24.0,
            step: 1.0,
        },
        "Boost a quiet mic or tame a hot one, in dB. The meter turns amber near clipping.",
    ));

    r.group("Speech recognition");
    r.add(g("voice.engine", "Engine", Kind::Choice(&["auto", "apple", "whisper", "grok"]), "apple: Apple's on-device recognizer (macOS 26+); whisper: whisper.cpp, local; auto: Apple when available, else whisper; grok: xAI's cloud recognizer: your microphone audio (each utterance, after the speaker lock) is sent to xAI."));
    let whisper = vc.engine == "whisper" || vc.engine == "auto";
    let grok = vc.engine == "grok";
    r.add(g(
        "voice.language",
        "Language",
        Kind::Text,
        "Spoken language code, e.g. en, or auto (Grok).",
    ));
    r.when(
        whisper,
        "engine = whisper or auto",
        g(
            "voice.model",
            "Whisper model",
            pick_or_text(l.map(|l| l.models.clone())),
            "The ggml model for final transcripts (large-v3-turbo is accurate and quick).",
        ),
    );
    r.when(
        whisper,
        "engine = whisper or auto",
        g(
            "voice.partial_model",
            "Live partials",
            Kind::Choice(&["auto", "main", "off"]),
            "Partial transcripts while you talk: auto uses the small wake model when there is one.",
        ),
    );
    r.when(
        whisper,
        "engine = whisper or auto",
        g(
            "voice.wake_model",
            "Wake word model",
            Kind::Text,
            "auto, off, or a path to a small model that screens for the wake word.",
        ),
    );
    r.when(
        whisper,
        "engine = whisper or auto",
        g(
            "voice.whisper_threads",
            "Threads",
            Kind::Number {
                min: 0,
                max: 16,
                step: 1,
            },
            "CPU threads for whisper; 0 picks about half of the idle cores.",
        ),
    );
    r.when(whisper, "engine = whisper or auto", g("voice.beam_size", "Beam size", Kind::Number { min: 1, max: 8, step: 1 }, "1 to 8: beam search for final transcripts (5 is more accurate; 1 is greedy and quicker)."));
    r.when(whisper, "engine = whisper or auto", g("voice.whisper_vad", "Whisper VAD", Kind::Toggle, "whisper.cpp's own voice activity detection trims non speech inside each utterance (a 1 MB model, fetched once)."));
    // Grok: the xAI login is shared with Grok talk back.
    let sources = crate::voice::grok_tts::sources(cfg);
    let mut ids: Vec<String> = vec!["auto".into()];
    ids.extend(sources.iter().map(|s| s.id.clone()));
    r.when(grok, "engine = grok", g("voice.grok.auth", "Grok: sign in with", Kind::Choice(&["oauth", "api_key"]), "The xAI login, shared with Grok talk back: oauth is a grok login (a grok account here, or the main ~/.grok); api_key an xAI API key, kept in the Keychain."));
    r.when(grok && vc.grok.auth == "api_key", "engine = grok and sign in = api_key", g(XAI_KEY, "Grok: xAI API key", Kind::Secret, "Typed masked and kept in the Keychain (GodTerm-xai-api-key), never in config.toml. Shared with Grok talk back."));
    r.when(grok && vc.grok.auth != "api_key", "engine = grok and sign in = oauth", Setting {
        label: "Grok login".to_string(),
        ..g("voice.grok.source", "", Kind::Pick(ids), "auto: the first logged in grok account, then the main ~/.grok. Shared with Grok talk back; the token is never logged.")
    });
    r.when(
        grok,
        "engine = grok",
        g(
            "voice.grok_stt.timeout_ms",
            "Grok: timeout (ms)",
            Kind::Number {
                min: 1000,
                max: 30000,
                step: 500,
            },
            "No text within this many milliseconds: the utterance goes to the fallback.",
        ),
    );
    r.when(
        grok,
        "engine = grok",
        g(
            "voice.grok_stt.local_partials",
            "Live partials (local)",
            Kind::Toggle,
            "Show partial transcripts from whisper or Apple, on this Mac, while you talk.",
        ),
    );
    r.when(grok, "engine = grok", g("voice.grok_stt.fallback", "Fallback", Kind::Choice(&["whisper", "apple"]), "Used when Grok fails or is slow (then the other local one). A lasting failure (no login, no credits) skips Grok for 5 minutes."));

    r.group("Background voices");
    r.add(g("voice.speaker_lock", "Speaker lock", Kind::Choice(&["off", "open_mic_only", "always"]), "Only your voice gets through: utterances that do not sound like you (a phone call, a TV) are ignored before transcription. open_mic_only checks open mic; always every hands free utterance. Needs Train my voice."));
    let lock = vc.speaker_lock != "off";
    r.when(lock, "speaker lock is on", g("voice.speaker_threshold", "Threshold", Kind::Float { min: 0.0, max: 0.9, step: 0.02 }, "Score needed to count as you, 0 to 0.9. 0 uses the one calibrated when you trained your voice."));
    r.when(
        lock,
        "speaker lock is on",
        g(
            "voice.speaker_margin",
            "Margin",
            Kind::Float {
                min: -0.2,
                max: 0.2,
                step: 0.02,
            },
            "Added to the calibrated threshold, -0.2 to 0.2: higher is stricter.",
        ),
    );
    r.when(lock, "speaker lock is on", g("voice.speaker_model", "Speaker model", Kind::Text, "auto (WeSpeaker ResNet34 in ~/.cache/godterm-models), or a path to another ONNX speaker embedding model."));
    r.add(g("voice.near_field", "Ignore far-away speech", Kind::Toggle, "Drop speech much quieter and duller than your voice (a speakerphone or TV across the room)."));
    r.when(
        vc.near_field,
        "ignore far-away speech is on",
        g(
            "voice.near_field_db",
            "Far away below (dB)",
            Kind::Float {
                min: 6.0,
                max: 30.0,
                step: 1.0,
            },
            "How many dB under your usual level counts as far away.",
        ),
    );

    talk_back_rows(&mut r, cfg, l);

    r.group("Talk back behavior");
    r.add(g(
        "voice.speaker_muted",
        "Speaker mute",
        Kind::Toggle,
        "Nothing is said out loud; answers still show as text. Separate from the mic mute (Ctrl-a O, or say \"be quiet\").",
    ));
    r.add(g(
        "assistant.speak_typed",
        "Speak replies to typed messages",
        Kind::Toggle,
        "off: a typed message gets a text answer only; a spoken one is answered out loud (unless the speaker is muted).",
    ));
    r.add(g(
        "voice.speak_confirm",
        "Confirmations",
        Kind::Toggle,
        "Short confirmations like \"sent\" and \"new tab\".",
    ));
    r.add(g(
        "voice.announce",
        "Announcements",
        Kind::Toggle,
        "Say when a background tab needs approval or finishes.",
    ));
    r.add(g(
        "voice.conversation",
        "Conversation mode",
        Kind::Toggle,
        "After talking back, listen for a reply without the wake word (wake mode).",
    ));
    r.add(g("voice.barge_in", "Barge-in", Kind::Toggle, "Talking over a reply stops it at once and takes what you say as the next request. Its own voice never counts."));
    r.when(
        vc.barge_in,
        "barge-in is on",
        g(
            "voice.barge_margin_db",
            "Barge-in margin (dB)",
            Kind::Float {
                min: 6.0,
                max: 30.0,
                step: 1.0,
            },
            "How many dB over the threshold speech must be to cut through.",
        ),
    );
    r.add(g(
        "voice.chime",
        "Wake chime",
        Kind::Toggle,
        "A short sound when the wake word is heard.",
    ));
    r.add(g("voice.pronounce", "Pronunciation fixes", Kind::List, "word=spoken form, comma separated, applied before speaking. E.g. godterm=clawed go, CLI=C L I."));

    r.group("Advanced and debugging");
    r.add(g(
        "voice.vad_threshold",
        "Speech threshold",
        Kind::Float {
            min: 1.5,
            max: 10.0,
            step: 0.5,
        },
        "Speech starts this many times above the noise floor (the bar on the meter).",
    ));
    r.add(g(
        "voice.end_silence_ms",
        "Silence timeout (ms)",
        Kind::Number {
            min: 300,
            max: 3000,
            step: 100,
        },
        "Milliseconds of silence that end an utterance.",
    ));
    r.add(g(
        "voice.max_utterance_s",
        "Longest utterance (s)",
        Kind::Number {
            min: 5,
            max: 60,
            step: 5,
        },
        "Seconds; one utterance is cut there.",
    ));
    r.add(g(
        "voice.preroll_ms",
        "Pre-roll (ms)",
        Kind::Number {
            min: 0,
            max: 1200,
            step: 60,
        },
        "Milliseconds kept from just before speech starts, so the first word is not clipped.",
    ));
    r.add(g(
        "voice.noise_floor",
        "Noise floor (dBFS)",
        Kind::Choice(NOISE_FLOORS),
        "auto learns the room for half a second, then follows it. A number fixes it.",
    ));
    r.add(g("voice.debug_save_audio", "Save last 20 utterances", Kind::Toggle, "Keeps the exact audio sent to recognition with its partials, final and timings in ~/.godterm/voice/debug (stays on this Mac)."));
    r.when(vc.instant_commands, "instant commands are on", g("voice.instant", "Instant set", Kind::List, "Comma separated phrases, or phrase=action (stop, approve, deny, sleep, wake, stop_talking, next_tab, previous_tab), e.g. yep=approve."));
    r.v
}

fn assistant_rows(cfg: &Config, all: bool) -> Vec<Setting> {
    let mut accts = vec!["best".to_string()];
    accts.extend(cfg.accounts.iter().map(|a| a.name.clone()));
    let on = cfg.assistant.mode != "off";
    let why = "the assistant is on";
    let mut r = Rows::new(all);
    r.group("Assistant");
    r.when(on, why, g("assistant.provider", "Provider", Kind::Choice(&["claude", "grok"]), "Which model provider runs the assistant: claude (on one of your Claude accounts) or grok (its own Grok login, never your Grok accounts). The conversation carries over when you switch."));
    r.when(on, why, g("assistant.panel", "Panel", Kind::Choice(&["docked", "overlay", "auto"]), "docked: beside the panes, which make room; overlay: floats over the right side and the panes keep their size; auto: docked while every pane keeps about 80 columns, otherwise overlay."));
    r.when(on, why, g("assistant.speak_typed", "Speak replies to typed messages", Kind::Toggle, "off: a typed message gets a text answer only; a spoken one is answered out loud (unless the speaker is muted)."));
    r.when(
        on,
        why,
        g(
            "voice.speaker_muted",
            "Speaker mute",
            Kind::Toggle,
            "Nothing is said out loud; answers still show as text. Separate from the mic mute.",
        ),
    );
    r.add(g("assistant.mode", "Mode", Kind::Choice(&["always", "off"]), "always: the assistant understands everything you say or type (apart from the instant one word commands); off: only the instant commands work. About your sessions it only gets metadata and short snippets, never whole transcripts."));
    r.when(on, why, g("assistant.account", "Account", Kind::Pick(accts), "Whose quota it spends. best: the logged in account with the most 5 hour quota left (it moves when that one runs low)."));
    r.when(on, why, g("assistant.model", "Model", Kind::Text, "claude-haiku-4-5 (default, quickest and lightest on quota), or sonnet / opus for harder requests."));
    r.when(
        on,
        why,
        g(
            "assistant.effort",
            "Effort",
            Kind::Choice(&["low", "medium", "high"]),
            "low is quickest (no extended thinking); higher thinks more before answering.",
        ),
    );
    r.when(
        on,
        why,
        g(
            "assistant.prewarm",
            "Keep it warm",
            Kind::Toggle,
            "Start its process at launch and when you start talking, so the first answer is quick.",
        ),
    );
    r.group("Speaking");
    r.when(
        on,
        why,
        g(
            "assistant.style",
            "Speaking style",
            Kind::Choice(&["concise", "chatty"]),
            "concise: one or two short sentences; chatty: a little more.",
        ),
    );
    r.when(
        on,
        why,
        g(
            "assistant.max_output_tokens",
            "Longest reply (tokens)",
            Kind::Number {
                min: 1024,
                max: 32000,
                step: 1024,
            },
            "Upper limit for one reply including tool calls; spoken answers are kept short separately.",
        ),
    );
    r.when(on, why, g("assistant.endpoint_ms", "End of speech (ms)", Kind::Number { min: 250, max: 2000, step: 50 }, "Milliseconds of silence that end what you say while the assistant or open mic is on; shorter answers sooner."));
    r.when(
        on,
        why,
        g(
            "assistant.follow_up_s",
            "Follow up window (s)",
            Kind::Number {
                min: 0,
                max: 60,
                step: 2,
            },
            "Seconds after it speaks to listen for your reply without the wake word (wake mode).",
        ),
    );
    r.when(
        on,
        why,
        g(
            "assistant.spoken_sentences",
            "Spoken sentences",
            Kind::Number {
                min: 0,
                max: 10,
                step: 1,
            },
            "Say at most this many sentences of a reply (0: all); the rest stays in the panel, and saying \"more\" reads it.",
        ),
    );
    r.when(
        on,
        why,
        g(
            "assistant.answer_wait_min",
            "Wait for tab answers (min)",
            Kind::Number {
                min: 1,
                max: 120,
                step: 5,
            },
            "A question the assistant asks a tab's agent is reported back when it answers, for up to this many minutes.",
        ),
    );
    r.group("Confirmations and limits");
    r.when(on, why, g("assistant.confirm", "Ask before", Kind::Choice(&["destructive", "always", "never"]), "destructive: a spoken yes before closing several or busy tabs, stopping loops, broadcasting, denying, or approving rm, push, force and the like; always: every approval too; never: no confirmations (not recommended)."));
    r.when(
        on,
        why,
        g(
            "assistant.max_tool_calls",
            "Most actions per request",
            Kind::Number {
                min: 1,
                max: 50,
                step: 1,
            },
            "It stops and reports after this many actions in one turn.",
        ),
    );
    r.group("Memory and history");
    r.when(on, why, g("assistant.memory_days", "Remember (days)", Kind::Number { min: 0, max: 30, step: 1 }, "Days of chats a new assistant process gets as a short summary (the last one word for word). 0: no memory."));
    r.when(
        on,
        why,
        g(
            "assistant.reset_after_turns",
            "Fresh conversation after (turns)",
            Kind::Number {
                min: 2,
                max: 200,
                step: 2,
            },
            "Keeps its context small and quick; a one line summary carries over.",
        ),
    );
    r.add(g(
        "assistant.history_days",
        "Keep conversations (days)",
        Kind::Number {
            min: 0,
            max: 3650,
            step: 5,
        },
        "Saved conversations older than this many days are deleted. 0 keeps them all.",
    ));
    r.v
}

#[cfg(test)]
#[test]
fn floats_read_well() {
    assert_eq!(fmt_float(0.800000011920929), "0.8");
    assert_eq!(fmt_float(1.0), "1");
    assert_eq!(fmt_float(-12.5), "-12.5");
    assert_eq!(fmt_float(0.0), "0");
}

/// Parse typed text for a Text or List setting.
pub fn from_text(s: &Setting, text: &str) -> Item {
    match s.kind {
        Kind::List => {
            let mut a = Array::new();
            for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                a.push(part);
            }
            value(a)
        }
        _ => value(text.trim()),
    }
}

/// Add an `[[account]]` table at the end of config.toml, keeping comments.
pub fn append_account(path: &Path, a: &crate::config::AccountCfg) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: DocumentMut = text.parse().context("config.toml does not parse")?;
    let mut t = toml_edit::Table::new();
    t["name"] = value(&a.name);
    t["label"] = value(&a.label);
    t["color"] = value(&a.color);
    if a.harness != "claude" {
        t["harness"] = value(&a.harness);
    }
    if let Some(c) = &a.cwd {
        t["cwd"] = value(c);
    }
    if !doc.contains_key("account") {
        doc["account"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    doc["account"]
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("account is not a list of tables"))?
        .push(t);
    let out = doc.to_string();
    Config::parse(&out)?;
    let tmp = path.with_extension("toml.tmp");
    crate::config::write_private(&tmp, out)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Replace `t[k]`, keeping the comments around the old value.
fn set_keep_decor(t: &mut toml_edit::Table, k: &str, item: Item) {
    match (
        t.get(k).and_then(Item::as_value).map(|v| v.decor().clone()),
        item,
    ) {
        (Some(decor), Item::Value(mut v)) => {
            *v.decor_mut() = decor;
            t[k] = Item::Value(v);
        }
        (_, item) => {
            t[k] = item;
        }
    }
}

/// Write one setting into config.toml, keeping comments and formatting.
/// `None` removes the key, which brings back the default (or, for account
/// overrides, inherits the global value). The result must still parse.
pub fn write(path: &Path, key: &Key, item: Option<Item>) -> Result<()> {
    if *key == Key::Global(XAI_KEY) {
        return Err(anyhow!("API keys are never written to config.toml"));
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: DocumentMut = text.parse().context("config.toml does not parse")?;
    match key {
        Key::Global(k) => {
            let parts: Vec<&str> = k.split('.').collect();
            let (table_path, last) = parts.split_at(parts.len() - 1);
            let mut t = doc.as_table_mut();
            for p in table_path {
                if !t.contains_key(p) {
                    t.insert(p, toml_edit::table());
                }
                t = t[*p]
                    .as_table_mut()
                    .ok_or_else(|| anyhow!("{p} is not a table"))?;
            }
            match item {
                Some(i) => set_keep_decor(t, last[0], i),
                None => {
                    t.remove(last[0]);
                }
            }
        }
        Key::Account(i, k) => {
            let arr = doc
                .get_mut("account")
                .and_then(Item::as_array_of_tables_mut)
                .ok_or_else(|| anyhow!("no [[account]] tables"))?;
            let t = arr.get_mut(*i).ok_or_else(|| anyhow!("no account {i}"))?;
            match item {
                Some(it) => set_keep_decor(t, k, it),
                None => {
                    t.remove(k);
                }
            }
        }
    }
    let out = doc.to_string();
    Config::parse(&out).context("that value would make config.toml invalid")?;
    let tmp = path.with_extension("toml.tmp");
    crate::config::write_private(&tmp, out)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_keep_comments_and_validate() {
        let dir = std::env::temp_dir().join(format!("godterm-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(
            &p,
            "# my notes\npermission_mode = \"bypass\" # keep\n\n[[account]]\n# work account\nname = \"work\"\n\n[voice]\ntts = true\n",
        )
        .unwrap();
        write(&p, &Key::Global("permission_mode"), Some(value("plan"))).unwrap();
        write(&p, &Key::Global("voice.tts"), Some(value(false))).unwrap();
        write(&p, &Key::Global("voice.end_silence_ms"), Some(value(900))).unwrap();
        write(&p, &Key::Account(0, "label"), Some(value("Work"))).unwrap();
        write(&p, &Key::Account(0, "auto_trust"), Some(value(false))).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(
            t.contains("# my notes") && t.contains("# work account") && t.contains("# keep"),
            "{t}"
        );
        let cfg = Config::parse(&t).unwrap();
        assert_eq!(cfg.permission_mode, "plan");
        assert!(!cfg.voice.tts);
        assert_eq!(cfg.voice.end_silence_ms, 900);
        assert_eq!(cfg.accounts[0].label, "Work");
        assert_eq!(cfg.accounts[0].auto_trust, Some(false));
        // Reset removes the key: back to the default or to inheriting.
        write(&p, &Key::Account(0, "auto_trust"), None).unwrap();
        write(&p, &Key::Global("permission_mode"), None).unwrap();
        let cfg = Config::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(cfg.accounts[0].auto_trust, None);
        assert_eq!(cfg.permission_mode, "bypass");
        // Invalid values are refused and the file is unchanged.
        let before = std::fs::read_to_string(&p).unwrap();
        assert!(write(&p, &Key::Account(0, "name"), Some(value("../bad"))).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn values_display_and_step() {
        let mut cfg = Config::default();
        let s = &settings_for(Section::General, &cfg)[0];
        assert_eq!(display(&cfg, s), "bypass");
        assert_eq!(stepped(&cfg, s, 1).unwrap().as_str(), Some("default"));
        assert_eq!(stepped(&cfg, s, -1).unwrap().as_str(), Some("dont-ask"));
        let trust = settings_for(Section::General, &cfg)
            .into_iter()
            .find(|s| s.key == Key::Global("auto_trust"))
            .unwrap();
        assert_eq!(display(&cfg, &trust), "on");
        assert_eq!(stepped(&cfg, &trust, 1).unwrap().as_bool(), Some(false));
        let sb = settings_for(Section::Memory, &cfg)
            .into_iter()
            .find(|s| s.key == Key::Global("scrollback_lines"))
            .unwrap();
        assert_eq!(stepped(&cfg, &sb, 1).unwrap().as_integer(), Some(2500));
        cfg.scrollback_lines = 0;
        assert_eq!(stepped(&cfg, &sb, -1).unwrap().as_integer(), Some(0));
        let accts = settings_for(Section::Accounts, &cfg);
        let pm = accts
            .iter()
            .find(|s| s.key == Key::Account(0, "permission_mode"))
            .unwrap();
        assert_eq!(display(&cfg, pm), "inherit");
        let wake = all_settings(Section::Voice, &cfg)
            .into_iter()
            .find(|s| s.key == Key::Global("voice.wake_words"))
            .unwrap();
        assert_eq!(display(&cfg, &wake), "hey god, god term");
        assert_eq!(
            from_text(&wake, "jarvis, computer ,")
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            SECTIONS
                .iter()
                .all(|(sec, _)| settings_for(*sec, &cfg).iter().all(|s| !s.soon)),
            "nothing is coming soon any more"
        );
    }
}
