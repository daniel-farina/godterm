//! The Loops view: scheduled prompts in running tabs, found by scanning
//! their transcripts in the background, and stopping them.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{App, AppEvent, Modal, View};
use crate::loops::{Kind, Loop, LoopCache};
use crate::pane::Activity;

/// A loop and the tab it runs in.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopRow {
    pub uid: u64,
    pub account: Option<usize>,
    pub tab_name: String,
    pub lp: Loop,
}

/// A stop request typed into a tab once it is idle.
#[derive(Debug, Clone, PartialEq)]
pub struct StopReq {
    pub uid: u64,
    pub ids: Vec<String>,
    pub wakeup: bool,
    pub sent: Option<Instant>,
}

/// The prompt that asks claude to stop these loops.
pub fn stop_prompt(ids: &[String], wakeup: bool) -> String {
    let mut parts = vec![];
    if !ids.is_empty() {
        parts.push(format!(
            "Cancel the scheduled job{} {} with CronDelete.",
            if ids.len() == 1 { "" } else { "s" },
            ids.join(", ")
        ));
    }
    if wakeup {
        parts.push("Stop the dynamic loop: call ScheduleWakeup with stop: true.".into());
    }
    parts.push("Do nothing else and reply only with what you stopped.".into());
    parts.join(" ")
}

impl App {
    /// Rescan running tabs' transcripts in the background (every few
    /// seconds from tick, at once when the view opens).
    pub fn refresh_loops(&mut self, now: bool) {
        if self.loops_scanning || (!now && self.loops_scanned.elapsed() < Duration::from_secs(5)) {
            return;
        }
        let mut jobs = vec![];
        for (s, slot) in self.panes.iter().enumerate() {
            for (t, tab) in slot.tabs.iter().enumerate() {
                // Loops are a Claude Code feature; grok tabs have none.
                let grok = slot.account.is_some_and(|a| {
                    self.cfg.accounts[a].harness() == crate::harness::Harness::Grok
                });
                if !tab.is_running() || grok {
                    continue;
                }
                let path = self.transcript_path(s, t);
                jobs.push((
                    tab.uid,
                    slot.account,
                    tab.name(),
                    path,
                    tab.cwd.clone(),
                    tab.started,
                ));
            }
        }
        self.loops_scanning = true;
        self.loops_scanned = Instant::now();
        let cache: Arc<Mutex<LoopCache>> = Arc::clone(&self.loop_cache);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut rows = vec![];
            let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
            for (uid, account, tab_name, path, cwd, _started) in jobs {
                let mut seen = vec![];
                if let Some(p) = path {
                    let sc = c.scanners.entry(p.clone()).or_default();
                    sc.update(&p);
                    for l in sc.loops() {
                        seen.push(l.id.clone());
                        rows.push(LoopRow {
                            uid,
                            account,
                            tab_name: tab_name.clone(),
                            lp: l.clone(),
                        });
                    }
                }
                for l in crate::loops::durable(&cwd) {
                    if !seen.contains(&l.id) {
                        rows.push(LoopRow {
                            uid,
                            account,
                            tab_name: tab_name.clone(),
                            lp: l,
                        });
                    }
                }
            }
            let _ = tx.send(AppEvent::Loops(rows));
        });
    }

    pub fn on_loops(&mut self, rows: Vec<LoopRow>) {
        self.loops_scanning = false;
        // Confirm stops that went through.
        let gone: Vec<(u64, String)> = rows
            .iter()
            .filter(|r| r.lp.deleted)
            .map(|r| (r.uid, r.lp.id.clone()))
            .collect();
        let mut done = vec![];
        self.stop_reqs.retain(|q| {
            let all = q
                .ids
                .iter()
                .all(|id| gone.iter().any(|(u, g)| *u == q.uid && g == id))
                && (!q.wakeup || gone.iter().any(|(u, g)| *u == q.uid && g == "wakeup"));
            if q.sent.is_some() && all {
                done.push(q.ids.clone());
                false
            } else {
                true
            }
        });
        for ids in done {
            let what = if ids.is_empty() {
                "the loop".to_string()
            } else {
                ids.join(", ")
            };
            self.flash(format!("Stopped {what} (confirmed in the transcript)"));
        }
        let now = Utc::now();
        self.loops = rows.into_iter().filter(|r| r.lp.active(now)).collect();
        self.sel_loop = self.sel_loop.min(self.loops.len().saturating_sub(1));
    }

    pub fn transcript_path(&self, slot: usize, tab: usize) -> Option<std::path::PathBuf> {
        self.transcript_of(slot, tab).map(|(p, _)| p)
    }

    /// Active loops in a tab.
    pub fn loops_in(&self, uid: u64) -> usize {
        self.loops.iter().filter(|r| r.uid == uid).count()
    }

    pub fn open_loops(&mut self) {
        self.view = if self.view == View::Loops {
            View::Grid
        } else {
            View::Loops
        };
        if self.view == View::Loops {
            self.refresh_loops(true);
        }
    }

    /// Stop these loops of one tab: typed in when it is idle.
    pub fn stop_loops(&mut self, uid: u64, ids: Vec<String>) {
        let wakeup = ids.iter().any(|i| i == "wakeup");
        let ids: Vec<String> = ids.into_iter().filter(|i| i != "wakeup").collect();
        if ids.is_empty() && !wakeup {
            return;
        }
        self.stop_reqs.push(StopReq {
            uid,
            ids,
            wakeup,
            sent: None,
        });
        self.stop_tick();
    }

    pub fn stop_selected_loop(&mut self) {
        if let Some(r) = self.loops.get(self.sel_loop).cloned() {
            self.stop_loops(r.uid, vec![r.lp.id.clone()]);
        }
    }

    pub fn stop_all_loops(&mut self) {
        let mut by_tab: Vec<(u64, Vec<String>)> = vec![];
        for r in &self.loops {
            match by_tab.iter_mut().find(|(u, _)| *u == r.uid) {
                Some((_, v)) => v.push(r.lp.id.clone()),
                None => by_tab.push((r.uid, vec![r.lp.id.clone()])),
            }
        }
        let n = self.loops.len();
        for (uid, ids) in by_tab {
            self.stop_loops(uid, ids);
        }
        self.flash(format!(
            "Stopping {n} loop{}: each tab is asked when it is idle",
            if n == 1 { "" } else { "s" }
        ));
    }

    /// Send queued stop requests to tabs that are ready.
    pub fn stop_tick(&mut self) {
        let mut sends = vec![];
        for q in self.stop_reqs.iter_mut() {
            if q.sent
                .is_some_and(|t| t.elapsed() > Duration::from_secs(120))
            {
                q.sent = None; // no confirmation: try again
            }
            if q.sent.is_some() {
                continue;
            }
            let found = self
                .panes
                .iter()
                .enumerate()
                .find_map(|(s, p)| p.tabs.iter().position(|t| t.uid == q.uid).map(|t| (s, t)));
            let Some((s, t)) = found else { continue };
            let tab = &self.panes[s].tabs[t];
            if tab.is_running() && tab.activity == Activity::Ready {
                q.sent = Some(Instant::now());
                sends.push((s, t, stop_prompt(&q.ids, q.wakeup)));
            }
        }
        self.stop_reqs.retain(|q| {
            self.panes
                .iter()
                .any(|p| p.tabs.iter().any(|t| t.uid == q.uid))
        });
        if !sends.is_empty() {
            // Look for the confirmation soon.
            self.loops_scanned = Instant::now() - Duration::from_secs(3);
        }
        for (s, t, text) in sends {
            crate::log::info(&format!(
                "loops: asking pane {} tab {} to stop: {text}",
                s + 1,
                t + 1
            ));
            self.send_text(s, t, &text);
        }
    }

    pub fn jump_to_loop(&mut self) {
        if let Some(r) = self.loops.get(self.sel_loop).cloned() {
            if let Some((s, t)) = self.find_tab(r.uid) {
                self.jump_to(s, t);
                self.view = View::Grid;
            }
        }
    }

    pub fn on_loops_key(&mut self, k: KeyEvent) {
        let n = self.loops.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('@') => self.view = View::Grid,
            KeyCode::Up | KeyCode::Char('k') => self.sel_loop = self.sel_loop.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel_loop = (self.sel_loop + 1).min(n.saturating_sub(1))
            }
            KeyCode::Enter | KeyCode::Char('g') => self.jump_to_loop(),
            KeyCode::Char('s') | KeyCode::Delete => self.stop_selected_loop(),
            KeyCode::Char('S') if n > 0 => self.modal = Modal::ConfirmStopLoops,
            KeyCode::Char('r') => self.refresh_loops(true),
            _ => {}
        }
    }
}

/// Short label for a loop's kind.
pub fn kind_label(l: &Loop) -> &'static str {
    match (l.kind, l.durable) {
        (Kind::Wakeup, _) => "loop",
        (_, true) => "durable",
        _ => "cron",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_prompts() {
        assert_eq!(
            stop_prompt(&["e0e3839a".into()], false),
            "Cancel the scheduled job e0e3839a with CronDelete. Do nothing else and reply only with what you stopped."
        );
        let p = stop_prompt(&["a".into(), "b".into()], true);
        assert!(p.contains("jobs a, b") && p.contains("ScheduleWakeup with stop: true"));
    }
}
