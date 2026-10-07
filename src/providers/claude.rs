//! Claude Code as the brain: one long lived `claude -p` with stream-json
//! in and out, on the account's config dir, with only godterm's MCP tools
//! (no built in tools), in an empty scratch folder.

use anyhow::{Context, Result};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use super::{Backend, Caps, Provider, StartCtx};
use crate::app::AppEvent;
use crate::assistant::{allowed_tools, dir, parse_line, write_mcp_config, BrainEvent};

pub const PROVIDER: Provider = Provider {
    id: "claude",
    name: "Claude",
    harness: Some(crate::harness::Harness::Claude),
    caps: Caps {
        persistent: true,
        partial: true,
        efforts: &["low", "medium", "high"],
    },
    models: &[
        ("claude-haiku-4-5", "Haiku (quickest)"),
        ("sonnet", "Sonnet"),
        ("opus", "Opus"),
    ],
    bin: |c| c.claude_bin(),
    own_home: None,
    start,
};

pub struct ClaudeBackend {
    child: Child,
    stdin: ChildStdin,
}

fn start(ctx: StartCtx) -> Result<Box<dyn Backend>> {
    let a = ctx.cfg;
    let model = ctx.model.as_str();
    let (claude, config_dir, pass_env, events, gen) = (
        ctx.bin.as_str(),
        ctx.home.as_path(),
        ctx.pass_env,
        ctx.events,
        ctx.gen,
    );
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
        &ctx.system_prompt,
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
    Ok(Box::new(ClaudeBackend { child, stdin }))
}

impl Backend for ClaudeBackend {
    fn send(&mut self, text: &str) -> Result<()> {
        let msg = json!({"type": "user", "message": {"role": "user", "content": text}});
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// claude's stream-json control request; the turn ends with a
    /// result, in flight tool calls finish.
    fn interrupt(&mut self) -> Result<()> {
        let id = format!("int-{}", std::process::id());
        let msg = json!({"type": "control_request", "request_id": id, "request": {"subtype": "interrupt"}});
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for ClaudeBackend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
