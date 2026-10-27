#![cfg(unix)]

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ROWS: u16 = 50;
const COLS: u16 = 200;

struct Tui {
    screen: Arc<Mutex<vt100::Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    home: PathBuf,
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn make_home(tag: &str, extra: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("godterm-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let fake = home.join("fake-claude.sh");
    std::fs::write(
        &fake,
        r#"#!/bin/sh
env > "$PWD/.tab-env.txt"
echo "$*" >> "$PWD/.tab-args.txt"
o=""; sep=""
for a in "$@"; do
  if [ ${#a} -gt 60 ]; then a="<long>"; fi
  o="$o$sep$a"; sep=" "
done
echo "fake claude in $(basename "$PWD") args:$o"
echo "? for shortcuts"
exec cat
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let work = home.join("work");
    let proj = home.join("projx");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    let creds = r#"{"claudeAiOauth":{"accessToken":"fake","refreshToken":"fake","expiresAt":4102444800000,"subscriptionType":"max"}}"#;
    let mut cfg = format!(
        "claude_bin = \"{}\"\nnotifications = false\nsetup_dont_show = true\nnew_tab_base = \"{}\"\n",
        fake.display(),
        home.join("tabs").display()
    );

    std::fs::create_dir_all(home.join("tabs")).unwrap();
    for (name, label) in [("one", "Alpha"), ("two", "Bravo")] {
        let dir = home.join("accounts").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".credentials.json"), creds).unwrap();
        cfg.push_str(&format!(
            "[[account]]\nname = \"{name}\"\nlabel = \"{label}\"\ncwd = \"{}\"\n",
            work.display()
        ));
    }
    cfg.push_str("[voice]\ntts = false\nchime = false\nwake_model = \"off\"\n");

    if !extra.contains("[tabs]") {
        cfg.push_str("[tabs]\njobs_tools = false\n");
    }
    cfg.push_str(extra);
    std::fs::write(home.join("config.toml"), cfg).unwrap();

    std::fs::write(home.join("tour_done"), "1").unwrap();
    home
}

fn assert_not_real(p: &std::path::Path, what: &str) {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let real = [".claude", ".grok", ".godterm", ".claudego"].map(|d| home.join(d));
    assert!(
        p != home && !real.iter().any(|r| p.starts_with(r)),
        "test isolation: {what}={} is real user data",
        p.display()
    );
}

impl Tui {
    fn start(home: PathBuf, args: &[&str]) -> Tui {
        Self::start_with(home, args, true)
    }

    fn start_with(home: PathBuf, args: &[&str], rw: bool) -> Tui {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_godterm"));
        cmd.args(args);
        cmd.cwd(&home);
        cmd.env("GODTERM_HOME", &home);
        if rw {
            cmd.env("GODTERM_CONTROL_RW", "1");
        }
        cmd.env("GODTERM_USAGE_FIXTURE", fixture("usage.json"));

        cmd.env("GODTERM_NO_AUDIO", "1");

        cmd.env("GODTERM_NO_MIC", "1");

        cmd.env("GODTERM_NO_OPEN", "1");

        cmd.env("GODTERM_NO_SPLASH", "1");

        cmd.env("GODTERM_MAIN_DIR", home.join("main-claude"));
        cmd.env("GODTERM_MAIN_GROK_DIR", home.join("main-grok"));
        cmd.env("TERM", "xterm-256color");

        cmd.env("GROK_AGENT_ID", "parent-agent");
        cmd.env("GROK_SESSION_ID", "parent-session");

        cmd.env_remove("CLAUDE_CONFIG_DIR");
        cmd.env_remove("GROK_HOME");
        for v in ["GODTERM_HOME", "GODTERM_MAIN_DIR", "GODTERM_MAIN_GROK_DIR"] {
            let p = cmd.get_env(v).map(PathBuf::from).unwrap();
            assert_not_real(&p, v);
        }
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let w2 = Arc::clone(&writer);
        let screen = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let s2 = Arc::clone(&screen);

        let master = Arc::new(Mutex::new(pair.master));
        std::thread::spawn(move || {
            let _keep = master;
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        let mut p = s2.lock().unwrap();
                        p.process(chunk);

                        let has = |pat: &[u8]| chunk.windows(pat.len()).any(|w| w == pat);
                        let mut reply = Vec::new();
                        if has(b"\x1b[6n") {
                            let (r, c) = p.screen().cursor_position();
                            reply
                                .extend_from_slice(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
                        }
                        if has(b"\x1b[c") || has(b"\x1b[0c") {
                            reply.extend_from_slice(b"\x1b[?62;22c");
                        }
                        drop(p);
                        if !reply.is_empty() {
                            let mut w = w2.lock().unwrap();
                            let _ = w.write_all(&reply);
                            let _ = w.flush();
                        }
                    }
                }
            }
        });
        Tui {
            screen,
            writer,
            child,
            home,
        }
    }

    fn text(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    fn send(&mut self, b: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(b).unwrap();
        w.flush().unwrap();
        drop(w);
        std::thread::sleep(Duration::from_millis(120));
    }

    fn ask(&mut self, text: &str) {
        self.send(text.as_bytes());
        let shown: String = text.chars().take(30).collect();
        self.wait_for(&format!("› {shown}"), 10);
        self.send(b"\r");
    }

    fn find(&self, needle: &str) -> Option<(u16, u16)> {
        let screen = self.screen.lock().unwrap();
        let sc = screen.screen();
        let (rows, cols) = sc.size();
        let mut found = None;
        for r in 0..rows {
            let line: String = (0..cols)
                .map(|c| {
                    sc.cell(r, c)
                        .map(|x| {
                            let s = x.contents();
                            if s.is_empty() {
                                " ".to_string()
                            } else {
                                s.to_string()
                            }
                        })
                        .unwrap_or_else(|| " ".into())
                })
                .collect();
            if let Some(i) = line.find(needle) {
                let col = line[..i].chars().count() as u16;
                found = Some((col, r));
            }
        }
        found
    }

    fn click_at(&mut self, col: u16, row: u16) {
        self.send(format!("\x1b[<0;{};{}M", col + 1, row + 1).as_bytes());
        self.send(format!("\x1b[<0;{};{}m", col + 1, row + 1).as_bytes());
    }

    fn click(&mut self, label: &str) {
        let (c, r) = self
            .find(label)
            .unwrap_or_else(|| panic!("{label:?} not on screen:\n{}", self.text()));
        self.click_at(c + label.chars().count() as u16 / 2, r);
    }

    fn cmd(&mut self, key: &[u8]) {
        self.send(b"\x01");
        self.send(key);
    }

    fn wait_file(&self, path: &Path, needle: &str, secs: u64) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            if std::fs::read_to_string(path).is_ok_and(|t| t.contains(needle)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out waiting for {needle:?} in {}: {:?}; screen:\n{}",
            path.display(),
            std::fs::read_to_string(path).unwrap_or_default(),
            self.text()
        );
    }

    fn wait_for(&self, needle: &str, secs: u64) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            if self.text().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {needle:?}; screen:\n{}", self.text());
    }

    fn wait_gone(&self, needle: &str, secs: u64) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            if !self.text().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{needle:?} did not go away; screen:\n{}", self.text());
    }

    fn quit_keep(mut self) {
        self.cmd(b"Q");
        let t0 = Instant::now();
        while self.child.try_wait().ok().flatten().is_none() {
            if t0.elapsed() > Duration::from_secs(10) {
                let _ = self.child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn quit(mut self) -> u32 {
        self.cmd(b"Q");
        let t0 = Instant::now();
        loop {
            if let Ok(Some(s)) = self.child.try_wait() {
                let _ = std::fs::remove_dir_all(&self.home);
                return s.exit_code();
            }
            if t0.elapsed() > Duration::from_secs(10) {
                let _ = self.child.kill();
                panic!("godterm did not exit");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn tabs_overview_and_palette() {
    let home = make_home("tabs", "");
    let proj = home.join("projx");
    let mut t = Tui::start(home, &[]);

    t.wait_for("fake claude in work", 15);

    t.wait_for("args:--dangerously", 5);
    t.wait_for("Alpha", 5);
    t.wait_for("Bravo", 5);

    t.cmd(b"t");
    t.wait_for("New tab for Alpha", 5);
    t.wait_for("Will create:", 5);

    t.send(b"\x0f");
    t.send(b"\x15");
    t.send(proj.to_string_lossy().as_bytes());
    t.send(b"\r");
    t.wait_for("tabs 2", 5);
    t.wait_for("fake claude in projx", 5);
    t.wait_for("Alpha [2/2]", 5);

    t.cmd(b"p");
    t.wait_for("Alpha [1/2]", 5);
    t.cmd(b"n");
    t.wait_for("Alpha [2/2]", 5);

    t.cmd(b"o");
    t.wait_for("Overview: every account", 5);
    t.wait_for("projx", 2);
    t.send(b"\x1b");
    t.wait_gone("Overview: every account", 5);

    t.cmd(b":");
    t.wait_for("command palette", 5);
    t.send(b"close tab");
    t.send(b"\r");
    t.wait_for("tabs 1", 5);
    t.wait_for("Undo", 5);
    t.wait_gone("[2/2]", 5);

    t.cmd(b":");
    t.send(b"in bravo type hello from the palette");
    t.send(b"\r");
    t.wait_for("you in bravo type hello from the palette", 5);
    t.send(b"\x1b");

    t.cmd(b"?");
    t.wait_for("godterm help", 5);
    t.wait_for("Voice: say the wake word", 2);
    t.send(b"x");

    assert_eq!(t.quit(), 0);
}

#[test]
fn tabs_are_restored_after_restart() {
    let home = make_home("restore", "");
    let proj = home.join("projx");
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b"t");
    t.wait_for("New tab for Alpha", 5);
    t.wait_for("Will create:", 5);

    t.send(b"\x0f");
    t.send(b"\x15");
    t.send(proj.to_string_lossy().as_bytes());
    t.send(b"\r");
    t.wait_for("tabs 2", 5);

    t.cmd(b"Q");
    let t0 = Instant::now();
    while t.child.try_wait().ok().flatten().is_none() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let state = std::fs::read_to_string(home.join("state.json")).unwrap();
    assert!(state.contains("projx"));

    let t2 = Tui::start(home, &[]);
    t2.wait_for("tabs 2", 15);

    t2.wait_for("fake claude in projx", 10);
    assert_eq!(t2.quit(), 0);
}

#[test]
fn assistant_remembers_across_restarts() {
    let home = make_home("memory", "");
    write_fakes(
        &home,
        &format!(
            r#"while read -r line; do
  echo "$line" >> "{home}/brain-in.txt"
  case "$line" in
    *"Recent conversation memory"*fotmesh*) say="Earlier we were looking at the code/fotmesh sessions; nothing was running there." ;;
    *fotmesh*) say="Two sessions in code/fotmesh." ;;
    *) say="I don't have context from a previous conversation." ;;
  esac
  echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t","name":"mcp__godterm__get_state","input":{{}}}}]}}}}'
  echo "{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$say\",\"is_error\":false}}"
done
"#,
            home = home.display()
        ),
    );
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("show me the code/fotmesh sessions");
    t.wait_for("Two sessions in code/fotmesh.", 20);
    t.quit_keep();

    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("what was the status of that thing, I forgot to ask you");
    t.wait_for("Earlier we were looking at the code/fotmesh", 20);
    let sent = std::fs::read_to_string(home.join("brain-in.txt")).unwrap();
    assert!(
        sent.contains("Recent conversation memory")
            && sent.contains("show me the code/fotmesh sessions"),
        "{sent}"
    );
    assert!(!t.text().contains("I don't have context"));
}

#[test]
fn misheard_project_resolves_to_fotmesh() {
    if !have("python3") {
        return;
    }
    let home = make_home("fotmesh", "");

    let proj = home.join("main-claude/projects/-Users-x-code-fotmesh");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("1111aaaa-0000-4000-8000-000000000001.jsonl"),
        "{\"type\":\"ai-title\",\"aiTitle\":\"Bot generator changes\"}\n{\"type\":\"user\",\"cwd\":\"/Users/x/code/fotmesh\",\"entrypoint\":\"cli\",\"message\":{\"role\":\"user\",\"content\":\"bot generator changes\"}}\n",
    )
    .unwrap();
    let exe = env!("CARGO_BIN_EXE_godterm");
    let brain = format!(
        r#"#!/usr/bin/env python3
import json, subprocess, sys
HOME = {home:?}
EXE = {exe:?}
def call(name, args):
    lines = json.dumps({{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {{}}}}) + "\n" + json.dumps({{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {{"name": name, "arguments": args}}}}) + "\n"
    out = subprocess.run([EXE, "mcp"], input=lines, capture_output=True, text=True, env={{"GODTERM_HOME": HOME, "PATH": "/usr/bin:/bin"}}).stdout
    for l in out.splitlines():
        v = json.loads(l)
        if v.get("id") == 2:
            r = json.loads(v["result"]["content"][0]["text"])
            with open(HOME + "/calls.jsonl", "a") as f:
                f.write(json.dumps({{"tool": name, "args": args, "reply": r}}) + "\n")
            return r
def emit(say):
    print(json.dumps({{"type": "assistant", "message": {{"content": [{{"type": "tool_use", "id": "x", "name": "mcp__godterm__sessions", "input": {{}}}}]}}}}))
    print(json.dumps({{"type": "result", "subtype": "success", "result": say, "is_error": False}}), flush=True)
# What the model passed in the user's log, turn by turn.
heard = ["code/fot mesh", "code/vodmesh", "code/vodmesh"]
n = 0
for line in sys.stdin:
    r = call("sessions", {{"project": heard[min(n, 2)]}})
    n += 1
    res = r.get("result", {{}})
    emit(res.get("say", "nothing") if res.get("matched") else "The search came back empty.")
"#,
        home = home.display().to_string(),
        exe = exe
    );
    write_fakes(
        &home,
        &format!("exec python3 \"{}/brain.py\"\n", home.display()),
    );
    std::fs::write(home.join("brain.py"), brain).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    for said in [
        "code/fot mesh directory",
        "yeah as the folder is code forward slash vodmesh",
        "yeah no the code slash vod mesh, that's the one",
    ] {
        t.ask(said);
        std::thread::sleep(Duration::from_millis(300));
    }
    t.wait_for("code/fotmesh", 40);
    let calls = std::fs::read_to_string(home.join("calls.jsonl")).unwrap();
    let last: serde_json::Value = serde_json::from_str(calls.lines().last().unwrap()).unwrap();
    assert!(
        last["reply"]["result"]["matched"].as_u64().unwrap_or(0) >= 1,
        "{calls}"
    );
    assert!(
        last["reply"]["result"]["say"]
            .as_str()
            .unwrap()
            .contains("code/fotmesh"),
        "{calls}"
    );
    assert!(!calls.contains("came back empty"), "{calls}");
}

#[test]
fn assistant_pauses_listening() {
    let home = make_home("pause", "");
    let exe = env!("CARGO_BIN_EXE_godterm");
    write_fakes(
        &home,
        &format!(
            r#"while read -r line; do
  case "$line" in
    *"ten seconds"*) secs=',"arguments":{{"seconds":10}}' ;;
    *) secs=',"arguments":{{}}' ;;
  esac
  printf '%s\n%s\n' '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}' "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{{\"name\":\"pause_listening\"$secs}}}}" | GODTERM_HOME="{home}" "{exe}" mcp > "{home}/pause-out.txt"
  echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t","name":"mcp__godterm__pause_listening","input":{{}}}}]}}}}'
  echo '{{"type":"result","subtype":"success","result":"Sure, I will wait.","is_error":false}}'
done
"#,
            home = home.display(),
            exe = exe
        ),
    );
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("hold on for ten seconds");
    t.wait_for("PAUSED 0:", 30);
    let out = std::fs::read_to_string(home.join("pause-out.txt")).unwrap();
    assert!(out.contains("paused_s") && out.contains("10"), "{out}");

    t.wait_gone("PAUSED 0:", 15);
    assert_eq!(t.quit(), 0);
}

#[test]
fn other_clients_are_read_only() {
    let home = make_home("readonly", "");
    let t = Tui::start_with(home.clone(), &[], false);
    t.wait_for("fake claude in work", 15);
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("control.json")).unwrap()).unwrap();
    assert_eq!(info["access"], "read-only");
    let mcp = |tool: &str, args: &str| {
        let input = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{}}}}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{{\"name\":\"{tool}\",\"arguments\":{args}}}}}\n");
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_godterm"))
            .arg("mcp")
            .env("GODTERM_HOME", &home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        String::from_utf8_lossy(&c.wait_with_output().unwrap().stdout).into_owned()
    };
    let look = mcp("get_state", "{}");
    assert!(look.contains("\\\"ok\\\":true"), "{look}");
    let act = mcp("close_tabs", "{\"tab\":\"all\"}");
    assert!(act.contains("read-only"), "{act}");

    let env = std::fs::read_to_string(home.join("work").join(".tab-env.txt")).unwrap_or_default();
    assert!(env.contains("PATH=") && !env.contains("GODTERM_"), "{env}");
}

#[test]
fn a_tab_gets_a_job_server_limited_to_itself() {
    let home = make_home("tabjobs", "[tabs]\njobs_tools = true\n");
    let mut t = Tui::start_with(home.clone(), &[], false);
    t.wait_for("fake claude in work", 15);

    t.wait_file(
        &home.join("work").join(".tab-args.txt"),
        "Prefer the godterm create_job tool",
        10,
    );

    let dir = home.join("tab-mcp");
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .next()
        .expect("a per-tab mcp config");
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let s = &cfg["mcpServers"]["godterm"];
    assert_eq!(s["env"]["GODTERM_CLIENT"], "tab");
    let mcp = |calls: &[(&str, &str)]| {
        let mut input = String::from(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        );
        for (i, (tool, args)) in calls.iter().enumerate() {
            input.push_str(&format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"tools/call\",\"params\":{{\"name\":\"{tool}\",\"arguments\":{args}}}}}\n", i + 2));
        }
        input.push_str("{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"tools/list\"}\n");
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_godterm"));
        c.arg("mcp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        for (k, v) in s["env"].as_object().unwrap() {
            c.env(k, v.as_str().unwrap());
        }
        let mut c = c.spawn().unwrap();
        use std::io::Write;
        c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        String::from_utf8_lossy(&c.wait_with_output().unwrap().stdout).into_owned()
    };
    let out = mcp(&[
        ("list_jobs", "{}"),
        ("get_state", "{}"),
        ("close_tabs", "{\"tab\":\"all\"}"),
        (
            "create_job",
            "{\"prompt\":\"x\",\"every\":\"1 hour\",\"tab\":\"t999\"}",
        ),
        ("create_job", "{\"prompt\":\"x\",\"every\":\"1 hour\"}"),
    ]);
    let lines: Vec<serde_json::Value> = out
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let text = |id: u64| -> String {
        lines
            .iter()
            .find(|l| l["id"] == id)
            .map(|l| {
                l["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default()
    };
    assert!(text(2).contains("\"ok\":true"), "list_jobs: {out}");
    assert!(!text(3).contains("\"ok\":true"), "get_state: {out}");
    assert!(!text(4).contains("\"ok\":true"), "close_tabs: {out}");
    assert!(text(5).contains("only use its own tab"), "{out}");
    assert!(text(6).contains("waiting_for_user"), "{out}");
    let tools = lines.iter().find(|l| l["id"] == 99).unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .len();

    assert_eq!(tools, 7);

    let jobs = std::fs::read_to_string(home.join("jobs.json")).unwrap_or_default();
    assert!(!jobs.contains("\"prompt\""), "{jobs}");

    t.wait_for("job from a tab", 10);
    t.send(b"\x1b");
    t.wait_gone("job from a tab", 5);
    let jobs = std::fs::read_to_string(home.join("jobs.json")).unwrap_or_default();
    assert!(!jobs.contains("\"prompt\""), "declined: {jobs}");
    assert_eq!(t.quit(), 0);
}

#[test]
fn first_run_opens_the_first_account_picker() {
    let home = std::env::temp_dir().join(format!("godterm-it-first-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("Welcome: add your first account", 15);
    for name in ["Claude Code", "Grok Build", "Codex CLI", "OpenCode"] {
        t.wait_for(name, 5);
    }
    assert!(!t.text().contains("How many agent slots"));
    let cfg = std::fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(!cfg.contains("[[accounts]]"), "{cfg}");

    t.send(b"\x1b");
    t.wait_for("Ctrl-a A adds one", 5);
    assert_eq!(t.quit(), 0);
}

#[test]
fn interrupted_setup_writes_no_config() {
    let home = std::env::temp_dir().join(format!("godterm-it-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let mut t = Tui::start(home.clone(), &["setup"]);
    t.wait_for("How many agent slots", 10);
    t.send(b"3\r");
    t.wait_for("Account 1 of 3", 5);
    t.send(b"\x03");
    let t0 = Instant::now();
    while t.child.try_wait().ok().flatten().is_none() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = t.child.kill();
    assert!(
        !home.join("config.toml").exists(),
        "no config after an interrupted setup"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn second_instance_refuses_and_keeps_the_socket() {
    let home = make_home("single", "");
    let t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    let before = std::fs::read_to_string(home.join("control.json")).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_godterm"))
        .env("GODTERM_HOME", &home)
        .env("GODTERM_MAIN_DIR", home.join("main-claude"))
        .env("GODTERM_MAIN_GROK_DIR", home.join("main-grok"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("GODTERM_NO_OPEN", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("GodTerm is already running (pid"), "{err}");
    assert_eq!(
        std::fs::read_to_string(home.join("control.json")).unwrap(),
        before,
        "the socket stays the first one's"
    );
    let lock = std::fs::metadata(home.join("godterm.lock")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(lock.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(home.join("control.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn eager_restore_starts_every_tab() {
    let home = make_home("eager", "restore = \"eager\"\n");
    let work = home.join("work");
    let proj = home.join("projx");
    let state = serde_json::json!({
        "focus": 1,
        "slots": [
            {"account": "one", "active": 0, "sidebar_collapsed": false, "tab_pos": "right", "tabs": [
                {"cwd": work, "session_id": null, "name": "main"},
                {"cwd": proj, "session_id": "sess-123", "name": "api work"},
            ]},
            {"account": "two", "active": 0, "sidebar_collapsed": false, "tabs": [
                {"cwd": proj, "session_id": "sess-456", "name": null},
            ]},
        ],
        "recents": []
    });
    std::fs::write(home.join("state.json"), state.to_string()).unwrap();
    let t = Tui::start(home.clone(), &[]);
    t.wait_for("Restored 3 tabs across 2 accounts", 20);

    let t0 = Instant::now();

    let ready = |s: &str| {
        s.split('│')
            .filter(|seg| {
                let t = seg.trim_start();
                t.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && t.chars().skip(1).collect::<String>().starts_with(" ○ ")
            })
            .count()
    };
    while (ready(&t.text()) < 3 || t.text().contains("starti"))
        && t0.elapsed() < Duration::from_secs(10)
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    let screen = t.text();
    assert!(
        ready(&screen) >= 3 && !screen.contains("starti"),
        "{screen}"
    );
    assert!(screen.contains("--resume sess-456"), "{screen}");

    let mut t = t;
    t.cmd(b"1");
    t.cmd(b"n");
    t.wait_for("api work", 5);
    t.wait_for("--resume sess-123", 5);
    assert_eq!(t.quit(), 0);
}

#[test]
fn move_tab_to_another_account() {
    let home = make_home("movetab", "");
    let work = home.join("work");
    let id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

    let enc: String = work
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let proj = home.join("accounts/one/projects").join(&enc);
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join(format!("{id}.jsonl")),
        format!("{{\"sessionId\":\"{id}\",\"cwd\":\"{}\",\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n", work.display()),
    )
    .unwrap();
    let state = serde_json::json!({
        "focus": 0,
        "slots": [
            {"account": "one", "active": 0, "tabs": [{"cwd": work, "session_id": id, "name": "api"}]},
            {"account": "two", "active": 0, "tabs": [{"cwd": work, "session_id": null, "name": null}]},
        ],
        "recents": []
    });
    std::fs::write(home.join("state.json"), state.to_string()).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("--resume aaaaaaaa", 15);

    t.cmd(b"m");
    t.wait_for("move 'api'", 5);
    t.wait_for("most left", 5);
    t.send(b"\r");
    t.wait_for("Moved 'api' to Bravo", 15);
    assert!(home
        .join("accounts/two/projects")
        .join(&enc)
        .join(format!("{id}.jsonl"))
        .is_file());
    let screen = t.text();

    assert!(
        screen.matches("--resume aaaaaaaa").count() == 1,
        "only the moved tab: {screen}"
    );
    assert!(screen.contains("UNDO MOVE"), "undo chip: {screen}");

    t.cmd(b"U");
    t.wait_for("Moved 'api' to Alpha", 15);
    assert_eq!(t.quit(), 0);
}

#[test]
fn loops_view_and_stop() {
    let home = make_home("loops", "");
    let work = home.join("work");
    let id = "cccccccc-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let enc: String = work
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let proj = home.join("accounts/one/projects").join(&enc);
    std::fs::create_dir_all(&proj).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let lines = [
        serde_json::json!({"type":"user","sessionId":id,"cwd":work,"timestamp":now,"message":{"role":"user","content":"start"}}),
        serde_json::json!({"type":"assistant","timestamp":now,"message":{"content":[{"type":"tool_use","id":"t1","name":"CronCreate","input":{"cron":"*/15 * * * *","prompt":"keep improving the map","recurring":true}}]}}),
        serde_json::json!({"type":"user","timestamp":now,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Scheduled recurring job e0e3839a (Every 15 minutes). Session-only."}]}}),
    ];
    let jsonl = proj.join(format!("{id}.jsonl"));
    std::fs::write(
        &jsonl,
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
    let state = serde_json::json!({
        "focus": 0,
        "slots": [{"account": "one", "active": 0, "tabs": [{"cwd": work, "session_id": id, "name": "map"}]}],
        "recents": []
    });
    std::fs::write(home.join("state.json"), state.to_string()).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.wait_for("⟳1", 15);
    t.wait_for("⟳1", 5);
    t.cmd(b"@");
    t.wait_for("e0e3839a", 5);
    t.wait_for("every 15 minutes", 5);
    t.send(b"s");

    t.send(b"\x1b");
    t.wait_for("Cancel the scheduled job e0e3839a", 10);

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl)
        .unwrap();
    writeln!(f, "{}", serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"CronDelete","input":{"id":"e0e3839a"}}]}})).unwrap();
    writeln!(f, "{}", serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t9","content":"Cancelled job e0e3839a."}]}})).unwrap();
    drop(f);
    t.wait_for("Stopped e0e3839a", 20);
    t.wait_gone("⟳1", 10);
    assert_eq!(t.quit(), 0);
}

#[test]
fn assistant_loop_with_a_stub_brain() {
    let home = make_home("assist", "");
    let exe = env!("CARGO_BIN_EXE_godterm");

    let brain = home.join("fake-claude.sh");
    std::fs::write(
        &brain,
        format!(
            r#"#!/bin/sh
if [ "$1" != "-p" ]; then
  echo "fake claude in $(basename "$PWD") args:$*"
  echo "? for shortcuts"
  exec cat
fi
echo "brain args: $*" > "{home}/brain-args.txt"
while read -r line; do
  printf '%s\n%s\n' '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}' '{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"get_state","arguments":{{}}}}}}' | GODTERM_HOME="{home}" "{exe}" mcp > "{home}/mcp-out.txt"
  echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"mcp__godterm__get_state","input":{{}}}}]}}}}'
  echo '{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"You have two tabs. "}}}}}}'
  echo '{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"Both are ready."}}}}}}'
  echo '{{"type":"result","subtype":"success","result":"You have two tabs. Both are ready.","is_error":false,"total_cost_usd":0.001}}'
done
"#,
            home = home.display(),
            exe = exe
        ),
    )
    .unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);

    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("what is going on");
    t.wait_for("Both are ready.", 20);
    t.wait_for("checked get state", 10);
    t.wait_for("Assistant: Claude on", 5);

    let out = std::fs::read_to_string(home.join("mcp-out.txt")).unwrap();
    assert!(
        out.contains("\"serverInfo\"") && out.contains("\\\"tabs\\\""),
        "{out}"
    );

    let args = std::fs::read_to_string(home.join("brain-args.txt")).unwrap()
        + &std::fs::read_to_string(home.join("assistant").join("system-prompt.md"))
            .unwrap_or_default();
    for want in [
        "--input-format stream-json",
        "--output-format stream-json",
        "--strict-mcp-config",
        "mcp__godterm__answer_prompt",
        "--model claude-sonnet-5-5",
        "--effort low",
        "--session-id ",
        "Do, don't offer",
        "Never answer it with \"Want me to do that?\"",
    ] {
        assert!(args.contains(want), "{want} missing in {args}");
    }
    assert_eq!(t.quit(), 0);
}

const FAKE_TAB: &str = r#"#!/usr/bin/env python3
import os, sys, tty, time, signal
cwd = os.path.basename(os.getcwd())
log = []
buf = ""
def draw(working=False):
    out = "\x1b[2J\x1b[H" + "fake claude in %s args:%s\r\n" % (cwd, " ".join(sys.argv[1:]))
    for l in log[-8:]:
        out += l + "\r\n"
    out += "> " + buf.replace("\n", " NL ") + "\r\n"
    out += ("* Working... (esc to interrupt)" if working else "? for shortcuts") + "\r\n"
    sys.stdout.write(out); sys.stdout.flush()
sys.stdout.write("\x1b[?2004h")
tty.setraw(0)
signal.signal(signal.SIGWINCH, lambda *a: draw())
draw()
paste = False
while True:
    data = os.read(0, 65536)
    if not data:
        break
    s = data.decode("utf-8", "ignore")
    burst_paste = "\x1b[200~" in s
    i = 0
    submit = False
    while i < len(s):
        if s.startswith("\x1b[200~", i):
            paste = True; i += 6; continue
        if s.startswith("\x1b[201~", i):
            paste = False; i += 6; continue
        c = s[i]; i += 1
        if c == "\r" and not paste and not burst_paste:
            submit = True
        elif c in "\r\n":
            buf += "\n"
        elif c == "\x03":
            sys.exit(0)
        else:
            buf += c
    if submit and buf.strip():
        log.append("SUBMITTED: " + buf.replace("\n", " NL "))
        with open("submitted.txt", "a") as f:
            f.write(buf + "\n")
        buf = ""
        draw(True)
        time.sleep(0.6)
    draw()
"#;

fn write_fakes(home: &Path, brain: &str) {
    std::fs::write(home.join("fake-tab.py"), FAKE_TAB).unwrap();
    std::fs::write(
        home.join("fake-claude.sh"),
        format!("#!/bin/sh\nif [ \"$1\" != \"-p\" ]; then\n  exec python3 \"{}/fake-tab.py\" \"$@\"\nfi\n{brain}", home.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in ["fake-tab.py", "fake-claude.sh"] {
            std::fs::set_permissions(home.join(f), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[test]
fn assistant_opens_a_tab_and_prompts_it() {
    if !have("python3") {
        eprintln!("skipping: python3 missing");
        return;
    }
    let home = make_home("chain", "");
    let exe = env!("CARGO_BIN_EXE_godterm");
    write_fakes(
        &home,
        &format!(
            r#"while read -r line; do
  printf '%s\n%s\n' '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}' '{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"open_tab","arguments":{{"account":2,"name":"calculator","prompt":"Create a simple calculator app.\nKeep it tiny."}}}}}}' | GODTERM_HOME="{home}" "{exe}" mcp > "{home}/mcp-out.txt"
  echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"mcp__godterm__open_tab","input":{{}}}}]}}}}'
  echo '{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"Opened calculator on account two and asked it to build a simple calculator."}}}}}}'
  echo '{{"type":"result","subtype":"success","result":"Opened calculator on account two and asked it to build a simple calculator.","is_error":false}}'
done
"#,
            home = home.display(),
            exe = exe
        ),
    );
    let mut t = Tui::start(home.clone(), &[]);

    t.wait_for("fake claude in work", 60);
    t.cmd(b".");
    t.wait_for("Type or speak", 20);
    t.ask("start a new tab on account two and create a simple calculator");
    t.wait_for("asked it to build", 60);
    assert!(home.join("tabs/calculator").is_dir(), "task folder");
    t.send(b"\x01.");
    t.wait_gone("assistant · Claude", 30);
    t.wait_for("fake claude in calculator", 30);

    let submitted = home.join("tabs/calculator/submitted.txt");
    t.wait_file(
        &submitted,
        "Create a simple calculator app.\nKeep it tiny.\n",
        30,
    );

    t.wait_for(
        "SUBMITTED: Create a simple calculator app. NL Keep it tiny.",
        30,
    );
    let out = std::fs::read_to_string(home.join("mcp-out.txt")).unwrap();
    assert!(out.contains("\\\"status\\\":\\\"delivered\\\""), "{out}");
    assert_eq!(
        std::fs::read_to_string(&submitted)
            .unwrap()
            .matches("Create a simple")
            .count(),
        1,
        "submitted once"
    );

    t.send(b"next step");
    t.wait_for("> next step", 10);
    t.send(b"\r");
    t.wait_file(&submitted, "next step\n", 10);
    assert_eq!(t.quit(), 0);
}

#[test]
fn a_cr_in_the_paste_burst_is_a_newline() {
    if !have("python3") {
        return;
    }
    let home = make_home("burst", "");
    write_fakes(&home, "exit 1\n");
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 20,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new("python3");
    cmd.arg(home.join("fake-tab.py"));
    cmd.cwd(&home);
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut w = pair.master.take_writer().unwrap();
    let screen = Arc::new(Mutex::new(vt100::Parser::new(20, 100, 0)));
    let s2 = Arc::clone(&screen);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            s2.lock().unwrap().process(&buf[..n]);
        }
    });
    let wait = |needle: &str| {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            if screen.lock().unwrap().screen().contents().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        panic!(
            "no {needle:?} in:\n{}",
            screen.lock().unwrap().screen().contents()
        );
    };
    wait("? for shortcuts");
    w.write_all(b"\x1b[200~hello\x1b[201~\r").unwrap();
    w.flush().unwrap();
    wait("> hello NL");
    assert!(!screen
        .lock()
        .unwrap()
        .screen()
        .contents()
        .contains("SUBMITTED"));
    std::thread::sleep(Duration::from_millis(100));
    w.write_all(b"\r").unwrap();
    w.flush().unwrap();
    wait("SUBMITTED: hello");
    let _ = child.kill();
}

#[test]
fn assistant_closes_a_batch_with_one_confirmation() {
    if !have("python3") {
        return;
    }
    let home = make_home("batch", "");
    let exe = env!("CARGO_BIN_EXE_godterm");
    write_fakes(
        &home,
        &format!(
            r#"n=0
while read -r line; do
  n=$((n+1))
  if [ $n = 1 ]; then
    printf '%s\n%s\n' '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}' '{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"close_tabs","arguments":{{"tab":"all"}}}}}}' | GODTERM_HOME="{home}" "{exe}" mcp > "{home}/ask.txt"
    q=$(sed -n 's/.*\\"question\\":\\"\([^\\]*\)\\".*/\1/p' "{home}/ask.txt")
    echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"mcp__godterm__close_tabs","input":{{}}}}]}}}}'
    echo "{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$q\",\"is_error\":false}}"
  else
    tok=$(sed -n 's/.*\\"token\\":\\"\([^\\]*\)\\".*/\1/p' "{home}/ask.txt")
    printf '%s\n%s\n' '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}' "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{{\"name\":\"close_tabs\",\"arguments\":{{\"confirm_token\":\"$tok\"}}}}}}" | GODTERM_HOME="{home}" "{exe}" mcp > "{home}/done.txt"
    echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t2","name":"mcp__godterm__close_tabs","input":{{}}}}]}}}}'
    if grep -q not_confirmed "{home}/done.txt"; then
      echo '{{"type":"result","subtype":"success","result":"Want me to close all three? Say yes.","is_error":false}}'
    else
      echo '{{"type":"result","subtype":"success","result":"Closed all three tabs.","is_error":false}}'
    fi
  fi
done
"#,
            home = home.display(),
            exe = exe
        ),
    );
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);

    t.cmd(b"t");
    t.wait_for("New tab", 5);
    t.send(b"\r");
    t.wait_for(" 2 ", 10);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("close all tabs in all accounts");
    t.wait_for("Close 3 tabs across 2 accounts?", 20);
    let ask = std::fs::read_to_string(home.join("ask.txt")).unwrap();
    assert!(ask.contains("needs_confirmation"), "{ask}");

    t.ask("Do you close them?");
    t.wait_for("Want me to close all three? Say yes.", 20);
    assert!(std::fs::read_to_string(home.join("done.txt"))
        .unwrap()
        .contains("not_confirmed"));
    t.ask("yes");
    t.wait_for("Closed all three tabs.", 20);
    let done = std::fs::read_to_string(home.join("done.txt")).unwrap();
    assert!(done.contains("\\\"closed\\\":3"), "{done}");

    let screen = t.text();
    assert_eq!(
        screen.matches("Close 3 tabs across 2 accounts?").count(),
        1,
        "{screen}"
    );
    assert_eq!(t.quit(), 0);
}

#[test]
fn assistant_follow_up_goes_to_the_last_tab() {
    if !have("python3") {
        return;
    }
    let home = make_home("followup", "");
    let exe = env!("CARGO_BIN_EXE_godterm");
    write_fakes(
        &home,
        &format!(
            r#"n=0
while read -r line; do
  n=$((n+1))
  if [ $n = 1 ]; then
    call='{{"name":"open_tab","arguments":{{"account":1,"name":"calc","prompt":"Build a calculator"}}}}'
    say="Opened calc and asked it to build a calculator."
  else
    call='{{"name":"send_prompt","arguments":{{"tab":"last","text":"Make it blue"}}}}'
    say="Asked calc to make it blue."
  fi
  printf '%s\n%s\n' '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}' "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":$call}}" | GODTERM_HOME="{home}" "{exe}" mcp > "{home}/out-$n.txt"
  echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t","name":"mcp__godterm__x","input":{{}}}}]}}}}'
  echo "{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$say\",\"is_error\":false}}"
done
"#,
            home = home.display(),
            exe = exe
        ),
    );
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("create a calculator");
    t.wait_for("Opened calc", 30);
    t.ask("make it blue");
    t.wait_for("Asked calc to make it blue.", 30);

    let out = std::fs::read_to_string(home.join("out-2.txt")).unwrap();
    assert!(out.contains("\\\"tab\\\":\\\"t3\\\""), "{out}");
    assert!(
        out.contains("\\\"status\\\":\\\"delivered\\\"")
            || out.contains("\\\"status\\\":\\\"queued\\\""),
        "{out}"
    );
    t.send(b"\x1b");
    t.wait_for("SUBMITTED: Make it blue", 30);
    t.wait_for("fake claude in calc", 5);
    assert_eq!(t.quit(), 0);
}

#[test]
fn assistant_batch_after_moves_delivers_everywhere() {
    if !have("python3") {
        return;
    }
    let home = make_home("seq", "");
    let exe = env!("CARGO_BIN_EXE_godterm");
    let brain = format!(
        r#"#!/usr/bin/env python3
import json, subprocess, sys
HOME = {home:?}
EXE = {exe:?}
state = {{}}
def call(name, args):
    lines = json.dumps({{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {{}}}}) + "\n" + json.dumps({{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {{"name": name, "arguments": args}}}}) + "\n"
    out = subprocess.run([EXE, "mcp"], input=lines, capture_output=True, text=True, env={{"GODTERM_HOME": HOME, "PATH": "/usr/bin:/bin"}}).stdout
    for l in out.splitlines():
        v = json.loads(l)
        if v.get("id") == 2:
            r = json.loads(v["result"]["content"][0]["text"])
            with open(HOME + "/calls.jsonl", "a") as f:
                f.write(json.dumps({{"tool": name, "args": args, "reply": r}}) + "\n")
            return r
def emit(tool, say):
    print(json.dumps({{"type": "assistant", "message": {{"content": [{{"type": "tool_use", "id": "x", "name": "mcp__godterm__" + tool, "input": {{}}}}]}}}}))
    print(json.dumps({{"type": "result", "subtype": "success", "result": say, "is_error": False}}), flush=True)
n = 0
for line in sys.stdin:
    n += 1
    if n == 1:
        a = call("open_tab", {{"account": 1, "name": "job", "count": 3}})
        b = call("open_tab", {{"account": 2, "name": "web", "count": 2}})
        state["one"] = [o["tab"] for o in a["result"]["opened"]]
        state["two"] = [o["tab"] for o in b["result"]["opened"]]
        emit("open_tab", "Opened five tabs.")
    elif n == 2:
        r = call("move_tab", {{"tab": state["one"], "to": 2}})
        state["moved"] = [m["now"] for m in r["result"]["moved"]]
        emit("move_tab", r["result"]["say"])
    elif n == 3:
        r = call("send_prompt", {{"tab": state["moved"] + state["two"], "text": "Make a clock page"}})
        state["token"] = r["token"]
        # Trying to confirm its own question at once must fail.
        call("send_prompt", {{"confirm_token": r["token"]}})
        emit("send_prompt", r["question"])
    elif n == 4:
        print(json.dumps({{"type": "result", "subtype": "success", "result": "I am waiting for your yes.", "is_error": False}}), flush=True)
    else:
        r = call("send_prompt", {{"confirm_token": state["token"]}})
        emit("send_prompt", r.get("result", {{}}).get("say") or r.get("error") or "?")
"#,
        home = home.display().to_string(),
        exe = exe
    );
    write_fakes(
        &home,
        &format!("exec python3 \"{}/brain.py\"\n", home.display()),
    );
    std::fs::write(home.join("brain.py"), brain).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    for (said, want) in [
        ("create five new tabs", "Opened five tabs."),
        (
            "put the three on account one onto account two",
            "Moved 3 tabs to Bravo.",
        ),
        (
            "in each of them make a clock page",
            "Send that prompt to 5 tabs on Bravo?",
        ),
        ("did you do it", "I am waiting for your yes."),
        ("oh yeah I said yes", "Typed into 5 tabs"),
    ] {
        t.ask(said);
        t.wait_for(want, 40);
    }
    let calls = std::fs::read_to_string(home.join("calls.jsonl")).unwrap();
    assert!(
        calls.contains("same turn"),
        "self confirmation refused: {calls}"
    );

    for d in ["job-1", "job-2", "job-3", "web-1", "web-2"] {
        let got = std::fs::read_to_string(home.join("tabs").join(d).join("submitted.txt"))
            .unwrap_or_default();
        assert_eq!(
            got.matches("Make a clock page").count(),
            1,
            "{d}: {got:?}\n{calls}"
        );
    }
    assert_eq!(t.quit(), 0);
}

#[test]
fn grok_account_runs_isolated() {
    let home = make_home("grok", "");
    let slot = home.join("accounts").join("three");
    std::fs::create_dir_all(&slot).unwrap();

    std::fs::write(
        slot.join("auth.json"),
        r#"{"user":{"email":"g@example.com"},"key":"stub"}"#,
    )
    .unwrap();
    let grok = home.join("fake-grok.sh");
    std::fs::write(
        &grok,
        format!(
            "#!/bin/sh\ncase \"$1\" in \"\"|models) exit 0;; esac\nenv > \"{home}/grok-env.txt\"\necho \"$*\" > \"{home}/grok-args.txt\"\necho \" Grok Build\"\necho \"\"\necho \" Allow this command?\"\necho \" › Allow once\"\necho \"   Always allow this command\"\necho \"   Always allow on all sessions\"\necho \"   Reject\"\nIFS= read -r k\necho \"answered: $k\" > \"{home}/grok-answer.txt\"\necho \" Type a message...\"\nexec cat\n",
            home = home.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&grok, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cfg = home.join("config.toml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text = text.replacen(
        "notifications = false\n",
        &format!("notifications = false\ngrok_bin = \"{}\"\n", grok.display()),
        1,
    );
    text = text.replacen("[voice]", &format!("[[account]]\nname = \"three\"\nlabel = \"Gamma\"\nharness = \"grok\"\ncwd = \"{}\"\n\n[voice]", home.join("work").display()), 1);
    std::fs::write(&cfg, text).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("Gamma", 15);
    t.wait_for("grok", 5);

    let t0 = Instant::now();
    while !home.join("grok-args.txt").is_file() && t0.elapsed() < Duration::from_secs(15) {
        t.cmd(b"3");
        std::thread::sleep(Duration::from_millis(300));
    }
    let args = std::fs::read_to_string(home.join("grok-args.txt")).unwrap();
    assert!(
        args.contains("--leader-socket")
            && args.contains(&slot.join("leader.sock").display().to_string()),
        "{args}"
    );
    assert!(
        args.contains("--always-approve"),
        "bypass maps to --always-approve: {args}"
    );
    let env = std::fs::read_to_string(home.join("grok-env.txt")).unwrap();
    assert!(
        env.contains(&format!("GROK_HOME={}", slot.display())),
        "own GROK_HOME"
    );
    assert!(
        !env.contains("GROK_AGENT_ID") && !env.contains("GROK_SESSION_ID"),
        "parent markers scrubbed"
    );
    assert!(
        !env.contains("CLAUDE_CONFIG_DIR="),
        "no claude home for grok"
    );
    t.wait_for("Allow once", 10);
    t.wait_for("waiting", 10);

    t.cmd(b"y");
    t.wait_for("approvals", 5);
    t.send(b"A");
    let t0 = Instant::now();
    while !home.join("grok-answer.txt").is_file() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let ans = std::fs::read_to_string(home.join("grok-answer.txt")).unwrap_or_default();
    assert!(
        ans.contains("\x1b[B"),
        "always allow = one down from Allow once: {ans:?}"
    );
    t.send(b"\x1b");
    assert_eq!(t.quit(), 0);
}

#[test]
fn assistant_opens_every_html_file_in_one_call() {
    if !have("python3") {
        return;
    }
    let home = make_home("openhtml", "");
    let exe = env!("CARGO_BIN_EXE_godterm");
    let brain = format!(
        r#"#!/usr/bin/env python3
import json, subprocess, sys
HOME = {home:?}
EXE = {exe:?}
def call(name, args):
    lines = json.dumps({{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {{}}}}) + "\n" + json.dumps({{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {{"name": name, "arguments": args}}}}) + "\n"
    out = subprocess.run([EXE, "mcp"], input=lines, capture_output=True, text=True, env={{"GODTERM_HOME": HOME, "PATH": "/usr/bin:/bin"}}).stdout
    for l in out.splitlines():
        v = json.loads(l)
        if v.get("id") == 2:
            r = json.loads(v["result"]["content"][0]["text"])
            with open(HOME + "/calls.jsonl", "a") as f:
                f.write(json.dumps({{"tool": name, "args": args, "reply": r}}) + "\n")
            return r
def emit(say):
    print(json.dumps({{"type": "assistant", "message": {{"content": [{{"type": "tool_use", "id": "x", "name": "mcp__godterm__x", "input": {{}}}}]}}}}))
    print(json.dumps({{"type": "result", "subtype": "success", "result": say, "is_error": False}}), flush=True)
n = 0
for line in sys.stdin:
    n += 1
    if n == 1:
        call("open_tab", {{"account": 1, "name": "site", "count": 3}})
        call("open_tab", {{"account": 2, "name": "site", "count": 2}})
        emit("Opened five tabs.")
    else:
        r = call("list_dir", {{"tab": "all", "match": "*.html"}})
        paths = [t["folder"] + "/" + e.split(" ")[0] for t in r["result"]["tabs"] for e in t.get("entries", []) if e.split(" ")[0].endswith(".html")]
        o = call("open_path", {{"paths": paths}})
        emit(o["result"]["say"])
"#,
        home = home.display().to_string(),
        exe = exe
    );
    write_fakes(
        &home,
        &format!("exec python3 \"{}/brain.py\"\n", home.display()),
    );
    std::fs::write(home.join("brain.py"), brain).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("create five tabs");
    t.wait_for("Opened five tabs.", 40);
    let sites: Vec<PathBuf> = std::fs::read_dir(home.join("tabs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().unwrap().to_string_lossy().starts_with("site"))
        .collect();
    assert_eq!(sites.len(), 5, "{sites:?}");
    for d in &sites {
        std::fs::write(d.join("index.html"), "<p>hi</p>").unwrap();
    }
    t.ask("okay open all the html files");
    t.wait_for("Opened 5 items.", 30);
    let calls = std::fs::read_to_string(home.join("calls.jsonl")).unwrap();
    let opens: Vec<&str> = calls
        .lines()
        .filter(|l| l.contains("\"tool\": \"open_path\""))
        .collect();
    assert_eq!(opens.len(), 1, "one open_path call");
    assert_eq!(
        opens[0].matches("index.html").count() >= 5,
        true,
        "{}",
        opens[0]
    );
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.contains("\"tool\": \"list_dir\""))
            .count(),
        1,
        "one batched list_dir"
    );
    assert!(!t.text().contains("can't"), "no refusal");
    assert_eq!(t.quit(), 0);
}

#[test]
fn assistant_lists_recent_sessions_grouped() {
    if !have("python3") {
        return;
    }
    let home = make_home("lastsess", "");
    let exe = env!("CARGO_BIN_EXE_godterm");

    let p = home.join("main-claude/projects/-Users-x-api");
    std::fs::create_dir_all(p.join("s1/subagents")).unwrap();
    std::fs::write(p.join("s1.jsonl"), "{\"type\":\"user\",\"cwd\":\"/Users/x/api\",\"message\":{\"role\":\"user\",\"content\":\"fix the login flow\"}}\n{\"type\":\"ai-title\",\"aiTitle\":\"Fix login flow\"}\n").unwrap();
    std::fs::write(p.join("s1/subagents/agent-a.jsonl"), "{\"type\":\"user\",\"isSidechain\":true,\"message\":{\"role\":\"user\",\"content\":\"search\"}}\n").unwrap();
    let g = home.join("main-grok/sessions/%2FUsers%2Fx%2Fweb/g1");
    std::fs::create_dir_all(&g).unwrap();
    std::fs::write(g.join("summary.json"), r#"{"info":{"id":"g1","cwd":"/Users/x/web"},"num_chat_messages":3,"generated_title":"Clock page"}"#).unwrap();
    let brain = format!(
        r#"#!/usr/bin/env python3
import json, subprocess, sys, time
HOME = {home:?}
EXE = {exe:?}
def call(name, args):
    lines = json.dumps({{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {{}}}}) + "\n" + json.dumps({{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {{"name": name, "arguments": args}}}}) + "\n"
    out = subprocess.run([EXE, "mcp"], input=lines, capture_output=True, text=True, env={{"GODTERM_HOME": HOME, "PATH": "/usr/bin:/bin"}}).stdout
    for l in out.splitlines():
        v = json.loads(l)
        if v.get("id") == 2:
            return json.loads(v["result"]["content"][0]["text"])
for line in sys.stdin:
    r = call("sessions", {{"limit": 5}})
    for _ in range(20):
        if not r["result"].get("indexing"):
            break
        time.sleep(0.2)
        r = call("sessions", {{"limit": 5}})
    rows = r["result"]["sessions"]
    claude = [x["title"] or x["first_prompt"] for x in rows if x["harness"] == "claude"]
    grok = [x["title"] or x["first_prompt"] for x in rows if x["harness"] == "grok"]
    with open(HOME + "/sessions-reply.json", "w") as f:
        json.dump(r, f)
    say = "Claude: " + ", ".join(claude) + ". Grok: " + ", ".join(grok) + "."
    print(json.dumps({{"type": "assistant", "message": {{"content": [{{"type": "tool_use", "id": "x", "name": "mcp__godterm__sessions", "input": {{}}}}]}}}}))
    print(json.dumps({{"type": "result", "subtype": "success", "result": say, "is_error": False}}), flush=True)
"#,
        home = home.display().to_string(),
        exe = exe
    );
    write_fakes(
        &home,
        &format!("exec python3 \"{}/brain.py\"\n", home.display()),
    );
    std::fs::write(home.join("brain.py"), brain).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("what were my last sessions?");
    t.wait_for("Claude: Fix login flow. Grok: Clock page.", 30);
    let r = std::fs::read_to_string(home.join("sessions-reply.json")).unwrap();
    assert!(!r.contains("agent-a"), "subagents left out: {r}");
    assert_eq!(t.quit(), 0);
}

#[test]
fn grok_footer_follows_its_status_line() {
    let home = make_home("grokline", "");
    let slot = home.join("accounts").join("three");
    std::fs::create_dir_all(&slot).unwrap();
    std::fs::write(slot.join("auth.json"), r#"{"x":{"key":"stub"}}"#).unwrap();
    let grok = home.join("fake-grok.sh");
    std::fs::write(&grok, "#!/bin/sh\necho \" Grok Build\"\necho \" Weekly limit left: 37% · Grok 4.7 (high) · always-approve\"\necho \" Type a message...\"\nexec cat\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&grok, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cfg = home.join("config.toml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text = text.replacen(
        "notifications = false\n",
        &format!("notifications = false\ngrok_bin = \"{}\"\n", grok.display()),
        1,
    );
    text = text.replacen("[voice]", &format!("[[account]]\nname = \"three\"\nlabel = \"Gamma\"\nharness = \"grok\"\ncwd = \"{}\"\n\n[voice]", home.join("work").display()), 1);
    std::fs::write(&cfg, text).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("Gamma", 15);
    let t0 = Instant::now();
    while !t.text().contains("37% left") && t0.elapsed() < Duration::from_secs(15) {
        t.cmd(b"3");
        std::thread::sleep(Duration::from_millis(400));
    }
    let screen = t.text();
    assert!(
        screen
            .lines()
            .any(|l| l.contains("Weekly") && l.contains("37% left")),
        "{screen}"
    );
    assert!(screen.contains("from grok status"), "{screen}");
    assert_eq!(t.quit(), 0);
}

fn have(bin: &str) -> bool {
    std::process::Command::new("which")
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn voice_file_bare_wake_then_command() {
    let model = dirs::home_dir()
        .unwrap()
        .join(".cache/whisper-models/ggml-large-v3-turbo.bin");
    if !(have("say")
        && have("ffmpeg")
        && model.is_file()
        && Path::new("/opt/homebrew/bin/whisper-server").is_file())
    {
        eprintln!("skipping voice test: say, ffmpeg, whisper or model missing");
        return;
    }
    let home = make_home("barewake", "");
    let aiff = home.join("cmd.aiff");
    let wav = home.join("cmd.wav");
    let ok = std::process::Command::new("say")
        .arg("-o")
        .arg(&aiff)
        .arg("[[slnc 1500]] Hey go. [[slnc 1800]] Next tab.")
        .status()
        .unwrap()
        .success()
        && std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-i"])
            .arg(&aiff)
            .args(["-ar", "16000", "-ac", "1"])
            .arg(&wav)
            .status()
            .unwrap()
            .success();
    assert!(ok, "could not make the test recording");
    let t = Tui::start(home, &["--voice", "--voice-file", &wav.to_string_lossy()]);
    t.wait_for("voice", 15);
    t.wait_for("listening…", 60);
    t.wait_for("now on", 60);
    assert_eq!(t.quit(), 0);
}

#[test]
fn voice_file_commands() {
    let model = dirs::home_dir()
        .unwrap()
        .join(".cache/whisper-models/ggml-large-v3-turbo.bin");
    if !(have("say")
        && have("ffmpeg")
        && model.is_file()
        && Path::new("/opt/homebrew/bin/whisper-server").is_file())
    {
        eprintln!("skipping voice test: say, ffmpeg, whisper or model missing");
        return;
    }
    let home = make_home("voice", "");
    let aiff = home.join("cmd.aiff");
    let wav = home.join("cmd.wav");
    let ok = std::process::Command::new("say")
        .arg("-o")
        .arg(&aiff)
        .arg("Hey go, next tab. [[slnc 900]] Hey go, sleep.")
        .status()
        .unwrap()
        .success()
        && std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-i"])
            .arg(&aiff)
            .args(["-ar", "16000", "-ac", "1"])
            .arg(&wav)
            .status()
            .unwrap()
            .success();
    assert!(ok, "could not make the test recording");
    let t = Tui::start(home, &["--voice", "--voice-file", &wav.to_string_lossy()]);
    t.wait_for("voice", 15);

    t.wait_for("now on", 60);
    t.wait_for("asleep until", 30);
    assert_eq!(t.quit(), 0);
}

#[test]
fn move_confirmation_buttons_click() {
    let home = make_home("moveclick", "");
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    for panel in [false, true] {
        if panel {
            t.cmd(b".");
            t.wait_for("Type or speak", 5);
            t.send(b"\x1b");
        }

        t.cmd(b"m");
        t.wait_for("keep the same folder", 5);
        t.click(" Cancel ");
        t.wait_gone("keep the same folder", 5);

        t.cmd(b"m");
        t.wait_for("keep the same folder", 5);
        let (_, row) = t.find(" Cancel ").unwrap();
        let (c, r) = t
            .find(" Move ")
            .filter(|(_, r)| *r == row)
            .expect("the Move button beside Cancel");
        t.click_at(c + 3, r);
        t.wait_gone("keep the same folder", 5);
        if panel {
            t.wait_gone("Press Enter to start claude", 10);
        } else {
            t.wait_for("tabs 2 ●", 10);
        }
    }
}

#[test]
fn voice_onboarding_welcome_steps_and_skip() {
    let home = make_home("onboarding", "");
    let cfg = home.join("config.toml");
    let t = std::fs::read_to_string(&cfg).unwrap().replace(
        "[voice]\n",
        "[voice]\nengine = \"whisper\"\nffmpeg = \"/nonexistent/ffmpeg\"\nwhisper_server = \"/nonexistent/whisper-server\"\nwhisper_cli = \"/nonexistent/whisper-cli\"\nmodel = \"/nonexistent/ggml.bin\"\nespeak = \"/nonexistent/espeak-ng\"\nkokoro_model = \"/nonexistent/kokoro.onnx\"\nkokoro_voices = \"/nonexistent/voices.bin\"\n",
    );
    std::fs::write(&cfg, t).unwrap();

    let mut t = Tui::start(home.clone(), &["--onboarding"]);
    t.wait_for("Run all your coding agent sessions just by talking.", 15);
    t.wait_for("Turn voice on", 5);
    assert!(!t.text().contains("Alpha"), "nothing of the UI shows yet");
    t.send(b"\x1b");
    t.wait_gone("Turn voice on", 5);
    t.wait_for("Alpha", 10);
    let marker = std::fs::read_to_string(home.join("voice_onboarding")).unwrap();
    assert!(marker.starts_with("skipped"), "{marker}");
    t.quit_keep();

    let mut t = Tui::start(home.clone(), &["--onboarding"]);
    t.wait_for("Turn voice on", 15);
    t.send(b"\r");
    t.wait_for("Voice needs a few pieces", 5);
    t.wait_for("Continue without voice", 5);
    t.send(b"c");
    t.wait_for("voice tour · step 1 of 5", 5);
    t.wait_for("▸ Ask how something works", 5);

    t.ask("skip");
    t.wait_for("voice tour · step 2 of 5", 5);
    t.cmd(b"J");
    t.wait_for("voice tour · step 3 of 5", 5);
    t.wait_for("– Log in a Claude session", 5);
    t.ask("stop onboarding");
    t.wait_gone("voice tour", 5);
    let marker = std::fs::read_to_string(home.join("voice_onboarding")).unwrap();
    assert!(marker.starts_with("stopped"), "{marker}");
    assert_eq!(t.quit(), 0);
}

#[test]
fn damaged_state_files_do_not_stop_a_start() {
    let home = make_home("damaged", "");
    for (f, body) in [
        ("state.json", "{\"panes\": [{\"tabs\": [{\"name\""),
        ("jobs.json", ""),
        ("archive.json", "\u{0}\u{0}\u{0}"),
        ("projects.json", "[{\"path\":"),
        ("session-index.json", "{"),
        ("project-index.json", "nul"),
        ("tab-history.jsonl", "{\"a\":1}\n{\"b\":\n\u{0}\n"),
        ("last_exit.json", "{\"clean\":"),
        ("control.json", "not json"),
    ] {
        std::fs::write(home.join(f), body).unwrap();
    }
    let backups = |name: &str| {
        std::fs::read_dir(&home)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{name}.corrupt-"))
            })
            .count()
    };
    let t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.wait_for("Alpha", 5);
    let log = std::fs::read_to_string(home.join("godterm.log")).unwrap_or_default();
    for f in ["state.json", "jobs.json", "archive.json"] {
        assert_eq!(backups(f), 1, "{f} moved aside; log:\n{log}");
        assert!(log.contains(&format!("{f} was damaged")), "{log}");
    }
    assert!(!log.contains("panic"), "{log}");
    t.quit_keep();
    let log = std::fs::read_to_string(home.join("godterm.log")).unwrap_or_default();
    assert!(!log.contains("panic"), "{log}");

    let st = std::fs::read_to_string(home.join("state.json")).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&st).is_ok(),
        "{st}"
    );

    std::fs::write(home.join("config.toml"), "claude_bin = \"/x\n[[account]\n").unwrap();
    let mut t = Tui::start(home.clone(), &[]);

    t.wait_for("No accounts yet", 15);
    assert_eq!(backups("config.toml"), 1);
    let log = std::fs::read_to_string(home.join("godterm.log")).unwrap_or_default();
    assert!(log.contains("config.toml was damaged"), "{log}");
    assert!(!log.contains("panic"), "{log}");
    let _ = t.child.kill();
    let _ = std::fs::remove_dir_all(&home);
}
