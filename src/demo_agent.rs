//! The demo agent: a stand in `claude` / `grok` for `godterm --demo`.
//!
//! `godterm --demo` links the godterm binary as `<demo home>/bin/claude`
//! and `bin/grok` (next to a marker file); started under one of those
//! names it runs this instead of GodTerm. It looks like Claude Code (or
//! Grok Build) in a terminal: a banner, the conversation, a spinner with
//! "esc to interrupt", tool calls with results and diffs, permission
//! prompts, the input box. What it does comes from a scenario file per
//! project (`<demo home>/scenarios/<folder name>.json`, see `Scenario`),
//! and it writes the matching transcript (with token usage), so the
//! sessions list, token counters, loops and the Live map have real data.
//!
//! Started with `-p` (the assistant's brain) it is a scripted brain
//! instead: canned answers that call the real control API tools.

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Claude,
    Grok,
}

/// This process is the demo agent: started as `claude` or `grok` from a
/// folder with the demo marker.
pub fn invoked_as() -> Option<Flavor> {
    let a0 = std::env::args().next()?;
    let flavor = match crate::procs::program_name(&a0).as_str() {
        "claude" => Flavor::Claude,
        "grok" => Flavor::Grok,
        _ => return None,
    };
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    dir.join(crate::demo::AGENT_MARKER)
        .is_file()
        .then_some(flavor)
}

/// The demo home: the agent lives in `<home>/bin`.
fn demo_home() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().and_then(Path::parent).map(Path::to_path_buf))
        .unwrap_or_else(crate::demo::home)
}

// ---------------------------------------------------------------------
// Scenarios

/// What one session does. JSON, one file per project folder name.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Scenario {
    /// The session's AI title (sessions list).
    pub title: String,
    /// Banner model line ("Opus 5.5", "grok-4.7-build-fast").
    pub model: String,
    /// Banner plan ("Claude Max").
    pub plan: String,
    /// Start the next task on its own (else wait for a prompt).
    pub autorun: bool,
    /// Seconds before the first task.
    pub start_delay: f32,
    /// After the last task, start again from the first one.
    pub repeat: bool,
    /// Seconds idle between tasks (min, max).
    pub idle: [f32; 2],
    /// Time multiplier (2.0 is twice as fast).
    pub speed: f32,
    /// Stop running tasks on its own after this many in one run (0: no
    /// limit); a resumed session goes on with the next one.
    pub pause_after: usize,
    /// Turns already in the transcript when the demo starts.
    pub history: Vec<Task>,
    pub tasks: Vec<Task>,
    /// For prompts typed (or sent) into the tab, in order.
    pub replies: Vec<Task>,
    /// A loop (CronCreate) set up by the first task, firing every `every` s.
    pub cron: Option<Cron>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Task {
    pub prompt: String,
    pub steps: Vec<Step>,
    pub summary: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    Think {
        secs: f32,
        #[serde(default)]
        verb: String,
    },
    Say {
        text: String,
    },
    Read {
        path: String,
        #[serde(default)]
        lines: u32,
    },
    Grep {
        pattern: String,
        #[serde(default)]
        matches: u32,
    },
    /// `diff` lines: "+42 new code", "-41 old code", " 40 context".
    Edit {
        path: String,
        #[serde(default)]
        diff: Vec<String>,
        #[serde(default)]
        approve: bool,
    },
    Write {
        path: String,
        #[serde(default)]
        lines: u32,
        #[serde(default)]
        approve: bool,
    },
    Bash {
        cmd: String,
        #[serde(default)]
        desc: String,
        #[serde(default)]
        out: Vec<String>,
        #[serde(default)]
        approve: bool,
        /// How long it runs (the output streams over this time).
        #[serde(default)]
        secs: f32,
    },
    Todo {
        items: Vec<String>,
    },
    Wait {
        secs: f32,
    },
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Cron {
    pub cron: String,
    pub human: String,
    pub prompt: String,
    /// Seconds between fires in the demo.
    pub every: f32,
    /// What a fire does.
    pub fire: Task,
}

pub fn load_scenario(home: &Path, cwd: &Path) -> Scenario {
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = home.join("scenarios");
    // "pricing-page-2" (a second folder of that name) plays pricing-page.
    let base = match name.rsplit_once('-') {
        Some((b, n)) if n.chars().all(|c| c.is_ascii_digit()) => b.to_string(),
        _ => name.clone(),
    };
    for cand in [
        format!("{name}.json"),
        format!("{base}.json"),
        "default.json".to_string(),
    ] {
        if let Ok(t) = std::fs::read_to_string(dir.join(&cand)) {
            if let Ok(mut s) = serde_json::from_str::<Scenario>(&t) {
                if s.speed <= 0.0 {
                    s.speed = 1.0;
                }
                return s;
            }
        }
    }
    Scenario {
        title: format!("Work on {name}"),
        speed: 1.0,
        replies: vec![Task {
            prompt: String::new(),
            steps: vec![
                Step::Think {
                    secs: 2.0,
                    verb: String::new(),
                },
                Step::Read {
                    path: "README.md".into(),
                    lines: 64,
                },
            ],
            summary: "Done. Let me know what you want next.".into(),
        }],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------
// Transcript

/// The session's transcript, written like the real agent writes it.
pub struct Transcript {
    flavor: Flavor,
    /// claude: the jsonl; grok: the session folder.
    path: PathBuf,
    pub sid: String,
    cwd: PathBuf,
    last: Option<String>,
    seq: u64,
    /// grok's running totals.
    totals: [u64; 4],
    messages: u64,
    /// Prompts asked (grok keeps it in summary.json for the replay).
    prompts: u64,
    title: String,
}

fn now_ts() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// grok's folder name for a cwd: the path percent encoded.
pub fn grok_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub fn transcript_path(flavor: Flavor, config: &Path, cwd: &Path, sid: &str) -> PathBuf {
    match flavor {
        Flavor::Claude => config
            .join("projects")
            .join(crate::state::encode_project_dir(cwd))
            .join(format!("{sid}.jsonl")),
        Flavor::Grok => config.join("sessions").join(grok_dir_name(cwd)).join(sid),
    }
}

impl Transcript {
    pub fn open(flavor: Flavor, config: &Path, cwd: &Path, sid: &str, title: &str) -> Transcript {
        let path = transcript_path(flavor, config, cwd, sid);
        let mut t = Transcript {
            flavor,
            path,
            sid: sid.to_string(),
            cwd: cwd.to_path_buf(),
            last: None,
            seq: 0,
            totals: [0; 4],
            messages: 0,
            prompts: 0,
            title: title.to_string(),
        };
        if flavor == Flavor::Grok {
            if let Ok(u) = std::fs::read_to_string(t.path.join("usage.json")) {
                if let Ok(v) = serde_json::from_str::<Value>(&u) {
                    let g = |k: &str| v["session"][k].as_u64().unwrap_or(0);
                    t.totals = [
                        g("inputTokens"),
                        g("outputTokens"),
                        g("cachedReadTokens"),
                        g("cacheCreationTokens"),
                    ];
                }
            }
            if let Ok(s) = std::fs::read_to_string(t.path.join("summary.json")) {
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    t.messages = v["num_chat_messages"].as_u64().unwrap_or(0);
                    t.prompts = v["demo_prompts"].as_u64().unwrap_or(0);
                }
            }
        }
        t
    }

    fn uuid(&mut self) -> String {
        self.seq += 1;
        crate::session_ops::new_uuid()
    }

    fn line(&mut self, mut v: Value) {
        if self.flavor == Flavor::Grok {
            return;
        }
        let uuid = self.uuid();
        if let Some(o) = v.as_object_mut() {
            o.entry("parentUuid").or_insert(json!(self.last));
            o.insert("isSidechain".into(), json!(false));
            o.insert("uuid".into(), json!(uuid));
            o.insert("timestamp".into(), json!(now_ts()));
            o.insert("cwd".into(), json!(self.cwd));
            o.insert("sessionId".into(), json!(self.sid));
            o.insert("version".into(), json!("2.1.291"));
        }
        self.last = Some(uuid);
        if let Some(d) = self.path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{v}");
        }
    }

    pub fn title_line(&mut self) {
        if self.flavor == Flavor::Claude && !self.path.exists() {
            let t = self.title.clone();
            self.raw(json!({"type": "ai-title", "aiTitle": t, "sessionId": self.sid}));
        }
    }

    fn raw(&mut self, v: Value) {
        if let Some(d) = self.path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{v}");
        }
    }

    pub fn user(&mut self, text: &str, scheduled: Option<&str>) {
        self.messages += 1;
        let mut v = json!({"type": "user", "message": {"role": "user", "content": text}});
        match scheduled {
            Some(id) => {
                v["turnOrigin"] = json!("scheduled");
                v["scheduledTaskId"] = json!(id);
            }
            None => self.prompts += 1,
        }
        self.line(v);
        self.grok_flush();
    }

    /// One assistant message with `content` blocks and usage.
    pub fn assistant(&mut self, content: Value, usage: [u64; 4]) {
        self.messages += 1;
        let id = format!("msg_demo{:016x}", rand_u64());
        let [inp, out, cr, cw] = usage;
        self.line(json!({"type": "assistant", "message": {
            "model": "claude-opus-5-5", "id": id, "type": "message", "role": "assistant",
            "content": content,
            "usage": {"input_tokens": inp, "cache_creation_input_tokens": cw, "cache_read_input_tokens": cr, "output_tokens": out},
        }}));
        for (i, u) in usage.iter().enumerate() {
            self.totals[i] += u;
        }
        self.grok_flush();
    }

    pub fn tool_result(&mut self, id: &str, text: &str) {
        self.line(
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": text}
            ]}}),
        );
    }

    fn grok_flush(&mut self) {
        if self.flavor != Flavor::Grok {
            return;
        }
        let _ = std::fs::create_dir_all(&self.path);
        let [i, o, cr, cw] = self.totals;
        let cost = (i as f64 * 3.0 + o as f64 * 15.0 + cr as f64 * 0.3) / 1e6;
        let _ = std::fs::write(
            self.path.join("usage.json"),
            json!({"session": {"inputTokens": i, "outputTokens": o, "cachedReadTokens": cr, "cacheCreationTokens": cw, "costUsdTicks": (cost * 1e10) as u64}}).to_string(),
        );
        let _ = std::fs::write(
            self.path.join("summary.json"),
            json!({"info": {"id": self.sid, "cwd": self.cwd}, "num_chat_messages": self.messages,
                   "generated_title": self.title, "current_model_id": "grok-4.7-build-fast", "session_kind": null,
                   "session_summary": self.title, "demo_prompts": self.prompts})
            .to_string(),
        );
    }
}

/// Past turns of a transcript, for the screen on resume: (prompts, lines).
fn replay(flavor: Flavor, path: &Path) -> (usize, Vec<Line>) {
    let mut lines = vec![];
    let mut prompts = 0;
    if flavor == Flavor::Grok {
        // grok's demo transcript keeps only totals: show the title.
        if let Ok(s) = std::fs::read_to_string(path.join("summary.json")) {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                let n = v["num_chat_messages"].as_u64().unwrap_or(0);
                if let Some(t) = v["generated_title"].as_str() {
                    lines.push(Line::Dim(format!("  Resumed: {t} ({n} messages)")));
                    lines.push(Line::Blank);
                }
                prompts = v["demo_prompts"].as_u64().unwrap_or(0) as usize;
            }
        }
        return (prompts, lines);
    }
    let Ok(f) = std::fs::File::open(path) else {
        return (0, lines);
    };
    // tool_use id -> (tool, argument), to word its result like the agent.
    let mut pending: std::collections::HashMap<String, (String, String)> = Default::default();
    for l in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&l) else {
            continue;
        };
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("user") => {
                if let Some(t) = content.as_str() {
                    if v["isMeta"].as_bool() != Some(true) {
                        if v["turnOrigin"].as_str() != Some("scheduled") {
                            prompts += 1;
                        }
                        lines.push(Line::User(t.to_string()));
                        lines.push(Line::Blank);
                    }
                } else if let Some(a) = content.as_array() {
                    for b in a {
                        if b["type"] == "tool_result" {
                            let id = b["tool_use_id"].as_str().unwrap_or("");
                            let text = b["content"].as_str().unwrap_or("");
                            if let Some((tool, arg)) = pending.remove(id) {
                                let first = text.lines().next().unwrap_or("").to_string();
                                lines.push(Line::Result(match tool.as_str() {
                                    "Read" => format!("Read {first}"),
                                    "Edit" => format!("Updated {arg}"),
                                    "Write" => format!("Wrote {arg}"),
                                    "TodoWrite" => "Todos updated".into(),
                                    _ => first,
                                }));
                                lines.push(Line::Blank);
                            }
                        } else if let Some(t) = b["text"].as_str() {
                            prompts += 1;
                            lines.push(Line::User(t.to_string()));
                        }
                    }
                }
            }
            Some("assistant") => {
                for b in content.as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("text") => {
                            lines.push(Line::Text(b["text"].as_str().unwrap_or("").to_string()));
                            lines.push(Line::Blank);
                        }
                        Some("tool_use") => {
                            let name = b["name"].as_str().unwrap_or("Tool");
                            let arg = tool_arg(name, &b["input"]);
                            pending.insert(
                                b["id"].as_str().unwrap_or("").to_string(),
                                (name.to_string(), arg.clone()),
                            );
                            lines.push(Line::Tool(display_tool(name).into(), arg));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (prompts, lines)
}

fn display_tool(name: &str) -> &str {
    match name {
        "Edit" | "MultiEdit" => "Update",
        "Grep" => "Search",
        n => n,
    }
}

fn tool_arg(name: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    match name {
        "Bash" => s("command"),
        "Read" | "Edit" | "Write" | "MultiEdit" => s("file_path"),
        "Grep" => format!("pattern: \"{}\"", s("pattern")),
        "CronCreate" => s("cron"),
        "TodoWrite" => "todos".into(),
        _ => String::new(),
    }
}

fn rand_u64() -> u64 {
    let mut b = [0u8; 8];
    crate::platform::fill_random(&mut b);
    u64::from_le_bytes(b)
}

/// A float in [a, b).
fn rand_in(a: f32, b: f32) -> f32 {
    a + (rand_u64() % 10_000) as f32 / 10_000.0 * (b - a).max(0.0)
}

// ---------------------------------------------------------------------
// Screen

#[derive(Debug, Clone)]
enum Line {
    Blank,
    User(String),
    Text(String),
    Tool(String, String),
    Result(String),
    /// Continuation of a result (indented, dim).
    More(String),
    Diff(char, u32, String),
    Todo(bool, String),
    Dim(String),
}

#[derive(Debug, Clone)]
struct Ask {
    /// "Bash command", "Edit file"...
    title: String,
    body: Vec<String>,
    diff: Vec<(char, u32, String)>,
    question: String,
    options: Vec<String>,
    sel: usize,
}

#[derive(Debug, Clone)]
enum Mode {
    Ready,
    Working {
        verb: String,
        since: Instant,
        tokens: u64,
    },
    Asking(Ask),
}

struct Screen {
    flavor: Flavor,
    banner: Vec<String>,
    lines: Vec<Line>,
    input: String,
    mode: Mode,
    notice: Option<(String, Instant)>,
    last_frame: String,
}

const ORANGE: &str = "\x1b[38;2;215;119;87m";
const GRAY: &str = "\x1b[38;2;136;136;136m";
const LIGHT: &str = "\x1b[38;2;200;200;200m";
const WHITE: &str = "\x1b[38;2;235;235;235m";
const GREEN: &str = "\x1b[38;2;78;186;101m";
const BLUE: &str = "\x1b[38;2;120;160;255m";
const GROK: &str = "\x1b[38;2;230;230;230m";
const BG_ADD: &str = "\x1b[48;2;28;64;36m";
const BG_DEL: &str = "\x1b[48;2;84;30;38m";
const BG_USER: &str = "\x1b[48;2;55;55;55m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Cut to `w` columns (display width).
fn fit(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        // A tab would move the cursor: it is spaces here.
        let c = if c == '\t' { ' ' } else { c };
        let cw = c.width().unwrap_or(0);
        if used + cw > w {
            break;
        }
        used += cw;
        out.push(c);
    }
    out
}

fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Word wrap to `w` columns.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let w = w.max(8);
    let mut out = vec![];
    for para in s.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            if !cur.is_empty() && width(&cur) + 1 + width(word) > w {
                out.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
            while width(&cur) > w {
                let head = fit(&cur, w);
                cur = cur[head.len()..].to_string();
                out.push(head);
            }
        }
        out.push(cur);
    }
    out
}

impl Screen {
    fn new(flavor: Flavor, sc: &Scenario, cwd: &Path) -> Screen {
        // Shown as a plain ~/code folder (the demo's live in its home).
        let shown = format!(
            "~/code/{}",
            cwd.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let banner = match flavor {
            Flavor::Claude => vec![
                format!("{ORANGE} ▐▛███▜▌{RESET}   {BOLD}Claude Code{RESET} v2.1.291"),
                format!(
                    "{ORANGE}▝▜█████▛▘{RESET}  {GRAY}{} · {}{RESET}",
                    if sc.model.is_empty() {
                        "Opus 5.5"
                    } else {
                        &sc.model
                    },
                    if sc.plan.is_empty() {
                        "Claude Max"
                    } else {
                        &sc.plan
                    }
                ),
                format!("{ORANGE}  ▘▘ ▝▝{RESET}    {GRAY}{shown}{RESET}"),
            ],
            Flavor::Grok => vec![format!("{GROK}{BOLD} Grok Build{RESET}")],
        };
        Screen {
            flavor,
            banner,
            lines: vec![],
            input: String::new(),
            mode: Mode::Ready,
            notice: None,
            last_frame: String::new(),
        }
    }

    fn render_line(&self, l: &Line, w: usize) -> Vec<String> {
        let g = self.flavor == Flavor::Grok;
        let dot = if g { "●" } else { "⏺" };
        match l {
            Line::Blank => vec![String::new()],
            Line::User(t) => wrap(t, w.saturating_sub(4))
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    let p = if i == 0 { "> " } else { "  " };
                    let pad = w.saturating_sub(width(&s) + 2);
                    if g {
                        format!("{LIGHT}{p}{s}{RESET}")
                    } else {
                        format!("{BG_USER}{LIGHT}{p}{s}{}{RESET}", " ".repeat(pad.min(2)))
                    }
                })
                .collect(),
            Line::Text(t) => wrap(t, w.saturating_sub(3))
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    if i == 0 {
                        format!("{WHITE}{dot}{RESET} {s}")
                    } else {
                        format!("  {s}")
                    }
                })
                .collect(),
            Line::Tool(name, arg) => {
                let a = fit(arg, w.saturating_sub(width(name) + 5));
                vec![format!("{GREEN}{dot}{RESET} {BOLD}{name}{RESET}({a})")]
            }
            Line::Result(t) => vec![format!("  {GRAY}⎿  {}{RESET}", fit(t, w.saturating_sub(6)))],
            Line::More(t) => vec![format!("     {GRAY}{}{RESET}", fit(t, w.saturating_sub(6)))],
            Line::Diff(k, n, code) => {
                let (bg, sign) = match k {
                    '+' => (BG_ADD, '+'),
                    '-' => (BG_DEL, '-'),
                    _ => ("", ' '),
                };
                let body = fit(code, w.saturating_sub(12));
                let pad = w.saturating_sub(12 + width(&body));
                vec![format!(
                    "     {GRAY}{n:>4}{RESET} {bg}{sign} {body}{}{RESET}",
                    " ".repeat(pad)
                )]
            }
            Line::Todo(done, t) => vec![if *done {
                format!("     {GREEN}☒{RESET} {GRAY}\x1b[9m{}{RESET}", fit(t, w - 8))
            } else {
                format!("     ☐ {}", fit(t, w.saturating_sub(8)))
            }],
            Line::Dim(t) => vec![format!("{GRAY}{}{RESET}", fit(t, w))],
        }
    }

    fn footer(&self, w: usize) -> Vec<String> {
        let mut out = vec![];
        let rule = format!("{GRAY}{}{RESET}", "─".repeat(w));
        match (&self.mode, self.flavor) {
            (
                Mode::Working {
                    verb,
                    since,
                    tokens,
                },
                Flavor::Claude,
            ) => {
                let secs = since.elapsed().as_secs();
                let frames = ['·', '✢', '✳', '✶', '✻', '✽', '✻', '✶', '✳', '✢'];
                let f = frames[(since.elapsed().as_millis() / 120) as usize % frames.len()];
                let tk = if *tokens >= 1000 {
                    format!("{:.1}k", *tokens as f64 / 1000.0)
                } else {
                    tokens.to_string()
                };
                out.push(String::new());
                out.push(fit(
                    &format!(
                        "{ORANGE}{f} {verb}…{RESET} {GRAY}({secs}s · ↓ {tk} tokens · esc to interrupt){RESET}"
                    ),
                    w + 60,
                ));
                out.push(String::new());
            }
            (Mode::Working { since, .. }, Flavor::Grok) => {
                let frames = ['✶', '✸', '✹', '✺', '✹', '✸'];
                let f = frames[(since.elapsed().as_millis() / 140) as usize % frames.len()];
                out.push(String::new());
                out.push(format!(
                    " {BLUE}{f}{RESET} Thinking ({}s)   {GRAY}Press Ctrl+c to cancel the turn{RESET}",
                    since.elapsed().as_secs()
                ));
                out.push(String::new());
            }
            _ => {}
        }
        if let Mode::Asking(a) = &self.mode {
            out.extend(self.ask_box(a, w));
            return out;
        }
        match self.flavor {
            Flavor::Claude => {
                out.push(rule.clone());
                out.push(format!("❯ {}{}", self.input, "\x1b[7m \x1b[27m"));
                out.push(rule);
                let hint = match &self.notice {
                    Some((n, t)) if t.elapsed() < Duration::from_secs(3) => n.clone(),
                    _ if matches!(self.mode, Mode::Working { .. }) => {
                        "  ⏵⏵ auto mode on (shift+tab to cycle)".to_string()
                    }
                    _ => "  ? for shortcuts".to_string(),
                };
                out.push(format!("{GRAY}{}{RESET}", fit(&hint, w)));
            }
            Flavor::Grok => {
                let inner = w.saturating_sub(2);
                out.push(format!("{GRAY}╭{}╮{RESET}", "─".repeat(inner)));
                let body = if self.input.is_empty() {
                    format!("{GRAY} Type a message...{RESET}")
                } else {
                    format!(" > {}\x1b[7m \x1b[27m", fit(&self.input, inner - 5))
                };
                let plain = if self.input.is_empty() {
                    " Type a message...".to_string()
                } else {
                    format!(" > {} ", fit(&self.input, inner - 5))
                };
                let pad = inner.saturating_sub(width(&plain));
                out.push(format!(
                    "{GRAY}│{RESET}{body}{}{GRAY}│{RESET}",
                    " ".repeat(pad)
                ));
                out.push(format!("{GRAY}╰{}╯{RESET}", "─".repeat(inner)));
                let hint = match &self.notice {
                    Some((n, t)) if t.elapsed() < Duration::from_secs(3) => n.clone(),
                    _ => " grok-4.7-build-fast · auto-approve edits".to_string(),
                };
                out.push(format!("{GRAY}{}{RESET}", fit(&hint, w)));
            }
        }
        out
    }

    fn ask_box(&self, a: &Ask, w: usize) -> Vec<String> {
        let mut out = vec![];
        match self.flavor {
            Flavor::Claude => {
                let inner = w.saturating_sub(4);
                let row = |s: String| {
                    let pad = inner.saturating_sub(width(&strip(&s)));
                    format!("{BLUE}│{RESET} {s}{} {BLUE}│{RESET}", " ".repeat(pad))
                };
                out.push(format!(
                    "{BLUE}╭{}╮{RESET}",
                    "─".repeat(w.saturating_sub(2))
                ));
                out.push(row(format!("{BOLD}{}{RESET}", fit(&a.title, inner))));
                out.push(row(String::new()));
                for b in &a.body {
                    out.push(row(fit(&format!("  {b}"), inner)));
                }
                for (k, n, code) in &a.diff {
                    let (bg, sign) = match k {
                        '+' => (BG_ADD, '+'),
                        '-' => (BG_DEL, '-'),
                        _ => ("", ' '),
                    };
                    let body = fit(code, inner.saturating_sub(8));
                    let pad = inner.saturating_sub(8 + width(&body));
                    out.push(row(format!(
                        "{GRAY}{n:>4}{RESET} {bg}{sign} {body}{}{RESET}",
                        " ".repeat(pad)
                    )));
                }
                out.push(row(String::new()));
                out.push(row(fit(&a.question, inner)));
                for (i, o) in a.options.iter().enumerate() {
                    let label = fit(&format!("{}. {o}", i + 1), inner.saturating_sub(2));
                    out.push(row(if i == a.sel {
                        format!("{BLUE}❯ {label}{RESET}")
                    } else {
                        format!("  {label}")
                    }));
                }
                out.push(format!(
                    "{BLUE}╰{}╯{RESET}",
                    "─".repeat(w.saturating_sub(2))
                ));
            }
            Flavor::Grok => {
                out.push(format!(
                    " {ORANGE}●{RESET} {BOLD}{}{RESET}",
                    fit(&a.title, w.saturating_sub(4))
                ));
                for b in &a.body {
                    out.push(format!("   {GRAY}{}{RESET}", fit(b, w.saturating_sub(4))));
                }
                out.push(format!(" {}", a.question));
                for (i, o) in a.options.iter().enumerate() {
                    out.push(if i == a.sel {
                        format!(" {BLUE}› {o}{RESET}")
                    } else {
                        format!("   {o}")
                    });
                }
            }
        }
        out
    }

    /// The whole screen for `cols` x `rows`.
    fn frame(&self, cols: u16, rows: u16) -> String {
        let w = (cols as usize).max(20);
        let h = (rows as usize).max(5);
        let foot = self.footer(w);
        let mut body: Vec<String> = self.banner.clone();
        body.push(String::new());
        for l in &self.lines {
            body.extend(self.render_line(l, w));
        }
        let room = h.saturating_sub(foot.len());
        let start = body.len().saturating_sub(room);
        let mut rows_out: Vec<String> = body[start..].to_vec();
        // The footer sits right under the conversation (as in a real
        // terminal), or at the bottom once the screen is full.
        rows_out.extend(foot);
        rows_out.truncate(h);
        let mut s = String::from("\x1b[?25l\x1b[H");
        for (i, r) in rows_out.iter().enumerate() {
            s.push_str(r);
            s.push_str(RESET);
            s.push_str("\x1b[K");
            if i + 1 < rows_out.len() {
                s.push_str("\r\n");
            }
        }
        s.push_str("\x1b[J");
        s
    }
}

/// Without escape sequences (for widths).
fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            if c.is_ascii_alphabetic() {
                esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            esc = true;
            continue;
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------
// The interactive agent

enum Ui {
    Push(Line),
    Mode(Mode),
    Tokens(u64),
    Status(&'static str),
}

enum Input {
    Prompt(String),
    Answer(usize),
}

struct Ctx {
    ui: Sender<Ui>,
    input: Arc<Mutex<Receiver<Input>>>,
    interrupt: Arc<AtomicBool>,
    speed: f32,
    tokens: Arc<AtomicU64>,
    flavor: Flavor,
}

/// The task was interrupted (Esc).
struct Interrupted;

impl Ctx {
    fn push(&self, l: Line) {
        let _ = self.ui.send(Ui::Push(l));
    }

    fn sleep(&self, secs: f32) -> Result<(), Interrupted> {
        let end = Instant::now() + Duration::from_secs_f32((secs / self.speed).max(0.0));
        while Instant::now() < end {
            if self.interrupt.swap(false, Ordering::SeqCst) {
                return Err(Interrupted);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        Ok(())
    }

    fn work(&self, verb: &str) {
        let _ = self.ui.send(Ui::Mode(Mode::Working {
            verb: verb.to_string(),
            since: Instant::now(),
            tokens: 0,
        }));
        let _ = self.ui.send(Ui::Status("busy"));
    }

    fn add_tokens(&self, n: u64) {
        let t = self.tokens.fetch_add(n, Ordering::SeqCst) + n;
        let _ = self.ui.send(Ui::Tokens(t));
    }

    /// Show a permission prompt and wait for the answer (0: yes, 1:
    /// always, 2: no).
    fn ask(&self, a: Ask, verb: &str) -> Result<usize, Interrupted> {
        let _ = self.ui.send(Ui::Mode(Mode::Asking(a)));
        let _ = self.ui.send(Ui::Status("idle"));
        let rx = self.input.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if self.interrupt.swap(false, Ordering::SeqCst) {
                return Err(Interrupted);
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Input::Answer(n)) => {
                    drop(rx);
                    self.work(verb);
                    return Ok(n);
                }
                Ok(Input::Prompt(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err(Interrupted),
            }
        }
    }
}

fn usage_for(text_len: usize) -> [u64; 4] {
    let out = (text_len as u64 / 3).max(40) + rand_u64() % 400;
    let cw = 1200 + rand_u64() % 5200;
    let inp = 3 + rand_u64() % 12;
    let cr = 18_000 + rand_u64() % 40_000;
    [inp, out, cr, cw]
}

/// Run one task: the prompt was already shown and written.
fn run_task(
    ctx: &Ctx,
    tr: &Arc<Mutex<Transcript>>,
    task: &Task,
    cron: Option<&Cron>,
) -> Result<(), Interrupted> {
    let verbs = [
        "Pondering",
        "Reticulating",
        "Synthesizing",
        "Cogitating",
        "Brewing",
        "Noodling",
        "Architecting",
    ];
    let verb = verbs[(rand_u64() % verbs.len() as u64) as usize];
    let thinking = if ctx.flavor == Flavor::Grok {
        "Thinking"
    } else {
        verb
    };
    ctx.work(thinking);
    let tool = |name: &str, input: Value, result: &str, usage: [u64; 4]| {
        let id = format!("toolu_demo{:012x}", rand_u64() & 0xffff_ffff_ffff);
        let mut t = tr.lock().unwrap_or_else(|e| e.into_inner());
        t.assistant(
            json!([{"type": "tool_use", "id": id, "name": name, "input": input}]),
            usage,
        );
        t.tool_result(&id, result);
    };
    for step in &task.steps {
        match step {
            Step::Think { secs, verb } => {
                if !verb.is_empty() {
                    ctx.work(verb);
                }
                let n = (*secs * 4.0) as usize;
                for _ in 0..n.max(1) {
                    ctx.sleep(0.25)?;
                    ctx.add_tokens(20 + rand_u64() % 90);
                }
            }
            Step::Say { text } => {
                ctx.sleep(0.6)?;
                ctx.push(Line::Text(text.clone()));
                ctx.push(Line::Blank);
                let u = usage_for(text.len());
                ctx.add_tokens(u[1]);
                tr.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .assistant(json!([{"type": "text", "text": text}]), u);
            }
            Step::Read { path, lines } => {
                ctx.sleep(0.7)?;
                let n = if *lines == 0 { 120 } else { *lines };
                ctx.push(Line::Tool("Read".into(), path.clone()));
                ctx.sleep(0.4)?;
                ctx.push(Line::Result(format!("Read {n} lines")));
                ctx.push(Line::Blank);
                ctx.add_tokens(60);
                tool(
                    "Read",
                    json!({"file_path": path}),
                    &format!("{n} lines"),
                    usage_for(200),
                );
            }
            Step::Grep { pattern, matches } => {
                ctx.sleep(0.6)?;
                ctx.push(Line::Tool(
                    "Search".into(),
                    format!("pattern: \"{pattern}\""),
                ));
                ctx.sleep(0.4)?;
                let n = if *matches == 0 { 7 } else { *matches };
                ctx.push(Line::Result(format!("Found {n} files")));
                ctx.push(Line::Blank);
                tool(
                    "Grep",
                    json!({"pattern": pattern}),
                    &format!("Found {n} files"),
                    usage_for(150),
                );
            }
            Step::Edit {
                path,
                diff,
                approve,
            } => {
                ctx.sleep(0.9)?;
                let d = parse_diff(diff);
                if *approve {
                    let file = Path::new(path)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let a = match ctx.flavor {
                        Flavor::Claude => Ask {
                            title: "Edit file".into(),
                            body: vec![path.clone()],
                            diff: d.clone(),
                            question: format!("Do you want to make this edit to {file}?"),
                            options: vec![
                                "Yes".into(),
                                "Yes, allow all edits during this session (shift+tab)".into(),
                                "No, and tell Claude what to do differently (esc)".into(),
                            ],
                            sel: 0,
                        },
                        Flavor::Grok => grok_ask(&format!("Edit({path})"), "Apply this edit"),
                    };
                    if ask_denied(ctx, a, verb)? {
                        return Ok(());
                    }
                }
                let adds = d.iter().filter(|x| x.0 == '+').count();
                let dels = d.iter().filter(|x| x.0 == '-').count();
                ctx.push(Line::Tool("Update".into(), path.clone()));
                ctx.push(Line::Result(format!(
                    "Updated {path} with {adds} addition{} and {dels} removal{}",
                    if adds == 1 { "" } else { "s" },
                    if dels == 1 { "" } else { "s" }
                )));
                for (k, n, c) in d {
                    ctx.push(Line::Diff(k, n, c));
                }
                ctx.push(Line::Blank);
                ctx.add_tokens(300);
                tool(
                    "Edit",
                    json!({"file_path": path, "old_string": "", "new_string": ""}),
                    "The file has been updated.",
                    usage_for(900),
                );
            }
            Step::Write {
                path,
                lines,
                approve,
            } => {
                ctx.sleep(0.9)?;
                if *approve {
                    let a = match ctx.flavor {
                        Flavor::Claude => Ask {
                            title: "Create file".into(),
                            body: vec![path.clone()],
                            diff: vec![],
                            question: format!(
                                "Do you want to create {}?",
                                Path::new(path)
                                    .file_name()
                                    .map(|f| f.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            ),
                            options: vec![
                                "Yes".into(),
                                "Yes, allow all edits during this session (shift+tab)".into(),
                                "No, and tell Claude what to do differently (esc)".into(),
                            ],
                            sel: 0,
                        },
                        Flavor::Grok => grok_ask(&format!("Write({path})"), "Create this file"),
                    };
                    if ask_denied(ctx, a, verb)? {
                        return Ok(());
                    }
                }
                let n = if *lines == 0 { 48 } else { *lines };
                ctx.push(Line::Tool("Write".into(), path.clone()));
                ctx.push(Line::Result(format!("Wrote {n} lines to {path}")));
                ctx.push(Line::Blank);
                ctx.add_tokens(n as u64 * 9);
                tool(
                    "Write",
                    json!({"file_path": path, "content": ""}),
                    "File created successfully",
                    usage_for(n as usize * 30),
                );
            }
            Step::Bash {
                cmd,
                desc,
                out,
                approve,
                secs,
            } => {
                ctx.sleep(0.7)?;
                if *approve {
                    let first = cmd.split_whitespace().next().unwrap_or("these");
                    let a = match ctx.flavor {
                        Flavor::Claude => Ask {
                            title: "Bash command".into(),
                            body: vec![cmd.clone(), desc.clone()],
                            diff: vec![],
                            question: "Do you want to proceed?".into(),
                            options: vec![
                                "Yes".into(),
                                format!(
                                    "Yes, and don't ask again for {first} commands in this project"
                                ),
                                "No, and tell Claude what to do differently (esc)".into(),
                            ],
                            sel: 0,
                        },
                        Flavor::Grok => grok_ask(&format!("Bash({cmd})"), desc),
                    };
                    if ask_denied(ctx, a, verb)? {
                        return Ok(());
                    }
                }
                ctx.push(Line::Tool("Bash".into(), cmd.clone()));
                let per = if out.is_empty() {
                    *secs
                } else {
                    *secs / out.len() as f32
                };
                for (i, o) in out.iter().enumerate() {
                    ctx.sleep(per.max(0.15))?;
                    ctx.push(if i == 0 {
                        Line::Result(o.clone())
                    } else {
                        Line::More(o.clone())
                    });
                    ctx.add_tokens(15);
                }
                if out.is_empty() {
                    ctx.sleep(secs.max(0.4))?;
                    ctx.push(Line::Result("(no output)".into()));
                }
                ctx.push(Line::Blank);
                tool(
                    "Bash",
                    json!({"command": cmd, "description": desc}),
                    &out.join("\n"),
                    usage_for(out.join("\n").len() + 300),
                );
            }
            Step::Todo { items } => {
                ctx.sleep(0.5)?;
                ctx.push(Line::Tool("Update Todos".into(), String::new()));
                for (i, it) in items.iter().enumerate() {
                    ctx.push(Line::Todo(i == 0, it.clone()));
                }
                ctx.push(Line::Blank);
                tool(
                    "TodoWrite",
                    json!({"todos": items}),
                    "Todos have been modified successfully",
                    usage_for(300),
                );
            }
            Step::Wait { secs } => ctx.sleep(*secs)?,
        }
    }
    if let Some(c) = cron {
        // The first task sets the loop up.
        ctx.sleep(0.8)?;
        let job = format!("{:08x}", rand_u64() as u32);
        ctx.push(Line::Tool("CronCreate".into(), c.cron.clone()));
        ctx.push(Line::Result(format!(
            "Scheduled recurring job {job} ({})",
            c.human
        )));
        ctx.push(Line::Blank);
        let id = format!("toolu_demo{:012x}", rand_u64() & 0xffff_ffff_ffff);
        let mut t = tr.lock().unwrap_or_else(|e| e.into_inner());
        t.assistant(
            json!([{"type": "tool_use", "id": id, "name": "CronCreate", "input": {"cron": c.cron, "prompt": c.prompt, "recurring": true}}]),
            usage_for(200),
        );
        t.tool_result(
            &id,
            &format!(
                "Scheduled recurring job {job} ({}). Session-only (not written to disk, dies when Claude exits).",
                c.human
            ),
        );
        drop(t);
        JOB.lock().unwrap_or_else(|e| e.into_inner()).replace(job);
    }
    if !task.summary.is_empty() {
        ctx.sleep(0.8)?;
        ctx.push(Line::Text(task.summary.clone()));
        ctx.push(Line::Blank);
        let u = usage_for(task.summary.len());
        tr.lock()
            .unwrap_or_else(|e| e.into_inner())
            .assistant(json!([{"type": "text", "text": task.summary}]), u);
    }
    Ok(())
}

/// The loop's job id once set up.
static JOB: Mutex<Option<String>> = Mutex::new(None);

fn grok_ask(title: &str, desc: &str) -> Ask {
    Ask {
        title: title.to_string(),
        body: vec![desc.to_string()],
        diff: vec![],
        question: "Allow this command?".into(),
        options: vec![
            "Allow once".into(),
            "Always allow this command".into(),
            "Always allow on all sessions".into(),
            "Reject".into(),
        ],
        sel: 0,
    }
}

/// Ask; true when the answer was no (the task stops there).
fn ask_denied(ctx: &Ctx, a: Ask, verb: &str) -> Result<bool, Interrupted> {
    let n = a.options.len();
    let ans = ctx.ask(a, verb)?;
    if ans + 1 >= n {
        ctx.push(Line::Result("User rejected the tool call".into()));
        ctx.push(Line::Blank);
        return Ok(true);
    }
    Ok(false)
}

fn parse_diff(diff: &[String]) -> Vec<(char, u32, String)> {
    diff.iter()
        .map(|l| {
            let mut c = l.chars();
            let k = c.next().unwrap_or(' ');
            let rest: String = c.collect();
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let n = digits.parse().unwrap_or(0);
            let code = rest[digits.len()..]
                .strip_prefix(' ')
                .unwrap_or(&rest[digits.len()..])
                .to_string();
            (k, n, code)
        })
        .collect()
}

fn session_file(flavor: Flavor, config: &Path) -> Option<PathBuf> {
    (flavor == Flavor::Claude).then(|| {
        config
            .join("sessions")
            .join(format!("{}.json", std::process::id()))
    })
}

fn write_session(path: &Option<PathBuf>, sid: &str, cwd: &Path, status: &str) {
    if let Some(p) = path {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(
            p,
            json!({"pid": std::process::id(), "sessionId": sid, "cwd": cwd, "status": status,
                   "kind": "interactive", "startedAt": chrono::Utc::now().timestamp_millis()})
            .to_string(),
        );
    }
}

/// The agent's entry point.
pub fn run(flavor: Flavor) -> Result<()> {
    // Everything below stays inside the demo home (tabs and the brain get
    // none of GodTerm's variables, so they are set again from where the
    // agent lives).
    let home = demo_home();
    std::env::set_var(crate::demo::ENV, "1");
    if std::env::var_os("GODTERM_HOME").is_none() {
        std::env::set_var("GODTERM_HOME", home.join("app"));
    }
    std::env::set_var("GODTERM_MAIN_DIR", home.join("main").join("claude"));
    std::env::set_var("GODTERM_MAIN_GROK_DIR", home.join("main").join("grok"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-p" || a == "--print") {
        return brain(&args);
    }
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("2.1.291 (Claude Code)");
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("login") {
        println!("Already logged in (demo).");
        return Ok(());
    }
    let config = std::env::var_os(match flavor {
        Flavor::Claude => "CLAUDE_CONFIG_DIR",
        Flavor::Grok => "GROK_HOME",
    })
    .map(PathBuf::from)
    .unwrap_or_else(|| home.join("main").join("claude"));
    // Never write into the user's real dirs, whatever the environment says.
    if crate::config::is_real_user_dir(&config) {
        anyhow::bail!("demo agent: refusing {}", config.display());
    }
    let cwd = std::env::current_dir()?;
    let sc = load_scenario(&home, &cwd);
    let sid = args
        .iter()
        .position(|a| a == "--resume" || a == "-r" || a == "--session-id")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(crate::session_ops::new_uuid);
    let tpath = transcript_path(flavor, &config, &cwd, &sid);
    // A fresh session (one started by hand, like the external one) gets
    // the scenario's past turns first, so it has history to carry along.
    if !tpath.exists() && !sc.history.is_empty() {
        let mut t = Transcript::open(flavor, &config, &cwd, &sid, &sc.title);
        t.title_line();
        for h in &sc.history {
            t.user(&h.prompt, None);
            t.assistant(
                json!([{"type": "text", "text": h.summary}]),
                usage_for(h.summary.len()),
            );
        }
    }
    let (done, past) = replay(flavor, &tpath);
    let tr = Arc::new(Mutex::new(Transcript::open(
        flavor, &config, &cwd, &sid, &sc.title,
    )));
    tr.lock().unwrap_or_else(|e| e.into_inner()).title_line();
    let sfile = session_file(flavor, &config);
    write_session(&sfile, &sid, &cwd, "idle");

    let mut screen = Screen::new(flavor, &sc, &cwd);
    screen.lines = past;
    if !screen.lines.is_empty() {
        screen.lines.push(Line::Blank);
    }

    let (ui_tx, ui_rx) = mpsc::channel::<Ui>();
    let (in_tx, in_rx) = mpsc::channel::<Input>();
    let interrupt = Arc::new(AtomicBool::new(false));
    let ctx = Ctx {
        ui: ui_tx,
        input: Arc::new(Mutex::new(in_rx)),
        interrupt: Arc::clone(&interrupt),
        speed: sc.speed,
        tokens: Arc::new(AtomicU64::new(0)),
        flavor,
    };
    let history = sc.history.len();
    let tr2 = Arc::clone(&tr);
    std::thread::spawn(move || script(ctx, sc, tr2, done.saturating_sub(history)));

    crossterm::terminal::enable_raw_mode().ok();
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[?1049h\x1b[?2004h\x1b[2J");
    let _ = out.flush();
    let sigint = Arc::new(AtomicBool::new(false));
    let sigterm = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&sigint));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm));
    let mut ctrl_c: Option<Instant> = None;
    let mut status = "idle";
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let exit = |out: &mut std::io::Stdout| {
        let _ = write!(
            out,
            "\x1b[?2004l\x1b[?1049l\x1b[?25h{RESET}\r\nResume this session with:\r\n  claude --resume {sid}\r\n"
        );
        let _ = out.flush();
        crossterm::terminal::disable_raw_mode().ok();
    };
    loop {
        // Signals: a second SIGINT (or a SIGTERM) ends it, like claude.
        let mut quit = sigterm.load(Ordering::SeqCst);
        if sigint.swap(false, Ordering::SeqCst) {
            if ctrl_c.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
                quit = true;
            } else {
                ctrl_c = Some(Instant::now());
                interrupt.store(true, Ordering::SeqCst);
                screen.notice = Some(("  Press Ctrl-C again to exit".into(), Instant::now()));
            }
        }
        if quit {
            break;
        }
        while let Ok(m) = ui_rx.try_recv() {
            match m {
                Ui::Push(l) => screen.lines.push(l),
                Ui::Mode(m) => screen.mode = m,
                Ui::Tokens(t) => {
                    if let Mode::Working { tokens, .. } = &mut screen.mode {
                        *tokens = t;
                    }
                }
                Ui::Status(s) => {
                    if s != status {
                        status = s;
                        write_session(&sfile, &sid, &cwd, s);
                    }
                    if s == "idle" && !matches!(screen.mode, Mode::Asking(_)) {
                        screen.mode = Mode::Ready;
                    }
                }
            }
        }
        if screen.lines.len() > 600 {
            screen.lines.drain(..200);
        }
        // Keys.
        while crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false) {
            use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
            match crossterm::event::read() {
                Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                    let asking = matches!(screen.mode, Mode::Asking(_));
                    match k.code {
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            sigint.store(true, Ordering::SeqCst)
                        }
                        KeyCode::Char(c @ '1'..='9') if asking => {
                            let n = c as usize - '1' as usize;
                            if let Mode::Asking(a) = &screen.mode {
                                if n < a.options.len() {
                                    let _ = in_tx.send(Input::Answer(n));
                                    screen.mode = Mode::Working {
                                        verb: "Working".into(),
                                        since: Instant::now(),
                                        tokens: 0,
                                    };
                                }
                            }
                        }
                        KeyCode::Up | KeyCode::Down if asking => {
                            if let Mode::Asking(a) = &mut screen.mode {
                                let n = a.options.len();
                                a.sel = if k.code == KeyCode::Up {
                                    (a.sel + n - 1) % n
                                } else {
                                    (a.sel + 1) % n
                                };
                            }
                        }
                        KeyCode::Enter if asking => {
                            if let Mode::Asking(a) = &screen.mode {
                                let _ = in_tx.send(Input::Answer(a.sel));
                                screen.mode = Mode::Working {
                                    verb: "Working".into(),
                                    since: Instant::now(),
                                    tokens: 0,
                                };
                            }
                        }
                        KeyCode::Esc if asking => {
                            if let Mode::Asking(a) = &screen.mode {
                                let _ = in_tx.send(Input::Answer(a.options.len() - 1));
                            }
                        }
                        KeyCode::Esc => {
                            if matches!(screen.mode, Mode::Working { .. }) {
                                interrupt.store(true, Ordering::SeqCst);
                            }
                        }
                        KeyCode::Enter => {
                            let t = std::mem::take(&mut screen.input);
                            if !t.trim().is_empty() {
                                let _ = in_tx.send(Input::Prompt(t));
                            }
                        }
                        KeyCode::Backspace => {
                            screen.input.pop();
                        }
                        KeyCode::Char(c) if !asking => screen.input.push(c),
                        _ => {}
                    }
                }
                Ok(Event::Paste(p)) => screen.input.push_str(&p.replace(['\r', '\n'], " ")),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if last_draw.elapsed() >= Duration::from_millis(80) {
            let (c, r) = crossterm::terminal::size().unwrap_or((80, 24));
            let f = screen.frame(c, r);
            if f != screen.last_frame {
                let _ = out.write_all(f.as_bytes());
                let _ = out.flush();
                screen.last_frame = f;
            }
            last_draw = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    if let Some(p) = &sfile {
        let _ = std::fs::remove_file(p);
    }
    exit(&mut out);
    Ok(())
}

/// The task runner thread.
fn script(ctx: Ctx, sc: Scenario, tr: Arc<Mutex<Transcript>>, mut next: usize) {
    let run_one = |task: &Task, scheduled: Option<&str>, cron: Option<&Cron>| {
        ctx.push(Line::User(task.prompt.clone()));
        ctx.push(Line::Blank);
        tr.lock()
            .unwrap_or_else(|e| e.into_inner())
            .user(&task.prompt, scheduled);
        if run_task(&ctx, &tr, task, cron).is_err() {
            ctx.push(Line::Result("Interrupted by user".into()));
            ctx.push(Line::Blank);
        }
        ctx.tokens.store(0, Ordering::SeqCst);
        let _ = ctx.ui.send(Ui::Status("idle"));
    };
    let mut replies = sc.replies.iter().cycle();
    let mut last_fire = Instant::now();
    let mut first = true;
    let mut ran = 0usize;
    let mut idle_until = Instant::now() + Duration::from_secs_f32(sc.start_delay / sc.speed);
    loop {
        // A prompt typed or sent into the tab comes first.
        let prompt = {
            let rx = ctx.input.lock().unwrap_or_else(|e| e.into_inner());
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Input::Prompt(p)) => Some(p),
                Ok(Input::Answer(_)) | Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => return,
            }
        };
        if let Some(p) = prompt {
            let mut t = replies.next().cloned().unwrap_or_default();
            t.prompt = p;
            run_one(&t, None, None);
            idle_until = Instant::now() + Duration::from_secs_f32(rand_in(sc.idle[0], sc.idle[1]));
            continue;
        }
        // The loop fires when idle.
        if let Some(c) = &sc.cron {
            let job = JOB.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(job) = job {
                if c.every > 0.0 && last_fire.elapsed().as_secs_f32() > c.every / sc.speed {
                    let mut t = c.fire.clone();
                    if t.prompt.is_empty() {
                        t.prompt = c.prompt.clone();
                    }
                    run_one(&t, Some(&job), None);
                    last_fire = Instant::now();
                    continue;
                }
            }
        }
        if !sc.autorun
            || Instant::now() < idle_until
            || (sc.pause_after > 0 && ran >= sc.pause_after)
        {
            continue;
        }
        if next >= sc.tasks.len() {
            if !sc.repeat || sc.tasks.is_empty() {
                continue;
            }
            next = 0;
        }
        let task = sc.tasks[next].clone();
        let cron = if first && next == 0 {
            sc.cron.as_ref()
        } else {
            None
        };
        first = false;
        next += 1;
        ran += 1;
        run_one(&task, None, cron);
        last_fire = Instant::now();
        idle_until =
            Instant::now() + Duration::from_secs_f32(rand_in(sc.idle[0], sc.idle[1]) / sc.speed);
    }
}

// ---------------------------------------------------------------------
// The scripted brain (`claude -p --input-format stream-json ...`)

fn emit(v: Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

fn brain(args: &[String]) -> Result<()> {
    // The godterm tools: the token and home from the mcp config.
    if let Some(p) = args
        .iter()
        .position(|a| a == "--mcp-config")
        .and_then(|i| args.get(i + 1))
    {
        if let Ok(t) = std::fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                if let Some(env) = v["mcpServers"]["godterm"]["env"].as_object() {
                    for (k, val) in env {
                        if let Some(s) = val.as_str() {
                            std::env::set_var(k, s);
                        }
                    }
                }
            }
        }
    }
    emit(json!({"type": "system", "subtype": "init", "model": "claude-haiku-demo", "tools": []}));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["type"] != "user" {
            continue;
        }
        let text = match &v["message"]["content"] {
            Value::String(s) => s.clone(),
            Value::Array(a) => a
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        let asked = text
            .split("\n\n<state>")
            .next()
            .unwrap_or("")
            .lines()
            .last()
            .unwrap_or("")
            .replace("(spoken) ", "");
        let reply = brain_turn(&asked);
        stream_text(&reply);
        emit(
            json!({"type": "result", "subtype": "success", "result": reply, "is_error": false, "total_cost_usd": 0.0}),
        );
    }
    Ok(())
}

/// Call a control tool the way claude would: tool_use, then its result.
fn call_tool(name: &str, input: Value) -> Value {
    let id = format!("toolu_brain{:010x}", rand_u64() & 0xff_ffff_ffff);
    emit(
        json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": format!("mcp__godterm__{name}"), "input": input}
        ]}}),
    );
    let out = crate::control::call(name, &input)
        .unwrap_or_else(|e| json!({"ok": false, "error": e.to_string()}));
    emit(
        json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": out.to_string()}
        ]}}),
    );
    out
}

fn stream_text(t: &str) {
    emit(
        json!({"type": "stream_event", "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}}),
    );
    for w in t.split_inclusive(' ') {
        emit(
            json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": w}}}),
        );
        std::thread::sleep(Duration::from_millis(28));
    }
    emit(
        json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": t}]}}),
    );
}

/// Words to the account they name ("research", "account three").
fn account_named(state: &Value, text: &str) -> Option<(u64, String)> {
    let t = text.to_lowercase();
    state["result"]["accounts"]
        .as_array()
        .or_else(|| state["accounts"].as_array())
        .into_iter()
        .flatten()
        .find(|a| {
            let l = a["label"].as_str().unwrap_or("").to_lowercase();
            !l.is_empty() && t.contains(&l)
        })
        .map(|a| {
            (
                a["account"].as_u64().unwrap_or(0),
                a["label"].as_str().unwrap_or("").to_string(),
            )
        })
}

fn accounts_of(state: &Value) -> Vec<Value> {
    state["result"]["accounts"]
        .as_array()
        .or_else(|| state["accounts"].as_array())
        .cloned()
        .unwrap_or_default()
}

/// The (label, left %) of the Claude account with the most 5 hour left.
fn best_account(state: &Value) -> Option<(String, f64)> {
    accounts_of(state)
        .iter()
        .filter(|a| a["available"].as_bool() != Some(false))
        .filter_map(|a| {
            Some((
                a["label"].as_str()?.to_string(),
                a["five_hour_left_pct"].as_f64()?,
            ))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

fn spoken_pct(p: f64) -> String {
    format!("{p:.0} percent")
}

/// The canned answer to one request (calling the real tools on the way).
pub fn brain_turn(asked: &str) -> String {
    let q = asked.to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| q.contains(x));
    if has(&["most left", "most quota", "which account", "best account"]) && !has(&["move", "open"])
    {
        let st = call_tool("get_state", json!({}));
        return match best_account(&st) {
            Some((l, p)) => format!(
                "{l} has the most left: {} of its five hour window.",
                spoken_pct(p)
            ),
            None => "I can't read the usage right now.".into(),
        };
    }
    if has(&[
        "working on",
        "what's going on",
        "whats going on",
        "status",
        "everyone",
    ]) {
        let st = call_tool("get_state", json!({}));
        let tabs = st["result"]["tabs"]
            .as_array()
            .or_else(|| st["tabs"].as_array())
            .cloned()
            .unwrap_or_default();
        let n_acc = accounts_of(&st).len();
        let waiting: Vec<String> = tabs
            .iter()
            .filter(|t| t["state"].as_str().unwrap_or("").contains("approval"))
            .filter_map(|t| t["name"].as_str().map(|n| n.replace('-', " ")))
            .collect();
        let mut s = format!(
            "{} sessions on {} accounts. Payments API is adding webhook retries, the ML pipeline is training",
            number_word(tabs.len()),
            number_word(n_acc).to_lowercase()
        );
        match waiting.len() {
            0 => s.push('.'),
            1 => s.push_str(&format!(", and {} wants your approval.", waiting[0])),
            n => s.push_str(&format!(
                ", and {} tabs want your approval.",
                number_word(n).to_lowercase()
            )),
        }
        return s;
    }
    if has(&["open", "new tab", "start"]) && has(&["build", "make", "create", "tab"]) {
        let st = call_tool("get_state", json!({}));
        let (acct, label) = account_named(&st, &q)
            .or_else(|| best_account(&st).and_then(|(l, _)| account_named(&st, &l.to_lowercase())))
            .unwrap_or((1, "your first account".into()));
        let task = q
            .split_once(" and ")
            .map(|(_, t)| t.trim().trim_end_matches(['.', '?', '!']).to_string())
            .unwrap_or_else(|| "build a pricing page".into());
        let name = if task.contains("pricing") {
            "pricing-page".to_string()
        } else {
            crate::control::slug(&task).chars().take(24).collect()
        };
        let _ = call_tool(
            "open_tab",
            json!({"account": acct, "name": name, "prompt": capitalize(&task)}),
        );
        return format!("On it. {label} has the most left, so the pricing page starts there.")
            .replace(
                "the pricing page",
                if name == "pricing-page" {
                    "the pricing page"
                } else {
                    "it"
                },
            );
    }
    if has(&["move"]) {
        let _ = call_tool("move_tab", json!({"to": "best"}));
        return "Moving it to the account with the most left. The conversation carries on there."
            .into();
    }
    if has(&["live map", "map"]) {
        let _ = call_tool("show", json!({"view": "livemap"}));
        return "Here's the live map.".into();
    }
    if has(&["approval", "approve", "waiting"]) {
        let _ = call_tool("show", json!({"view": "approvals"}));
        return "Here are the approvals waiting for you.".into();
    }
    "Done.".into()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn number_word(n: usize) -> String {
    const W: [&str; 21] = [
        "No",
        "One",
        "Two",
        "Three",
        "Four",
        "Five",
        "Six",
        "Seven",
        "Eight",
        "Nine",
        "Ten",
        "Eleven",
        "Twelve",
        "Thirteen",
        "Fourteen",
        "Fifteen",
        "Sixteen",
        "Seventeen",
        "Eighteen",
        "Nineteen",
        "Twenty",
    ];
    W.get(n).map(|s| s.to_string()).unwrap_or(n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_lines_parse() {
        let d = parse_diff(&["+42 let x = 1;".into(), "-41 old".into(), " 40 ctx".into()]);
        assert_eq!(d[0], ('+', 42, "let x = 1;".into()));
        assert_eq!(d[1], ('-', 41, "old".into()));
        assert_eq!(d[2], (' ', 40, "ctx".into()));
    }

    #[test]
    fn screens_read_like_the_real_agents() {
        let sc = Scenario::default();
        let cwd = std::env::temp_dir().join("demo-proj");
        let mut s = Screen::new(Flavor::Claude, &sc, &cwd);
        s.lines.push(Line::User("add tests".into()));
        s.mode = Mode::Working {
            verb: "Pondering".into(),
            since: Instant::now(),
            tokens: 1200,
        };
        let plain = strip(&s.frame(80, 24));
        assert_eq!(
            crate::pane::detect_activity(&plain),
            crate::pane::Activity::Working
        );
        s.mode = Mode::Ready;
        let plain = strip(&s.frame(80, 24));
        assert_eq!(
            crate::pane::detect_activity(&plain),
            crate::pane::Activity::Ready
        );
        s.mode = Mode::Asking(Ask {
            title: "Bash command".into(),
            body: vec!["rm -rf build".into(), "Remove the build directory".into()],
            diff: vec![],
            question: "Do you want to proceed?".into(),
            options: vec![
                "Yes".into(),
                "Yes, and don't ask again for rm commands in this project".into(),
                "No, and tell Claude what to do differently (esc)".into(),
            ],
            sel: 0,
        });
        // The frame is drawn with cursor moves; as a screen it is lines.
        let plain = strip(&s.frame(80, 30)).replace("\r\n", "\n");
        assert_eq!(
            crate::pane::detect_activity(&plain),
            crate::pane::Activity::Permission
        );
        let p = crate::prompt::parse_prompt(&plain).expect("a prompt");
        assert_eq!(p.options.len(), 3);
        // grok's screens too.
        let mut g = Screen::new(Flavor::Grok, &sc, &cwd);
        g.mode = Mode::Working {
            verb: "Thinking".into(),
            since: Instant::now(),
            tokens: 0,
        };
        assert_eq!(
            crate::pane::detect_activity(&strip(&g.frame(80, 24))),
            crate::pane::Activity::Working
        );
        g.mode = Mode::Ready;
        assert_eq!(
            crate::pane::detect_activity(&strip(&g.frame(80, 24))),
            crate::pane::Activity::Ready
        );
        g.mode = Mode::Asking(grok_ask("Bash(rm -rf build)", "Remove the build directory"));
        let plain = strip(&g.frame(80, 24)).replace("\r\n", "\n");
        assert_eq!(
            crate::pane::detect_activity(&plain),
            crate::pane::Activity::Permission
        );
    }

    #[test]
    fn transcripts_count_tokens_and_prompts() {
        let d = std::env::temp_dir().join(format!("godterm-demo-tr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let cwd = d.join("proj");
        let mut t = Transcript::open(Flavor::Claude, &d, &cwd, "s-1", "Title");
        t.title_line();
        t.user("first", None);
        t.assistant(json!([{"type": "text", "text": "hi"}]), [10, 20, 30, 40]);
        t.user("tick", Some("abcd1234"));
        let p = transcript_path(Flavor::Claude, &d, &cwd, "s-1");
        let (prompts, lines) = replay(Flavor::Claude, &p);
        assert_eq!(prompts, 1, "scheduled fires are not prompts");
        assert!(lines.len() >= 3);
        let info = crate::sessions::parse_file(&p).expect("parses");
        assert_eq!(info.title.as_deref(), Some("Title"));
        assert!(info.tokens.output >= 20);
        let _ = std::fs::remove_dir_all(&d);
    }
}
