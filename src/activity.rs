//! What "idle" means, in one place. A session is idle only when its agent
//! is not busy, no permission prompt waits, no subagent is running, no
//! background shell or task runs under it (its own helpers such as MCP
//! servers aside) and its transcript has not been written for a while.
//! Everything else gets a specific state: working, waiting_approval,
//! background (2 subagents, 1 shell), or idle_with_loops.

use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// A subagent transcript written this recently is a running subagent,
/// whatever its last line says (a message may still be streaming in).
pub const SUBAGENT_FRESH: Duration = Duration::from_secs(30);
/// A subagent whose last real message is a tool call or a tool result
/// counts as running by its transcript alone while it was written this
/// recently. Older than this it counts only when the screen shows it
/// ("Waiting for N background agents"): a stopped agent (its claude died,
/// hit a limit or was interrupted) leaves exactly this tail behind, and
/// must not keep a tab "busy" for hours.
pub const SUBAGENT_RECENT: Duration = Duration::from_secs(5 * 60);
/// An unfinished transcript older than this is never taken for a running
/// agent, even when the screen shows agents (it is a different, dead one).
pub const SUBAGENT_STALE: Duration = Duration::from_secs(2 * 3600);
/// A transcript written this recently means the agent is working.
pub const WRITE_FRESH: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub cmd: String,
}

/// Every process (pid, parent, command line): one `ps`, or on Windows the
/// system's list.
pub fn process_table() -> Vec<Proc> {
    crate::procs::processes()
        .into_iter()
        .map(|p| Proc {
            pid: p.pid,
            ppid: p.ppid,
            cmd: p.command_line(),
        })
        .collect()
}

#[cfg(test)]
pub fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some(Proc {
                pid,
                ppid,
                cmd: it.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

/// Every process below `pid`.
pub fn descendants(table: &[Proc], pid: u32) -> Vec<&Proc> {
    let mut out: Vec<&Proc> = vec![];
    let mut frontier = vec![pid];
    while let Some(p) = frontier.pop() {
        for c in table.iter().filter(|c| c.ppid == p) {
            if out.iter().all(|o| o.pid != c.pid) {
                out.push(c);
                frontier.push(c.pid);
            }
        }
    }
    out
}

/// The agent's own long lived helpers, not work: MCP servers, the
/// language servers and watchers it keeps, caffeinate.
pub fn is_helper(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    c.contains("mcp")
        || c.contains("language-server")
        || c.contains("languageserver")
        || c.contains("caffeinate")
        // GodTerm's own helpers, by program (not any path with it in).
        || crate::procs::program_name(cmd.split_whitespace().next().unwrap_or(""))
            .to_lowercase()
            .starts_with("godterm")
        || c.contains("--stdio")
        || c.contains("tsserver")
        || c.contains("rust-analyzer")
        || c.contains("watchman")
        || c.ends_with(" serve-mcp")
        || c.contains("claude-code/vendor")
        // Windows gives every console program a console host.
        || c.contains("conhost")
        || c.contains("openconsole")
}

/// A child that is a shell (or a job a shell started).
fn kind_of(cmd: &str) -> &'static str {
    let first = cmd
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    match crate::procs::program_name(first).as_str() {
        "sh" | "bash" | "zsh" | "fish" | "dash" | "-zsh" | "-bash" | "cmd" | "powershell"
        | "pwsh" => "shell",
        _ => "task",
    }
}

/// One subagent of a session (an Agent tool call of Claude Code), from
/// its transcript under `<session>/subagents/agent-<id>.jsonl`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Subagent {
    /// The file stem ("agent-a1b2c3").
    pub id: String,
    /// Its type or name when the transcript says (general-purpose,
    /// Explore...), else None.
    pub name: Option<String>,
    /// What it was asked, shortened (its first prompt).
    pub task: Option<String>,
    /// Seconds since it started (its first line, else the file's birth).
    pub age_s: u64,
    /// Seconds since its transcript was last written.
    pub idle_s: u64,
    /// Still going by its transcript: written in the last SUBAGENT_FRESH,
    /// or its last real message is a tool call or a tool result and it
    /// was written in the last SUBAGENT_RECENT. Attachment, api-request
    /// and system records are not messages and are skipped.
    pub running: bool,
    /// Its last real message is a tool call or a tool result (what a
    /// running agent looks like, and a stopped one too), whenever it was
    /// written.
    pub unfinished: bool,
    /// The tool it is in, when it waits on one ("Bash", "Read").
    pub tool: Option<String>,
}

impl Subagent {
    /// "general-purpose (36 min)" for a list.
    pub fn label(&self) -> String {
        let name = self.name.as_deref().unwrap_or("agent");
        format!("{name} ({})", crate::sessions::rel_secs(self.age_s))
    }
}

/// The subagents of a session, from their transcripts under
/// `<session>/subagents/` (every file, running or done, sorted by start).
/// Reads only each file's head and tail, so it is cheap for every tab
/// every turn.
pub fn subagents(transcript: &Path, now: SystemTime) -> Vec<Subagent> {
    let dir = transcript.with_extension("").join("subagents");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out: Vec<Subagent> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| subagent_of(&e.path(), now))
        .collect();
    out.sort_by(|a, b| b.age_s.cmp(&a.age_s).then_with(|| a.id.cmp(&b.id)));
    out
}

/// The running ones only.
pub fn running_subagents_of(transcript: &Path, now: SystemTime) -> Vec<Subagent> {
    subagents(transcript, now)
        .into_iter()
        .filter(|s| s.running)
        .collect()
}

/// How many subagents of a session still run.
pub fn running_subagents(transcript: &Path, now: SystemTime) -> usize {
    running_subagents_of(transcript, now).len()
}

/// The head and the tail of a file, as text (the whole file when small).
fn head_and_tail(path: &Path, n: u64) -> Option<(String, String)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut head = vec![0u8; len.min(n) as usize];
    f.read_exact(&mut head).ok()?;
    let head = String::from_utf8_lossy(&head).into_owned();
    if len <= n {
        return Some((head.clone(), head));
    }
    f.seek(SeekFrom::Start(len - n)).ok()?;
    let mut tail = vec![0u8; n as usize];
    f.read_exact(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail).into_owned();
    // The first line of the tail is cut; drop it.
    let tail = tail
        .split_once('\n')
        .map(|x| x.1.to_string())
        .unwrap_or_default();
    Some((head, tail))
}

/// A string field named one of `keys`, anywhere in the top level of `v`
/// or in its message.
fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}

fn user_text(v: &Value) -> Option<String> {
    match &v["message"]["content"] {
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
    }
    .filter(|t| !t.trim().is_empty())
}

/// One subagent transcript: what it is, how old, and whether it still
/// runs (see `Subagent`).
pub fn subagent_of(path: &Path, now: SystemTime) -> Option<Subagent> {
    const PART: u64 = 48 * 1024;
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let idle = now.duration_since(modified).unwrap_or_default();
    let (head, tail) = head_and_tail(path, PART)?;
    let mut s = Subagent {
        id: path
            .file_stem()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default(),
        idle_s: idle.as_secs(),
        ..Default::default()
    };
    // The head: its name and task, and when it started.
    let mut started: Option<SystemTime> = None;
    for line in head.lines().take(12) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if s.name.is_none() {
            s.name = str_field(
                &v,
                &[
                    "agentType",
                    "subagentType",
                    "subagent_type",
                    "agentName",
                    "agent_type",
                ],
            );
        }
        if started.is_none() {
            started = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(SystemTime::from);
        }
        if s.task.is_none() && v.get("type").and_then(Value::as_str) == Some("user") {
            if let Some(t) = user_text(&v) {
                s.task = Some(crate::sessions::snippet(&t, 120));
            }
        }
    }
    let started = started.or_else(|| meta.created().ok()).unwrap_or(modified);
    s.age_s = now.duration_since(started).unwrap_or_default().as_secs();
    // The tail: the last message decides whether it is still going.
    let mut last: Option<(&'static str, bool, Option<String>)> = None;
    for line in tail.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let blocks = v["message"]["content"].as_array();
                let tool = blocks.and_then(|a| {
                    a.iter()
                        .rev()
                        .find(|b| b["type"] == "tool_use")
                        .and_then(|b| b["name"].as_str())
                        .map(str::to_string)
                });
                last = Some(("assistant", tool.is_some(), tool));
            }
            Some("user") => {
                // A tool result or a prompt leaves the model something to
                // answer; "[Request interrupted by user]" ends it.
                let stopped = user_text(&v)
                    .is_some_and(|t| t.trim_start().starts_with("[Request interrupted"));
                last = Some(("user", !stopped, None));
            }
            // attachment, api-request, system, queue-operation...: not
            // messages, they say nothing about whether it still works.
            _ => {}
        }
    }
    let unfinished = match last {
        // Mid tool call, or the model is answering a tool result.
        Some(("assistant", calls, _)) => calls,
        Some(("user", live, _)) => live,
        // Nothing parsable: only a fresh write says it runs.
        None => false,
        Some(_) => false,
    };
    s.tool = last.and_then(|l| l.2);
    s.unfinished = unfinished && idle < SUBAGENT_STALE;
    s.running = idle < SUBAGENT_FRESH || (unfinished && idle < SUBAGENT_RECENT);
    Some(s)
}

/// What a Claude Code screen says about background agents: the "Waiting
/// for N background agents to finish" line and the agent rows under it
/// ("○ general-purpose 36m 26s": a hollow circle is one still running).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScreenAgents {
    /// N of "Waiting for N background agents".
    pub waiting: usize,
    /// (name, age in seconds) of each running row.
    pub rows: Vec<(String, u64)>,
}

impl ScreenAgents {
    pub fn count(&self) -> usize {
        self.waiting.max(self.rows.len())
    }
}

/// "36m 26s", "1h 2m", "45s" in seconds.
pub fn parse_age(words: &[&str]) -> Option<u64> {
    let mut total = 0u64;
    let mut any = false;
    for w in words {
        let (num, unit) = w.split_at(w.find(|c: char| !c.is_ascii_digit())?);
        let n: u64 = num.parse().ok()?;
        total += match unit {
            "s" => n,
            "m" => n * 60,
            "h" => n * 3600,
            "d" => n * 86400,
            _ => return None,
        };
        any = true;
    }
    any.then_some(total)
}

/// The agents a screen shows (its last lines, where the running turn or
/// the turn just over draws them).
pub fn screen_agents(screen: &str) -> ScreenAgents {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(24)..];
    let mut out = ScreenAgents::default();
    for l in tail {
        let low = l.to_lowercase();
        if low.contains("waiting for") && low.contains("background agent") {
            let words: Vec<&str> = low.split_whitespace().collect();
            if let Some(i) = words.iter().position(|w| *w == "for") {
                if let Some(n) = words.get(i + 1).and_then(|w| w.parse::<usize>().ok()) {
                    out.waiting = out.waiting.max(n);
                }
            }
            continue;
        }
        // "  ⎿  ○ general-purpose 36m 26s" (a done row is ✔ or ●).
        let Some(rest) = l.split_once('○').map(|x| x.1) else {
            continue;
        };
        let words: Vec<&str> = rest.split_whitespace().collect();
        if words.len() < 2 {
            continue;
        }
        // The age is the trailing run of "36m 26s" words.
        let mut k = words.len();
        while k > 1 && parse_age(&words[k - 1..k]).is_some() {
            k -= 1;
        }
        if k == words.len() {
            continue;
        }
        let Some(age) = parse_age(&words[k..]) else {
            continue;
        };
        let name = words[..k].join(" ");
        if name
            .chars()
            .all(|c| c.is_alphanumeric() || "-_ .".contains(c))
        {
            out.rows.push((name, age));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Activity {
    /// working, waiting_approval, background (...), idle_with_loops, idle.
    pub state: String,
    pub subagents_running: usize,
    /// The running subagents, with names and ages when known.
    pub subagents: Vec<Subagent>,
    pub background_shells: usize,
    pub background_tasks: usize,
    pub children: Vec<(u32, String)>,
    pub last_write_s: Option<u64>,
    pub loops: usize,
}

impl Activity {
    /// Idle in the sense take-over and the user mean: nothing at all runs.
    pub fn idle(&self) -> bool {
        self.state == "idle" || self.state == "idle_with_loops"
    }

    pub fn background(&self) -> usize {
        self.subagents_running + self.background_shells + self.background_tasks
    }

    /// "3 subagents (general-purpose 36 min, Explore 2 min)".
    pub fn subagents_line(&self) -> Option<String> {
        if self.subagents_running == 0 {
            return None;
        }
        let n = crate::control::plural(self.subagents_running, "subagent");
        let names: Vec<String> = self.subagents.iter().take(6).map(Subagent::label).collect();
        Some(if names.is_empty() {
            n
        } else {
            format!("{n} ({})", names.join(", "))
        })
    }

    pub fn json(&self) -> Value {
        json!({
            "state": self.state, "subagents_running": self.subagents_running,
            "subagents": self.subagents.iter().map(|a| json!({
                "name": a.name, "task": a.task, "age_s": a.age_s, "idle_s": a.idle_s, "tool": a.tool,
            })).collect::<Vec<_>>(),
            "background_shells": self.background_shells,
            "background_tasks": self.background_tasks, "children": self.children.iter().map(|(p, c)| json!({"pid": p, "cmd": crate::sessions::snippet(c, 80)})).collect::<Vec<_>>(),
            "last_write_s": self.last_write_s, "loops": self.loops,
        })
    }
}

/// The state from its parts (the rule, kept separate so it can be tested).
pub fn classify(
    busy: bool,
    waiting_approval: bool,
    last_write_s: Option<u64>,
    subagents_running: usize,
    children: Vec<(u32, String)>,
    loops: usize,
) -> Activity {
    let work: Vec<(u32, String)> = children
        .into_iter()
        .filter(|(_, c)| !is_helper(c))
        .collect();
    let shells = work.iter().filter(|(_, c)| kind_of(c) == "shell").count();
    let tasks = work.len() - shells;
    let fresh = last_write_s.is_some_and(|s| s < WRITE_FRESH.as_secs());
    let mut a = Activity {
        subagents_running,
        subagents: vec![],
        background_shells: shells,
        background_tasks: tasks,
        children: work,
        last_write_s,
        loops,
        state: String::new(),
    };
    a.state = if waiting_approval {
        "waiting_approval".into()
    } else if busy || fresh {
        "working".into()
    } else if a.background() > 0 {
        let mut parts = vec![];
        let n = |k: usize, one: &str| format!("{k} {one}{}", if k == 1 { "" } else { "s" });
        if subagents_running > 0 {
            parts.push(n(subagents_running, "subagent"));
        }
        if shells > 0 {
            parts.push(n(shells, "shell"));
        }
        if tasks > 0 {
            parts.push(n(tasks, "task"));
        }
        format!("background ({})", parts.join(", "))
    } else if loops > 0 {
        "idle_with_loops".into()
    } else {
        "idle".into()
    };
    a
}

/// A session running in another terminal.
pub fn of_live(l: &crate::takeover::Live, table: &[Proc], loops: usize) -> Activity {
    let now = SystemTime::now();
    let transcript = crate::takeover::transcript_of(l);
    let last_write_s = transcript
        .as_deref()
        .and_then(|t| std::fs::metadata(t).and_then(|m| m.modified()).ok())
        .map(|m| now.duration_since(m).unwrap_or_default().as_secs());
    let subs = transcript
        .as_deref()
        .map(|t| running_subagents_of(t, now))
        .unwrap_or_default();
    let children = descendants(table, l.pid)
        .into_iter()
        .map(|p| (p.pid, p.cmd.clone()))
        .collect();
    let mut a = classify(
        l.status == "busy",
        false,
        last_write_s,
        subs.len(),
        children,
        loops,
    );
    a.subagents = subs;
    a
}

/// The process table, at most two seconds old (snapshots ask every turn).
pub fn process_table_cached() -> Vec<Proc> {
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, Vec<Proc>)>> =
        std::sync::Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, t)) = c.as_ref() {
        if at.elapsed() < Duration::from_secs(2) {
            return t.clone();
        }
    }
    let t = if cfg!(test) { vec![] } else { process_table() };
    *c = Some((std::time::Instant::now(), t.clone()));
    t
}

impl crate::app::App {
    /// A GodTerm tab's activity: its screen state, its subagents and what
    /// runs under its agent process.
    pub fn tab_activity(&self, s: usize, t: usize) -> Activity {
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return Activity::default();
        };
        let now = SystemTime::now();
        let transcript = self.transcript_path(s, t);
        let last_write_s = transcript
            .as_deref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .map(|m| now.duration_since(m).unwrap_or_default().as_secs());
        // A tab stopped at its usage limit has no agents working: they
        // wait for a model that will not answer until the reset.
        let limited = self.tab_at_limit(s, t);
        let mut subs = transcript
            .as_deref()
            .filter(|_| !limited)
            .map(|p| running_subagents_of(p, now))
            .unwrap_or_default();
        // The screen is the other witness: claude's "Waiting for N
        // background agents" and its agent rows. An Agent tool subagent
        // in a long tool call writes nothing for minutes, and a tab
        // started elsewhere may have no transcript here; the rows still
        // show it. The larger count wins, the rows fill in names and
        // ages the transcripts did not give.
        if !limited && tab.is_running() && matches!(tab.activity, crate::pane::Activity::Ready) {
            let seen = screen_agents(&tab.screen_text());
            if seen.count() > subs.len() {
                let from_rows: Vec<Subagent> = seen
                    .rows
                    .iter()
                    .map(|(name, age)| Subagent {
                        id: format!("screen:{name}"),
                        name: Some(name.clone()),
                        age_s: *age,
                        running: true,
                        ..Default::default()
                    })
                    .collect();
                if from_rows.len() >= subs.len() {
                    subs = from_rows;
                }
                while subs.len() < seen.count() {
                    subs.push(Subagent {
                        id: format!("screen:{}", subs.len()),
                        running: true,
                        ..Default::default()
                    });
                }
            }
        }
        let children = match tab.pid() {
            Some(pid) => descendants(&process_table_cached(), pid)
                .into_iter()
                .map(|p| (p.pid, p.cmd.clone()))
                .collect(),
            None => vec![],
        };
        let busy = tab.activity == crate::pane::Activity::Working;
        let asking = tab.activity == crate::pane::Activity::Permission;
        let mut a = classify(
            busy,
            asking,
            last_write_s,
            subs.len(),
            children,
            self.loops_in(tab.uid),
        );
        a.subagents = subs;
        a
    }

    /// What still runs under a tab whose screen is at its prompt:
    /// "2 subagents, 1 shell" from its process tree and subagent
    /// transcripts (cached 2 s), or "background work" when only claude's
    /// footer says so. None when nothing runs, or when the tab is working
    /// or asking (busy anyway).
    ///
    /// Busy is not the same as "ready for a prompt". A tab with background
    /// work is busy: it must never be called idle, swept by archive_tabs,
    /// paused by the memory saver or moved without waiting (stopping its
    /// claude stops the shells and subagents too). It still takes a
    /// prompt like a ready tab (claude handles it alongside the background
    /// work), so prompt delivery does not wait for it.
    pub fn tab_background_work(&self, s: usize, t: usize) -> Option<String> {
        let tab = self.panes.get(s)?.tabs.get(t)?;
        // Stopped at its usage limit: nothing it started goes on, and a
        // move or archive must not wait for it.
        if self.tab_at_limit(s, t) && tab.activity != crate::pane::Activity::Working {
            return None;
        }
        if let Some(w) = crate::ui_chrome::background_work(self, s, t) {
            return Some(w);
        }
        (tab.bg_wait && tab.is_running() && tab.activity == crate::pane::Activity::Ready)
            .then(|| "background work".to_string())
    }

    /// The tab is stopped at its usage limit: its screen shows the limit
    /// error, GodTerm's scan marked it, or (at its prompt) its account's
    /// binding limit is used up. Its subagents and shells are not work in
    /// progress: they wait for a model that will not answer until the
    /// reset.
    pub fn tab_at_limit(&self, s: usize, t: usize) -> bool {
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return false;
        };
        if !tab.is_running() {
            return false;
        }
        if self.limit_hit.contains(&tab.uid) || crate::app_failover::limit_error(&tab.screen_text())
        {
            return true;
        }
        matches!(
            tab.activity,
            crate::pane::Activity::Ready | crate::pane::Activity::Idle
        ) && self.panes[s]
            .account
            .and_then(|a| self.account_binding_left(a))
            .is_some_and(|left| left < 1.0)
    }

    /// Why a move (or anything that stops the tab) must wait for it, in
    /// words: "2 subagents running (general-purpose (36 min), ...)",
    /// "working on a turn", "1 shell running". None when nothing does.
    pub fn tab_busy_why(&self, s: usize, t: usize) -> Option<String> {
        let tab = self.panes.get(s)?.tabs.get(t)?;
        if tab.is_running() && tab.activity == crate::pane::Activity::Working {
            return Some("working on a turn".into());
        }
        self.tab_background_work(s, t)?;
        let a = self.tab_activity(s, t);
        let mut parts = vec![];
        if let Some(l) = a.subagents_line() {
            parts.push(format!("{l} running"));
        }
        if a.background_shells > 0 {
            parts.push(format!(
                "{} running",
                crate::control::plural(a.background_shells, "shell")
            ));
        }
        if a.background_tasks > 0 {
            parts.push(format!(
                "{} running",
                crate::control::plural(a.background_tasks, "task")
            ));
        }
        Some(if parts.is_empty() {
            "background work running".to_string()
        } else {
            parts.join(", ")
        })
    }

    /// Busy: working, waiting for an approval, or work still running in
    /// the background (tab_background_work). The opposite of idle.
    pub fn tab_busy(&self, s: usize, t: usize) -> bool {
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return false;
        };
        (tab.is_running()
            && matches!(
                tab.activity,
                crate::pane::Activity::Working | crate::pane::Activity::Permission
            ))
            || self.tab_background_work(s, t).is_some()
    }

    /// Stopping its claude now would cut work off: it is working, or work
    /// runs in the background. (Moves and failover wait for this; a tab
    /// waiting for an approval keeps its own rules there.)
    pub fn tab_has_running_work(&self, s: usize, t: usize) -> bool {
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return false;
        };
        (tab.is_running() && tab.activity == crate::pane::Activity::Working)
            || self.tab_background_work(s, t).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_means_nothing_runs() {
        let ps = parse_ps(
            "  100     1 claude --resume x\n  101   100 node /x/mcp-server.js --stdio\n  102   100 /bin/zsh -c npm run dev\n  103   102 node vite\n  200     1 claude\n  201   200 node github-mcp\n",
        );
        let ch = |pid| {
            descendants(&ps, pid)
                .into_iter()
                .map(|p| (p.pid, p.cmd.clone()))
                .collect::<Vec<_>>()
        };
        // A dev server started from the session: background, not idle.
        let a = classify(false, false, Some(120), 0, ch(100), 0);
        assert_eq!(a.state, "background (1 shell, 1 task)");
        assert!(!a.idle());
        // Only its MCP helper: idle.
        let b = classify(false, false, Some(120), 0, ch(200), 0);
        assert_eq!(b.state, "idle");
        assert!(b.idle());
        // A running subagent is never idle.
        assert_eq!(
            classify(false, false, Some(120), 2, vec![], 0).state,
            "background (2 subagents)"
        );
        // A fresh transcript write, busy, a prompt: the specific states.
        assert_eq!(
            classify(false, false, Some(3), 0, vec![], 0).state,
            "working"
        );
        assert_eq!(classify(true, false, None, 0, vec![], 0).state, "working");
        assert_eq!(
            classify(false, true, None, 0, vec![], 0).state,
            "waiting_approval"
        );
        assert_eq!(
            classify(false, false, Some(300), 0, vec![], 1).state,
            "idle_with_loops"
        );
    }

    fn set_mtime(p: &Path, ago: Duration) {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(SystemTime::now() - ago)
            .unwrap();
    }

    /// A subagent whose transcript ends in a tool call or a tool result
    /// runs while it was written in the last few minutes; one whose last
    /// message is plain text is done; a fresh write always counts; one
    /// silent for longer mid call was stopped (an attachment or
    /// api-request record after its last message does not make it run).
    #[test]
    fn subagents_run_until_their_last_message_is_text() {
        let d = std::env::temp_dir().join(format!("godterm-subs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let t = d.join("abc.jsonl");
        let sub = d.join("abc/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(&t, "").unwrap();
        let started = (chrono::Utc::now() - chrono::Duration::minutes(36)).to_rfc3339();
        let prompt = |task: &str| {
            format!(
                r#"{{"type":"user","isSidechain":true,"agentType":"general-purpose","timestamp":"{started}","message":{{"role":"user","content":"{task}"}}}}"#
            )
        };
        let call = r#"{"type":"assistant","isSidechain":true,"message":{"id":"m1","content":[{"type":"text","text":"Running the tests."},{"type":"tool_use","id":"x1","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        let result = r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x1","content":"ok"}]}}"#;
        let done = r#"{"type":"assistant","isSidechain":true,"message":{"id":"m2","content":[{"type":"text","text":"All green."}]}}"#;
        // In a tool call for 3 minutes: a test run, still going.
        std::fs::write(
            sub.join("agent-mid-call.jsonl"),
            format!("{}\n{call}\n", prompt("Run the suite")),
        )
        .unwrap();
        set_mtime(&sub.join("agent-mid-call.jsonl"), Duration::from_secs(180));
        // Thinking after a tool result, 2 minutes ago: going.
        std::fs::write(
            sub.join("agent-thinking.jsonl"),
            format!("{}\n{call}\n{result}\n", prompt("Check the logs")),
        )
        .unwrap();
        set_mtime(&sub.join("agent-thinking.jsonl"), Duration::from_secs(120));
        // Stopped agents: the last real message is a tool call, then only
        // an attachment and an api-request record; 50 minutes old.
        let noise = r#"{"type":"attachment","attachment":{"type":"queued_command","prompt":"x"}}
{"type":"api-request","isSidechain":true}
{"type":"system","subtype":"info"}"#;
        std::fs::write(
            sub.join("agent-stopped.jsonl"),
            format!("{}\n{call}\n{noise}\n", prompt("Stopped one")),
        )
        .unwrap();
        set_mtime(
            &sub.join("agent-stopped.jsonl"),
            Duration::from_secs(50 * 60),
        );
        // Interrupted by the user 4 minutes ago: ended, not running.
        let cut = r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        std::fs::write(
            sub.join("agent-cut.jsonl"),
            format!("{}\n{call}\n{cut}\n", prompt("Cut one")),
        )
        .unwrap();
        set_mtime(&sub.join("agent-cut.jsonl"), Duration::from_secs(4 * 60));
        // Reported 10 minutes ago: done.
        std::fs::write(
            sub.join("agent-done.jsonl"),
            format!("{}\n{call}\n{result}\n{done}\n", prompt("Review it")),
        )
        .unwrap();
        set_mtime(&sub.join("agent-done.jsonl"), Duration::from_secs(600));
        // Mid call but silent for three hours: abandoned.
        std::fs::write(
            sub.join("agent-lost.jsonl"),
            format!("{}\n{call}\n", prompt("Lost one")),
        )
        .unwrap();
        set_mtime(&sub.join("agent-lost.jsonl"), Duration::from_secs(3 * 3600));
        // Done, but written a moment ago: still streaming, counts.
        std::fs::write(
            sub.join("agent-fresh.jsonl"),
            format!("{}\n{call}\n{result}\n{done}\n", prompt("Fresh one")),
        )
        .unwrap();
        let now = SystemTime::now();
        let all = subagents(&t, now);
        assert_eq!(all.len(), 7);
        // The stopped one is unfinished (the screen may vouch for it) but
        // not running by its transcript.
        let stopped = all.iter().find(|s| s.id == "agent-stopped").unwrap();
        assert!(stopped.unfinished && !stopped.running);
        assert!(!all.iter().find(|s| s.id == "agent-cut").unwrap().unfinished);
        let running = running_subagents_of(&t, now);
        let mut ids: Vec<&str> = running.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["agent-fresh", "agent-mid-call", "agent-thinking"]);
        let mid = running.iter().find(|s| s.id == "agent-mid-call").unwrap();
        assert_eq!(mid.name.as_deref(), Some("general-purpose"));
        assert_eq!(mid.task.as_deref(), Some("Run the suite"));
        assert_eq!(mid.tool.as_deref(), Some("Bash"));
        assert!((35 * 60..=38 * 60).contains(&mid.age_s), "{}", mid.age_s);
        assert!(mid.idle_s >= 170);
        assert_eq!(mid.label(), "general-purpose (36 min)");
        assert_eq!(running_subagents(&t, now), 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The screen's "Waiting for N background agents" line and its agent
    /// rows, wherever they sit above the prompt box.
    #[test]
    fn the_screen_shows_background_agents_with_ages() {
        let screen = "⏺ Agent(Review the diff)\n  ⎿  Done (12 tool uses · 41.2k tokens · 3m 10s)\n\n⏺ Waiting for 3 background agents to finish\n  ⎿  ○ general-purpose 36m 26s\n     ○ general-purpose 36m 26s\n     ○ Explore 2m 5s\n     ✔ general-purpose 12m 1s\n\n───────────────────\n❯ \n───────────────────\n  ? for shortcuts\n";
        let a = screen_agents(screen);
        assert_eq!(a.waiting, 3);
        assert_eq!(
            a.rows,
            vec![
                ("general-purpose".to_string(), 36 * 60 + 26),
                ("general-purpose".to_string(), 36 * 60 + 26),
                ("Explore".to_string(), 125),
            ]
        );
        assert_eq!(a.count(), 3);
        assert!(crate::app_continue::background_busy(screen));
        // A footer count alone still counts; a plain prompt does not.
        assert_eq!(screen_agents("❯ \n  2 background tasks\n").count(), 0);
        assert!(crate::app_continue::background_busy(
            "❯ \n  2 background tasks\n"
        ));
        assert!(!crate::app_continue::background_busy(
            "⏺ Done.\n\n❯ \n  ? for shortcuts\n"
        ));
        assert_eq!(parse_age(&["1h", "2m"]), Some(3720));
        assert_eq!(parse_age(&["45s"]), Some(45));
        assert_eq!(parse_age(&["now"]), None);
    }

    #[test]
    fn fresh_subagent_transcripts_count() {
        let d = std::env::temp_dir().join(format!("godterm-act-{}", std::process::id()));
        let t = d.join("abc.jsonl");
        std::fs::create_dir_all(d.join("abc/subagents")).unwrap();
        std::fs::write(&t, "").unwrap();
        std::fs::write(d.join("abc/subagents/agent-1.jsonl"), "{}").unwrap();
        std::fs::write(d.join("abc/subagents/agent-2.jsonl"), "{}").unwrap();
        let old = SystemTime::now() - Duration::from_secs(600);
        std::fs::File::options()
            .write(true)
            .open(d.join("abc/subagents/agent-2.jsonl"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(running_subagents(&t, SystemTime::now()), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
