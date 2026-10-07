//! Processes on every OS: the list (pid, parent, name, exe, command line,
//! folder where the OS tells), who listens on a port, and stopping one
//! gracefully. Unix asks `ps` and `lsof`; Windows asks the system through
//! sysinfo, `netstat -ano`, and the console (Ctrl-C) for a graceful stop.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    /// The program's file name ("claude", "node.exe").
    pub name: String,
    pub exe: Option<PathBuf>,
    pub cmd: Vec<String>,
    /// Its working folder, when the OS lets us read it.
    pub cwd: Option<PathBuf>,
}

impl ProcInfo {
    /// The command line as one string (the name when it is not readable).
    pub fn command_line(&self) -> String {
        if self.cmd.is_empty() {
            self.name.clone()
        } else {
            self.cmd.join(" ")
        }
    }
}

/// A program's base name, compared the same on every OS: no folder, no
/// ".exe", lower case on Windows ("C:\\x\\Claude.exe" and "/x/claude" are
/// both "claude").
pub fn program_name(s: &str) -> String {
    let base = s.rsplit(['/', '\\']).next().unwrap_or(s);
    let base = if cfg!(windows) {
        base.to_lowercase()
    } else {
        base.to_string()
    };
    base.strip_suffix(".exe").unwrap_or(&base).to_string()
}

#[cfg(unix)]
pub fn processes() -> Vec<ProcInfo> {
    let Ok(out) = output_timeout(
        std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,args="]),
        std::time::Duration::from_secs(2),
    ) else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let cmd: Vec<String> = it.map(str::to_string).collect();
            let name = cmd
                .first()
                .map(|c| c.rsplit('/').next().unwrap_or(c).to_string())
                .unwrap_or_default();
            Some(ProcInfo {
                pid,
                ppid,
                name,
                exe: None,
                cmd,
                cwd: None,
            })
        })
        .collect()
}

#[cfg(windows)]
fn from_sysinfo(p: &sysinfo::Process) -> ProcInfo {
    ProcInfo {
        pid: p.pid().as_u32(),
        ppid: p.parent().map(|x| x.as_u32()).unwrap_or(0),
        name: p.name().to_string_lossy().into_owned(),
        exe: p.exe().map(|e| e.to_path_buf()),
        cmd: p
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        cwd: p.cwd().map(|c| c.to_path_buf()),
    }
}

#[cfg(windows)]
fn refresh_kind() -> sysinfo::ProcessRefreshKind {
    use sysinfo::{ProcessRefreshKind, UpdateKind};
    ProcessRefreshKind::nothing()
        .with_cmd(UpdateKind::Always)
        .with_exe(UpdateKind::Always)
        .with_cwd(UpdateKind::Always)
}

#[cfg(windows)]
pub fn processes() -> Vec<ProcInfo> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, refresh_kind());
    sys.processes().values().map(from_sysinfo).collect()
}

/// One process, if it runs.
#[cfg(windows)]
pub fn process(pid: u32) -> Option<ProcInfo> {
    let mut sys = sysinfo::System::new();
    let p = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[p]), true, refresh_kind());
    sys.process(p).map(from_sysinfo)
}

#[cfg(unix)]
#[allow(dead_code)] // Unix asks ps per field where it needs one (takeover)
pub fn process(pid: u32) -> Option<ProcInfo> {
    let field = |f: &str| {
        let o = std::process::Command::new("ps")
            .args(["-o", &format!("{f}="), "-p", &pid.to_string()])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    let args = field("args")?;
    let cmd: Vec<String> = args.split_whitespace().map(str::to_string).collect();
    Some(ProcInfo {
        pid,
        ppid: field("ppid").and_then(|p| p.parse().ok()).unwrap_or(0),
        name: field("comm")
            .map(|c| c.rsplit('/').next().unwrap_or(&c).to_string())
            .unwrap_or_default(),
        exe: None,
        cmd,
        cwd: crate::paths::cwd_of_pid(pid),
    })
}

/// The pids listening on a TCP port in `netstat -ano` output (Windows).
#[cfg_attr(unix, allow(dead_code))]
pub fn parse_netstat(text: &str, port: u16) -> Vec<u32> {
    let mut out = vec![];
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        // Proto, local, foreign, state, pid.
        if w.len() < 5 || !w[0].eq_ignore_ascii_case("tcp") {
            continue;
        }
        if !w[3].eq_ignore_ascii_case("listening") {
            continue;
        }
        let local_port = w[1].rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
        if local_port == Some(port) {
            if let Ok(pid) = w[4].parse::<u32>() {
                if !out.contains(&pid) {
                    out.push(pid);
                }
            }
        }
    }
    out
}

/// Who listens on a TCP port: (pid, program).
pub fn listeners(port: u16) -> Vec<(u32, String)> {
    #[cfg(windows)]
    {
        let Ok(o) = output_timeout(
            std::process::Command::new("netstat").args(["-ano", "-p", "TCP"]),
            std::time::Duration::from_secs(3),
        ) else {
            return vec![];
        };
        let all = processes();
        parse_netstat(&String::from_utf8_lossy(&o.stdout), port)
            .into_iter()
            .map(|pid| {
                let name = all
                    .iter()
                    .find(|p| p.pid == pid)
                    .map(|p| program_name(&p.name))
                    .unwrap_or_default();
                (pid, name)
            })
            .collect()
    }
    #[cfg(unix)]
    {
        let Ok(o) = output_timeout(
            std::process::Command::new("lsof").args([
                "-nP",
                &format!("-iTCP:{port}"),
                "-sTCP:LISTEN",
            ]),
            std::time::Duration::from_secs(3),
        ) else {
            return vec![];
        };
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .skip(1)
            .filter_map(|l| {
                let w: Vec<&str> = l.split_whitespace().collect();
                Some((w.get(1)?.parse::<u32>().ok()?, w.first()?.to_string()))
            })
            .collect()
    }
}

/// Ask a process to stop the way a user would (Ctrl-C): SIGINT on Unix; on
/// Windows a Ctrl-C (then a Ctrl-Break) into its console, sent by a short
/// lived helper (only a process attached to a console can signal it).
/// False when it could not be delivered (no console to attach to, as on
/// a headless runner): the caller then stops it by force.
pub fn interrupt(pid: u32) -> bool {
    #[cfg(unix)]
    {
        crate::platform::kill(pid as i32, crate::platform::SIGINT) == 0
    }
    #[cfg(windows)]
    {
        let Some(mut helper) = helper(pid) else {
            return false;
        };
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Ok(mut c) = helper
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return false;
        };
        let t0 = std::time::Instant::now();
        loop {
            match c.try_wait() {
                // 0: it exited; 2: delivered, still running; 1: no console.
                Ok(Some(st)) => return matches!(st.code(), Some(0) | Some(2)),
                Ok(None) if t0.elapsed() > std::time::Duration::from_secs(8) => {
                    let _ = c.kill();
                    return false;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => return false,
            }
        }
    }
}

/// The helper process: `godterm __ctrl-c PID`; in unit tests the test
/// binary itself, running only `ctrl_c_helper` (see the tests).
#[cfg(windows)]
fn helper(pid: u32) -> Option<std::process::Command> {
    let exe = std::env::current_exe().ok()?;
    let mut c = std::process::Command::new(exe);
    if cfg!(test) {
        c.args(["--exact", "procs::tests::ctrl_c_helper", "--test-threads=1"])
            .env("GODTERM_CTRL_C_PID", pid.to_string());
    } else {
        c.args(["__ctrl-c", &pid.to_string()]);
    }
    Some(c)
}

/// The helper's side (`godterm __ctrl-c PID`), its exit code: 0 the target
/// exited, 2 delivered but it still runs, 1 no console to attach to.
/// It leaves its own console, joins the target's, ignores the events
/// itself, sends Ctrl-C to everyone there, and when that is ignored (a
/// process can inherit "ignore Ctrl-C") a Ctrl-Break, which no process
/// can ignore that way.
#[cfg(windows)]
pub fn send_ctrl_c(pid: u32) -> i32 {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
        CTRL_BREAK_EVENT, CTRL_C_EVENT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    // SAFETY: console and process calls on handles we own; no pointers
    // beyond the null handler.
    unsafe {
        let h = OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        let exited = |ms: u32| !h.is_null() && WaitForSingleObject(h, ms) == WAIT_OBJECT_0;
        FreeConsole();
        if AttachConsole(pid) == 0 {
            if !h.is_null() {
                CloseHandle(h);
            }
            return 1;
        }
        SetConsoleCtrlHandler(None, 1);
        let mut sent = GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) != 0;
        let mut code = if sent && exited(1500) { 0 } else { 2 };
        if code != 0 {
            sent |= GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0) != 0;
            if exited(2500) {
                code = 0;
            }
        }
        FreeConsole();
        if !h.is_null() {
            CloseHandle(h);
        }
        if sent {
            code
        } else {
            1
        }
    }
}

/// Stop a process that did not answer Ctrl-C: SIGTERM on Unix (it can
/// still clean up); on Windows there is no polite second step, so this
/// terminates it (callers say it was forced).
pub fn terminate(pid: u32) -> bool {
    crate::platform::kill(pid as i32, crate::platform::SIGTERM) == 0
}

/// Whether `terminate` is forced on this OS (no cleanup in the process).
pub const TERMINATE_IS_FORCED: bool = cfg!(windows);

/// Put text on the clipboard (pbcopy) in the background, at most 2 s: the
/// screen never waits on it.
pub fn copy_to_clipboard(text: String) {
    std::thread::spawn(move || {
        use std::io::Write;
        let Ok(mut c) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return;
        };
        if let Some(mut i) = c.stdin.take() {
            let _ = i.write_all(text.as_bytes());
        }
        let t0 = std::time::Instant::now();
        loop {
            match c.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if t0.elapsed() > std::time::Duration::from_secs(2) => {
                    let _ = c.kill();
                    let _ = c.wait();
                    return;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    });
}

/// Run a command for its output, but never longer than `limit`: then it
/// is killed and reaped, and the result says it timed out. Output is read
/// as it comes, so a full pipe cannot stall it.
pub fn output_timeout(
    cmd: &mut std::process::Command,
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let read = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = vec![];
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut v);
            }
            v
        })
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let t0 = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if t0.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {} ms", limit.as_millis()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(15)),
            Err(e) => return Err(e.to_string()),
        }
    };
    Ok(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_netstat() {
        assert_eq!(program_name("/usr/local/bin/claude"), "claude");
        assert_eq!(program_name("claude"), "claude");
        if cfg!(windows) {
            assert_eq!(program_name(r"C:\Users\x\Claude.EXE"), "claude");
        }
        let text = "\r\nActive Connections\r\n\r\n  Proto  Local Address          Foreign Address        State           PID\r\n  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1040\r\n  TCP    0.0.0.0:3000           0.0.0.0:0              LISTENING       4242\r\n  TCP    [::]:3000              [::]:0                 LISTENING       4242\r\n  TCP    127.0.0.1:3000         127.0.0.1:50112        ESTABLISHED     4242\r\n  TCP    127.0.0.1:30000        0.0.0.0:0              LISTENING       7\r\n";
        assert_eq!(parse_netstat(text, 3000), vec![4242]);
        assert_eq!(parse_netstat(text, 135), vec![1040]);
        assert!(parse_netstat(text, 1).is_empty());
    }

    /// The helper process of `interrupt` in tests (Windows): only when
    /// started for that, it sends the Ctrl-C and exits with the result.
    #[test]
    fn ctrl_c_helper() {
        #[cfg(windows)]
        if let Some(pid) = std::env::var("GODTERM_CTRL_C_PID")
            .ok()
            .and_then(|p| p.parse::<u32>().ok())
        {
            std::process::exit(send_ctrl_c(pid));
        }
    }

    /// A stub agent in a console of its own stops on a graceful interrupt
    /// (SIGINT; on Windows Ctrl-C, then Ctrl-Break, into its console). With
    /// no console to reach (a headless Windows runner), interrupt says so,
    /// and the force stop the take-over falls back to ends it.
    #[test]
    fn interrupt_stops_an_agent_gracefully() {
        let d = std::env::temp_dir().join(format!("godterm-ctrlc-{}", std::process::id()));
        let stub = crate::test_stub::claude(&d, &[("sleep_s", "30".into())]);
        let mut c = crate::test_stub::own_console(&mut std::process::Command::new(&stub))
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(crate::platform::alive(c.id()));
        let gone_within = |c: &mut std::process::Child, s: u64| {
            let t0 = std::time::Instant::now();
            while t0.elapsed() < std::time::Duration::from_secs(s) {
                if let Ok(Some(_)) = c.try_wait() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            false
        };
        if interrupt(c.id()) {
            assert!(gone_within(&mut c, 8), "a delivered interrupt stops it");
        } else {
            // Only Windows can lack a console to signal.
            assert!(cfg!(windows), "SIGINT is always deliverable");
            assert_eq!(
                crate::takeover::after_interrupt(0, false),
                crate::takeover::INTERRUPT_GIVE_UP,
                "the take-over goes straight to the confirmed force stop"
            );
            assert!(terminate(c.id()));
            assert!(gone_within(&mut c, 5), "the force stop ends it");
            assert!(TERMINATE_IS_FORCED, "and the reply says forced");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lists_ourselves_and_a_child() {
        let me = std::process::id();
        let all = processes();
        assert!(all.iter().any(|p| p.pid == me), "we are in the list");
        let d = std::env::temp_dir().join(format!("godterm-procs-{me}"));
        let stub = crate::test_stub::claude(&d, &[("sleep_s", "20".into())]);
        let mut c = std::process::Command::new(&stub)
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let p = process(c.id()).expect("the child");
        assert_eq!(p.ppid, me);
        assert_eq!(program_name(&p.name), "claude");
        assert!(listeners(1).is_empty());
        let _ = c.kill();
        let _ = c.wait();
        let _ = std::fs::remove_dir_all(&d);
    }
}
