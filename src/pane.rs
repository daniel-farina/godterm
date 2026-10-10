//! A pane: one `claude` child in a PTY, rendered through a vt100 parser.

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::AppEvent;

/// Env vars that would make a child use some other identity than its slot.
const SCRUBBED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// True for variables a child claude must not inherit. When godterm is
/// started from inside a Claude Code session, its environment carries
/// CLAUDECODE and CLAUDE_CODE_* markers (CHILD_SESSION, SESSION_ID,
/// MESSAGING_SOCKET, ...) that make every child think it is a subagent:
/// transcripts are switched off and the permission mode is inherited.
/// `pass` lists names to keep anyway (`pass_env` in config.toml).
pub fn should_scrub(key: &str, pass: &[String]) -> bool {
    if key == "CLAUDE_CONFIG_DIR" || pass.iter().any(|p| p == key) {
        return false;
    }
    // A parent grok's markers would make a child grok its subagent.
    SCRUBBED_ENV.contains(&key)
        || key.starts_with("CLAUDE_CODE_")
        // GodTerm's own variables (its home, control socket and token)
        // stay out of tabs: a tab must not drive the others (QA-26).
        || key.starts_with("GODTERM_")
        || key.starts_with("CLAUDEGO_")
        || crate::harness::scrub_grok(key)
        || key == "CODEX_HOME"
        || key.starts_with("CODEX_INTERNAL_")
        || key == "CODEX_THREAD_ID"
}

/// Most bytes a second one tab's output is read at (see the reader).
pub const STREAM_CAP: usize = 4 << 20;
const STREAM_SLICE: std::time::Duration = std::time::Duration::from_millis(100);

/// Lines of history kept per tab. Each line costs about 32 bytes per
/// column, so this bounds memory: 12 tabs x 2000 lines x 110 columns is
/// about 85 MB at worst. Set from `scrollback_lines` in config.toml.
static SCROLLBACK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(2000);

pub fn set_scrollback(lines: usize) {
    SCROLLBACK.store(lines.min(100_000), std::sync::atomic::Ordering::Relaxed);
}

/// Input queued per tab, in writes (from `writer_queue_kb`), and the PTY
/// read buffer in bytes. Both apply to tabs started afterwards.
static WRITER_QUEUE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1024);
static READ_BUF: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(16384);

pub fn set_buffers(writer_queue_kb: usize, read_buffer_kb: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    WRITER_QUEUE.store(writer_queue_kb.clamp(16, 65_536), Relaxed);
    READ_BUF.store(read_buffer_kb.clamp(1, 256) * 1024, Relaxed);
}

pub fn buffers() -> (usize, usize) {
    use std::sync::atomic::Ordering::Relaxed;
    (WRITER_QUEUE.load(Relaxed), READ_BUF.load(Relaxed))
}

fn scrollback() -> usize {
    SCROLLBACK.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaneState {
    /// Nothing spawned yet.
    Idle,
    Running,
    Exited(Option<u32>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LaunchKind {
    Normal,
    Login,
    Resume(String),
}

pub struct LaunchSpec {
    pub program: String,
    /// The variable that names the account's isolated home
    /// (CLAUDE_CONFIG_DIR, GROK_HOME or CODEX_HOME). Empty for native stores.
    pub home_env: &'static str,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub config_dir: PathBuf,
    pub kind: LaunchKind,
    /// Variables to pass through even though they look like Claude Code's.
    pub pass_env: Vec<String>,
    /// Bytes to type into the child once its output goes quiet.
    pub type_when_idle: Option<Vec<u8>>,
    /// More variables for the child (grok's compat switches).
    pub extra_env: Vec<(String, String)>,
}

struct Proc {
    master: Box<dyn MasterPty + Send>,
    /// Bytes for the child, written by a dedicated thread so a child that
    /// stops reading can never block the UI.
    input: std::sync::mpsc::SyncSender<Vec<u8>>,
    child: Box<dyn Child + Send + Sync>,
}

/// What a running claude appears to be doing, read off its screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// No process.
    Idle,
    /// Process running, nothing recognizable yet.
    Starting,
    /// Spinner with "esc to interrupt" visible.
    Working,
    /// A permission prompt ("Do you want to proceed?") is open.
    Permission,
    /// The input box is waiting for a prompt.
    Ready,
    Exited,
}

impl Activity {
    pub fn label(self) -> &'static str {
        match self {
            Activity::Idle => "idle",
            Activity::Starting => "starting",
            Activity::Working => "working",
            Activity::Permission => "needs approval",
            Activity::Ready => "waiting for input",
            Activity::Exited => "exited",
        }
    }
}

/// Classify a claude screen. Pure, so it is easy to test.
pub fn detect_activity(text: &str) -> Activity {
    if let Some(p) = crate::prompt::parse_prompt(text) {
        use crate::prompt::PromptKind;
        match p.kind {
            PromptKind::Permission | PromptKind::Trust | PromptKind::Plan | PromptKind::Bypass => {
                return Activity::Permission
            }
            PromptKind::Other => {}
        }
    }
    let permission = text.contains("No, and tell Claude what to do")
        || (text.contains("Do you want to")
            && (text.contains("1. Yes") || text.contains("❯ Yes") || text.contains("2. No")));
    if permission {
        Activity::Permission
    } else if text.contains("esc to interrupt") || text.contains("Ctrl+c to cancel the turn") {
        Activity::Working
    } else if text.contains("? for shortcuts")
        || text.contains("Type a message")
        || text.contains("│ >")
        || text
            .lines()
            .any(|l| l.trim_start().starts_with('>') || l.trim_start().starts_with('❯'))
    {
        Activity::Ready
    } else {
        Activity::Starting
    }
}

static NEXT_UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The id the next tab gets.
pub fn next_uid() -> u64 {
    NEXT_UID.load(std::sync::atomic::Ordering::Relaxed)
}

/// Never hand out an id below `n` (ids used before a restart).
pub fn reserve_uids_below(n: u64) {
    NEXT_UID.fetch_max(n, std::sync::atomic::Ordering::Relaxed);
}

/// Ids handed out are reserved on disk ahead of use, a block at a time
/// (`<home>/tab-ids`), so one used between two state saves is never
/// handed out again after a crash.
static RESERVED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const RESERVE_BLOCK: u64 = 32;

fn reserve_file() -> PathBuf {
    crate::config::app_home().join("tab-ids")
}

/// The id ceiling a previous run reserved on disk (0: none).
pub fn reserved_on_disk() -> u64 {
    std::fs::read_to_string(reserve_file())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn alloc_uid() -> u64 {
    // The first id of a run starts past what the last run reserved.
    if RESERVED.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        let d = reserved_on_disk();
        NEXT_UID.fetch_max(d, std::sync::atomic::Ordering::Relaxed);
        RESERVED.fetch_max(d.max(1), std::sync::atomic::Ordering::Relaxed);
    }
    let id = NEXT_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if id >= RESERVED.load(std::sync::atomic::Ordering::Relaxed) {
        let ceiling = id + RESERVE_BLOCK;
        RESERVED.fetch_max(ceiling, std::sync::atomic::Ordering::Relaxed);
        let _ = crate::config::write_private(&reserve_file(), ceiling.to_string());
    }
    id
}

pub struct Pane {
    /// Unique across all tabs, used to route PTY events.
    pub uid: u64,
    /// Where the session is now (its process or transcript), when that
    /// differs from `cwd`; refreshed every 2 s while shown.
    pub live_cwd: Option<PathBuf>,
    pub cwd_polled: Option<std::time::Instant>,
    /// Working directory the session runs in.
    pub cwd: PathBuf,
    /// Name given with rename (Ctrl-a , or "rename tab to ...").
    pub custom_name: Option<String>,
    /// Accent color picked from the tab's menu (a palette name).
    pub accent: Option<String>,
    /// Claude session id, known when resumed or detected from transcripts.
    pub session_id: Option<String>,
    /// When the current process started.
    pub started: std::time::SystemTime,
    /// Launch to perform when this tab is first shown (restored tabs).
    pub pending: Option<LaunchKind>,
    pub activity: Activity,
    /// A helper tab (editor, doctor): not saved or restored.
    pub transient: bool,
    /// The trust dialog of this process was already answered.
    pub trust_answered: bool,
    /// Automatic restart scheduled after a crash.
    pub restart_at: Option<Instant>,
    /// Recent crash times, to stop restart loops.
    pub crashes: Vec<Instant>,
    /// When `activity` last changed.
    pub activity_since: Instant,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub state: PaneState,
    pub kind: LaunchKind,
    pub scroll: usize,
    /// Bumped on each spawn so stale exit events can be ignored.
    pub generation: u64,
    size: (u16, u16),
    proc: Option<Proc>,
    last_output: Arc<Mutex<Instant>>,
    pending_type: Option<(Instant, Vec<u8>)>,
    /// History lines kept (see `set_history`).
    history: usize,
    /// Stopped by the memory saver; resumes (with --resume) when shown.
    pub suspended: bool,
    /// Its group in the account's tab list (by name), if any.
    pub group: Option<String>,
    /// Pinned: first in the list, asks before closing, left out of
    /// "close all".
    pub pinned: bool,
    /// When the tab was first opened (kept across restarts).
    pub opened: std::time::SystemTime,
    /// The user's last keystroke or prompt into it.
    pub last_input: Instant,
}

impl Pane {
    pub fn new(cwd: PathBuf) -> Pane {
        Pane {
            live_cwd: None,
            cwd_polled: None,
            uid: alloc_uid(),
            cwd,
            custom_name: None,
            accent: None,
            session_id: None,
            started: std::time::SystemTime::now(),
            pending: None,
            activity: Activity::Idle,
            restart_at: None,
            trust_answered: false,
            transient: false,
            crashes: vec![],
            activity_since: Instant::now(),
            parser: Arc::new(Mutex::new(vt100::Parser::new(24, 80, scrollback()))),
            state: PaneState::Idle,
            kind: LaunchKind::Normal,
            scroll: 0,
            generation: 0,
            size: (24, 80),
            proc: None,
            last_output: Arc::new(Mutex::new(Instant::now())),
            pending_type: None,
            history: scrollback(),
            suspended: false,
            group: None,
            pinned: false,
            opened: std::time::SystemTime::now(),
            last_input: Instant::now(),
        }
    }

    /// The last output, input or state change.
    pub fn last_activity(&self) -> Instant {
        let out = *self.last_output.lock().unwrap_or_else(|e| e.into_inner());
        out.max(self.activity_since).max(self.last_input)
    }

    /// History lines this tab keeps. Changing it rebuilds the screen
    /// parser (vt100 fixes it per parser) and replays the visible screen,
    /// so lowering it frees the old history.
    pub fn set_history(&mut self, lines: usize) {
        if lines == self.history {
            return;
        }
        self.history = lines;
        let mut p = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        let (rows, cols) = p.screen().size();
        let replay = p.screen().contents_formatted();
        let mut fresh = vt100::Parser::new(rows, cols, lines);
        fresh.process(&replay);
        *p = fresh;
        drop(p);
        self.scroll = 0;
    }

    #[cfg(test)]
    pub fn history(&self) -> usize {
        self.history
    }

    /// How to bring this tab back: resume its conversation when known.
    pub fn relaunch_kind(&self) -> LaunchKind {
        match &self.session_id {
            Some(id) => LaunchKind::Resume(id.clone()),
            None => LaunchKind::Normal,
        }
    }

    /// True when the last process ended with a non zero exit code.
    pub fn crashed(&self) -> bool {
        matches!(self.state, PaneState::Exited(Some(c)) if c != 0)
    }

    /// Process id of the running child, if any.
    pub fn pid(&self) -> Option<u32> {
        self.proc.as_ref().and_then(|p| p.child.process_id())
    }

    pub fn is_running(&self) -> bool {
        self.state == PaneState::Running
    }

    pub fn spawn(&mut self, spec: LaunchSpec, events: Sender<AppEvent>) -> Result<()> {
        // A test never starts the test binary itself (as claude, doctor,
        // install...): that would run the suite again, recursively.
        if cfg!(test) && crate::test_guard::is_current_exe(std::path::Path::new(&spec.program)) {
            anyhow::bail!("refused in tests: {} is the test binary", spec.program);
        }
        self.kill();
        self.generation += 1;
        let (rows, cols) = self.size;
        // Fresh screen for the new process.
        self.parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, self.history)));
        self.scroll = 0;
        self.suspended = false;
        self.kind = spec.kind.clone();
        self.cwd = spec.cwd.clone();
        self.started = std::time::SystemTime::now();
        self.pending = None;
        self.trust_answered = false;
        if let LaunchKind::Resume(id) = &spec.kind {
            self.session_id = Some(id.clone());
        }
        self.set_activity(Activity::Starting);

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("opening pty")?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        for (k, _) in std::env::vars_os() {
            if let Some(k) = k.to_str() {
                if should_scrub(k, &spec.pass_env) {
                    cmd.env_remove(k);
                }
            }
        }
        if spec.home_env != "CLAUDE_CONFIG_DIR" {
            cmd.env_remove("CLAUDE_CONFIG_DIR");
        }
        if !spec.home_env.is_empty() {
            cmd.env(spec.home_env, &spec.config_dir);
        }
        for (k, v) in &spec.extra_env {
            cmd.env(k, v);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "godterm");
        let child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                self.state = PaneState::Failed(format!("{}: {e}", spec.program));
                return Err(e).context("spawning claude");
            }
        };
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let (queue, read_buf) = buffers();
        let (input, input_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(queue);
        std::thread::Builder::new()
            .name(format!("pty-in-{}", self.uid))
            .spawn(move || {
                while let Ok(bytes) = input_rx.recv() {
                    if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                        break;
                    }
                }
            })?;

        let parser = Arc::clone(&self.parser);
        let w2 = input.clone();
        let last = Arc::clone(&self.last_output);
        let (id, gen) = (self.uid, self.generation);
        std::thread::Builder::new()
            .name(format!("pty-{id}"))
            .spawn(move || {
                let mut buf = vec![0u8; read_buf];
                // A tab that streams without pause (`yes`, a huge cat) is
                // read at most STREAM_CAP bytes a second: the rest waits in
                // the PTY, which slows the writer down, instead of every
                // core parsing output nobody can read that fast (QA-14).
                let mut window = Instant::now();
                let mut in_window = 0usize;
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            in_window += n;
                            if window.elapsed() >= STREAM_SLICE {
                                window = Instant::now();
                                in_window = n;
                            } else if in_window > STREAM_CAP / 10 {
                                std::thread::sleep(STREAM_SLICE.saturating_sub(window.elapsed()));
                                window = Instant::now();
                                in_window = 0;
                            }
                            let chunk = &buf[..n];
                            let reply = {
                                let mut p = parser.lock().unwrap_or_else(|e| e.into_inner());
                                p.process(chunk);
                                query_replies(chunk, p.screen())
                            };
                            if !reply.is_empty() {
                                let _ = w2.try_send(reply);
                            }
                            *last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                            // One wake up per frame is enough; coalesce.
                            if !OUTPUT_PENDING.swap(true, std::sync::atomic::Ordering::AcqRel)
                                && events.send(AppEvent::PaneOutput).is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                let _ = events.send(AppEvent::PaneExited(id, gen));
            })?;

        *self.last_output.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.pending_type = spec.type_when_idle.map(|b| (Instant::now(), b));
        self.proc = Some(Proc {
            master: pair.master,
            input,
            child,
        });
        self.state = PaneState::Running;
        Ok(())
    }

    /// Called when the reader thread hits EOF.
    pub fn on_exit(&mut self, gen: u64) {
        if gen != self.generation {
            return;
        }
        let code = self.proc.as_mut().and_then(|p| {
            // Give the child a moment to be reaped.
            for _ in 0..20 {
                if let Ok(Some(s)) = p.child.try_wait() {
                    return Some(s.exit_code());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        });
        if let Some(p) = self.proc.take() {
            if code.is_none() {
                reap(p.child);
            }
        }
        self.pending_type = None;
        self.state = PaneState::Exited(code);
        self.set_activity(Activity::Exited);
    }

    pub fn kill(&mut self) {
        if let Some(p) = self.proc.take() {
            // SIGHUP now (cheap), then wait and escalate off the UI thread:
            // portable-pty's own kill() sleeps up to 200 ms per child.
            if let Some(pid) = p.child.process_id() {
                crate::platform::kill(pid as i32, crate::platform::SIGHUP);
            }
            let Proc {
                master,
                input,
                child,
            } = p;
            drop(input);
            reap_after_hup(child, master);
        }
        self.pending_type = None;
        if self.state == PaneState::Running {
            self.state = PaneState::Exited(None);
            self.set_activity(Activity::Exited);
        }
    }

    pub fn set_activity(&mut self, a: Activity) {
        if self.activity != a {
            self.activity = a;
            self.activity_since = Instant::now();
        }
    }

    /// Re-read the screen and classify it. Returns (old, new) on change.
    pub fn update_activity(&mut self) -> Option<(Activity, Activity)> {
        if self.state != PaneState::Running {
            return None;
        }
        let text = self
            .parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .contents();
        let next = detect_activity(&text);
        let old = self.activity;
        if next != old {
            self.set_activity(next);
            Some((old, next))
        } else {
            None
        }
    }

    /// Short tab name: last path component of the cwd.
    pub fn name(&self) -> String {
        if let Some(n) = self.custom_name.as_ref().filter(|n| !n.trim().is_empty()) {
            return n.clone();
        }
        self.cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "~".into())
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.last_input = Instant::now();
        if let Some(p) = &self.proc {
            // Never blocks: if the child has stopped reading and 1024 writes
            // are queued, further input is dropped.
            let _ = p.input.try_send(bytes.to_vec());
        }
    }

    /// The PTY's size (rows, cols).
    #[cfg(test)]
    pub fn pty_size(&self) -> (u16, u16) {
        self.size
    }

    /// Resize the PTY and the virtual screen to the pane's inner area.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        self.parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_size(rows, cols);
        if let Some(p) = &self.proc {
            let _ = p.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let mut p = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        // From where the view is: vt100 moves it up as output arrives, to
        // keep the same lines in view.
        let next = (p.screen().scrollback() as isize + delta).max(0) as usize;
        p.screen_mut().set_scrollback(next);
        // vt100 clamps to the available history.
        self.scroll = p.screen().scrollback();
    }

    pub fn reset_scroll(&mut self) {
        if self.scroll != 0 {
            self.scroll = 0;
            self.parser
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .screen_mut()
                .set_scrollback(0);
        }
    }

    pub fn app_cursor(&self) -> bool {
        self.parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .application_cursor()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .bracketed_paste()
    }

    /// Type queued input once the child has been quiet for a moment.
    pub fn tick(&mut self) {
        let Some((started, _)) = &self.pending_type else {
            return;
        };
        let quiet = self
            .last_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed();
        let since = started.elapsed();
        if since > Duration::from_millis(1500) && quiet > Duration::from_millis(700)
            || since > Duration::from_secs(8)
        {
            if let Some((_, bytes)) = self.pending_type.take() {
                self.write(&bytes);
            }
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Set by PTY readers when new output arrived and a redraw is pending.
pub static OUTPUT_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

static REAPING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Give a hung up child 400 ms to exit, then SIGKILL it, and reap it.
fn reap_after_hup(mut child: Box<dyn Child + Send + Sync>, master: Box<dyn MasterPty + Send>) {
    REAPING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let spawned = std::thread::Builder::new()
        .name("reaper".into())
        .spawn(move || {
            let t0 = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) if t0.elapsed() < Duration::from_millis(400) => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Ok(None) => {
                        if let Some(pid) = child.process_id() {
                            crate::platform::kill(pid as i32, crate::platform::SIGKILL);
                        }
                        let _ = child.wait();
                        break;
                    }
                }
            }
            drop(master);
            REAPING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    if spawned.is_err() {
        REAPING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Block (at most `limit`) until every killed child has been reaped. Used
/// on shutdown so no child outlives godterm.
pub fn wait_for_reapers(limit: Duration) {
    let t0 = Instant::now();
    while REAPING.load(std::sync::atomic::Ordering::SeqCst) > 0 && t0.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait for a child on a background thread so it never lingers as a zombie.
fn reap(mut child: Box<dyn Child + Send + Sync>) {
    let _ = std::thread::Builder::new()
        .name("reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

/// Answer terminal queries that TUI apps send while probing capabilities.
/// Without replies some programs wait for a timeout before drawing.
pub fn query_replies(chunk: &[u8], screen: &vt100::Screen) -> Vec<u8> {
    let mut out = Vec::new();
    let has = |pat: &[u8]| chunk.windows(pat.len()).any(|w| w == pat);
    if has(b"\x1b[6n") {
        let (r, c) = screen.cursor_position();
        out.extend_from_slice(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
    }
    if has(b"\x1b[5n") {
        out.extend_from_slice(b"\x1b[0n");
    }
    if has(b"\x1b[>0q") || has(b"\x1b[>q") {
        out.extend_from_slice(b"\x1bP>|godterm\x1b\\");
    }
    if has(b"\x1b[>c") || has(b"\x1b[>0c") {
        out.extend_from_slice(b"\x1b[>0;10;1c");
    }
    if has(b"\x1b[c") || has(b"\x1b[0c") {
        out.extend_from_slice(b"\x1b[?62;22c");
    }
    if has(b"\x1b]10;?") {
        out.extend_from_slice(b"\x1b]10;rgb:d4d4/d0d0/c8c8\x1b\\");
    }
    if has(b"\x1b]11;?") {
        out.extend_from_slice(b"\x1b]11;rgb:1c1c/1e1e/2121\x1b\\");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_activity() {
        assert_eq!(
            detect_activity("Bash(rm x)\n Do you want to proceed?\n ❯ 1. Yes\n   2. No, and tell Claude what to do differently (esc)"),
            Activity::Permission
        );
        assert_eq!(
            detect_activity("✻ Pondering... (12s · esc to interrupt)\n> \n"),
            Activity::Working
        );
        assert_eq!(
            detect_activity("──────\n> \n──────\n  ? for shortcuts"),
            Activity::Ready
        );
        assert_eq!(
            detect_activity("Welcome to Claude Code"),
            Activity::Starting
        );
    }

    #[test]
    fn scrollback_is_bounded() {
        // A flood of output keeps at most the configured history.
        let mut p = vt100::Parser::new(10, 40, 100);
        for i in 0..20_000 {
            p.process(format!("line {i} \x1b[31mred\x1b[0m\r\n").as_bytes());
        }
        p.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(p.screen().scrollback(), 100);
    }

    #[test]
    fn scrubs_inherited_claude_code_markers() {
        let none: Vec<String> = vec![];
        for k in [
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_EXECPATH",
            "CLAUDE_CODE_SESSION_ATTENDED",
            "CLAUDE_CODE_VERSION",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_PID",
            "CLAUDE_EFFORT",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "GODTERM_HOME",
            "GODTERM_TOKEN",
            "CLAUDEGO_HOME",
        ] {
            assert!(should_scrub(k, &none), "{k} must be removed");
        }
        for k in [
            "CLAUDE_CONFIG_DIR",
            "PATH",
            "HOME",
            "TERM",
            "ANTHROPIC_BASE_URL_X",
            "CLAUDE",
        ] {
            assert!(!should_scrub(k, &none), "{k} must be kept");
        }
        // The allowlist wins.
        assert!(!should_scrub(
            "CLAUDE_CODE_USE_BEDROCK",
            &["CLAUDE_CODE_USE_BEDROCK".to_string()]
        ));
    }

    #[test]
    fn replies_to_queries() {
        let mut p = vt100::Parser::new(10, 20, 0);
        p.process(b"ab\r\ncd");
        let r = query_replies(b"\x1b[6n\x1b[c", p.screen());
        assert_eq!(r, b"\x1b[2;3R\x1b[?62;22c");
        assert!(query_replies(b"plain text", p.screen()).is_empty());
    }
}
