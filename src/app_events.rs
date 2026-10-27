use std::time::{Duration, Instant};

use crate::app::App;
use crate::pane::Activity;

pub const EVENT_GAP: Duration = Duration::from_secs(60);

const RETRY: Duration = Duration::from_secs(30);

const KEEP: Duration = Duration::from_secs(2 * 3600);

const MAX: usize = 30;

const RESULT_CHARS: usize = 400;

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    Finished,

    Approval(String),

    Update,
}

#[derive(Debug, Clone)]
pub struct TabEvent {
    pub uid: u64,
    pub tab: String,
    pub project: String,
    pub kind: EventKind,

    pub text: String,
    pub at: chrono::DateTime<chrono::Local>,
    pub born: Instant,

    pub delivered: bool,
}

impl TabEvent {
    pub fn line(&self) -> String {
        let who = format!("t{} ({}, {})", self.uid, self.tab, self.project);
        match &self.kind {
            EventKind::Finished if self.text.is_empty() => format!("{who} finished its turn."),
            EventKind::Finished => {
                format!(
                    "{who} finished its turn; its last message: \"{}\"",
                    self.text
                )
            }
            EventKind::Approval(what) => {
                format!("{who} waits for the user's approval: {what}")
            }
            EventKind::Update => format!("update mode news: {}", self.text),
        }
    }

    fn word(&self) -> &'static str {
        match self.kind {
            EventKind::Finished => "finished",
            EventKind::Approval(_) => "approval",
            EventKind::Update => "update",
        }
    }
}

#[derive(Debug, Default)]
pub struct Inbox {
    pub events: Vec<TabEvent>,

    pub last_turn: Option<Instant>,
    pub retry_at: Option<Instant>,
}

impl App {
    pub fn assistant_tab_event(&mut self, uid: u64, kind: EventKind, text: String) {
        let Some((s, t)) = self.find_tab(uid) else {
            return;
        };
        let tab = self.spoken_tab_name(uid);
        let project = crate::config::tilde(&self.panes[s].tabs[t].cwd);
        let text = crate::sessions::snippet(text.trim(), RESULT_CHARS);
        let inbox = &mut self.assistant.events;

        if inbox
            .events
            .iter()
            .any(|e| !e.delivered && e.uid == uid && e.kind == kind && e.text == text)
        {
            return;
        }
        crate::log::info(&format!(
            "events: t{uid} {} for the assistant",
            match &kind {
                EventKind::Finished => "finished",
                EventKind::Approval(_) => "needs approval",
                EventKind::Update => "update",
            }
        ));
        inbox.events.push(TabEvent {
            uid,
            tab,
            project,
            kind,
            text,
            at: chrono::Local::now(),
            born: Instant::now(),
            delivered: false,
        });
        let n = inbox.events.len();
        if n > MAX {
            inbox.events.drain(..n - MAX);
        }
    }

    pub fn update_event(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        crate::log::info("events: update mode news for the assistant");
        let inbox = &mut self.assistant.events;
        inbox.events.push(TabEvent {
            uid: 0,
            tab: "update mode".into(),
            project: String::new(),
            kind: EventKind::Update,
            text: crate::sessions::snippet(text, 2 * RESULT_CHARS),
            at: chrono::Local::now(),
            born: Instant::now(),
            delivered: false,
        });
        let n = inbox.events.len();
        if n > MAX {
            inbox.events.drain(..n - MAX);
        }
    }

    pub fn tab_finished_event(&mut self, uid: u64) {
        if let Some((s, t)) = self.find_tab(uid) {
            let cwd = self.panes[s].tabs[t].cwd.clone();
            self.git_tab_finished(&cwd);
        }
        let last = self
            .find_tab(uid)
            .and_then(|(s, t)| self.transcript_path(s, t))
            .and_then(|p| crate::sessions::last_assistant_text(&p))
            .unwrap_or_default();
        self.assistant_tab_event(uid, EventKind::Finished, last);
    }

    pub fn tab_approval_event(&mut self, uid: u64) {
        let Some((s, t)) = self.find_tab(uid) else {
            return;
        };
        let what = self
            .request_of(s, t)
            .map(|r| r.summary())
            .unwrap_or_else(|| "a permission prompt".into());
        self.assistant_tab_event(uid, EventKind::Approval(what), String::new());
    }

    fn event_live(&self, e: &TabEvent) -> bool {
        match &e.kind {
            EventKind::Approval(_) => self
                .find_tab(e.uid)
                .is_some_and(|(s, t)| self.panes[s].tabs[t].activity == Activity::Permission),
            EventKind::Finished => !self.assistant.follow_ups.iter().any(|f| {
                f.uid == e.uid
                    && matches!(
                        f.state,
                        crate::app_followup::FuState::Waiting
                            | crate::app_followup::FuState::Answered { .. }
                    )
            }),
            EventKind::Update => true,
        }
    }

    pub fn events_tick(&mut self) {
        self.assistant
            .events
            .events
            .retain(|e| !e.delivered || e.born.elapsed() < KEEP);

        let stale: Vec<usize> = (0..self.assistant.events.events.len())
            .filter(|&i| {
                let e = &self.assistant.events.events[i];
                !e.delivered && !self.event_live(e)
            })
            .collect();
        for i in stale {
            self.assistant.events.events[i].delivered = true;
        }
        let pending: Vec<usize> = (0..self.assistant.events.events.len())
            .filter(|&i| !self.assistant.events.events[i].delivered)
            .collect();
        if pending.is_empty() {
            return;
        }

        if self.direct.on.is_some() || self.brainstorm_on() || self.paused() {
            return;
        }
        if !self.quiet_moment() || self.brain_account().is_none() {
            return;
        }
        let now = Instant::now();
        if self.assistant.events.retry_at.is_some_and(|t| now < t) {
            return;
        }
        let urgent = pending
            .iter()
            .any(|&i| self.assistant.events.events[i].kind != EventKind::Finished);
        if !urgent
            && self
                .assistant
                .events
                .last_turn
                .is_some_and(|t| now.duration_since(t) < EVENT_GAP)
        {
            return;
        }
        let lines: Vec<String> = pending
            .iter()
            .map(|&i| format!("- {}", self.assistant.events.events[i].line()))
            .collect();
        let updates = pending
            .iter()
            .any(|&i| self.assistant.events.events[i].kind == EventKind::Update);
        let msg = format!(
            "(Events from GodTerm, not from the user. Tabs never talk to the user themselves; you decide what the user hears.)\n{}\n\
             Tell the user, in one or two short spoken sentences, only what they need now: an approval that waits for them (which tab, what it asks){}, an error, a question a tab asks them, or a result they were waiting for. \
             Routine finished turns need nothing: then call ignore with reason \"nothing the user needs now\" and say nothing. Never mention subagents. Call no other tools. \
             Keep these facts: the user may ask about them later.",
            lines.join("\n"),
            if updates {
                ", update mode news (the user turned update mode on to be kept posted: say it briefly)"
            } else {
                ""
            }
        );
        let names: Vec<String> = pending
            .iter()
            .map(|&i| {
                let e = &self.assistant.events.events[i];
                format!("t{} {}", e.uid, e.word())
            })
            .collect();
        let note = format!("(events: {})", names.join(", "));
        crate::log::info(&format!("events: to the assistant: {}", names.join(", ")));
        self.ask_assistant_system(&note, &msg);
        if !self.assistant.busy {
            crate::log::info("events: the assistant did not start; they wait");
            self.assistant.events.retry_at = Some(now + RETRY);
            return;
        }
        self.assistant.events.retry_at = None;
        self.assistant.events.last_turn = Some(now);
        for i in pending {
            self.assistant.events.events[i].delivered = true;
        }
    }

    pub fn events_line(&self) -> String {
        let items: Vec<String> = self
            .assistant
            .events
            .events
            .iter()
            .rev()
            .take(8)
            .map(|e| {
                format!(
                    "{} {}{}",
                    e.at.format("%H:%M"),
                    e.line(),
                    if e.delivered { "" } else { " (not told yet)" }
                )
            })
            .collect();
        if items.is_empty() {
            String::new()
        } else {
            format!("recent_tab_events (newest first): {}\n", items.join(" | "))
        }
    }
}
