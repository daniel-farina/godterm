//! Conversation ownership in the app (docs/SYNC.md 4, Phase 3).
//!
//! With sync on, a tab resumes a session only once this machine holds its
//! lease: the resume waits for the lease worker (its own thread: fetch,
//! compare and swap, push), which also pulls the transcript first. A
//! session another live machine owns opens read only, with its transcript
//! and "Owned by MacBook, active 1 min ago", and two ways on: Request take
//! over (the owner lets go once idle) and Take over now (an explicit yes).
//!
//! When this machine loses a lease (taken over, or found taken after an
//! offline spell) input to the tab is blocked at once, the running turn
//! may end, then the CLI is stopped gracefully (SIGINT, a second SIGINT,
//! SIGTERM; never SIGKILL, as takeover.rs does) and whatever it wrote that
//! was never pushed becomes a forked session: "Moved to MacBook; your
//! unsent part is fork 1a2b3c4d". A request for one of ours is the same
//! stop, then a release with the final bytes pushed.
//!
//! Jobs fire only on the machine that owns their session. A tab moved to
//! another account keeps its session id and so its lease; a new session
//! id seen in a tab (a copy, a duplicate, a fresh start) takes a new one.
//! Nothing here touches a project repo.

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{App, AppEvent};
use crate::pane::{Activity, LaunchKind};
use crate::sync::lease::{Board, Status};
use crate::sync::takeover::{Cmd, Event};

/// Why a tab's CLI is being stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum StopWhy {
    /// Another machine has it now (`conflict`: the synced copy moved on
    /// while this machine worked offline, and the lease is still ours).
    Lost { conflict: bool },
    /// Another machine asked for it.
    Release,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StopStage {
    /// The turn may finish.
    WaitIdle(Instant),
    /// Interrupts sent so far (two SIGINTs, then SIGTERM).
    Signal { at: Instant, sent: u8 },
}

/// A tab whose session someone else holds (or this machine let go of).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReadOnly {
    pub sid: String,
    /// Who has it, in words ("MacBook").
    pub label: String,
    /// Its heartbeat (ms since 1970), 0 when unknown.
    pub beat_ms: i64,
    /// We asked for it and wait for the owner to let go.
    pub requested: bool,
    /// What happened ("Moved to MacBook; your unsent part is fork ...").
    pub note: Option<String>,
    /// The fork holding what this machine never pushed.
    pub fork: Option<String>,
    /// Decided offline (from the last board seen).
    pub offline: bool,
    /// Nobody live holds it: Enter resumes it here.
    pub resumable: bool,
    /// "Take over now?" is asked; y says yes.
    pub confirm: bool,
    /// A request or take over is on its way.
    pub busy: Option<String>,
    /// The transcript's last exchanges (user, reply), and when read.
    pub turns: Vec<(String, String)>,
    pub turns_stamp: Option<(u64, std::time::SystemTime)>,
}

/// A tab's lease state (None: sync off, or not a synced session).
#[derive(Debug, Clone, PartialEq)]
pub enum TabLease {
    /// The resume waits for the lease (and the pull before it).
    Waiting {
        sid: String,
        kind: LaunchKind,
    },
    /// Ours. `offline`: resumed from the last board seen ("offline: will
    /// fork if someone else took over").
    Owned {
        sid: String,
        offline: bool,
    },
    ReadOnly(ReadOnly),
    /// Input blocked; the CLI stops once the turn ends.
    Stopping {
        sid: String,
        why: StopWhy,
        label: String,
        stage: StopStage,
    },
}

impl TabLease {
    pub fn sid(&self) -> &str {
        match self {
            TabLease::Waiting { sid, .. }
            | TabLease::Owned { sid, .. }
            | TabLease::Stopping { sid, .. } => sid,
            TabLease::ReadOnly(r) => &r.sid,
        }
    }

    /// Typing into the tab goes nowhere (only Owned takes input).
    pub fn blocks_input(&self) -> bool {
        !matches!(self, TabLease::Owned { .. })
    }

    /// The banner over a tab's screen, if any.
    pub fn banner(&self) -> Option<String> {
        match self {
            TabLease::Owned { offline: true, .. } => {
                Some(" offline: will fork if someone else took over ".into())
            }
            TabLease::Stopping { label, why, .. } => Some(match why {
                StopWhy::Release => format!(
                    " {label} asked for this conversation: input is blocked; it moves there when this turn ends "
                ),
                StopWhy::Lost { conflict: false } => format!(
                    " {label} took over this conversation: input is blocked; the running turn may finish "
                ),
                StopWhy::Lost { conflict: true } => {
                    " The synced copy moved on while you were offline: input is blocked; your part becomes a fork ".into()
                }
            }),
            _ => None,
        }
    }
}

/// The app's side of the leases.
#[derive(Default)]
pub struct LeaseApp {
    pub cmd: Option<Sender<Cmd>>,
    /// Sessions run here as their owner (sid -> account), read by the
    /// lease worker on every poll.
    pub held: Arc<Mutex<BTreeMap<String, String>>>,
    /// The lease worker said ours.
    pub granted: BTreeSet<String>,
    /// Resumed offline.
    pub offline: BTreeSet<String>,
    pub board: Option<Board>,
    pub online: bool,
    pub board_at: Option<Instant>,
    /// This machine's id.
    pub me: String,
    /// Commands made when no worker runs (tests read them).
    pub sent: Vec<Cmd>,
    /// The sync's view of the app: which sessions' CLIs run here (never
    /// extended or forked under them).
    pub link: Option<crate::sync::Link>,
    /// Set by tests: leases on without the worker threads.
    pub test_on: bool,
    /// Asked for from here (sid -> account).
    pub requested: BTreeMap<String, String>,
}

/// "active 3 min ago".
pub fn active_words(beat_ms: i64, now_ms: i64) -> String {
    if beat_ms <= 0 {
        return "no heartbeat seen".into();
    }
    let m = (now_ms - beat_ms).max(0) / 60_000;
    match m {
        0 => "active just now".into(),
        1 => "active 1 min ago".into(),
        m if m < 120 => format!("active {m} min ago"),
        m => format!("active {} h ago", m / 60),
    }
}

/// The lease worker: one thread, never the UI's.
pub fn spawn(
    make: Arc<dyn Fn() -> anyhow::Result<crate::sync::Ctx> + Send + Sync>,
    tx: Sender<AppEvent>,
    held: Arc<Mutex<BTreeMap<String, String>>>,
) -> Sender<Cmd> {
    let (ctx_tx, rx) = std::sync::mpsc::channel::<Cmd>();
    let _ = std::thread::Builder::new()
        .name("sync-lease".into())
        .spawn(move || {
            let mut eng = crate::sync::takeover::Engine::new(held);
            // The first look at once: resumes at startup wait on it.
            let mut next = Instant::now();
            loop {
                let wait = next.saturating_duration_since(Instant::now());
                let got = rx.recv_timeout(wait);
                let ctx = match make() {
                    Ok(c) if c.sync().enabled => c,
                    _ => {
                        if matches!(got, Err(std::sync::mpsc::RecvTimeoutError::Disconnected)) {
                            return;
                        }
                        next = Instant::now() + Duration::from_secs(60);
                        continue;
                    }
                };
                let events = match got {
                    Ok(c) => {
                        let mut cmds = vec![c];
                        while let Ok(c) = rx.try_recv() {
                            cmds.push(c);
                        }
                        eng.handle(&ctx, cmds)
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        let e = eng.poll(&ctx);
                        next = Instant::now() + Duration::from_secs(eng.interval_s(&ctx));
                        e
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                };
                for e in events {
                    if tx.send(AppEvent::Lease(e)).is_err() {
                        return;
                    }
                }
            }
        });
    ctx_tx
}

impl App {
    /// Leases apply: sync is on (and this is not a demo).
    pub fn lease_on(&self) -> bool {
        (self.cfg.sync.enabled || self.lease.test_on) && !crate::demo::active()
    }

    pub(crate) fn lease_send(&mut self, c: Cmd) {
        // A resume at startup can come before the first tick started the
        // worker.
        if self.lease.cmd.is_none() {
            self.lease_start();
        }
        match &self.lease.cmd {
            Some(tx) if tx.send(c.clone()).is_ok() => {}
            _ => self.lease.sent.push(c),
        }
    }

    /// The account name of a slot, for a synced harness (Claude, Grok).
    fn lease_acct(&self, pane: usize) -> Option<String> {
        let a = self.panes.get(pane)?.account?;
        let acct = &self.cfg.accounts[a];
        matches!(
            acct.harness(),
            crate::harness::Harness::Claude | crate::harness::Harness::Grok
        )
        .then(|| acct.name.clone())
    }

    /// Before a resume in the active tab of `pane`: true when it must
    /// wait (the lease is asked for; the launch comes with the answer).
    pub(crate) fn lease_gate(&mut self, pane: usize, kind: &LaunchKind) -> bool {
        let LaunchKind::Resume(sid) = kind else {
            return false;
        };
        if !self.lease_on() || !crate::sync::lease::valid_id(sid) {
            return false;
        }
        let Some(acct) = self.lease_acct(pane) else {
            return false;
        };
        let granted = self.lease.granted.contains(sid);
        let offline = self.lease.offline.contains(sid);
        let t = self.panes[pane].cur_mut();
        if let Some(TabLease::Owned { sid: s, .. }) = &t.lease {
            if s == sid && granted {
                return false;
            }
        }
        if granted {
            t.lease = Some(TabLease::Owned {
                sid: sid.clone(),
                offline,
            });
            return false;
        }
        t.lease = Some(TabLease::Waiting {
            sid: sid.clone(),
            kind: kind.clone(),
        });
        t.pending = Some(kind.clone());
        self.lease_send(Cmd::Acquire {
            sid: sid.clone(),
            acct,
            pull: true,
        });
        true
    }

    /// A session id just seen in a running tab (a new conversation, a
    /// copy, a duplicate): its own lease.
    pub(crate) fn lease_claim(&mut self, s: usize, t: usize, sid: &str) {
        if !self.lease_on() || !crate::sync::lease::valid_id(sid) {
            return;
        }
        let Some(acct) = self.lease_acct(s) else {
            return;
        };
        let tab = &mut self.panes[s].tabs[t];
        if matches!(&tab.lease, Some(TabLease::Owned { sid: x, .. }) if x == sid) {
            return;
        }
        tab.lease = Some(TabLease::Owned {
            sid: sid.to_string(),
            offline: false,
        });
        if self.lease.granted.insert(sid.to_string()) {
            self.lease_send(Cmd::Acquire {
                sid: sid.to_string(),
                acct,
                pull: false,
            });
        }
    }

    /// Tabs whose lease names `sid`.
    fn lease_tabs(&self, sid: &str) -> Vec<(usize, usize)> {
        let mut v = vec![];
        for (s, p) in self.panes.iter().enumerate() {
            for (t, tab) in p.tabs.iter().enumerate() {
                if tab.lease.as_ref().is_some_and(|l| l.sid() == sid) {
                    v.push((s, t));
                }
            }
        }
        v
    }

    fn read_only_from(&self, sid: &str, st: &Status, offline: bool) -> ReadOnly {
        let (label, beat_ms, resumable) = match st {
            Status::Live { label, beat_ms, .. } => (label.clone(), *beat_ms, false),
            Status::Stale { label, beat_ms, .. } => (label.clone(), *beat_ms, true),
            other => (crate::sync::takeover::label_of(other), 0, true),
        };
        ReadOnly {
            sid: sid.to_string(),
            label,
            beat_ms,
            offline,
            resumable,
            ..Default::default()
        }
    }

    /// Something from the lease worker.
    pub fn lease_event(&mut self, e: Event) {
        match e {
            Event::Acquired {
                sid,
                offline,
                forked,
                warn,
                ..
            } => {
                self.lease.granted.insert(sid.clone());
                if offline {
                    self.lease.offline.insert(sid.clone());
                } else {
                    self.lease.offline.remove(&sid);
                }
                // Asked for from a tab that was closed since: let it go
                // again rather than hold it with nothing open.
                let tabs = self.lease_tabs(&sid);
                if tabs.is_empty() && !offline {
                    self.lease.granted.remove(&sid);
                    if let Some(acct) = self.lease.requested.remove(&sid) {
                        self.lease_send(Cmd::Release { sid, acct });
                    }
                    return;
                }
                for (s, t) in tabs {
                    let kind = match &self.panes[s].tabs[t].lease {
                        Some(TabLease::Waiting { kind, .. }) => kind.clone(),
                        Some(TabLease::ReadOnly(_)) => LaunchKind::Resume(sid.clone()),
                        Some(TabLease::Owned { .. }) => {
                            self.panes[s].tabs[t].lease = Some(TabLease::Owned {
                                sid: sid.clone(),
                                offline,
                            });
                            continue;
                        }
                        _ => continue,
                    };
                    self.panes[s].tabs[t].lease = Some(TabLease::Owned {
                        sid: sid.clone(),
                        offline,
                    });
                    if !self.panes[s].tabs[t].is_running() {
                        self.launch_tab(s, t, kind);
                    }
                }
                if let Some(f) = forked {
                    self.flash(format!(
                        "Lines this machine never pushed are kept as session {}",
                        crate::paths::head(&f, 8)
                    ));
                } else if let Some(w) = warn {
                    self.flash(w);
                }
            }
            Event::Held {
                sid,
                status,
                offline,
            } => {
                for (s, t) in self.lease_tabs(&sid) {
                    let cur = self.panes[s].tabs[t].lease.clone();
                    let running = self.panes[s].tabs[t].is_running();
                    match &cur {
                        Some(TabLease::Owned { .. }) if running => {
                            let label = crate::sync::takeover::label_of(&status);
                            self.lease_lost(&sid, &label, false);
                        }
                        Some(TabLease::Waiting { .. }) | Some(TabLease::ReadOnly(_)) => {
                            let mut ro = self.read_only_from(&sid, &status, offline);
                            if let Some(TabLease::ReadOnly(old)) = &cur {
                                ro.note = old.note.clone();
                                ro.fork = old.fork.clone();
                                ro.requested = old.requested;
                            }
                            self.panes[s].tabs[t].lease = Some(TabLease::ReadOnly(ro));
                            self.panes[s].tabs[t].web.stop_input();
                        }
                        _ => {}
                    }
                }
            }
            Event::Requested { sid, status } => {
                let label = crate::sync::takeover::label_of(&status);
                for (s, t) in self.lease_tabs(&sid) {
                    if let Some(TabLease::ReadOnly(r)) = &mut self.panes[s].tabs[t].lease {
                        r.requested = true;
                        r.busy = None;
                    }
                }
                self.flash(format!(
                    "Asked {label} for this conversation: it comes here once that turn ends"
                ));
            }
            Event::Lost {
                sid,
                label,
                conflict,
            } => self.lease_lost(&sid, &label, conflict),
            Event::Asked { sid, label } => {
                self.lease.granted.remove(&sid);
                let tabs = self.lease_tabs(&sid);
                let acct = self
                    .lease
                    .held
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&sid)
                    .cloned();
                let mut any_running = false;
                for (s, t) in tabs {
                    if self.panes[s].tabs[t].is_running() {
                        any_running = true;
                        self.lease_block(s, t, StopWhy::Release, &label);
                    } else {
                        self.panes[s].tabs[t].lease = Some(TabLease::ReadOnly(ReadOnly {
                            sid: sid.clone(),
                            label: label.clone(),
                            note: Some(format!("Releasing to {label}...")),
                            ..Default::default()
                        }));
                    }
                }
                if !any_running {
                    if let Some(acct) = acct {
                        self.lease_send(Cmd::Release { sid, acct });
                    }
                }
                self.flash(format!(
                    "{label} asked for a conversation open here: it moves there when its turn ends"
                ));
            }
            Event::Released { sid, to } => {
                self.lease.granted.remove(&sid);
                let label = to
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| "the other machine".into());
                for (s, t) in self.lease_tabs(&sid) {
                    self.panes[s].tabs[t].lease = Some(TabLease::ReadOnly(ReadOnly {
                        sid: sid.clone(),
                        label: label.clone(),
                        note: Some(format!("Released to {label} with every line pushed")),
                        resumable: true,
                        ..Default::default()
                    }));
                    self.panes[s].tabs[t].web.stop_input();
                }
            }
            Event::Forked {
                sid,
                fork,
                label,
                owner,
            } => {
                let note = match (&fork, owner) {
                    (Some(f), true) => format!(
                        "The synced copy moved on while you were offline; your part is fork {}",
                        crate::paths::head(f, 8)
                    ),
                    (Some(f), false) => format!(
                        "Moved to {label}; your unsent part is fork {}",
                        crate::paths::head(f, 8)
                    ),
                    (None, true) => "The synced copy moved on while you were offline; it is here now: Enter resumes it".to_string(),
                    (None, false) => format!("Moved to {label}"),
                };
                for (s, t) in self.lease_tabs(&sid) {
                    let mut ro = match &self.panes[s].tabs[t].lease {
                        Some(TabLease::ReadOnly(r)) => r.clone(),
                        _ => ReadOnly {
                            sid: sid.clone(),
                            label: label.clone(),
                            ..Default::default()
                        },
                    };
                    ro.note = Some(note.clone());
                    ro.fork = fork.clone();
                    ro.resumable = owner;
                    self.panes[s].tabs[t].lease = Some(TabLease::ReadOnly(ro));
                }
                self.flash(note);
                self.sync_poke("fork");
            }
            Event::Board { board, online } => {
                self.lease.online = online;
                self.lease.board_at = Some(Instant::now());
                let now = chrono::Utc::now().timestamp_millis();
                let lease_ms = self.cfg.sync.lease_s.max(1) as i64 * 1000;
                let me = self.lease.me.clone();
                for p in self.panes.iter_mut() {
                    for tab in p.tabs.iter_mut() {
                        if let Some(TabLease::ReadOnly(r)) = &mut tab.lease {
                            match board.status(&r.sid, &me, now, lease_ms) {
                                Status::Live { label, beat_ms, .. } => {
                                    r.label = label;
                                    r.beat_ms = beat_ms;
                                    r.resumable = false;
                                }
                                Status::Stale { label, beat_ms, .. } => {
                                    r.label = label;
                                    r.beat_ms = beat_ms;
                                    r.resumable = true;
                                }
                                _ => r.resumable = true,
                            }
                            r.offline = !online;
                        }
                    }
                }
                self.lease.board = Some(board);
            }
            Event::Failed { sid, why } => {
                for (s, t) in self.lease_tabs(&sid) {
                    let lease = &mut self.panes[s].tabs[t].lease;
                    if matches!(lease, Some(TabLease::Waiting { .. })) {
                        *lease = Some(TabLease::ReadOnly(ReadOnly {
                            sid: sid.clone(),
                            note: Some(why.clone()),
                            resumable: true,
                            ..Default::default()
                        }));
                    } else if let Some(TabLease::ReadOnly(r)) = lease {
                        r.busy = None;
                        r.note = Some(why.clone());
                    }
                }
                self.flash(format!("Sync: {why}"));
            }
        }
    }

    /// Not ours any more: block input, let the turn end, stop the CLI,
    /// keep what it never pushed as a fork.
    fn lease_lost(&mut self, sid: &str, label: &str, conflict: bool) {
        self.lease.granted.remove(sid);
        self.lease.offline.remove(sid);
        let acct = self
            .lease
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(sid)
            .cloned();
        let mut stopped_now = false;
        let mut any = false;
        for (s, t) in self.lease_tabs(sid) {
            any = true;
            if self.panes[s].tabs[t].is_running() {
                self.lease_block(s, t, StopWhy::Lost { conflict }, label);
            } else {
                stopped_now = true;
                self.panes[s].tabs[t].lease = Some(TabLease::ReadOnly(ReadOnly {
                    sid: sid.to_string(),
                    label: label.to_string(),
                    note: Some(format!("Moved to {label}; saving your unsent part...")),
                    ..Default::default()
                }));
            }
        }
        if stopped_now || !any {
            if let Some(acct) = acct.or_else(|| self.lease_acct_of(sid)) {
                self.lease_send(Cmd::Fork {
                    sid: sid.to_string(),
                    acct,
                    owner: conflict,
                    label: label.to_string(),
                });
            }
        }
        if any {
            self.flash(if conflict {
                "The synced copy moved on while you were offline: input to that tab is blocked; your part becomes a fork".to_string()
            } else {
                format!("{label} took over a conversation open here: input to it is blocked; the running turn may finish")
            });
        }
    }

    fn lease_acct_of(&self, sid: &str) -> Option<String> {
        let (s, _) = *self.lease_tabs(sid).first()?;
        self.lease_acct(s)
    }

    /// Input to the tab stops now; the CLI stops when the turn ends.
    fn lease_block(&mut self, s: usize, t: usize, why: StopWhy, label: &str) {
        let tab = &mut self.panes[s].tabs[t];
        let sid = tab
            .lease
            .as_ref()
            .map(|l| l.sid().to_string())
            .or_else(|| tab.session_id.clone())
            .unwrap_or_default();
        tab.lease = Some(TabLease::Stopping {
            sid,
            why,
            label: label.to_string(),
            stage: StopStage::WaitIdle(Instant::now()),
        });
        tab.web.stop_input();
    }

    /// Every tick: share what runs here, move stopping tabs along, keep
    /// read only transcripts fresh.
    pub fn lease_tick(&mut self) {
        if !self.lease_on() {
            return;
        }
        let mut held = BTreeMap::new();
        let mut running = BTreeSet::new();
        for (s, p) in self.panes.iter().enumerate() {
            for tab in &p.tabs {
                if tab.is_running() {
                    if let Some(id) = &tab.session_id {
                        running.insert(id.clone());
                    }
                }
                if let Some(TabLease::Owned { sid, .. }) = &tab.lease {
                    if let Some(a) = self.lease_acct(s) {
                        held.insert(sid.clone(), a);
                    }
                }
            }
        }
        *self.lease.held.lock().unwrap_or_else(|e| e.into_inner()) = held;
        if let Some(l) = &self.lease.link {
            *l.0.running.lock().unwrap_or_else(|e| e.into_inner()) = running;
        }
        // Stopping tabs.
        let mut done = vec![];
        for s in 0..self.panes.len() {
            for t in 0..self.panes[s].tabs.len() {
                if matches!(self.panes[s].tabs[t].lease, Some(TabLease::Stopping { .. }))
                    && self.lease_stop_step(s, t)
                {
                    done.push((s, t));
                }
            }
        }
        for (s, t) in done {
            let Some(TabLease::Stopping {
                sid, why, label, ..
            }) = self.panes[s].tabs[t].lease.clone()
            else {
                continue;
            };
            let Some(acct) = self.lease_acct(s) else {
                continue;
            };
            let note = match &why {
                StopWhy::Release => {
                    self.lease_send(Cmd::Release {
                        sid: sid.clone(),
                        acct,
                    });
                    format!("Releasing to {label}...")
                }
                StopWhy::Lost { conflict } => {
                    self.lease_send(Cmd::Fork {
                        sid: sid.clone(),
                        acct,
                        owner: *conflict,
                        label: label.clone(),
                    });
                    format!("Moved to {label}; saving your unsent part...")
                }
            };
            self.panes[s].tabs[t].lease = Some(TabLease::ReadOnly(ReadOnly {
                sid,
                label,
                note: Some(note),
                ..Default::default()
            }));
        }
        // Read only views: the transcript's last exchanges.
        for s in 0..self.panes.len() {
            let acct = self.lease_acct(s);
            for t in 0..self.panes[s].tabs.len() {
                let Some(TabLease::ReadOnly(r)) = &self.panes[s].tabs[t].lease else {
                    continue;
                };
                let Some(p) = acct
                    .as_deref()
                    .and_then(|a| self.lease_transcript(s, t, a, &r.sid))
                else {
                    continue;
                };
                let st = crate::takeover::stamp(&p);
                if r.turns_stamp == Some(st) {
                    continue;
                }
                let turns: Vec<(String, String)> = crate::sessions::recent_turns(&p, 6)
                    .into_iter()
                    .map(|x| (x.user, x.reply.unwrap_or_default()))
                    .collect();
                if let Some(TabLease::ReadOnly(r)) = &mut self.panes[s].tabs[t].lease {
                    r.turns = turns;
                    r.turns_stamp = Some(st);
                }
            }
        }
    }

    fn lease_transcript(
        &self,
        s: usize,
        t: usize,
        acct: &str,
        sid: &str,
    ) -> Option<std::path::PathBuf> {
        if let Some((p, id)) = self.transcript_of(s, t) {
            if id == sid {
                return Some(p);
            }
        }
        let a = self.cfg.accounts.iter().find(|a| a.name == acct)?;
        let dir = a.config_dir();
        let name = format!("{sid}.jsonl");
        std::fs::read_dir(dir.join("projects"))
            .ok()?
            .flatten()
            .map(|d| d.path().join(&name))
            .find(|p| p.is_file())
    }

    /// One step of a graceful stop; true once the CLI is gone.
    fn lease_stop_step(&mut self, s: usize, t: usize) -> bool {
        let tab = &mut self.panes[s].tabs[t];
        if !tab.is_running() {
            return true;
        }
        let working = tab.activity == Activity::Working;
        let pid = tab.pid();
        let Some(TabLease::Stopping { stage, .. }) = &mut tab.lease else {
            return false;
        };
        match stage.clone() {
            StopStage::WaitIdle(since) => {
                // A permission prompt waits for the user, who can no longer
                // answer here: that counts as the turn's end.
                if !working || crate::clock::age(since) > Duration::from_secs(1800) {
                    *stage = StopStage::Signal {
                        at: crate::clock::ago(Duration::from_secs(10)),
                        sent: 0,
                    };
                }
            }
            StopStage::Signal { at, sent } => {
                let wait = match sent {
                    0 => Duration::ZERO,
                    1 => Duration::from_millis(700),
                    2 => Duration::from_secs(3),
                    _ => return false,
                };
                if at.elapsed() < wait {
                    return false;
                }
                let Some(pid) = pid else {
                    return false;
                };
                if sent < 2 {
                    let ok = crate::procs::interrupt(pid);
                    crate::log::info(&format!(
                        "lease: Ctrl-C (SIGINT) to pid {pid}{}",
                        if ok { "" } else { " (not delivered)" }
                    ));
                    *stage = StopStage::Signal {
                        at: Instant::now(),
                        sent: crate::takeover::after_interrupt(sent, ok),
                    };
                } else {
                    // SIGTERM, it can still clean up; never SIGKILL.
                    crate::log::info(&format!("lease: SIGTERM to pid {pid}"));
                    crate::procs::terminate(pid);
                    *stage = StopStage::Signal {
                        at: Instant::now(),
                        sent: 3,
                    };
                }
            }
        }
        false
    }

    /// A key in a tab with a lease that blocks input. True when handled.
    pub(crate) fn lease_key(&mut self, s: usize, t: usize, k: KeyEvent) -> bool {
        let Some(l) = self.panes[s].tabs[t].lease.clone() else {
            return false;
        };
        if !l.blocks_input() {
            return false;
        }
        match l {
            TabLease::Waiting { .. } => {
                self.flash("Checking who owns this conversation...");
            }
            TabLease::Stopping { label, .. } => {
                self.flash(format!(
                    "Input is blocked: {label} has this conversation now"
                ));
            }
            TabLease::ReadOnly(r) => {
                let ch = match k.code {
                    KeyCode::Char(c) => Some(c),
                    _ => None,
                };
                if r.confirm {
                    let yes = matches!(ch, Some('y') | Some('Y'));
                    if let Some(TabLease::ReadOnly(x)) = &mut self.panes[s].tabs[t].lease {
                        x.confirm = false;
                    }
                    if yes {
                        let _ = self.lease_act(s, t, "take_over");
                    }
                    return true;
                }
                let op = match (ch, k.code) {
                    (Some('r'), _) => "request",
                    (Some('T') | Some('t'), _) => {
                        if let Some(TabLease::ReadOnly(x)) = &mut self.panes[s].tabs[t].lease {
                            x.confirm = true;
                        }
                        return true;
                    }
                    (Some('o'), _) => "offline",
                    (Some('f'), _) => "open_fork",
                    (_, KeyCode::Enter) => "resume",
                    _ => return true,
                };
                if let Err(e) = self.lease_act(s, t, op) {
                    self.flash(e);
                }
            }
            TabLease::Owned { .. } => return false,
        }
        true
    }

    /// The read only tab's actions (keys, the web's buttons): request,
    /// take_over (the caller made sure the user said yes), offline,
    /// open_fork, resume.
    pub fn lease_act(&mut self, s: usize, t: usize, op: &str) -> Result<String, String> {
        let Some(TabLease::ReadOnly(r)) = self.panes[s].tabs[t].lease.clone() else {
            return Err("this tab is not read only".into());
        };
        let acct = self
            .lease_acct(s)
            .ok_or("this tab's account does not sync")?;
        let sid = r.sid.clone();
        let busy = |app: &mut App, w: &str| {
            if let Some(TabLease::ReadOnly(x)) = &mut app.panes[s].tabs[t].lease {
                x.busy = Some(w.to_string());
            }
        };
        match op {
            "request" => {
                busy(self, "Asking for it...");
                self.lease.requested.insert(sid.clone(), acct.clone());
                self.lease_send(Cmd::Request { sid, acct });
                Ok(format!("Asked {} for it", r.label))
            }
            "take_over" => {
                busy(self, "Taking over...");
                self.lease_send(Cmd::TakeOverNow { sid, acct });
                Ok("Taking it over now".into())
            }
            "offline" => {
                if !r.offline {
                    return Err("only while offline".into());
                }
                self.lease_send(Cmd::Offline { sid, acct });
                Ok(
                    "Working offline: your part becomes a fork on reconnect if someone else has it"
                        .into(),
                )
            }
            "resume" => {
                if !r.resumable {
                    return Err(format!(
                        "{} has it: r asks for it, T takes it over now",
                        r.label
                    ));
                }
                self.panes[s].tabs[t].lease = Some(TabLease::Waiting {
                    sid: sid.clone(),
                    kind: LaunchKind::Resume(sid.clone()),
                });
                self.lease_send(Cmd::Acquire {
                    sid,
                    acct,
                    pull: true,
                });
                Ok("Resuming here".into())
            }
            "open_fork" => {
                let f = r.fork.clone().ok_or("there is no fork for this tab")?;
                let cwd = self.panes[s].tabs[t].cwd.clone();
                let ti = self.panes[s].add_tab(cwd);
                self.panes[s].tabs[ti].session_id = Some(f.clone());
                self.panes[s].active = ti;
                self.focus = s;
                self.launch(s, LaunchKind::Resume(f.clone()));
                self.state_dirty = true;
                Ok(format!("Opened fork {}", crate::paths::head(&f, 8)))
            }
            _ => Err(format!("unknown action {op}")),
        }
    }

    /// Why a job may not fire here (its session is owned elsewhere, or it
    /// belongs to another machine), if so.
    pub fn lease_job_block(&self, j: &crate::jobs::Job) -> Option<String> {
        if !self.lease_on() || self.lease.me.is_empty() {
            return None;
        }
        let me = &self.lease.me;
        match (&j.target.session_id, &j.machine) {
            (Some(sid), _) => {
                let b = self.lease.board.as_ref()?;
                let now = chrono::Utc::now().timestamp_millis();
                let lease_ms = self.cfg.sync.lease_s.max(1) as i64 * 1000;
                match b.status(sid, me, now, lease_ms) {
                    Status::Live { label, .. } => Some(format!(
                        "paused here: {label} owns its conversation (take it over to run it here)"
                    )),
                    Status::Stale { label, .. } => Some(format!(
                        "paused: {label} owns its conversation and is offline (take it over to run it here)"
                    )),
                    _ => None,
                }
            }
            (None, Some(m)) if !me.is_empty() && m != me => {
                let label = self
                    .lease
                    .board
                    .as_ref()
                    .map(|b| b.label_of(m))
                    .unwrap_or_else(|| crate::paths::head(m, 8).to_string());
                Some(format!("it runs on {label}"))
            }
            _ => None,
        }
    }

    /// The Sync screen's ownership lines.
    pub fn lease_lines(&self) -> Vec<String> {
        let Some(b) = &self.lease.board else {
            return vec!["Leases: not fetched yet".into()];
        };
        let now = chrono::Utc::now().timestamp_millis();
        let lease_ms = self.cfg.sync.lease_s.max(1) as i64 * 1000;
        let me = &self.lease.me;
        let mut here = vec![];
        let mut elsewhere = vec![];
        let mut pending = vec![];
        for (sid, l) in &b.leases {
            let name = self.lease_title(sid);
            match b.status(sid, me, now, lease_ms) {
                Status::Mine { takeover, .. } => {
                    here.push(format!("  {name}"));
                    if let Some(t) = takeover.filter(|t| &t.by != me) {
                        pending.push(format!("  {name}: {} asks for it", b.label_of(&t.by)));
                    }
                }
                Status::Live {
                    label,
                    beat_ms,
                    takeover,
                    ..
                } => {
                    elsewhere.push(format!("  {name}: {label}, {}", active_words(beat_ms, now)));
                    if takeover.is_some_and(|t| &t.by == me) {
                        pending.push(format!("  {name}: asked {label} for it"));
                    }
                }
                Status::Stale { label, beat_ms, .. } => elsewhere.push(format!(
                    "  {name}: {label} (offline, {})",
                    active_words(beat_ms, now)
                )),
                _ => {
                    let _ = l;
                }
            }
        }
        let mut v = vec![String::new()];
        v.push(format!(
            "Conversations owned here: {}{}",
            here.len(),
            if self.lease.online {
                ""
            } else {
                " (offline: as last seen)"
            }
        ));
        v.extend(here);
        v.push(format!("Owned elsewhere: {}", elsewhere.len()));
        v.extend(elsewhere);
        if !pending.is_empty() {
            v.push(format!("Take over requests: {}", pending.len()));
            v.extend(pending);
        }
        let paused: Vec<String> = self
            .jobs
            .jobs
            .iter()
            .filter_map(|j| {
                self.lease_job_block(j)
                    .map(|w| format!("  {}: {w}", j.name))
            })
            .collect();
        if !paused.is_empty() {
            v.push(format!("Paused jobs: {}", paused.len()));
            v.extend(paused);
        }
        v.push(String::new());
        v.push("Machines:".into());
        for (id, beat) in &b.beats {
            let ms = crate::sync::lease::ms_of(&beat.at);
            v.push(format!(
                "  {}{}: {}",
                b.label_of(id),
                if id == me { " (this one)" } else { "" },
                active_words(ms, now)
            ));
        }
        v
    }

    /// A session's name for lists: an open tab's, else its short id.
    fn lease_title(&self, sid: &str) -> String {
        for p in &self.panes {
            for t in &p.tabs {
                if t.session_id.as_deref() == Some(sid) {
                    return t.name();
                }
            }
        }
        crate::paths::head(sid, 8).to_string()
    }

    /// The web's view of a tab's lease (None: nothing to show).
    pub fn lease_json(&self, s: usize, t: usize) -> Option<Value> {
        let l = self.panes[s].tabs[t].lease.as_ref()?;
        let now = chrono::Utc::now().timestamp_millis();
        Some(match l {
            TabLease::Owned { offline: false, .. } => return None,
            TabLease::Owned { offline: true, sid } => {
                json!({"state": "offline", "session": sid, "banner": l.banner()})
            }
            TabLease::Waiting { sid, .. } => json!({"state": "waiting", "session": sid}),
            TabLease::Stopping { sid, label, .. } => {
                json!({"state": "stopping", "session": sid, "owner": label, "banner": l.banner()})
            }
            TabLease::ReadOnly(r) => json!({
                "state": "read_only", "session": r.sid, "owner": r.label,
                "active": active_words(r.beat_ms, now), "requested": r.requested,
                "note": r.note, "fork": r.fork, "offline": r.offline, "resumable": r.resumable,
                "busy": r.busy,
            }),
        })
    }

    /// `sync_lease`: the user's own press on a read only tab (the web's
    /// buttons; its confirm dialog is the yes for take_over). Never the
    /// assistant's: it has take_over and release_session, which ask.
    pub fn sync_lease_tool(&mut self, args: &Value, from_user: bool) -> Result<Value, String> {
        if !from_user {
            return Err(
                "sync_lease is the user's own button; ask with take_over or release_session".into(),
            );
        }
        let uid = args["tab"]
            .as_u64()
            .or_else(|| {
                args["tab"]
                    .as_str()
                    .and_then(|x| x.trim_start_matches('t').parse().ok())
            })
            .ok_or("tab: the tab's id")?;
        let (s, t) = self.find_tab(uid).ok_or("no such tab")?;
        let op = args["op"].as_str().unwrap_or("");
        let say = self.lease_act(s, t, op)?;
        Ok(json!({"ok": true, "result": {"say": say}}))
    }

    /// A store the app holds, changed on another machine: applied here and
    /// saved by the app itself (a sync never writes it under the app).
    pub fn sync_apply(&mut self, a: crate::sync::StoreApply, reply: Sender<bool>) {
        let ok = match a.store {
            "archive" => serde_json::to_value(&self.archive)
                .ok()
                .and_then(|v| serde_json::from_value(a.apply_to(&v)).ok())
                .map(|st: crate::archive::Store| {
                    self.archive = st;
                    self.archive.save().is_ok()
                })
                .unwrap_or(false),
            "jobs" => serde_json::to_value(&self.jobs)
                .ok()
                .and_then(|v| serde_json::from_value(a.apply_to(&v)).ok())
                .map(|st: crate::jobs::Store| {
                    self.jobs = st;
                    self.jobs.save().is_ok()
                })
                .unwrap_or(false),
            _ => false,
        };
        if ok {
            self.flash("Sync: changes from another machine applied");
        }
        let _ = reply.send(ok);
    }
}

impl App {
    /// The tab here ("t12") that holds a conversation, if one does.
    fn lease_tab_of(&self, sid: &str) -> Option<String> {
        self.panes.iter().flat_map(|p| p.tabs.iter()).find_map(|t| {
            let same = t.session_id.as_deref() == Some(sid)
                || t.lease.as_ref().is_some_and(|l| l.sid() == sid);
            same.then(|| format!("t{}", t.uid))
        })
    }

    /// The ownership part of sync_status (the web's Settings > Sync).
    pub fn lease_status_json(&self) -> Value {
        let Some(b) = &self.lease.board else {
            return json!({"fetched": false});
        };
        let now = chrono::Utc::now().timestamp_millis();
        let lease_ms = self.cfg.sync.lease_s.max(1) as i64 * 1000;
        let me = &self.lease.me;
        let mut here = vec![];
        let mut elsewhere = vec![];
        let mut requests = vec![];
        for sid in b.leases.keys() {
            let name = self.lease_title(sid);
            match b.status(sid, me, now, lease_ms) {
                Status::Mine { takeover, .. } => {
                    let asked = takeover.filter(|t| &t.by != me).map(|t| b.label_of(&t.by));
                    if let Some(by) = &asked {
                        requests.push(
                            json!({"session": sid, "name": name, "by": by, "direction": "in"}),
                        );
                    }
                    here.push(json!({"session": sid, "name": name, "asked_by": asked, "tab": self.lease_tab_of(sid)}));
                }
                Status::Live {
                    label,
                    beat_ms,
                    takeover,
                    ..
                } => {
                    if takeover.is_some_and(|t| &t.by == me) {
                        requests.push(
                            json!({"session": sid, "name": name, "to": label, "direction": "out"}),
                        );
                    }
                    elsewhere.push(json!({"session": sid, "name": name, "owner": label,
                        "active": active_words(beat_ms, now), "live": true, "tab": self.lease_tab_of(sid)}));
                }
                Status::Stale { label, beat_ms, .. } => {
                    elsewhere.push(json!({"session": sid, "name": name, "owner": label,
                        "active": active_words(beat_ms, now), "live": false, "tab": self.lease_tab_of(sid)}));
                }
                _ => {}
            }
        }
        let machines: Vec<Value> = b
            .beats
            .iter()
            .map(|(id, h)| {
                let ms = crate::sync::lease::ms_of(&h.at);
                json!({"id": crate::paths::head(id, 8), "label": b.label_of(id), "this": id == me,
                       "active": active_words(ms, now), "live": now - ms < lease_ms})
            })
            .collect();
        let paused: Vec<Value> = self
            .jobs
            .jobs
            .iter()
            .filter_map(|j| {
                self.lease_job_block(j)
                    .map(|w| json!({"id": j.id, "name": j.name, "why": w}))
            })
            .collect();
        json!({"fetched": true, "online": self.lease.online, "owned_here": here,
               "owned_elsewhere": elsewhere, "requests": requests, "machines": machines,
               "paused_jobs": paused})
    }

    /// take_over / release_session for the assistant: the session and
    /// its tab, and the question (always asked: a later turn's yes runs
    /// it).
    pub(crate) fn lease_plan_for(
        &mut self,
        tool: &str,
        args: &Value,
    ) -> Option<Result<(Value, String, bool), String>> {
        if tool != "take_over" && tool != "release_session" {
            return None;
        }
        Some((|| {
            if !self.lease_on() {
                return Err("sync is off on this machine".to_string());
            }
            let (s, t) = self.one_tab(args)?;
            let tab = &self.panes[s].tabs[t];
            let name = tab.name();
            let sid = tab
                .lease
                .as_ref()
                .map(|l| l.sid().to_string())
                .or_else(|| tab.session_id.clone())
                .ok_or("that tab has no conversation")?;
            let plan = json!({"tab": tab.uid, "session": sid});
            match (tool, &tab.lease) {
                ("take_over", Some(TabLease::ReadOnly(r))) => Ok((
                    plan,
                    format!(
                        "Take over \"{name}\" from {} now? It stops writing it there, and anything it never pushed becomes a fork.",
                        r.label
                    ),
                    true,
                )),
                ("take_over", _) => Err("that conversation is already this machine's".into()),
                ("release_session", Some(TabLease::Owned { .. })) => Ok((
                    plan,
                    format!("Hand \"{name}\" over now? Its tab here stops once the current turn ends, then another machine can take it."),
                    true,
                )),
                _ => Err("that conversation is not owned here".into()),
            }
        })())
    }

    pub(crate) fn lease_run_plan(
        &mut self,
        tool: &str,
        plan: &Value,
    ) -> Option<Result<Value, String>> {
        if tool != "take_over" && tool != "release_session" {
            return None;
        }
        Some((|| {
            let uid = plan["tab"].as_u64().ok_or("bad plan")?;
            let (s, t) = self.find_tab(uid).ok_or("that tab is gone")?;
            let say = if tool == "take_over" {
                self.lease_act(s, t, "take_over")?
            } else {
                if self.panes[s].tabs[t].is_running() {
                    self.lease_block(s, t, StopWhy::Release, "another machine");
                } else if let Some(acct) = self.lease_acct(s) {
                    let sid = plan["session"].as_str().unwrap_or("").to_string();
                    self.lease_send(Cmd::Release { sid, acct });
                }
                "It is handed over once the current turn ends.".to_string()
            };
            Ok(json!({"ok": true, "result": {"say": say}}))
        })())
    }
}
