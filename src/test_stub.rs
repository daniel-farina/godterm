//! A stand in "claude" for tests that runs on every OS: a tiny Rust
//! program compiled once per test run (instead of a `#!/bin/sh` script),
//! copied next to a `.cfg` file that says what this copy does:
//!   args_to=PATH      append the arguments as one line
//!   home_env_to=PATH  record only home/session marker vars for launch tests
//!   stdin_to=PATH     copy stdin there as it arrives (stdin_append=1 appends)
//!   sleep_s=N         then stay alive N seconds
//!   exit_now=1        exit at once (like /usr/bin/true)
//!   child_sleep_s=N   first start a child (another copy) that sleeps N s
//!   print_n=N         print the numbers 1 to N
//!   ready_to=PATH     write that file once started (after the child)
//!   fake_admin=1      act out `mcp add|list|remove|login` and `plugin
//!                     install|uninstall|enable|disable|marketplace add`, with
//!                     state in $CLAUDE_CONFIG_DIR (or $GROK_HOME)/fake-admin.txt;
//!                     remote servers need a sign in (login_fail=1 fails it);
//!                     `mcp login` "opens the browser" (a line in
//!                     browser-opens.txt), prints the URL with --no-browser,
//!                     or says it could not open one (browser_fails=1)
//!   print_file=PATH   print that file (a canned output), then stderr_line
//!                     to stderr and exit with exit_code (after args_to)
//!   fake_login=1      show a login screen with a wrapped OAuth URL and a code
//!                     prompt; login_code=CODE is the code that works
//!   rc_answers=A,B..  answer remote_control requests in turn: ok (a
//!                     session_url, then bridge_state connected), fail
//!                     (initialization failed) or silent; counted across
//!                     runs of this copy
//!   resume_fail=1     a --resume exits at once with claude's "No
//!                     conversation found" error
//!   resume_bg=SHORT   a plain --resume prints claude's "That session is
//!                     running in the background (SHORT)" and exits 1;
//!                     `stop` makes later resumes work; attach and
//!                     --fork-session run normally
//!   env_to=PATH       append "cwd|CLAUDE_CONFIG_DIR" as one line
//!   bg_print=TEXT    `--bg` prints TEXT and returns at once (bg_fail=MSG: fails)
//!   do=STEP           (repeatable, run in order after the options above,
//!                     then exit) a scripted program in place of a /bin/sh -c
//!                     one-liner, the same on every OS:
//!                       say:TEXT        print TEXT (\\n \\r \\e \\\\ escapes; {x} is
//!                                       the last line `read:x` got)
//!                       read:x          read a line of stdin into x
//!                       sleep:SECS      sleep (SECS may be 0.3)
//!                       lines:N:FMT     print FMT + newline for i in 0..N
//!                                       ({i} or {i5}: zero padded to 5)
//!                       bytes:N:C       print N bytes of the character C
//!                       yes:N           print `y` lines up to N bytes
//!                       hexdump:N       read N raw bytes, print them as
//!                                       ` xx` (16 a line), like od -An -tx1
//!                       echo_lines:PRE  print PRE + each stdin line to EOF
//!                       cat             copy stdin to stdout to EOF
//!                       noecho / raw    like stty -echo / stty raw -echo
//! Without stdin_to it reads stdin to the end and drops it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const SOURCE: &str = r#"
use std::io::{Read, Write};
// Windows: take Ctrl-C the default way (exit), even when the parent left
// "ignore Ctrl-C" to inherit, as an interactive claude does.
#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(handler: usize, add: i32) -> i32;
    fn GetStdHandle(n: u32) -> isize;
    fn GetConsoleMode(h: isize, m: *mut u32) -> i32;
    fn SetConsoleMode(h: isize, m: u32) -> i32;
}
// Unix: take SIGINT the default way (exit) even when the parent left it
// ignored (a shell's background job ignores SIGINT, and that is inherited).
#[cfg(unix)]
extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}
// Windows: on a console (a tab's ConPTY) read keys as bytes like an
// interactive claude does: not in line mode, where Escape clears the line
// and never arrives and Enter comes as CR LF.
static VT_IN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn main() {
    #[cfg(windows)]
    unsafe {
        SetConsoleCtrlHandler(0, 0);
        let h = GetStdHandle(-10i32 as u32);
        let mut m = 0u32;
        // ENABLE_PROCESSED_INPUT 1 (Ctrl-C still ends us), ENABLE_VIRTUAL_TERMINAL_INPUT 0x200.
        if GetConsoleMode(h, &mut m) != 0 && SetConsoleMode(h, 0x201) != 0 {
            VT_IN.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[cfg(unix)]
    unsafe {
        signal(2, 0);
    }
    let exe = std::env::current_exe().unwrap();
    let cfg = std::fs::read_to_string(exe.with_extension("cfg")).unwrap_or_default();
    let get = |k: &str| cfg.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(|v| v.to_string()));
    if get("exit_now").is_some() {
        return;
    }
    // A child of ours (a background task): it only sleeps.
    if let Ok(s) = std::env::var("GODTERM_STUB_CHILD") {
        std::thread::sleep(std::time::Duration::from_secs(s.parse().unwrap_or(30)));
        return;
    }
    let child = get("child_sleep_s").map(|s| {
        std::process::Command::new(&exe)
            .env("GODTERM_STUB_CHILD", s)
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap()
    });
    let _ = &child;
    // Started (and its child too): tests wait for this.
    if let Some(p) = get("ready_to") {
        let _ = std::fs::write(p, std::process::id().to_string());
    }
    if let Some(n) = get("print_n").and_then(|n| n.parse::<u64>().ok()) {
        let mut o = std::io::stdout().lock();
        for i in 1..=n {
            if writeln!(o, "{i}").is_err() {
                break;
            }
        }
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(p) = get("home_env_to") {
        let keys = ["CODEX_HOME", "CODEX_THREAD_ID", "CODEX_INTERNAL_ORIGINATOR_OVERRIDE", "CLAUDE_CONFIG_DIR", "GROK_HOME", "HOME"];
        let values: Vec<String> = keys.iter().filter_map(|k| std::env::var(k).ok().map(|v| format!("{k}={v}"))).collect();
        std::fs::write(p, values.join("\n")).unwrap();
    }
    if get("fake_admin").is_some() && matches!(args.first().map(String::as_str), Some("mcp") | Some("plugin")) {
        std::process::exit(fake_admin(&args, &get));
    }
    if get("fake_login").is_some() {
        fake_login(&get);
        return;
    }
    if let Some(p) = get("args_to") {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap();
        // A prompt given as a file (Windows) is recorded as if it were inline.
        let shown: Vec<String> = args
            .iter()
            .enumerate()
            .map(|(i, a)| match i.checked_sub(1).map(|j| args[j].as_str()) {
                Some("--append-system-prompt-file") => std::fs::read_to_string(a).unwrap_or_else(|_| a.clone()),
                _ => a.clone(),
            })
            .collect();
        let _ = writeln!(f, "{}", shown.join(" ").replace("--append-system-prompt-file", "--append-system-prompt"));
    }
    if let Some(p) = get("env_to") {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap();
        let _ = writeln!(f, "{}|{}", std::env::current_dir().unwrap().display(), std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_default());
    }
    // `claude --bg ...`: prints a job id (bg_print=TEXT) and returns at
    // once, or fails (bg_fail=MESSAGE).
    if args.iter().any(|a| a == "--bg") && (get("bg_print").is_some() || get("bg_fail").is_some()) {
        if let Some(m) = get("bg_fail") {
            eprintln!("{m}");
            std::process::exit(1);
        }
        println!("{}", get("bg_print").unwrap_or_default());
        return;
    }
    if let Some(p) = get("print_file") {
        print!("{}", std::fs::read_to_string(p).unwrap_or_default());
        let _ = std::io::stdout().flush();
        if let Some(e) = get("stderr_line") {
            eprintln!("{e}");
        }
        std::process::exit(get("exit_code").and_then(|c| c.parse().ok()).unwrap_or(0));
    }
    // A resume of a session it does not have fails like claude's.
    if get("resume_fail").is_some() {
        if let Some(i) = args.iter().position(|a| a == "--resume") {
            eprintln!("No conversation found with session ID: {}", args.get(i + 1).map(String::as_str).unwrap_or(""));
            std::process::exit(1);
        }
    }
    // A background run holds the session: a plain resume is refused like
    // claude 2.1.29x refuses it, until `stop` ran; attach and
    // --fork-session go ahead.
    if let Some(short) = get("resume_bg") {
        let stopped = exe.with_extension("stopped");
        if args.first().map(String::as_str) == Some("stop") {
            let _ = std::fs::write(&stopped, "1");
            return;
        }
        let plain = args.first().map(String::as_str) != Some("attach")
            && args.iter().any(|a| a == "--resume")
            && !args.iter().any(|a| a == "--fork-session");
        if plain && !stopped.exists() {
            print!("That session is running in the background ({short}). Run `claude\r\nattach {short}` to open it, or `claude stop {short}` first to\r\nresume it here. Add --fork-session to branch off a copy instead.\r\n");
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::process::exit(1);
        }
    }
    let steps: Vec<String> = cfg.lines().filter_map(|l| l.strip_prefix("do=").map(str::to_string)).collect();
    if !steps.is_empty() {
        run_steps(&steps);
        return;
    }
    let rc: Vec<String> = get("rc_answers").map(|s| s.split(',').map(|x| x.to_string()).collect()).unwrap_or_default();
    let mut line = Vec::new();
    let mut out: Option<std::fs::File> = get("stdin_to").map(|p| {
        let append = get("stdin_append").as_deref() == Some("1");
        std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(p).unwrap()
    });
    let mut buf = [0u8; 4096];
    let mut input = std::io::stdin();
    loop {
        match input.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                // What a unix tty's line discipline does with Enter (CR to LF).
                if VT_IN.load(std::sync::atomic::Ordering::SeqCst) {
                    buf[..n].iter_mut().filter(|b| **b == b'\r').for_each(|b| *b = b'\n');
                }
                if let Some(f) = out.as_mut() {
                    let _ = f.write_all(&buf[..n]);
                    let _ = f.flush();
                }
                if !rc.is_empty() {
                    for &b in &buf[..n] {
                        if b != b'\n' {
                            line.push(b);
                            continue;
                        }
                        let l = String::from_utf8_lossy(&line).to_string();
                        line.clear();
                        if l.contains("\"subtype\":\"remote_control\"") && l.contains("\"enabled\":true") {
                            remote_answer(&exe, &rc, &l);
                        }
                    }
                }
            }
        }
    }
    if let Some(s) = get("sleep_s").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(s));
    }
}

// Answers a remote_control request with the next of rc_answers (ok, fail,
// silent; counted across runs in <exe>.rc_n): ok gives a session_url
// and bridge_state connected.
fn unescape(s: &str) -> String {
    let mut o = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('n') => o.push('\n'),
            Some('r') => o.push('\r'),
            Some('e') => o.push('\x1b'),
            Some(x) => o.push(x),
            None => o.push('\\'),
        }
    }
    o
}

/// stty -echo (raw=false) or stty raw -echo (raw=true) on our stdin.
fn tty_mode(raw: bool) {
    #[cfg(windows)]
    unsafe {
        let h = GetStdHandle(-10i32 as u32);
        let mut m = 0u32;
        if GetConsoleMode(h, &mut m) != 0 {
            // ENABLE_ECHO_INPUT 4, ENABLE_VIRTUAL_TERMINAL_INPUT 0x200.
            SetConsoleMode(h, if raw { 0x200 } else { m & !4 });
        }
    }
    #[cfg(unix)]
    {
        let args: &[&str] = if raw { &["raw", "-echo"] } else { &["-echo"] };
        let _ = std::process::Command::new("stty").args(args).status();
    }
}

fn read_line(input: &mut impl Read) -> Option<String> {
    let mut b = Vec::new();
    let mut one = [0u8; 1];
    loop {
        match input.read(&mut one) {
            Ok(0) | Err(_) => return if b.is_empty() { None } else { Some(String::from_utf8_lossy(&b).into_owned()) },
            Ok(_) if one[0] == b'\n' => break,
            Ok(_) if one[0] == b'\r' && VT_IN.load(std::sync::atomic::Ordering::SeqCst) => break,
            Ok(_) => b.push(one[0]),
        }
    }
    Some(String::from_utf8_lossy(&b).trim_end_matches('\r').to_string())
}

fn run_steps(steps: &[String]) {
    let mut out = std::io::stdout();
    let mut input = std::io::stdin();
    let mut vars: Vec<(String, String)> = Vec::new();
    for st in steps {
        let (op, arg) = st.split_once(':').unwrap_or((st.as_str(), ""));
        match op {
            "say" => {
                let mut t = unescape(arg);
                for (k, v) in &vars {
                    t = t.replace(&format!("{{{k}}}"), v);
                }
                let _ = out.write_all(t.as_bytes());
            }
            "read" => {
                let v = read_line(&mut input).unwrap_or_default();
                vars.retain(|(k, _)| k != arg);
                vars.push((arg.to_string(), v));
            }
            "sleep" => std::thread::sleep(std::time::Duration::from_secs_f64(arg.parse().unwrap_or(1.0))),
            "lines" => {
                let (n, f) = arg.split_once(':').unwrap_or(("0", ""));
                let f = unescape(f);
                for i in 0..n.parse::<u64>().unwrap_or(0) {
                    let l = f.replace("{i5}", &format!("{i:05}")).replace("{i}", &i.to_string());
                    if writeln!(out, "{l}").is_err() {
                        break;
                    }
                }
            }
            "bytes" => {
                let (n, c) = arg.split_once(':').unwrap_or(("0", "x"));
                let mut left = n.parse::<usize>().unwrap_or(0);
                let chunk = c.repeat(65536.min(left.max(1)));
                while left > 0 {
                    let k = left.min(chunk.len());
                    if out.write_all(&chunk.as_bytes()[..k]).is_err() {
                        break;
                    }
                    left -= k;
                }
            }
            "yes" => {
                let mut left = arg.parse::<usize>().unwrap_or(0);
                let chunk = "y\n".repeat(32768);
                while left > 0 {
                    let k = left.min(chunk.len());
                    if out.write_all(&chunk.as_bytes()[..k]).is_err() {
                        break;
                    }
                    left -= k;
                }
            }
            "hexdump" => {
                let mut n = arg.parse::<usize>().unwrap_or(0);
                let mut text = String::new();
                let mut count = 0;
                let mut buf = [0u8; 1];
                while n > 0 && input.read(&mut buf).map(|k| k == 1).unwrap_or(false) {
                    text.push_str(&format!(" {:02x}", buf[0]));
                    count += 1;
                    if count % 16 == 0 {
                        text.push('\n');
                    }
                    n -= 1;
                }
                text.push('\n');
                let _ = out.write_all(text.as_bytes());
            }
            "echo_lines" => {
                let pre = unescape(arg);
                while let Some(l) = read_line(&mut input) {
                    let _ = write!(out, "{pre}{l}\n");
                    let _ = out.flush();
                }
            }
            "cat" => {
                let mut buf = [0u8; 4096];
                while let Ok(n) = input.read(&mut buf) {
                    if n == 0 || out.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = out.flush();
                }
            }
            "noecho" => tty_mode(false),
            "raw" => tty_mode(true),
            _ => {}
        }
        let _ = out.flush();
    }
}

fn remote_answer(exe: &std::path::Path, rc: &[String], l: &str) {
    let n_path = exe.with_extension("rc_n");
    let n: usize = std::fs::read_to_string(&n_path).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let _ = std::fs::write(&n_path, (n + 1).to_string());
    let what = rc.get(n).or(rc.last()).map(String::as_str).unwrap_or("ok");
    let id = l.split("\"request_id\":\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("");
    let mut o = std::io::stdout().lock();
    match what {
        "ok" => {
            let _ = writeln!(o, "{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"{id}\",\"response\":{{\"session_url\":\"https://claude.ai/code/session_stub{n}\",\"bridge_session_id\":\"cse_{n}\"}}}}}}");
            let _ = writeln!(o, "{{\"type\":\"system\",\"subtype\":\"bridge_state\",\"state\":\"connected\"}}");
        }
        "fail" => {
            let _ = writeln!(o, "{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"error\",\"request_id\":\"{id}\",\"error\":\"Remote Control initialization failed\"}}}}");
        }
        _ => {}
    }
    let _ = o.flush();
}

fn home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("CLAUDE_CONFIG_DIR").or_else(|_| std::env::var("GROK_HOME")).unwrap_or_else(|_| ".".into()))
}
fn state() -> Vec<Vec<String>> {
    std::fs::read_to_string(home().join("fake-admin.txt")).unwrap_or_default().lines().map(|l| l.split('\t').map(|x| x.to_string()).collect()).collect()
}
fn save(v: &[Vec<String>]) {
    let t: Vec<String> = v.iter().map(|r| r.join("\t")).collect();
    std::fs::write(home().join("fake-admin.txt"), t.join("\n")).unwrap();
}
fn fake_admin(args: &[String], get: &dyn Fn(&str) -> Option<String>) -> i32 {
    let mut st = state();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["mcp", "add", rest @ ..] => {
            let mut transport = "stdio";
            let mut pos = vec![];
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--" => { pos.extend(rest[i + 1..].iter().copied()); break; }
                    "--transport" | "-t" => { transport = rest[i + 1]; i += 2; }
                    "--scope" | "-s" | "--client-id" | "--callback-port" | "-e" | "--env" | "-H" | "--header" => i += 2,
                    x => { pos.push(x); i += 1; }
                }
            }
            let name = pos[0].to_string();
            let target = pos[1..].join(" ");
            let status = if transport == "stdio" { "connected" } else { "needs-auth" };
            st.retain(|r| !(r[0] == "mcp" && r[1] == name));
            st.push(vec!["mcp".into(), name.clone(), target, transport.into(), status.into()]);
            save(&st);
            println!("Added {transport} MCP server {name} to user config");
            0
        }
        ["mcp", "list"] | ["mcp", "list", "--json"] => {
            println!("Checking MCP server health...\n");
            for r in st.iter().filter(|r| r[0] == "mcp") {
                let s = match r[4].as_str() { "connected" => "\u{2713} Connected", "needs-auth" => "! Needs authentication", _ => "\u{2717} Failed to connect" };
                let k = if r[3] == "stdio" { String::new() } else { format!(" ({})", r[3].to_uppercase()) };
                println!("{}: {}{k} - {s}", r[1], r[2]);
            }
            0
        }
        ["mcp", "remove", .., name] => {
            let n = st.len();
            st.retain(|r| !(r[0] == "mcp" && r[1] == *name));
            save(&st);
            if st.len() < n { println!("Removed MCP server {name}"); 0 } else { eprintln!("No MCP server named {name}"); 1 }
        }
        ["mcp", "login", name, rest @ ..] => {
            let url = format!("https://auth.example.test/authorize?server={name}&state=s1");
            if rest.contains(&"--no-browser") {
                println!("Open this URL to sign in: {url}");
            } else if get("browser_fails").is_some() {
                println!("Couldn't open a browser. Visit: {url}");
            } else {
                // Like claude: it opens the browser itself.
                println!("Opening your browser to sign in...");
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(home().join("browser-opens.txt")).unwrap();
                let _ = writeln!(f, "{url}");
            }
            std::thread::sleep(std::time::Duration::from_millis(get("login_delay_ms").and_then(|d| d.parse().ok()).unwrap_or(300)));
            if get("login_fail").is_some() { eprintln!("Authentication failed: access_denied"); return 1; }
            for r in st.iter_mut().filter(|r| r[0] == "mcp" && r[1] == *name) { r[4] = "connected".into(); }
            save(&st);
            println!("Authentication successful");
            0
        }
        ["plugin", "install", id, ..] => {
            st.push(vec!["plugin".into(), id.to_string()]);
            let short = id.split('@').next().unwrap_or(id);
            if short == "slack" {
                st.push(vec!["mcp".into(), "plugin:slack:slack".into(), "https://mcp.slack.com/mcp".into(), "http".into(), "needs-auth".into()]);
            }
            save(&st);
            println!("\u{2714} Successfully installed plugin: {id}");
            0
        }
        ["plugin", "marketplace", "add", src] => { st.push(vec!["market".into(), src.to_string()]); save(&st); println!("Added marketplace {src}"); 0 }
        ["plugin", act, id, ..] => { st.push(vec![act.to_string(), id.to_string()]); save(&st); println!("{act} {id}"); 0 }
        _ => { eprintln!("unknown"); 2 }
    }
}
fn fake_login(get: &dyn Fn(&str) -> Option<String>) {
    use std::io::BufRead;
    let code = get("login_code").unwrap_or_else(|| "CODE-1234".into());
    println!("Browser didn't open? Use the url below to sign in:\r\n\r");
    println!("https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-\r");
    println!("5944d1962f5e&state=abc123\r\n\r");
    print!("Paste code here if prompted > ");
    let _ = std::io::stdout().flush();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let l = line.replace("\x1b[200~", "").replace("\x1b[201~", "");
        if l.trim() == code {
            println!("\r\nLogin successful. Press Enter to continue\r");
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_secs(20));
            return;
        }
        print!("\r\nOAuth error: Invalid code\r\nPaste code here if prompted > ");
        let _ = std::io::stdout().flush();
    }
}
"#;

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// The compiled stub (once per test process).
fn built() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("godterm-stub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("stub.rs");
        std::fs::write(&src, SOURCE).unwrap();
        let out = dir.join(exe_name("stub"));
        let tmp = dir.join(exe_name("stub-building"));
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let st = std::process::Command::new(rustc)
            .args(["--edition", "2021", "-O", "-o"])
            .arg(&tmp)
            .arg(&src)
            .status()
            .expect("rustc for the test stub");
        assert!(st.success(), "the test stub did not compile");
        std::fs::rename(&tmp, &out).unwrap();
        out
    })
}

/// Run a stub "agent" in a console of its own on Windows (as a claude in
/// another terminal is), so a Ctrl-C sent to its console never reaches
/// the test process.
pub fn own_console(c: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        c.creation_flags(CREATE_NEW_CONSOLE);
    }
    c.stdin(std::process::Stdio::null())
}

/// Wait (up to 5 s) for a stub's ready_to file.
pub fn wait_ready(p: &Path) {
    let t0 = std::time::Instant::now();
    while !p.exists() && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(p.exists(), "the stub did not start in 5 s");
}

/// A program that exits at once, like /usr/bin/true (which Windows lacks):
/// the claude for tests that never need one to run.
pub fn true_bin() -> String {
    if cfg!(unix) {
        return "/usr/bin/true".into();
    }
    static T: OnceLock<PathBuf> = OnceLock::new();
    T.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("godterm-true-{}", std::process::id()));
        claude(&d, &[("exit_now", "1".into())])
    })
    .to_string_lossy()
    .into_owned()
}

/// A stub program in `dir` that runs `steps` (the `do=` steps, see the
/// module doc) in place of a `/bin/sh -c "..."` script. Returns its path.
pub fn script(dir: &Path, steps: &[&str]) -> PathBuf {
    let opts: Vec<(&str, String)> = steps.iter().map(|s| ("do", s.to_string())).collect();
    claude(dir, &opts)
}

/// A stub "claude" in `dir` doing what `opts` say (see the module doc).
pub fn claude(dir: &Path, opts: &[(&str, String)]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join(exe_name("claude"));
    // A hard link: no file of ours is ever open for writing, so a fork in
    // another test thread cannot hold it (ETXTBSY on exec). Else a copy to
    // a temp name, synced, closed, then renamed into place.
    let _ = std::fs::remove_file(&exe);
    if std::fs::hard_link(built(), &exe).is_err() {
        let tmp = dir.join(format!(".claude-{}.tmp", std::process::id()));
        {
            let mut out = std::fs::File::create(&tmp).unwrap();
            let mut src = std::fs::File::open(built()).unwrap();
            std::io::copy(&mut src, &mut out).unwrap();
            out.sync_all().unwrap();
        }
        let perm = std::fs::metadata(built()).unwrap().permissions();
        std::fs::set_permissions(&tmp, perm).unwrap();
        std::fs::rename(&tmp, &exe).unwrap();
    }
    let cfg: String = opts.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    std::fs::write(exe.with_extension("cfg"), cfg).unwrap();
    exe
}

#[cfg(test)]
mod tests {
    #[test]
    fn records_args_and_stdin() {
        use std::io::Write;
        let d = std::env::temp_dir().join(format!("godterm-stubtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let exe = super::claude(
            &d,
            &[
                ("args_to", d.join("args.txt").display().to_string()),
                ("stdin_to", d.join("in.txt").display().to_string()),
            ],
        );
        let mut c = std::process::Command::new(&exe)
            .args(["-p", "--model", "x"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(b"hello\n").unwrap();
        assert!(c.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(d.join("args.txt")).unwrap(),
            "-p --model x\n"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("in.txt")).unwrap(),
            "hello\n"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scripted_steps_replace_a_shell_one_liner() {
        use std::io::{Read, Write};
        let d = std::env::temp_dir().join(format!("godterm-stubscript-{}", std::process::id()));
        let exe = super::script(
            &d,
            &[
                "say:first\\n",
                "read:x",
                "say:got {x}\\n",
                "lines:2:l{i5}",
                "bytes:5:z",
                "say:\\n",
                "echo_lines:> ",
            ],
        );
        let mut c = std::process::Command::new(&exe)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut i = c.stdin.take().unwrap();
        i.write_all(b"hi\nmore\n").unwrap();
        drop(i);
        let mut o = String::new();
        c.stdout.take().unwrap().read_to_string(&mut o).unwrap();
        assert_eq!(
            o.replace("\r\n", "\n"),
            "first\ngot hi\nl00000\nl00001\nzzzzz\n> more\n"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
