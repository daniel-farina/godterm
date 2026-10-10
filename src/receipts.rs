//! Receipts for prompts sent to a tab: what claude's transcript says
//! happened to one, after it was typed in.
//!
//! The records claude writes for a prompt (main chain of the session
//! jsonl):
//! - an idle tab: a `user` record with the prompt (a new turn);
//! - a busy tab: `queue-operation` `enqueue` with the prompt as content;
//!   then either `dequeue` and a `user` record (a turn of its own once
//!   the running one ends), or `remove` with reason `absorbed_mid_turn`
//!   and a `queued_command` attachment (taken into the running turn;
//!   `delivered_to_agent` is the same for a message to a subagent), or
//!   `popAll` (Esc or Up arrow pulled the queue back into the input box:
//!   the text sits there unsent);
//! - the end of a turn: `system` `turn_duration`; an interrupted one has
//!   a `user` "[Request interrupted by user]" (or "... for tool use")
//!   first.
//!
//! A queue entry claude never resolves (it restarted or exited with the
//! prompt still queued) shows as an enqueue with nothing after it.

use std::io::BufReader;
use std::path::Path;

use serde_json::Value;

/// Where a prompt is, after GodTerm typed it in.
#[derive(Debug, Clone, PartialEq)]
pub enum Receipt {
    /// Not in the transcript yet (still queued in GodTerm, being typed,
    /// or typed and waiting to show up).
    Pending,
    /// In the transcript: queued in claude behind its running turn.
    Received,
    /// A turn started with it, or the running turn took it in.
    Started,
    /// That turn finished.
    Done,
    /// It was never run (the reason says how it was lost).
    Dropped(String),
    /// The turn that held it was interrupted before it finished.
    Interrupted(String),
    /// Typed in, but there is no transcript to confirm it (Grok, or a tab
    /// whose session file is not known).
    Unconfirmed,
}

impl Receipt {
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            Receipt::Done | Receipt::Dropped(_) | Receipt::Interrupted(_) | Receipt::Unconfirmed
        )
    }

    /// Lost: the user should resend it.
    pub fn lost(&self) -> bool {
        matches!(self, Receipt::Dropped(_) | Receipt::Interrupted(_))
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Receipt::Dropped(r) | Receipt::Interrupted(r) => Some(r),
            _ => None,
        }
    }
}

/// What the transcript shows for one prompt.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    Nothing,
    /// Queued in claude.
    Enqueued,
    /// A turn of its own (`absorbed` false) or taken into the running one.
    Started {
        absorbed: bool,
    },
    Done,
    /// Taken off claude's queue without being run (popAll, or another
    /// remove reason).
    Dropped(String),
    Interrupted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scan {
    pub seen: Seen,
    /// The turn that holds it ends on a reply with no tool call after it
    /// (over, when the tab is also at its prompt).
    pub turn_quiet: bool,
}

/// Records this long before the send still count (clock and write lag).
const SLACK_MS: i64 = 3000;

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The part of a prompt looked for in the transcript.
pub fn key(text: &str) -> String {
    norm(text).chars().take(40).collect()
}

/// The first words of a prompt, for a spoken notice.
pub fn first_words(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut s = words.iter().take(8).copied().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 60 {
        s = s.chars().take(60).collect();
    }
    if words.len() > 8 || s.chars().count() < norm(text).chars().count() {
        s.push_str("...");
    }
    s
}

fn text_of(v: &Value) -> Option<String> {
    match v {
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
}

/// A record's timestamp, read without parsing the line.
fn stamp_of(line: &str) -> Option<&str> {
    const K: &str = "\"timestamp\":\"";
    let i = line.find(K)? + K.len();
    let rest = &line[i..];
    rest.find('"').map(|j| &rest[..j])
}

fn millis(v: &Value) -> Option<i64> {
    let t = v.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(t)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// Follow one prompt (`text`, typed in at `since_ms`) through transcript
/// lines.
pub fn scan_lines<I: Iterator<Item = String>>(lines: I, text: &str, since_ms: i64) -> Scan {
    let k = key(text);
    let hit = |s: &str| !k.is_empty() && norm(s).contains(&k);
    let mut seen = Seen::Nothing;
    let mut quiet = false;
    let from = since_ms - SLACK_MS;
    let started = |s: &Seen| matches!(s, Seen::Started { .. });
    // Records are stamped like this; older ones are skipped unparsed.
    let from_str = chrono::DateTime::from_timestamp_millis(from)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some(ts) = stamp_of(&line) {
            if ts.len() == from_str.len() && ts.ends_with('Z') && ts < from_str.as_str() {
                continue;
            }
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
        if millis(&v).is_none_or(|t| t < from) {
            continue;
        }
        // Done and interrupted are for good; the rest can still move on
        // (a popAll'd prompt typed in again is queued again).
        if matches!(seen, Seen::Done | Seen::Interrupted) {
            break;
        }
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "queue-operation" => {
                let content = v.get("content").and_then(Value::as_str).unwrap_or("");
                match v.get("operation").and_then(Value::as_str).unwrap_or("") {
                    "enqueue" if hit(content) && !started(&seen) => seen = Seen::Enqueued,
                    "remove" if hit(content) => {
                        let reason = v.get("reason").and_then(Value::as_str).unwrap_or("");
                        match reason {
                            "absorbed_mid_turn" | "delivered_to_agent" if !started(&seen) => {
                                seen = Seen::Started { absorbed: true };
                                quiet = false;
                            }
                            "absorbed_mid_turn" | "delivered_to_agent" => {}
                            _ if seen == Seen::Enqueued => {
                                seen = Seen::Dropped(if reason.is_empty() {
                                    "claude took it off its queue without running it".into()
                                } else {
                                    format!("claude took it off its queue without running it ({reason})")
                                })
                            }
                            _ => {}
                        }
                    }
                    // The queue went back into the input box, unsent.
                    "popAll"
                        if seen == Seen::Enqueued && (content.is_empty() || hit(content)) =>
                    {
                        seen = Seen::Dropped(
                            "it was pulled out of claude's queue back into the input box, unsent (Esc or Up arrow)".into(),
                        )
                    }
                    _ => {}
                }
            }
            "attachment" => {
                let a = &v["attachment"];
                if a["type"] == "queued_command"
                    && text_of(&a["prompt"]).is_some_and(|p| hit(&p))
                    && !started(&seen)
                {
                    seen = Seen::Started { absorbed: true };
                    quiet = false;
                }
            }
            "user" => {
                let Some(t) = text_of(&v["message"]["content"]) else {
                    // A tool result: the turn goes on.
                    quiet = false;
                    continue;
                };
                if t.trim_start().starts_with("[Request interrupted") {
                    if started(&seen) {
                        seen = Seen::Interrupted;
                    }
                    continue;
                }
                if hit(&t) && !started(&seen) {
                    seen = Seen::Started { absorbed: false };
                    quiet = false;
                } else if started(&seen) && !t.trim().is_empty() && !t.starts_with('<') {
                    // Another prompt began a turn: ours had ended.
                    seen = Seen::Done;
                }
            }
            "assistant" if started(&seen) => {
                let blocks = v["message"]["content"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let calls = blocks.iter().any(|b| b["type"] == "tool_use");
                let texts = blocks.iter().any(|b| b["type"] == "text");
                if calls {
                    quiet = false;
                } else if texts {
                    quiet = true;
                }
            }
            "system" if started(&seen) && v["subtype"] == "turn_duration" => seen = Seen::Done,
            _ => {}
        }
    }
    Scan {
        seen,
        turn_quiet: quiet,
    }
}

/// `scan_lines` over a transcript file: from `from` (its length when the
/// prompt was typed in; claude only appends), else its last 4 MB.
pub fn scan(path: &Path, text: &str, since_ms: i64, from: Option<u64>) -> Scan {
    use std::io::{Seek, SeekFrom};
    let nothing = Scan {
        seen: Seen::Nothing,
        turn_quiet: false,
    };
    let Ok(mut f) = std::fs::File::open(path) else {
        return nothing;
    };
    const TAIL: u64 = 4 * 1024 * 1024;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = match from.filter(|o| *o <= len) {
        Some(o) => o.max(len.saturating_sub(TAIL)),
        None => len.saturating_sub(TAIL),
    };
    // Mid line (the tail of a big file): that first line is skipped.
    let skip = start > 0 && from != Some(start);
    if start > 0 && f.seek(SeekFrom::Start(start)).is_err() {
        return nothing;
    }
    let lines = crate::sessions::lossy_lines(BufReader::new(f))
        .enumerate()
        .filter(move |(i, _)| !(skip && *i == 0))
        .map(|(_, l)| l);
    scan_lines(lines, text, since_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn run(recs: &[Value]) -> Scan {
        scan_lines(
            recs.iter().map(|r| r.to_string()),
            "fix the build please",
            1_000_000,
        )
    }

    const T: i64 = 1_000_000;

    #[test]
    fn a_turn_of_its_own() {
        let user = json!({"type": "user", "timestamp": at(T + 10), "message": {"content": "fix the build please"}});
        assert_eq!(
            run(std::slice::from_ref(&user)).seen,
            Seen::Started { absorbed: false }
        );
        let reply = json!({"type": "assistant", "timestamp": at(T + 20), "message": {"content": [{"type": "text", "text": "done"}]}});
        let s = run(&[user.clone(), reply.clone()]);
        assert!(s.turn_quiet);
        let end = json!({"type": "system", "subtype": "turn_duration", "timestamp": at(T + 30)});
        assert_eq!(run(&[user, reply, end]).seen, Seen::Done);
    }

    #[test]
    fn older_records_do_not_count() {
        let user = json!({"type": "user", "timestamp": at(T - 60_000), "message": {"content": "fix the build please"}});
        assert_eq!(run(&[user]).seen, Seen::Nothing);
    }

    #[test]
    fn queued_then_absorbed() {
        let enq = json!({"type": "queue-operation", "operation": "enqueue", "timestamp": at(T + 5), "content": "fix the build please"});
        assert_eq!(run(std::slice::from_ref(&enq)).seen, Seen::Enqueued);
        let rm = json!({"type": "queue-operation", "operation": "remove", "reason": "absorbed_mid_turn", "timestamp": at(T + 9), "content": "fix the build please"});
        assert_eq!(
            run(&[enq.clone(), rm]).seen,
            Seen::Started { absorbed: true }
        );
        let att = json!({"type": "attachment", "timestamp": at(T + 9), "attachment": {"type": "queued_command", "prompt": "fix the build please"}});
        assert_eq!(run(&[enq, att]).seen, Seen::Started { absorbed: true });
    }

    #[test]
    fn popall_and_other_removes_drop_it() {
        let enq = json!({"type": "queue-operation", "operation": "enqueue", "timestamp": at(T + 5), "content": "fix the build please"});
        let pop = json!({"type": "queue-operation", "operation": "popAll", "timestamp": at(T + 6), "content": "fix the build please"});
        assert!(
            matches!(run(&[enq.clone(), pop.clone()]).seen, Seen::Dropped(r) if r.contains("input box"))
        );
        // Typed in again: queued again.
        let again = json!({"type": "queue-operation", "operation": "enqueue", "timestamp": at(T + 8), "content": "fix the build please"});
        assert_eq!(run(&[enq.clone(), pop, again]).seen, Seen::Enqueued);
        let rm = json!({"type": "queue-operation", "operation": "remove", "reason": "cleared", "timestamp": at(T + 6), "content": "fix the build please"});
        assert!(matches!(run(&[enq, rm]).seen, Seen::Dropped(r) if r.contains("cleared")));
    }

    #[test]
    fn an_interrupted_turn() {
        let user = json!({"type": "user", "timestamp": at(T + 10), "message": {"content": "fix the build please"}});
        let tool = json!({"type": "assistant", "timestamp": at(T + 11), "message": {"content": [{"type": "tool_use", "name": "Bash"}]}});
        let int = json!({"type": "user", "timestamp": at(T + 12), "message": {"content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}});
        let end = json!({"type": "system", "subtype": "turn_duration", "timestamp": at(T + 13)});
        assert_eq!(run(&[user, tool, int, end]).seen, Seen::Interrupted);
    }

    #[test]
    fn the_next_prompt_ends_the_turn() {
        let user = json!({"type": "user", "timestamp": at(T + 10), "message": {"content": "fix the build please"}});
        let next = json!({"type": "user", "timestamp": at(T + 50), "message": {"content": "and now the docs"}});
        assert_eq!(run(&[user, next]).seen, Seen::Done);
    }

    #[test]
    fn a_file_is_read_from_the_send() {
        let dir = std::env::temp_dir().join(format!("godterm-receipt-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.jsonl");
        let old = json!({"type": "user", "timestamp": at(T + 1), "message": {"content": "fix the build please"}});
        std::fs::write(&p, format!("{old}\n")).unwrap();
        let from = std::fs::metadata(&p).unwrap().len();
        // Only what came after the send counts (the same text before it
        // was another request).
        assert_eq!(
            scan(&p, "fix the build please", T, Some(from)).seen,
            Seen::Nothing
        );
        assert_eq!(
            scan(&p, "fix the build please", T, None).seen,
            Seen::Started { absorbed: false }
        );
        let enq = json!({"type": "queue-operation", "operation": "enqueue", "timestamp": at(T + 5), "content": "fix the build please"});
        std::fs::write(&p, format!("{old}\n{enq}\n")).unwrap();
        assert_eq!(
            scan(&p, "fix the build please", T, Some(from)).seen,
            Seen::Enqueued
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_words_are_short() {
        assert_eq!(first_words("fix it"), "fix it");
        assert_eq!(
            first_words("one two three four five six seven eight nine ten"),
            "one two three four five six seven eight..."
        );
    }
}
