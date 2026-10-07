//! The assistant's brain: one long lived headless claude
//! (`claude -p` with stream-json in and out) on the chosen account's
//! config dir, with only godterm's MCP tools, in an empty scratch folder.

use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum BrainEvent {
    /// A piece of the reply as it streams.
    Delta(String),
    /// A whole text block (when no deltas were streamed).
    Text(String),
    /// It called a tool.
    Tool(String, Value),
    /// What a tool returned.
    ToolResult(String),
    /// A content block began ("text" or "tool_use").
    BlockStart(String),
    /// The turn ended: final text, cost in USD if reported, error flag.
    Done {
        text: String,
        cost: Option<f64>,
        error: bool,
    },
    /// The process ended (with the tail of stderr).
    Exited(String),
}

pub struct Brain {
    inner: Box<dyn crate::providers::Backend>,
    /// The account it spends (None: a provider with its own login).
    pub account: Option<usize>,
    pub model: String,
    pub provider: &'static str,
    /// Which process this is (events from an older one are ignored).
    pub gen: u64,
}

static BRAIN_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn next_gen() -> u64 {
    BRAIN_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub fn dir() -> PathBuf {
    crate::config::app_home().join("assistant")
}

/// The `godterm mcp` server entry the brain gets: its command, args and
/// environment (the brain's own token).
pub fn mcp_server(exe: &Path) -> Value {
    let mut v = json!({
        "command": exe,
        "args": ["mcp"],
        "env": {"GODTERM_HOME": crate::config::app_home(), "GODTERM_CLIENT": "brain", "GODTERM_TOKEN": crate::control::brain_token().unwrap_or_default()},
    });
    // In tests the server would be the test binary: mark it, so its guard
    // stops it before it runs the suite (see test_guard).
    if cfg!(test) {
        v["env"][crate::test_guard::ENV] = json!("brain-mcp");
    }
    v
}

/// The MCP config handing claude the `godterm mcp` server.
pub fn write_mcp_config(exe: &Path) -> Result<PathBuf> {
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let p = d.join("mcp.json");
    let cfg = json!({"mcpServers": {"godterm": mcp_server(exe)}});
    crate::config::write_private(&p, serde_json::to_string_pretty(&cfg)?)?;
    Ok(p)
}

/// Tool names as claude sees them.
pub fn allowed_tools() -> Vec<String> {
    crate::control::TOOLS
        .iter()
        .map(|t| format!("mcp__godterm__{}", t.name))
        .collect()
}

/// GodTerm's assistant prompt: its sections (the user's edits included,
/// see `sysprompt`), without the learned rules.
pub fn system_prompt(style: &str) -> String {
    crate::sysprompt::assemble(style)
}

impl Brain {
    /// Start `provider` (on `account`, or its own login).
    pub fn start(
        provider: &'static crate::providers::Provider,
        account: Option<usize>,
        ctx: crate::providers::StartCtx,
    ) -> Result<Brain> {
        let model = ctx.model.clone();
        let gen = ctx.gen;
        let inner = (provider.start)(ctx)?;
        Ok(Brain {
            inner,
            account,
            model,
            provider: provider.id,
            gen,
        })
    }

    /// Send one user turn.
    pub fn send(&mut self, text: &str) -> Result<()> {
        self.inner.send(text)
    }

    /// Stop the current turn (the turn still ends with a result).
    pub fn interrupt(&mut self) -> Result<()> {
        self.inner.interrupt()
    }

    pub fn pid(&self) -> u32 {
        self.inner.pid()
    }

    pub fn running(&mut self) -> bool {
        self.inner.running()
    }
}

/// Events in one stream-json line.
pub fn parse_line(line: &str) -> Vec<BrainEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    let mut out = vec![];
    match v.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            let e = &v["event"];
            if e["type"] == "content_block_start" {
                if let Some(k) = e["content_block"]["type"].as_str() {
                    out.push(BrainEvent::BlockStart(k.to_string()));
                }
            }
            if e["type"] == "content_block_delta" && e["delta"]["type"] == "text_delta" {
                if let Some(t) = e["delta"]["text"].as_str() {
                    out.push(BrainEvent::Delta(t.to_string()));
                }
            }
        }
        Some("assistant") => {
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str() {
                            out.push(BrainEvent::Text(t.to_string()));
                        }
                    }
                    Some("tool_use") => {
                        let name = b["name"].as_str().unwrap_or("");
                        // Grok reaches MCP tools through use_tool.
                        if name == "use_tool" {
                            if let Some((n, a)) =
                                crate::providers::grok::unwrap_use_tool(&b["input"])
                            {
                                out.push(BrainEvent::Tool(n, a));
                                continue;
                            }
                        }
                        let name = name
                            .trim_start_matches("mcp__godterm__")
                            .trim_start_matches("godterm__")
                            .to_string();
                        out.push(BrainEvent::Tool(name, b["input"].clone()));
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_result" {
                    let text = match &b["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(a) => a
                            .iter()
                            .filter_map(|x| x["text"].as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    };
                    out.push(BrainEvent::ToolResult(text));
                }
            }
        }
        Some("result") => out.push(BrainEvent::Done {
            text: v["result"].as_str().unwrap_or("").to_string(),
            cost: v["total_cost_usd"].as_f64(),
            error: v["is_error"].as_bool().unwrap_or(false),
        }),
        _ => {}
    }
    out
}

/// The sentences of a reply: cut after . ! ? (or a line break) that is
/// followed by space, so "1.08" and "v2.1" stay whole.
/// claude's "response exceeded the N output token maximum" error.
pub fn token_cap_hit(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("exceeded the") && t.contains("output token maximum")
}

/// The most a retry may raise the reply limit to.
pub const MAX_OUTPUT_CAP: u32 = 32000;

pub fn sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = vec![];
    let mut cur = String::new();
    for i in 0..chars.len() {
        cur.push(chars[i]);
        let end = matches!(chars[i], '.' | '!' | '?' | '\n' | '\u{2026}');
        let next_space = chars.get(i + 1).is_none_or(|c| c.is_whitespace());
        if end && next_space && !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// A sentence that only says what it is about to do ("Let me check...",
/// "I'll check the tab."): never spoken.
pub fn is_narration(sentence: &str) -> bool {
    let s = sentence
        .trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
        .replace('\u{2019}', "'");
    let s = s
        .strip_prefix("now ")
        .or_else(|| s.strip_prefix("first, "))
        .or_else(|| s.strip_prefix("first "))
        .or_else(|| s.strip_prefix("okay, "))
        .or_else(|| s.strip_prefix("ok, "))
        .unwrap_or(&s);
    const OPENERS: &[&str] = &[
        "let me ",
        "let's ",
        "lets ",
        "i'll check",
        "i will check",
        "i'll look",
        "i'll take a look",
        "i'll get ",
        "i'll pull",
        "i'll read",
        "i'll see ",
        "i'll find",
        "i'm going to check",
        "i'm going to look",
        "i'm checking",
        "i'm looking",
        "checking ",
        "looking at ",
        "looking into ",
    ];
    OPENERS.iter().any(|o| s.starts_with(o))
}

/// What of a reply is spoken: narration dropped, at most `max` sentences
/// (0: all). Returns (spoken, narration, the rest left unsaid).
pub fn spoken_part(text: &str, max: usize) -> (String, Vec<String>, Option<String>) {
    let (narr, keep): (Vec<String>, Vec<String>) =
        sentences(text).into_iter().partition(|s| is_narration(s));
    // Only narration: nothing else to say, so it is not dropped.
    if keep.is_empty() {
        return (narr.join(" "), vec![], None);
    }
    let n = if max == 0 {
        keep.len()
    } else {
        max.min(keep.len())
    };
    let rest = keep[n..].join(" ");
    (
        keep[..n].join(" "),
        narr,
        (!rest.trim().is_empty()).then_some(rest),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stream_json() {
        let d = parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Two tabs"}}}"#,
        );
        assert_eq!(d, vec![BrainEvent::Delta("Two tabs".into())]);
        let a = parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","id":"x","name":"mcp__godterm__list_tabs","input":{}}]}}"#,
        );
        assert_eq!(
            a,
            vec![
                BrainEvent::Text("ok".into()),
                BrainEvent::Tool("list_tabs".into(), json!({}))
            ]
        );
        let r = parse_line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"x","content":[{"type":"text","text":"{\"ok\":true}"}]}]}}"#,
        );
        assert_eq!(r, vec![BrainEvent::ToolResult("{\"ok\":true}".into())]);
        let done = parse_line(
            r#"{"type":"result","subtype":"success","result":"Done.","is_error":false,"total_cost_usd":0.0012}"#,
        );
        assert_eq!(
            done,
            vec![BrainEvent::Done {
                text: "Done.".into(),
                cost: Some(0.0012),
                error: false
            }]
        );
        assert!(parse_line("not json").is_empty());
        assert!(parse_line(r#"{"type":"system","subtype":"init"}"#).is_empty());
    }

    #[test]
    fn sentences_split_and_narration() {
        assert_eq!(
            sentences("Marcus runs at 1.08 now. It is brisk! Done"),
            vec!["Marcus runs at 1.08 now.", "It is brisk!", "Done"]
        );
        for n in [
            "Let me check\u{2026}",
            "Let me get the session detail\u{2026}",
            "Now let me look at the PLAN.",
            "I'll check the git log.",
            "I\u{2019}ll check with the tab.",
        ] {
            assert!(is_narration(n), "{n}");
        }
        for a in [
            "I'll tell you when it answers.",
            "Lettuce is on the list.",
            "The tab is idle.",
        ] {
            assert!(!is_narration(a), "{a}");
        }
        let (say, narr, rest) = spoken_part(
            "Let me check. The fix landed. Pauses are gone. Marcus is brisk.",
            2,
        );
        assert_eq!(say, "The fix landed. Pauses are gone.");
        assert_eq!(narr, vec!["Let me check."]);
        assert_eq!(rest.as_deref(), Some("Marcus is brisk."));
        assert_eq!(spoken_part("One. Two.", 0).2, None);
    }

    #[test]
    fn tools_and_prompt() {
        let t = allowed_tools();
        assert!(
            t.contains(&"mcp__godterm__get_state".to_string())
                && t.contains(&"mcp__godterm__close_tabs".to_string())
        );
        assert!(t.iter().all(|x| x.starts_with("mcp__godterm__")));
        let p = system_prompt("concise");
        assert!(
            p.contains("confirm_token")
                && p.contains("change only through the admin tools")
                && p.contains("list_dir")
        );
    }
}
