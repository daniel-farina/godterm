//! Grok Build as the brain: one headless `grok -p` per turn
//! (streaming-messages-json, the Claude stream shape), the conversation
//! kept with --session-id / --resume. It runs in a home of its own,
//! ~/.godterm/assistant/grok-home, with its own Grok login: the user's
//! Grok accounts are never touched, and HOME points there too so the
//! user's ~/.claude and ~/.cursor plugins, hooks and MCP servers (which
//! Grok would import) stay out. Every built in tool is removed and the
//! shell denied: it acts only through godterm's MCP tools.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use super::{Backend, Caps, Provider, StartCtx};
use crate::app::AppEvent;
use crate::assistant::{parse_line, BrainEvent};

pub const PROVIDER: Provider = Provider {
    id: "grok",
    name: "Grok",
    harness: None,
    caps: Caps {
        persistent: false,
        partial: true,
        efforts: &["none", "low", "medium", "high"],
    },
    models: &[
        ("grok-4.7-build-fast", "Grok 4.7 fast (quickest)"),
        ("grok-4.7", "Grok 4.7"),
        ("grok-4.6", "Grok 4.6"),
    ],
    bin: |c| crate::harness::Harness::Grok.bin(c.grok_bin.as_deref()),
    own_home: Some(home),
    start,
};

/// Grok's built in tools, all removed for the brain.
pub const BUILTIN_TOOLS: &[&str] = &[
    "run_terminal_command",
    "read_file",
    "search_replace",
    "list_dir",
    "grep",
    "kill_command_or_subagent",
    "todo_write",
    "get_command_or_subagent_output",
    "spawn_subagent",
    "scheduler_create",
    "scheduler_delete",
    "scheduler_list",
    "monitor",
    "workflow",
    "enter_plan_mode",
    "exit_plan_mode",
    "ask_user_question",
    "send_feedback",
    "web_search",
    "web_fetch",
    "image_gen",
    "image_edit",
    "image_to_video",
    "reference_to_video",
    "write",
    "Agent",
];

/// The assistant's own Grok home (its login, sessions and MCP config).
pub fn home() -> PathBuf {
    crate::assistant::dir().join("grok-home")
}

pub fn logged_in() -> bool {
    crate::harness::grok::logged_in(&home())
}

/// Its config.toml: godterm's MCP server, and Grok's imports of other
/// tools' settings off.
pub fn write_config(home: &Path, exe: &Path) -> Result<()> {
    std::fs::create_dir_all(home.join("work"))?;
    let m = crate::assistant::mcp_server(exe);
    let mut t = toml_edit::DocumentMut::new();
    let mut compat = toml_edit::Table::new();
    compat.set_implicit(true);
    for vendor in ["claude", "cursor"] {
        let mut c = toml_edit::Table::new();
        for k in ["skills", "rules", "agents", "mcps", "hooks"] {
            c.insert(k, toml_edit::value(false));
        }
        compat.insert(vendor, toml_edit::Item::Table(c));
    }
    t.insert("compat", toml_edit::Item::Table(compat));
    let mut s = toml_edit::Table::new();
    s.insert(
        "command",
        toml_edit::value(m["command"].as_str().unwrap_or("godterm")),
    );
    let mut args = toml_edit::Array::new();
    args.push("mcp");
    s.insert("args", toml_edit::value(args));
    let mut env = toml_edit::InlineTable::new();
    if let Some(o) = m["env"].as_object() {
        for (k, v) in o {
            env.insert(k, v.as_str().unwrap_or("").into());
        }
    }
    s.insert("env", toml_edit::value(env));
    let mut servers = toml_edit::Table::new();
    servers.set_implicit(true);
    servers.insert("godterm", toml_edit::Item::Table(s));
    t["mcp_servers"] = toml_edit::Item::Table(servers);
    crate::config::write_private(&home.join("config.toml"), t.to_string())?;
    Ok(())
}

/// The tool keys in the prompt, so it calls use_tool at once instead of
/// searching first.
pub fn tools_note() -> String {
    let keys: Vec<String> = crate::control::TOOLS
        .iter()
        .map(|t| format!("godterm__{}", t.name))
        .collect();
    format!(
        "\n\nYour tools are godterm's MCP tools, called with use_tool and these keys (search_tool gives a tool's parameters if you need them): {}. You have no shell, file or web tools.",
        keys.join(", ")
    )
}

pub struct GrokBackend {
    bin: String,
    home: PathBuf,
    model: String,
    prompt: String,
    session: String,
    started: bool,
    pass_env: Vec<String>,
    events: std::sync::mpsc::Sender<AppEvent>,
    gen: u64,
    child: Arc<Mutex<Option<Child>>>,
}

fn start(ctx: StartCtx) -> Result<Box<dyn Backend>> {
    let home = ctx.home.clone();
    if !crate::harness::grok::logged_in(&home) {
        anyhow::bail!("the assistant's Grok login is missing: say \"log in the assistant's Grok\"");
    }
    let exe = crate::install::real_exe()?;
    write_config(&home, &exe)?;
    crate::log::info(&format!(
        "assistant: grok brain in {} (HOME and GROK_HOME isolated there; no ~/.claude or ~/.cursor imports; built in tools removed, shell denied)",
        home.display()
    ));
    Ok(Box::new(GrokBackend {
        bin: ctx.bin,
        home,
        model: ctx.model,
        prompt: format!("{}{}", ctx.system_prompt, tools_note()),
        session: crate::session_ops::new_uuid(),
        started: false,
        pass_env: ctx.pass_env.to_vec(),
        events: ctx.events,
        gen: ctx.gen,
        child: Arc::new(Mutex::new(None)),
    }))
}

/// The argv for one turn.
pub fn turn_args(
    model: &str,
    prompt: &str,
    session: &str,
    resume: bool,
    text: &str,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-p".into(),
        text.into(),
        "-m".into(),
        model.into(),
        "--output-format".into(),
        "streaming-messages-json".into(),
        "--include-partial-messages".into(),
        "--disallowed-tools".into(),
        BUILTIN_TOOLS.join(","),
        "--deny".into(),
        "Bash(*)".into(),
        // Headless grok cancels a tool call nobody approves: godterm's
        // own tools are allowed (their confirmations are GodTerm's).
        "--allow".into(),
        "MCPTool(godterm__*)".into(),
        "--system-prompt-override".into(),
        prompt.into(),
        "--max-turns".into(),
        "30".into(),
    ];
    a.push(if resume { "--resume" } else { "--session-id" }.into());
    a.push(session.into());
    a
}

impl Backend for GrokBackend {
    fn send(&mut self, text: &str) -> Result<()> {
        let args = turn_args(&self.model, &self.prompt, &self.session, self.started, text);
        self.started = true;
        let mut c = Command::new(&self.bin);
        c.args(&args)
            .current_dir(self.home.join("work"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, _) in std::env::vars() {
            if crate::pane::should_scrub(&k, &self.pass_env) || crate::harness::scrub_grok(&k) {
                c.env_remove(&k);
            }
        }
        c.env("GROK_HOME", &self.home).env("HOME", &self.home);
        let mut child = c
            .spawn()
            .with_context(|| format!("starting {}", self.bin))?;
        let out = child.stdout.take().context("no stdout")?;
        let err = child.stderr.take().context("no stderr")?;
        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        let tail = Arc::new(Mutex::new(String::new()));
        let t2 = tail.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                let mut t = t2.lock().unwrap_or_else(|e| e.into_inner());
                t.push_str(&l);
                t.push('\n');
                if t.len() > 4000 {
                    let cut = t.len() - 4000;
                    t.drain(..cut);
                }
            }
        });
        let (events, gen, slot) = (self.events.clone(), self.gen, self.child.clone());
        std::thread::spawn(move || {
            let mut done = false;
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                for ev in parse_line(&line) {
                    done |= matches!(ev, BrainEvent::Done { .. });
                    if events.send(AppEvent::Assistant(gen, ev)).is_err() {
                        return;
                    }
                }
            }
            if let Some(mut c) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = c.wait();
            }
            // Ended without a result: the turn failed (say why).
            if !done {
                std::thread::sleep(std::time::Duration::from_millis(30));
                let t = tail.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let why = t
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("grok ended without an answer")
                    .to_string();
                let _ = events.send(AppEvent::Assistant(
                    gen,
                    BrainEvent::Done {
                        text: why,
                        cost: None,
                        error: true,
                    },
                ));
            }
        });
        Ok(())
    }

    /// The turn's process is stopped; its end is reported as a Done.
    fn interrupt(&mut self) -> Result<()> {
        if let Some(c) = self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            crate::procs::interrupt(c.id());
        }
        Ok(())
    }

    fn pid(&self) -> u32 {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|c| c.id())
            .unwrap_or(0)
    }

    /// A process per turn: always ready for the next one.
    fn running(&mut self) -> bool {
        true
    }
}

impl Drop for GrokBackend {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// The real tool behind a use_tool call ("godterm__get_state").
pub fn unwrap_use_tool(input: &Value) -> Option<(String, Value)> {
    let name = ["tool_name", "name", "tool", "key"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))?;
    let args = [
        "tool_input",
        "arguments",
        "args",
        "input",
        "parameters",
        "params",
    ]
    .iter()
    .find_map(|k| input.get(*k).cloned())
    .unwrap_or(Value::Null);
    let args = match args {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        v => v,
    };
    Some((name.trim_start_matches("godterm__").to_string(), args))
}
