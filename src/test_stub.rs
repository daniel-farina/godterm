//! A stand in "claude" for tests that runs on every OS: a tiny Rust
//! program compiled once per test run (instead of a `#!/bin/sh` script),
//! copied next to a `.cfg` file that says what this copy does:
//!   args_to=PATH      append the arguments as one line
//!   stdin_to=PATH     copy stdin there as it arrives (stdin_append=1 appends)
//!   sleep_s=N         then stay alive N seconds
//!   exit_now=1        exit at once (like /usr/bin/true)
//!   child_sleep_s=N   first start a child (another copy) that sleeps N s
//!   print_n=N         print the numbers 1 to N
//!   ready_to=PATH     write that file once started (after the child)
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
    if let Some(p) = get("args_to") {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap();
        let _ = writeln!(f, "{}", args.join(" "));
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
