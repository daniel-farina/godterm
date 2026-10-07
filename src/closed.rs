//! Closing tabs and windows, with a way back: the last 20 closed tabs are
//! kept (in state.json), a 10 second "Closed 'x' · Undo" toast reopens the
//! last close (a whole window at once), and Tabs ▾ has Reopen closed tab
//! and Recently closed ▸. Reopening resumes the session in its folder on
//! its account (or the best one of the same agent) and its loops.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::{App, Modal};
use crate::pane::{Activity, LaunchKind};

/// How many closed tabs are kept.
pub const KEEP: usize = 20;
/// How long the Undo toast stays.
pub const TOAST: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedLoop {
    pub id: String,
    #[serde(default)]
    pub cron: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub recurring: bool,
    /// A dynamic /loop (ScheduleWakeup) rather than a cron job.
    #[serde(default)]
    pub wakeup: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClosedTab {
    /// Account name (config `name`).
    pub account: String,
    pub cwd: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub loops: Vec<SavedLoop>,
    /// Tabs closed together (a window) share a group and reopen together.
    pub group: u64,
    /// Unix seconds.
    pub closed_at: i64,
    /// What the tab was called.
    pub label: String,
    /// Its tab group's name, and whether it was pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_group: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// When to ask before closing one tab (config `confirm_close`).
pub fn needs_confirm(mode: &str, running: bool, busy: bool, loops: usize) -> bool {
    match mode {
        "never" => false,
        "always" => running,
        _ => running && (busy || loops > 0),
    }
}

impl App {
    /// Working or waiting for an answer.
    pub fn tab_busy(&self, s: usize, t: usize) -> bool {
        self.panes
            .get(s)
            .and_then(|p| p.tabs.get(t))
            .is_some_and(|x| {
                x.is_running() && matches!(x.activity, Activity::Working | Activity::Permission)
            })
    }

    /// Close a tab, asking first per `confirm_close` (busy or looping
    /// tabs by default).
    pub fn request_close(&mut self, s: usize, t: usize) {
        self.jump_to(s, t);
        let tab = &self.panes[s].tabs[t];
        // Pinned tabs always ask (unless confirm_close is never).
        let ask = needs_confirm(
            &self.cfg.confirm_close,
            tab.is_running(),
            self.tab_busy(s, t),
            self.loops_in(tab.uid),
        ) || (tab.pinned && self.cfg.confirm_close != "never");
        if ask {
            self.modal = Modal::ConfirmClose;
        } else {
            self.close_tab_now();
        }
    }

    fn snapshot_tab(&self, s: usize, t: usize, group: u64) -> Option<ClosedTab> {
        let slot = self.panes.get(s)?;
        let a = slot.account?;
        let tab = slot.tabs.get(t)?;
        // A tab that never ran has nothing to come back to.
        if !tab.is_running() && tab.session_id.is_none() {
            return None;
        }
        let loops = self
            .loops
            .iter()
            .filter(|r| r.uid == tab.uid && !r.lp.durable)
            .map(|r| SavedLoop {
                id: r.lp.id.clone(),
                cron: r.lp.cron.clone(),
                prompt: r.lp.prompt.clone(),
                recurring: r.lp.recurring,
                wakeup: r.lp.kind == crate::loops::Kind::Wakeup,
            })
            .collect();
        Some(ClosedTab {
            account: self.cfg.accounts[a].name.clone(),
            cwd: self.tab_dir(s, t).to_string_lossy().into_owned(),
            session_id: tab.session_id.clone(),
            name: tab.custom_name.clone(),
            accent: tab.accent.clone(),
            loops,
            group,
            closed_at: chrono::Utc::now().timestamp(),
            label: tab.name(),
            tab_group: tab.group.clone(),
            pinned: tab.pinned,
        })
    }

    pub fn next_close_group(&mut self) -> u64 {
        self.closed_seq += 1;
        chrono::Utc::now().timestamp_millis() as u64 * 1000 + (self.closed_seq % 1000)
    }

    /// Remember tab (s, t) as closed in `group`.
    pub fn remember_closed(&mut self, s: usize, t: usize, group: u64) -> bool {
        let Some(c) = self.snapshot_tab(s, t, group) else {
            return false;
        };
        self.closed.push(c);
        let n = self.closed.len();
        if n > KEEP {
            self.closed.drain(..n - KEEP);
        }
        self.state_dirty = true;
        true
    }

    fn toast(&mut self, msg: String) {
        self.undo_toast = Some((Instant::now(), msg));
    }

    /// Close tab `t` of pane `s` now, remembering it.
    pub fn close_tab_at(&mut self, s: usize, t: usize) {
        self.close_tab_by(s, t, "user")
    }

    /// Close a tab, recording who did it (user or assistant).
    pub fn close_tab_by(&mut self, s: usize, t: usize, by: &str) {
        let g = self.next_close_group();
        let label = self.panes[s]
            .tabs
            .get(t)
            .map(|x| x.name())
            .unwrap_or_default();
        self.tab_event(s, t, "close", Some(by), None);
        let kept = self.remember_closed(s, t, g);
        let fallback = self.panes[s]
            .account
            .map(|a| self.cfg.accounts[a].work_dir())
            .unwrap_or_else(crate::config::home_dir);
        self.panes[s].close_tab(t, fallback);
        self.state_dirty = true;
        if kept {
            self.toast(format!("Closed '{label}'"));
        } else {
            self.flash(format!("Closed tab {} of pane {}", t + 1, s + 1));
        }
    }

    /// Ask before closing a whole window (pane): it names the tabs.
    pub fn request_close_window(&mut self, s: usize) {
        if s < self.panes.len() {
            self.modal = Modal::ConfirmCloseWindow(s);
        }
    }

    /// Lines for the close window dialog.
    pub fn close_window_lines(&self, s: usize) -> Vec<String> {
        let Some(slot) = self.panes.get(s) else {
            return vec![];
        };
        let name = slot
            .account
            .map(|a| self.cfg.accounts[a].display().to_string())
            .unwrap_or_else(|| "this pane".into());
        let running: Vec<String> = slot
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_running())
            .map(|(i, t)| {
                let busy = self.tab_busy(s, i);
                let nl = self.loops_in(t.uid);
                let mut what = vec![];
                if busy {
                    what.push("working".to_string());
                }
                if nl > 0 {
                    what.push(format!("{nl} loop{}", if nl == 1 { "" } else { "s" }));
                }
                if what.is_empty() {
                    t.name()
                } else {
                    format!("{} ({})", t.name(), what.join(", "))
                }
            })
            .collect();
        let n = slot.tabs.len();
        let mut v = vec![format!(
            "Close the {name} window and its {n} tab{}?",
            if n == 1 { "" } else { "s" }
        )];
        if running.is_empty() {
            v.push("Nothing is running in it.".into());
        } else {
            v.push(format!("Running: {}", running.join(", ")));
        }
        v.push("The pane is hidden; Undo or Tabs ▾ > Reopen closed tab brings it back.".into());
        v
    }

    /// Close every tab of pane `s` (remembered as one group) and hide it.
    pub fn close_window(&mut self, s: usize) {
        if s >= self.panes.len() {
            return;
        }
        let g = self.next_close_group();
        let n = self.panes[s].tabs.len();
        let mut kept = 0;
        for t in 0..n {
            self.tab_event(
                s,
                t,
                "close",
                Some("user"),
                Some("closed the window".into()),
            );
            if self.remember_closed(s, t, g) {
                kept += 1;
            }
        }
        let fallback = self.panes[s]
            .account
            .map(|a| self.cfg.accounts[a].work_dir())
            .unwrap_or_else(crate::config::home_dir);
        self.panes[s].kill_all();
        self.panes[s].tabs = vec![crate::pane::Pane::new(fallback)];
        self.panes[s].active = 0;
        // Hidden unless it is the last one on screen.
        if self.visible_panes().len() > 1 {
            self.panes[s].hidden = true;
            if self.focus == s {
                self.focus = self.visible_panes().first().copied().unwrap_or(0);
            }
        }
        self.state_dirty = true;
        let name = self.panes[s]
            .account
            .map(|a| self.cfg.accounts[a].display().to_string())
            .unwrap_or_else(|| format!("pane {}", s + 1));
        if kept > 0 {
            self.toast(format!(
                "Closed the {name} window ({n} tab{})",
                if n == 1 { "" } else { "s" }
            ));
        } else {
            self.flash(format!("Closed the {name} window"));
        }
    }

    /// Reopen the last close (a whole window at once).
    pub fn reopen_closed(&mut self) -> Result<String, String> {
        let g = self
            .closed
            .last()
            .map(|c| c.group)
            .ok_or("nothing was closed recently")?;
        let items: Vec<ClosedTab> = self
            .closed
            .iter()
            .filter(|c| c.group == g)
            .cloned()
            .collect();
        self.closed.retain(|c| c.group != g);
        self.undo_toast = None;
        self.state_dirty = true;
        let mut done = vec![];
        for c in items {
            done.push(self.reopen_one(c)?);
        }
        let msg = if done.len() == 1 {
            done.remove(0)
        } else {
            format!("Reopened {} tabs", done.len())
        };
        self.flash(msg.clone());
        Ok(msg)
    }

    /// Reopen entry `i` of the stack (Recently closed ▸; newest is 0).
    pub fn reopen_closed_at(&mut self, i: usize) -> Result<String, String> {
        let n = self.closed.len();
        let k = n.checked_sub(1 + i).ok_or("no such closed tab")?;
        let c = self.closed.remove(k);
        self.state_dirty = true;
        let msg = self.reopen_one(c)?;
        self.flash(msg.clone());
        Ok(msg)
    }

    /// Reopen one closed tab (also used for tab history records).
    pub fn reopen_closed_tab(&mut self, c: ClosedTab) -> Result<String, String> {
        self.reopen_one(c)
    }

    fn reopen_one(&mut self, c: ClosedTab) -> Result<String, String> {
        let orig = self.cfg.accounts.iter().position(|a| a.name == c.account);
        let harness = orig.map(|a| self.cfg.accounts[a].harness());
        // Its own account when it can run, else the best of the same agent.
        let usable = |app: &App, a: usize| app.accounts[a].login.logged_in();
        let a = match orig {
            Some(a) if usable(self, a) => a,
            _ => {
                let mut best: Option<(usize, f64)> = None;
                for a in 0..self.cfg.accounts.len() {
                    if harness.is_some_and(|h| self.cfg.accounts[a].harness() != h)
                        || !usable(self, a)
                    {
                        continue;
                    }
                    let l = self.accounts[a].effective_left().unwrap_or(0.0);
                    if best.is_none_or(|(_, b)| l > b) {
                        best = Some((a, l));
                    }
                }
                best.map(|(a, _)| a)
                    .or(orig)
                    .ok_or(format!("no account to reopen '{}' on", c.label))?
            }
        };
        // The session moves with it when it reopens on another account.
        let mut session = c.session_id.clone();
        if let (Some(o), Some(sid)) = (orig, &c.session_id) {
            if o != a {
                let src = self.cfg.accounts[o].config_dir();
                let path = src
                    .join("projects")
                    .join(crate::state::encode_project_dir(std::path::Path::new(
                        &c.cwd,
                    )))
                    .join(format!("{sid}.jsonl"));
                let copied = path.is_file()
                    && self.cfg.accounts[a].harness() == crate::harness::Harness::Claude
                    && crate::session_ops::copy_session(
                        &src,
                        &path,
                        sid,
                        &self.cfg.accounts[a].config_dir(),
                        crate::session_ops::Conflict::Skip,
                    )
                    .is_ok();
                if !copied {
                    session = None;
                }
            }
        }
        // Its account's pane (shown again if hidden), or a new one.
        let p = match self
            .panes
            .iter()
            .position(|s| s.account == Some(a) && !s.hidden)
            .or_else(|| self.panes.iter().position(|s| s.account == Some(a)))
        {
            Some(p) => p,
            None => {
                self.panes.push(crate::slot::Slot::new(
                    Some(a),
                    self.cfg.accounts[a].work_dir(),
                ));
                self.panes.len() - 1
            }
        };
        self.panes[p].hidden = false;
        let cwd = PathBuf::from(&c.cwd);
        let cwd = if cwd.is_dir() {
            cwd
        } else {
            self.cfg.accounts[a].work_dir()
        };
        // Reuse a fresh placeholder tab, else open a new one.
        let cur = self.panes[p].cur();
        let t = if !cur.is_running()
            && cur.session_id.is_none()
            && self.panes[p].tabs.len() == 1
            && matches!(cur.state, crate::pane::PaneState::Idle)
        {
            self.panes[p].tabs[0].cwd = cwd;
            0
        } else {
            self.panes[p].add_tab(cwd)
        };
        self.panes[p].tabs[t].custom_name = c.name.clone();
        self.panes[p].tabs[t].accent = c.accent.clone();
        self.panes[p].tabs[t].pinned = c.pinned;
        if let Some(g) = &c.tab_group {
            self.panes[p].group_tabs(&[t], g, None);
        }
        let kind = match &session {
            Some(sid) => LaunchKind::Resume(sid.clone()),
            None => LaunchKind::Normal,
        };
        let resumed = session.is_some();
        self.panes[p].active = t;
        self.launch_tab(p, t, kind);
        self.panes[p].active = t;
        self.focus = p;
        self.view = crate::app::View::Grid;
        self.state_dirty = true;
        let uid = self.panes[p].tabs[t].uid;
        if resumed && !c.loops.is_empty() {
            let loops: Vec<crate::loops::Loop> = c.loops.iter().map(to_loop).collect();
            self.deliver(uid, &crate::takeover::recreate_prompt(&loops));
        }
        let on = self.cfg.accounts[a].display().to_string();
        let mut msg = format!("Reopened '{}' on {on}", c.label);
        if !resumed && c.session_id.is_some() {
            msg.push_str(" (a new session: the old one stays on its account)");
        }
        if resumed && !c.loops.is_empty() {
            msg.push_str(&format!(
                ", {} loop{} checked",
                c.loops.len(),
                if c.loops.len() == 1 { "" } else { "s" }
            ));
        }
        Ok(msg)
    }

    /// The Undo toast, while it lasts.
    pub fn toast_text(&self) -> Option<&str> {
        self.undo_toast
            .as_ref()
            .filter(|(t, _)| crate::clock::age(*t) < TOAST)
            .map(|(_, m)| m.as_str())
    }
}

fn to_loop(s: &SavedLoop) -> crate::loops::Loop {
    crate::loops::Loop {
        id: s.id.clone(),
        kind: if s.wakeup {
            crate::loops::Kind::Wakeup
        } else {
            crate::loops::Kind::Cron
        },
        cron: s.cron.clone(),
        prompt: s.prompt.clone(),
        recurring: s.recurring,
        durable: false,
        created: None,
        last_fire: None,
        fires: 0,
        wake_at: None,
        deleted: false,
        confidence: crate::loops::Confidence::High,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_rules() {
        assert!(!needs_confirm("busy", true, false, 0));
        assert!(needs_confirm("busy", true, true, 0));
        assert!(needs_confirm("busy", true, false, 2));
        assert!(
            !needs_confirm("busy", false, true, 2),
            "nothing running, nothing to lose"
        );
        assert!(needs_confirm("always", true, false, 0));
        assert!(!needs_confirm("never", true, true, 3));
    }
}
