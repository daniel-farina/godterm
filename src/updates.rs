use std::collections::{BTreeSet, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

pub const READ_MAX: u64 = 2 * 1024 * 1024;

pub const ASK_TAIL: u64 = 48 * 1024;

pub const MAX_PROJECTS: usize = 5;

const MAX_WORDS: usize = 24;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Todo {
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Facts {
    pub texts: Vec<String>,

    pub edited: BTreeSet<String>,
    pub commands: usize,
    pub reads: usize,
    pub web: usize,

    pub doing: Option<&'static str>,
    pub todo: Option<Todo>,

    pub agents_started: Vec<String>,
    pub agents_done: usize,

    pub bg_started: usize,
    pub errors: usize,

    pub error_what: Option<&'static str>,

    pub turn_ended: bool,

    pub approval: bool,
    pub finished: bool,
    pub started: bool,
    pub exited: bool,

    pub jobs_done: Vec<String>,

    pub dropped: usize,
}

impl Facts {
    pub fn is_empty(&self) -> bool {
        *self == Facts::default()
    }

    #[cfg(test)]
    pub fn is_event(&self) -> bool {
        self.approval
            || self.errors > 0
            || self.finished
            || self.turn_ended
            || self.exited
            || !self.jobs_done.is_empty()
            || self.dropped > 0
    }

    pub fn merge(&mut self, o: Facts) {
        self.texts.extend(o.texts);
        let n = self.texts.len();
        if n > 3 {
            self.texts.drain(..n - 3);
        }
        self.edited.extend(o.edited);
        self.commands += o.commands;
        self.reads += o.reads;
        self.web += o.web;
        if o.doing.is_some() {
            self.doing = o.doing;
        }
        if o.todo.is_some() {
            self.todo = o.todo;
        }
        self.agents_started.extend(o.agents_started);
        self.agents_done += o.agents_done;
        self.bg_started += o.bg_started;
        self.errors += o.errors;
        if o.error_what.is_some() {
            self.error_what = o.error_what;
        }

        self.turn_ended |= o.turn_ended;
        self.approval |= o.approval;
        self.finished |= o.finished;
        self.started |= o.started;
        self.exited |= o.exited;
        self.jobs_done.extend(o.jobs_done);
        self.dropped += o.dropped;
    }
}

#[derive(Debug, Clone, Default)]
pub struct Cursor {
    pub offset: u64,

    pub open_agents: HashSet<String>,
}

fn command_kind(cmd: &str) -> Option<&'static str> {
    let c = cmd.to_lowercase();
    let has = |w: &str| c.contains(w);
    if has("test") || has("pytest") || has("jest") || has("vitest") || has("rspec") {
        Some("running the tests")
    } else if has("terraform apply") || has("kubectl apply") || has("deploy") {
        Some("deploying")
    } else if has("terraform plan") {
        Some("running the plan")
    } else if has("git push") {
        Some("pushing")
    } else if has("git commit") {
        Some("committing")
    } else if has("cargo build")
        || has("npm run build")
        || has("make")
        || has("go build")
        || has("xcodebuild")
        || has(" build")
    {
        Some("building")
    } else if has("migrat") {
        Some("running the migration")
    } else if has("install") {
        Some("installing dependencies")
    } else {
        None
    }
}

fn result_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

pub fn scan_lines<'a>(lines: impl Iterator<Item = &'a str>, cur: &mut Cursor) -> Facts {
    let mut f = Facts::default();

    let mut tools: std::collections::HashMap<String, &'static str> = Default::default();

    let mut last_text: Option<bool> = None;
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || v.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        {
            continue;
        }
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(content) = v.pointer("/message/content") else {
            continue;
        };
        match kind {
            "assistant" => {
                let Some(blocks) = content.as_array() else {
                    continue;
                };
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let t = b.get("text").and_then(Value::as_str).unwrap_or("").trim();
                            if !t.is_empty() {
                                f.texts.push(t.to_string());
                                last_text = Some(true);
                            }
                        }
                        Some("tool_use") => {
                            last_text = Some(false);
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                            let id = b
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let input = b.get("input").cloned().unwrap_or(Value::Null);
                            let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
                            let what: &'static str = match name {
                                "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                                    let p = s("file_path");
                                    let p = if p.is_empty() { s("notebook_path") } else { p };
                                    f.edited.insert(p.to_string());
                                    "an edit"
                                }
                                "Bash" | "PowerShell" => {
                                    f.commands += 1;
                                    if input
                                        .get("run_in_background")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false)
                                    {
                                        f.bg_started += 1;
                                    }
                                    if let Some(k) = command_kind(s("command")) {
                                        f.doing = Some(k);
                                    }
                                    "a command"
                                }
                                "Read" | "Grep" | "Glob" | "LS" => {
                                    f.reads += 1;
                                    "reading a file"
                                }
                                "WebFetch" | "WebSearch" => {
                                    f.web += 1;
                                    "a web lookup"
                                }
                                "Task" | "Agent" => {
                                    let d = s("description");
                                    f.agents_started.push(d.to_string());
                                    if !id.is_empty() {
                                        cur.open_agents.insert(id.clone());
                                    }
                                    "a subagent"
                                }
                                "TodoWrite" => {
                                    if let Some(list) = input.get("todos").and_then(Value::as_array)
                                    {
                                        fn st(t: &Value) -> &str {
                                            t.get("status").and_then(Value::as_str).unwrap_or("")
                                        }
                                        let done =
                                            list.iter().filter(|t| st(t) == "completed").count();
                                        let current = list
                                            .iter()
                                            .find(|t| st(t) == "in_progress")
                                            .and_then(|t| {
                                                t.get("activeForm")
                                                    .or_else(|| t.get("content"))
                                                    .and_then(Value::as_str)
                                            })
                                            .map(str::to_string);
                                        f.todo = Some(Todo {
                                            done,
                                            total: list.len(),
                                            current,
                                        });
                                    }
                                    "the to do list"
                                }
                                _ => "a tool",
                            };
                            if !id.is_empty() {
                                tools.insert(id, what);
                            }
                        }
                        _ => {}
                    }
                }
                let stop = v
                    .pointer("/message/stop_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if stop == "end_turn" && last_text == Some(true) {
                    f.turn_ended = true;
                }
            }
            "user" => {
                let Some(blocks) = content.as_array() else {
                    continue;
                };
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                    if cur.open_agents.remove(id) {
                        f.agents_done += 1;
                    }
                    if b.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                        let text =
                            result_text(b.get("content").unwrap_or(&Value::Null)).to_lowercase();

                        let declined = text.contains("doesn't want to proceed")
                            || text.contains("rejected")
                            || text.contains("interrupted by user");
                        if !declined {
                            f.errors += 1;
                            f.error_what = Some(tools.get(id).copied().unwrap_or("a tool"));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    f
}

pub fn read_new(path: &Path, cur: &mut Cursor) -> Facts {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Facts::default();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < cur.offset {
        cur.offset = 0;
    }
    if len == cur.offset {
        return Facts::default();
    }
    let mut start = cur.offset;
    let mut skip_first = false;
    if len - start > READ_MAX {
        start = len - READ_MAX;
        skip_first = true;
    }
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Facts::default();
    }
    let mut buf = Vec::new();
    if BufReader::new(file)
        .take(len - start)
        .read_to_end(&mut buf)
        .is_err()
    {
        return Facts::default();
    }

    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
        if skip_first {
            cur.offset = len;
        }
        return Facts::default();
    };
    let text = String::from_utf8_lossy(&buf[..=end]).into_owned();
    cur.offset = start + end as u64 + 1;
    let mut lines = text.lines();
    if skip_first {
        lines.next();
    }
    scan_lines(lines, cur)
}

pub fn start_offset(path: &Path, ask: bool) -> u64 {
    let Ok(m) = std::fs::metadata(path) else {
        return 0;
    };
    let len = m.len();
    if !ask {
        return len;
    }
    let recent = m
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|d| d < Duration::from_secs(1800));
    if !recent {
        return len;
    }
    tail_offset(path, ASK_TAIL)
}

pub fn switched_offset(path: &Path) -> u64 {
    let Ok(m) = std::fs::metadata(path) else {
        return 0;
    };
    let born = m.created().or_else(|_| m.modified()).ok();
    let young = born
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|d| d < Duration::from_secs(120));
    if young {
        0
    } else {
        m.len()
    }
}

pub fn tail_offset(path: &Path, max: u64) -> u64 {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(max);
    if from == 0 {
        return 0;
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return len;
    };
    if f.seek(SeekFrom::Start(from)).is_err() {
        return len;
    }
    let mut r = BufReader::new(f);
    let mut skip = Vec::new();
    match r.read_until(b'\n', &mut skip) {
        Ok(n) => from + n as u64,
        Err(_) => len,
    }
}

pub fn screen_facts(prev: crate::pane::Activity, now: crate::pane::Activity, bg: bool) -> Facts {
    use crate::pane::Activity as A;
    let mut f = Facts::default();
    if prev == now {
        return f;
    }
    match now {
        A::Permission => f.approval = true,
        A::Ready if matches!(prev, A::Working | A::Permission) && !bg => f.finished = true,
        A::Working if matches!(prev, A::Ready | A::Idle | A::Starting) => f.started = true,
        A::Exited => f.exited = true,
        _ => {}
    }
    f
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prio {
    Approval,
    Error,
    Finished,
    Progress,
}

pub fn number(n: usize) -> String {
    const W: [&str; 11] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    W.get(n)
        .map(|s| s.to_string())
        .unwrap_or_else(|| n.to_string())
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("a {one}")
    } else {
        format!("{} {many}", number(n))
    }
}

fn unsayable(w: &str) -> bool {
    let bare = w.trim_matches(|c: char| {
        matches!(
            c,
            ',' | '.' | ';' | ':' | '!' | '?' | '"' | '\'' | '(' | ')'
        )
    });
    if bare.is_empty() {
        return false;
    }
    let lower = bare.to_lowercase();
    if lower.starts_with("http") || lower.starts_with("www.") {
        return true;
    }
    if bare.contains('/') || bare.contains('\\') || bare.starts_with('~') {
        return true;
    }
    if bare.chars().any(|c| {
        matches!(
            c,
            '{' | '}' | '(' | ')' | '[' | ']' | '<' | '>' | '=' | ';' | '$' | '|' | '`' | '@' | '#'
        )
    }) || bare.contains("::")
        || bare.contains("->")
    {
        return true;
    }
    for p in ["sk-", "ghp_", "gho_", "xox", "akia", "eyj", "-----"] {
        if lower.starts_with(p) {
            return true;
        }
    }
    let digits = bare.chars().filter(char::is_ascii_digit).count();
    let hexish = bare.chars().all(|c| c.is_ascii_hexdigit());
    if hexish && bare.len() >= 7 && digits > 0 && bare.chars().any(|c| c.is_ascii_alphabetic()) {
        return true;
    }

    if bare.len() >= 16 && digits > 0 {
        return true;
    }
    if bare.chars().count() > 30 {
        return true;
    }

    if bare.contains('_') || (bare.matches('.').count() >= 2) {
        return true;
    }
    false
}

pub fn sayable(text: &str) -> Option<String> {
    let sp = crate::app_direct::speakable(text);
    for sentence in split_sentences(&sp) {
        if sentence.starts_with("It showed") {
            continue;
        }

        const LEAD: &[&str] = &[
            "in", "at", "to", "the", "a", "an", "of", "from", "on", "into", "see",
        ];
        let mut words: Vec<&str> = vec![];
        for w in sentence.split_whitespace() {
            if unsayable(w) {
                if words
                    .last()
                    .is_some_and(|l| LEAD.contains(&l.to_lowercase().as_str()))
                {
                    words.pop();
                }
                continue;
            }
            words.push(w);
        }
        if words.len() < 2 {
            continue;
        }
        let mut s = words.join(" ");
        if words.len() > MAX_WORDS {
            let head = s.split(',').next().unwrap_or("").trim().to_string();
            if head.split_whitespace().count() <= MAX_WORDS && head.split_whitespace().count() >= 3
            {
                s = format!("{head}.");
            } else {
                continue;
            }
        }
        let s = s.replace(" ,", ",").replace(" .", ".");
        let s = s.trim().trim_end_matches([',', ';', ':']).to_string();
        if s.is_empty() {
            continue;
        }
        return Some(if s.ends_with(['.', '!', '?']) {
            s
        } else {
            format!("{s}.")
        });
    }
    None
}

fn split_sentences(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        cur.push(*c);
        if matches!(c, '.' | '!' | '?') && chars.get(i + 1).is_none_or(|n| n.is_whitespace()) {
            out.push(cur.trim().to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

pub fn third_person(s: &str) -> String {
    let mut out: Vec<String> = vec![];
    let words: Vec<&str> = s.split(' ').collect();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let (core, tail) = split_punct(w);
        let next = words.get(i + 1).map(|n| split_punct(n).0.to_lowercase());
        let rep: Option<&str> = match core.replace('\u{2019}', "'").as_str() {
            "I'm" => Some("it's"),
            "I've" => Some("it has"),
            "I'll" => Some("it will"),
            "I'd" => Some("it would"),
            "I" if next.as_deref() == Some("am") => {
                i += 1;
                Some("it is")
            }
            "I" => Some("it"),
            "Let" | "let" if next.as_deref() == Some("me") => {
                i += 1;
                Some("it will")
            }
            "my" | "My" => Some("its"),
            "myself" => Some("itself"),
            _ => None,
        };
        match rep {
            Some(r) => out.push(format!("{r}{tail}")),
            None => out.push(w.to_string()),
        }
        i += 1;
    }
    out.join(" ")
}

fn split_punct(w: &str) -> (String, String) {
    let core = w.trim_end_matches([',', '.', ';', ':', '!', '?']);
    (core.to_string(), w[core.len()..].to_string())
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => format!("{}{}", f.to_uppercase(), c.as_str()),
        None => String::new(),
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) if c.clone().next().is_some_and(|n| n.is_lowercase()) => {
            format!("{}{}", f.to_lowercase(), c.as_str())
        }
        Some(f) => format!("{f}{}", c.as_str()),
        None => String::new(),
    }
}

fn join_and(parts: &[String]) -> String {
    match parts.len() {
        0 => String::new(),
        1 => parts[0].clone(),
        n => format!("{} and {}", parts[..n - 1].join(", "), parts[n - 1]),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Said {
    pub approval: bool,

    pub finished: bool,
}

pub fn say_project(name: &str, f: &Facts, said: Said) -> Option<(Prio, String)> {
    let latest = f
        .texts
        .iter()
        .rev()
        .find_map(|t| sayable(t))
        .map(|s| third_person(&s));
    let mut prio = Prio::Progress;
    let mut first: Option<String> = None;
    if f.approval && !said.approval {
        prio = Prio::Approval;
        first = Some(format!("{name} is waiting for your approval."));
    } else if f.errors > 0 {
        prio = Prio::Error;
        let what = f.error_what.unwrap_or("a tool");
        first = Some(if f.errors == 1 {
            format!("{name} hit an error in {what}.")
        } else {
            format!(
                "{name} hit {} errors, the last in {what}.",
                number(f.errors)
            )
        });
    } else if f.exited {
        prio = Prio::Error;
        first = Some(format!("{name} exited."));
    } else if f.finished || f.turn_ended {
        prio = Prio::Finished;
        let question = latest.as_deref().is_some_and(|l| l.ends_with('?'));
        first = match (&latest, said.finished, question) {
            (Some(l), _, true) => {
                Some(format!("{name} has a question for you: {}", lower_first(l)))
            }
            (Some(l), false, _) => Some(format!("{name} finished: {}", lower_first(l))),
            (Some(l), true, _) => Some(format!("{name}: {}", lower_first(l))),
            (None, false, _) => Some(format!("{name} finished.")),
            (None, true, _) => None,
        };
        if first.is_none() {
            prio = Prio::Progress;
        }
    }

    let mut clauses: Vec<String> = vec![];
    if let Some(t) = &f.todo {
        if t.total > 0 && t.done >= t.total {
            clauses.push(format!("all {} steps done", number(t.total)));
        } else if t.total > 0 {
            let mut c = format!("{} of {} steps done", number(t.done), number(t.total));
            if let Some(now) = t.current.as_deref().and_then(sayable) {
                let now = now.trim_end_matches(['.', '!', '?']);
                c.push_str(&format!(", now {}", lower_first(now)));
            }
            clauses.push(c);
        }
    }
    if let Some(d) = f.doing {
        if !clauses.iter().any(|c| c.contains(d)) {
            clauses.push(d.to_string());
        }
    }
    match f.agents_started.len() {
        0 => {}
        1 => {
            let d = f.agents_started[0].trim();
            let d = sayable(d).map(|s| s.trim_end_matches('.').to_string());
            clauses.push(match d {
                Some(d) if !d.is_empty() => format!("started a subagent to {}", lower_first(&d)),
                _ => "started a subagent".into(),
            });
        }
        n => clauses.push(format!("started {} subagents", number(n))),
    }

    if f.bg_started > 0 {
        clauses.push(if f.bg_started == 1 {
            "started a background command".to_string()
        } else {
            format!("started {} background commands", number(f.bg_started))
        });
    }
    if clauses.is_empty() && !f.edited.is_empty() {
        clauses.push(format!(
            "edited {}",
            plural(f.edited.len(), "file", "files")
        ));
    }
    if clauses.is_empty() && f.commands > 0 {
        clauses.push(format!("ran {}", plural(f.commands, "command", "commands")));
    }
    if clauses.is_empty() && f.web > 0 {
        clauses.push("looking things up on the web".into());
    }
    if clauses.is_empty() && f.reads > 0 && latest.is_none() {
        clauses.push("reading through the code".into());
    }
    for j in &f.jobs_done {
        let j = sayable(j).map(|s| s.trim_end_matches('.').to_string());
        clauses.push(match j {
            Some(j) => format!("the job {j} ran"),
            None => "a scheduled job ran".into(),
        });
    }
    if f.dropped > 0 {
        clauses.push(if f.dropped == 1 {
            "a request to it was dropped".to_string()
        } else {
            format!("{} requests to it were dropped", number(f.dropped))
        });
    }
    let clauses: Vec<String> = clauses.into_iter().take(3).collect();
    let mut out = match first {
        Some(s) => {
            if clauses.is_empty() {
                s
            } else {
                format!("{s} {}.", cap(&join_and(&clauses)))
            }
        }
        None => {
            if !clauses.is_empty() {
                let mut s = format!("{name}: {}.", join_and(&clauses));
                if let Some(l) = &latest {
                    s.push(' ');
                    s.push_str(&cap(l));
                }
                s
            } else if let Some(l) = &latest {
                format!("{name}: {}", lower_first(l))
            } else if f.started {
                format!("{name} started working.")
            } else if f.commands + f.reads + f.web > 0 || !f.edited.is_empty() {
                format!("{name} is still working.")
            } else {
                return None;
            }
        }
    };

    let ss = split_sentences(&out);
    if ss.len() > 2 {
        out = ss[..2].join(" ");
    }
    Some((prio, out))
}

pub fn batch(mut lines: Vec<(Prio, String)>) -> String {
    lines.sort_by_key(|l| l.0);
    let extra = lines.len().saturating_sub(MAX_PROJECTS);
    let mut out: Vec<String> = lines.into_iter().take(MAX_PROJECTS).map(|l| l.1).collect();
    if extra > 0 {
        out.push(if extra == 1 {
            "One more project moved too.".into()
        } else {
            format!("{} more projects moved too.", cap(&number(extra)))
        });
    }
    out.join(" ")
}

pub fn due(
    now: Instant,
    last_said: Option<Instant>,
    last_round: Instant,
    gap: Duration,
    every: Duration,
    event: bool,
    any: bool,
) -> bool {
    if !any {
        return false;
    }
    if last_said.is_some_and(|t| now.saturating_duration_since(t) < gap) {
        return false;
    }
    event || now.saturating_duration_since(last_round) >= every
}

pub fn next_in(now: Instant, last_round: Instant, every: Duration) -> Duration {
    every.saturating_sub(now.saturating_duration_since(last_round))
}

pub fn short_span(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m", s.div_ceil(60))
    } else {
        format!("{s}s")
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phrase {
    On,
    Off,

    Quiet,

    Now,
}

const ON: &[&str] = &[
    "update mode",
    "update mode on",
    "updates mode",
    "updates mode on",
    "turn on update mode",
    "start update mode",
    "enter update mode",
    "keep me posted",
    "keep me posted on everything",
    "keep me posted on the projects",
    "keep me posted on all projects",
    "keep me updated",
    "keep me in the loop",
    "give me updates",
    "give me progress updates",
    "give me regular updates",
    "start giving me updates",
    "updates on",
    "turn on updates",
    "turn updates on",
    "start updates",
];
const OFF: &[&str] = &[
    "stop updates",
    "stop the updates",
    "stop update mode",
    "update mode off",
    "updates mode off",
    "updates off",
    "turn off update mode",
    "turn off updates",
    "turn updates off",
    "end update mode",
    "exit update mode",
    "leave update mode",
    "no more updates",
    "stop giving me updates",
    "stop keeping me posted",
    "enough updates",
];
const QUIET: &[&str] = &["quiet", "be quiet", "quiet please", "go quiet"];
const NOW: &[&str] = &[
    "what's the latest",
    "whats the latest",
    "what is the latest",
    "what's the latest news",
    "any updates",
    "any update",
    "any news",
    "give me an update",
    "give me the latest",
    "status update",
    "what's new",
    "whats new",
    "what's happening",
    "whats happening",
    "update me",
    "catch me up",
];

pub fn parse_phrase(raw: &str) -> Option<Phrase> {
    let n = crate::voice::grammar::normalize(&raw.replace(['\u{2019}', '\u{2018}'], "'"));
    let s = crate::app_direct::core_words(&n).join(" ");
    if s.is_empty() {
        return None;
    }
    let is = |l: &[&str]| l.contains(&s.as_str());
    if is(ON) {
        Some(Phrase::On)
    } else if is(OFF) {
        Some(Phrase::Off)
    } else if is(QUIET) {
        Some(Phrase::Quiet)
    } else if is(NOW) {
        Some(Phrase::Now)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn asst(blocks: Value, stop: Option<&str>) -> String {
        json!({"type": "assistant", "message": {"id": "m", "content": blocks, "stop_reason": stop}})
            .to_string()
    }
    fn result(id: &str, err: bool, text: &str) -> String {
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "is_error": err, "content": text}]}})
            .to_string()
    }

    #[test]
    fn transcript_lines_become_facts() {
        let lines = [
            json!({"type": "user", "message": {"content": "fix the migration"}}).to_string(),
            asst(json!([{"type": "text", "text": "I'll start with the schema."}]), None),
            asst(json!([{"type": "tool_use", "id": "t1", "name": "TodoWrite", "input": {"todos": [
                {"content": "Read schema", "status": "completed", "activeForm": "Reading schema"},
                {"content": "Write migration", "status": "completed", "activeForm": "Writing migration"},
                {"content": "Apply migration", "status": "in_progress", "activeForm": "Applying the migration"},
                {"content": "a", "status": "pending"}, {"content": "b", "status": "pending"},
                {"content": "c", "status": "pending"}, {"content": "d", "status": "pending"}]}}]), None),
            asst(json!([{"type": "tool_use", "id": "t2", "name": "Edit", "input": {"file_path": "/x/a.rs"}}]), None),
            asst(json!([{"type": "tool_use", "id": "t3", "name": "Edit", "input": {"file_path": "/x/b.rs"}}]), None),
            asst(json!([{"type": "tool_use", "id": "t4", "name": "Bash", "input": {"command": "cargo test -q"}}]), None),
            result("t4", true, "test failed"),
            asst(json!([{"type": "tool_use", "id": "t5", "name": "Bash", "input": {"command": "rm x"}}]), None),
            result("t5", true, "The user doesn't want to proceed with this tool use."),
            asst(json!([{"type": "tool_use", "id": "t6", "name": "Task", "input": {"description": "Review the API"}}]), None),

            json!({"type": "assistant", "isSidechain": true, "message": {"content": [{"type": "text", "text": "sub"}]}}).to_string(),
            "not json".into(),
        ];
        let mut cur = Cursor::default();
        let f = scan_lines(lines.iter().map(String::as_str), &mut cur);
        assert_eq!(f.texts, vec!["I'll start with the schema.".to_string()]);
        assert_eq!(f.edited.len(), 2);
        assert_eq!(f.commands, 2);
        assert_eq!(f.doing, Some("running the tests"));
        assert_eq!(
            f.todo,
            Some(Todo {
                done: 2,
                total: 7,
                current: Some("Applying the migration".into())
            })
        );
        assert_eq!(f.errors, 1, "a denied call is not an error");
        assert_eq!(f.error_what, Some("a command"));
        assert_eq!(f.agents_started, vec!["Review the API".to_string()]);
        assert!(!f.turn_ended);

        let later = [
            result("t6", false, "done"),
            asst(
                json!([{"type": "text", "text": "All done. The migration is applied."}]),
                Some("end_turn"),
            ),
        ];
        let g = scan_lines(later.iter().map(String::as_str), &mut cur);
        assert_eq!(g.agents_done, 1);
        assert!(g.turn_ended);
        assert!(g.is_event());
    }

    #[test]
    fn reading_moves_on_and_waits_for_whole_lines() {
        let d = std::env::temp_dir().join(format!("godterm-updates-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        let old = asst(
            json!([{"type": "text", "text": "Old news here."}]),
            Some("end_turn"),
        );
        std::fs::write(&p, format!("{old}\n")).unwrap();

        let mut cur = Cursor {
            offset: start_offset(&p, false),
            ..Default::default()
        };
        assert!(read_new(&p, &mut cur).is_empty());

        let mut ask = Cursor {
            offset: start_offset(&p, true),
            ..Default::default()
        };
        assert_eq!(
            read_new(&p, &mut ask).texts,
            vec!["Old news here.".to_string()]
        );

        let new = asst(json!([{"type": "text", "text": "Now the queue."}]), None);
        let (a, b) = new.split_at(10);
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "{a}").unwrap();
        f.flush().unwrap();
        assert!(read_new(&p, &mut cur).is_empty());
        writeln!(f, "{b}").unwrap();
        f.flush().unwrap();
        assert_eq!(
            read_new(&p, &mut cur).texts,
            vec!["Now the queue.".to_string()]
        );
        assert!(read_new(&p, &mut cur).is_empty());

        std::fs::write(&p, format!("{old}\n")).unwrap();
        assert_eq!(read_new(&p, &mut cur).texts.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sentences_are_plain_and_safe() {
        assert_eq!(
            sayable("I fixed `parse_row()` in /Users/x/src/a.rs at 3fa9c1d2e. Then more.")
                .as_deref(),
            Some("I fixed.")
        );
        assert_eq!(
            sayable("**Done.** The deploy finished the plan and is applying now.").as_deref(),
            Some("The deploy finished the plan and is applying now.")
        );
        assert_eq!(
            sayable("```\nlet x = 1;\n```\nAll green now.").as_deref(),
            Some("All green now.")
        );
        assert!(sayable("token sk-abc123def456ghi789").is_none_or(|s| !s.contains("sk-")));
        assert_eq!(
            third_person("I'm running the tests and I fixed my bug. Let me check."),
            "it's running the tests and it fixed its bug. it will check."
        );
    }

    #[test]
    fn a_project_in_a_sentence_or_two() {
        let mut f = Facts {
            todo: Some(Todo {
                done: 3,
                total: 7,
                current: Some("Applying the migration".into()),
            }),
            ..Default::default()
        };
        let (p, s) = say_project("Hyper", &f, Said::default()).unwrap();
        assert_eq!(p, Prio::Progress);
        assert_eq!(
            s,
            "Hyper: three of seven steps done, now applying the migration."
        );
        f.texts.push("I updated the schema.".into());
        let (_, s) = say_project("Hyper", &f, Said::default()).unwrap();
        assert_eq!(
            s,
            "Hyper: three of seven steps done, now applying the migration. It updated the schema."
        );
        let fin = Facts {
            finished: true,
            texts: vec!["The deploy finished the plan and is applying now.".into()],
            ..Default::default()
        };
        assert_eq!(
            say_project("ACME", &fin, Said::default()).unwrap(),
            (
                Prio::Finished,
                "ACME finished: the deploy finished the plan and is applying now.".into()
            )
        );

        assert_eq!(
            say_project(
                "ACME",
                &fin,
                Said {
                    finished: true,
                    approval: false
                }
            )
            .unwrap()
            .1,
            "ACME: the deploy finished the plan and is applying now."
        );
        let bare = Facts {
            finished: true,
            ..Default::default()
        };
        assert!(say_project(
            "ACME",
            &bare,
            Said {
                finished: true,
                approval: false
            }
        )
        .is_none());
        let ask = Facts {
            approval: true,
            ..Default::default()
        };
        assert_eq!(
            say_project("Bot", &ask, Said::default()).unwrap().0,
            Prio::Approval
        );
        assert!(say_project(
            "Bot",
            &ask,
            Said {
                approval: true,
                finished: false
            }
        )
        .is_none());
        let err = Facts {
            errors: 2,
            error_what: Some("a command"),
            ..Default::default()
        };
        assert_eq!(
            say_project("Bot", &err, Said::default()).unwrap(),
            (
                Prio::Error,
                "Bot hit two errors, the last in a command.".into()
            )
        );
        assert!(say_project("Idle", &Facts::default(), Said::default()).is_none());
    }

    #[test]
    fn several_projects_most_important_first() {
        let lines = vec![
            (Prio::Progress, "A: going.".to_string()),
            (Prio::Finished, "B finished.".to_string()),
            (
                Prio::Approval,
                "C is waiting for your approval.".to_string(),
            ),
            (Prio::Error, "D hit an error in a command.".to_string()),
        ];
        assert_eq!(
            batch(lines),
            "C is waiting for your approval. D hit an error in a command. B finished. A: going."
        );
        let many: Vec<(Prio, String)> = (0..7)
            .map(|i| (Prio::Progress, format!("P{i}: going.")))
            .collect();
        let b = batch(many);
        assert!(
            b.ends_with("P4: going. Two more projects moved too."),
            "{b}"
        );
    }

    #[test]
    fn cadence_waits_for_the_gap_and_the_roundup() {
        let t0 = Instant::now();
        let gap = Duration::from_secs(60);
        let every = Duration::from_secs(240);
        let at = |s: u64| t0 + Duration::from_secs(s);

        assert!(!due(at(1000), None, t0, gap, every, true, false));

        assert!(due(at(1), None, t0, gap, every, true, true));

        assert!(!due(at(30), Some(at(1)), at(1), gap, every, true, true));
        assert!(due(at(61), Some(at(1)), at(1), gap, every, true, true));

        assert!(!due(at(200), Some(at(1)), at(1), gap, every, false, true));
        assert!(due(at(241), Some(at(1)), at(1), gap, every, false, true));
        assert_eq!(next_in(at(61), at(1), every), Duration::from_secs(180));
        assert_eq!(short_span(Duration::from_secs(180)), "3m");
        assert_eq!(short_span(Duration::from_secs(40)), "40s");
    }

    #[test]
    fn phrases_turn_it_on_and_off() {
        for s in [
            "Update mode.",
            "Keep me posted.",
            "okay, give me updates please",
            "Turn on update mode.",
        ] {
            assert_eq!(parse_phrase(s), Some(Phrase::On), "{s}");
        }
        for s in ["Stop updates.", "Update mode off.", "No more updates."] {
            assert_eq!(parse_phrase(s), Some(Phrase::Off), "{s}");
        }
        assert_eq!(parse_phrase("Quiet."), Some(Phrase::Quiet));
        for s in [
            "What's the latest?",
            "Any updates?",
            "what\u{2019}s the latest",
        ] {
            assert_eq!(parse_phrase(s), Some(Phrase::Now), "{s}");
        }
        for s in [
            "update the readme",
            "keep me posted about the deploy in hyper",
            "run the tests",
        ] {
            assert_eq!(parse_phrase(s), None, "{s}");
        }
    }

    #[test]
    fn screen_state_for_the_other_clis() {
        use crate::pane::Activity as A;
        assert!(screen_facts(A::Ready, A::Permission, false).approval);
        assert!(screen_facts(A::Working, A::Ready, false).finished);
        assert!(
            !screen_facts(A::Working, A::Ready, true).finished,
            "background work still runs"
        );
        assert!(screen_facts(A::Ready, A::Working, false).started);
        assert!(screen_facts(A::Ready, A::Ready, false).is_empty());
        let f = screen_facts(A::Working, A::Ready, false);
        assert_eq!(
            say_project("Grok tab", &f, Said::default()).unwrap().1,
            "Grok tab finished."
        );
        let s = screen_facts(A::Ready, A::Working, false);
        assert_eq!(
            say_project("Codex tab", &s, Said::default()).unwrap(),
            (Prio::Progress, "Codex tab started working.".into())
        );
    }
}
