//! A stand in "claude" for tests that runs on every OS: a tiny Rust
//! program compiled once per test run (instead of a `#!/bin/sh` script),
//! copied next to a `.cfg` file that says what this copy does:
//!   args_to=PATH      append the arguments as one line
//!   env_to=PATH       record only home/session marker vars for launch tests
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
}
// Unix: take SIGINT the default way (exit) even when the parent left it
// ignored (a shell's background job ignores SIGINT, and that is inherited).
#[cfg(unix)]
extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}
fn main() {
    #[cfg(windows)]
    unsafe {
        SetConsoleCtrlHandler(0, 0);
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
    if let Some(p) = get("env_to") {
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
        let _ = writeln!(f, "{}", args.join(" "));
    }
    if let Some(p) = get("print_file") {
        print!("{}", std::fs::read_to_string(p).unwrap_or_default());
        let _ = std::io::stdout().flush();
        if let Some(e) = get("stderr_line") {
            eprintln!("{e}");
        }
        std::process::exit(get("exit_code").and_then(|c| c.parse().ok()).unwrap_or(0));
    }
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
                if let Some(f) = out.as_mut() {
                    let _ = f.write_all(&buf[..n]);
                    let _ = f.flush();
                }
            }
        }
    }
    if let Some(s) = get("sleep_s").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(s));
    }
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
}
