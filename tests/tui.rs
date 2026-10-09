//! End to end tests: run the real godterm binary in a PTY against a fake
//! claude, send keys, and read the screen through a vt100 parser. No tmux,
//! no network (usage comes from a fixture), no real claude.
//! Unix only: the fake claude is a shell script.
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

/// A throwaway GODTERM_HOME with two logged in (fake) accounts whose
/// "claude" is a shell script that echoes its input.
fn make_home(tag: &str, extra: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("godterm-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let fake = home.join("fake-claude.sh");
    std::fs::write(
        &fake,
        "#!/bin/sh\nenv > \"$PWD/.tab-env.txt\"\necho \"fake claude in $(basename \"$PWD\") args:$*\"\necho \"? for shortcuts\"\nexec cat\n",
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
    // New tab folders go in a workspace (GodTerm's home itself is private).
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
    cfg.push_str(extra);
    std::fs::write(home.join("config.toml"), cfg).unwrap();
    // The first launch tour has its own test; keep it out of the way here.
    std::fs::write(home.join("tour_done"), "1").unwrap();
    home
}

/// Fails when a test would point the app at the user's real data.
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

    /// `rw`: other local clients (the test's stub brains and scripts) may
    /// change things (GODTERM_CONTROL_RW=1); otherwise they are read-only.
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
        // Never make a sound on the user's speakers.
        cmd.env("GODTERM_NO_AUDIO", "1");
        // Never open the user's microphone either.
        cmd.env("GODTERM_NO_MIC", "1");
        // Never open files or browsers on the user's screen.
        cmd.env("GODTERM_NO_OPEN", "1");
        // Never the start up splash (the fresh homes would show it).
        cmd.env("GODTERM_NO_SPLASH", "1");
        // The main installs are fixtures here, never the user's own.
        cmd.env("GODTERM_MAIN_DIR", home.join("main-claude"));
        cmd.env("GODTERM_MAIN_GROK_DIR", home.join("main-grok"));
        cmd.env("TERM", "xterm-256color");
        // As if started from inside a grok session: its markers must not
        // reach the tabs.
        cmd.env("GROK_AGENT_ID", "parent-agent");
        cmd.env("GROK_SESSION_ID", "parent-session");
        // Whatever the shell running the tests has, the app under test
        // never sees the user's own config dirs.
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
        // Keep the master alive for the life of the reader.
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
                        // Answer the queries a real terminal would.
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

    /// Type to the assistant and send: Enter only once the text shows in
    /// its input box, so it is read as a key of its own, never as part of
    /// a burst (a paste, where Enter is a new line).
    fn ask(&mut self, text: &str) {
        self.send(text.as_bytes());
        let shown: String = text.chars().take(30).collect();
        self.wait_for(&format!("› {shown}"), 10);
        self.send(b"\r");
    }

    /// Where `needle` is on screen (0-based column, row), the last match.
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

    /// A left click (SGR mouse, as a terminal sends it) at a cell.
    fn click_at(&mut self, col: u16, row: u16) {
        self.send(format!("\x1b[<0;{};{}M", col + 1, row + 1).as_bytes());
        self.send(format!("\x1b[<0;{};{}m", col + 1, row + 1).as_bytes());
    }

    /// Click the middle of a label on screen.
    fn click(&mut self, label: &str) {
        let (c, r) = self
            .find(label)
            .unwrap_or_else(|| panic!("{label:?} not on screen:\n{}", self.text()));
        self.click_at(c + label.chars().count() as u16 / 2, r);
    }

    /// Ctrl-a then a key.
    fn cmd(&mut self, key: &[u8]) {
        self.send(b"\x01");
        self.send(key);
    }

    /// Wait until a file has `needle` (what a stub wrote: no screen
    /// wrapping, no timing).
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

    /// Quit and keep the home (to start again on it).
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
    // Both logged in panes start the fake claude, in bypass mode by default.
    t.wait_for("fake claude in work", 15);
    t.wait_for("args:--dangerously-skip-permissions", 5);
    t.wait_for("Alpha", 5);
    t.wait_for("Bravo", 5);

    // New tab via the picker, typed path.
    t.cmd(b"t");
    t.wait_for("New tab for Alpha", 5);
    t.wait_for("Will create:", 5);
    // Open existing: clear the prefilled base, type the folder, Enter.
    t.send(b"\x0f");
    t.send(b"\x15");
    t.send(proj.to_string_lossy().as_bytes());
    t.send(b"\r");
    t.wait_for("tabs 2", 5);
    t.wait_for("fake claude in projx", 5);
    t.wait_for("Alpha [2/2]", 5);

    // Switch back and forth.
    t.cmd(b"p");
    t.wait_for("Alpha [1/2]", 5);
    t.cmd(b"n");
    t.wait_for("Alpha [2/2]", 5);

    // Overview lists every tab.
    t.cmd(b"o");
    t.wait_for("Overview: every account and tab", 5);
    t.wait_for("projx", 2);
    t.send(b"\x1b");
    t.wait_gone("Overview: every account and tab", 5);

    // Palette action: close tab. It is idle, so no confirmation by
    // default (confirm_close = "busy"), and an Undo toast offers it back.
    t.cmd(b":");
    t.wait_for("command palette", 5);
    t.send(b"close tab");
    t.send(b"\r");
    t.wait_for("tabs 1", 5);
    t.wait_for("Undo", 5);
    t.wait_gone("[2/2]", 5);

    // Typed text that is no action's name is a request for the assistant.
    t.cmd(b":");
    t.send(b"in bravo type hello from the palette");
    t.send(b"\r");
    t.wait_for("you in bravo type hello from the palette", 5);
    t.send(b"\x1b");

    // Help overlay lists keys and voice commands.
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
    // Open existing: clear the prefilled base, type the folder, Enter.
    t.send(b"\x0f");
    t.send(b"\x15");
    t.send(proj.to_string_lossy().as_bytes());
    t.send(b"\r");
    t.wait_for("tabs 2", 5);
    // Quit without removing the home this time.
    t.cmd(b"Q");
    let t0 = Instant::now();
    while t.child.try_wait().ok().flatten().is_none() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let state = std::fs::read_to_string(home.join("state.json")).unwrap();
    assert!(state.contains("projx"));

    let t2 = Tui::start(home, &[]);
    t2.wait_for("tabs 2", 15);
    // The restored active tab starts on its own.
    t2.wait_for("fake claude in projx", 10);
    assert_eq!(t2.quit(), 0);
}

/// Eager restore starts every saved tab, not only the visible ones, with
/// --resume when the session id is known, and says how many it restored.
/// After a restart the assistant still knows the last conversation: the
/// new brain's first message carries the memory, and the answer uses it.
#[test]
fn assistant_remembers_across_restarts() {
    let home = make_home("memory", "");
    write_fakes(
        &home,
        &format!(
            r#"while read -r line; do
  echo "$line" >> "{home}/brain-in.txt"
  case "$line" in
    *"Recent conversation memory"*botmesh*) say="Earlier we were looking at the lol/botmesh sessions; nothing was running there." ;;
    *botmesh*) say="Two sessions in lol/botmesh." ;;
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
    t.ask("show me the lol/botmesh sessions");
    t.wait_for("Two sessions in lol/botmesh.", 20);
    t.quit_keep();
    // GodTerm starts again: a new brain.
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("what was the status of that thing, I forgot to ask you");
    t.wait_for("Earlier we were looking at the lol/botmesh", 20);
    let sent = std::fs::read_to_string(home.join("brain-in.txt")).unwrap();
    assert!(
        sent.contains("Recent conversation memory")
            && sent.contains("show me the lol/botmesh sessions"),
        "{sent}"
    );
    assert!(!t.text().contains("I don't have context"));
}

/// The user's 19:38 conversation: "lol/bot mesh", then "lol forward
/// slash podmesh", then "the lol slash pod mesh": the brain passes what
/// it heard, and the sessions tool still finds ~/lol/botmesh (it is
/// among the projects) and says it took the name as lol/botmesh.
#[test]
fn misheard_project_resolves_to_botmesh() {
    if !have("python3") {
        return;
    }
    let home = make_home("botmesh", "");
    // A session in lol/botmesh in the (fixture) main ~/.claude.
    let proj = home.join("main-claude/projects/-Users-x-lol-botmesh");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("1111aaaa-0000-4000-8000-000000000001.jsonl"),
        "{\"type\":\"ai-title\",\"aiTitle\":\"Bot generator changes\"}\n{\"type\":\"user\",\"cwd\":\"/Users/x/lol/botmesh\",\"entrypoint\":\"cli\",\"message\":{\"role\":\"user\",\"content\":\"bot generator changes\"}}\n",
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
heard = ["lol/bot mesh", "lol/podmesh", "lol/podmesh"]
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
        "lol/bot mesh directory",
        "yeah as the folder is lol forward slash podmesh",
        "yeah no the lol slash pod mesh, that's the one",
    ] {
        t.ask(said);
        std::thread::sleep(Duration::from_millis(300));
    }
    t.wait_for("lol/botmesh", 40);
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
            .contains("lol/botmesh"),
        "{calls}"
    );
    assert!(!calls.contains("came back empty"), "{calls}");
}

/// "Hold on for ten seconds": the brain reads the duration and calls
/// pause_listening(10); the status bar counts down and it resumes alone.
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
    // It resumes by itself when the time is up.
    t.wait_gone("PAUSED 0:", 15);
    assert_eq!(t.quit(), 0);
}

/// QA-26: without --control-rw, other local clients can only look, and
/// tabs do not get GodTerm's home or token in their environment.
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
    // The tab's own environment: no GodTerm home or token.
    let env = std::fs::read_to_string(home.join("work").join(".tab-env.txt")).unwrap_or_default();
    assert!(env.contains("PATH=") && !env.contains("GODTERM_"), "{env}");
}

/// QA-25: Ctrl-C during the first run setup leaves no config behind, so
/// setup is offered again.
#[test]
fn interrupted_setup_writes_no_config() {
    let home = std::env::temp_dir().join(format!("godterm-it-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("How many Claude accounts", 10);
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

/// QA-23: one GodTerm per home. A second start explains and exits; the
/// first keeps its control socket.
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
    // The hidden tab started too: every tab in the sidebars reads ready
    // (a lazy tab would stay idle until shown).
    let t0 = Instant::now();
    // Tab rows read "N ○ name" once ready (idle at the prompt: a dim ○;
    // starting is ◌).
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
    assert!(
        screen.contains("args:--dangerously-skip-permissions --resume sess-456"),
        "{screen}"
    );
    // The hidden tab kept its name and resumed its session.
    let mut t = t;
    t.cmd(b"1");
    t.cmd(b"n");
    t.wait_for("api work", 5);
    t.wait_for("--resume sess-123", 5);
    assert_eq!(t.quit(), 0);
}

/// Moving a live tab copies its session to the other account, resumes it
/// there, closes the original once the new one is up, and can be undone.
#[test]
fn move_tab_to_another_account() {
    let home = make_home("movetab", "");
    let work = home.join("work");
    let id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    // claude's project dir name for the cwd.
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
    // Ctrl-a m: the picker lists Bravo with its usage, marked most left.
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
    // Pane 2 now resumes the same conversation; pane 1's tab closed.
    assert!(
        screen.matches("--resume aaaaaaaa").count() == 1,
        "only the moved tab: {screen}"
    );
    assert!(screen.contains("UNDO MOVE"), "undo chip: {screen}");
    // Undo moves it back.
    t.cmd(b"U");
    t.wait_for("Moved 'api' to Alpha", 15);
    assert_eq!(t.quit(), 0);
}

/// Loops are found in a running tab's transcript; Stop types a request
/// into the idle tab and the transcript confirms it.
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
    t.wait_for("--resume cccccccc", 15);
    t.wait_for("⟳1", 15);
    t.wait_for("⟳1", 5);
    t.cmd(b"@");
    t.wait_for("e0e3839a", 5);
    t.wait_for("every 15 minutes", 5);
    t.send(b"s");
    // Back on the grid the stub echoes what was typed into it.
    t.send(b"\x1b");
    t.wait_for("Cancel the scheduled job e0e3839a", 10);
    // claude would now run CronDelete; the transcript says so.
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

/// The assistant end to end with a stub "claude -p": it reads the turn,
/// calls `godterm mcp` (socket, token, control API) for list_tabs, and
/// streams a reply that shows in the panel.
#[test]
fn assistant_loop_with_a_stub_brain() {
    let home = make_home("assist", "");
    let exe = env!("CARGO_BIN_EXE_godterm");
    // Tabs get the normal fake; "-p" (the brain) gets this.
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
    // Ctrl-a . opens the panel; type a question and send it.
    t.cmd(b".");
    t.wait_for("Type or speak", 5);
    t.ask("what is going on");
    t.wait_for("Both are ready.", 20);
    t.wait_for("checked get state", 10);
    t.wait_for("Assistant: Claude on", 5);
    // The stub really reached the control API through godterm mcp.
    let out = std::fs::read_to_string(home.join("mcp-out.txt")).unwrap();
    assert!(
        out.contains("\"serverInfo\"") && out.contains("\\\"tabs\\\""),
        "{out}"
    );
    let args = std::fs::read_to_string(home.join("brain-args.txt")).unwrap();
    for want in [
        "--input-format stream-json",
        "--output-format stream-json",
        "--strict-mcp-config",
        "mcp__godterm__answer_prompt",
        "--model claude-haiku-4-5",
        "--effort low",
        "--no-session-persistence",
        // The brain does what is asked instead of offering to.
        "Do, don't offer",
        "Never answer it with \"Want me to do that?\"",
    ] {
        assert!(args.contains(want), "{want} missing in {args}");
    }
    assert_eq!(t.quit(), 0);
}

///// A stand in for claude's input box, as far as delivery goes: it turns on
/// bracketed paste, keeps pasted text (and LF) as newlines, treats a CR
/// that arrives in the same burst as a paste as a newline too (what makes
/// a prompt sit in the box), and submits only on a CR of its own. A
/// submitted prompt shows a spinner for a moment, as claude does.
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

/// The tab claude is the stub above; `-p` (the brain) runs `brain`.
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

/// A multi step request: the stub brain opens a tab with a prompt in one
/// call. The prompt reaches the new tab once its claude is ready, in a new
/// folder named after the task, and is really submitted (a paste, then a
/// separate Enter), which the tool reports as delivered.
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
    // Generous waits (conditions, not sleeps): CI runners are slow.
    t.wait_for("fake claude in work", 60);
    t.cmd(b".");
    t.wait_for("Type or speak", 20);
    t.ask("start a new tab on account two and create a simple calculator");
    t.wait_for("asked it to build", 60);
    assert!(home.join("tabs/calculator").is_dir(), "task folder");
    t.send(b"\x01."); // close the panel to see the tab
    t.wait_gone("assistant · Claude", 30);
    t.wait_for("fake claude in calculator", 30);
    // One prompt, both lines, submitted once: what the stub received (its
    // screen may have wrapped it while the docked panel made it narrow).
    let submitted = home.join("tabs/calculator/submitted.txt");
    t.wait_file(
        &submitted,
        "Create a simple calculator app.\nKeep it tiny.\n",
        30,
    );
    // On screen too, once the closed panel gave the pane its width back
    // (the stub redraws on the resize).
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
    // The user's own Enter still submits in that tab afterwards: once
    // the text shows in its input, Enter of its own.
    t.send(b"next step");
    t.wait_for("> next step", 10);
    t.send(b"\r");
    t.wait_file(&submitted, "next step\n", 10);
    assert_eq!(t.quit(), 0);
}

/// The stub itself: a paste and its Enter in one burst (how godterm used
/// to send prompts) leave the prompt in the box; a CR of its own submits.
/// This is the failure the assistant's delivery avoids.
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

/// "close all tabs in all accounts": one question for the whole set; a
/// question back ("Do you close them?") is not a yes; one clear yes closes
/// every tab, one summary.
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
    // A second tab on account one: three tabs across two accounts.
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
    // A question back is not a yes: nothing closes, it asks again.
    t.ask("Do you close them?");
    t.wait_for("Want me to close all three? Say yes.", 20);
    assert!(std::fs::read_to_string(home.join("done.txt"))
        .unwrap()
        .contains("not_confirmed"));
    t.ask("yes");
    t.wait_for("Closed all three tabs.", 20);
    let done = std::fs::read_to_string(home.join("done.txt")).unwrap();
    assert!(done.contains("\\\"closed\\\":3"), "{done}");
    // One question, one yes: the reply log asked once.
    let screen = t.text();
    assert_eq!(
        screen.matches("Close 3 tabs across 2 accounts?").count(),
        1,
        "{screen}"
    );
    assert_eq!(t.quit(), 0);
}

/// A follow up goes to the tab used last ("make it blue" after "create a
/// calculator"), addressed as "last", not by guessing a number.
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
    // It went to the last tab: delivered at once, or (a slow machine,
    // where the tab is still busy with its first prompt) queued for it.
    // Either way the tab itself is the proof, below.
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

/// The user's 14:06 sequence: open five tabs, move three to another
/// account, one prompt to all five with a confirmation, "did you do it"
/// in between, then "yes": every tab gets the prompt, verified. On the
/// way the brain tries to redeem a token in the turn that issued it, and
/// is refused.
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
        ("oh yeah I said yes", "Claude started on it in 5 tabs."),
    ] {
        t.ask(said);
        t.wait_for(want, 40);
    }
    let calls = std::fs::read_to_string(home.join("calls.jsonl")).unwrap();
    assert!(
        calls.contains("same turn"),
        "self confirmation refused: {calls}"
    );
    // Every one of the five folders got the prompt, once.
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

/// A grok account: the tab runs the (stub) grok with its own GROK_HOME,
/// --leader-socket and --always-approve, none of a parent grok's markers
/// and no CLAUDE_CONFIG_DIR; the header shows the grok badge; the stub's
/// approval prompt is answered with grok's own options. No real grok, no
/// login, no prompt sent anywhere.
#[test]
fn grok_account_runs_isolated() {
    let home = make_home("grok", "");
    let slot = home.join("accounts").join("three");
    std::fs::create_dir_all(&slot).unwrap();
    // A stand in login file for the stub only (no real grok runs here).
    std::fs::write(
        slot.join("auth.json"),
        r#"{"user":{"email":"g@example.com"},"key":"stub"}"#,
    )
    .unwrap();
    let grok = home.join("fake-grok.sh");
    std::fs::write(
        &grok,
        format!(
            "#!/bin/sh\nenv > \"{home}/grok-env.txt\"\necho \"$*\" > \"{home}/grok-args.txt\"\necho \" Grok Build\"\necho \"\"\necho \" Allow this command?\"\necho \" › Allow once\"\necho \"   Always allow this command\"\necho \"   Always allow on all sessions\"\necho \"   Reject\"\nIFS= read -r k\necho \"answered: $k\" > \"{home}/grok-answer.txt\"\necho \" Type a message...\"\nexec cat\n",
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
    // Show the grok pane and approve its prompt from the approval strip.
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
    // Ctrl-a y lists it; Enter... simpler: the always key from the strip.
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

/// "open all the html files" across five tabs: one batched list_dir, one
/// open_path with all five files, a short factual reply. Nothing really
/// opens (GODTERM_NO_OPEN).
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

/// "what were my last sessions?": one sessions call, a grouped factual
/// answer (Claude first, then Grok), subagents left out.
#[test]
fn assistant_lists_recent_sessions_grouped() {
    if !have("python3") {
        return;
    }
    let home = make_home("lastsess", "");
    let exe = env!("CARGO_BIN_EXE_godterm");
    // A main ~/.claude fixture with a session and its subagent, and a main grok one.
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

/// A grok account whose usage API is unavailable: the footer mirrors
/// grok's own status line ("Weekly limit left: 37%") within a refresh.
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

/// The wake word alone through the whole pipeline (say, whisper, the
/// app): it answers and opens the follow up window, and the command said
/// after a pause runs without the wake word. Runs only where `say`,
/// ffmpeg, whisper and the default model exist; nothing is played aloud.
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

/// Runs only where `say`, ffmpeg, whisper and the default model exist.
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
    // Instant commands, straight from the recording.
    t.wait_for("now on", 60);
    t.wait_for("asleep until", 30);
    assert_eq!(t.quit(), 0);
}

/// The move picker's buttons work with a real mouse (SGR clicks through
/// the terminal), with the assistant panel closed and open.
#[test]
fn move_confirmation_buttons_click() {
    let home = make_home("moveclick", "");
    let mut t = Tui::start(home.clone(), &[]);
    t.wait_for("fake claude in work", 15);
    for panel in [false, true] {
        if panel {
            t.cmd(b".");
            t.wait_for("Type or speak", 5);
            t.send(b"\x1b"); // the keys back to the grid
        }
        // Cancel closes it, nothing moves.
        t.cmd(b"m");
        t.wait_for("keep the same folder", 5);
        t.click(" Cancel ");
        t.wait_gone("keep the same folder", 5);
        // Move moves it.
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
            // Back onto Alpha: its idle tab runs the session now.
            t.wait_gone("Press Enter to start claude", 10);
        } else {
            t.wait_for("tabs 2 ●", 10);
        }
    }
}
