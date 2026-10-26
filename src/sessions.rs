//! Scanning Claude Code transcripts in `<configDir>/projects/*/*.jsonl`.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    /// Cache reads repeat the whole context on every turn: kept apart,
    /// never added into a headline figure.
    pub cache_read: u64,
    /// The largest context one turn had (input + cache read + cache write).
    #[serde(default)]
    pub context: u64,
}

impl Tokens {
    /// New tokens: fresh input (with cache writes) and output. Cache reads
    /// are left out, they count the same context again every turn.
    pub fn total(&self) -> u64 {
        self.input + self.cache_creation + self.output
    }
    fn add_usage(&mut self, u: &Value) {
        let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        self.input += g("input_tokens");
        self.output += g("output_tokens");
        self.cache_creation += g("cache_creation_input_tokens");
        self.cache_read += g("cache_read_input_tokens");
        self.context = self.context.max(
            g("input_tokens") + g("cache_creation_input_tokens") + g("cache_read_input_tokens"),
        );
    }
    /// A subagent's tokens into its parent (its context is its own).
    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_creation += o.cache_creation;
        self.cache_read += o.cache_read;
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    /// Working directory recorded in the transcript, or decoded from the
    /// project dir name as a fallback.
    pub cwd: String,
    pub modified: Option<SystemTime>,
    pub messages: usize,
    pub first_prompt: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub tokens: Tokens,
    /// The most recent prompt (for search and "what was I doing").
    pub last_prompt: Option<String>,
    /// Subagent (Task / Agent tool) transcripts under this session; their
    /// tokens are rolled into `tokens`.
    pub subagents: usize,
    /// For a subagent row (include_subagents): the parent session id.
    pub parent: Option<String>,
    /// Counts estimated from the start and end of a very large transcript.
    #[serde(default)]
    pub approx: bool,
    /// When it started (the first timestamp in it).
    #[serde(default)]
    pub created: Option<SystemTime>,
    /// The name the user gave the session (/rename, "VARIUM REFACTOR"):
    /// it wins over the generated title.
    #[serde(default)]
    pub name: Option<String>,
    /// Run without a person at a terminal: claude -p / the SDK
    /// (entrypoint "sdk-cli") or grok's headless mode.
    #[serde(default)]
    pub headless: bool,
    /// Files its tool calls named (Read, Edit, Write...), at most
    /// `MAX_FILES`, for finding a session by what it worked on.
    #[serde(default)]
    pub files: Vec<String>,
}

/// Touched files kept per session.
pub const MAX_FILES: usize = 40;

/// Keep a file path a tool call named, once, up to `MAX_FILES`.
fn note_file(files: &mut Vec<String>, p: &str) {
    if files.len() < MAX_FILES
        && p.len() < 400
        && (p.starts_with('/') || p.starts_with('~') || p.contains(":\\"))
        && !files.iter().any(|f| f == p)
    {
        files.push(p.to_string());
    }
}

impl SessionInfo {
    pub fn summary(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.title.clone())
            .or_else(|| self.first_prompt.clone())
            .unwrap_or_else(|| "(no prompt)".into())
    }
}

/// Best effort reverse of Claude's project dir encoding (every non
/// alphanumeric char becomes '-'). Lossy, so the jsonl `cwd` is preferred.
pub fn decode_project_dir(name: &str) -> String {
    if name.starts_with('-') {
        name.replace('-', "/")
    } else {
        name.to_string()
    }
}

fn text_of_content(c: &Value) -> Option<String> {
    match c {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut has_tool_result = false;
            let mut texts = vec![];
            for it in items {
                match it.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = it.get("text").and_then(Value::as_str) {
                            texts.push(t.to_string());
                        }
                    }
                    Some("tool_result") => has_tool_result = true,
                    _ => {}
                }
            }
            if has_tool_result || texts.is_empty() {
                None
            } else {
                Some(texts.join(" "))
            }
        }
        _ => None,
    }
}

/// Collapse whitespace and truncate to `max` chars.
pub fn snippet(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn is_noise_prompt(s: &str) -> bool {
    let t = s.trim_start();
    t.is_empty()
        || t.starts_with('<')
        || t.starts_with("Caveat:")
        || t.starts_with("[Request interrupted")
}

/// Parse one transcript from any reader. Unparsable lines are skipped.
#[cfg(test)]
pub fn parse_transcript<R: BufRead>(reader: R, id: &str) -> SessionInfo {
    parse_transcript_as(reader, id, false)
}

/// A file under `<session>/subagents/`: every line there is a sidechain
/// line, and they are its own prompts and replies.
pub fn is_subagent_file(p: &Path) -> bool {
    p.parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d == "subagents")
}

pub fn parse_transcript_as<R: BufRead>(reader: R, id: &str, subagent: bool) -> SessionInfo {
    let mut info = SessionInfo {
        id: id.to_string(),
        ..Default::default()
    };
    // Streaming writes one line per content block, all sharing message.id and
    // repeating usage. Keep the last usage seen per id.
    let mut usage_by_id: HashMap<String, Value> = HashMap::new();
    let mut anon_usage: Vec<Value> = vec![];
    let mut assistant_ids: HashSet<String> = HashSet::new();
    let mut user_msgs = 0usize;

    for line in lossy_lines(reader) {
        if line.trim().is_empty() {
            continue;
        }
        // Big tool results (file dumps, command output) are neither prompts
        // nor usage: skip parsing them, they are most of the time spent.
        if line.len() > 16 * 1024 && line.contains("\"tool_result\"") && !line.contains("\"usage\"")
        {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if info.cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                info.cwd = c.to_string();
            }
        }
        if info.created.is_none() {
            if let Some(t) = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            {
                info.created = Some(SystemTime::from(t));
            }
        }
        if v.get("entrypoint").and_then(Value::as_str) == Some("sdk-cli") {
            info.headless = true;
        }
        let sidechain = !subagent
            && v.get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let meta = v.get("isMeta").and_then(Value::as_bool).unwrap_or(false);
        match v.get("type").and_then(Value::as_str) {
            Some("user") if !sidechain && !meta => {
                if let Some(text) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(text_of_content)
                {
                    user_msgs += 1;
                    if !is_noise_prompt(&text) {
                        if info.first_prompt.is_none() {
                            info.first_prompt = Some(snippet(&text, 160));
                        }
                        info.last_prompt = Some(snippet(&text, 160));
                    }
                }
            }
            Some("assistant") => {
                let Some(msg) = v.get("message") else {
                    continue;
                };
                let mid = msg.get("id").and_then(Value::as_str).map(str::to_string);
                if let Some(u) = msg.get("usage") {
                    match &mid {
                        Some(id) => {
                            usage_by_id.insert(id.clone(), u.clone());
                        }
                        None => anon_usage.push(u.clone()),
                    }
                }
                if !sidechain {
                    for b in msg
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                    {
                        let i = &b["input"];
                        for k in ["file_path", "notebook_path", "path"] {
                            if let Some(p) = i.get(k).and_then(Value::as_str) {
                                note_file(&mut info.files, p);
                            }
                        }
                    }
                    if let Some(m) = msg.get("model").and_then(Value::as_str) {
                        if m != "<synthetic>" {
                            info.model = Some(m.to_string());
                        }
                    }
                    match mid {
                        Some(id) => {
                            assistant_ids.insert(id);
                        }
                        None => user_msgs += 1,
                    }
                }
            }
            Some("custom-title") => {
                if let Some(t) = v
                    .get("customTitle")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                {
                    info.name = Some(snippet(t, 120));
                }
            }
            Some("ai-title") => {
                if let Some(t) = v.get("aiTitle").and_then(Value::as_str) {
                    info.title = Some(snippet(t, 120));
                }
            }
            Some("summary") if info.title.is_none() => {
                if let Some(t) = v.get("summary").and_then(Value::as_str) {
                    info.title = Some(snippet(t, 120));
                }
            }
            _ => {}
        }
    }
    for u in usage_by_id.values().chain(anon_usage.iter()) {
        info.tokens.add_usage(u);
    }
    info.messages = user_msgs + assistant_ids.len();
    info
}

/// Like `parse_file`, but a very large transcript (over 24 MB) is read
/// only at its start (prompts, cwd) and end (latest prompts, title,
/// context), with message and token counts scaled up from those parts.
pub fn parse_file_quick(path: &Path) -> Option<SessionInfo> {
    use std::io::{Read, Seek, SeekFrom};
    const BIG: u64 = 24 << 20;
    const HEAD: u64 = 3 << 20;
    const TAIL: u64 = 6 << 20;
    let len = fs::metadata(path).ok()?.len();
    if len <= BIG {
        return parse_file(path);
    }
    let mut f = fs::File::open(path).ok()?;
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let mut head = vec![0u8; HEAD as usize];
    f.read_exact(&mut head).ok()?;
    let cut = head.iter().rposition(|b| *b == b'\n').unwrap_or(0);
    head.truncate(cut + 1);
    f.seek(SeekFrom::Start(len - TAIL)).ok()?;
    let mut tail = vec![];
    f.read_to_end(&mut tail).ok()?;
    let start = tail
        .iter()
        .position(|b| *b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let tail = &tail[start..];
    let sub = is_subagent_file(path);
    let a = parse_transcript_as(BufReader::new(&head[..]), &id, sub);
    let b = parse_transcript_as(BufReader::new(tail), &id, sub);
    let sampled = (head.len() + tail.len()) as f64;
    let k = len as f64 / sampled.max(1.0);
    let scale = |x: u64| (x as f64 * k) as u64;
    let mut info = a.clone();
    info.messages = ((a.messages + b.messages) as f64 * k) as usize;
    info.tokens = Tokens {
        input: scale(a.tokens.input + b.tokens.input),
        output: scale(a.tokens.output + b.tokens.output),
        cache_creation: scale(a.tokens.cache_creation + b.tokens.cache_creation),
        cache_read: scale(a.tokens.cache_read + b.tokens.cache_read),
        context: a.tokens.context.max(b.tokens.context),
    };
    info.title = b.title.or(a.title);
    info.name = b.name.or(a.name);
    info.last_prompt = b.last_prompt.or(a.last_prompt);
    info.model = b.model.or(a.model);
    for f in &b.files {
        note_file(&mut info.files, f);
    }
    info.approx = true;
    info.path = path.to_path_buf();
    info.modified = fs::metadata(path).and_then(|m| m.modified()).ok();
    if info.cwd.is_empty() {
        if let Some(dir) = path.parent().and_then(Path::file_name) {
            info.cwd = decode_project_dir(&dir.to_string_lossy());
        }
    }
    Some(info)
}

pub fn parse_file(path: &Path) -> Option<SessionInfo> {
    let f = fs::File::open(path).ok()?;
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let mut info = parse_transcript_as(BufReader::new(f), &id, is_subagent_file(path));
    info.path = path.to_path_buf();
    info.modified = fs::metadata(path).and_then(|m| m.modified()).ok();
    if info.cwd.is_empty() {
        if let Some(dir) = path.parent().and_then(Path::file_name) {
            info.cwd = decode_project_dir(&dir.to_string_lossy());
        }
    }
    Some(info)
}

/// Cache keyed by path, invalidated by (mtime, len).
#[derive(Default)]
pub struct SessionCache {
    entries: HashMap<PathBuf, (SystemTime, u64, SessionInfo)>,
}

/// Parsed sessions kept per cache (`transcript_cache`).
static CACHE_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(50);

pub fn set_cache_limit(n: usize) {
    CACHE_LIMIT.store(n, std::sync::atomic::Ordering::Relaxed);
}

impl SessionCache {
    /// All sessions for one config dir, newest first. Empty transcripts
    /// (no messages) are dropped.
    pub fn scan(&mut self, config_dir: &Path) -> Vec<SessionInfo> {
        let mut out = vec![];
        let Ok(projects) = fs::read_dir(config_dir.join("projects")) else {
            return out;
        };
        for proj in projects.flatten() {
            if !proj.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            // The assistant's own kept session is not one of the user's.
            if crate::app_resume::is_assistant_project(&proj.file_name().to_string_lossy()) {
                continue;
            }
            let Ok(files) = fs::read_dir(proj.path()) else {
                continue;
            };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(meta) = f.metadata() else { continue };
                let (mtime, len) = (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                );
                let info = match self.entries.get(&p) {
                    Some((m, l, info)) if *m == mtime && *l == len => info.clone(),
                    _ => match parse_file(&p) {
                        Some(info) => {
                            self.entries.insert(p.clone(), (mtime, len, info.clone()));
                            info
                        }
                        None => continue,
                    },
                };
                if info.messages > 0 {
                    out.push(info);
                }
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.modified));
        // Keep only the newest parsed entries; older ones are parsed again
        // when needed.
        let limit = CACHE_LIMIT.load(std::sync::atomic::Ordering::Relaxed);
        if self.entries.len() > limit {
            let keep: std::collections::HashSet<PathBuf> =
                out.iter().take(limit).map(|s| s.path.clone()).collect();
            self.entries.retain(|p, _| keep.contains(p));
        }
        out
    }
}

/// Lines of a reader, tolerating invalid UTF-8 (replaced) instead of
/// stopping, and skipping absurdly long lines (over 64 MB) without holding
/// them in memory twice.
pub fn lossy_lines<R: BufRead>(mut reader: R) -> impl Iterator<Item = String> {
    std::iter::from_fn(move || {
        let mut buf = Vec::new();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                if buf.len() > 64 * 1024 * 1024 {
                    return Some(String::new());
                }
                Some(
                    String::from_utf8_lossy(&buf)
                        .trim_end_matches(['\n', '\r'])
                        .to_string(),
                )
            }
        }
    })
}

/// Text of the last assistant message in a transcript (main chain only).
/// Streaming writes one line per content block, so text blocks sharing a
/// message id are joined.
pub fn last_assistant_text(path: &Path) -> Option<String> {
    use std::io::{Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    // Only the tail matters; skip most of a huge transcript.
    const TAIL: u64 = 4 * 1024 * 1024;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let skip_partial = len > TAIL;
    if skip_partial {
        f.seek(SeekFrom::Start(len - TAIL)).ok()?;
    }
    let mut last: Option<(String, Vec<String>)> = None;
    for (i, line) in lossy_lines(BufReader::new(f)).enumerate() {
        if i == 0 && skip_partial {
            continue;
        }
        if !line.contains("\"assistant\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant")
            || v.get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            continue;
        }
        let Some(msg) = v.get("message") else {
            continue;
        };
        let id = msg
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let texts: Vec<String> = msg
            .get("content")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if texts.is_empty() {
            continue;
        }
        match &mut last {
            Some((lid, parts)) if *lid == id && !id.is_empty() => parts.extend(texts),
            _ => last = Some((id, texts)),
        }
    }
    last.map(|(_, p)| p.join("\n\n"))
        .filter(|t| !t.trim().is_empty())
}

/// One exchange in a transcript: what was asked, and the last text the
/// assistant wrote for it (its answer when the turn is over).
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub user: String,
    pub reply: Option<String>,
    pub at: Option<String>,
    /// Its last message is text with no tool call after it (the turn is
    /// over, not paused between tools).
    pub finished: bool,
    /// Messages typed while this turn ran, which claude took into it
    /// instead of starting a turn of their own (a queued command
    /// attachment, or the queue entry it absorbed mid turn).
    pub queued: Vec<String>,
    /// When the reply's last text was written.
    pub reply_at: Option<String>,
}

/// The last `n` exchanges of a transcript (main chain only), oldest first.
pub fn recent_turns(path: &Path, n: usize) -> Vec<Turn> {
    let mut turns = scan_turns(path);
    turns.retain(|t| !t.user.is_empty());
    let k = turns.len().saturating_sub(n);
    turns.split_off(k)
}

/// Like recent_turns, but a turn whose start is before the part of a big
/// transcript that is read is kept too (with an empty `user`): its
/// queued messages and reply still count when following up on a
/// question sent while it ran.
pub fn follow_turns(path: &Path, n: usize) -> Vec<Turn> {
    let mut turns = scan_turns(path);
    let k = turns.len().saturating_sub(n);
    turns.split_off(k)
}

fn blank_turn() -> Turn {
    Turn {
        user: String::new(),
        reply: None,
        at: None,
        finished: false,
        queued: vec![],
        reply_at: None,
    }
}

fn scan_turns(path: &Path) -> Vec<Turn> {
    use std::io::{Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(path) else {
        return vec![];
    };
    const TAIL: u64 = 4 * 1024 * 1024;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let skip_partial = len > TAIL;
    if skip_partial && f.seek(SeekFrom::Start(len - TAIL)).is_err() {
        return vec![];
    }
    let mut turns: Vec<Turn> = vec![];
    // The message id the current reply text belongs to.
    let mut msg_id = String::new();
    for (i, line) in lossy_lines(BufReader::new(f)).enumerate() {
        if i == 0 && skip_partial {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || v.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        {
            continue;
        }
        let at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                let text = match &v["message"]["content"] {
                    Value::String(s) => Some(s.clone()),
                    Value::Array(a) => {
                        let t: Vec<&str> = a
                            .iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect();
                        (!t.is_empty()).then(|| t.join("\n"))
                    }
                    _ => None,
                };
                match text.filter(|t| !t.trim().is_empty() && !t.starts_with('<')) {
                    Some(t) => {
                        turns.push(Turn {
                            user: t,
                            at,
                            ..blank_turn()
                        });
                        msg_id.clear();
                    }
                    // A tool result: the turn goes on.
                    None => {
                        if let Some(turn) = turns.last_mut() {
                            turn.finished = false;
                        }
                    }
                }
            }
            Some("assistant") => {
                if turns.is_empty() {
                    turns.push(blank_turn());
                }
                let Some(turn) = turns.last_mut() else {
                    continue;
                };
                let msg = &v["message"];
                let id = msg["id"].as_str().unwrap_or("").to_string();
                let calls = msg["content"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|b| b["type"] == "tool_use"));
                turn.finished = !calls;
                let texts: Vec<&str> = msg["content"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                if texts.is_empty() {
                    continue;
                }
                let t = texts.join("\n\n");
                match &mut turn.reply {
                    Some(r) if id == msg_id && !id.is_empty() => {
                        r.push_str("\n\n");
                        r.push_str(&t);
                    }
                    _ => turn.reply = Some(t),
                }
                if at.is_some() {
                    turn.reply_at = at;
                }
                msg_id = id;
            }
            // A message sent while the turn ran, taken into it.
            Some("attachment") | Some("queue-operation") => {
                let Some(text) = queued_text(&v) else {
                    continue;
                };
                if turns.is_empty() {
                    turns.push(blank_turn());
                }
                if let Some(turn) = turns.last_mut() {
                    let key = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !turn.queued.iter().any(|q| key(q) == key(&text)) {
                        turn.queued.push(text);
                    }
                    // The turn goes on to answer it.
                    turn.finished = false;
                }
            }
            _ => {}
        }
    }
    turns
}

/// The text of a message claude took into a running turn: a
/// `queued_command` attachment, or a queue entry removed as
/// `absorbed_mid_turn`. A queued entry claude later starts a turn of its
/// own for (dequeue) is a plain user message, not this.
fn queued_text(v: &Value) -> Option<String> {
    let text = match v.get("type").and_then(Value::as_str)? {
        "attachment" => {
            let a = v.get("attachment")?;
            if a.get("type").and_then(Value::as_str)? != "queued_command" {
                return None;
            }
            match a.get("prompt")? {
                Value::String(s) => s.clone(),
                Value::Array(blocks) => blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => return None,
            }
        }
        "queue-operation" => {
            if v.get("operation").and_then(Value::as_str)? != "remove"
                || v.get("reason").and_then(Value::as_str)? != "absorbed_mid_turn"
            {
                return None;
            }
            v.get("content")?.as_str()?.to_string()
        }
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

/// Where a conversation is right now, from the tail of its transcript:
/// the last text its assistant wrote, the tool it is in when its turn is
/// not over, and when it was last written.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TailStatus {
    /// The last text the assistant wrote (shortened by the caller).
    pub last_text: Option<String>,
    /// The last prompt (the user's, not a tool result).
    pub last_user: Option<String>,
    /// The tool the turn is in, when its last message is a tool call or
    /// a tool result ("Bash").
    pub tool: Option<String>,
    /// A short argument of that tool (the command, the file).
    pub tool_arg: Option<String>,
    /// The turn is over: its last message is text with no tool call.
    pub finished: bool,
    /// Timestamp (RFC 3339) of the last message.
    pub last_at: Option<String>,
}

/// The tail of a transcript (main chain), see `TailStatus`. Reads the
/// last 256 KB only.
pub fn tail_status(path: &Path) -> Option<TailStatus> {
    use std::io::{Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    const TAIL: u64 = 256 * 1024;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let skip_partial = len > TAIL;
    if skip_partial {
        f.seek(SeekFrom::Start(len - TAIL)).ok()?;
    }
    let mut st = TailStatus::default();
    let mut text_id = String::new();
    for (i, line) in lossy_lines(BufReader::new(f)).enumerate() {
        if i == 0 && skip_partial {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || v.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        {
            continue;
        }
        let at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                let content = &v["message"]["content"];
                let result = content
                    .as_array()
                    .is_some_and(|a| a.iter().any(|b| b["type"] == "tool_result"));
                let text = match content {
                    Value::String(s) => Some(s.clone()),
                    Value::Array(a) => {
                        let t: Vec<&str> = a
                            .iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect();
                        (!t.is_empty()).then(|| t.join("\n"))
                    }
                    _ => None,
                };
                if let Some(t) = text.filter(|t| !is_noise_prompt(t)) {
                    st.last_user = Some(t);
                    st.finished = false;
                    st.tool = None;
                    st.tool_arg = None;
                } else if result {
                    // The model is answering the result: the tool is over,
                    // the turn is not.
                    st.finished = false;
                }
                if at.is_some() {
                    st.last_at = at;
                }
            }
            Some("assistant") => {
                let msg = &v["message"];
                let id = msg["id"].as_str().unwrap_or("").to_string();
                let blocks = msg["content"].as_array();
                let call = blocks.and_then(|a| a.iter().rev().find(|b| b["type"] == "tool_use"));
                if let Some(c) = call {
                    st.tool = c["name"].as_str().map(str::to_string);
                    st.tool_arg = tool_brief(&c["input"]);
                    st.finished = false;
                } else {
                    st.finished = true;
                    st.tool = None;
                    st.tool_arg = None;
                }
                let texts: Vec<&str> = blocks
                    .map(|a| {
                        a.iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                if !texts.is_empty() {
                    let t = texts.join("\n\n");
                    match &mut st.last_text {
                        Some(r) if id == text_id && !id.is_empty() => {
                            r.push_str("\n\n");
                            r.push_str(&t);
                        }
                        _ => st.last_text = Some(t),
                    }
                    text_id = id;
                }
                if at.is_some() {
                    st.last_at = at;
                }
            }
            _ => {}
        }
    }
    Some(st)
}

/// The one argument of a tool call worth saying: the command, the file,
/// the pattern, the prompt's start.
pub fn tool_brief(input: &Value) -> Option<String> {
    for k in [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "description",
        "prompt",
        "url",
    ] {
        if let Some(s) = input.get(k).and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return Some(snippet(s, 80));
            }
        }
    }
    None
}

/// Text of the last prompt typed into a transcript (main chain, not tool
/// results or meta lines).
pub fn last_user_text(path: &Path) -> Option<String> {
    use std::io::{Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    const TAIL: u64 = 1024 * 1024;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > TAIL {
        f.seek(SeekFrom::Start(len - TAIL)).ok()?;
    }
    let mut last = None;
    for line in lossy_lines(BufReader::new(f)) {
        if !line.contains("\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("user")
            || v.get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || v.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        {
            continue;
        }
        let c = &v["message"]["content"];
        let text = match c {
            Value::String(s) => Some(s.clone()),
            Value::Array(a) => {
                let t: Vec<&str> = a
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect();
                (!t.is_empty()).then(|| t.join("\n"))
            }
            _ => None,
        };
        if let Some(t) = text.filter(|t| !t.trim().is_empty() && !t.starts_with('<')) {
            last = Some(t);
        }
    }
    last
}

/// The first sentences of `text`, up to about `max` characters.
pub fn first_sentences(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for sent in text.split_inclusive(['.', '!', '?']) {
        if !out.is_empty() && out.chars().count() + sent.chars().count() > max {
            break;
        }
        out.push_str(sent);
        if out.chars().count() >= max {
            break;
        }
    }
    if out.is_empty() {
        out = text.chars().take(max).collect();
    }
    out.trim().to_string()
}

/// A duration in seconds as a person says it: "45 s", "36 min",
/// "2 h 5 min", "3 d".
pub fn rel_secs(s: u64) -> String {
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else if s < 86400 {
        let m = (s % 3600) / 60;
        if m == 0 {
            format!("{} h", s / 3600)
        } else {
            format!("{} h {m} min", s / 3600)
        }
    } else {
        format!("{} d", s / 86400)
    }
}

pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_reads_never_make_the_headline() {
        // 100 turns, each reading a 100k token context from the cache.
        let mut t = String::from(
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"go\"}}\n",
        );
        for i in 0..100 {
            t.push_str(&format!("{{\"type\":\"assistant\",\"message\":{{\"id\":\"m{i}\",\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}],\"usage\":{{\"input_tokens\":10,\"cache_read_input_tokens\":100000,\"cache_creation_input_tokens\":200,\"output_tokens\":50}}}}}}\n"));
            // Streaming repeats the same message id: counted once.
            t.push_str(&format!("{{\"type\":\"assistant\",\"message\":{{\"id\":\"m{i}\",\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}],\"usage\":{{\"input_tokens\":10,\"cache_read_input_tokens\":100000,\"cache_creation_input_tokens\":200,\"output_tokens\":50}}}}}}\n"));
        }
        let info = parse_transcript(std::io::Cursor::new(t), "x");
        assert_eq!(
            info.tokens.total(),
            100 * (10 + 200 + 50),
            "new tokens only"
        );
        assert!(info.tokens.total() < 1_000_000, "never 10M");
        assert_eq!(info.tokens.cache_read, 100 * 100_000, "kept apart");
        assert_eq!(
            info.tokens.context, 100_210,
            "the largest context one turn had"
        );
        assert_eq!(info.tokens.output, 5000);
    }
    use std::io::Cursor;

    const FIXTURE: &str = include_str!("../tests/fixtures/session.jsonl");

    #[test]
    fn parses_fixture_transcript() {
        let s = parse_transcript(Cursor::new(FIXTURE), "11111111-2222-3333-4444-555555555555");
        assert_eq!(s.cwd, "/Users/test/proj-a");
        assert_eq!(
            s.first_prompt.as_deref(),
            Some("Refactor the parser so it handles empty lines")
        );
        assert_eq!(s.title.as_deref(), Some("Parser empty line handling"));
        assert_eq!(s.model.as_deref(), Some("claude-opus-5-5"));
        // 3 user prompts (the slash command counts, meta and tool results do
        // not) plus 2 unique main chain assistant messages.
        assert_eq!(s.messages, 5);
        // msg_A counted once (last block), plus msg_B, plus sidechain msg_S.
        assert_eq!(
            s.tokens,
            Tokens {
                input: 113,
                output: 157,
                cache_creation: 1020,
                cache_read: 1000,
                context: s.tokens.context,
            }
        );
        assert_eq!(s.tokens.total(), 1290, "cache reads are not in the total");
        assert!(s.tokens.context > 0);
    }

    #[test]
    fn empty_and_garbage_input() {
        let s = parse_transcript(Cursor::new("\n\nnot json\n{\"type\":\"user\"}\n"), "x");
        assert_eq!(s.messages, 0);
        assert!(s.first_prompt.is_none());
        assert_eq!(s.tokens.total(), 0);
    }

    #[test]
    fn scans_directory_tree() {
        let root = std::env::temp_dir().join(format!("godterm-test-{}", std::process::id()));
        let proj = root.join("projects").join("-Users-test-proj-a");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("11111111-2222-3333-4444-555555555555.jsonl"),
            FIXTURE,
        )
        .unwrap();
        fs::write(proj.join("empty.jsonl"), "").unwrap();
        fs::write(proj.join("notes.txt"), "x").unwrap();
        let mut cache = SessionCache::default();
        let found = cache.scan(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "11111111-2222-3333-4444-555555555555");
        assert!(found[0].modified.is_some());
        // Second scan hits the cache.
        assert_eq!(cache.scan(&root).len(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn last_reply_and_speech_text() {
        let root = std::env::temp_dir().join(format!("godterm-last-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = root.join("s.jsonl");
        fs::write(&p, FIXTURE).unwrap();
        assert_eq!(last_assistant_text(&p).as_deref(), Some("Done."));
        let extra = r#"{"type":"assistant","isSidechain":false,"message":{"id":"msg_C","content":[{"type":"text","text":"Here is the fix:"}]}}
{"type":"assistant","isSidechain":false,"message":{"id":"msg_C","content":[{"type":"tool_use","name":"Edit"}]}}
{"type":"assistant","isSidechain":false,"message":{"id":"msg_C","content":[{"type":"text","text":"```rust\nfn x() {}\n```\nAll **tests** pass."}]}}
{"type":"assistant","isSidechain":true,"message":{"id":"msg_D","content":[{"type":"text","text":"subagent noise"}]}}
"#;
        fs::write(&p, format!("{FIXTURE}{extra}")).unwrap();
        let last = last_assistant_text(&p).unwrap();
        assert!(last.starts_with("Here is the fix:"));
        assert!(last.contains("All **tests** pass."));
        assert_eq!(
            first_sentences("One two. Three four. Five six seven.", 20),
            "One two. Three four."
        );
        assert_eq!(first_sentences("short", 50), "short");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn survives_bad_bytes_and_huge_lines() {
        // Invalid UTF-8, a 3 MB garbage line and truncated JSON before a good
        // record: parsing keeps going and still finds it.
        let mut data: Vec<u8> = vec![0xff, 0xfe, b'{', b'\n'];
        data.extend(std::iter::repeat_n(b'x', 3_000_000));
        data.push(b'\n');
        data.extend_from_slice(
            b"{\"type\":\"user\",\"message\":{\"content\":\"hi \xc3\x28 there\"",
        );
        data.push(b'\n');
        data.extend_from_slice(FIXTURE.as_bytes());
        let s = parse_transcript(Cursor::new(&data), "x");
        assert_eq!(s.messages, 5);
        assert_eq!(s.title.as_deref(), Some("Parser empty line handling"));

        let root = std::env::temp_dir().join(format!("godterm-huge-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = root.join("big.jsonl");
        // Over the 4 MB tail window: the last reply is still found.
        let mut big = vec![b'y'; 5_000_000];
        big.push(b'\n');
        big.extend_from_slice(FIXTURE.as_bytes());
        fs::write(&p, &big).unwrap();
        assert_eq!(last_assistant_text(&p).as_deref(), Some("Done."));
        let _ = fs::remove_dir_all(&root);
    }

    /// Fuzz style: random bytes never panic the transcript parser.
    #[test]
    fn fuzz_transcript_parser() {
        let mut seed: u64 = 0x1234_5678_9abc_def1;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let pieces: [&[u8]; 12] = [
            b"{",
            b"}",
            b"\"type\":\"assistant\"",
            b"\"message\":",
            b"\"usage\":{\"input_tokens\":",
            b"\n",
            b"\"content\":[",
            b"]",
            b"\xff\xfe",
            b"\"text\"",
            b"99999999999999999999",
            b",",
        ];
        for _ in 0..3000 {
            let mut data = vec![];
            for _ in 0..(rnd() % 40) {
                if rnd() % 3 == 0 {
                    data.push((rnd() & 0xff) as u8);
                } else {
                    data.extend_from_slice(pieces[(rnd() % pieces.len() as u64) as usize]);
                }
            }
            let _ = parse_transcript(Cursor::new(&data), "f");
            let _ = first_sentences(&String::from_utf8_lossy(&data), (rnd() % 50) as usize);
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(decode_project_dir("-Users-alex-code"), "/Users/alex/code");
        assert_eq!(snippet("a\n  b   c", 10), "a b c");
        assert_eq!(snippet("abcdefghijkl", 8), "abcde...");
        assert_eq!(fmt_tokens(950), "950");
        assert_eq!(fmt_tokens(12_345), "12.3k");
        assert_eq!(fmt_tokens(2_500_000), "2.5M");
    }
}
