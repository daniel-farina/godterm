//! The loops drawer: a lower section inside a pane (between its terminal
//! and the model bar) listing the active tab's scheduled prompts, opened
//! with the `⟳ loops (N)` button on the model bar's row or Ctrl-a ;.
//!
//! GodTerm has no scheduler of its own: every loop is the tab's own
//! (loops.rs reads them): Claude Code's CronCreate jobs and dynamic
//! /loop wakeups from its transcript plus durable ones in
//! `.claude/scheduled_tasks.json`; Grok Build's scheduler_create tasks
//! from its chat history. Nothing here edits those files: "Run now"
//! sends the loop's prompt to the tab as a turn, "Cancel" asks the tab to
//! cancel it (typed in once it is idle), "Copy" copies its details, and
//! "New loop…" types `/loop ` into the tab's input for the user to finish.

use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};

use crate::app::App;
use crate::app_loops::LoopRow;
use crate::harness::Harness;

/// Rows a new drawer gets (its top edge included).
pub const DEFAULT_ROWS: u16 = 8;
pub const MIN_ROWS: u16 = 4;
pub const MAX_ROWS: u16 = 16;
/// The terminal keeps at least this many rows.
pub const MIN_TERM: u16 = 6;
/// A second click on Cancel within this long asks the tab.
const CONFIRM: Duration = Duration::from_secs(6);

/// The per loop buttons.
pub const OP_RUN: u8 = 0;
pub const OP_CANCEL: u8 = 1;
pub const OP_COPY: u8 = 2;

/// "in 3:12" under an hour (it counts down each second), else "in 2h";
/// past ones "4m ago".
pub fn countdown(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let s = (t - now).num_seconds();
    if (0..3600).contains(&s) {
        format!("in {}:{:02}", s / 60, s % 60)
    } else {
        crate::loops::rel(t, now)
    }
}

/// The wall clock time, with the day when it is not today.
pub fn clock(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let l = t.with_timezone(&Local);
    if l.date_naive() == now.with_timezone(&Local).date_naive() {
        l.format("%H:%M").to_string()
    } else {
        l.format("%a %H:%M").to_string()
    }
}

/// One loop's line parts: (when, last, status).
pub fn loop_facts(r: &LoopRow, now: DateTime<Utc>) -> (String, String, String) {
    let l = &r.lp;
    let next = match l.next_fire(now) {
        Some(t) => format!("next {} ({})", countdown(t, now), clock(t, now)),
        None if l.every.is_some() => "next: not recorded by Grok".into(),
        None => "next: ?".into(),
    };
    let last = match (l.last_fire, l.fires) {
        (Some(t), n) => format!(
            "last {}, {n} run{}",
            crate::loops::rel(t, now),
            if n == 1 { "" } else { "s" }
        ),
        (None, 0) => "not run yet".into(),
        (None, n) => format!("{n} runs"),
    };
    let status = match l.expires() {
        Some(e) if e > now => format!("armed, ends {}", crate::loops::rel(e, now)),
        _ if l.kind == crate::loops::Kind::Wakeup => "armed (wakes itself)".into(),
        _ if l.durable => "armed (durable)".into(),
        _ => "armed".into(),
    };
    (next, last, status)
}

impl App {
    /// Whether pane `i`'s drawer is open.
    pub fn loops_drawer_open(&self, i: usize) -> bool {
        self.loops_drawer.contains_key(&i)
    }

    /// The loops of pane `i`'s active tab.
    pub fn drawer_loops(&self, i: usize) -> Vec<LoopRow> {
        let Some(slot) = self.panes.get(i) else {
            return vec![];
        };
        let uid = slot.cur().uid;
        self.loops
            .iter()
            .filter(|r| r.uid == uid)
            .cloned()
            .collect()
    }

    /// Open or close pane `i`'s drawer (the button, Ctrl-a ;).
    pub fn toggle_loops_drawer(&mut self, i: usize) {
        if i >= self.panes.len() {
            return;
        }
        if self.loops_drawer.remove(&i).is_some() {
            if self.loops_drawer_focus == Some(i) {
                self.loops_drawer_focus = None;
            }
            return;
        }
        self.loops_drawer.insert(i, DEFAULT_ROWS);
        self.loops_drawer_focus = Some(i);
        self.loops_drawer_sel = 0;
        self.focus = i;
        self.view = crate::app::View::Grid;
        self.refresh_loops(true);
    }

    /// Its top edge dragged to `row`.
    pub fn drag_loops_drawer(&mut self, i: usize, row: u16) {
        let Some(r) = self.loops_drawer_geo.borrow().get(&i).copied() else {
            return;
        };
        let bottom = r.y + r.height;
        let h = bottom.saturating_sub(row).clamp(MIN_ROWS, MAX_ROWS);
        self.loops_drawer.insert(i, h);
    }

    /// Keys while a drawer has them: Esc closes it, Up and Down select;
    /// anything else gives the keys back to the tab (false: not taken).
    pub fn loops_drawer_key(&mut self, k: &KeyEvent) -> bool {
        let Some(i) = self.loops_drawer_focus else {
            return false;
        };
        if !self.loops_drawer_open(i) || i != self.focus {
            self.loops_drawer_focus = None;
            return false;
        }
        let n = self.drawer_loops(i).len();
        match k.code {
            KeyCode::Esc => {
                self.toggle_loops_drawer(i);
                true
            }
            KeyCode::Up => {
                self.loops_drawer_sel = self.loops_drawer_sel.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                self.loops_drawer_sel = (self.loops_drawer_sel + 1).min(n.saturating_sub(1));
                true
            }
            _ => {
                self.loops_drawer_focus = None;
                false
            }
        }
    }

    /// The details copied for a loop.
    fn loop_details(&self, r: &LoopRow, source: &str) -> String {
        let now = Utc::now();
        let (next, last, status) = loop_facts(r, now);
        format!(
            "{source} loop {} on tab {} ({}): {}\n{next}; {last}; {status}\nprompt: {}",
            r.lp.id,
            crate::control::tab_id(r.uid),
            r.tab_name,
            r.lp.cadence(),
            r.lp.prompt
        )
    }

    /// A drawer button: loop `k` of pane `i`, op OP_RUN / OP_CANCEL / OP_COPY.
    pub fn loop_action(&mut self, i: usize, k: usize, op: u8) {
        self.focus = i;
        self.loops_drawer_focus = Some(i);
        self.loops_drawer_sel = k;
        let Some(r) = self.drawer_loops(i).get(k).cloned() else {
            return;
        };
        let source = match self.pane_harness(i) {
            Some(Harness::Grok) => "Grok",
            _ => "Claude",
        };
        let name = r.tab_name.clone();
        match op {
            OP_RUN => {
                if r.lp.prompt.trim().is_empty() {
                    self.flash("That loop has no prompt to run");
                    return;
                }
                self.deliver_request(r.uid, &r.lp.prompt);
                self.flash(format!(
                    "Running loop {} now in {name} (typed in once it is idle)",
                    r.lp.id
                ));
            }
            OP_CANCEL => {
                let again = self.loops_confirm.as_ref().is_some_and(|(u, id, at)| {
                    *u == r.uid && *id == r.lp.id && at.elapsed() < CONFIRM
                });
                if again {
                    self.loops_confirm = None;
                    self.stop_loops(r.uid, vec![r.lp.id.clone()]);
                    self.flash(format!(
                        "Asking {name} to cancel loop {} once it is idle",
                        r.lp.id
                    ));
                } else {
                    self.loops_confirm = Some((r.uid, r.lp.id.clone(), Instant::now()));
                    self.flash(format!(
                        "Cancel loop {}? Click Cancel again to ask {name} to cancel it",
                        r.lp.id
                    ));
                }
            }
            _ => {
                let text = self.loop_details(&r, source);
                self.copy_text(text);
            }
        }
    }

    /// Whether Cancel on this loop waits for its second click.
    pub fn loop_cancel_armed(&self, uid: u64, id: &str) -> bool {
        self.loops_confirm
            .as_ref()
            .is_some_and(|(u, i, at)| *u == uid && i == id && at.elapsed() < CONFIRM)
    }

    /// Whether a stop was asked of this loop and is not confirmed yet.
    pub fn loop_cancel_asked(&self, uid: u64, id: &str) -> bool {
        self.stop_reqs.iter().any(|q| {
            q.uid == uid && (q.ids.iter().any(|x| x == id) || (q.wakeup && id == "wakeup"))
        })
    }

    /// "New loop…": `/loop ` typed into the tab's input (not sent), and
    /// the keys go to the tab to finish it.
    pub fn new_loop_in(&mut self, i: usize) {
        let t = self.panes[i].active;
        let tab = &mut self.panes[i].tabs[t];
        if !tab.is_running() {
            self.flash("Start the tab first: its loops run in its session");
            return;
        }
        if tab.activity == crate::pane::Activity::Permission {
            self.flash("The tab is waiting for an answer: answer it first");
            return;
        }
        tab.reset_scroll();
        let bracketed = tab.bracketed_paste();
        tab.write(&crate::keys::encode_paste("/loop ", bracketed));
        self.focus = i;
        self.assistant.focused = false;
        self.loops_drawer_focus = None;
        self.flash("Typed /loop into the tab: add the interval and the prompt, then Enter (e.g. 30m check the deploy)");
    }

    /// The show_tab_loops tool: open the drawer on that tab and list them.
    pub fn show_tab_loops_tool(&mut self, args: &Value) -> Result<Value, String> {
        let (s, t) = self.one_tab(args)?;
        self.jump_to(s, t);
        self.view = crate::app::View::Grid;
        let open = args["open"].as_bool().unwrap_or(true);
        if open != self.loops_drawer_open(s) {
            self.toggle_loops_drawer(s);
        }
        let now = Utc::now();
        let rows: Vec<Value> = self
            .drawer_loops(s)
            .iter()
            .map(|r| {
                let (next, last, status) = loop_facts(r, now);
                json!({"id": r.lp.id, "every": r.lp.cadence(), "prompt": crate::sessions::snippet(&r.lp.prompt, 120), "next": next, "last": last, "status": status})
            })
            .collect();
        let name = self.panes[s].tabs[t].name();
        let say = match (open, rows.len()) {
            (false, _) => format!("Closed the loops on {name}."),
            (true, 0) => format!("{name} has no loops; the drawer shows that."),
            (true, n) => format!("Showing {} on {name}.", crate::control::plural(n, "loop")),
        };
        Ok(
            json!({"ok": true, "result": {"tab": crate::control::tab_id(self.panes[s].tabs[t].uid), "open": open, "loops": rows, "say": say}}),
        )
    }
}
