//! The assistant's memory across restarts: a compact block, built from the
//! conversations saved in the last `assistant.memory_days`, that a new
//! brain gets in its first message. Each conversation is a few lines (the
//! summary the brain saved with remember_summary, or one made from what
//! was asked and which tools ran); the most recent one also has its last
//! turns word for word. About 1.5k tokens at most.

use serde_json::Value;
use std::path::Path;

use crate::assistant_history::{dir, read};

/// Most characters of the block (about 1.5k tokens).
pub const MAX_CHARS: usize = 6000;

/// One saved conversation, remembered.
#[derive(Debug, Clone, PartialEq)]
pub struct Remembered {
    pub id: String,
    /// "today 7:37 PM".
    pub started: String,
    /// "7:48 PM".
    pub ended: String,
    /// One to three lines.
    pub summary: String,
    /// Its last turns, "user: ..." / "you: ...".
    pub last_turns: Vec<String>,
    pub turns: usize,
}

fn time_of(e: &Value) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    e["at"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
}

fn clip(s: &str, n: usize) -> String {
    crate::sessions::snippet(&s.replace('\n', " "), n)
}

/// What a tool call touched, briefly: "sessions project=lol/botmesh",
/// "open_tab t12 ~/godterm-tabs/calc".
fn tool_line(e: &Value) -> Option<String> {
    let name = e["name"].as_str()?;
    if matches!(
        name,
        "get_state" | "ignore" | "remember_summary" | "pause_listening"
    ) {
        return None;
    }
    let args = &e["args"];
    let mut bits: Vec<String> = vec![];
    for k in [
        "tab", "project", "text", "id", "dir", "name", "account", "to", "seconds", "query", "path",
        "paths",
    ] {
        match &args[k] {
            Value::String(s) if !s.is_empty() => bits.push(format!("{k}={}", clip(s, 40))),
            Value::Number(n) => bits.push(format!("{k}={n}")),
            Value::Array(a) if !a.is_empty() => bits.push(format!(
                "{k}={}",
                clip(
                    &a.iter()
                        .map(|x| x
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| x.to_string()))
                        .collect::<Vec<_>>()
                        .join(","),
                    40
                )
            )),
            _ => {}
        }
    }
    // Tab ids and folders from the result, when it opened something.
    let r = e["result"].as_str().unwrap_or("");
    for key in ["\"tab\":\"t", "\"folder\":\""] {
        if let Some(i) = r.find(key) {
            let v: String = r[i + key.len() - if key.starts_with("\"tab") { 1 } else { 0 }..]
                .chars()
                .take_while(|c| *c != '"')
                .collect();
            if !v.is_empty() && bits.len() < 6 {
                bits.push(v);
            }
        }
    }
    Some(if bits.is_empty() {
        name.to_string()
    } else {
        format!("{name} {}", bits.join(" "))
    })
}

/// A tab a remembered conversation named, with what it was then.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TabRef {
    /// "t19".
    pub tab: String,
    pub session: Option<String>,
    pub folder: Option<String>,
}

/// The string value after `"key":"` in a (possibly cut off) JSON text.
fn json_str(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = text.find(&pat)? + pat.len();
    let v: String = text[i..].chars().take_while(|c| *c != '"').collect();
    (!v.is_empty()).then_some(v)
}

/// Every tab id the conversation's tools named, with the session and
/// folder it had then (from the tool's arguments and result).
pub fn tab_refs(evs: &[Value]) -> Vec<TabRef> {
    let mut out: Vec<TabRef> = vec![];
    for e in evs.iter().filter(|e| e["kind"] == "tool") {
        let r = e["result"].as_str().unwrap_or("");
        let args = &e["args"];
        let tab = json_str(r, "tab")
            .filter(|t| t.starts_with('t'))
            .or_else(|| args["tab"].as_str().map(str::to_string))
            .filter(|t| t.len() > 1 && t[1..].chars().all(|c| c.is_ascii_digit()));
        let Some(tab) = tab else { continue };
        let session = json_str(r, "opened")
            .or_else(|| json_str(r, "session_id"))
            .or_else(|| {
                (e["name"] == "open_session")
                    .then(|| args["id"].as_str().map(str::to_string))
                    .flatten()
            })
            .filter(|s| s.len() >= 8 && s.contains('-'));
        let folder = json_str(r, "folder").or_else(|| json_str(r, "cwd"));
        match out.iter_mut().find(|x| x.tab == tab) {
            Some(x) => {
                x.session = x.session.take().or(session);
                x.folder = x.folder.take().or(folder);
            }
            None => out.push(TabRef {
                tab,
                session,
                folder,
            }),
        }
    }
    out
}

/// Tab refs of the conversations the memory block holds.
pub fn recent_tab_refs(days: u32) -> Vec<TabRef> {
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return vec![];
    };
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(days.max(1) as u64 * 86_400);
    let mut paths: Vec<std::path::PathBuf> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= cutoff)
        })
        .map(|e| e.path())
        .collect();
    paths.sort();
    // Later conversations win for the same id.
    let mut out: Vec<TabRef> = vec![];
    for p in paths {
        for r in tab_refs(&read(&p)) {
            out.retain(|x| x.tab != r.tab);
            out.push(r);
        }
    }
    out
}

/// "today 7:37 PM", "yesterday 3:52 PM", "Mon Oct 5 9:10 AM": spoken
/// times, so the model never converts a 24 hour clock (it said "two
/// fifty-five" for 19:41).
fn when(t: chrono::DateTime<chrono::FixedOffset>) -> String {
    let local = t.with_timezone(&chrono::Local);
    let today = chrono::Local::now().date_naive();
    let day = local.date_naive();
    let d = if day == today {
        "today".to_string()
    } else if Some(day) == today.pred_opt() {
        "yesterday".to_string()
    } else {
        local.format("%a %b %-d").to_string()
    };
    format!("{d} {}", local.format("%-I:%M %p"))
}

/// The saved summary, or one made from the events.
pub fn remember(path: &Path) -> Option<Remembered> {
    let evs = read(path);
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let users: Vec<&Value> = evs.iter().filter(|e| e["kind"] == "user").collect();
    if users.is_empty() {
        return None;
    }
    let started = evs.first().and_then(time_of).map(when).unwrap_or_default();
    let ended = evs
        .last()
        .and_then(time_of)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%-I:%M %p")
                .to_string()
        })
        .unwrap_or_default();
    let saved = evs
        .iter()
        .rev()
        .find(|e| e["kind"] == "summary")
        .and_then(|e| e["text"].as_str())
        .map(str::to_string);
    let summary = saved.unwrap_or_else(|| {
        let first = users.first().and_then(|u| u["text"].as_str()).unwrap_or("");
        let last = users.last().and_then(|u| u["text"].as_str()).unwrap_or("");
        let mut lines = vec![format!("Asked: {}", clip(first, 140))];
        if users.len() > 1 {
            lines.push(format!("Last asked: {}", clip(last, 140)));
        }
        let mut tools: Vec<String> = evs
            .iter()
            .filter(|e| e["kind"] == "tool")
            .filter_map(tool_line)
            .collect();
        tools.dedup();
        if !tools.is_empty() {
            let n = tools.len();
            let shown: Vec<String> = tools
                .into_iter()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            lines.push(format!(
                "Did: {}{}",
                shown.join("; "),
                if n > 6 {
                    format!(" (and {} more)", n - 6)
                } else {
                    String::new()
                }
            ));
        }
        // Left open: a question asked without a later yes, or the last
        // request never answered.
        let last_user_i = evs.iter().rposition(|e| e["kind"] == "user").unwrap_or(0);
        let answered = evs[last_user_i..].iter().any(|e| e["kind"] == "reply");
        // Left open in that chat: said as of then. A yes/no question that
        // came from a confirmation has expired by now (two minutes), so
        // it is not carried at all; the state says what waits now.
        let ended = evs.last().and_then(time_of);
        let old = ended.is_none_or(|t| {
            chrono::Local::now().signed_duration_since(t) > chrono::Duration::minutes(2)
        });
        let asked_confirm = evs.iter().rev().take(6).any(|e| {
            e["kind"] == "tool"
                && e["result"]
                    .as_str()
                    .is_some_and(|r| r.contains("needs_confirmation"))
        });
        let when = ended
            .map(|t| t.format("%-I:%M %p").to_string())
            .unwrap_or_default();
        if !answered {
            lines.push(format!("Unanswered then ({when}): \"{}\"", clip(last, 100)));
        } else if let Some(q) = evs
            .iter()
            .rev()
            .find(|e| e["kind"] == "reply")
            .and_then(|e| e["text"].as_str())
            .filter(|t| t.trim_end().ends_with('?'))
        {
            if !(old && asked_confirm) {
                let tag = if old {
                    format!("Left open then ({when}), may be resolved or expired")
                } else {
                    "Pending".to_string()
                };
                lines.push(format!("{tag}: you asked \"{}\"", clip(q, 120)));
            }
        }
        lines.join("\n")
    });
    // Pause talk would read as if listening were still paused.
    let turns: Vec<String> = evs
        .iter()
        .filter(|e| !crate::assistant_history::about_pause(e["text"].as_str().unwrap_or("")))
        .filter_map(|e| match e["kind"].as_str() {
            Some("user") => Some(format!(
                "user: {}",
                clip(e["text"].as_str().unwrap_or(""), 200)
            )),
            Some("reply") => Some(format!(
                "you: {}",
                clip(e["text"].as_str().unwrap_or(""), 200)
            )),
            _ => None,
        })
        .collect();
    let mut last_turns = turns[turns.len().saturating_sub(10)..].to_vec();
    if crate::assistant_history::had_pause(&evs) {
        last_turns.push("(The earlier pause has ended: listening is active.)".into());
    }
    Some(Remembered {
        id,
        started,
        ended,
        summary,
        last_turns,
        turns: users.len(),
    })
}

/// Conversations of the last `days` days, oldest first.
pub fn recent(days: u32) -> Vec<Remembered> {
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return vec![];
    };
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(days.max(1) as u64 * 86_400);
    let mut paths: Vec<std::path::PathBuf> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= cutoff)
        })
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths.iter().filter_map(|p| remember(p)).collect()
}

/// The block a new brain gets: every recent conversation in a few lines,
/// the last one's turns word for word, within MAX_CHARS (older
/// conversations go first when it is too long).
pub fn block(days: u32) -> Option<String> {
    let mut convs = recent(days);
    if convs.is_empty() {
        return None;
    }
    let render = |convs: &[Remembered]| -> String {
        let mut out = format!(
            "(Recent conversation memory: your chats with the user in the last {} day{}, oldest first. Use it when they refer to something earlier; call history for more.)\n",
            days,
            if days == 1 { "" } else { "s" }
        );
        for (i, c) in convs.iter().enumerate() {
            out.push_str(&format!(
                "- {} to {} ({} request{}): {}\n",
                c.started,
                c.ended,
                c.turns,
                if c.turns == 1 { "" } else { "s" },
                c.summary.replace('\n', " | ")
            ));
            if i + 1 == convs.len() && !c.last_turns.is_empty() {
                out.push_str("  Its last turns:\n");
                for t in &c.last_turns {
                    out.push_str(&format!("  {t}\n"));
                }
            }
        }
        out
    };
    let mut text = render(&convs);
    while text.chars().count() > MAX_CHARS && convs.len() > 1 {
        convs.remove(0);
        text = render(&convs);
    }
    Some(text.chars().take(MAX_CHARS).collect())
}

/// How many conversations the memory holds (for the panel).
pub fn count(days: u32) -> usize {
    recent(days).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_summary_says_what_was_asked_done_and_left() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("godterm-mem-{}", std::process::id()));
        crate::config::testing::set_home(&home);
        let c = crate::assistant_history::ConvLog::new();
        c.write(
            "user",
            serde_json::json!({"text": "lol/bot mesh directory"}),
        );
        c.write("tool", serde_json::json!({"name": "sessions", "args": {"project": "lol/bot mesh"}, "result": "{\"ok\":true}"}));
        c.write(
            "reply",
            serde_json::json!({"text": "Two sessions in lol/botmesh. Want me to open one?"}),
        );
        let r = remember(&c.path).unwrap();
        assert!(
            r.summary.contains("Asked: lol/bot mesh directory"),
            "{}",
            r.summary
        );
        assert!(
            r.summary.contains("sessions project=lol/bot mesh"),
            "{}",
            r.summary
        );
        assert!(r.summary.contains("Pending: you asked"), "{}", r.summary);
        let b = block(2).unwrap();
        assert!(
            b.contains("Recent conversation memory")
                && b.contains("you: Two sessions in lol/botmesh"),
            "{b}"
        );
        // A saved summary wins.
        c.write(
            "summary",
            serde_json::json!({"text": "Looked at lol/botmesh sessions; offered to open one."}),
        );
        assert!(remember(&c.path)
            .unwrap()
            .summary
            .starts_with("Looked at lol/botmesh"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
