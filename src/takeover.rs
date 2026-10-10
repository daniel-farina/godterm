//! Bringing a session that runs somewhere else (a claude or grok in
//! another terminal, on any config dir) into GodTerm.
//!
//! Detection: Claude Code writes `<config>/sessions/<pid>.json` for each
//! interactive process (sessionId, cwd, status busy / idle / shell); grok
//! keeps `<GROK_HOME>/active_sessions.json` (session_id, pid, cwd). Only
//! live processes that really are claude or grok, and not GodTerm's own
//! tabs, count.
//!
//! Take over: wait until it is idle (or interrupt it), stop it gracefully
//! (SIGINT, a second SIGINT, then SIGTERM; never SIGKILL), wait for the
//! transcript to settle, copy it into the target account, resume it there
//! in the same folder, and set its loops up again (cron jobs live only in
//! the old process's memory).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::app::App;
use crate::harness::Harness;

#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub pid: u32,
    pub harness: Harness,
    pub session_id: String,
    pub cwd: PathBuf,
    /// The config dir (CLAUDE_CONFIG_DIR or GROK_HOME) it runs on.
    pub home: PathBuf,
    pub tty: Option<String>,
    /// busy, idle or shell (claude's own word), else "running".
    pub status: String,
    pub name: Option<String>,
    /// iTerm, Terminal, tmux... when it can be told.
    pub terminal: Option<String>,
}

impl Live {
    pub fn describe(&self) -> String {
        let place = match (&self.terminal, &self.tty) {
            (Some(t), Some(y)) => format!("{t} {y}"),
            (None, Some(y)) => y.clone(),
            (Some(t), None) => t.clone(),
            _ => "another terminal".into(),
        };
        format!("{} in {place} (pid {})", self.harness.name(), self.pid)
    }
}

pub fn alive(pid: u32) -> bool {
    crate::platform::alive(pid)
}

/// One field of a process, as `ps -o` names them (args, ppid, comm, tty).
#[cfg(windows)]
fn ps(pid: u32, field: &str) -> Option<String> {
    let p = crate::procs::process(pid)?;
    match field {
        "args" => Some(p.command_line()),
        "ppid" => Some(p.ppid.to_string()),
        "comm" => Some(p.name),
        _ => None,
    }
}

/// One field of a process (`ps -o FIELD= -p PID`), at most 1 s: the
/// take-over checks run from the screen's thread.
#[cfg(unix)]
fn ps(pid: u32, field: &str) -> Option<String> {
    let o = crate::procs::output_timeout(
        std::process::Command::new("ps").args(["-o", &format!("{field}="), "-p", &pid.to_string()]),
        std::time::Duration::from_secs(1),
    )
    .ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// True when `pid` runs claude or grok (by its program name).
pub fn is_agent(pid: u32, h: Harness) -> bool {
    let Some(args) = ps(pid, "args") else {
        return false;
    };
    let mut words = args.split_whitespace();
    let mut prog = words.next().unwrap_or("").trim_matches('"');
    // Run through an interpreter (npm's node install, a wrapper script).
    if matches!(
        crate::procs::program_name(prog).as_str(),
        "node" | "bun" | "sh" | "bash" | "zsh" | "cmd"
    ) {
        prog = words.next().unwrap_or("").trim_matches('"');
    }
    let base = crate::procs::program_name(prog);
    let p = prog.replace('\\', "/").to_lowercase();
    match h {
        Harness::Claude => {
            base == "claude"
                || p.contains("/claude/versions/")
                || p.contains("@anthropic-ai/claude-code/cli.js")
        }
        Harness::Grok => base == "grok" || p.contains("/.grok/bin/"),
        _ => false,
    }
}

/// `ps -o tty` for "no terminal": "??" on macOS, "?" on Linux.
fn has_tty(t: &str) -> bool {
    !matches!(t.trim(), "" | "?" | "??" | "-")
}

/// After interrupt number `sent` (0 or 1): the next step. Delivered: the
/// next interrupt; not deliverable: straight to the stop (`terminate`,
/// forced on Windows), since waiting would change nothing.
pub fn after_interrupt(sent: u8, delivered: bool) -> u8 {
    if delivered {
        sent + 1
    } else {
        INTERRUPT_GIVE_UP
    }
}

/// The step of the Interrupt stage that terminates.
pub const INTERRUPT_GIVE_UP: u8 = 2;

/// The terminal app an agent runs in, from its parent chain.
pub fn terminal_of(pid: u32) -> Option<String> {
    let mut p = pid;
    for _ in 0..12 {
        let pp: u32 = ps(p, "ppid")?.parse().ok()?;
        if pp <= 1 {
            return None;
        }
        let comm = ps(pp, "comm").unwrap_or_default();
        for (needle, name) in [
            ("iTerm", "iTerm"),
            ("Terminal.app", "Terminal"),
            ("tmux", "tmux"),
            ("WezTerm", "WezTerm"),
            ("Ghostty", "Ghostty"),
            ("kitty", "kitty"),
            ("Alacritty", "Alacritty"),
            ("gnome-terminal", "GNOME Terminal"),
            ("konsole", "Konsole"),
            ("xterm", "xterm"),
            ("godterm", "GodTerm"),
            ("WindowsTerminal", "Windows Terminal"),
            ("powershell", "PowerShell"),
            ("pwsh", "PowerShell"),
            ("Code.exe", "VS Code"),
        ] {
            if comm.contains(needle) {
                return Some(name.to_string());
            }
        }
        p = pp;
    }
    None
}

/// Live sessions of these homes, leaving out GodTerm's own processes.
pub fn find(homes: &[(Harness, PathBuf)], ours: &std::collections::HashSet<u32>) -> Vec<Live> {
    let mut out = vec![];
    for (h, home) in homes {
        if !h.integrated() {
            continue;
        }
        match h {
            Harness::Claude => {
                let Ok(rd) = std::fs::read_dir(home.join("sessions")) else {
                    continue;
                };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) != Some("json") {
                        continue;
                    }
                    let Ok(t) = std::fs::read_to_string(&p) else {
                        continue;
                    };
                    let Ok(v) = serde_json::from_str::<Value>(&t) else {
                        continue;
                    };
                    let (Some(pid), Some(sid)) =
                        (v["pid"].as_u64().map(|x| x as u32), v["sessionId"].as_str())
                    else {
                        continue;
                    };
                    if v["kind"].as_str().is_some_and(|k| k != "interactive")
                        || ours.contains(&pid)
                        || !alive(pid)
                        || !is_agent(pid, *h)
                    {
                        continue;
                    }
                    out.push(Live {
                        pid,
                        harness: *h,
                        session_id: sid.to_string(),
                        cwd: PathBuf::from(v["cwd"].as_str().unwrap_or("")),
                        home: home.clone(),
                        tty: ps(pid, "tty").filter(|t| has_tty(t)),
                        status: v["status"].as_str().unwrap_or("running").to_string(),
                        name: v["name"].as_str().map(str::to_string),
                        terminal: None,
                    });
                }
            }
            Harness::Grok => {
                let Ok(t) = std::fs::read_to_string(home.join("active_sessions.json")) else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                for e in v.as_array().into_iter().flatten() {
                    let (Some(pid), Some(sid)) = (
                        e["pid"].as_u64().map(|x| x as u32),
                        e["session_id"].as_str(),
                    ) else {
                        continue;
                    };
                    if ours.contains(&pid) || !alive(pid) || !is_agent(pid, *h) {
                        continue;
                    }
                    out.push(Live {
                        pid,
                        harness: *h,
                        session_id: sid.to_string(),
                        cwd: PathBuf::from(e["cwd"].as_str().unwrap_or("")),
                        home: home.clone(),
                        tty: ps(pid, "tty").filter(|t| has_tty(t)),
                        status: "running".into(),
                        name: None,
                        terminal: None,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// The transcript of a live session (claude: the jsonl; grok: its folder).
pub fn transcript_of(l: &Live) -> Option<PathBuf> {
    match l.harness {
        Harness::Claude => {
            let guess = l
                .home
                .join("projects")
                .join(crate::state::encode_project_dir(&l.cwd))
                .join(format!("{}.jsonl", l.session_id));
            if guess.is_file() {
                return Some(guess);
            }
            std::fs::read_dir(l.home.join("projects"))
                .ok()?
                .flatten()
                .map(|p| p.path().join(format!("{}.jsonl", l.session_id)))
                .find(|p| p.is_file())
        }
        Harness::Grok => std::fs::read_dir(l.home.join("sessions"))
            .ok()?
            .flatten()
            .map(|p| p.path().join(&l.session_id))
            .find(|p| p.join("summary.json").is_file()),
        _ => None,
    }
}

/// Size and mtime of a transcript (for grok, its newest file).
pub fn stamp(p: &Path) -> (u64, SystemTime) {
    let one = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| (m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
            .unwrap_or((0, SystemTime::UNIX_EPOCH))
    };
    if p.is_dir() {
        std::fs::read_dir(p)
            .map(|r| {
                r.flatten()
                    .map(|e| one(&e.path()))
                    .fold((0, SystemTime::UNIX_EPOCH), |a, b| {
                        (a.0 + b.0, a.1.max(b.1))
                    })
            })
            .unwrap_or((0, SystemTime::UNIX_EPOCH))
    } else {
        one(p)
    }
}

/// Mid turn: its own status says busy, or it wrote in the last few seconds.
pub fn working(l: &Live, transcript: Option<&Path>) -> bool {
    if l.status == "busy" {
        return true;
    }
    transcript.is_some_and(|t| stamp(t).1.elapsed().unwrap_or_default() < Duration::from_secs(4))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Copy and resume here; the original keeps running (two branches).
    Copy,
    /// Stop it there, continue here.
    Take,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    WaitIdle(Instant),
    Interrupt {
        at: Instant,
        sent: u8,
    },
    Flush {
        at: Instant,
        last: (u64, SystemTime),
    },
    Resume,
    WaitTab {
        at: Instant,
        uid: u64,
    },
    Loops {
        at: Instant,
        uid: u64,
        before: u64,
    },
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct TakeOver {
    pub live: Live,
    pub target: usize,
    pub mode: Mode,
    /// When it is mid turn: interrupt it (else wait until idle).
    #[allow(dead_code)]
    pub interrupt: bool,
    pub close_terminal: bool,
    pub transcript: PathBuf,
    pub loops: Vec<crate::loops::Loop>,
    pub stage: Stage,
    pub started: Instant,
    /// Rename the tab to this once it is open (take_over_session name).
    pub name: Option<String>,
}

/// A transcript from byte `from` on.
fn read_from(p: &Path, from: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(p) else {
        return String::new();
    };
    if f.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s
}

/// The prompt that makes sure the loops are armed in the resumed
/// session. Recent claude brings session cron jobs back on --resume by
/// itself ("resurrected N session cron task(s)"), older ones do not: so
/// it checks with CronList first and recreates only what is missing (no
/// duplicates either way).
pub fn recreate_prompt(loops: &[crate::loops::Loop]) -> String {
    let mut jobs = vec![];
    let mut dynamic = vec![];
    for l in loops {
        match (&l.kind, &l.cron) {
            (crate::loops::Kind::Cron, Some(c)) => jobs.push(format!(
                "job {} (cron \"{c}\", {}, prompt {})",
                l.id,
                if l.recurring { "recurring" } else { "one shot" },
                serde_json::to_string(&l.prompt).unwrap_or_default()
            )),
            _ => dynamic.push(l.prompt.clone()),
        }
    }
    let mut p = String::from("This session was moved to a new process. ");
    if !jobs.is_empty() {
        p.push_str(&format!("Run CronList. For each of these scheduled jobs that is not listed, recreate it with CronCreate exactly: {}. ", jobs.join("; ")));
    }
    for d in dynamic {
        p.push_str(&format!(
            "Also continue the dynamic loop \"{d}\" with ScheduleWakeup. "
        ));
    }
    p.push_str("Reply only \"ok\".");
    p
}

/// Loops of `loops` now armed in the transcript text written after the
/// check: listed by CronList (same id) or created again.
pub fn armed_after(text: &str, loops: &[crate::loops::Loop]) -> usize {
    let created = text.matches("\"name\":\"CronCreate\"").count();
    let listed = loops.iter().filter(|l| text.contains(&l.id)).count();
    let answered = text.contains("\"name\":\"CronList\"") || created > 0;
    if !answered {
        return 0;
    }
    (listed + created).min(loops.len())
}

impl App {
    /// Every config dir sessions may run on: the accounts, ~/.claude, ~/.grok.
    pub fn agent_homes(&self) -> Vec<(Harness, PathBuf)> {
        let mut v: Vec<(Harness, PathBuf)> = self
            .cfg
            .accounts
            .iter()
            .map(|a| (a.harness(), a.config_dir()))
            .collect();
        // Tests never look at (let alone signal) the user's own sessions.
        if crate::config::dirs().mains {
            v.push((Harness::Claude, crate::session_ops::main_dir()));
            v.push((Harness::Grok, Harness::Grok.main_home()));
        }
        v
    }

    /// GodTerm's own agent processes (tabs and the assistant).
    pub fn own_pids(&self) -> std::collections::HashSet<u32> {
        let mut s: std::collections::HashSet<u32> = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| t.pid())
            .collect();
        if let Some(b) = &self.assistant.brain {
            s.insert(b.pid());
        }
        s
    }

    pub fn live_sessions(&self) -> Vec<Live> {
        find(&self.agent_homes(), &self.own_pids())
    }

    /// Start bringing `live` here onto account `target`.
    pub fn start_takeover(
        &mut self,
        live: Live,
        target: usize,
        mode: Mode,
        interrupt: bool,
        close_terminal: bool,
    ) -> Result<String, String> {
        if self.own_pids().contains(&live.pid) {
            return Err("that session is already in GodTerm".into());
        }
        if self.cfg.accounts[target].harness() != live.harness {
            return Err(format!(
                "a {} session goes to a {} account",
                live.harness.name(),
                live.harness.name()
            ));
        }
        let transcript = transcript_of(&live).ok_or("its transcript was not found")?;
        // Loops it has now (they live only in that process).
        let mut sc = crate::loops::Scanner::default();
        if live.harness == Harness::Claude {
            sc.update(&transcript);
        }
        let now = chrono::Utc::now();
        let loops: Vec<crate::loops::Loop> = sc
            .loops()
            .iter()
            .filter(|l| l.active(now))
            .cloned()
            .collect();
        crate::log::info(&format!(
            "takeover: {} session {} on {} -> {} ({:?}, {} loop(s))",
            live.describe(),
            live.session_id,
            live.home.display(),
            self.cfg.accounts[target].name,
            mode,
            loops.len()
        ));
        let stage = match mode {
            Mode::Copy => Stage::Resume,
            Mode::Take if working(&live, Some(&transcript)) && !interrupt => {
                Stage::WaitIdle(Instant::now())
            }
            Mode::Take => Stage::Interrupt {
                at: Instant::now() - Duration::from_secs(10),
                sent: 0,
            },
        };
        let msg = match &stage {
            Stage::WaitIdle(_) => "waiting for it to finish its turn".to_string(),
            Stage::Resume => {
                "copying it here (the original keeps running: they will diverge)".to_string()
            }
            _ => "stopping it there".to_string(),
        };
        self.takeovers.push(TakeOver {
            live,
            target,
            mode,
            interrupt,
            close_terminal,
            transcript,
            loops,
            stage,
            started: Instant::now(),
            name: None,
        });
        self.takeover_tick();
        Ok(msg)
    }

    /// Move every take-over along.
    pub fn takeover_tick(&mut self) {
        let mut ts = std::mem::take(&mut self.takeovers);
        let mut forced: Vec<String> = vec![];
        for t in &mut ts {
            let pid = t.live.pid;
            let next = match t.stage.clone() {
                Stage::WaitIdle(since) => {
                    if !alive(pid) {
                        Some(Stage::Flush {
                            at: Instant::now(),
                            last: stamp(&t.transcript),
                        })
                    } else if t.transcript_status_idle()
                        && stamp(&t.transcript).1.elapsed().unwrap_or_default()
                            > Duration::from_secs(4)
                    {
                        Some(Stage::Interrupt {
                            at: Instant::now() - Duration::from_secs(10),
                            sent: 0,
                        })
                    } else if since.elapsed() > Duration::from_secs(1800) {
                        Some(Stage::Failed(
                            "it stayed busy for 30 minutes; nothing was stopped".into(),
                        ))
                    } else {
                        None
                    }
                }
                Stage::Interrupt { at, sent } => {
                    if !alive(pid) {
                        crate::log::info(&format!(
                            "takeover: pid {pid} exited after {sent} signal(s)"
                        ));
                        Some(Stage::Flush {
                            at: Instant::now(),
                            last: stamp(&t.transcript),
                        })
                    } else {
                        // SIGINT stops a turn, a second one exits; SIGTERM
                        // after 3 s; never SIGKILL.
                        let wait = match sent {
                            0 => Duration::ZERO,
                            1 => Duration::from_millis(700),
                            2 => Duration::from_secs(3),
                            _ => Duration::from_secs(8),
                        };
                        if at.elapsed() >= wait {
                            match sent {
                                0 | 1 => {
                                    let ok = crate::procs::interrupt(pid);
                                    crate::log::info(&format!(
                                        "takeover: Ctrl-C (SIGINT) to pid {pid}{}",
                                        if ok { "" } else { " (could not deliver it: no console to reach; stopping it by force)" }
                                    ));
                                    // Not deliverable: the next tick goes to the
                                    // confirmed stop at once (no point waiting).
                                    let next = after_interrupt(sent, ok);
                                    Some(Stage::Interrupt {
                                        at: if ok { Instant::now() } else { Instant::now() - Duration::from_secs(10) },
                                        sent: next,
                                    })
                                }
                                2 => {
                                    // Unix: SIGTERM, it can still clean up.
                                    // Windows has no such step: this is
                                    // TerminateProcess, after the confirmed
                                    // take-over, and the reply says forced.
                                    crate::log::info(&format!(
                                        "takeover: {} pid {pid}",
                                        if crate::procs::TERMINATE_IS_FORCED {
                                            "forced stop (TerminateProcess) of"
                                        } else {
                                            "SIGTERM to"
                                        }
                                    ));
                                    crate::procs::terminate(pid);
                                    if crate::procs::TERMINATE_IS_FORCED {
                                        forced.push(t.live.session_id.clone());
                                    }
                                    Some(Stage::Interrupt { at: Instant::now(), sent: 3 })
                                }
                                _ => Some(Stage::Failed(format!("pid {pid} did not stop after SIGINT twice and SIGTERM; not killing it (stop it there and try again)"))),
                            }
                        } else {
                            None
                        }
                    }
                }
                Stage::Flush { at, last } => {
                    // Settled: same size and time for a second.
                    let now = stamp(&t.transcript);
                    if now != last {
                        Some(Stage::Flush {
                            at: Instant::now(),
                            last: now,
                        })
                    } else if at.elapsed() >= Duration::from_millis(1000) {
                        Some(Stage::Resume)
                    } else {
                        None
                    }
                }
                Stage::Resume => Some(match self.takeover_resume(t) {
                    Ok(uid) => Stage::WaitTab {
                        at: Instant::now(),
                        uid,
                    },
                    Err(e) => Stage::Failed(e),
                }),
                Stage::WaitTab { at, uid } => match self.find_tab(uid) {
                    Some((s, ti))
                        if self.panes[s].tabs[ti].is_running()
                            && self.panes[s].tabs[ti].activity == crate::pane::Activity::Ready =>
                    {
                        // The rename asked with the take-over, now that the tab is here.
                        if let Some(n) = t.name.take() {
                            self.rename_tab(s, ti, &n);
                        }
                        if t.loops.is_empty() || t.live.harness != Harness::Claude {
                            Some(Stage::Done("resumed here".into()))
                        } else {
                            let target_t = self
                                .transcript_path(s, ti)
                                .unwrap_or_else(|| t.transcript.clone());
                            let before = std::fs::metadata(&target_t).map(|m| m.len()).unwrap_or(0);
                            crate::log::info(&format!(
                                "takeover: checking {} loop(s) in the resumed session",
                                t.loops.len()
                            ));
                            self.deliver(uid, &recreate_prompt(&t.loops));
                            Some(Stage::Loops {
                                at: Instant::now(),
                                uid,
                                before,
                            })
                        }
                    }
                    None => Some(Stage::Failed("the resumed tab closed".into())),
                    _ if at.elapsed() > Duration::from_secs(60) => Some(Stage::Failed(
                        "the resumed tab did not come up in a minute".into(),
                    )),
                    _ => None,
                },
                Stage::Loops { at, uid, before } => {
                    let path = self
                        .find_tab(uid)
                        .and_then(|(s, ti)| self.transcript_path(s, ti));
                    let text = path
                        .as_deref()
                        .map(|p| read_from(p, before))
                        .unwrap_or_default();
                    let n = armed_after(&text, &t.loops);
                    let replied =
                        text.contains("\"type\":\"assistant\"") && text.contains("\"text\":\"ok");
                    if n >= t.loops.len() && (replied || at.elapsed() > Duration::from_secs(20)) {
                        Some(Stage::Done(format!(
                            "resumed here; loops restored: {}",
                            t.loops.len()
                        )))
                    } else if at.elapsed() > Duration::from_secs(150) {
                        Some(Stage::Done(format!("resumed here; loops restored: {n} of {} (check Loops and set the rest up again)", t.loops.len())))
                    } else {
                        None
                    }
                }
                Stage::Done(_) | Stage::Failed(_) => None,
            };
            if let Some(n) = next {
                match &n {
                    Stage::Done(m) => {
                        crate::log::info(&format!("takeover: {}: {m}", t.live.session_id));
                        let msg = format!(
                            "{} {m}",
                            if t.mode == Mode::Copy {
                                "Copied:"
                            } else {
                                "Took over:"
                            }
                        );
                        self.flash(msg.clone());
                        self.assistant_note(msg);
                        if t.close_terminal {
                            close_terminal_tab(&t.live);
                        }
                    }
                    Stage::Failed(m) => {
                        crate::log::info(&format!("takeover: {} failed: {m}", t.live.session_id));
                        self.flash(format!("Take over failed: {m}"));
                        self.assistant_note(format!("Take over failed: {m}"));
                    }
                    _ => {}
                }
                t.stage = n;
            }
        }
        ts.retain(|t| {
            !matches!(t.stage, Stage::Done(_) | Stage::Failed(_))
                || t.started.elapsed() < Duration::from_secs(600)
        });
        self.takeovers = ts;
        for sid in forced {
            self.flash("Stopped a session by force: it did not answer Ctrl-C (Windows has no gentler step)");
            self.forced_stops.push(sid);
        }
    }

    /// Copy the settled transcript into the target and resume it there.
    fn takeover_resume(&mut self, t: &TakeOver) -> Result<u64, String> {
        let to = self.cfg.accounts[t.target].config_dir();
        if to != t.live.home {
            match t.live.harness {
                Harness::Claude => {
                    crate::session_ops::copy_session(
                        &t.live.home,
                        &t.transcript,
                        &t.live.session_id,
                        &to,
                        crate::session_ops::Conflict::Overwrite,
                    )
                    .map_err(|e| format!("copy: {e:#}"))?;
                }
                Harness::Grok => {
                    let rel = t
                        .transcript
                        .strip_prefix(&t.live.home)
                        .map_err(|_| "grok session outside its home")?;
                    let _ = std::fs::remove_dir_all(to.join(rel));
                    crate::harness::grok::copy_session(
                        &t.transcript,
                        &t.live.home,
                        &to,
                        false,
                        &crate::config::app_home().join("trash"),
                    )
                    .map_err(|e| format!("copy: {e:#}"))?;
                }

                _ => return Err("No transcript adapter for this CLI".into()),
            }
        }
        // Resume in the same folder.
        let info = crate::sessions::SessionInfo {
            id: t.live.session_id.clone(),
            cwd: t.live.cwd.to_string_lossy().into_owned(),
            ..Default::default()
        };
        if let Some(st) = self.accounts.get_mut(t.target) {
            st.sessions.retain(|s| s.id != info.id);
            st.sessions.insert(0, info);
        }
        self.resume(t.target, t.live.session_id.clone());
        let uid = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .find(|x| x.session_id.as_deref() == Some(&t.live.session_id))
            .map(|x| x.uid)
            .ok_or("no tab opened")?;
        if let Some((s, ti)) = self.find_tab(uid) {
            let from = t.live.describe();
            self.tab_event(s, ti, "takeover", None, Some(format!("from {from}")));
        }
        self.last_target = Some(uid);
        Ok(uid)
    }
}

impl TakeOver {
    /// claude's own status file says idle (or there is none).
    fn transcript_status_idle(&self) -> bool {
        let f = self
            .live
            .home
            .join("sessions")
            .join(format!("{}.json", self.live.pid));
        std::fs::read_to_string(f)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["status"].as_str().map(|s| s != "busy"))
            .unwrap_or(true)
    }
}

/// Close the terminal tab it ran in (iTerm or Terminal, by tty).
pub fn close_terminal_tab(l: &Live) {
    let Some(tty) = &l.tty else { return };
    let dev = format!("/dev/{tty}");
    let script = match l.terminal.as_deref() {
        Some("iTerm") => format!("tell application \"iTerm2\" to repeat with w in windows\nrepeat with t in tabs of w\nrepeat with s in sessions of t\nif tty of s is \"{dev}\" then close s\nend repeat\nend repeat\nend repeat"),
        Some("Terminal") => format!("tell application \"Terminal\" to repeat with w in windows\nrepeat with t in tabs of w\nif tty of t is \"{dev}\" then close t\nend repeat\nend repeat"),
        _ => return,
    };
    if !cfg!(test) {
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .output();
    }
}

#[cfg(test)]
mod interrupt_tests {
    /// An undeliverable interrupt (no console on Windows) goes straight
    /// to the stop; a delivered one tries once more first.
    #[test]
    fn undeliverable_interrupt_goes_to_the_stop() {
        assert_eq!(super::after_interrupt(0, true), 1);
        assert_eq!(super::after_interrupt(1, true), 2);
        assert_eq!(super::after_interrupt(0, false), super::INTERRUPT_GIVE_UP);
    }
}

#[cfg(test)]
mod tty_tests {
    #[test]
    fn no_tty_reads_as_none() {
        for t in ["?", "??", "-", " "] {
            assert!(!super::has_tty(t), "{t}");
        }
        assert!(super::has_tty("ttys004") && super::has_tty("pts/3"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_live_claude_and_grok_but_not_ours() {
        let root = std::env::temp_dir().join(format!("godterm-takeover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let c = root.join("claude");
        let g = root.join("grok");
        std::fs::create_dir_all(c.join("sessions")).unwrap();
        std::fs::create_dir_all(&g).unwrap();
        // A stub "claude" process that sleeps.
        let exe = crate::test_stub::claude(&root.join("bin"), &[("sleep_s", "30".into())]);
        let mut child = crate::test_stub::own_console(&mut std::process::Command::new(&exe))
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(200));
        std::fs::write(c.join("sessions").join(format!("{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"s-1","cwd":"/tmp","status":"idle","kind":"interactive"}}"#)).unwrap();
        // A dead pid and a non agent pid are ignored.
        std::fs::write(c.join("sessions/1.json"), r#"{"pid":999999,"sessionId":"dead","cwd":"/tmp","status":"idle","kind":"interactive"}"#).unwrap();
        let homes = vec![(Harness::Claude, c.clone()), (Harness::Grok, g.clone())];
        let found = find(&homes, &Default::default());
        assert_eq!(
            found
                .iter()
                .map(|l| l.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["s-1"]
        );
        assert_eq!(found[0].status, "idle");
        // GodTerm's own processes never count.
        let ours: std::collections::HashSet<u32> = [pid].into_iter().collect();
        assert!(find(&homes, &ours).is_empty());
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn loop_prompts() {
        let l = crate::loops::Loop {
            id: "j1".into(),
            kind: crate::loops::Kind::Cron,
            cron: Some("*/5 * * * *".into()),
            prompt: "say hello".into(),
            recurring: true,
            durable: false,
            created: None,
            last_fire: None,
            fires: 1,
            wake_at: None,
            deleted: false,
            confidence: crate::loops::Confidence::High,
        };
        let p = recreate_prompt(std::slice::from_ref(&l));
        assert!(
            p.contains("CronList")
                && p.contains("CronCreate")
                && p.contains("*/5 * * * *")
                && p.contains("\"say hello\"")
                && p.contains("recurring")
        );
        // Kept by claude (listed) or created again both count; nothing yet is 0.
        assert_eq!(
            armed_after(
                "{\"name\":\"CronList\"} ... j1 every 5 minutes",
                std::slice::from_ref(&l)
            ),
            1
        );
        assert_eq!(
            armed_after("{\"name\":\"CronCreate\"}", std::slice::from_ref(&l)),
            1
        );
        assert_eq!(armed_after("nothing", std::slice::from_ref(&l)), 0);
    }
}
