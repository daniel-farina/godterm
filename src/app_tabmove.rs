//! Move (or copy) a live tab to another account: the conversation goes on
//! there with `claude --resume <id>`, for example when one account runs
//! low. The session is copied with the session engine; the target tab
//! must come up before the source closes.

use std::time::{Duration, Instant};

use crate::app::{App, Modal, View};
use crate::pane::Activity;
use crate::pane::LaunchKind;
use crate::session_ops::{self, Conflict};

/// One account in the "Move tab to..." picker.
#[derive(Debug, Clone, PartialEq)]
pub struct MoveTarget {
    pub account: usize,
    /// None when it can take the tab; else why not.
    pub disabled: Option<String>,
    pub five_left: Option<f64>,
    pub week_left: Option<f64>,
    pub tabs: usize,
}

impl MoveTarget {
    /// The lower of the two windows: what the account can really do.
    pub fn effective(&self) -> Option<f64> {
        match (self.five_left, self.week_left) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Out of usage (under 2% in either window).
    pub fn exhausted(&self) -> bool {
        self.effective().is_some_and(|e| e < 2.0)
    }
}

/// The picker dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct MovePicker {
    pub slot: usize,
    pub tab: usize,
    pub sel: usize,
    /// Copy keeps the source tab open.
    pub copy: bool,
    pub same_folder: bool,
    /// The tab's name, editable in the Tab section (Esc goes back to
    /// name_orig).
    pub name: String,
    pub name_edit: bool,
    pub name_orig: String,
    /// Focus: None on the Move section's targets, Some(i) on option row i
    /// (app_tabopts::panel_rows).
    pub opt: Option<usize>,
    /// The session's own title, editable.
    pub title: String,
    pub title_orig: String,
    pub title_edit: bool,
    /// The Color row's swatch (0: none) and the Group row's choice.
    pub color_sel: usize,
    pub group_sel: usize,
    /// Copy only the conversation there (no tab opens).
    pub session_only: bool,
}

/// A move waiting for its tab to go idle.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedMove {
    pub uid: u64,
    pub target: usize,
    pub copy: bool,
    pub same_folder: bool,
}

/// A started move: the source closes once the target is up.
#[derive(Debug, Clone)]
pub struct PendingMove {
    pub source_uid: u64,
    pub target_uid: u64,
    pub copy: bool,
    pub since: Instant,
    pub name: String,
    pub target: usize,
}

/// For Undo (about 10 s after a move).
#[derive(Debug, Clone)]
pub struct DoneMove {
    pub target_uid: u64,
    pub source_account: usize,
    pub at: Instant,
}

pub const UNDO_FOR: Duration = Duration::from_secs(10);

/// Sort targets: usable first, by 5 hour % left then weekly % left, both
/// descending; disabled ones at the bottom.
pub fn sort_targets(v: &mut [MoveTarget]) {
    v.sort_by(|a, b| {
        // Usable first; then by what is really left (the binding window);
        // an exhausted account sorts with the unusable ones.
        let key = |t: &MoveTarget| {
            (
                u8::from(t.exhausted()) + 2 * u8::from(t.disabled.is_some()),
                -(t.effective().unwrap_or(-1.0)),
                -(t.five_left.unwrap_or(-1.0)),
            )
        };
        let (x, y) = (key(a), key(b));
        x.0.cmp(&y.0)
            .then(x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(x.2.partial_cmp(&y.2).unwrap_or(std::cmp::Ordering::Equal))
    });
}

impl App {
    pub fn week_left(&self, a: usize) -> Option<f64> {
        self.accounts
            .get(a)?
            .usage
            .as_ref()?
            .get("seven_day")
            .map(|w| w.left_now())
    }

    /// Every other account, sorted for the picker.
    pub fn move_targets(&self, slot: usize) -> Vec<MoveTarget> {
        let from = self.panes.get(slot).and_then(|s| s.account);
        let mut v: Vec<MoveTarget> = (0..self.cfg.accounts.len())
            .filter(|a| Some(*a) != from)
            .map(|a| {
                let st = &self.accounts[a];
                let same_harness = from.is_none_or(|f| {
                    self.cfg.accounts[f].harness() == self.cfg.accounts[a].harness()
                });
                let disabled = if let Some(why) =
                    from.and_then(|f| self.cfg.accounts[f].harness().no_move_reason())
                {
                    Some(why)
                } else if !same_harness {
                    Some(format!(
                        "a {} account (conversations move within one harness)",
                        self.cfg.accounts[a].harness().name()
                    ))
                } else if !st.login.logged_in() {
                    Some("not logged in".to_string())
                } else if st
                    .login
                    .expires_at
                    .is_some_and(|e| e < chrono::Utc::now().timestamp_millis())
                    && !st.login.has_refresh
                {
                    Some("token expired".to_string())
                } else {
                    None
                };
                MoveTarget {
                    account: a,
                    disabled,
                    five_left: st.five_hour_left(),
                    week_left: self.week_left(a),
                    tabs: self
                        .panes
                        .iter()
                        .filter(|s| s.account == Some(a))
                        .map(|s| s.tabs.len())
                        .sum(),
                }
            })
            .collect();
        sort_targets(&mut v);
        v
    }

    /// Double click on a tab, Ctrl-a m, the tab menu, voice.
    pub fn open_move_picker(&mut self, slot: usize, tab: usize, copy: bool) {
        if self.panes.get(slot).and_then(|s| s.tabs.get(tab)).is_none() {
            return;
        }
        if let Some(why) = self
            .panes
            .get(slot)
            .and_then(|s| s.account)
            .and_then(|a| self.cfg.accounts[a].harness().no_move_reason())
        {
            self.flash(why);
            return;
        }
        if self.move_targets(slot).iter().all(|t| t.disabled.is_some()) {
            self.flash("No other logged in account to move this tab to");
            return;
        }
        self.open_tab_panel(slot, tab);
        if let Modal::MoveTab(p) = &mut self.modal {
            p.copy = copy;
        }
    }

    /// Confirm the picker's selection.
    /// Save the name typed in the move dialog (if it changed).
    pub fn apply_picker_name(&mut self, p: &MovePicker) {
        let cur = self
            .panes
            .get(p.slot)
            .and_then(|s| s.tabs.get(p.tab))
            .map(|t| t.name());
        if cur.is_some_and(|c| c != p.name.trim()) {
            self.rename_tab(p.slot, p.tab, &p.name);
        }
    }

    /// Cancel the move dialog; a name edit is still kept.
    pub fn cancel_move_picker(&mut self) {
        if let Modal::MoveTab(p) = self.modal.clone() {
            self.apply_picker_name(&p);
        }
        self.modal = Modal::None;
    }

    pub fn confirm_move_picker(&mut self) {
        let Modal::MoveTab(p) = self.modal.clone() else {
            return;
        };
        self.apply_picker_name(&p);
        let targets = self.move_targets(p.slot);
        let Some(t) = targets.get(p.sel) else { return };
        if let Some(why) = &t.disabled {
            self.flash(format!("{}: {why}", self.cfg.accounts[t.account].display()));
            return;
        }
        self.modal = Modal::None;
        if p.session_only {
            return self.copy_session_to(p.slot, p.tab, t.account);
        }
        self.request_move(p.slot, p.tab, t.account, p.copy, p.same_folder);
    }

    /// Copy a tab's conversation to `target` so it can be resumed there;
    /// no tab opens.
    pub fn copy_session_to(&mut self, slot: usize, tab: usize, target: usize) {
        self.detect_sessions();
        match self.copy_transcript(slot, tab, target) {
            Ok(Some(id)) => self.flash(format!(
                "Copied the session to {}; resume it there from Sessions ({})",
                self.cfg.accounts[target].display(),
                &id[..id.len().min(8)]
            )),
            Ok(None) => self.flash("This tab has no conversation to copy yet"),
            Err(e) => self.flash(format!("Could not copy the session: {e}")),
        }
    }

    /// Copy the tab's transcript into `target`'s config dir: the id there,
    /// or None when the tab has no conversation yet.
    fn copy_transcript(
        &self,
        slot: usize,
        tab: usize,
        target: usize,
    ) -> Result<Option<String>, String> {
        let Some(src_acct) = self.panes[slot].account else {
            return Ok(None);
        };
        let Some((jsonl, id)) = self.transcript_of(slot, tab) else {
            return Ok(None);
        };
        let sdir = self.cfg.accounts[src_acct].config_dir();
        let tdir = self.cfg.accounts[target].config_dir();
        // Already there: reuse it, unless ours is longer (newer turns, e.g.
        // moving back after an undo), then it wins.
        let there = jsonl.strip_prefix(&sdir).map(|r| tdir.join(r)).ok();
        let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let conflict = match there {
            Some(t) if t.exists() && size(&jsonl) > size(&t) => Conflict::Overwrite,
            _ => Conflict::Skip,
        };
        session_ops::copy_session(&sdir, &jsonl, &id, &tdir, conflict)
            .map(|c| Some(c.id))
            .map_err(|e| format!("{e:#}"))
    }

    /// Move now, or ask first when the tab is busy.
    pub fn request_move(
        &mut self,
        slot: usize,
        tab: usize,
        target: usize,
        copy: bool,
        same_folder: bool,
    ) {
        let Some(t) = self.panes.get(slot).and_then(|s| s.tabs.get(tab)) else {
            return;
        };
        if t.is_running() && matches!(t.activity, Activity::Working) {
            self.modal = Modal::MoveBusy(QueuedMove {
                uid: t.uid,
                target,
                copy,
                same_folder,
            });
            return;
        }
        self.move_tab_now(slot, tab, target, copy, same_folder);
    }

    /// Busy dialog: wait for idle (queued), or interrupt then move.
    pub fn move_busy_choice(&mut self, interrupt: bool) {
        let Modal::MoveBusy(q) = self.modal.clone() else {
            return;
        };
        self.modal = Modal::None;
        if interrupt {
            if let Some((s, t)) = self.find_tab(q.uid) {
                self.panes[s].tabs[t].write(b"\x1b");
            }
        }
        self.flash(if interrupt {
            "Interrupted; moving once it stops"
        } else {
            "The tab moves once it finishes"
        });
        self.queued_moves.push(q);
    }

    /// A move that did not happen: its old id names its own tab again.
    pub(crate) fn forget_move(&mut self, source: u64, target: u64) {
        if self.moved_ids.get(&source) == Some(&target) {
            self.moved_ids.remove(&source);
            crate::log::info(&format!(
                "move: t{source} stays t{source} (t{target} failed)"
            ));
        }
    }

    pub fn find_tab(&self, uid: u64) -> Option<(usize, usize)> {
        self.panes
            .iter()
            .enumerate()
            .find_map(|(s, p)| p.tabs.iter().position(|t| t.uid == uid).map(|t| (s, t)))
    }

    /// Where a tab's transcript lives in its account.
    pub(crate) fn transcript_of(
        &self,
        slot: usize,
        tab: usize,
    ) -> Option<(std::path::PathBuf, String)> {
        let a = self.panes[slot].account?;
        let t = &self.panes[slot].tabs[tab];
        let id = t.session_id.clone()?;
        let dir = self.cfg.accounts[a].config_dir();
        let guess = dir
            .join("projects")
            .join(crate::state::encode_project_dir(&t.cwd))
            .join(format!("{id}.jsonl"));
        if guess.is_file() {
            return Some((guess, id));
        }
        let projects = std::fs::read_dir(dir.join("projects")).ok()?;
        projects
            .flatten()
            .map(|p| p.path().join(format!("{id}.jsonl")))
            .find(|p| p.is_file())
            .map(|p| (p, id))
    }

    /// Copy the session to `target` and open it there. The source closes
    /// (for a move) only once the target tab is up.
    pub fn move_tab_now(
        &mut self,
        slot: usize,
        tab: usize,
        target: usize,
        copy: bool,
        same_folder: bool,
    ) {
        let Some(src_acct) = self.panes[slot].account else {
            return;
        };
        if let Some(why) = self.cfg.accounts[src_acct].harness().no_move_reason() {
            self.flash(why);
            return;
        }
        // Learn the session id if the tab started recently.
        self.detect_sessions();
        let src = &self.panes[slot].tabs[tab];
        let (cwd, name, uid) = (src.cwd.clone(), src.custom_name.clone(), src.uid);
        let cwd = if same_folder {
            cwd
        } else {
            self.cfg.accounts[target].work_dir()
        };
        let label = src.name();
        // The browser binding goes with the conversation.
        let browser = src.browser.clone();
        let kind = match self.copy_transcript(slot, tab, target) {
            Ok(Some(id)) => LaunchKind::Resume(id),
            Err(e) => {
                self.flash(format!("Could not copy the session: {e}"));
                return;
            }
            // No conversation yet: a fresh tab in the same folder.
            Ok(None) => LaunchKind::Normal,
        };
        crate::log::info(&format!(
            "move tab {label} from {} to {} ({kind:?}, copy {copy})",
            self.cfg.accounts[src_acct].name, self.cfg.accounts[target].name
        ));
        let p = self.pane_for_account(target);
        if self.panes[p].account != Some(target) {
            self.ensure_panes();
        }
        let p = self.pane_for_account(target);
        self.panes[p].hidden = false;
        let ti = if self.panes[p].tabs.len() == 1
            && !self.panes[p].cur().is_running()
            && self.panes[p].cur().pending.is_none()
        {
            self.panes[p].cur_mut().cwd = cwd.clone();
            0
        } else {
            self.panes[p].add_tab(cwd.clone())
        };
        self.panes[p].tabs[ti].custom_name = name;
        self.panes[p].tabs[ti].session_id = match &kind {
            LaunchKind::Resume(id) => Some(id.clone()),
            _ => None,
        };
        self.panes[p].tabs[ti].browser = browser;
        self.launch_tab(p, ti, kind);
        self.panes[p].active = ti;
        self.focus = p;
        self.view = View::Grid;
        let target_uid = self.panes[p].tabs[ti].uid;
        let from = self.cfg.accounts[src_acct].display().to_string();
        self.tab_event(
            p,
            ti,
            if copy { "copy" } else { "move" },
            None,
            Some(format!("from {from} (was t{uid})")),
        );
        if !copy {
            self.moved_ids.insert(uid, target_uid);
            crate::log::info(&format!("move: t{uid} is now t{target_uid}"));
            let account = self.cfg.accounts[target].name.clone();
            self.jobs_follow_move(uid, target_uid, &account);
        }
        self.pending_moves.push(PendingMove {
            source_uid: uid,
            target_uid,
            copy,
            since: Instant::now(),
            name: label,
            target,
        });
        self.state_dirty = true;
    }

    /// Called from tick: finish started moves, start queued ones.
    pub fn tabmove_tick(&mut self) {
        // Queued: the tab went idle.
        let queued = std::mem::take(&mut self.queued_moves);
        for q in queued {
            match self.find_tab(q.uid) {
                Some((s, t)) if matches!(self.panes[s].tabs[t].activity, Activity::Working) => {
                    self.queued_moves.push(q)
                }
                Some((s, t)) => self.move_tab_now(s, t, q.target, q.copy, q.same_folder),
                None => {}
            }
        }
        // Started: the target is up (or failed).
        let pending = std::mem::take(&mut self.pending_moves);
        for m in pending {
            let Some((ts, tt)) = self.find_tab(m.target_uid) else {
                // The new tab is gone: the source is the tab again.
                self.forget_move(m.source_uid, m.target_uid);
                continue;
            };
            let t = &self.panes[ts].tabs[tt];
            let up = t.is_running()
                && (matches!(
                    t.activity,
                    Activity::Ready | Activity::Working | Activity::Permission
                ) || m.since.elapsed() > Duration::from_secs(8));
            let failed = !t.is_running() && m.since.elapsed() > Duration::from_millis(300);
            let left = self.accounts[m.target]
                .five_hour_left()
                .map(|l| format!(" ({l:.0}% left)"))
                .unwrap_or_default();
            let tname = self.cfg.accounts[m.target].display().to_string();
            if failed {
                // The original stays open and keeps its id: tools that name
                // it must reach it, not the failed copy.
                if self.find_tab(m.source_uid).is_some() {
                    self.forget_move(m.source_uid, m.target_uid);
                }
                self.flash(format!(
                    "Could not resume '{}' in {tname}; the original tab is still open (Ctrl-a r restarts it)",
                    m.name
                ));
                continue;
            }
            if !up {
                self.pending_moves.push(m);
                continue;
            }
            let mut src_acct = None;
            if !m.copy {
                if let Some((ss, st)) = self.find_tab(m.source_uid) {
                    src_acct = self.panes[ss].account;
                    let fallback = src_acct
                        .map(|a| self.cfg.accounts[a].work_dir())
                        .unwrap_or_default();
                    self.panes[ss].close_tab(st, fallback);
                }
            }
            if let Some(sa) = src_acct {
                self.last_tab_move = Some(DoneMove {
                    target_uid: m.target_uid,
                    source_account: sa,
                    at: Instant::now(),
                });
                self.flash(format!(
                    "Moved '{}' to {tname}{left}. Undo: Ctrl-a U (10 s)",
                    m.name
                ));
            } else {
                self.flash(format!(
                    "Copied '{}' to {tname}{left}; both tabs are open",
                    m.name
                ));
            }
            self.state_dirty = true;
        }
    }

    /// Undo the last tab move within UNDO_FOR: move it back.
    pub fn undo_tab_move(&mut self) {
        let Some(d) = self.last_tab_move.take() else {
            self.flash("No tab move to undo");
            return;
        };
        if d.at.elapsed() > UNDO_FOR {
            self.flash("Too late to undo; move the tab back with Ctrl-a m");
            return;
        }
        if let Some((s, t)) = self.find_tab(d.target_uid) {
            self.move_tab_now(s, t, d.source_account, false, true);
        }
    }

    /// An Undo chip shows while the last move can still be undone.
    pub fn undo_move_live(&self) -> bool {
        self.last_tab_move
            .as_ref()
            .is_some_and(|d| d.at.elapsed() < UNDO_FOR)
    }

    /// A low account's tab gets a chip suggesting the account with the
    /// most left: (target, % left).
    pub fn move_suggestion(&self, slot: usize) -> Option<(usize, f64)> {
        let below = self.cfg.suggest_move_below;
        if below <= 0.0 {
            return None;
        }
        let a = self.panes.get(slot)?.account?;
        let left = self.accounts.get(a)?.effective_left()?;
        if left >= below {
            return None;
        }
        let best = self
            .move_targets(slot)
            .into_iter()
            .find(|t| t.disabled.is_none() && !t.exhausted())?;
        let bl = best.effective()?;
        (bl > left + 20.0).then_some((best.account, bl))
    }
}

impl App {
    pub fn open_tab_menu(&mut self, slot: usize, tab: usize) {
        self.jump_to(slot, tab);
        self.modal = Modal::TabMenu(slot, tab, 0);
    }

    /// Row `i` of the right click menu (app_tabopts::tab_actions).
    pub fn tab_menu_run(&mut self, slot: usize, tab: usize, i: usize) {
        match self.tab_actions(slot, tab).get(i).map(|r| r.act) {
            Some(a) => self.tab_act_run(slot, tab, a),
            None => self.modal = Modal::None,
        }
    }

    pub fn on_move_modal_key(&mut self, k: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match self.modal.clone() {
            Modal::MoveTab(_) => self.on_tab_panel_key(k),
            Modal::MoveBusy(_) => match k.code {
                KeyCode::Char('w') | KeyCode::Char('y') | KeyCode::Enter => {
                    self.move_busy_choice(false)
                }
                KeyCode::Char('i') => self.move_busy_choice(true),
                _ => self.modal = Modal::None,
            },
            Modal::TabMenu(s, t, sel) => {
                let rows = self.tab_actions(s, t);
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Up => self.modal = Modal::TabMenu(s, t, sel.saturating_sub(1)),
                    KeyCode::Down => {
                        self.modal =
                            Modal::TabMenu(s, t, (sel + 1).min(rows.len().saturating_sub(1)))
                    }
                    KeyCode::Enter => self.tab_menu_run(s, t, sel),
                    KeyCode::Char(c) => {
                        if let Some(i) = rows.iter().position(|r| r.key == c) {
                            self.tab_menu_run(s, t, i);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(a: usize, f: Option<f64>, w: Option<f64>, dis: bool) -> MoveTarget {
        MoveTarget {
            account: a,
            disabled: dis.then(|| "not logged in".into()),
            five_left: f,
            week_left: w,
            tabs: 0,
        }
    }

    #[test]
    fn sorting() {
        let mut v = vec![
            t(0, Some(40.0), Some(90.0), false),
            t(1, Some(82.0), Some(10.0), false),
            t(2, Some(82.0), Some(50.0), false),
            t(3, Some(99.0), None, true),
            t(4, None, None, false),
            t(5, Some(100.0), Some(0.0), false),
        ];
        sort_targets(&mut v);
        let order: Vec<usize> = v.iter().map(|x| x.account).collect();
        // By what is really left (the lower window), unknown usage after,
        // a weekly exhausted account (100% 5h, 0% weekly) below every
        // usable one, disabled at the bottom whatever their usage.
        assert_eq!(order, vec![2, 0, 1, 4, 5, 3]);
        assert!(t(5, Some(100.0), Some(0.0), false).exhausted());
    }
}
