use std::cell::Cell;
use std::collections::HashSet;

use chrono::{DateTime, Local, TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Color;

use crate::app::{App, Modal};
use crate::app_jobs::{
    SchedKind, SchedRow, Tone, OP_ADOPT, OP_DELETE, OP_OPEN, OP_PAUSE, OP_RUN, OP_STOP,
};
use crate::control::tab_id;
use crate::loops::{describe_cron, Kind, Loop};
use crate::theme;

#[derive(Debug, Default)]
pub struct SchedUi {
    pub scroll: usize,

    pub sel: usize,

    pub focus: bool,

    pub folded: HashSet<String>,

    pub view: Cell<(usize, usize)>,

    pub rect: Cell<ratatui::layout::Rect>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SchedLine {
    Group {
        key: String,
        label: String,
        count: String,
        folded: bool,
    },

    Row {
        idx: usize,
        indent: bool,
    },
}

fn group_count(rows: &[&SchedRow]) -> String {
    let jobs = rows.iter().filter(|r| r.kind == SchedKind::Managed).count();
    let loops = rows.len() - jobs;
    let mut v = vec![];
    if jobs > 0 {
        v.push(crate::control::plural(jobs, "job"));
    }
    if loops > 0 {
        v.push(crate::control::plural(loops, "loop"));
    }
    v.join(" · ")
}

pub fn sched_lines(rows: &[SchedRow], folded: &HashSet<String>) -> Vec<SchedLine> {
    let mut order: Vec<&str> = vec![];
    for r in rows {
        if !order.contains(&r.group.as_str()) {
            order.push(&r.group);
        }
    }
    let mut out = vec![];
    for g in order {
        let mut idxs: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].group == g).collect();
        idxs.sort_by_key(|&i| rows[i].kind != SchedKind::Managed);
        if idxs.len() == 1 {
            out.push(SchedLine::Row {
                idx: idxs[0],
                indent: false,
            });
            continue;
        }
        let f = folded.contains(g);
        let members: Vec<&SchedRow> = idxs.iter().map(|&i| &rows[i]).collect();
        out.push(SchedLine::Group {
            key: g.to_string(),
            label: rows[idxs[0]].group_label.clone(),
            count: group_count(&members),
            folded: f,
        });
        if !f {
            out.extend(
                idxs.into_iter()
                    .map(|idx| SchedLine::Row { idx, indent: true }),
            );
        }
    }
    out
}

pub fn plan(open: bool, lines: usize, panel_h: u16, avail: u16) -> (usize, u16) {
    if avail == 0 {
        return (0, 0);
    }
    if !open || lines == 0 || avail == 1 {
        return (0, 1);
    }
    let cap = ((panel_h as usize) * 2 / 5).max(3);
    let body = lines.min(cap).min(avail as usize - 1);
    (body, 1 + body as u16)
}

pub fn clamp_scroll(scroll: usize, total: usize, shown: usize) -> usize {
    scroll.min(total.saturating_sub(shown))
}

pub fn scroll_words(scroll: usize, total: usize, shown: usize) -> Option<String> {
    (shown > 0 && total > shown).then(|| {
        let s = clamp_scroll(scroll, total, shown);
        format!("{} to {} of {total}", s + 1, s + shown)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtnTone {
    Primary,
    Neutral,
    Danger,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardBtn {
    pub label: String,
    pub op: u8,
    pub key: char,
    pub hint: String,
    pub tone: BtnTone,
}

pub const OP_CLOSE: u8 = 9;

#[derive(Debug, Clone, PartialEq)]
pub struct SchedCard {
    pub key: String,
    pub kind: SchedKind,
    pub name: String,

    pub badges: Vec<(String, Color)>,

    pub prompt: String,

    pub facts: Vec<(&'static str, String, Color)>,
    pub buttons: Vec<CardBtn>,

    pub note: Option<String>,
}

impl SchedCard {
    #[cfg(test)]
    pub fn text(&self) -> String {
        let mut v = vec![self.name.clone()];
        v.extend(self.badges.iter().map(|b| b.0.clone()));
        v.push(self.prompt.clone());
        v.extend(self.facts.iter().map(|(l, t, _)| format!("{l}: {t}")));
        v.extend(self.buttons.iter().map(|b| b.label.clone()));
        v.extend(self.note.clone());
        v.join("\n")
    }
}

pub fn kind_color(k: SchedKind) -> Color {
    match k {
        SchedKind::Managed => theme::SAGE,
        SchedKind::Session => theme::SAND,
        SchedKind::Durable => theme::SLATE,
        SchedKind::Grok => theme::MAUVE,
    }
}

fn abs_local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local)
        .format("%a %b %-d %H:%M")
        .to_string()
}

fn when(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    format!("{} ({})", abs_local(t), crate::loops::rel(t, now))
}

fn ts(t: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(t, 0).single()
}

pub fn cron_words(c: &str, recurring: bool) -> String {
    let d = describe_cron(c);
    let head = if recurring { "Schedule" } else { "One time" };
    if d == c {
        format!("{head}: cron {c}")
    } else if recurring {
        format!("{head}: cron {c}, i.e. {d}")
    } else {
        format!("{head}: cron {c}, i.e. the next time it matches ({d})")
    }
}

pub fn loop_kind_words(l: &Loop) -> String {
    if let Some(e) = &l.every {
        return if l.recurring {
            format!("Recurring {e}")
        } else {
            format!("One time: {e}")
        };
    }
    match (l.kind, &l.cron) {
        (Kind::Wakeup, _) => {
            "Loop: wakes itself dynamically (the session picks each next time)".into()
        }
        (_, Some(c)) => cron_words(c, l.recurring),
        _ => "Unknown schedule".into(),
    }
}

pub fn job_kind_words(j: &crate::jobs::Job) -> String {
    if let Some(n) = j.every_min {
        return format!("Recurring {}", crate::jobs::every_words(n));
    }
    if let Some(c) = &j.cron {
        return cron_words(c, true);
    }
    if let Some(t) = j.at.and_then(ts) {
        return format!("One time at {}", abs_local(t));
    }
    "No schedule".into()
}

fn btn(label: &str, op: u8, key: char, hint: &str, tone: BtnTone) -> CardBtn {
    CardBtn {
        label: label.into(),
        op,
        key,
        hint: hint.into(),
        tone,
    }
}

pub const MANAGE_NOTE: &str = "Manage with GodTerm: GodTerm takes it over and stops the copy inside the session, so it keeps running even if the tab closes.";

impl App {
    pub fn sched_view(&self) -> (Vec<SchedRow>, Vec<SchedLine>) {
        let rows = self.sched_rows();
        let lines = sched_lines(&rows, &self.sched_ui.folded);
        (rows, lines)
    }

    pub fn sched_scroll_by(&mut self, d: isize) {
        let (shown, total) = self.sched_ui.view.get();
        let s = (self.sched_ui.scroll as isize + d).max(0) as usize;
        self.sched_ui.scroll = clamp_scroll(s, total, shown);
    }

    pub fn sched_toggle_group(&mut self, key: &str) {
        if !self.sched_ui.folded.remove(key) {
            self.sched_ui.folded.insert(key.to_string());
        }
        self.sched_ui.focus = true;
    }

    pub fn sched_key(&mut self, k: &KeyEvent) -> bool {
        if !self.sched_ui.focus || !self.sched_open {
            self.sched_ui.focus = false;
            return false;
        }
        let (_, lines) = self.sched_view();
        let n = lines.len();
        let (shown, _) = self.sched_ui.view.get();
        let shown = shown.max(1);
        let sel = self.sched_ui.sel.min(n.saturating_sub(1));
        let new_sel = match k.code {
            KeyCode::Up => sel.saturating_sub(1),
            KeyCode::Down => (sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => sel.saturating_sub(shown),
            KeyCode::PageDown => (sel + shown).min(n.saturating_sub(1)),
            KeyCode::Home => 0,
            KeyCode::End => n.saturating_sub(1),
            KeyCode::Enter => {
                match lines.get(sel) {
                    Some(SchedLine::Group { key, .. }) => {
                        let key = key.clone();
                        self.sched_toggle_group(&key);
                    }
                    Some(SchedLine::Row { idx, .. }) => {
                        let rows = self.sched_rows();
                        if let Some(r) = rows.get(*idx) {
                            self.open_sched_card(&r.key);
                        }
                    }
                    None => {}
                }
                return true;
            }
            KeyCode::Esc => {
                self.sched_ui.focus = false;
                return true;
            }
            _ => {
                self.sched_ui.focus = false;
                return false;
            }
        };
        self.sched_ui.sel = new_sel;

        if new_sel < self.sched_ui.scroll {
            self.sched_ui.scroll = new_sel;
        } else if new_sel >= self.sched_ui.scroll + shown {
            self.sched_ui.scroll = new_sel + 1 - shown;
        }
        true
    }

    fn loop_of(&self, key: &str) -> Option<crate::app_loops::LoopRow> {
        let (uid, id) = key.split_once(':')?;
        let uid: u64 = uid.parse().ok()?;
        self.loops
            .iter()
            .find(|r| r.uid == uid && r.lp.id == id)
            .cloned()
    }

    pub fn sched_adopt(&mut self, key: &str) -> Option<String> {
        let (uid, id) = key.split_once(':')?;
        let (uid, id) = (uid.parse::<u64>().unwrap_or(0), id.to_string());
        match self.adopt_loop_now(uid, &id, None, true) {
            Ok(j) => {
                self.flash(format!(
                    "Now managed by GodTerm: {}. The session's own copy was stopped.",
                    j.name
                ));
                Some(j.id)
            }
            Err(e) => {
                self.flash(format!("Could not manage it with GodTerm: {e}"));
                None
            }
        }
    }

    pub fn sched_run_loop(&mut self, key: &str) {
        let Some(r) = self.loop_of(key) else {
            self.flash("That loop is not running any more");
            return;
        };
        if r.lp.prompt.trim().is_empty() {
            self.flash("That loop has no prompt to send");
            return;
        }
        if self.find_tab(r.uid).is_none() {
            self.flash("Its tab is closed");
            return;
        }
        self.deliver(r.uid, &r.lp.prompt);
        self.flash(format!(
            "Sent it to {} now (its own schedule is unchanged)",
            tab_id(r.uid)
        ));
    }

    pub fn sched_stop_loop(&mut self, key: &str) -> bool {
        let Some(r) = self.loop_of(key) else {
            self.flash("That loop is not running any more");
            return false;
        };
        let ck = format!("stop:{key}");
        if !self.sched_delete_armed(&ck) {
            self.sched_confirm = Some((ck, std::time::Instant::now()));
            self.flash("Stop it? Click stop again: the tab is asked to cancel it when it is idle");
            return false;
        }
        self.sched_confirm = None;
        self.stop_loops(r.uid, vec![r.lp.id.clone()]);
        self.flash(format!(
            "Asked {} to stop loop {} (it goes once the tab is idle)",
            tab_id(r.uid),
            r.lp.id
        ));
        true
    }

    pub fn open_sched_card(&mut self, key: &str) {
        self.sched_ui.focus = true;
        let (rows, lines) = self.sched_view();
        if let Some(i) = lines
            .iter()
            .position(|l| matches!(l, SchedLine::Row { idx, .. } if rows[*idx].key == key))
        {
            self.sched_ui.sel = i;
        }
        self.modal = Modal::SchedCard(key.to_string());
    }

    pub fn sched_card(&self, key: &str) -> Option<SchedCard> {
        let rows = self.sched_rows();
        let r = rows.iter().find(|r| r.key == key)?;
        if r.kind == SchedKind::Managed {
            self.job_card(r)
        } else {
            self.session_card(r)
        }
    }

    fn where_words(r: &SchedRow) -> String {
        let mut v = vec![if r.tab_id.is_empty() {
            r.tab.clone()
        } else {
            format!("{} {}", r.tab_id, r.tab)
        }];
        if !r.folder.is_empty() {
            v.push(format!("folder {}", r.folder));
        }
        if !r.account.is_empty() {
            v.push(format!("account {}", r.account));
        }
        v.join(" · ")
    }

    fn session_card(&self, r: &SchedRow) -> Option<SchedCard> {
        let row = self.loop_of(&r.key)?;
        let l = &row.lp;
        let now = Utc::now();
        let mut badges = vec![
            (r.kind.badge().to_string(), kind_color(r.kind)),
            (r.shape().to_string(), theme::STONE),
        ];
        if let Some(e) = l.expires() {
            badges.push((
                format!("expires {}", crate::loops::rel(e, now)),
                theme::SAND,
            ));
        }
        let place = Self::where_words(r);
        let tab_words = {
            let mut extra = vec![];
            if !r.folder.is_empty() {
                extra.push(format!("folder {}", r.folder));
            }
            if !r.account.is_empty() {
                extra.push(format!("account {}", r.account));
            }
            let t = format!("{} {}", r.tab_id, r.tab);
            if extra.is_empty() {
                t
            } else {
                format!("{t} ({})", extra.join(", "))
            }
        };
        let owner = match r.kind {
            SchedKind::Durable => format!(
                "Kept by Claude Code in {}/.claude/scheduled_tasks.json: it fires only while a Claude session runs in that folder (now tab {tab_words})",
                if r.folder.is_empty() { "the project" } else { &r.folder }
            ),
            SchedKind::Grok => format!(
                "Runs inside the Grok session in tab {tab_words}. It stops when that session ends."
            ),
            _ => format!(
                "Runs inside the session in tab {tab_words}. It stops when that session ends."
            ),
        };
        let next = match l.next_fire(now) {
            Some(t) => when(t, now),
            None if l.kind == Kind::Wakeup => "When the session wakes itself next".into(),
            None => "Not known".into(),
        };
        let last = match l.last_fire {
            Some(t) => format!(
                "Ran {} ({}), {} so far",
                abs_local(t),
                crate::loops::rel(t, now),
                crate::control::plural(l.fires, "run")
            ),
            None => "Not run yet".into(),
        };
        let ends = match (l.kind, l.durable, l.recurring, l.expires()) {
            (Kind::Wakeup, ..) => "When the session ends or the loop stops itself".to_string(),
            (_, true, ..) => "No end date (while the file keeps it)".to_string(),
            (_, _, false, _) => "After its one run".to_string(),
            (_, _, true, Some(e)) => format!(
                "Expires {}: session crons expire by themselves 7 days after they are made",
                when(e, now)
            ),
            _ => "Session crons expire by themselves 7 days after they are made".to_string(),
        };
        let mut id_words = format!("{} (the session's own id for it)", l.id);
        let twins: Vec<&str> = self
            .loops
            .iter()
            .filter(|o| {
                o.uid == row.uid
                    && o.lp.id != l.id
                    && o.lp.cron == l.cron
                    && o.lp.every == l.every
                    && o.lp.prompt.trim() == l.prompt.trim()
            })
            .map(|o| o.lp.id.as_str())
            .collect();
        if !twins.is_empty() {
            id_words += &format!(
                ". This tab has another loop with the same schedule and prompt ({}): two separate loops, both fire",
                twins.join(", ")
            );
        }
        let facts = vec![
            ("Kind", loop_kind_words(l), theme::FG),
            ("Owner", owner, kind_color(r.kind)),
            ("Tab", place, theme::FG),
            ("Next run", next, theme::SLATE),
            ("Last run", last, theme::DIM),
            ("Ends", ends, theme::SAND),
            ("Id", id_words, theme::DIM),
        ];
        let mut buttons = vec![];
        let dynamic = l.kind == Kind::Wakeup && l.every.is_none();
        if !dynamic {
            buttons.push(btn(
                "Manage with GodTerm",
                OP_ADOPT,
                'm',
                "GodTerm takes it over as its own job and the session's copy is stopped (m)",
                BtnTone::Primary,
            ));
        }
        buttons.push(btn(
            "Run now",
            OP_RUN,
            'r',
            "Send its prompt to the tab now; its schedule is unchanged (r)",
            BtnTone::Neutral,
        ));
        let armed = self.sched_delete_armed(&format!("stop:{}", r.key));
        buttons.push(btn(
            if armed { "Click again to stop" } else { "Stop" },
            OP_STOP,
            's',
            "Cancel this loop in its session (asks twice) (s)",
            BtnTone::Danger,
        ));
        buttons.push(btn(
            "Open tab",
            OP_OPEN,
            'o',
            "Go to the tab it runs in (o)",
            BtnTone::Neutral,
        ));
        buttons.push(btn(
            "Close",
            OP_CLOSE,
            'q',
            "Close this card (Esc)",
            BtnTone::Neutral,
        ));
        let note = Some(if dynamic {
            "A dynamic loop has no fixed schedule, so GodTerm cannot take it over as is: ask the assistant to manage it with a cron or an interval.".to_string()
        } else {
            MANAGE_NOTE.to_string()
        });
        Some(SchedCard {
            key: r.key.clone(),
            kind: r.kind,
            name: format!(
                "{} {} in {} {}",
                if r.dynamic {
                    "Loop"
                } else {
                    "Session schedule"
                },
                l.id,
                r.tab_id,
                r.tab
            ),
            badges,
            prompt: l.prompt.clone(),
            facts,
            buttons,
            note,
        })
    }

    fn job_card(&self, r: &SchedRow) -> Option<SchedCard> {
        let j = self.jobs.find(&r.key)?;
        let now = Utc::now();
        let failed = j.last_result.as_ref().is_some_and(|x| x.failed());
        let mut badges = vec![
            (r.kind.badge().to_string(), kind_color(r.kind)),
            (r.shape().to_string(), theme::STONE),
        ];
        if !j.enabled {
            badges.push(("paused".into(), theme::STONE));
        }
        if let Some(f) = &j.finished {
            badges.push((f.clone(), theme::STONE));
        }
        if failed {
            badges.push(("last run failed".into(), theme::MUTED_RED));
        }
        let mut owner = "Managed by GodTerm: kept in GodTerm's jobs, it survives restarts and wakes its tab if the tab is closed or archived".to_string();
        if let Some(a) = &j.adopted_from {
            owner += &format!(" (taken over from the session loop {a})");
        }
        let state = self.job_target_state(&j.target);
        let place = if state == "open" {
            Self::where_words(r)
        } else {
            format!("{} (now {state}: a run wakes it)", Self::where_words(r))
        };
        let next = match (&j.finished, j.enabled, j.next_run.and_then(ts)) {
            (Some(f), _, _) => format!("None: it is {f}"),
            (None, false, _) => "None while paused".into(),
            (None, true, Some(t)) => when(t, now),
            (None, true, None) => "Not scheduled".into(),
        };
        let (last, last_color) = match &j.last_result {
            Some(x) => {
                let at = ts(x.at).map(|t| when(t, now)).unwrap_or_default();
                let mut s = format!("{} {at}", x.state);
                if !x.detail.is_empty() {
                    s += &format!(": {}", x.detail);
                }
                s += &format!(
                    ", {} so far",
                    crate::control::plural(j.runs as usize, "run")
                );
                (
                    s,
                    match r.tone {
                        Tone::Good => theme::SAGE,
                        Tone::Bad => theme::MUTED_RED,
                        Tone::Warn => theme::SAND,
                        Tone::Plain => theme::DIM,
                    },
                )
            }
            None => ("Not run yet".into(), theme::DIM),
        };
        let ends = match j.end_at.and_then(ts) {
            Some(t) => format!("Ends {}", when(t, now)),
            None => "No end date: it runs until you pause or delete it".into(),
        };
        let facts = vec![
            ("Kind", job_kind_words(j), theme::FG),
            ("Owner", owner, theme::SAGE),
            ("Tab", place, theme::FG),
            ("Next run", next, theme::SLATE),
            ("Last run", last, last_color),
            ("Ends", ends, theme::SAND),
            (
                "Id",
                format!("job {}, made by {}", j.id, j.created_by),
                theme::DIM,
            ),
        ];
        let armed = self.sched_delete_armed(&r.key);
        let buttons = vec![
            btn(
                "Run now",
                OP_RUN,
                'r',
                "Run it now, outside its schedule (wakes its tab if needed) (r)",
                BtnTone::Primary,
            ),
            if j.enabled {
                btn(
                    "Pause",
                    OP_PAUSE,
                    'p',
                    "Pause it; resume any time (p)",
                    BtnTone::Neutral,
                )
            } else {
                btn(
                    "Resume",
                    OP_PAUSE,
                    'p',
                    "Resume it at its next regular time (p)",
                    BtnTone::Neutral,
                )
            },
            btn(
                if armed {
                    "Click again to delete"
                } else {
                    "Delete"
                },
                OP_DELETE,
                'd',
                "Delete this job for good (asks twice) (d)",
                BtnTone::Danger,
            ),
            btn(
                "Open tab",
                OP_OPEN,
                'o',
                "Go to its tab (o)",
                BtnTone::Neutral,
            ),
            btn(
                "Close",
                OP_CLOSE,
                'q',
                "Close this card (Esc)",
                BtnTone::Neutral,
            ),
        ];
        Some(SchedCard {
            key: r.key.clone(),
            kind: r.kind,
            name: j.name.clone(),
            badges,
            prompt: j.prompt.clone(),
            facts,
            buttons,
            note: None,
        })
    }

    pub fn sched_card_act(&mut self, op: u8) {
        let Modal::SchedCard(key) = self.modal.clone() else {
            return;
        };
        let managed = self.jobs.find(&key).is_some();
        match op {
            OP_CLOSE => self.modal = Modal::None,
            OP_OPEN => {
                self.modal = Modal::None;
                self.sched_open(&key);
            }
            OP_ADOPT if !managed => {
                if let Some(id) = self.sched_adopt(&key) {
                    self.modal = Modal::SchedCard(id);
                }
            }
            OP_STOP if !managed => {
                let stopped = self.sched_stop_loop(&key);
                if stopped {
                    self.modal = Modal::None;
                }
            }
            OP_RUN => self.sched_action(&key, OP_RUN),
            OP_PAUSE | OP_DELETE if managed => {
                self.sched_action(&key, op);
                if self.jobs.find(&key).is_none() {
                    self.modal = Modal::None;
                }
            }
            _ => {}
        }
    }

    pub fn on_sched_card_key(&mut self, k: KeyEvent) {
        let Modal::SchedCard(key) = self.modal.clone() else {
            return;
        };
        let Some(card) = self.sched_card(&key) else {
            self.modal = Modal::None;
            return;
        };
        let op = match k.code {
            KeyCode::Esc => Some(OP_CLOSE),
            KeyCode::Char(c) => card.buttons.iter().find(|b| b.key == c).map(|b| b.op),
            _ => None,
        };
        if let Some(op) = op {
            self.sched_card_act(op);
        }
    }
}
