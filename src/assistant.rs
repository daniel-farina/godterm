//! The assistant's brain: one long lived headless claude
//! (`claude -p` with stream-json in and out) on the chosen account's
//! config dir, with only godterm's MCP tools, in an empty scratch folder.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;

use crate::app::AppEvent;

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
    child: Child,
    stdin: ChildStdin,
    pub account: usize,
    pub model: String,
    /// Which process this is (events from an older one are ignored).
    pub gen: u64,
}

static BRAIN_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn dir() -> PathBuf {
    crate::config::app_home().join("assistant")
}

/// The MCP config handing claude the `godterm mcp` server.
pub fn write_mcp_config(exe: &Path) -> Result<PathBuf> {
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let p = d.join("mcp.json");
    let cfg = json!({"mcpServers": {"godterm": {
        "command": exe,
        "args": ["mcp"],
        "env": {"GODTERM_HOME": crate::config::app_home(), "GODTERM_CLIENT": "brain", "GODTERM_TOKEN": crate::control::brain_token().unwrap_or_default()},
    }}});
    // In tests the server would be the test binary: mark it, so its guard
    // stops it before it runs the suite (see test_guard).
    let mut cfg = cfg;
    if cfg!(test) {
        cfg["mcpServers"]["godterm"]["env"][crate::test_guard::ENV] = json!("brain-mcp");
    }
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
    /// Start it on `account` (config dir `config_dir`).
    pub fn start(
        claude: &str,
        config_dir: &Path,
        account: usize,
        a: &crate::config::AssistantCfg,
        pass_env: &[String],
        events: Sender<AppEvent>,
    ) -> Result<Brain> {
        let (model, style) = (a.model.as_str(), a.style.as_str());
        let exe = crate::install::real_exe()?;
        let mcp = write_mcp_config(&exe)?;
        let d = dir();
        std::fs::create_dir_all(&d)?;
        let mut cmd = Command::new(claude);
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--tools",
            "",
            "--model",
            model,
            "--effort",
            &a.effort,
            // The slot's own hooks and plugins stay out of the assistant.
            "--setting-sources",
            "local",
            "--append-system-prompt",
            // The user's learned preferences follow the built in rules.
            &format!(
                "{}{}",
                system_prompt(style),
                crate::learned::prompt_section(&crate::learned::load())
            ),
            "--mcp-config",
        ])
        .arg(&mcp)
        .arg("--allowedTools")
        .args(allowed_tools())
        .current_dir(&d)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        // Same environment hygiene as the tabs.
        for (k, _) in std::env::vars() {
            if crate::pane::should_scrub(&k, pass_env) {
                cmd.env_remove(&k);
            }
        }
        cmd.env("CLAUDE_CONFIG_DIR", config_dir);
        // Quick and quiet: low effort, no thinking at low, short replies,
        // and nothing at startup that is not needed (no updater, telemetry,
        // error reports, surveys, auto memory or other background traffic).
        // (--bare would also skip these but switches off the OAuth login.)
        cmd.env("CLAUDE_EFFORT", &a.effort)
            .env("CLAUDE_CODE_EFFORT_LEVEL", &a.effort)
            .env(
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
                a.max_output_tokens.max(64).to_string(),
            )
            .env("DISABLE_AUTOUPDATER", "1")
            .env("DISABLE_TELEMETRY", "1")
            .env("DISABLE_ERROR_REPORTING", "1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "1")
            .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
            .env("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1");
        if a.effort == "low" {
            cmd.env("CLAUDE_CODE_DISABLE_THINKING", "1")
                .env("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING", "1");
        }
        let gen = BRAIN_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut child = cmd.spawn().with_context(|| format!("starting {claude}"))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        let err_tail = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let et = std::sync::Arc::clone(&err_tail);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut t = et.lock().unwrap_or_else(|e| e.into_inner());
                t.push_str(&line);
                t.push('\n');
                if t.len() > 4000 {
                    let cut = t.len() - 4000;
                    t.drain(..cut);
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                for ev in parse_line(&line) {
                    if events.send(AppEvent::Assistant(gen, ev)).is_err() {
                        return;
                    }
                }
            }
            let tail = err_tail.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let _ = events.send(AppEvent::Assistant(gen, BrainEvent::Exited(tail)));
        });
        Ok(Brain {
            child,
            stdin,
            account,
            model: model.to_string(),
            gen,
        })
    }

    /// Send one user turn.
    pub fn send(&mut self, text: &str) -> Result<()> {
        let msg = json!({"type": "user", "message": {"role": "user", "content": text}});
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Stop the current turn (claude's stream-json control request; the
    /// turn ends with a result, in flight tool calls finish).
    pub fn interrupt(&mut self) -> Result<()> {
        let id = format!("int-{}", std::process::id());
        let msg = json!({"type": "control_request", "request_id": id, "request": {"subtype": "interrupt"}});
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        Ok(())
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Brain {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
                        let name = b["name"]
                            .as_str()
                            .unwrap_or("")
                            .trim_start_matches("mcp__godterm__")
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
                && p.contains("must not change permission")
                && p.contains("list_dir")
        );
    }
}
