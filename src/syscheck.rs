//! system_check: read-only looks at the machine for the assistant, so it
//! can verify what it is about to say ("the server is gone", "nothing
//! runs there"). Only allowlisted commands, run directly with their argv
//! (no shell, no pipes, no redirection), with checked arguments: no write
//! flags, the file guard on every path, a 5 s timeout and 8 KB of output.
//! Anything else is refused: a tab agent does it, or the user.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_OUT: usize = 8 * 1024;

const REFUSE: &str = "not a read-only check; ask a tab agent to do it, or ask the user";

/// Characters a shell would act on: never part of an argument here. On
/// Windows a backslash is a path, but cmd's ^ and % are refused too.
fn has_shell_syntax(s: &str) -> bool {
    if WINDOWS.with(|w| w.get()) {
        return s.chars().any(|c| {
            matches!(
                c,
                ';' | '|'
                    | '&'
                    | '$'
                    | '`'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '\n'
                    | '\r'
                    | '*'
                    | '?'
                    | '!'
                    | '^'
                    | '%'
            )
        });
    }
    s.chars().any(|c| {
        matches!(
            c,
            ';' | '|'
                | '&'
                | '$'
                | '`'
                | '<'
                | '>'
                | '('
                | ')'
                | '{'
                | '}'
                | '\n'
                | '\r'
                | '*'
                | '?'
                | '!'
                | '\\'
        )
    })
}

/// A checked command: (program, args, working folder).
#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub prog: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// Arguments that are paths must be readable by the guard rules.
fn path_ok(a: &str, allowed: &[PathBuf]) -> Result<(), String> {
    let p = crate::config::expand_tilde(a);
    if let Some(why) = crate::guard::denied(&p, allowed) {
        return Err(format!("refused: {why}"));
    }
    Ok(())
}

thread_local! {
    /// Check as Windows does (tests flip it to check the Windows rules on
    /// any machine).
    static WINDOWS: std::cell::Cell<bool> = const { std::cell::Cell::new(cfg!(windows)) };
}

/// The Windows allowlist: tasklist, netstat -ano, where, hostname, read
/// only git, and dir / type (done here, not by cmd: there is no shell).
fn check_windows(argv: &[String], cwd: Option<&Path>, allowed: &[PathBuf]) -> Result<Cmd, String> {
    let name = crate::procs::program_name(&argv[0]).to_lowercase();
    let args: Vec<String> = argv[1..].to_vec();
    let up: Vec<String> = args.iter().map(|a| a.to_uppercase()).collect();
    let refuse = || Err(format!("refused: {} ({REFUSE})", argv.join(" ")));
    let internal = |prog: &str, args: Vec<String>| -> Result<Cmd, String> {
        if args.is_empty() {
            return Err(format!("{prog} needs a path"));
        }
        for p in &args {
            path_ok(p, allowed)?;
        }
        Ok(Cmd {
            prog: prog.into(),
            args,
            cwd: None,
        })
    };
    match name.as_str() {
        "tasklist" => {
            // Filters and formats only: never /S /U /P (another machine).
            let mut i = 0;
            while i < up.len() {
                match up[i].as_str() {
                    "/FI" | "/FO" if i + 1 < up.len() => i += 2,
                    "/V" | "/NH" | "/SVC" | "/APPS" => i += 1,
                    "/M" => {
                        i += if i + 1 < up.len() && !up[i + 1].starts_with('/') {
                            2
                        } else {
                            1
                        }
                    }
                    _ => return refuse(),
                }
            }
            Ok(Cmd {
                prog: "tasklist".into(),
                args,
                cwd: None,
            })
        }
        "netstat" => {
            let ok = up.iter().all(|a| {
                matches!(
                    a.as_str(),
                    "-A" | "-N"
                        | "-O"
                        | "-ANO"
                        | "-AN"
                        | "-NO"
                        | "-P"
                        | "TCP"
                        | "UDP"
                        | "TCPV6"
                        | "-S"
                        | "-E"
                        | "-R"
                )
            });
            if !ok {
                return refuse();
            }
            Ok(Cmd {
                prog: "netstat".into(),
                args,
                cwd: None,
            })
        }
        "where" => {
            if args.iter().any(|a| a.contains(['\\', '/', ':'])) || args.is_empty() {
                return refuse();
            }
            Ok(Cmd {
                prog: "where".into(),
                args,
                cwd: None,
            })
        }
        "hostname" | "whoami" if args.is_empty() => Ok(Cmd {
            prog: name,
            args,
            cwd: None,
        }),
        "dir" | "ls" => internal(
            "dir",
            args.into_iter()
                .filter(|a| !a.starts_with(['/', '-']) || Path::new(a).is_absolute())
                .collect(),
        ),
        "type" | "cat" | "head" => internal(
            "type",
            args.into_iter()
                .filter(|a| !a.starts_with('-') && a.parse::<u64>().is_err())
                .collect(),
        ),
        "git" => {
            // The same read only subcommands as elsewhere.
            let mut unix: Vec<String> = vec!["git".into()];
            unix.extend(args);
            WINDOWS.with(|w| w.set(false));
            let r = check(&unix, cwd, allowed);
            WINDOWS.with(|w| w.set(true));
            r
        }
        _ => refuse(),
    }
}

/// Check `argv` against the allowlist. `cwd` is where git runs (a tab's
/// folder); `allowed` are the workspaces the guard lets through.
pub fn check(argv: &[String], cwd: Option<&Path>, allowed: &[PathBuf]) -> Result<Cmd, String> {
    let Some(prog) = argv.first() else {
        return Err("give a command, e.g. [\"ps\", \"-p\", \"123\"]".into());
    };
    if argv.iter().any(|a| has_shell_syntax(a)) {
        return Err(format!("refused: shell syntax is not allowed ({REFUSE})"));
    }
    if WINDOWS.with(|w| w.get()) {
        return check_windows(argv, cwd, allowed);
    }
    let args: Vec<String> = argv[1..].to_vec();
    let flags: Vec<&str> = args
        .iter()
        .filter(|a| a.starts_with('-') || a.starts_with('+'))
        .map(String::as_str)
        .collect();
    let paths: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-') && !a.starts_with('+'))
        .map(String::as_str)
        .collect();
    let name = prog.rsplit('/').next().unwrap_or(prog).to_string();
    let plain = |ok_flags: &[&str]| {
        flags
            .iter()
            .all(|f| ok_flags.iter().any(|o| f.starts_with(o)))
    };
    let ok = match name.as_str() {
        "ps" => !args.iter().any(|a| a.contains("kill")),
        "pgrep" => true,
        "lsof" => {
            plain(&[
                "-i", "-p", "-c", "-n", "-P", "-a", "-s", "-t", "-u", "-d", "+D", "-l",
            ]) && paths.iter().all(|p| {
                p.starts_with(':')
                    || p.parse::<u32>().is_ok()
                    || path_ok(p, allowed).is_ok()
                    || p.contains("TCP")
                    || p.contains("UDP")
                    || p.contains("LISTEN")
            })
        }
        "top" => {
            args == ["-l", "1"]
                || (args.len() == 4 && args[0] == "-l" && args[1] == "1" && args[2] == "-n")
        }
        "vm_stat" | "uptime" | "sw_vers" => true,
        "which" => paths.iter().all(|p| !p.contains('/')),
        "df" => plain(&["-h", "-k", "-H", "-l"]),
        "du" => plain(&["-s", "-h", "-sh", "-d"]) && !paths.is_empty(),
        "netstat" => plain(&["-a", "-n", "-an", "-p", "-l", "-t", "-u", "-W"]),
        "ss" => plain(&[
            "-t", "-u", "-l", "-n", "-p", "-a", "-tlnp", "-tulpn", "-tln",
        ]),
        "ls" => plain(&[
            "-l", "-a", "-h", "-t", "-r", "-S", "-1", "-la", "-lh", "-lah", "-al", "-d", "-R",
        ]),
        "stat" | "file" | "wc" => true,
        "head" | "tail" => plain(&["-n", "-c"]) && !args.iter().any(|a| a == "-f" || a == "-F"),
        "git" => {
            let sub = paths.first().copied().unwrap_or("");
            matches!(
                sub,
                "status" | "log" | "branch" | "diff" | "show" | "rev-parse" | "remote"
            ) && !args.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-D" | "-d"
                        | "--delete"
                        | "-m"
                        | "-M"
                        | "--force"
                        | "-f"
                        | "add"
                        | "rm"
                        | "set-url"
                        | "--output"
                        | "-o"
                )
            }) && (sub != "diff"
                || args
                    .iter()
                    .any(|a| a == "--stat" || a == "--name-only" || a == "--shortstat"))
        }
        "launchctl" => args == ["list"] || (args.len() == 2 && args[0] == "list"),
        "pmset" => args == ["-g"] || (args.len() == 2 && args[0] == "-g"),
        "system_profiler" => args.iter().all(|a| {
            matches!(
                a.as_str(),
                "SPHardwareDataType"
                    | "SPSoftwareDataType"
                    | "SPDisplaysDataType"
                    | "SPPowerDataType"
                    | "SPStorageDataType"
                    | "-detailLevel"
                    | "mini"
                    | "basic"
            )
        }),
        _ => false,
    };
    if !ok {
        return Err(format!("refused: {} ({REFUSE})", argv.join(" ")));
    }
    // Paths read by the file commands go through the guard.
    if matches!(
        name.as_str(),
        "ls" | "stat" | "file" | "wc" | "head" | "tail" | "du"
    ) {
        for p in &paths {
            if p.parse::<u64>().is_ok() && matches!(name.as_str(), "head" | "tail") {
                continue; // the -n count
            }
            path_ok(p, allowed)?;
        }
    }
    let cwd = if name == "git" {
        let c = cwd.ok_or("git checks run in a tab's folder: give tab")?;
        path_ok(&c.to_string_lossy(), allowed)?;
        Some(c.to_path_buf())
    } else {
        None
    };
    Ok(Cmd {
        prog: name,
        args,
        cwd,
    })
}

/// Run a checked command: at most TIMEOUT, at most MAX_OUT of output.
pub fn run(c: &Cmd) -> Value {
    use std::io::Read;
    // dir and type (Windows): done here, never by cmd.
    if c.prog == "dir" || c.prog == "type" {
        let mut out = String::new();
        for a in &c.args {
            let p = crate::config::expand_tilde(a);
            if c.prog == "dir" {
                match std::fs::read_dir(&p) {
                    Ok(rd) => {
                        out.push_str(&format!("{}:\n", p.display()));
                        let mut names: Vec<String> = rd
                            .flatten()
                            .map(|e| {
                                let d = e.file_type().is_ok_and(|t| t.is_dir());
                                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                                format!(
                                    "{}{}  {}",
                                    e.file_name().to_string_lossy(),
                                    if d { std::path::MAIN_SEPARATOR_STR } else { "" },
                                    if d { String::new() } else { len.to_string() }
                                )
                            })
                            .collect();
                        names.sort();
                        out.push_str(&names.join("\n"));
                        out.push('\n');
                    }
                    Err(e) => out.push_str(&format!("{}: {e}\n", p.display())),
                }
            } else {
                let mut buf = vec![];
                let r = std::fs::File::open(&p)
                    .and_then(|f| f.take(MAX_OUT as u64 + 1).read_to_end(&mut buf));
                match r {
                    Ok(_) => out.push_str(&String::from_utf8_lossy(&buf)),
                    Err(e) => out.push_str(&format!("{}: {e}\n", p.display())),
                }
            }
        }
        let truncated = out.len() > MAX_OUT;
        let mut end = out.len().min(MAX_OUT);
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        return json!({"command": format!("{} {}", c.prog, c.args.join(" ")), "exit": 0, "output": out, "stderr": "", "truncated": truncated});
    }
    let mut cmd = std::process::Command::new(&c.prog);
    cmd.args(&c.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(d) = &c.cwd {
        cmd.current_dir(d);
    }
    let started = std::time::Instant::now();
    let Ok(mut child) = cmd.spawn() else {
        return json!({"error": format!("{} is not available here", c.prog)});
    };
    // Read both streams as they come (a full pipe would stall the
    // command), keeping the first MAX_OUT bytes.
    let reader = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut more = false;
            if let Some(mut r) = r {
                let mut buf = [0u8; 8192];
                while let Ok(n) = r.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let room = MAX_OUT.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                    more |= n > room;
                }
            }
            (kept, more)
        })
    };
    let out = reader(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let errs = reader(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code(),
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return json!({"error": format!("{} took longer than {} s and was stopped", c.prog, TIMEOUT.as_secs()), "timed_out": true});
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return json!({"error": e.to_string()}),
        }
    };
    let (o, truncated) = out.join().unwrap_or_default();
    let (e, _) = errs.join().unwrap_or_default();
    json!({
        "command": format!("{} {}", c.prog, c.args.join(" ")).trim(),
        "exit": code,
        "output": String::from_utf8_lossy(&o),
        "stderr": String::from_utf8_lossy(&e),
        "truncated": truncated,
    })
}

/// A process, its parents and everything below it.
pub fn process_tree(pid: u32) -> Value {
    let table = crate::activity::process_table();
    let Some(me) = table.iter().find(|p| p.pid == pid) else {
        return json!({"pid": pid, "running": false});
    };
    let mut parents = vec![];
    // Windows reuses pids and keeps a dead parent's id, so parent links can
    // form a cycle: stop at any pid already seen.
    let mut seen = std::collections::HashSet::from([pid]);
    let mut cur = me.ppid;
    while let Some(p) = table.iter().find(|p| p.pid == cur) {
        if !seen.insert(p.pid) {
            break;
        }
        parents.push(json!({"pid": p.pid, "cmd": crate::sessions::snippet(&p.cmd, 80)}));
        if p.ppid == p.pid || p.ppid <= 1 {
            break;
        }
        cur = p.ppid;
    }
    let kids: Vec<Value> = crate::activity::descendants(&table, pid).into_iter().map(|p| json!({"pid": p.pid, "parent": p.ppid, "cmd": crate::sessions::snippet(&p.cmd, 100), "helper": crate::activity::is_helper(&p.cmd)})).collect();
    json!({"pid": pid, "running": true, "cmd": crate::sessions::snippet(&me.cmd, 120), "parents": parents, "children": kids})
}

/// What listens on a TCP port (lsof; netstat -ano on Windows).
pub fn port_owner(port: u16) -> Value {
    let owners: Vec<Value> = crate::procs::listeners(port)
        .into_iter()
        .map(|(pid, command)| json!({"command": command, "pid": pid}))
        .collect();
    json!({"port": port, "listening": !owners.is_empty(), "owners": owners})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn allowlist_on_windows() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        WINDOWS.with(|w| w.set(true));
        let tmp = std::env::temp_dir();
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let t = tmp.to_string_lossy().into_owned();
        for ok in [
            a(&["tasklist", "/FI", "PID eq 4", "/FO", "CSV"]),
            a(&["tasklist.exe", "/V", "/NH"]),
            a(&["netstat", "-ano"]),
            a(&["NETSTAT", "-a", "-n", "-o", "-p", "TCP"]),
            a(&["where", "git"]),
            a(&["hostname"]),
            a(&["dir", &t]),
            a(&["ls", "-la", &t]),
        ] {
            assert!(check(&ok, None, &[]).is_ok(), "{ok:?}");
        }
        assert!(check(&a(&["git", "status"]), Some(&tmp), &[]).is_ok());
        for bad in [
            a(&["tasklist", "/S", "server"]),
            a(&["taskkill", "/PID", "4"]),
            a(&["netstat", "-b"]),
            a(&["cmd", "/c", "dir"]),
            a(&["powershell", "-c", "ls"]),
            a(&["del", &t]),
            a(&["dir", &format!("{t} & del x")]),
            a(&["where", r"C:\Windows\x"]),
            a(&["git", "push"]),
            a(&["type", "%USERPROFILE%"]),
        ] {
            assert!(check(&bad, Some(&tmp), &[]).is_err(), "{bad:?}");
        }
        // dir and type go through the guard and run here, not in cmd.
        let ssh = crate::config::home_dir().join(".ssh").join("id_ed25519");
        assert!(check(&a(&["type", &ssh.to_string_lossy()]), None, &[]).is_err());
        let d = tmp.join(format!("godterm-dir-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("notes.txt");
        std::fs::write(&f, "hello").unwrap();
        let c = check(&a(&["type", &f.to_string_lossy()]), None, &[]).unwrap();
        assert_eq!(run(&c)["output"], json!("hello"));
        let c = check(&a(&["dir", &d.to_string_lossy()]), None, &[]).unwrap();
        assert!(run(&c)["output"].as_str().unwrap().contains("notes.txt  5"));
        let _ = std::fs::remove_dir_all(&d);
        WINDOWS.with(|w| w.set(cfg!(windows)));
    }

    #[test]
    fn allowlist() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The Unix rules (on any machine).
        WINDOWS.with(|w| w.set(false));
        let tmp = std::env::temp_dir();
        for ok in [
            "ps -p 1",
            "ls -la /tmp",
            "lsof -i :3000",
            "top -l 1",
            "uptime",
            "df -h",
            "pgrep -fl claude",
            "netstat -an",
            "launchctl list",
            "pmset -g",
        ] {
            assert!(check(&argv(ok), None, &[]).is_ok(), "{ok}");
        }
        assert!(check(&argv("git status"), Some(&tmp), &[]).is_ok());
        for bad in [
            "rm -rf /tmp/x",
            "kill -9 1",
            "curl https://x",
            "sh -c ls",
            "bash",
            "ps;",
            "ls $(whoami)",
            "sudo ls",
            "git push",
            "git branch -D x",
            "git diff",
            "top",
            "tail -f /tmp/x",
            "pkill claude",
            "ls `id`",
            "ls /tmp|wc",
        ] {
            let a: Vec<String> = if bad == "ps;" {
                vec!["ps".into(), ";".into(), "rm".into()]
            } else {
                argv(bad)
            };
            assert!(check(&a, Some(&tmp), &[]).is_err(), "{bad}");
        }
        // The file guard on path arguments.
        let h = crate::config::home_dir();
        for p in [
            h.join(".ssh"),
            h.join(".claude/.credentials.json"),
            crate::config::app_home().join("control.json"),
        ] {
            assert!(
                check(&["ls".into(), p.to_string_lossy().into_owned()], None, &[]).is_err(),
                "{}",
                p.display()
            );
            assert!(check(
                &["head".into(), p.to_string_lossy().into_owned()],
                None,
                &[]
            )
            .is_err());
        }
        assert!(
            check(&argv("git status"), None, &[]).is_err(),
            "git needs a tab folder"
        );
        WINDOWS.with(|w| w.set(cfg!(windows)));
    }

    #[test]
    fn runs_with_caps() {
        // Portable programs: the test stub printing or sleeping.
        let d = std::env::temp_dir().join(format!("godterm-caps-{}", std::process::id()));
        let stub = |tag: &str, opts: &[(&str, String)]| {
            crate::test_stub::claude(&d.join(tag), opts)
                .to_string_lossy()
                .into_owned()
        };
        let r = run(&Cmd {
            prog: stub("one", &[("print_n", "1".into())]),
            args: vec![],
            cwd: None,
        });
        assert_eq!(r["output"].as_str().unwrap().trim(), "1");
        let big = run(&Cmd {
            prog: stub("big", &[("print_n", "100000".into())]),
            args: vec![],
            cwd: None,
        });
        assert!(
            big["truncated"] == json!(true) && big["output"].as_str().unwrap().len() <= MAX_OUT
        );
        let slow = run(&Cmd {
            prog: stub("slow", &[("sleep_s", "9".into())]),
            args: vec![],
            cwd: None,
        });
        assert_eq!(slow["timed_out"], json!(true));
        let t = process_tree(std::process::id());
        assert_eq!(t["running"], json!(true));
    }
}
