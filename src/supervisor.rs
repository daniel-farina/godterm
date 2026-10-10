//! The supervisor: a watchdog that opens GodTerm again after an unclean
//! exit (a crash, kill -9, a panic, the terminal window closed) and
//! restarts one whose control socket stopped answering. GodTerm is a TUI
//! that needs a terminal window, so the watchdog never owns the process:
//! launchd runs `godterm supervisor run` every 20 s (a user agent,
//! `~/Library/LaunchAgents/com.godterm.supervisor.plist`), and each run
//! checks once and relaunches through the launcher (`open GodTerm.app`,
//! which opens iTerm or Terminal and runs run.sh), or the command in
//! `supervisor.launch`.
//!
//! GodTerm marks `<home>/last_exit.json` unclean when it starts and clean
//! only when the user quits, so a quit is never undone. A relaunch leaves
//! `<home>/supervisor/relaunched`, which makes the new start restore every
//! tab at once (as after a restart to update); the assistant resumes its
//! session and turns Remote Control back on by itself (app_resume,
//! app_remote). At most 3 relaunches in 10 minutes, then it stops and says
//! so (the log and a notification).
//!
//! Linux would need a systemd user timer and service in place of launchd
//! (`systemctl --user enable --now godterm-supervisor.timer`), a launcher
//! that opens a terminal (`x-terminal-emulator -e godterm`, set as
//! supervisor.launch) and notify-send for the notice; the checks are the
//! same.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::config::{app_home, env_var, Config};

pub const LABEL: &str = "com.godterm.supervisor";
/// Seconds between launchd runs.
pub const INTERVAL_S: u64 = 20;
/// Relaunches allowed per window.
pub const MAX_RELAUNCHES: usize = 3;
pub const WINDOW_S: i64 = 600;
/// A relaunch gets this long to take the lock before another is tried.
pub const GRACE_S: i64 = 45;

pub fn dir() -> PathBuf {
    app_home().join("supervisor")
}

fn exit_path() -> PathBuf {
    app_home().join("last_exit.json")
}

fn state_path() -> PathBuf {
    dir().join("state.json")
}

fn relaunched_path() -> PathBuf {
    dir().join("relaunched")
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LastExit {
    pub clean: bool,
    pub pid: u32,
    pub at: i64,
}

fn load_exit() -> Option<LastExit> {
    serde_json::from_str(&std::fs::read_to_string(exit_path()).ok()?).ok()
}

fn save_exit(e: &LastExit) {
    if let Ok(s) = serde_json::to_string(e) {
        let _ = crate::config::write_private(&exit_path(), s);
    }
}

/// GodTerm started: an exit from now on is unclean until a quit.
pub fn mark_running() {
    save_exit(&LastExit {
        clean: false,
        pid: std::process::id(),
        at: now(),
    });
}

/// The user quit.
pub fn mark_clean() {
    save_exit(&LastExit {
        clean: true,
        pid: std::process::id(),
        at: now(),
    });
}

/// This start was a relaunch after a crash (the mark is used up).
pub fn take_relaunched() -> bool {
    std::fs::remove_file(relaunched_path()).is_ok()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    /// When it relaunched (unix seconds), newest last.
    pub relaunches: Vec<i64>,
    /// The control socket has not answered since.
    pub hang_since: Option<i64>,
    /// The rate limit notice went out.
    pub notified_at: Option<i64>,
}

fn load_state() -> State {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(s: &State) {
    let _ = std::fs::create_dir_all(dir());
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = crate::config::write_private(&state_path(), t);
    }
}

fn log(msg: &str) {
    let _ = std::fs::create_dir_all(dir());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir().join("supervisor.log"))
    {
        let _ = writeln!(
            f,
            "{} {msg}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
    }
}

/// What one check sees.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    /// The running GodTerm's pid (it holds the lock and is alive).
    pub running: Option<u32>,
    /// Its control socket answered (only asked while running).
    pub answered: bool,
    pub last_exit: Option<LastExit>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Nothing(String),
    /// Not answering, not yet for long enough.
    HangPending(i64),
    /// Hung: stop it (then relaunch).
    Restart(u32),
    Relaunch,
    /// Too many relaunches: stop and say so (true: the notice is new).
    RateLimited(bool),
}

/// Decide from what was seen; `st` is updated (hang start, relaunch times).
pub fn decide(cfg: &crate::config::SupervisorCfg, seen: &Seen, st: &mut State, t: i64) -> Action {
    if !cfg.enabled {
        return Action::Nothing("supervisor.enabled is off".into());
    }
    st.relaunches.retain(|r| t - r < WINDOW_S);
    if let Some(pid) = seen.running {
        if seen.answered || !cfg.hang_restart {
            st.hang_since = None;
            return Action::Nothing(format!("running (pid {pid})"));
        }
        let since = *st.hang_since.get_or_insert(t);
        if t - since < hang_s(cfg) {
            return Action::HangPending(t - since);
        }
        st.hang_since = None;
        return Action::Restart(pid);
    }
    st.hang_since = None;
    match &seen.last_exit {
        None => return Action::Nothing("no record of a run".into()),
        Some(e) if e.clean => return Action::Nothing("the last exit was a quit".into()),
        Some(_) => {}
    }
    if st.relaunches.last().is_some_and(|r| t - r < GRACE_S) {
        return Action::Nothing("a relaunch is starting".into());
    }
    if st.relaunches.len() >= MAX_RELAUNCHES {
        let new = st.notified_at.is_none_or(|n| t - n >= WINDOW_S);
        if new {
            st.notified_at = Some(t);
        }
        return Action::RateLimited(new);
    }
    st.relaunches.push(t);
    Action::Relaunch
}

fn hang_s(cfg: &crate::config::SupervisorCfg) -> i64 {
    env_var("SUPERVISOR_HANG_S")
        .and_then(|s| s.parse().ok())
        .unwrap_or(cfg.hang_s.max(10)) as i64
}

/// The control socket answers a line (any reply: the socket thread is
/// alive; a stopped or wedged process gives none).
pub fn ping() -> bool {
    #[cfg(unix)]
    {
        let Some(sock) = std::fs::read_to_string(crate::control::token_path())
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["socket"].as_str().map(str::to_string))
        else {
            return false;
        };
        let Ok(mut s) = std::os::unix::net::UnixStream::connect(sock) else {
            return false;
        };
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
        if writeln!(s, "{{\"token\":\"supervisor-ping\"}}").is_err() {
            return false;
        }
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).is_ok_and(|n| n > 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// SIGTERM (and SIGCONT, so a stopped one can act on it), then SIGKILL
/// after the wait.
fn stop(pid: u32) {
    let wait = env_var("SUPERVISOR_KILL_WAIT_S")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10u64);
    #[cfg(unix)]
    {
        crate::platform::kill(pid as i32, libc::SIGTERM);
        crate::platform::kill(pid as i32, libc::SIGCONT);
    }
    let t0 = std::time::Instant::now();
    while crate::platform::alive(pid) && t0.elapsed() < Duration::from_secs(wait) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if crate::platform::alive(pid) {
        log(&format!("pid {pid} did not stop in {wait} s: SIGKILL"));
        #[cfg(unix)]
        crate::platform::kill(pid as i32, libc::SIGKILL);
        let t0 = std::time::Instant::now();
        while crate::platform::alive(pid) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The launcher command: GODTERM_SUPERVISOR_LAUNCH, supervisor.launch, or
/// opening GodTerm.app.
pub fn launch_command(cfg: &crate::config::SupervisorCfg) -> String {
    if let Some(c) = env_var("SUPERVISOR_LAUNCH") {
        return c;
    }
    if !cfg.launch.trim().is_empty() {
        return cfg.launch.clone();
    }
    let user = crate::config::home_dir().join("Applications/GodTerm.app");
    let app = if user.exists() {
        user
    } else {
        PathBuf::from("/Applications/GodTerm.app")
    };
    format!("open {}", shell_quote(&app.to_string_lossy()))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn launch(cmd: &str) -> Result<()> {
    let path = format!(
        "/opt/homebrew/bin:/usr/local/bin:{}",
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into())
    );
    let _ = std::fs::create_dir_all(dir());
    std::fs::write(relaunched_path(), now().to_string())?;
    let st = std::process::Command::new("/bin/sh")
        .args(["-c", cmd])
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("running {cmd}"))?;
    if !st.success() {
        let _ = std::fs::remove_file(relaunched_path());
        bail!("{cmd} exited with {st}");
    }
    Ok(())
}

fn notify(text: &str) {
    if cfg!(test) || env_var("NO_OPEN").is_some() || !cfg!(target_os = "macos") {
        return;
    }
    let script = format!(
        "display notification \"{}\" with title \"GodTerm\"",
        text.replace('\\', "").replace('"', "'")
    );
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .status();
}

/// One check (what launchd runs).
pub fn run_once() -> Result<()> {
    if !Config::path().exists() {
        return Ok(());
    }
    let cfg = Config::load_or_init()?.supervisor;
    let running = crate::instance::running()
        .map(|h| h.pid)
        .filter(|p| *p != 0 && crate::platform::alive(*p));
    let seen = Seen {
        running,
        answered: running.is_some() && ping(),
        last_exit: load_exit(),
    };
    let mut st = load_state();
    let t = now();
    let mut action = decide(&cfg, &seen, &mut st, t);
    if let Action::Restart(pid) = action {
        log(&format!(
            "pid {pid}: the control socket has not answered for {} s; restarting it",
            hang_s(&cfg)
        ));
        stop(pid);
        // Stopped by us: an unclean exit (whatever it wrote on the way out).
        save_exit(&LastExit {
            clean: false,
            pid,
            at: t,
        });
        let seen = Seen {
            running: None,
            answered: false,
            last_exit: load_exit(),
        };
        action = decide(&cfg, &seen, &mut st, t);
    }
    match &action {
        Action::Relaunch => {
            let cmd = launch_command(&cfg);
            log(&format!(
                "GodTerm is not running and its last exit was not a quit: relaunching ({} of {MAX_RELAUNCHES} in 10 min): {cmd}",
                st.relaunches.len()
            ));
            if let Err(e) = launch(&cmd) {
                log(&format!("relaunch failed: {e:#}"));
            }
        }
        Action::RateLimited(new) => {
            if *new {
                let msg = format!(
                    "GodTerm crashed {MAX_RELAUNCHES} times in 10 minutes; the supervisor stopped relaunching it. Open it yourself when ready."
                );
                log(&format!("rate limit: {msg}"));
                notify(&msg);
            }
        }
        Action::HangPending(s) => log(&format!("the control socket has not answered for {s} s")),
        Action::Nothing(_) | Action::Restart(_) => {}
    }
    save_state(&st);
    Ok(())
}

fn label(args: &[String]) -> String {
    args.windows(2)
        .find(|w| w[0] == "--label")
        .map(|w| w[1].clone())
        .or_else(|| env_var("SUPERVISOR_LABEL"))
        .unwrap_or_else(|| LABEL.into())
}

pub fn plist_path(label: &str) -> PathBuf {
    crate::config::home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The launchd user agent: `exe supervisor run` every `interval` s, with
/// the home and the GODTERM_SUPERVISOR_* settings of the installing shell.
pub fn plist(label: &str, exe: &str, interval: u64, env: &[(String, String)]) -> String {
    let mut e = String::new();
    for (k, v) in env {
        e.push_str(&format!(
            "    <key>{}</key><string>{}</string>\n",
            xml(k),
            xml(v)
        ));
    }
    let out = xml(&dir().join("launchd.log").to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{}</string>
  <key>ProgramArguments</key>
  <array><string>{}</string><string>supervisor</string><string>run</string></array>
  <key>EnvironmentVariables</key>
  <dict>
{e}  </dict>
  <key>RunAtLoad</key><true/>
  <key>StartInterval</key><integer>{interval}</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{out}</string>
  <key>StandardErrorPath</key><string>{out}</string>
</dict>
</plist>
"#,
        xml(label),
        xml(exe)
    )
}

fn uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid(2) cannot fail.
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn launchctl(args: &[&str]) -> (bool, String) {
    match std::process::Command::new("launchctl").args(args).output() {
        Ok(o) => (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        ),
        Err(e) => (false, e.to_string()),
    }
}

fn install(args: &[String]) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("the supervisor is macOS only for now (Linux would use a systemd user timer)");
    }
    let label = label(args);
    let interval = args
        .windows(2)
        .find(|w| w[0] == "--interval")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(INTERVAL_S);
    let exe = crate::install::real_exe()?;
    let mut env: Vec<(String, String)> = vec![];
    if let Ok(h) = std::env::var("GODTERM_HOME") {
        env.push(("GODTERM_HOME".into(), h));
    }
    for (k, v) in std::env::vars() {
        if k.starts_with("GODTERM_SUPERVISOR_") {
            env.push((k, v));
        }
    }
    let p = plist_path(&label);
    std::fs::create_dir_all(p.parent().context("no LaunchAgents folder")?)?;
    std::fs::create_dir_all(dir())?;
    std::fs::write(&p, plist(&label, &exe.to_string_lossy(), interval, &env))?;
    let domain = format!("gui/{}", uid());
    let _ = launchctl(&["bootout", &format!("{domain}/{label}")]);
    let (ok, out) = launchctl(&["bootstrap", &domain, &p.to_string_lossy()]);
    if !ok {
        bail!("launchctl bootstrap failed: {}", out.trim());
    }
    log(&format!("installed {label} ({})", p.display()));
    println!(
        "Installed {label}: it checks every {interval} s and relaunches GodTerm after an unclean exit.\n  {}\n  launcher: {}",
        p.display(),
        launch_command(&Config::load_or_init().unwrap_or_default().supervisor)
    );
    Ok(())
}

fn uninstall(args: &[String]) -> Result<()> {
    let label = label(args);
    let (_, out) = launchctl(&["bootout", &format!("gui/{}/{label}", uid())]);
    let p = plist_path(&label);
    let had = p.exists();
    let _ = std::fs::remove_file(&p);
    log(&format!("uninstalled {label}"));
    println!(
        "Removed {label}{}{}",
        if had { "" } else { " (no plist was there)" },
        if out.trim().is_empty() {
            String::new()
        } else {
            format!(" ({})", out.trim())
        }
    );
    Ok(())
}

fn status(args: &[String]) -> Result<()> {
    let label = label(args);
    let p = plist_path(&label);
    let (loaded, _) = launchctl(&["print", &format!("gui/{}/{label}", uid())]);
    let cfg = Config::load_or_init().unwrap_or_default().supervisor;
    println!(
        "{label}: {}{}",
        if p.exists() {
            "installed"
        } else {
            "not installed"
        },
        if loaded { ", loaded" } else { "" }
    );
    println!(
        "supervisor.enabled: {}, hang restart: {} ({} s)",
        cfg.enabled,
        cfg.hang_restart,
        hang_s(&cfg)
    );
    println!("launcher: {}", launch_command(&cfg));
    match crate::instance::running() {
        Some(h) if h.pid != 0 => println!("GodTerm: running (pid {})", h.pid),
        _ => println!("GodTerm: not running"),
    }
    match load_exit() {
        Some(e) => println!(
            "last exit: {} (pid {})",
            if e.clean {
                "a quit"
            } else {
                "unclean, or still running"
            },
            e.pid
        ),
        None => println!("last exit: no record"),
    }
    let st = load_state();
    let t = now();
    println!(
        "relaunches in the last 10 min: {}",
        st.relaunches.iter().filter(|r| t - **r < WINDOW_S).count()
    );
    Ok(())
}

/// `godterm supervisor install|uninstall|status|run`.
pub fn command(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("run") => run_once(),
        Some("install") => install(&args[1..]),
        Some("uninstall") => uninstall(&args[1..]),
        Some("status") | None => status(&args[1..]),
        Some(o) => bail!("unknown: supervisor {o} (install, uninstall, status or run)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SupervisorCfg;

    fn seen(running: Option<u32>, answered: bool, clean: Option<bool>) -> Seen {
        Seen {
            running,
            answered,
            last_exit: clean.map(|c| LastExit {
                clean: c,
                pid: 1,
                at: 0,
            }),
        }
    }

    #[test]
    fn relaunches_only_after_an_unclean_exit_and_at_most_three_times() {
        let cfg = SupervisorCfg::default();
        let mut st = State::default();
        let t = 10_000;
        assert!(matches!(
            decide(&cfg, &seen(None, false, Some(true)), &mut st, t),
            Action::Nothing(_)
        ));
        assert!(matches!(
            decide(&cfg, &seen(None, false, None), &mut st, t),
            Action::Nothing(_)
        ));
        assert_eq!(
            decide(&cfg, &seen(None, false, Some(false)), &mut st, t),
            Action::Relaunch
        );
        // Still starting: not twice.
        assert!(matches!(
            decide(&cfg, &seen(None, false, Some(false)), &mut st, t + 20),
            Action::Nothing(_)
        ));
        assert_eq!(
            decide(&cfg, &seen(None, false, Some(false)), &mut st, t + 60),
            Action::Relaunch
        );
        assert_eq!(
            decide(&cfg, &seen(None, false, Some(false)), &mut st, t + 120),
            Action::Relaunch
        );
        // A fourth within 10 minutes: stopped, the notice once.
        assert_eq!(
            decide(&cfg, &seen(None, false, Some(false)), &mut st, t + 180),
            Action::RateLimited(true)
        );
        assert_eq!(
            decide(&cfg, &seen(None, false, Some(false)), &mut st, t + 200),
            Action::RateLimited(false)
        );
        // The window passes: allowed again.
        assert_eq!(
            decide(&cfg, &seen(None, false, Some(false)), &mut st, t + 700),
            Action::Relaunch
        );
        // Off: nothing.
        let off = SupervisorCfg {
            enabled: false,
            ..Default::default()
        };
        assert!(matches!(
            decide(
                &off,
                &seen(None, false, Some(false)),
                &mut State::default(),
                t
            ),
            Action::Nothing(_)
        ));
    }

    #[test]
    fn a_silent_control_socket_restarts_after_the_threshold() {
        let cfg = SupervisorCfg {
            hang_s: 120,
            ..Default::default()
        };
        let mut st = State::default();
        assert!(matches!(
            decide(&cfg, &seen(Some(7), true, Some(false)), &mut st, 0),
            Action::Nothing(_)
        ));
        assert_eq!(
            decide(&cfg, &seen(Some(7), false, Some(false)), &mut st, 100),
            Action::HangPending(0)
        );
        assert_eq!(
            decide(&cfg, &seen(Some(7), false, Some(false)), &mut st, 200),
            Action::HangPending(100)
        );
        // It answered again: the count starts over.
        decide(&cfg, &seen(Some(7), true, Some(false)), &mut st, 210);
        assert_eq!(
            decide(&cfg, &seen(Some(7), false, Some(false)), &mut st, 220),
            Action::HangPending(0)
        );
        assert_eq!(
            decide(&cfg, &seen(Some(7), false, Some(false)), &mut st, 340),
            Action::Restart(7)
        );
        // Hang restarts off: a silent socket is left alone.
        let off = SupervisorCfg {
            hang_restart: false,
            ..Default::default()
        };
        assert!(matches!(
            decide(
                &off,
                &seen(Some(7), false, Some(false)),
                &mut State::default(),
                0
            ),
            Action::Nothing(_)
        ));
    }

    #[test]
    fn marks_and_plist() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("godterm-sup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        crate::config::testing::set_home(&home);
        std::fs::create_dir_all(&home).unwrap();
        mark_running();
        assert!(!load_exit().unwrap().clean);
        mark_clean();
        assert!(load_exit().unwrap().clean);
        assert!(!take_relaunched());
        std::fs::create_dir_all(dir()).unwrap();
        std::fs::write(relaunched_path(), "1").unwrap();
        assert!(take_relaunched() && !take_relaunched());
        let p = plist(
            "com.godterm.supervisor.test",
            "/x/godterm",
            20,
            &[("GODTERM_HOME".into(), "/h & h".into())],
        );
        assert!(p.contains("<string>com.godterm.supervisor.test</string>"));
        assert!(p.contains(
            "<string>/x/godterm</string><string>supervisor</string><string>run</string>"
        ));
        assert!(p.contains("<key>StartInterval</key><integer>20</integer>"));
        assert!(p.contains("<string>/h &amp; h</string>"));
        // The launcher: the setting, else GodTerm.app.
        let c = crate::config::SupervisorCfg {
            launch: "tmux new -d x".into(),
            ..Default::default()
        };
        assert_eq!(launch_command(&c), "tmux new -d x");
        assert!(launch_command(&Default::default()).starts_with("open '"));
        let _ = std::fs::remove_dir_all(home);
    }
}
