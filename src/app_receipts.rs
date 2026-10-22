//! Receipts for the user's requests to tabs: each send_prompt delivery
//! goes queued, received, started and done, from what the tab's own
//! transcript shows (receipts.rs), or dropped or interrupted, said out
//! loud at once so the user can resend. Before this, a request typed into
//! a busy tab counted as delivered once claude was working; one that
//! claude's queue then lost (Esc pulled it back into the input box, claude
//! restarted with it queued, Enter never registered) or whose turn was
//! interrupted went unnoticed, and GodTerm's own queue died with it on a
//! restart. Pending requests are saved with the state and come back.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_assistant::{Entry, Who};
use crate::app_quiet::Voice;
use crate::control::{tab_id, Delivery, Stage};
use crate::pane::Activity;
use crate::receipts::{first_words, Receipt, Seen};

/// The transcript is read at most this often per request.
const CHECK_EVERY: Duration = Duration::from_millis(700);
/// Typed in and taken, but not in the transcript after this: dropped.
pub const NOT_SEEN: Duration = Duration::from_secs(30);
/// A drop the transcript shows counts once it stays (it may be queued again).
const DROP_SETTLE: Duration = Duration::from_secs(3);
/// Started, the reply has no tool call after it and the tab sits at its
/// prompt this long: done.
const DONE_QUIET: Duration = Duration::from_secs(3);
/// Received (in claude's queue), but the tab sits at its prompt this
/// long without running it: claude let it go.
const QUEUE_IDLE: Duration = Duration::from_secs(15);
/// After Escape, the tab is looked at once it is back at its prompt.
const ESC_CHECK: Duration = Duration::from_secs(2);

/// The receipt side of a delivery.
#[derive(Debug, Clone)]
pub struct Track {
    pub receipt: Receipt,
    /// When the receipt last changed.
    pub receipt_at: Instant,
    pub created_at: chrono::DateTime<chrono::Local>,
    /// When it was typed in (wall clock ms, for the transcript).
    pub sent_ms: Option<i64>,
    pub sent_at: Option<Instant>,
    /// The tab was busy when it was asked for, or claude queued it.
    pub busy: bool,
    /// A request from the user (send_prompt, open_tab, resend): its
    /// receipts are said and shown. Internal prompts are only logged.
    pub notify: bool,
    pub checked: Option<Instant>,
    /// A transcript was found for its tab.
    pub has_transcript: bool,
    /// The transcript has shown it dropped since then.
    pub drop_seen: Option<Instant>,
    /// The tab has been at its prompt since then.
    pub ready_since: Option<Instant>,
    /// Escape or Ctrl-C was pressed in its tab then.
    pub esc_at: Option<Instant>,
    /// Sent again as this delivery.
    pub resent_as: Option<u64>,
    /// The tab's process when claude queued it (a restart loses the queue).
    pub proc_started: Option<SystemTime>,
    /// Came back from the saved state after a restart.
    pub restored: bool,
    /// The transcript's length when it was typed in (read from there).
    pub from_offset: Option<u64>,
    /// How it went into the tab (direct, queue, side, background).
    pub mode: crate::app_busy::Mode,
    /// Why that mode, when it is not what was asked (a fallback).
    pub note: Option<String>,
}

impl Track {
    pub fn new(busy: bool) -> Track {
        Track {
            receipt: Receipt::Pending,
            receipt_at: Instant::now(),
            created_at: chrono::Local::now(),
            sent_ms: None,
            sent_at: None,
            busy,
            notify: false,
            checked: None,
            has_transcript: false,
            drop_seen: None,
            ready_since: None,
            esc_at: None,
            resent_as: None,
            proc_started: None,
            restored: false,
            from_offset: None,
            mode: crate::app_busy::Mode::Direct,
            note: None,
        }
    }

    /// Worth keeping: not final, or ended a little while ago (a lost one
    /// waits an hour for a resend).
    pub fn keep(&self) -> bool {
        let keep = if self.receipt.lost() && self.resent_as.is_none() {
            3600
        } else {
            900
        };
        !self.receipt.is_final() || self.receipt_at.elapsed() < Duration::from_secs(keep)
    }

    /// Typed in, its transcript can confirm it and has not yet: a tool
    /// reply waits a moment for that.
    pub fn awaiting_receipt(&self, stage: &Stage) -> bool {
        *stage == Stage::Delivered && self.has_transcript && self.receipt == Receipt::Pending
    }

    /// Lost and not sent again.
    pub fn resendable(&self) -> bool {
        self.receipt.lost() && self.resent_as.is_none()
    }
}

impl Delivery {
    /// Its state for the user: queued, sending, sent, received, started,
    /// done, dropped, interrupted or unconfirmed.
    pub fn state_word(&self) -> &'static str {
        match &self.rc.receipt {
            Receipt::Pending => match self.stage {
                Stage::Queued => "queued",
                Stage::Pasted(_) | Stage::Submitted { .. } => "sending",
                Stage::Delivered | Stage::Failed(_) => "sent",
            },
            Receipt::Received => "received",
            Receipt::Started => "started",
            Receipt::Done => "done",
            Receipt::Dropped(_) => "dropped",
            Receipt::Interrupted(_) => "interrupted",
            Receipt::Unconfirmed => "unconfirmed",
        }
    }

    /// Saved with the state: still to be run (queued in GodTerm, typed in
    /// but not seen, or in claude's queue).
    pub fn pending_for_save(&self) -> bool {
        matches!(self.rc.receipt, Receipt::Pending | Receipt::Received)
            && !matches!(self.stage, Stage::Failed(_))
            && !self.rc.mode.external()
    }
}

/// A receipt change to announce.
struct Change {
    id: u64,
    uid: u64,
    name: String,
    text: String,
    notify: bool,
    busy: bool,
    mode: crate::app_busy::Mode,
    from: Receipt,
    to: Receipt,
}

fn word(r: &Receipt) -> &'static str {
    match r {
        Receipt::Pending => "pending",
        Receipt::Received => "received",
        Receipt::Started => "started",
        Receipt::Done => "done",
        Receipt::Dropped(_) => "dropped",
        Receipt::Interrupted(_) => "interrupted",
        Receipt::Unconfirmed => "unconfirmed",
    }
}

/// The bottom of a tab's screen (its input box), where unsent text sits.
fn input_area(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let k = lines.len().saturating_sub(8);
    lines[k..].join("\n")
}

impl App {
    /// Follow every request's receipt one step, and say what changed.
    pub fn receipts_tick(&mut self) {
        if self.deliveries.is_empty() {
            return;
        }
        let mut ds = std::mem::take(&mut self.deliveries);
        let mut changes = vec![];
        for d in &mut ds {
            if d.rc.receipt.is_final() {
                continue;
            }
            let Some(to) = self.receipt_next(d) else {
                continue;
            };
            if to == d.rc.receipt {
                continue;
            }
            let uid = self.current_uid(d.uid);
            let name = self.spoken_tab_name(uid);
            if to == Receipt::Received {
                d.rc.busy = true;
            }
            changes.push(Change {
                id: d.id,
                uid,
                name,
                text: d.text.clone(),
                notify: d.rc.notify,
                busy: d.rc.busy,
                mode: d.rc.mode,
                from: d.rc.receipt.clone(),
                to: to.clone(),
            });
            d.rc.receipt = to;
            d.rc.receipt_at = Instant::now();
        }
        // Deliveries made meanwhile (none here, but keep any).
        ds.append(&mut self.deliveries);
        self.deliveries = ds;
        if !changes.is_empty() {
            self.state_dirty = true;
        }
        for c in changes {
            let src = crate::told::Source {
                tab: Some(c.uid),
                account: self
                    .find_tab(c.uid)
                    .and_then(|(s, _)| self.panes[s].account),
                fact: Some(format!(
                    "request #{} to {} {} to {}",
                    c.id,
                    tab_id(c.uid),
                    word(&c.from),
                    word(&c.to)
                )),
            };
            self.told_from("delivery receipt", src, |app| app.announce_receipt(c));
        }
    }

    /// The next receipt for one request, from its tab and transcript.
    fn receipt_next(&self, d: &mut Delivery) -> Option<Receipt> {
        if d.rc.mode.external() {
            return self.external_receipt(d);
        }
        let started = d.rc.receipt == Receipt::Started;
        let uid = self.current_uid(d.uid);
        let Some((s, t)) = self.find_tab(uid) else {
            return Some(if started {
                Receipt::Interrupted("the tab was closed before it finished".into())
            } else {
                Receipt::Dropped("the tab was closed".into())
            });
        };
        let Some(sent_ms) = d.rc.sent_ms else {
            // Never typed in.
            return match &d.stage {
                Stage::Failed(why) => Some(Receipt::Dropped(format!("it never went in: {why}"))),
                _ => None,
            };
        };
        let tab = &self.panes[s].tabs[t];
        // Only Claude's transcript gives queue records; Grok and the other
        // CLIs: delivered or not.
        let grok = self.panes[s]
            .account
            .is_some_and(|a| self.cfg.accounts[a].harness() != crate::harness::Harness::Claude);
        if grok {
            // Grok's session files give no queue records: delivered or not.
            return match &d.stage {
                Stage::Delivered => Some(Receipt::Unconfirmed),
                Stage::Failed(why) => Some(Receipt::Dropped(why.clone())),
                _ => None,
            };
        }
        if d.rc.checked.is_some_and(|c| c.elapsed() < CHECK_EVERY) {
            return None;
        }
        d.rc.checked = Some(Instant::now());
        let path = self.transcript_path(s, t);
        d.rc.has_transcript = path.is_some();
        let scan = path
            .as_ref()
            .map(|p| crate::receipts::scan(p, &d.text, sent_ms, d.rc.from_offset));
        let seen = scan
            .as_ref()
            .map(|x| x.seen.clone())
            .unwrap_or(Seen::Nothing);
        let quiet = scan.as_ref().is_some_and(|x| x.turn_quiet);
        if !matches!(seen, Seen::Dropped(_)) {
            d.rc.drop_seen = None;
        }
        if tab.activity == Activity::Ready {
            d.rc.ready_since.get_or_insert_with(Instant::now);
        } else {
            d.rc.ready_since = None;
        }
        let ready_for =
            |rc: &Track, dur: Duration| rc.ready_since.is_some_and(|r| r.elapsed() >= dur);
        let after_esc =
            d.rc.esc_at
                .is_some_and(|e| e.elapsed() >= ESC_CHECK && ready_for(&d.rc, Duration::ZERO));
        let in_box = || crate::control::shows_prompt(&input_area(&self.tab_screen(s, t)), &d.text);
        match seen {
            Seen::Interrupted => Some(Receipt::Interrupted(
                "its turn was interrupted before it finished".into(),
            )),
            Seen::Done => Some(Receipt::Done),
            Seen::Started { .. } => {
                if !tab.is_running() {
                    Some(Receipt::Interrupted(
                        "the tab's claude exited before it finished".into(),
                    ))
                } else if started && quiet && ready_for(&d.rc, DONE_QUIET) {
                    Some(Receipt::Done)
                } else {
                    Some(Receipt::Started)
                }
            }
            Seen::Dropped(why) => {
                let at = *d.rc.drop_seen.get_or_insert_with(Instant::now);
                (at.elapsed() >= DROP_SETTLE).then_some(Receipt::Dropped(why))
            }
            Seen::Enqueued => {
                if d.rc.receipt == Receipt::Pending {
                    d.rc.proc_started = Some(tab.started);
                    return Some(Receipt::Received);
                }
                // Claude's queue lives in its memory: an exit or a restart
                // loses it.
                if !tab.is_running() || d.rc.proc_started.is_some_and(|p| p != tab.started) {
                    return Some(Receipt::Dropped(
                        "claude restarted or exited with it still in its queue".into(),
                    ));
                }
                if after_esc && in_box() {
                    return Some(Receipt::Dropped(
                        "Escape put it back in the tab's input box, unsent".into(),
                    ));
                }
                if ready_for(&d.rc, QUEUE_IDLE) {
                    return Some(Receipt::Dropped(if in_box() {
                        "it sits in the tab's input box, unsent".into()
                    } else {
                        "claude's queue let it go without running it".into()
                    }));
                }
                None
            }
            Seen::Nothing => {
                if let Stage::Failed(why) = &d.stage {
                    return Some(Receipt::Dropped(why.clone()));
                }
                if d.stage != Stage::Delivered {
                    return None;
                }
                if after_esc && path.is_some() && in_box() {
                    return Some(Receipt::Dropped(
                        "Escape left it in the tab's input box, unsent".into(),
                    ));
                }
                if d.rc.sent_at.is_some_and(|a| a.elapsed() >= NOT_SEEN) {
                    return Some(if path.is_some() {
                        Receipt::Dropped("it never showed up in the tab's conversation".into())
                    } else {
                        Receipt::Unconfirmed
                    });
                }
                None
            }
        }
    }

    /// Log a change, and tell the user: received and started briefly
    /// (only when the tab was busy) at a quiet moment, a lost request at
    /// once.
    fn announce_receipt(&mut self, c: Change) {
        let tab = tab_id(c.uid);
        // Every step on the way is logged (a jump from pending to done
        // went through received and started).
        let steps: Vec<&str> = match (&c.from, &c.to) {
            (Receipt::Pending, Receipt::Started) => vec!["received", "started"],
            (Receipt::Pending, Receipt::Done) => vec!["received", "started", "done"],
            (Receipt::Received, Receipt::Done) => vec!["started", "done"],
            _ => vec![word(&c.to)],
        };
        let why = c.to.reason().map(|r| format!(": {r}")).unwrap_or_default();
        crate::log::info(&format!(
            "receipt: #{} {tab} {} -> {}{why}",
            c.id,
            word(&c.from),
            steps.join(" -> ")
        ));
        if !c.notify {
            if c.to.lost() {
                self.assistant_note(format!("Prompt #{} to {tab} {}{why}", c.id, word(&c.to)));
                // Nothing was said: update mode tells it.
                self.updates_note_dropped(c.uid);
            }
            return;
        }
        let words = first_words(&c.text);
        if c.mode == crate::app_busy::Mode::Background && c.to == Receipt::Received {
            // The user hears what happened plainly; the result follows.
            self.say_update(
                Voice::Reply,
                &if c.busy {
                    format!(
                        "{} is busy, so I started a background session for it.",
                        c.name
                    )
                } else {
                    format!(
                        "I started a background session from {}'s conversation.",
                        c.name
                    )
                },
            );
            return;
        }
        if c.mode.external() && !c.to.lost() {
            // Side answers and background results arrive as follow-ups.
            return;
        }
        match &c.to {
            Receipt::Received if c.busy => {
                self.say_update(
                    Voice::Reply,
                    &format!(
                        "{} got your request; it's queued behind its current work.",
                        c.name
                    ),
                );
            }
            Receipt::Started if c.busy => {
                // Received not said yet: started says it all.
                let rec = format!(
                    "{} got your request; it's queued behind its current work.",
                    c.name
                );
                self.assistant.quiet.queue.retain(|u| u.text != rec);
                self.say_update(Voice::Reply, &format!("{} started on it.", c.name));
            }
            Receipt::Dropped(r) | Receipt::Interrupted(r) => {
                let what = if matches!(c.to, Receipt::Dropped(_)) {
                    "was dropped"
                } else {
                    "was interrupted before it finished"
                };
                self.assistant_note(format!(
                    "Request #{} to {tab} ({}) {what} ({r}): \"{words}\". resend sends it again.",
                    c.id, c.name
                ));
                // Waiting for the answer to a request that never runs
                // would only end in "no answer found".
                for f in &mut self.assistant.follow_ups {
                    if f.uid == c.uid
                        && f.state == crate::app_followup::FuState::Waiting
                        && crate::app_followup::key_of(&f.question)
                            == crate::app_followup::key_of(&c.text)
                    {
                        f.state = crate::app_followup::FuState::Cancelled;
                        f.ended = Some(Instant::now());
                    }
                }
                self.say_urgent(
                    Voice::Reply,
                    &format!(
                        "Your request to {} {what}: '{words}'. Say resend to send it again.",
                        c.name
                    ),
                );
            }
            _ => {}
        }
    }

    /// Before an interrupt (Escape, Ctrl-C) in a tab: what it may cost,
    /// and the tab's requests are checked afterwards.
    pub fn interrupt_warning(&mut self, uid: u64) -> Option<String> {
        let mut items = vec![];
        let now = Instant::now();
        let mine: Vec<u64> = self
            .deliveries
            .iter()
            .filter(|d| {
                !d.rc.receipt.is_final() && !d.rc.mode.external() && self.current_uid(d.uid) == uid
            })
            .map(|d| d.id)
            .collect();
        for d in self.deliveries.iter_mut().filter(|d| mine.contains(&d.id)) {
            d.rc.esc_at = Some(now);
            let what = match (&d.rc.receipt, &d.stage) {
                (Receipt::Started, _) => "started: this interrupts it",
                (Receipt::Received, _) => "received, in claude's queue: Escape can drop it",
                (Receipt::Pending, Stage::Queued) => {
                    "queued in GodTerm: it goes in once the tab is ready"
                }
                _ => "typed in, not confirmed yet: Escape can drop it",
            };
            items.push(format!("#{} {what} (\"{}\")", d.id, first_words(&d.text)));
        }
        if items.is_empty() {
            return None;
        }
        crate::log::info(&format!(
            "receipt: interrupt in {} with {} open requests",
            tab_id(uid),
            items.len()
        ));
        Some(format!(
            "This tab has {} not finished: {}. GodTerm checks them after the interrupt and reports any that were dropped or interrupted; tell the user.",
            crate::control::plural(items.len(), "request"),
            items.join("; ")
        ))
    }

    /// Requests for get_state, newest first.
    pub fn deliveries_json(&self) -> Value {
        let now = chrono::Local::now();
        json!(self
            .deliveries
            .iter()
            .rev()
            .take(15)
            .map(|d| {
                let changed =
                    now - chrono::Duration::from_std(d.rc.receipt_at.elapsed()).unwrap_or_default();
                let mut v = json!({
                    "id": d.id,
                    "tab": tab_id(self.current_uid(d.uid)),
                    "name": self.locate_uid(d.uid).map(|(s, t)| self.panes[s].tabs[t].name()),
                    "request": crate::sessions::snippet(&d.text, 100),
                    "state": d.state_word(),
                    "sent_at": d.rc.created_at.format("%H:%M:%S").to_string(),
                    "state_since": changed.format("%H:%M:%S").to_string(),
                    "from_user": d.rc.notify,
                    "mode": d.rc.mode.word(),
                });
                if let Some(n) = &d.rc.note {
                    v["mode_note"] = json!(n);
                }
                if let Some(r) = self.run_json(d.id).and_then(|r| r.as_object().cloned()) {
                    for (k, x) in r {
                        v[k] = x;
                    }
                }
                if let Some(r) = d.rc.receipt.reason() {
                    v["reason"] = json!(r);
                } else if let (Stage::Failed(r), Receipt::Pending) = (&d.stage, &d.rc.receipt) {
                    v["reason"] = json!(r);
                }
                if let Some(n) = d.rc.resent_as {
                    v["resent_as"] = json!(n);
                } else if d.rc.receipt.lost() {
                    v["resend"] = json!(true);
                }
                if d.rc.restored {
                    v["restored_after_restart"] = json!(true);
                }
                v
            })
            .collect::<Vec<_>>())
    }

    /// The requests line for the state block: open ones, and lost ones not
    /// yet resent.
    pub fn deliveries_state_lines(&self) -> String {
        let mut s = String::new();
        for d in &self.deliveries {
            let open = !d.done() || (d.rc.notify && !d.rc.receipt.is_final());
            let lost = d.rc.notify
                && d.rc.resendable()
                && d.rc.receipt_at.elapsed() < Duration::from_secs(1800);
            if lost {
                s.push_str(&format!(
                    "LOST request #{} to {} ({}: {}): \"{}\" (resend sends it again)\n",
                    d.id,
                    tab_id(self.current_uid(d.uid)),
                    d.state_word(),
                    d.rc.receipt.reason().unwrap_or(""),
                    first_words(&d.text)
                ));
            } else if open {
                s.push_str(&format!(
                    "request #{} to {}: {} [{}{}] ({} s) \"{}\"\n",
                    d.id,
                    tab_id(self.current_uid(d.uid)),
                    d.state_word(),
                    d.rc.mode.word(),
                    self.busy
                        .runs
                        .iter()
                        .find(|r| r.delivery == d.id)
                        .and_then(|r| r.short())
                        .map(|s| format!(" {s}"))
                        .unwrap_or_default(),
                    d.created.elapsed().as_secs(),
                    first_words(&d.text)
                ));
            }
        }
        s
    }

    /// Send a lost request again, with its exact text, to its tab.
    pub fn resend_one(&mut self, id: u64) -> Result<(u64, String), String> {
        let (uid, text) = self
            .deliveries
            .iter()
            .find(|d| d.id == id)
            .map(|d| (self.current_uid(d.uid), d.text.clone()))
            .ok_or(format!("there is no request #{id} any more"))?;
        if self.find_tab(uid).is_none() {
            return Err(format!(
                "{} is closed; send the request to another tab with send_prompt",
                tab_id(uid)
            ));
        }
        let name = self.spoken_tab_name(uid);
        let new = self.deliver_request(uid, &text);
        if let Some(d) = self.deliveries.iter_mut().find(|d| d.id == id) {
            d.rc.resent_as = Some(new);
        }
        crate::log::info(&format!(
            "receipt: #{id} resent to {} as #{new}",
            tab_id(uid)
        ));
        Ok((new, name))
    }

    /// The newest lost request not sent again (of one tab).
    fn latest_lost(&self, uid: Option<u64>) -> Option<u64> {
        self.deliveries
            .iter()
            .rev()
            .filter(|d| d.rc.notify && d.rc.resendable())
            .find(|d| uid.is_none_or(|u| self.current_uid(d.uid) == u))
            .map(|d| d.id)
    }

    /// The resend tool.
    pub fn resend_tool(&mut self, args: &Value) -> Result<Value, String> {
        let uid = match args.get("tab") {
            Some(v) if !v.is_null() => {
                let (s, t) = self.one_tab(args)?;
                Some(self.panes[s].tabs[t].uid)
            }
            _ => None,
        };
        let id = match args.get("id").and_then(Value::as_u64) {
            Some(i) => {
                let d = self.deliveries.iter().find(|d| d.id == i).ok_or(format!(
                    "there is no request #{i} (deliveries in get_state)"
                ))?;
                if let Some(n) = d.rc.resent_as {
                    return Err(format!("request #{i} was already resent as #{n}"));
                }
                if !d.rc.receipt.lost() && d.rc.receipt != Receipt::Unconfirmed {
                    return Err(format!(
                        "request #{i} is {}; it needs no resend",
                        d.state_word()
                    ));
                }
                i
            }
            None => self
                .latest_lost(uid)
                .ok_or("no dropped or interrupted request to resend")?,
        };
        let (new, name) = self.resend_one(id)?;
        Ok(json!({
            "_wait": {"deliveries": [new], "wait_queued": false, "max_ms": 6000},
            "resent": {"from": id, "id": new},
            "report_say": format!("Resent request #{id} to {name} (now #{new})."),
        }))
    }

    /// "resend" said with a lost request about: sent again right away.
    pub fn resend_said(&mut self, text: &str) -> bool {
        let c: String = text
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect();
        let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
        let phrases = [
            "resend",
            "resend it",
            "resend that",
            "resend please",
            "please resend",
            "yes resend",
            "re send",
            "re send it",
            "resend the request",
            "resend my request",
            "send it again",
            "send that again",
            "send it again please",
        ];
        if self.assistant.busy || !phrases.contains(&c.as_str()) {
            return false;
        }
        let Some(id) = self.latest_lost(None) else {
            return false;
        };
        let words = self
            .deliveries
            .iter()
            .find(|d| d.id == id)
            .map(|d| first_words(&d.text))
            .unwrap_or_default();
        let say = match self.resend_one(id) {
            Ok((_, name)) => format!("Resent to {name}: '{words}'."),
            Err(e) => format!("I couldn't resend it: {e}."),
        };
        self.assistant.log.push(Entry {
            who: Who::User,
            text: text.to_string(),
        });
        self.assistant.log.push(Entry {
            who: Who::Reply,
            text: say.clone(),
        });
        self.assistant.unsaid = None;
        self.assistant.muted = false;
        self.assistant.spoken.push(say.clone());
        self.told_record("resend", &say, crate::told::Channel::Panel, false);
        if self.will_speak_assistant() {
            self.told_record("resend", &say, crate::told::Channel::Spoken, false);
        }
        self.speak_assistant(&say);
        true
    }

    /// Pending requests for the saved state.
    pub fn saved_deliveries(&self) -> Vec<crate::state::SavedDelivery> {
        self.deliveries
            .iter()
            .filter(|d| d.pending_for_save())
            .map(|d| crate::state::SavedDelivery {
                id: d.id,
                tab: self.current_uid(d.uid),
                text: d.text.clone(),
                state: d.state_word().to_string(),
                sent_ms: d.rc.sent_ms,
                notify: d.rc.notify,
                created: d.rc.created_at.timestamp(),
            })
            .collect()
    }

    /// After a restart: requests that were still to run are queued again
    /// (unless the transcript shows they started meanwhile); a tab that
    /// is gone makes them dropped, said out loud.
    pub fn restore_deliveries(&mut self, saved: &[crate::state::SavedDelivery]) {
        let mut requeued: Vec<String> = vec![];
        for sd in saved {
            self.delivery_seq = self.delivery_seq.max(sd.id);
            let mut rc = Track::new(false);
            rc.notify = sd.notify;
            rc.restored = true;
            if let Some(t) = chrono::DateTime::from_timestamp(sd.created, 0) {
                rc.created_at = t.with_timezone(&chrono::Local);
            }
            let mut d = Delivery {
                id: sd.id,
                uid: sd.tab,
                text: sd.text.clone(),
                stage: Stage::Queued,
                created: Instant::now(),
                before: None,
                reported: true,
                rc,
            };
            let loc = self.find_tab(sd.tab);
            let started = match (loc, sd.sent_ms) {
                (Some((s, t)), Some(ms)) => self.transcript_path(s, t).map(|p| {
                    let seen = crate::receipts::scan(&p, &sd.text, ms, None).seen;
                    (seen, ms)
                }),
                _ => None,
            };
            match (loc, started) {
                (None, _) => {
                    d.stage = Stage::Failed("lost on a restart".into());
                    d.rc.receipt = Receipt::Dropped("GodTerm restarted and its tab is gone".into());
                    crate::log::info(&format!(
                        "receipt: #{} {} dropped: GodTerm restarted and the tab is gone",
                        sd.id,
                        tab_id(sd.tab)
                    ));
                    let words = first_words(&sd.text);
                    if sd.notify {
                        self.assistant_note(format!(
                            "Request #{} to {} was dropped (GodTerm restarted and the tab is gone): \"{words}\". resend sends it to another tab only with send_prompt.",
                            sd.id,
                            tab_id(sd.tab)
                        ));
                        self.say_urgent(
                            Voice::Reply,
                            &format!(
                                "Your request to {} was dropped: '{words}'. GodTerm restarted and that tab is gone, so send it again to another tab.",
                                tab_id(sd.tab)
                            ),
                        );
                    }
                }
                (Some(_), Some((Seen::Started { .. } | Seen::Done, ms))) => {
                    // It ran before the restart: follow it, do not resend.
                    d.stage = Stage::Delivered;
                    d.rc.sent_ms = Some(ms);
                    d.rc.receipt = Receipt::Started;
                    crate::log::info(&format!(
                        "receipt: #{} {} started before the restart; following it",
                        sd.id,
                        tab_id(sd.tab)
                    ));
                }
                (Some(_), _) => {
                    crate::log::info(&format!(
                        "receipt: #{} {} was {} at the restart; queued again",
                        sd.id,
                        tab_id(sd.tab),
                        sd.state
                    ));
                    if sd.notify {
                        requeued.push(format!(
                            "{} ('{}')",
                            self.spoken_tab_name(sd.tab),
                            first_words(&sd.text)
                        ));
                    }
                }
            }
            self.deliveries.push(d);
        }
        if !requeued.is_empty() {
            let msg = format!(
                "After the restart, {} queued again: {}.",
                if requeued.len() == 1 {
                    "your request is".to_string()
                } else {
                    format!("{} requests are", requeued.len())
                },
                requeued.join(", ")
            );
            self.assistant_note(msg.clone());
            self.say_update(Voice::Reply, &msg);
        }
    }

    /// Tabs with a request queued again after a restart are started (a
    /// lazy restore would otherwise leave them waiting to be shown).
    pub fn start_tabs_with_requests(&mut self) {
        let uids: Vec<u64> = self
            .deliveries
            .iter()
            .filter(|d| d.rc.restored && d.stage == Stage::Queued)
            .map(|d| d.uid)
            .collect();
        for uid in uids {
            let Some((p, t)) = self.find_tab(uid) else {
                continue;
            };
            let tab = &self.panes[p].tabs[t];
            if tab.is_running()
                || tab.pending.is_none()
                || self.restore_queue.iter().any(|(_, u)| *u == uid)
            {
                continue;
            }
            if self.restore_queue.is_empty() {
                self.restore_next = Instant::now() + Duration::from_millis(300);
            }
            self.restore_queue.push_back((p, uid));
        }
    }
}
