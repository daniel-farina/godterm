use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Backend, Caps, Provider, StartCtx, TabModel};
use crate::app::AppEvent;
use crate::assistant::{parse_line, BrainEvent};

pub const PROVIDER: Provider = Provider {
    id: "grok",
    name: "Grok",
    harness: None,
    caps: Caps {
        persistent: false,
        partial: true,
        efforts: &["low", "medium", "high"],
        remote_control: false,
    },
    models: &[
        ("grok-4.7-build-fast", "grok-4.7-build-fast", "quickest"),
        ("grok-4.7", "grok-4.7", "most capable"),
        ("grok-4.6", "grok-4.6", "the previous one"),
    ],
    tab_models: &[
        TabModel {
            arg: "grok-4.7-build-fast",
            label: "4.7 fast",
            desc: "quickest",
            matches: &["grok-4.7-build-fast", "grok 4.7 build fast"],
        },
        TabModel {
            arg: "grok-4.7",
            label: "4.7",
            desc: "most capable",
            matches: &["grok-4.7", "grok 4.7"],
        },
        TabModel {
            arg: "grok-4.6",
            label: "4.6",
            desc: "the previous one",
            matches: &["grok-4.6", "grok 4.6"],
        },
    ],
    usage: |u| super::buckets_of(u, &[("seven_day", "wk", "weekly"), ("product:*", "", "")]),
    bin: |c| crate::harness::Harness::Grok.bin(c.grok_bin.as_deref()),
    own_home: Some(home),
    start,
};

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
    "run_terminal_cmd",
];

pub fn home() -> PathBuf {
    crate::assistant::dir().join("grok-home")
}

pub fn logged_in() -> bool {
    crate::harness::grok::logged_in(&home())
}

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

pub const PRIMER: &str = "/session-info";

const PRIMER_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 15 });

pub fn effort_for(effort: &str) -> &'static str {
    match effort {
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        _ => "low",
    }
}

pub struct GrokBackend {
    launch: Launch,
    model: String,
    effort: &'static str,
    prompt: String,
    session: String,
    started: bool,
    events: std::sync::mpsc::Sender<AppEvent>,
    gen: u64,
    child: Arc<Mutex<Option<Child>>>,

    stop: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Launch {
    bin: String,
    home: PathBuf,
    pass_env: Vec<String>,
}

impl Launch {
    fn command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(args)
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
        c
    }
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
    Ok(Box::new(GrokBackend::new(
        Launch {
            bin: ctx.bin,
            home,
            pass_env: ctx.pass_env.to_vec(),
        },
        ctx.model,
        effort_for(&ctx.cfg.effort),
        format!("{}{}", fit_prompt(ctx.system_prompt), tools_note()),
        ctx.events,
        ctx.gen,
        ctx.session,
    )))
}

const WINDOWS_PROMPT_MAX: usize = 20_000;

fn fit_prompt(p: String) -> String {
    if cfg!(windows) {
        fit_argv(p, WINDOWS_PROMPT_MAX)
    } else {
        p
    }
}

pub fn fit_argv(prompt: String, max: usize) -> String {
    if prompt.len() <= max {
        return prompt;
    }
    let mut end = max;
    while !prompt.is_char_boundary(end) {
        end -= 1;
    }
    let cut = prompt[..end].rfind("\n\n").unwrap_or(end);
    format!(
        "{}\n\n(Some of the rules were left out: they did not fit.)",
        &prompt[..cut]
    )
}

impl GrokBackend {
    fn new(
        launch: Launch,
        model: String,
        effort: &'static str,
        prompt: String,
        events: std::sync::mpsc::Sender<AppEvent>,
        gen: u64,
        session: Option<super::SessionPlan>,
    ) -> GrokBackend {
        let (session, started) = match session {
            Some(s) => (s.id, s.resume),
            None => (crate::session_ops::new_uuid(), false),
        };
        GrokBackend {
            launch,
            model,
            effort,
            prompt,
            session,
            started,
            events,
            gen,
            child: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn args(&self, resume: bool, text: &str) -> Vec<String> {
        turn_args(
            &self.model,
            self.effort,
            &self.prompt,
            &self.session,
            resume,
            text,
        )
    }
}

pub fn turn_args(
    model: &str,
    effort: &str,
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
        "--reasoning-effort".into(),
        effort.into(),
        "--output-format".into(),
        "streaming-messages-json".into(),
        "--include-partial-messages".into(),
        "--disallowed-tools".into(),
        BUILTIN_TOOLS.join(","),
        "--deny".into(),
        "Bash(*)".into(),
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

fn lock(slot: &Mutex<Option<Child>>) -> std::sync::MutexGuard<'_, Option<Child>> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

fn prime(launch: &Launch, args: &[String], slot: &Arc<Mutex<Option<Child>>>) -> Result<()> {
    let mut child = launch
        .command(args)
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", launch.bin))?;
    let out = child.stdout.take().context("no stdout")?;
    let id = child.id();
    crate::control::trust_brain_pid(id);
    *lock(slot) = Some(child);

    let (watch, done) = (slot.clone(), Arc::new(AtomicBool::new(false)));
    let d2 = done.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        while t0.elapsed() < PRIMER_TIMEOUT {
            if d2.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Some(c) = lock(&watch).as_mut().filter(|c| c.id() == id) {
            let _ = c.kill();
        }
    });
    let mut lines = BufReader::new(out).lines();
    let mut ok = false;
    for line in lines.by_ref().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            if v["type"] == "result" {
                ok = v["is_error"] != true;
                break;
            }
        }
    }
    done.store(true, Ordering::SeqCst);
    let child = lock(slot).take_if(|c| c.id() == id);
    std::thread::spawn(move || {
        lines.for_each(drop);
        if let Some(mut c) = child {
            let _ = c.wait();
        }
    });
    if ok {
        Ok(())
    } else {
        anyhow::bail!("grok did not open the session")
    }
}

fn run_turn(
    launch: &Launch,
    args: &[String],
    events: &std::sync::mpsc::Sender<AppEvent>,
    gen: u64,
    slot: &Arc<Mutex<Option<Child>>>,
) {
    let fail = |text: String| {
        let _ = events.send(AppEvent::Assistant(
            gen,
            BrainEvent::Done {
                text,
                cost: None,
                error: true,
            },
        ));
    };
    let mut child = match launch.command(args).spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("starting {}: {e}", launch.bin)),
    };

    crate::control::trust_brain_pid(child.id());
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        return fail("grok has no output".into());
    };
    *lock(slot) = Some(child);
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
    let mut done = false;
    for line in BufReader::new(out).lines().map_while(Result::ok) {
        for ev in parse_line(&line) {
            done |= matches!(ev, BrainEvent::Done { .. });
            if events.send(AppEvent::Assistant(gen, ev)).is_err() {
                return;
            }
        }
    }
    if let Some(mut c) = lock(slot).take() {
        let _ = c.wait();
    }

    if !done {
        std::thread::sleep(Duration::from_millis(30));
        let t = tail.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let why = t
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("grok ended without an answer")
            .to_string();
        fail(why);
    }
}

impl Backend for GrokBackend {
    fn send(&mut self, text: &str) -> Result<()> {
        let first = !self.started;
        self.started = true;
        self.stop.store(false, Ordering::SeqCst);
        let primer = first.then(|| self.args(false, PRIMER));
        let (resume, open) = (self.args(true, text), self.args(false, text));
        let launch = self.launch.clone();
        let (events, gen, slot, stop) = (
            self.events.clone(),
            self.gen,
            self.child.clone(),
            self.stop.clone(),
        );
        std::thread::spawn(move || {
            let mut args = resume;
            if let Some(p) = primer {
                let t0 = Instant::now();
                match prime(&launch, &p, &slot) {
                    Ok(()) => crate::log::info(&format!(
                        "assistant: grok session opened in {} ms",
                        t0.elapsed().as_millis()
                    )),
                    Err(e) => {
                        crate::log::info(&format!(
                            "assistant: grok primer failed ({e:#}); the turn opens the session"
                        ));
                        args = open;
                    }
                }
                if stop.load(Ordering::SeqCst) {
                    let _ = events.send(AppEvent::Assistant(
                        gen,
                        BrainEvent::Done {
                            text: "stopped".into(),
                            cost: None,
                            error: true,
                        },
                    ));
                    return;
                }
            }
            run_turn(&launch, &args, &events, gen, &slot);
        });
        Ok(())
    }

    fn interrupt(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(c) = lock(&self.child).as_ref() {
            crate::procs::interrupt(c.id());
        }
        Ok(())
    }

    fn pid(&self) -> u32 {
        lock(&self.child).as_ref().map(|c| c.id()).unwrap_or(0)
    }

    fn running(&mut self) -> bool {
        true
    }
}

impl Drop for GrokBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = lock(&self.child).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

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

#[cfg(test)]
#[path = "grok_tests.rs"]
mod tests;
