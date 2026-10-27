use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::{App, Modal, View};
use crate::pane::Activity;
use crate::pane::LaunchKind;
use crate::session_ops::{self, Conflict};

#[derive(Debug, Clone, PartialEq)]
pub struct MoveTarget {
    pub account: usize,

    pub disabled: Option<String>,
    pub five_left: Option<f64>,
    pub week_left: Option<f64>,
    pub tabs: usize,
}

impl MoveTarget {
    pub fn effective(&self) -> Option<f64> {
        match (self.five_left, self.week_left) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn exhausted(&self) -> bool {
        self.effective().is_some_and(|e| e < 2.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MovePicker {
    pub slot: usize,
    pub tab: usize,
    pub sel: usize,

    pub copy: bool,
    pub same_folder: bool,

    pub name: String,
    pub name_edit: bool,
    pub name_orig: String,

    pub opt: Option<usize>,

    pub title: String,
    pub title_orig: String,
    pub title_edit: bool,

    pub color_sel: usize,
    pub group_sel: usize,

    pub session_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueuedMove {
    pub uid: u64,
    pub target: usize,
    pub copy: bool,
    pub same_folder: bool,
}

#[derive(Debug, Clone)]
pub struct PendingMove {
    pub source_uid: u64,
    pub target_uid: u64,
    pub copy: bool,
    pub since: Instant,
    pub name: String,
    pub target: usize,
}

#[derive(Debug, Clone)]
pub struct DoneMove {
    pub target_uid: u64,
    pub source_account: usize,
    pub at: Instant,
}

pub const UNDO_FOR: Duration = Duration::from_secs(10);

pub fn sort_targets(v: &mut [MoveTarget]) {
    v.sort_by(|a, b| {
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

    pub fn move_refusal(&self, slot: usize, target: usize) -> Option<String> {
        let src = self.panes.get(slot)?.account?;
        let from = self.cfg.accounts.get(src)?.harness();
        if let Some(why) = from.no_move_reason() {
            return Some(why);
        }
        let to = self.cfg.accounts.get(target)?;
        (to.harness() != from).then(|| {
            format!(
                "{} is a {} account: a {} conversation moves only to another {} account",
                to.display(),
                to.harness().label(),
                from.label(),
                from.label()
            )
        })
    }

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

    pub fn copy_session_to(&mut self, slot: usize, tab: usize, target: usize) {
        self.detect_sessions();
        match self.copy_transcript(slot, tab, target) {
            Ok(Some(id)) => self.flash(format!(
                "Copied the session to {}; resume it there from Sessions ({})",
                self.cfg.accounts[target].display(),
                crate::paths::head(&id, 8)
            )),
            Ok(None) => self.flash("This tab has no conversation to copy yet"),
            Err(e) => self.flash(format!("Could not copy the session: {e}")),
        }
    }

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

    pub fn queue_move(&mut self, q: QueuedMove) {
        self.queued_moves.retain(|m| m.uid != q.uid);
        self.queued_moves.push(q);
    }

    pub fn move_pending(&self, uid: u64) -> Option<(String, String)> {
        let q = self.queued_moves.iter().find(|m| m.uid == uid)?;
        let (s, t) = self.find_tab(uid)?;
        let to = self.cfg.accounts.get(q.target)?.display().to_string();
        let why = self
            .tab_busy_why(s, t)
            .unwrap_or_else(|| "its work ends".into());
        Some((to, why))
    }

    pub(crate) fn move_now_plan_for(
        &mut self,
        args: &Value,
    ) -> Result<(Value, String, bool), String> {
        let tabs = self.tab_targets(args, true)?;
        let to = match args.get("to") {
            Some(v) if !v.is_null() => self.account_arg(v)?,
            _ => return Err("to is required".into()),
        };
        let mut uids = vec![];
        let mut stops: Vec<String> = vec![];
        for &(s, t) in &tabs {
            let uid = self.panes[s].tabs[t].uid;
            if self.panes[s].account == Some(to) {
                continue;
            }
            if let Some(why) = self.move_refusal(s, to) {
                return Err(why);
            }
            uids.push(uid);
            if let Some(w) = self.tab_busy_why(s, t) {
                stops.push(format!("{}: {w}", self.panes[s].tabs[t].name()));
            }
        }
        if uids.is_empty() {
            return Err("those tabs are already on that account".into());
        }
        let label = self.cfg.accounts[to].display().to_string();
        let q = if stops.is_empty() {
            format!("Move {} to {label} now?", self.describe_set(&tabs))
        } else {
            format!(
                "Move {} to {label} now? This stops what runs in {}.",
                self.describe_set(&tabs),
                stops.join("; ")
            )
        };
        Ok((json!({"uids": uids, "to": to}), q, true))
    }

    pub(crate) fn move_now_run(&mut self, plan: &Value) -> Result<Value, String> {
        let to = plan["to"].as_u64().ok_or("the plan has no account")? as usize;
        let mut moved = vec![];
        for uid in plan["uids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
        {
            let uid = self.current_uid(uid);
            self.queued_moves.retain(|m| m.uid != uid);
            let Some((s, t)) = self.find_tab(uid) else {
                continue;
            };
            if self.panes[s].account == Some(to) {
                continue;
            }
            self.move_tab_now(s, t, to, false, true);
            let new = self.moved_ids.get(&uid).copied().or_else(|| {
                self.pending_moves
                    .last()
                    .filter(|m| m.source_uid == uid)
                    .map(|m| m.target_uid)
            });
            moved.push(json!({"from": crate::control::tab_id(uid), "now": new.map(crate::control::tab_id)}));
        }
        let label = self.cfg.accounts[to].display().to_string();
        Ok(json!({"ok": true, "result": {
            "moved": moved, "to": label,
            "say": format!("Moved {} to {label}; what ran in {} was stopped.",
                crate::control::plural(moved.len(), "tab"),
                if moved.len() == 1 { "it" } else { "them" }),
        }}))
    }

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

        if self.tab_has_running_work(slot, tab) {
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
        self.queue_move(q);
    }

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
        if let Some(why) = self.move_refusal(slot, target) {
            self.flash(why);
            return;
        }

        self.detect_sessions();
        let src = &self.panes[slot].tabs[tab];
        let (cwd, name, uid) = (src.cwd.clone(), src.custom_name.clone(), src.uid);
        let cwd = if same_folder {
            cwd
        } else {
            self.cfg.accounts[target].work_dir()
        };
        let label = src.name();

        let browser = src.browser.clone();
        let browser_also = src.browser_also.clone();
        let kind = match self.copy_transcript(slot, tab, target) {
            Ok(Some(id)) => LaunchKind::Resume(id),
            Err(e) => {
                self.flash(format!("Could not copy the session: {e}"));
                return;
            }

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
        self.panes[p].tabs[ti].browser_also = browser_also;
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

    pub fn tabmove_tick(&mut self) {
        let queued = std::mem::take(&mut self.queued_moves);
        for q in queued {
            match self.find_tab(q.uid) {
                Some((s, t)) if self.tab_has_running_work(s, t) => self.queued_moves.push(q),
                Some((s, t)) => self.move_tab_now(s, t, q.target, q.copy, q.same_folder),
                None => {}
            }
        }

        let pending = std::mem::take(&mut self.pending_moves);
        for m in pending {
            let Some((ts, tt)) = self.find_tab(m.target_uid) else {
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

    pub fn undo_move_live(&self) -> bool {
        self.last_tab_move
            .as_ref()
            .is_some_and(|d| d.at.elapsed() < UNDO_FOR)
    }

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

        assert_eq!(order, vec![2, 0, 1, 4, 5, 3]);
        assert!(t(5, Some(100.0), Some(0.0), false).exhausted());
    }
}
