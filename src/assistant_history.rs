//! Saved assistant conversations: one JSON lines file per conversation in
//! `~/.godterm/assistant/conversations/`, with what the user said (typed,
//! or spoken with whisper's transcript), every tool call and its result
//! (truncated), the replies, timing, and the account and model.
//!
//! The brain runs `claude -p --no-session-persistence`, so claude keeps no
//! session of its own: those would land in the account's projects folder
//! and show up as clutter in Sessions. This log is the record; resuming
//! starts a fresh brain with a summary of it.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::app::App;
use crate::app_assistant::{Entry, Who};

pub fn dir() -> PathBuf {
    crate::assistant::dir().join("conversations")
}

/// The file of the conversation going on.
#[derive(Debug, Clone)]
pub struct ConvLog {
    pub id: String,
    pub path: PathBuf,
}

impl Default for ConvLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ConvLog {
    pub fn new() -> ConvLog {
        let id = format!(
            "{}-{}",
            chrono::Local::now().format("%Y-%m-%d-%H%M%S"),
            &crate::session_ops::new_uuid()[..6]
        );
        let path = dir().join(format!("{id}.jsonl"));
        ConvLog { id, path }
    }

    pub fn open(id: &str) -> Option<ConvLog> {
        let path = dir().join(format!("{id}.jsonl"));
        path.is_file().then(|| ConvLog {
            id: id.to_string(),
            path,
        })
    }

    /// Append one event. Errors are logged, never fatal.
    pub fn write(&self, kind: &str, mut v: Value) {
        v["kind"] = json!(kind);
        v["at"] = json!(chrono::Local::now().to_rfc3339());
        let res = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(dir())?;
            let mut f = {
                use crate::platform::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(&self.path)?
            };
            #[cfg(unix)]
            {
                use crate::platform::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
            }
            writeln!(f, "{v}")
        })();
        if let Err(e) = res {
            crate::log::info(&format!("assistant history: {e}"));
        }
    }
}

/// A saved conversation in the History list.
#[derive(Debug, Clone)]
pub struct Summary {
    pub id: String,
    pub started: String,
    /// The first thing the user said.
    pub first: String,
    pub turns: usize,
    /// All of its text, lower case (for search).
    pub text: String,
}

pub fn read(path: &Path) -> Vec<Value> {
    let Ok(f) = std::fs::File::open(path) else {
        return vec![];
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

pub fn summarize(path: &Path) -> Option<Summary> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let evs = read(path);
    let users: Vec<&Value> = evs.iter().filter(|e| e["kind"] == "user").collect();
    let first = users
        .first()
        .and_then(|u| u["text"].as_str())
        .unwrap_or("")
        .to_string();
    let started = evs
        .first()
        .and_then(|e| e["at"].as_str())
        .map(|t| t.chars().take(16).collect::<String>().replace('T', " "))
        .unwrap_or_default();
    let text = evs
        .iter()
        .filter_map(|e| e["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    Some(Summary {
        id,
        started,
        first,
        turns: users.len(),
        text,
    })
}

/// Saved conversations, newest first.
pub fn list() -> Vec<Summary> {
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return vec![];
    };
    let mut v: Vec<Summary> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| summarize(&p))
        .filter(|s| s.turns > 0)
        .collect();
    v.sort_by(|a, b| b.id.cmp(&a.id));
    v
}

/// Delete conversations older than `days` (0 keeps all). Returns how many.
pub fn purge(days: u32) -> usize {
    if days == 0 {
        return 0;
    }
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return 0;
    };
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(days as u64 * 86_400);
    let mut n = 0;
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t < cutoff);
        if old
            && e.path().extension().is_some_and(|x| x == "jsonl")
            && std::fs::remove_file(e.path()).is_ok()
        {
            n += 1;
        }
    }
    n
}

pub fn delete(id: &str) -> bool {
    !id.contains('/') && std::fs::remove_file(dir().join(format!("{id}.jsonl"))).is_ok()
}

/// A line about pausing listening (the pause itself, waiting for the
/// wake word): it must not carry into a new process, where it would read
/// as if listening were still paused.
pub fn about_pause(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "paus",
        "i'll wait",
        "i will wait",
        "say hey god",
        "won't respond",
        "not listening",
        "stop listening",
        "hold on",
        "hang on",
    ]
    .iter()
    .any(|w| t.contains(w))
}

/// The conversation had a pause in it (now over).
pub fn had_pause(evs: &[Value]) -> bool {
    evs.iter()
        .any(|e| e["kind"] == "tool" && e["name"] == "pause_listening")
        || evs.iter().any(|e| {
            matches!(e["kind"].as_str(), Some("user" | "reply"))
                && about_pause(e["text"].as_str().unwrap_or(""))
        })
}

/// What a resumed brain is told: the last exchanges, about 1500 chars.
/// Pause talk is left out and replaced by a note that it has ended.
pub fn carry_summary(evs: &[Value]) -> String {
    let paused = had_pause(evs);
    let mut parts: Vec<String> = evs
        .iter()
        .filter(|e| !about_pause(e["text"].as_str().unwrap_or("")))
        .filter_map(|e| match e["kind"].as_str() {
            Some("user") => Some(format!("user: {}", e["text"].as_str().unwrap_or(""))),
            Some("reply") => Some(format!("you: {}", e["text"].as_str().unwrap_or(""))),
            _ => None,
        })
        .collect();
    let mut out = String::new();
    while let Some(p) = parts.pop() {
        if out.len() + p.len() > 1500 {
            break;
        }
        out = if out.is_empty() {
            p
        } else {
            format!("{p} / {out}")
        };
    }
    if paused {
        out.push_str(" (The earlier pause has ended: listening is active.)");
    }
    out
}

/// The History tab of the assistant panel.
#[derive(Debug, Clone, Default)]
pub struct HistoryUi {
    pub items: Vec<Summary>,
    pub query: String,
    pub sel: usize,
    /// Reading this conversation.
    pub open: Option<String>,
    /// Delete was pressed once; press again to delete.
    pub confirm_delete: bool,
}

impl HistoryUi {
    pub fn load() -> HistoryUi {
        HistoryUi {
            items: list(),
            ..Default::default()
        }
    }

    pub fn shown(&self) -> Vec<&Summary> {
        let q = self.query.trim().to_lowercase();
        self.items
            .iter()
            .filter(|s| q.is_empty() || s.text.contains(&q) || s.id.contains(&q))
            .collect()
    }
}

impl App {
    pub fn toggle_assistant_history(&mut self) {
        self.assistant.history = match self.assistant.history {
            Some(_) => None,
            None => Some(HistoryUi::load()),
        };
    }

    /// Continue a saved conversation: a fresh brain told what was said,
    /// the panel shows it, new turns go on in the same file.
    pub fn resume_conversation(&mut self, id: &str) -> bool {
        let Some(c) = ConvLog::open(id) else {
            self.flash("That conversation is gone");
            return false;
        };
        if self.assistant.busy {
            self.assistant.resume_after = Some(id.to_string());
            return true;
        }
        let evs = read(&c.path);
        self.assistant.brain = None;
        self.assistant.turns_in_brain = 0;
        self.assistant.carry = Some(carry_summary(&evs));
        self.assistant.log.clear();
        for e in evs
            .iter()
            .rev()
            .take(60)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let who = match e["kind"].as_str() {
                Some("user") => Who::User,
                Some("reply") => Who::Reply,
                _ => continue,
            };
            self.assistant.log.push(Entry {
                who,
                text: e["text"].as_str().unwrap_or("").to_string(),
            });
        }
        let started = summarize(&c.path).map(|s| s.started).unwrap_or_default();
        self.assistant.conv = Some(c);
        self.assistant.history = None;
        self.assistant.show = true;
        self.assistant.log.push(Entry {
            who: Who::Note,
            text: format!("Continuing the conversation from {started}."),
        });
        true
    }

    pub fn delete_conversation(&mut self, id: &str) {
        if self.assistant.conv.as_ref().is_some_and(|c| c.id == id) {
            self.assistant.conv = None;
        }
        delete(id);
        if let Some(h) = &mut self.assistant.history {
            h.items.retain(|s| s.id != id);
            h.open = None;
            h.confirm_delete = false;
            h.sel = h.sel.min(h.shown().len().saturating_sub(1));
        }
    }

    /// Keys while the History tab is open.
    pub fn on_history_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        let Some(h) = self.assistant.history.as_mut() else {
            return false;
        };
        let n = h.shown().len();
        let sel_id = h
            .shown()
            .get(h.sel.min(n.saturating_sub(1)))
            .map(|s| s.id.clone());
        if let Some(open) = h.open.clone() {
            match k.code {
                KeyCode::Esc => {
                    h.open = None;
                    h.confirm_delete = false;
                }
                KeyCode::Char('r') | KeyCode::Enter => {
                    self.resume_conversation(&open);
                }
                KeyCode::Char('d') if h.confirm_delete => self.delete_conversation(&open),
                KeyCode::Char('d') => h.confirm_delete = true,
                _ => h.confirm_delete = false,
            }
            return true;
        }
        match k.code {
            KeyCode::Esc => self.assistant.history = None,
            KeyCode::Up => h.sel = h.sel.saturating_sub(1),
            KeyCode::Down => h.sel = (h.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Enter => h.open = sel_id,
            KeyCode::Backspace => {
                h.query.pop();
                h.sel = 0;
            }
            KeyCode::Char(c) => {
                h.query.push(c);
                h.sel = 0;
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carry_keeps_the_latest() {
        let evs: Vec<Value> = (0..100)
            .flat_map(|i| {
                [
                    json!({"kind": "user", "text": format!("question {i}")}),
                    json!({"kind": "tool", "name": "x"}),
                    json!({"kind": "reply", "text": format!("answer {i}")}),
                ]
            })
            .collect();
        let c = carry_summary(&evs);
        assert!(
            c.ends_with("you: answer 99") && c.len() <= 1500 && !c.contains("question 0 "),
            "{c}"
        );
    }
}
