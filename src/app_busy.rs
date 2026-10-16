//! What send_prompt does with a tab that is busy.
//!
//! Before this a prompt for a working tab was "queued": it sat behind the
//! tab's current turn and the assistant could say nothing more useful.
//! Now `mode` picks (auto is the default):
//! - side: Claude Code's side-question channel, `/btw <question>`. It is
//!   answered at once from the conversation so far, without interrupting
//!   the tab's work (it has no tools). The answer is read from the tab's
//!   screen and reported back.
//! - background: a fork of the tab's session started with `claude --bg
//!   --resume <id> --fork-session --session-id <new> <task>` on the same
//!   account and in the same folder. It works in parallel; its result is
//!   read from the fork's transcript and reported back. The tab's own
//!   session id never becomes the fork's (state::detect_session skips it).
//! - queue: the old behaviour, typed in and run after the current turn.
//! - auto: an idle tab gets the prompt as before. A busy Claude tab gets
//!   a question that needs no tools as side, anything else as background.
//!   Grok tabs have neither channel and queue (the result says so).
//!
//! Anything that cannot be done falls back to queue with a note: the
//! side channel not showing up on the tab, a background launch failing.
//! The delivery keeps its id and shows the mode used (get_state, receipts).

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_followup::FuState;
use crate::control::{tab_id, Delivery, Stage};
use crate::harness::Harness;
use crate::pane::Activity;
use crate::receipts::Receipt;

/// How a prompt went (or goes) into a tab.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Mode {
    /// An idle tab: typed in.
    #[default]
    Direct,
    /// Typed in behind the tab's current work.
    Queue,
    /// `/btw`: answered at once without interrupting.
    Side,
    /// A forked background session.
    Background,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Direct => "direct",
            Mode::Queue => "queue",
            Mode::Side => "side",
            Mode::Background => "background",
        }
    }

    /// Runs outside the tab's own prompt flow (followed here, not by the
    /// tab's transcript).
    pub fn external(self) -> bool {
        matches!(self, Mode::Side | Mode::Background)
    }
}

/// The `mode` argument of send_prompt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModeArg {
    Auto,
    Queue,
    Background,
    Side,
}

impl ModeArg {
    pub fn parse(s: Option<&str>) -> Result<ModeArg, String> {
        match s.map(|s| s.trim().to_lowercase()).as_deref() {
            None | Some("") | Some("auto") => Ok(ModeArg::Auto),
            Some("queue") | Some("queued") => Ok(ModeArg::Queue),
            Some("background") | Some("bg") | Some("fork") => Ok(ModeArg::Background),
            Some("side") | Some("btw") | Some("side_question") => Ok(ModeArg::Side),
            Some(o) => Err(format!(
                "mode must be auto, queue, background or side (not \"{o}\")"
            )),
        }
    }
}

/// What a tab can take right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Busy {
    /// At its prompt.
    Ready,
    /// In a turn.
    Working,
    /// Waiting for an approval (typing would answer it).
    Permission,
    /// Starting, paused or not running: only a queue works.
    NotUp,
}

/// Words that ask for a look at files, commands or the web: a side
/// question has no tools, so these go to a background session.
const NEEDS_TOOLS: &[&str] = &[
    "check",
    "run ",
    "look at",
    "look in",
    "look up",
    "find ",
    "search",
    "grep",
    "open ",
    "read ",
    "test",
    "list ",
    "show me",
    "fetch",
    "download",
    "install",
    "build",
    "compile",
    "fix",
    "edit",
    "change",
    "write",
    "create",
    "implement",
    "add ",
    "remove",
    "delete",
    "update",
    "refactor",
    "commit",
    "push",
    "deploy",
    "investigate",
    "debug",
    "review",
    "analyze",
    "analyse",
    "http",
    "git ",
    "file",
    "folder",
    "repo",
];

/// Whether `text` is a question (so a side question fits): the caller said
/// so (`expect_reply`), or it reads as one.
pub fn is_question(text: &str, expect_reply: Option<bool>) -> bool {
    if let Some(e) = expect_reply {
        return e;
    }
    let t = text.trim().to_lowercase();
    if t.ends_with('?') {
        return true;
    }
    let first = t
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .find(|w| !w.is_empty())
        .unwrap_or("");
    [
        "what", "what's", "whats", "why", "how", "when", "where", "who", "which", "is", "are",
        "was", "were", "can", "could", "does", "do", "did", "should", "will", "would", "any",
        "status",
    ]
    .contains(&first)
}

/// Whether answering `text` would need tools (files, commands) or is too
/// long for a quick side question.
pub fn needs_tools(text: &str) -> bool {
    let t = format!(" {} ", text.trim().to_lowercase());
    text.chars().count() > 500
        || text.trim().lines().count() > 3
        || NEEDS_TOOLS.iter().any(|k| t.contains(k))
}

/// The mode for one tab, and why when it is not what was asked for.
#[allow(clippy::too_many_arguments)]
pub fn choose(
    arg: ModeArg,
    busy: Busy,
    claude: bool,
    question: bool,
    tools: bool,
    images: bool,
    unsent: bool,
) -> (Mode, Option<String>) {
    let queue_or_direct = |why: Option<String>| {
        (
            if busy == Busy::Ready {
                Mode::Direct
            } else {
                Mode::Queue
            },
            why,
        )
    };
    match arg {
        ModeArg::Queue => queue_or_direct(None),
        _ if !claude && arg != ModeArg::Auto => (
            Mode::Queue,
            Some("this is a Grok tab: it has no side channel or background sessions, so the prompt is queued".into()),
        ),
        ModeArg::Auto if busy == Busy::Ready || busy == Busy::NotUp => queue_or_direct(None),
        ModeArg::Auto if !claude => (
            Mode::Queue,
            Some("this is a Grok tab: it has no side channel or background sessions, so the prompt is queued".into()),
        ),
        ModeArg::Auto => {
            if busy == Busy::Working && question && !tools && !images && !unsent {
                (Mode::Side, None)
            } else {
                (Mode::Background, None)
            }
        }
        ModeArg::Background => (Mode::Background, None),
        ModeArg::Side => {
            if busy == Busy::NotUp {
                (
                    Mode::Queue,
                    Some("the tab is not up yet, so the prompt is queued".into()),
                )
            } else if busy == Busy::Permission {
                (
                    Mode::Background,
                    Some("the tab waits for an approval (a side question would answer it), so a background session took it".into()),
                )
            } else if images {
                (
                    Mode::Background,
                    Some("a side question cannot take images, so a background session took it".into()),
                )
            } else if unsent {
                (
                    Mode::Background,
                    Some("the tab has unsent text in its input box, so a background session took it".into()),
                )
            } else {
                (Mode::Side, None)
            }
        }
    }
}

/// Flatten a question to one line for `/btw`.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The start of the question as the screen shows it.
fn head_of(q: &str) -> String {
    one_line(q).chars().take(24).collect()
}

fn strip_box(l: &str) -> &str {
    l.trim()
        .trim_matches(|c: char| "│┃|╭╮╰╯─━ ".contains(c))
        .trim()
}

fn is_prompt_line(l: &str) -> bool {
    let t = strip_box(l);
    t.starts_with('>') || t.starts_with('❯')
}

fn border_only(l: &str) -> bool {
    let t = l.trim();
    !t.is_empty() && t.chars().all(|c| "│┃|╭╮╰╯─━ ".contains(c)) && t.contains(['─', '━'])
}

/// What a tab's screen shows of a side question.
#[derive(Debug, Clone, PartialEq)]
pub enum Scan {
    /// The panel is not up (yet).
    NotShown,
    /// The tab's claude does not know `/btw`.
    Unavailable(String),
    /// The panel is up. `answer` is empty while it is still thinking;
    /// `footer`: its dismiss line is showing.
    Showing { answer: String, footer: bool },
}

/// Read a side question's panel from a screen: the line that echoes
/// `/btw <question>` (not the input box), the answer after it, up to the
/// dismiss line or the input box border.
pub fn scan_side(screen: &str, question: &str) -> Scan {
    let head = head_of(question);
    let lines: Vec<&str> = screen.lines().collect();
    let echo = lines.iter().rposition(|l| {
        let flat = one_line(l);
        !head.is_empty() && flat.contains(&head) && !is_prompt_line(l)
    });
    let Some(i) = echo else {
        let tail = lines[lines.len().saturating_sub(14)..]
            .join("\n")
            .to_lowercase();
        if tail.contains("btw")
            && [
                "unknown command",
                "unknown skill",
                "isn't available",
                "is not available",
                "no such command",
                "not a command",
            ]
            .iter()
            .any(|k| tail.contains(k))
        {
            return Scan::Unavailable("this claude does not know /btw".into());
        }
        return Scan::NotShown;
    };
    let mut ans: Vec<String> = vec![];
    let mut footer = false;
    for l in &lines[i + 1..] {
        let low = l.to_lowercase();
        if low.contains("esc")
            && (low.contains("dismiss") || low.contains("close") || low.contains("exit"))
            || low.contains("press space")
        {
            footer = true;
            break;
        }
        if border_only(l) && !ans.is_empty() {
            break;
        }
        if is_prompt_line(l) && !ans.is_empty() {
            break;
        }
        ans.push(strip_box(l).to_string());
    }
    while ans.last().is_some_and(|l| l.is_empty()) {
        ans.pop();
    }
    while ans.first().is_some_and(|l| l.is_empty()) {
        ans.remove(0);
    }
    let text = ans.join("\n");
    let low = text.to_lowercase();
    let pending = !text.contains('\n')
        && text.chars().count() <= 40
        && (low.starts_with("answering")
            || low.starts_with("thinking")
            || low.starts_with("loading")
            || text.ends_with('…')
            || text.ends_with("..."));
    Scan::Showing {
        answer: if pending { String::new() } else { text },
        footer,
    }
}

/// The word in a background job's state that means it still runs.
fn job_active(state: &str) -> bool {
    let s = state.to_lowercase();
    ["work", "run", "start", "busy", "active", "block", "wait"]
        .iter()
        .any(|k| s.contains(k))
}

/// The id `claude --bg` printed: a word of 8 or more hex digits.
pub fn parse_bg_id(out: &str) -> Option<String> {
    out.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .find(|w| {
            w.len() >= 8
                && w.len() <= 36
                && w.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
                && w.chars().filter(|c| *c != '-').count() >= 8
        })
        .map(|w| w.chars().take(8).collect())
}

/// Run `claude --bg ...` and return what it printed (bounded in time).
fn run_bg(
    bin: &str,
    config_env: (&str, &std::path::Path),
    cwd: &std::path::Path,
    args: &[String],
    pass: &[String],
) -> Result<String, String> {
    let mut c = std::process::Command::new(bin);
    c.args(args)
        .current_dir(cwd)
        .env(config_env.0, config_env.1)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, _) in std::env::vars() {
        if crate::pane::should_scrub(&k, pass) {
            c.env_remove(&k);
        }
    }
    let mut child = c
        .spawn()
        .map_err(|e| format!("could not run claude --bg: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                use std::io::Read;
                let (mut out, mut err) = (String::new(), String::new());
                if let Some(mut o) = child.stdout.take() {
                    let _ = o.read_to_string(&mut out);
                }
                if let Some(mut e) = child.stderr.take() {
                    let _ = e.read_to_string(&mut err);
                }
                return if s.success() {
                    Ok(out)
                } else {
                    let err = err.trim();
                    Err(if err.is_empty() {
                        format!("claude --bg ended with {s}")
                    } else {
                        err.chars().take(200).collect()
                    })
                };
            }
            Ok(None) if start.elapsed() > Duration::from_secs(30) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("claude --bg did not return in 30 s".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Where a side question is.
#[derive(Debug, Clone, PartialEq)]
enum SideAt {
    /// Typed, Enter to follow (pending_enters).
    Typed,
    /// Entered at this time; the panel not seen yet.
    Entered(Instant),
    /// The panel is up since this time.
    Panel(Instant),
}

enum Kind {
    Side {
        question: String,
        at: SideAt,
        last: String,
        changed: Instant,
        dismissed: bool,
    },
    Bg {
        /// The launch, until it returns.
        rx: Option<Receiver<Result<String, String>>>,
        /// The fork's session id and job id.
        session: String,
        short: String,
        config_dir: PathBuf,
        forked_from: Option<String>,
        task_state: String,
        last_poll: Instant,
        idle_since: Option<Instant>,
        seen_job: bool,
    },
}

/// How a run ended (or not yet).
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Going,
    Answered(String),
    Failed(String),
}

/// One side question or background session out for a delivery.
pub struct Run {
    pub delivery: u64,
    pub uid: u64,
    kind: Kind,
    pub outcome: Outcome,
    pub started: Instant,
    pub ended: Option<Instant>,
}

impl Run {
    pub fn mode(&self) -> Mode {
        match self.kind {
            Kind::Side { .. } => Mode::Side,
            Kind::Bg { .. } => Mode::Background,
        }
    }

    /// The background job id.
    pub fn short(&self) -> Option<&str> {
        match &self.kind {
            Kind::Bg { short, .. } => Some(short),
            _ => None,
        }
    }

    /// The launch or the Enter has not happened yet.
    pub fn starting(&self) -> bool {
        match &self.kind {
            Kind::Bg { rx, .. } => rx.is_some(),
            Kind::Side { at, .. } => *at == SideAt::Typed,
        }
    }
}

/// What App keeps for these.
#[derive(Default)]
pub struct State {
    pub runs: Vec<Run>,
    /// Session ids of forks started here (never a tab's own session).
    pub fork_ids: Vec<String>,
}

#[cfg(not(test))]
const SIDE_NO_PANEL: Duration = Duration::from_secs(15);
#[cfg(test)]
const SIDE_NO_PANEL: Duration = Duration::from_millis(1500);
const SIDE_GIVE_UP: Duration = Duration::from_secs(150);
const SIDE_STABLE: Duration = Duration::from_secs(6);
const SIDE_STABLE_FOOTER: Duration = Duration::from_millis(1200);
#[cfg(not(test))]
const BG_POLL: Duration = Duration::from_secs(2);
#[cfg(test)]
const BG_POLL: Duration = Duration::from_millis(100);
#[cfg(not(test))]
const BG_QUIET: Duration = Duration::from_secs(4);
#[cfg(test)]
const BG_QUIET: Duration = Duration::from_millis(600);
const BG_CAP: Duration = Duration::from_secs(3 * 3600);
const KEEP_RUN: Duration = Duration::from_secs(900);

impl App {
    fn busy_of(&self, s: usize, t: usize) -> Busy {
        let tab = &self.panes[s].tabs[t];
        if !tab.is_running() {
            return Busy::NotUp;
        }
        match tab.activity {
            Activity::Working => Busy::Working,
            Activity::Permission => Busy::Permission,
            Activity::Ready => Busy::Ready,
            _ => Busy::NotUp,
        }
    }

    fn claude_tab(&self, s: usize) -> bool {
        self.panes[s]
            .account
            .and_then(|a| self.cfg.accounts.get(a))
            .is_none_or(|a| a.harness() == Harness::Claude)
    }

    /// send_prompt for one tab: the mode, then the delivery. Returns its
    /// id. `text` carries any image lines; `plain` is the text without.
    pub fn send_in_mode(
        &mut self,
        uid: u64,
        text: &str,
        plain: &str,
        has_images: bool,
        arg: ModeArg,
        expect_reply: Option<bool>,
    ) -> u64 {
        let Some((s, t)) = self.find_tab(uid) else {
            return self.deliver_request(uid, text);
        };
        let busy = self.busy_of(s, t);
        let claude = self.claude_tab(s);
        let unsent = crate::archive::unsent_input(&self.tab_screen(s, t)).is_some();
        let user = self.assistant.last_user.clone();
        let q = is_question(plain, expect_reply)
            || (expect_reply.is_none()
                && crate::app_followup::asks_something(&user)
                && plain.trim().ends_with('?'));
        let (mode, note) = choose(arg, busy, claude, q, needs_tools(plain), has_images, unsent);
        crate::log::info(&format!(
            "send_prompt: {} busy={busy:?} arg={arg:?} -> {}{}",
            tab_id(uid),
            mode.word(),
            note.as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default()
        ));
        let id = match mode {
            Mode::Side => self.start_side(uid, s, t, plain),
            Mode::Background => self.start_background(uid, s, t, text, busy != Busy::Ready),
            Mode::Direct | Mode::Queue => {
                let id = self.deliver_request(uid, text);
                if let Some(d) = self.deliveries.iter_mut().find(|d| d.id == id) {
                    // A tab that is busy by the time it is typed in is a queue.
                    d.rc.mode = if mode == Mode::Direct && !d.rc.busy {
                        Mode::Direct
                    } else {
                        Mode::Queue
                    };
                }
                id
            }
        };
        if let Some(n) = note {
            if let Some(d) = self.deliveries.iter_mut().find(|d| d.id == id) {
                d.rc.note = Some(n);
            }
        }
        id
    }

    /// A delivery that is followed here, not through the tab's transcript.
    fn external_delivery(&mut self, uid: u64, text: &str, mode: Mode, busy: bool) -> u64 {
        self.delivery_seq += 1;
        let id = self.delivery_seq;
        let mut rc = crate::app_receipts::Track::new(busy);
        rc.notify = true;
        rc.mode = mode;
        self.deliveries.push(Delivery {
            id,
            uid,
            text: text.to_string(),
            stage: Stage::Delivered,
            created: Instant::now(),
            before: None,
            reported: false,
            rc,
        });
        self.state_dirty = true;
        id
    }

    fn start_side(&mut self, uid: u64, s: usize, t: usize, question: &str) -> u64 {
        let busy = self.panes[s].tabs[t].activity == Activity::Working;
        let id = self.external_delivery(uid, question, Mode::Side, busy);
        let typed = format!("/btw {}", one_line(question));
        self.paste_then_enter(s, t, &typed);
        self.busy.runs.push(Run {
            delivery: id,
            uid,
            kind: Kind::Side {
                question: question.to_string(),
                at: SideAt::Typed,
                last: String::new(),
                changed: Instant::now(),
                dismissed: false,
            },
            outcome: Outcome::Going,
            started: Instant::now(),
            ended: None,
        });
        id
    }

    /// Fork the tab's session into a background run with `task`.
    fn start_background(&mut self, uid: u64, s: usize, t: usize, task: &str, busy: bool) -> u64 {
        let id = self.external_delivery(uid, task, Mode::Background, busy);
        let Some(acct) = self.panes[s].account else {
            self.fall_back(id, "the tab has no account".into());
            return id;
        };
        let a = &self.cfg.accounts[acct];
        let h = a.harness();
        let config_dir = a.config_dir();
        let tab = &self.panes[s].tabs[t];
        let cwd = if tab.cwd.is_dir() {
            tab.cwd.clone()
        } else {
            a.work_dir()
        };
        let parent = tab.session_id.clone();
        let session = crate::session_ops::new_uuid();
        let mut args: Vec<String> = vec!["--bg".into()];
        args.extend(h.permission_args(&self.cfg.mode_for(acct)));
        args.extend(a.args.iter().cloned());
        if let Some(p) = &parent {
            args.extend(["--resume".into(), p.clone(), "--fork-session".into()]);
        }
        args.extend(["--session-id".into(), session.clone()]);
        let mut prompt = task.to_string();
        if parent.is_none() {
            prompt = format!(
                "(You are a background session started for a busy tab that has no saved conversation yet; you start without its context.)\n{prompt}"
            );
        }
        if prompt.starts_with('-') {
            prompt = format!(" {prompt}");
        }
        args.push(prompt);
        let (bin, pass) = (self.cfg.claude_bin(), self.cfg.pass_env.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let (dir, home_env) = (config_dir.clone(), h.home_env());
        std::thread::spawn(move || {
            let _ = tx.send(run_bg(&bin, (home_env, &dir), &cwd, &args, &pass));
        });
        self.busy.fork_ids.push(session.clone());
        crate::log::info(&format!(
            "background: {} forks {} as {} for #{id}",
            tab_id(uid),
            parent.as_deref().unwrap_or("(no session)"),
            &session[..8]
        ));
        self.busy.runs.push(Run {
            delivery: id,
            uid,
            kind: Kind::Bg {
                rx: Some(rx),
                short: session[..8].to_string(),
                session,
                config_dir,
                forked_from: parent,
                task_state: String::new(),
                last_poll: Instant::now() - BG_POLL,
                idle_since: None,
                seen_job: false,
            },
            outcome: Outcome::Going,
            started: Instant::now(),
            ended: None,
        });
        id
    }

    /// The side channel or a background launch failed: the same delivery
    /// is queued instead, with the reason.
    fn fall_back(&mut self, id: u64, why: String) {
        crate::log::info(&format!("send_prompt: #{id} falls back to queue: {why}"));
        self.busy.runs.retain(|r| r.delivery != id);
        for f in &mut self.assistant.follow_ups {
            if f.external == Some(id) {
                f.external = None;
            }
        }
        if let Some(d) = self.deliveries.iter_mut().find(|d| d.id == id) {
            d.rc.mode = Mode::Queue;
            d.rc.note = Some(format!("{why}; queued instead"));
            d.stage = Stage::Queued;
            d.rc.receipt = Receipt::Pending;
            d.rc.receipt_at = Instant::now();
            d.created = Instant::now();
            d.reported = false;
        }
        self.deliveries_tick();
    }

    /// Whether a delivery's reply should still wait (its launch or Enter).
    pub fn external_starting(&self, id: u64) -> bool {
        self.busy
            .runs
            .iter()
            .any(|r| r.delivery == id && r.starting() && r.outcome == Outcome::Going)
    }

    /// The receipt a side question or background session has earned.
    pub fn external_receipt(&self, d: &Delivery) -> Option<Receipt> {
        let r = self.busy.runs.iter().find(|r| r.delivery == d.id)?;
        Some(match (&r.outcome, &r.kind) {
            (Outcome::Answered(_), _) => Receipt::Done,
            (Outcome::Failed(why), _) => Receipt::Dropped(why.clone()),
            (Outcome::Going, Kind::Side { at, .. }) => match at {
                SideAt::Panel(_) => Receipt::Started,
                _ => Receipt::Pending,
            },
            (Outcome::Going, Kind::Bg { rx, task_state, .. }) => {
                if rx.is_some() {
                    Receipt::Pending
                } else if job_active(task_state) {
                    Receipt::Started
                } else {
                    Receipt::Received
                }
            }
        })
    }

    /// Details of a delivery's run for the tool results and get_state.
    pub fn run_json(&self, id: u64) -> Option<Value> {
        let r = self.busy.runs.iter().find(|r| r.delivery == id)?;
        Some(match &r.kind {
            Kind::Side { .. } => json!({"via": "/btw side question"}),
            Kind::Bg {
                short,
                session,
                forked_from,
                task_state,
                ..
            } => {
                json!({"background_session": short, "session": session, "forked_from": forked_from,
                "job_state": if task_state.is_empty() { Value::Null } else { json!(task_state) },
                "attach": format!("claude attach {short}")})
            }
        })
    }

    /// Background sessions started from a tab that still run (tab_json).
    pub fn tab_background_runs(&self, uid: u64) -> Vec<Value> {
        self.busy
            .runs
            .iter()
            .filter(|r| {
                r.mode() == Mode::Background
                    && self.current_uid(r.uid) == uid
                    && r.outcome == Outcome::Going
            })
            .map(|r| {
                let task = self
                    .deliveries
                    .iter()
                    .find(|d| d.id == r.delivery)
                    .map(|d| crate::sessions::snippet(&d.text, 80))
                    .unwrap_or_default();
                let mut v = self.run_json(r.delivery).unwrap_or_else(|| json!({}));
                v["request"] = json!(r.delivery);
                v["task"] = json!(task);
                v
            })
            .collect()
    }

    /// Follow every side question and background session one step.
    pub fn busy_tick(&mut self) {
        if self.busy.runs.is_empty() {
            return;
        }
        let mut runs = std::mem::take(&mut self.busy.runs);
        let mut back: Vec<(u64, String)> = vec![];
        for r in &mut runs {
            if r.ended.is_some() {
                continue;
            }
            let uid = self.current_uid(r.uid);
            r.uid = uid;
            match &mut r.kind {
                Kind::Side {
                    question,
                    at,
                    last,
                    changed,
                    dismissed,
                } => {
                    let Some((s, t)) = self.find_tab(uid) else {
                        r.outcome = Outcome::Failed("the tab was closed before the answer".into());
                        r.ended = Some(Instant::now());
                        continue;
                    };
                    if *at == SideAt::Typed {
                        if self.pending_enters.iter().any(|p| p.uid == uid) {
                            continue;
                        }
                        *at = SideAt::Entered(Instant::now());
                        continue;
                    }
                    let since = match at {
                        SideAt::Entered(i) | SideAt::Panel(i) => *i,
                        SideAt::Typed => Instant::now(),
                    };
                    let screen = self.tab_screen(s, t);
                    match scan_side(&screen, question) {
                        Scan::Unavailable(why) => {
                            self.clear_btw_input(s, t, question);
                            back.push((r.delivery, why));
                        }
                        Scan::NotShown => {
                            if matches!(at, SideAt::Entered(_)) && since.elapsed() > SIDE_NO_PANEL {
                                self.clear_btw_input(s, t, question);
                                back.push((
                                    r.delivery,
                                    "the side channel did not show up on that tab".into(),
                                ));
                            } else if matches!(at, SideAt::Panel(_)) && !last.is_empty() {
                                // The panel closed after an answer was read.
                                continue;
                            } else if matches!(at, SideAt::Panel(_))
                                && r.started.elapsed() > SIDE_GIVE_UP
                            {
                                r.outcome = Outcome::Failed("no answer came back".into());
                                r.ended = Some(Instant::now());
                            }
                        }
                        Scan::Showing { answer, footer } => {
                            if !matches!(at, SideAt::Panel(_)) {
                                *at = SideAt::Panel(Instant::now());
                            }
                            if answer != *last {
                                *last = answer.clone();
                                *changed = Instant::now();
                            }
                            let stable = changed.elapsed();
                            let done = !answer.is_empty()
                                && ((footer && stable >= SIDE_STABLE_FOOTER)
                                    || stable >= SIDE_STABLE);
                            if done {
                                if footer && !*dismissed {
                                    // Only while the dismiss line is on
                                    // screen: Escape then closes the panel
                                    // and not the tab's work.
                                    *dismissed = true;
                                    self.panes[s].tabs[t].write(b"\x1b");
                                }
                                r.outcome = Outcome::Answered(answer);
                                r.ended = Some(Instant::now());
                            } else if r.started.elapsed() > SIDE_GIVE_UP {
                                r.outcome = Outcome::Failed(
                                    "the side question never produced an answer".into(),
                                );
                                r.ended = Some(Instant::now());
                            }
                        }
                    }
                }
                Kind::Bg {
                    rx,
                    short,
                    session,
                    config_dir,
                    task_state,
                    last_poll,
                    idle_since,
                    seen_job,
                    ..
                } => {
                    if let Some(chan) = rx.as_ref() {
                        match chan.try_recv() {
                            Ok(Ok(out)) => {
                                if let Some(id) = parse_bg_id(&out) {
                                    // Only a job that exists is believed.
                                    if id != *short {
                                        if let Some(j) = crate::session_ids::bg_job(config_dir, &id)
                                        {
                                            crate::log::info(&format!(
                                                "background: claude printed job {id}, expected {short}"
                                            ));
                                            *short = id;
                                            if let Some(sid) = j.session_id {
                                                self.busy.fork_ids.push(sid.clone());
                                                *session = sid;
                                            }
                                        }
                                    }
                                }
                                *rx = None;
                                *last_poll = Instant::now() - BG_POLL;
                            }
                            Ok(Err(e)) => {
                                *rx = None;
                                back.push((
                                    r.delivery,
                                    format!("the background session did not start ({e})"),
                                ));
                                continue;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => continue,
                            Err(_) => {
                                *rx = None;
                                back.push((
                                    r.delivery,
                                    "the background launch ended without a result".into(),
                                ));
                                continue;
                            }
                        }
                    }
                    if last_poll.elapsed() < BG_POLL {
                        continue;
                    }
                    *last_poll = Instant::now();
                    let job = crate::session_ids::bg_job(config_dir, short);
                    let active = match &job {
                        Some(j) => {
                            *seen_job = true;
                            *task_state = j.state.clone().unwrap_or_default();
                            job_active(task_state) || task_state.is_empty()
                        }
                        None => !*seen_job,
                    };
                    if active {
                        *idle_since = None;
                        if r.started.elapsed() > BG_CAP {
                            r.outcome = Outcome::Failed(
                                "still running after 3 hours; no longer followed (claude attach has it)".into(),
                            );
                            r.ended = Some(Instant::now());
                        }
                        continue;
                    }
                    let since = *idle_since.get_or_insert_with(Instant::now);
                    if since.elapsed() < BG_QUIET {
                        continue;
                    }
                    let reply = crate::session_ids::transcript(config_dir, session)
                        .map(|p| crate::sessions::follow_turns(&p, 4))
                        .and_then(|turns| {
                            turns
                                .iter()
                                .rev()
                                .find_map(|t| t.reply.clone().filter(|x| !x.trim().is_empty()))
                        });
                    let text = reply.unwrap_or_else(|| {
                        format!(
                            "The background session {short} finished without a reply I could read; `claude attach {short}` or `claude logs {short}` shows it."
                        )
                    });
                    r.outcome = Outcome::Answered(text);
                    r.ended = Some(Instant::now());
                }
            }
        }
        // Finished runs: tell the follow-up its answer is in.
        for r in &runs {
            if let (Some(_), Outcome::Answered(text)) = (r.ended, &r.outcome) {
                let label = match r.mode() {
                    Mode::Background => format!(
                        "[from a background session{}] ",
                        r.short().map(|s| format!(" {s}")).unwrap_or_default()
                    ),
                    _ => "[side answer, from /btw; it had no tools] ".to_string(),
                };
                for f in &mut self.assistant.follow_ups {
                    if f.external == Some(r.delivery) && f.state == FuState::Waiting {
                        f.state = FuState::Answered {
                            text: format!("{label}{text}"),
                            at: chrono::Local::now(),
                        };
                    }
                }
            }
            if let (Some(_), Outcome::Failed(_)) = (r.ended, &r.outcome) {
                for f in &mut self.assistant.follow_ups {
                    if f.external == Some(r.delivery) && f.state == FuState::Waiting {
                        f.state = FuState::Cancelled;
                        f.ended = Some(Instant::now());
                    }
                }
            }
        }
        runs.retain(|r| r.ended.is_none_or(|e| e.elapsed() < KEEP_RUN));
        runs.append(&mut self.busy.runs);
        self.busy.runs = runs;
        let ids: Vec<u64> = self.deliveries.iter().map(|d| d.id).collect();
        self.busy.runs.retain(|r| ids.contains(&r.delivery));
        for (id, why) in back {
            self.fall_back(id, why);
        }
    }

    /// Take a `/btw` that stayed in the input box out again (Ctrl-U).
    fn clear_btw_input(&mut self, s: usize, t: usize, question: &str) {
        let screen = self.tab_screen(s, t);
        let head = head_of(question);
        let in_box = screen
            .lines()
            .any(|l| is_prompt_line(l) && one_line(l).contains("/btw"))
            || crate::archive::unsent_input(&screen).is_some_and(|u| one_line(&u).contains(&head));
        if in_box {
            self.panes[s].tabs[t].write(b"\x15");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse() {
        assert_eq!(ModeArg::parse(None), Ok(ModeArg::Auto));
        assert_eq!(ModeArg::parse(Some("Side")), Ok(ModeArg::Side));
        assert_eq!(ModeArg::parse(Some("bg")), Ok(ModeArg::Background));
        assert!(ModeArg::parse(Some("later")).is_err());
    }

    #[test]
    fn questions_and_work() {
        assert!(is_question("what are you working on right now?", None));
        assert!(is_question("how far along are you", None));
        assert!(!is_question("add a dark mode toggle", None));
        assert!(is_question("add a toggle", Some(true)));
        assert!(!is_question("what is the status?", Some(false)));
        assert!(needs_tools("check the build log for errors?"));
        assert!(!needs_tools("what are you working on right now?"));
        assert!(needs_tools(&"x ".repeat(300)));
    }

    #[test]
    fn auto_picks_by_state_and_kind() {
        let c = |arg, busy, claude, q, tools| choose(arg, busy, claude, q, tools, false, false);
        assert_eq!(
            c(ModeArg::Auto, Busy::Ready, true, true, false).0,
            Mode::Direct
        );
        assert_eq!(
            c(ModeArg::Auto, Busy::NotUp, true, true, false).0,
            Mode::Queue
        );
        assert_eq!(
            c(ModeArg::Auto, Busy::Working, true, true, false).0,
            Mode::Side
        );
        assert_eq!(
            c(ModeArg::Auto, Busy::Working, true, true, true).0,
            Mode::Background
        );
        assert_eq!(
            c(ModeArg::Auto, Busy::Working, true, false, false).0,
            Mode::Background
        );
        assert_eq!(
            c(ModeArg::Auto, Busy::Permission, true, true, false).0,
            Mode::Background
        );
        let (m, n) = c(ModeArg::Auto, Busy::Working, false, true, false);
        assert_eq!(m, Mode::Queue);
        assert!(n.unwrap().contains("Grok"));
        assert_eq!(
            c(ModeArg::Queue, Busy::Working, true, true, false).0,
            Mode::Queue
        );
        assert_eq!(
            c(ModeArg::Queue, Busy::Ready, true, true, false).0,
            Mode::Direct
        );
        assert_eq!(
            c(ModeArg::Side, Busy::Ready, true, false, true).0,
            Mode::Side
        );
        let (m, n) = c(ModeArg::Side, Busy::Permission, true, true, false);
        assert_eq!((m, n.is_some()), (Mode::Background, true));
        assert_eq!(
            choose(ModeArg::Side, Busy::Working, true, true, false, true, false).0,
            Mode::Background
        );
        assert_eq!(
            choose(ModeArg::Auto, Busy::Working, true, true, false, false, true).0,
            Mode::Background
        );
    }

    #[test]
    fn reads_the_side_panel() {
        let q = "what are you working on right now?";
        let typing = format!("working...\n\n╭──────╮\n│ > /btw {q} │\n╰──────╯");
        assert_eq!(scan_side(&typing, q), Scan::NotShown);
        let thinking = format!("working\n  /btw {q}\n  Answering…\n\n╭──────╮\n│ >  │\n╰──────╯");
        assert_eq!(
            scan_side(&thinking, q),
            Scan::Showing {
                answer: String::new(),
                footer: false
            }
        );
        let done = format!("working\n  /btw {q}\n  I am fixing the build.\n  Two tests are left.\n\n  Press Esc to dismiss\n╭──────╮\n│ >  │\n╰──────╯");
        assert_eq!(
            scan_side(&done, q),
            Scan::Showing {
                answer: "I am fixing the build.\nTwo tests are left.".into(),
                footer: true
            }
        );
        let unknown = "...\nbtw: Unknown command: /btw\n> ";
        assert!(matches!(scan_side(unknown, q), Scan::Unavailable(_)));
    }

    #[test]
    fn reads_the_background_id() {
        assert_eq!(
            parse_bg_id("started 516d52e6\n").as_deref(),
            Some("516d52e6")
        );
        assert_eq!(
            parse_bg_id("Background session 516d52e6-15ea-46d1-8318-ffaf31bc91cd").as_deref(),
            Some("516d52e6")
        );
        assert_eq!(parse_bg_id("nothing here"), None);
    }
}
