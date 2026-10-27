use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::AppEvent;

const SCRUBBED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

pub fn should_scrub(key: &str, pass: &[String]) -> bool {
    if key == "CLAUDE_CONFIG_DIR" || pass.iter().any(|p| p == key) {
        return false;
    }

    SCRUBBED_ENV.contains(&key)
        || key.starts_with("CLAUDE_CODE_")
        || key.starts_with("GODTERM_")
        || key.starts_with("CLAUDEGO_")
        || crate::harness::scrub_grok(key)
        || key == "CODEX_HOME"
        || key.starts_with("CODEX_INTERNAL_")
        || key == "CODEX_THREAD_ID"
}

pub const STREAM_CAP: usize = 4 << 20;
const STREAM_SLICE: std::time::Duration = std::time::Duration::from_millis(100);

static SCROLLBACK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(2000);

pub fn set_scrollback(lines: usize) {
    SCROLLBACK.store(lines.min(100_000), std::sync::atomic::Ordering::Relaxed);
}

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

    pub home_env: &'static str,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub config_dir: PathBuf,
    pub kind: LaunchKind,

    pub pass_env: Vec<String>,

    pub type_when_idle: Option<Vec<u8>>,

    pub extra_env: Vec<(String, String)>,

    pub harness: crate::harness::Harness,
}

struct Proc {
    master: Box<dyn MasterPty + Send>,

    input: std::sync::mpsc::SyncSender<Vec<u8>>,
    child: Box<dyn Child + Send + Sync>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Idle,

    Starting,

    Working,

    Permission,

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

            Activity::Ready => "ready",
            Activity::Exited => "exited",
        }
    }
}

pub fn asks_question(text: &str) -> bool {
    text.contains("Enter to select") && text.contains("Esc to cancel")
}

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
    if permission || asks_question(text) {
        Activity::Permission
    } else if text.contains("esc to interrupt")
        || text.contains("Ctrl+c to cancel the turn")
        || text.contains("Ctrl+c:cancel")
    {
        Activity::Working
    } else if text.contains("? for shortcuts")
        || text.contains("Type a message")
        || text.contains("│ >")
        || text.contains("│ ❯")
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

pub fn next_uid() -> u64 {
    NEXT_UID.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn reserve_uids_below(n: u64) {
    NEXT_UID.fetch_max(n, std::sync::atomic::Ordering::Relaxed);
}

static RESERVED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const RESERVE_BLOCK: u64 = 32;

fn reserve_file() -> PathBuf {
    crate::config::app_home().join("tab-ids")
}

pub fn reserved_on_disk() -> u64 {
    std::fs::read_to_string(reserve_file())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn alloc_uid() -> u64 {
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
    pub uid: u64,

    pub live_cwd: Option<PathBuf>,
    pub cwd_polled: Option<std::time::Instant>,

    pub cwd: PathBuf,

    pub custom_name: Option<String>,

    pub accent: Option<String>,

    pub session_id: Option<String>,

    pub started: std::time::SystemTime,

    pub pending: Option<LaunchKind>,
    pub activity: Activity,

    pub transient: bool,

    pub trust_answered: bool,

    pub restart_at: Option<Instant>,

    pub crashes: Vec<Instant>,

    pub activity_since: Instant,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub state: PaneState,
    pub kind: LaunchKind,
    pub scroll: usize,

    pub generation: u64,
    size: (u16, u16),
    proc: Option<Proc>,
    last_output: Arc<Mutex<Instant>>,
    pending_type: Option<(Instant, Vec<u8>)>,

    history: usize,

    pub suspended: bool,

    pub group: Option<String>,

    pub pinned: bool,

    pub browser: Option<String>,

    pub browser_also: Vec<String>,

    pub opened: std::time::SystemTime,

    pub last_input: Instant,

    pub finished: bool,

    pub bg_wait: bool,

    pub bg_wait_unseen: bool,

    pub bg_lock: Option<crate::session_ids::BgLock>,

    pub next_launch: Option<crate::session_ids::NextLaunch>,

    pub lease: Option<crate::app_lease::TabLease>,

    pub model: Option<String>,

    pub effort: Option<String>,
    pub model_polled: Option<Instant>,

    pub model_stamp: Option<(u64, std::time::SystemTime)>,

    pub model_set: Option<(String, Instant)>,

    pub harness: crate::harness::Harness,

    pub web: Arc<crate::web::tap::Tap>,

    pub web_size: Option<(u16, u16)>,

    pub web_sizer: Option<String>,

    pub secret: Option<Arc<Mutex<crate::acct_tokens::Scrub>>>,
}

pub const SECRET_COLS: u16 = 320;

impl Pane {
    pub fn screen_text(&self) -> String {
        self.parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .contents()
    }

    pub fn new(cwd: PathBuf) -> Pane {
        let uid = alloc_uid();
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, scrollback())));
        Pane {
            live_cwd: None,
            cwd_polled: None,
            uid,
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
            web: crate::web::tap::Tap::new(uid, Arc::clone(&parser)),
            web_size: None,
            web_sizer: None,
            secret: None,
            parser,
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
            browser: None,
            browser_also: vec![],
            opened: std::time::SystemTime::now(),
            last_input: Instant::now(),
            finished: false,
            bg_wait: false,
            bg_wait_unseen: false,
            bg_lock: None,
            next_launch: None,
            lease: None,
            model: None,
            effort: None,
            model_polled: None,
            model_stamp: None,
            model_set: None,
            harness: crate::harness::Harness::Claude,
        }
    }

    pub fn last_output_at(&self) -> Instant {
        *self.last_output.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn last_activity(&self) -> Instant {
        let out = *self.last_output.lock().unwrap_or_else(|e| e.into_inner());
        out.max(self.activity_since).max(self.last_input)
    }

    pub fn set_history(&mut self, lines: usize) {
        if lines == self.history {
            return;
        }
        self.history = lines;
        let mut p = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        let was = p.screen().scrollback();
        let (mut fresh, keep) = rebuilt(&mut p, lines);
        fresh.screen_mut().set_scrollback(was.min(keep));
        self.scroll = fresh.screen().scrollback();
        *p = fresh;
    }

    #[cfg(test)]
    pub fn history(&self) -> usize {
        self.history
    }

    pub fn history_lines(&self) -> usize {
        self.history
    }

    pub fn relaunch_kind(&self) -> LaunchKind {
        match &self.session_id {
            Some(id) => LaunchKind::Resume(id.clone()),
            None => LaunchKind::Normal,
        }
    }

    pub fn crashed(&self) -> bool {
        matches!(self.state, PaneState::Exited(Some(c)) if c != 0)
    }

    pub fn pid(&self) -> Option<u32> {
        self.proc.as_ref().and_then(|p| p.child.process_id())
    }

    pub fn is_running(&self) -> bool {
        self.state == PaneState::Running
    }

    pub fn spawn(&mut self, spec: LaunchSpec, events: Sender<AppEvent>) -> Result<()> {
        if cfg!(test) && crate::test_guard::is_current_exe(std::path::Path::new(&spec.program)) {
            anyhow::bail!("refused in tests: {} is the test binary", spec.program);
        }
        self.kill();
        self.generation += 1;
        let (rows, cols) = self.size;

        self.parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, self.history)));
        self.sync_tap();
        self.scroll = 0;
        self.suspended = false;
        self.harness = spec.harness;
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

        self.web.respawn(
            self.generation,
            Arc::clone(&self.parser),
            Some(input.clone()),
        );
        let web = Arc::clone(&self.web);
        let parser = Arc::clone(&self.parser);
        let lines = self.history;
        let w2 = input.clone();
        let last = Arc::clone(&self.last_output);
        let (id, gen) = (self.uid, self.generation);
        let secret = self.secret.clone();

        let reader_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(windows)]
        if let Some(pid) = child.process_id() {
            let (done, events) = (Arc::clone(&reader_done), events.clone());
            let _ = std::thread::Builder::new()
                .name(format!("pty-exit-{id}"))
                .spawn(move || {
                    let mut gone = 0;
                    while !done.load(std::sync::atomic::Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(200));
                        gone = if crate::platform::kill(pid as i32, 0) != 0 {
                            gone + 1
                        } else {
                            0
                        };
                        if gone >= 2 {
                            std::thread::sleep(Duration::from_millis(400));
                            if !done.load(std::sync::atomic::Ordering::SeqCst) {
                                let _ = events.send(AppEvent::PaneExited(id, gen));
                            }
                            return;
                        }
                    }
                });
        }
        let reader_flag = Arc::clone(&reader_done);
        std::thread::Builder::new()
            .name(format!("pty-{id}"))
            .spawn(move || {
                let mut buf = vec![0u8; read_buf];

                let mut window = Instant::now();
                let mut in_window = 0usize;

                let mut cap = crate::vt_guard::CsiCap::default();
                let mut clean = Vec::with_capacity(read_buf);
                let mut scrubbed = Vec::new();
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
                            let mut chunk = &buf[..n];
                            if let Some(sc) = &secret {
                                scrubbed.clear();
                                sc.lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .filter(chunk, &mut scrubbed);
                                chunk = &scrubbed;
                            }
                            let reply = {
                                let mut p = parser.lock().unwrap_or_else(|e| e.into_inner());
                                let (rows, cols) = p.screen().size();
                                clean.clear();
                                cap.filter(chunk, rows.max(cols), &mut clean);
                                let alt = p.screen().alternate_screen();
                                let fed = process_guarded(&mut p, &clean, lines);
                                if !fed {
                                    crate::log::error(&format!(
                                        "{}: the terminal parser failed on this tab's output; its screen was rebuilt",
                                        crate::control::tab_id(id)
                                    ));
                                }

                                if web.active() {

                                    if fed && alt && !p.screen().alternate_screen() {
                                        web.resync_all();
                                    } else if fed {
                                        web.publish(gen, &clean);
                                    } else {
                                        web.resync_all();
                                    }
                                }
                                query_replies(chunk, p.screen())
                            };
                            if !reply.is_empty() {
                                let _ = w2.try_send(reply);
                            }
                            *last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();

                            if !OUTPUT_PENDING.swap(true, std::sync::atomic::Ordering::AcqRel)
                                && events.send(AppEvent::PaneOutput).is_err()
                            {
                                break;
                            }
                        }
                    }
                }

                if let Some(sc) = &secret {
                    let mut tail = vec![];
                    sc.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .flush(&mut tail);
                    if !tail.is_empty() {
                        let mut p = parser.lock().unwrap_or_else(|e| e.into_inner());
                        if process_guarded(&mut p, &tail, lines) && web.active() {
                            web.publish(gen, &tail);
                        }
                    }
                }
                reader_flag.store(true, std::sync::atomic::Ordering::SeqCst);
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

    pub fn on_exit(&mut self, gen: u64) {
        if gen != self.generation {
            return;
        }
        let code = self.proc.as_mut().and_then(|p| {
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
        self.web.stop_input();
        self.pending_type = None;
        self.state = PaneState::Exited(code);
        self.set_activity(Activity::Exited);
    }

    pub fn kill(&mut self) {
        if let Some(p) = self.proc.take() {
            if let Some(pid) = p.child.process_id() {
                crate::platform::kill(pid as i32, crate::platform::SIGHUP);
            }
            let Proc {
                master,
                input,
                child,
            } = p;
            drop(input);

            self.web.stop_input();
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
        let next = crate::prompt_cli::activity_for(self.harness, &text)
            .unwrap_or_else(|| detect_activity(&text));
        let old = self.activity;
        if next != old {
            self.set_activity(next);
            Some((old, next))
        } else {
            None
        }
    }

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
        if bytes.is_empty() || self.lease.as_ref().is_some_and(|l| l.blocks_input()) {
            return;
        }
        if is_typing(bytes) {
            self.last_input = Instant::now();
        }
        #[cfg(test)]
        SENT.with(|s| s.borrow_mut().extend_from_slice(bytes));
        if let Some(p) = &self.proc {
            let _ = p.input.try_send(bytes.to_vec());
        }
    }

    #[cfg(test)]
    pub fn pty_size(&self) -> (u16, u16) {
        self.size
    }

    fn sync_tap(&mut self) {
        if self.web.uid != self.uid {
            self.web.forget();
            self.web = crate::web::tap::Tap::new(self.uid, Arc::clone(&self.parser));
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.sync_tap();
        let (rows, cols) = self.web_size.unwrap_or((rows, cols));
        let (rows, cols) = (rows.max(2), cols.max(10));
        let cols = if self.secret.is_some() {
            cols.max(SECRET_COLS)
        } else {
            cols
        };
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        {
            let mut p = self.parser.lock().unwrap_or_else(|e| e.into_inner());
            p.screen_mut().set_size(rows, cols);

            if wide_at_edge(&mut p) {
                let was = p.screen().scrollback();
                let modes = p.screen().input_mode_formatted();
                let (mut fresh, keep) = rebuilt(&mut p, self.history);
                fresh.process(&modes);
                fresh.screen_mut().set_scrollback(was.min(keep));
                self.scroll = fresh.screen().scrollback();
                *p = fresh;

                self.web.resync_all();
            } else {
                self.web.resized(rows, cols);
            }
        }
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

        let next = (p.screen().scrollback() as isize + delta).max(0) as usize;
        p.screen_mut().set_scrollback(next);

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
        self.web.forget();
        self.kill();
    }
}

pub static OUTPUT_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

static REAPING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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

pub fn wait_for_reapers(limit: Duration) {
    let t0 = Instant::now();
    while REAPING.load(std::sync::atomic::Ordering::SeqCst) > 0 && t0.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn reap(mut child: Box<dyn Child + Send + Sync>) {
    let _ = std::thread::Builder::new()
        .name("reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

fn rebuilt(p: &mut vt100::Parser, lines: usize) -> (vt100::Parser, usize) {
    let (rows, cols) = p.screen().size();
    p.screen_mut().set_scrollback(usize::MAX);
    let keep = p.screen().scrollback().min(lines);
    let mut text: Vec<String> = Vec::with_capacity(keep + rows as usize);
    for k in (1..=keep).rev() {
        p.screen_mut().set_scrollback(k);
        text.push(p.screen().rows(0, cols).next().unwrap_or_default());
    }
    p.screen_mut().set_scrollback(0);
    let replay = p.screen().contents_formatted();
    let mut fresh = vt100::Parser::new(rows, cols, lines);
    if keep > 0 {
        text.extend(p.screen().rows(0, cols));
        let n = text.len();
        for (i, l) in text.iter().enumerate() {
            fresh.process(l.as_bytes());
            if i + 1 < n {
                fresh.process(b"\r\n");
            }
        }
    }
    fresh.process(&replay);
    (fresh, keep)
}

fn wide_at_edge(p: &mut vt100::Parser) -> bool {
    let was = p.screen().scrollback();
    p.screen_mut().set_scrollback(0);
    let (rows, cols) = p.screen().size();
    let found =
        cols > 0 && (0..rows).any(|r| p.screen().cell(r, cols - 1).is_some_and(|c| c.is_wide()));
    p.screen_mut().set_scrollback(was);
    found
}

pub fn process_guarded(p: &mut vt100::Parser, bytes: &[u8], lines: usize) -> bool {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    if catch_unwind(AssertUnwindSafe(|| p.process(bytes))).is_ok() {
        return true;
    }
    let fresh = catch_unwind(AssertUnwindSafe(|| {
        let modes = p.screen().input_mode_formatted();
        let (mut f, _) = rebuilt(p, lines);
        f.process(&modes);
        f
    }));
    *p = fresh.unwrap_or_else(|_| {
        let (rows, cols) = p.screen().size();
        vt100::Parser::new(rows, cols, lines)
    });
    false
}

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
        let mut p = vt100::Parser::new(10, 40, 100);
        for i in 0..20_000 {
            p.process(format!("line {i} \x1b[31mred\x1b[0m\r\n").as_bytes());
        }
        p.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(p.screen().scrollback(), 100);
    }

    #[test]
    fn a_resize_that_cuts_a_wide_char_lays_the_screen_out_again() {
        let mut t = Pane::new("/tmp".into());
        t.resize(5, 11);
        {
            let mut p = t.parser.lock().unwrap();
            p.process("history\r\n".repeat(20).as_bytes());
            p.process("\x1b[2;1Hkept\x1b[2;10H日".as_bytes());
        }

        t.resize(5, 10);
        let mut p = t.parser.lock().unwrap();

        assert!(process_guarded(&mut p, b"\x1b[2;10H\x1b[K", 100));
        assert!(process_guarded(&mut p, b"\x1b[2;10Hx", 100));
        assert!(p.screen().contents().contains("kept"));
        p.screen_mut().set_scrollback(usize::MAX);
        assert!(p.screen().scrollback() > 0, "the history came along");
    }

    #[test]
    fn history_survives_a_new_limit() {
        let mut t = Pane::new("/tmp".into());
        t.resize(10, 40);
        t.set_history(10_000);
        {
            let mut p = t.parser.lock().unwrap();
            for i in 1..=300 {
                p.process(format!("line {i}\r\n").as_bytes());
            }
            p.process(b"\x1b[31mprompt\x1b[0m");
        }
        let total = |t: &Pane| {
            let mut p = t.parser.lock().unwrap();
            p.screen_mut().set_scrollback(usize::MAX);
            let n = p.screen().scrollback();
            p.screen_mut().set_scrollback(0);
            n
        };
        assert_eq!(total(&t), 291);
        t.set_history(100);
        assert_eq!(total(&t), 100);
        t.set_history(10_000);
        assert_eq!(total(&t), 100, "raising the limit keeps the history");
        let p = t.parser.lock().unwrap();
        let screen = p.screen().contents();
        assert!(
            screen.contains("line 300") && screen.ends_with("prompt"),
            "{screen}"
        );
        assert_eq!(
            p.screen().cell(9, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        drop(p);

        t.scroll_by(1000);
        let p = t.parser.lock().unwrap();
        assert!(
            p.screen().contents().starts_with("line 192"),
            "{}",
            p.screen().contents()
        );
        drop(p);

        t.scroll_by(-50);
        let at = t.scroll;
        t.set_history(5_000);
        assert_eq!(t.scroll, at);
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

#[cfg(test)]
thread_local! {
    pub static SENT: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn is_typing(b: &[u8]) -> bool {
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b && b.get(i + 1) == Some(&b'[') {
            let rest = &b[i + 2..];
            let end = rest.iter().position(|c| (0x40..=0x7e).contains(c));
            let Some(e) = end else { return true };
            let (body, fin) = (&rest[..e], rest[e]);
            let report = match fin {
                b'I' | b'O' if body.is_empty() => true,
                b'M' | b'm' if body.first() == Some(&b'<') => true,
                b'R' | b'c' => true,
                _ => false,
            };
            if !report {
                return true;
            }
            i += 2 + e + 1;
        } else {
            return true;
        }
    }
    false
}
