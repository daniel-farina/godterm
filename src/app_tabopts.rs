//! A tab's options, one list for both places that offer them: the right
//! click menu (each row runs or opens its dialog) and the double click
//! panel, where they sit under the Move section and most act in place.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Modal};
use crate::app_tabmove::MovePicker;
use crate::color_pick::Target;
use crate::pane::Activity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAct {
    Rename,
    SessionTitle,
    Color,
    Pin,
    Group,
    Move,
    Duplicate,
    CopySession,
    Restart,
    Interrupt,
    CopyPath,
    Finder,
    Terminal,
    Git,
    TalkDirect,
    Archive,
    Close,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabActRow {
    pub act: TabAct,
    pub label: String,
    /// The right click menu's shortcut.
    pub key: char,
    pub hint: &'static str,
    /// Some: shown dim, with why.
    pub disabled: Option<String>,
}

impl App {
    /// Every per tab action, in menu order.
    pub fn tab_actions(&self, s: usize, t: usize) -> Vec<TabActRow> {
        use TabAct::*;
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return vec![];
        };
        let no_target = self
            .move_targets(s)
            .iter()
            .all(|x| x.disabled.is_some())
            .then(|| "no other logged in account".to_string());
        let no_session = tab
            .session_id
            .is_none()
            .then(|| "no conversation yet".to_string());
        let row = |act, label: &str, key, hint, disabled: Option<String>| TabActRow {
            act,
            label: label.into(),
            key,
            hint,
            disabled,
        };
        vec![
            row(Rename, "Rename", 'r', "The tab's name in GodTerm", None),
            row(
                SessionTitle,
                "Session title",
                't',
                "The session's own title (claude --resume, Grok's picker)",
                no_session.clone(),
            ),
            row(Color, "Accent color...", 'a', "The tab's own color", None),
            row(
                Pin,
                if tab.pinned { "Unpin" } else { "Pin" },
                'p',
                "Pinned: first in the list, asks before closing",
                None,
            ),
            row(
                Group,
                "Group...",
                'g',
                "Add to a group, a new one, or none",
                None,
            ),
            row(
                Move,
                "Move to...",
                'm',
                "Continue the conversation on another account",
                no_target.clone(),
            ),
            row(
                Duplicate,
                "Duplicate to...",
                'd',
                "Open it on another account too; this tab stays",
                no_target.clone(),
            ),
            row(
                CopySession,
                "Copy session to...",
                's',
                "Copy the conversation to another account, no tab opened",
                no_session.or(no_target),
            ),
            row(
                Restart,
                if tab.is_running() {
                    "Restart"
                } else {
                    "Reopen"
                },
                'e',
                "Start the tab again, resuming its conversation",
                None,
            ),
            row(
                Interrupt,
                "Interrupt",
                'i',
                "Send Esc: stop its current work",
                (!matches!(tab.activity, Activity::Working) || !tab.is_running())
                    .then(|| "not working".to_string()),
            ),
            row(
                CopyPath,
                "Copy folder path",
                'y',
                "Copy the tab's folder",
                None,
            ),
            row(
                Finder,
                "Open folder in Finder",
                'o',
                "Show the folder",
                None,
            ),
            row(
                Terminal,
                "Terminal here",
                'h',
                "A terminal in the tab's folder",
                None,
            ),
            row(
                Git,
                "Git...",
                'G',
                "Status, commit, push and pull, branches, the GitHub repo (nothing happens without your yes)",
                None,
            ),
            row(
                TalkDirect,
                if self.direct_with(tab.uid) {
                    "Stop talking directly"
                } else {
                    "Talk directly by voice"
                },
                'v',
                "Your voice goes to this tab and its replies are said; \"back to the assistant\" returns (Ctrl-a K)",
                (tab.activity == Activity::Exited
                    || (!tab.is_running() && tab.pending.is_none() && !tab.suspended))
                    .then(|| "not running".to_string()),
            ),
            row(
                Archive,
                "Archive",
                'f',
                "Stop its session and put it under Archived; a double click there resumes it (Ctrl-a f)",
                self.archive_blockers(s, t)
                    .into_iter()
                    .find(|b| b.hard)
                    .map(|b| b.why),
            ),
            row(
                Close,
                "Close tab",
                'x',
                "Close it (asks first when busy or pinned; Undo brings it back)",
                None,
            ),
        ]
    }

    /// The rows the double click panel lists under its Move section.
    pub fn panel_rows(&self, s: usize, t: usize) -> Vec<TabActRow> {
        self.tab_actions(s, t)
            .into_iter()
            // Move is the panel's top section; Git opens its own section
            // from the right click menu (the panel stays 80x24).
            .filter(|r| r.act != TabAct::Move && r.act != TabAct::Git)
            .collect()
    }

    /// The session's own title, when the index has it (never waits on it).
    pub fn session_title(&self, s: usize, t: usize) -> Option<String> {
        let id = self.panes.get(s)?.tabs.get(t)?.session_id.clone()?;
        let ix = self.session_index.try_lock().ok()?;
        ix.rows
            .iter()
            .find(|r| r.info.id == id)
            .and_then(|r| r.info.name.clone().or_else(|| r.info.title.clone()))
    }

    /// Run an action from the right click menu (or one the panel hands on).
    pub fn tab_act_run(&mut self, s: usize, t: usize, act: TabAct) {
        self.modal = Modal::None;
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return;
        };
        let uid = tab.uid;
        if let Some(why) = self
            .tab_actions(s, t)
            .into_iter()
            .find(|r| r.act == act)
            .and_then(|r| r.disabled)
        {
            self.flash(format!("Can't do that: {why}"));
            return;
        }
        match act {
            TabAct::Rename => self.start_rename(s, t),
            TabAct::SessionTitle => {
                self.open_tab_panel(s, t);
                if let Modal::MoveTab(mut p) = self.modal.clone() {
                    p.opt = self.panel_index(s, t, TabAct::SessionTitle);
                    p.title_edit = true;
                    self.modal = Modal::MoveTab(p);
                }
            }
            TabAct::Color => self.open_color_pick(Target::Tab(uid)),
            TabAct::Pin => self.toggle_pin(s, t),
            TabAct::Group => self.modal = Modal::GroupPick(s, t, 0),
            TabAct::Move => self.open_move_picker(s, t, false),
            TabAct::Duplicate => self.open_move_picker(s, t, true),
            TabAct::CopySession => {
                self.open_move_picker(s, t, true);
                if let Modal::MoveTab(p) = &mut self.modal {
                    p.session_only = true;
                }
            }
            TabAct::Restart => {
                self.jump_to(s, t);
                let kind = self.panes[s].tabs[t].relaunch_kind();
                self.launch_tab(s, t, kind);
            }
            TabAct::Interrupt => self.panes[s].tabs[t].write(b"\x1b"),
            TabAct::CopyPath => {
                self.jump_to(s, t);
                self.copy_path(s);
            }
            TabAct::Finder => {
                self.jump_to(s, t);
                self.open_in_finder(s);
            }
            TabAct::Terminal => {
                self.jump_to(s, t);
                self.terminal_here(s);
            }
            TabAct::Git => self.open_git(s, t),
            TabAct::TalkDirect => {
                if self.direct_with(uid) {
                    self.end_direct("tab menu", true);
                } else {
                    self.direct_on_tab(s, t);
                }
            }
            TabAct::Archive => self.request_archive(s, t),
            TabAct::Close => self.request_close(s, t),
        }
    }

    fn panel_index(&self, s: usize, t: usize, act: TabAct) -> Option<usize> {
        self.panel_rows(s, t).iter().position(|r| r.act == act)
    }

    /// Double click on a tab: Move on top, the tab's options below. Opens
    /// even when no other account can take the tab.
    pub fn open_tab_panel(&mut self, s: usize, t: usize) {
        if self.panes.get(s).and_then(|p| p.tabs.get(t)).is_none() {
            return;
        }
        let tab = &self.panes[s].tabs[t];
        let name = tab.name();
        let color_sel = tab
            .accent
            .as_ref()
            .and_then(|c| crate::theme::SWATCHES.iter().position(|x| x == c))
            .map(|i| i + 1)
            .unwrap_or(0);
        let group_sel = tab
            .group
            .as_ref()
            .and_then(|g| self.panes[s].groups.iter().position(|x| &x.name == g))
            .unwrap_or(0);
        let title = self.session_title(s, t).unwrap_or_default();
        let none = self.move_targets(s).iter().all(|x| x.disabled.is_some());
        self.modal = Modal::MoveTab(MovePicker {
            slot: s,
            tab: t,
            sel: 0,
            copy: false,
            same_folder: true,
            name_orig: name.clone(),
            name,
            name_edit: false,
            opt: none.then_some(0),
            title_orig: title.clone(),
            title,
            title_edit: false,
            color_sel,
            group_sel,
            session_only: false,
        });
    }

    /// Activate option row `i` of the panel (Enter or a click).
    pub fn panel_activate(&mut self, i: usize) {
        let Modal::MoveTab(mut p) = self.modal.clone() else {
            return;
        };
        let (s, t) = (p.slot, p.tab);
        let Some(row) = self.panel_rows(s, t).into_iter().nth(i) else {
            return;
        };
        p.opt = Some(i);
        p.name_edit = false;
        p.title_edit = false;
        if let Some(why) = &row.disabled {
            if row.act == TabAct::SessionTitle {
                self.flash(format!("Session title: {why}"));
            } else {
                self.flash(format!("{}: {why}", row.label));
            }
            self.modal = Modal::MoveTab(p);
            return;
        }
        let uid = self.panes[s].tabs[t].uid;
        match row.act {
            TabAct::Rename => {
                p.name_orig = p.name.clone();
                p.name_edit = true;
            }
            TabAct::SessionTitle => {
                p.title_orig = p.title.clone();
                p.title_edit = true;
            }
            TabAct::Color => self.set_color(Target::Tab(uid), p.color_sel),
            TabAct::Pin => self.toggle_pin(s, t),
            TabAct::Group => {
                self.group_pick_run(s, t, p.group_sel);
                // New group... asks for its name; the others stay here.
                if !matches!(self.modal, Modal::None) {
                    return;
                }
            }
            TabAct::Duplicate | TabAct::CopySession => {
                p.copy = true;
                p.session_only = row.act == TabAct::CopySession;
                p.opt = None;
            }
            other => return self.tab_act_run(s, t, other),
        }
        self.modal = Modal::MoveTab(p);
    }

    /// Activate the panel row of `act`.
    pub fn panel_activate_act(&mut self, act: TabAct) {
        if let Modal::MoveTab(p) = &self.modal {
            if let Some(i) = self.panel_index(p.slot, p.tab, act) {
                self.panel_activate(i);
            }
        }
    }

    /// A click elsewhere in the panel keeps what was typed in a field.
    pub fn panel_end_edit(&mut self) {
        let Modal::MoveTab(mut p) = self.modal.clone() else {
            return;
        };
        if p.name_edit {
            self.apply_picker_name(&p);
            p.name_orig = p.name.clone();
        } else if p.title_edit {
            self.save_session_title(&p);
            p.title_orig = p.title.clone();
        } else {
            return;
        }
        p.name_edit = false;
        p.title_edit = false;
        if matches!(self.modal, Modal::MoveTab(_)) {
            self.modal = Modal::MoveTab(p);
        }
    }

    /// Save the session title typed in the panel.
    fn save_session_title(&mut self, p: &MovePicker) {
        let name = p.title.trim();
        if name.is_empty() || name == p.title_orig.trim() {
            return;
        }
        let Some(id) = self
            .panes
            .get(p.slot)
            .and_then(|x| x.tabs.get(p.tab))
            .and_then(|x| x.session_id.clone())
        else {
            return;
        };
        let args = serde_json::json!({"id": id, "name": name});
        let r = self
            .rename_plan_for(&args)
            .and_then(|(plan, _, _)| self.rename_run(&plan));
        match r {
            Ok(v) => {
                let say = v["say"]
                    .as_str()
                    .unwrap_or("Renamed the session")
                    .to_string();
                self.flash(say);
            }
            Err(e) => self.flash(format!("Could not rename the session: {e}")),
        }
    }

    /// Keys while the panel is open.
    pub fn on_tab_panel_key(&mut self, k: KeyEvent) {
        let Modal::MoveTab(mut p) = self.modal.clone() else {
            return;
        };
        let n = self.move_targets(p.slot).len();
        let rows = self.panel_rows(p.slot, p.tab);
        let m = rows.len();
        if p.name_edit || p.title_edit {
            let field = if p.name_edit {
                &mut p.name
            } else {
                &mut p.title
            };
            match k.code {
                KeyCode::Esc => {
                    if p.name_edit {
                        p.name = p.name_orig.clone();
                    } else {
                        p.title = p.title_orig.clone();
                    }
                    p.name_edit = false;
                    p.title_edit = false;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    if p.name_edit {
                        self.apply_picker_name(&p);
                        p.name_orig = p.name.clone();
                    } else {
                        self.save_session_title(&p);
                        p.title_orig = p.title.clone();
                    }
                    p.name_edit = false;
                    p.title_edit = false;
                }
                KeyCode::Backspace => {
                    field.pop();
                }
                KeyCode::Char(c) if field.chars().count() < 80 => field.push(c),
                _ => {}
            }
            if matches!(self.modal, Modal::MoveTab(_)) {
                self.modal = Modal::MoveTab(p);
            }
            return;
        }
        // One line through both sections: targets, then the options.
        let total = n + m;
        let cur = match p.opt {
            None => p.sel.min(n.saturating_sub(1)),
            Some(i) => n + i,
        };
        let go = |p: &mut MovePicker, ix: usize| {
            if ix < n {
                p.opt = None;
                p.sel = ix;
            } else {
                p.opt = Some((ix - n).min(m.saturating_sub(1)));
            }
        };
        let act = p.opt.and_then(|i| rows.get(i)).map(|r| r.act);
        match k.code {
            KeyCode::Esc => return self.cancel_move_picker(),
            KeyCode::Up | KeyCode::BackTab => go(&mut p, cur.saturating_sub(1)),
            KeyCode::Down | KeyCode::Tab if total > 0 => go(&mut p, (cur + 1).min(total - 1)),
            KeyCode::Left | KeyCode::Right => {
                let back = k.code == KeyCode::Left;
                let step = |v: usize, len: usize| {
                    if len == 0 {
                        0
                    } else if back {
                        (v + len - 1) % len
                    } else {
                        (v + 1) % len
                    }
                };
                match act {
                    Some(TabAct::Color) => {
                        p.color_sel = step(p.color_sel, crate::color_pick::count())
                    }
                    Some(TabAct::Group) => {
                        p.group_sel = step(p.group_sel, self.group_choices(p.slot).len())
                    }
                    _ => {}
                }
            }
            KeyCode::Char('n') => {
                p.opt = self.panel_index(p.slot, p.tab, TabAct::Rename);
                p.name_orig = p.name.clone();
                p.name_edit = true;
            }
            KeyCode::Char('c') => {
                p.copy = !p.copy;
                p.session_only = false;
            }
            KeyCode::Char('f') => p.same_folder = !p.same_folder,
            KeyCode::Enter => {
                return match p.opt {
                    None => self.confirm_move_picker(),
                    Some(i) => {
                        self.modal = Modal::MoveTab(p);
                        self.panel_activate(i)
                    }
                }
            }
            _ => {}
        }
        if matches!(self.modal, Modal::MoveTab(_)) {
            self.modal = Modal::MoveTab(p);
        }
    }
}
