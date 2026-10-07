//! What "idle" means, in one place. A session is idle only when its agent
//! is not busy, no permission prompt waits, no subagent is running, no
//! background shell or task runs under it (its own helpers such as MCP
//! servers aside) and its transcript has not been written for a while.
//! Everything else gets a specific state: working, waiting_approval,
//! background (2 subagents, 1 shell), or idle_with_loops.

use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// A subagent transcript written this recently is a running subagent.
pub const SUBAGENT_FRESH: Duration = Duration::from_secs(30);
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
pub fn descendants<'a>(table: &'a [Proc], pid: u32) -> Vec<&'a Proc> {
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

/// How many subagents ran in the last SUBAGENT_FRESH, from their
/// transcripts under `<session>/subagents/`.
pub fn running_subagents(transcript: &Path, now: SystemTime) -> usize {
    let dir = transcript.with_extension("").join("subagents");
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
                .filter(|e| {
                    e.metadata()
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| now.duration_since(t).unwrap_or_default() < SUBAGENT_FRESH)
                })
                .count()
        })
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Activity {
    /// working, waiting_approval, background (...), idle_with_loops, idle.
    pub state: String,
    pub subagents_running: usize,
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

    pub fn json(&self) -> Value {
        json!({
            "state": self.state, "subagents_running": self.subagents_running, "background_shells": self.background_shells,
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
        .map(|t| running_subagents(t, now))
        .unwrap_or(0);
    let children = descendants(table, l.pid)
        .into_iter()
        .map(|p| (p.pid, p.cmd.clone()))
        .collect();
    classify(
        l.status == "busy",
        false,
        last_write_s,
        subs,
        children,
        loops,
    )
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
        let subs = transcript
            .as_deref()
            .map(|p| running_subagents(p, now))
            .unwrap_or(0);
        let children = match tab.pid() {
            Some(pid) => descendants(&process_table_cached(), pid)
                .into_iter()
                .map(|p| (p.pid, p.cmd.clone()))
                .collect(),
            None => vec![],
        };
        let busy = tab.activity == crate::pane::Activity::Working;
        let asking = tab.activity == crate::pane::Activity::Permission;
        classify(
            busy,
            asking,
            last_write_s,
            subs,
            children,
            self.loops_in(tab.uid),
        )
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
