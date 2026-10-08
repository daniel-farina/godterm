//! Mouse handling: look clicks up in the hit registry of the last frame and
//! run the action. Hover updates the highlight and the status bar hint.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::{Duration, Instant};

use crate::app::{App, Modal, View};
use crate::hits::{List, UiAction};

const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// Tour steps: (title, text).
pub const TOUR: &[(&str, &str)] = &[
    (
        "Menu bar",
        "Tabs, View, Voice and Settings are menus, like the macOS menu bar: click one (then hover across to the others), pick with the mouse or arrows and Enter, Esc or a click outside closes. Each item shows its Ctrl-a shortcut. Approvals and the Mic mute (solid red ● MUTED when muted) are one click. To come back here, the grid of all your sessions, click the ◆ GodTerm logo (or Grid) or press Esc.",
    ),
    (
        "Panes",
        "Every account side by side, each logged in separately (more than fit go on pages). Click a pane to focus it; what you type goes to the focused claude.",
    ),
    (
        "Tabs",
        "Each pane lists its tabs (left by default; right or top with Ctrl-a S or the account menu). Click a tab to switch, double click to move it to another account (right click: Rename, Move, Duplicate, Close), x to close, + New tab to add one, « to collapse the list.",
    ),
    (
        "Usage footer",
        "How much is left in the 5 hour and weekly windows for that account. Click it to open the dashboard for the account.",
    ),
    (
        "Approvals",
        "Tabs start in bypass mode (the badge in each header; click it to change). In other modes, or for claude's one time bypass warning, Approve / Always / Deny buttons appear under the pane and the Approvals button lists every waiting tab.",
    ),
    (
        "Status bar",
        "Modes show as chips at the bottom left, always in the same order: the voice mode (click to change it), MEM SAVER, PRIVACY, ZOOMED, the assistant's account, UNDO MOVE. Click a chip to change or undo it.",
    ),
    (
        "Voice",
        "Click Mic (or Ctrl-a space) and say what you want: \"open a tab on account two\", \"what did it say\". The Voice menu picks push to talk, wake word or open mic.",
    ),
];

/// Encode a mouse event for a child that enabled mouse reporting (SGR).
pub fn encode_mouse(m: &MouseEvent, col: u16, row: u16) -> Option<Vec<u8>> {
    let (code, release) = match m.kind {
        MouseEventKind::Down(b) => (button_code(b), false),
        MouseEventKind::Up(b) => (button_code(b), true),
        MouseEventKind::Drag(b) => (button_code(b) + 32, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::Moved => (35, false),
        _ => return None,
    };
    let mut code = code;
    if m.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if m.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if m.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    Some(
        format!(
            "\x1b[<{code};{};{}{}",
            col + 1,
            row + 1,
            if release { 'm' } else { 'M' }
        )
        .into_bytes(),
    )
}

fn button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

impl App {
    /// Menus, context menus and popovers: a click outside closes them.
    pub fn is_popup(&self) -> bool {
        matches!(
            self.modal,
            Modal::Menu(_)
                | Modal::TakeOver(_)
                | Modal::TabMenu(..)
                | Modal::AccountMenu(..)
                | Modal::PathMenu(_)
                | Modal::AddAccount(_)
                | Modal::ColorPick(..)
                | Modal::EmptySlotMenu(_)
                | Modal::MenuOverflow
                | Modal::GrokLogins(_)
        )
    }

    /// The region under (col, row), honoring an open modal: only the
    /// modal's own regions are live while it is open.
    pub fn region_at(&self, col: u16, row: u16) -> Option<crate::hits::Region> {
        let regions = &self.last_hits.regions;
        let from = if self.modal != Modal::None {
            self.last_modal_from.unwrap_or(regions.len())
        } else {
            0
        };
        regions[from.min(regions.len())..]
            .iter()
            .rev()
            .find(|r| crate::hits::contains(r.rect, col, row))
            .cloned()
    }

    /// Status bar hint for whatever is under the mouse.
    pub fn hover_hint(&self) -> Option<String> {
        let (c, r) = self.mouse_pos?;
        self.region_at(c, r)
            .map(|r| r.hint)
            .filter(|h| !h.is_empty())
    }

    pub fn on_mouse_event(&mut self, m: MouseEvent) {
        self.mouse_pos = Some((m.column, m.row));
        // Dragging a pane border.
        if let Some(b) = self.dragging.clone() {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.drag_border(&b, m.column, m.row);
                    return;
                }
                MouseEventKind::Up(_) => {
                    self.dragging = None;
                    return;
                }
                _ => self.dragging = None,
            }
        }
        // Dragging a pane header: drop on another pane to swap.
        if let Some((from, _)) = self.pane_drag {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.pane_drag = Some((from, true));
                    return;
                }
                MouseEventKind::Up(_) => {
                    self.pane_drag = None;
                    let to = self
                        .pane_rects
                        .iter()
                        .position(|r| crate::hits::contains(*r, m.column, m.row));
                    if let Some(to) = to.filter(|&t| t != from && !self.panes[t].hidden) {
                        self.swap_panes(from, to);
                        self.flash(format!("Swapped panes {} and {}", from + 1, to + 1));
                    }
                    return;
                }
                _ => self.pane_drag = None,
            }
        }
        // Dragging a tab: onto a group header it joins the group; onto
        // another tab it moves there.
        if let Some((s, t, moved)) = self.tab_drag {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.tab_drag = Some((s, t, true));
                    return;
                }
                MouseEventKind::Up(_) => {
                    self.tab_drag = None;
                    if moved {
                        let on = self.region_at(m.column, m.row).map(|r| r.action);
                        self.drop_tab(s, t, on);
                        return;
                    }
                }
                _ => self.tab_drag = None,
            }
        }
        // Dragging the edge of a tab list.
        if let Some(i) = self.sidebar_drag {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.drag_sidebar(i, m.column);
                    return;
                }
                MouseEventKind::Up(_) => {
                    self.sidebar_drag = None;
                    return;
                }
                _ => self.sidebar_drag = None,
            }
        }
        if self.livemap_mouse(&m) {
            return;
        }
        let region = self.region_at(m.column, m.row);
        if m.kind == MouseEventKind::Moved && matches!(self.modal, Modal::Menu(_)) {
            self.on_menu_hover(m.column, m.row);
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
                if region.is_none() && self.is_popup() =>
            {
                // Outside an open menu or popover: another menu title
                // switches to it; anywhere else closes it, and the click
                // stops there (as on macOS).
                let title = self
                    .last_hits
                    .regions
                    .iter()
                    .find(|r| {
                        crate::hits::contains(r.rect, m.column, m.row)
                            && matches!(r.action, UiAction::MenuOpen(_))
                    })
                    .map(|r| r.action.clone());
                match title {
                    Some(UiAction::MenuOpen(id)) if matches!(self.modal, Modal::Menu(ref o) if o.id != id) => {
                        self.open_menu(id)
                    }
                    _ => self.modal = Modal::None,
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(region) = region else {
                    // A click outside a popup closes the simple ones.
                    if matches!(
                        self.modal,
                        Modal::Help
                            | Modal::Palette(_)
                            | Modal::Approvals(_)
                            | Modal::AccountMenu(..)
                    ) {
                        self.modal = Modal::None;
                    }
                    return;
                };
                let double = self
                    .last_click
                    .as_ref()
                    .is_some_and(|(t, a)| *a == region.action && t.elapsed() < DOUBLE_CLICK);
                self.last_click = Some((Instant::now(), region.action.clone()));
                self.click(region.action, double, &m);
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Right click on a tab: its menu.
                match region.map(|r| r.action) {
                    Some(UiAction::SelectTab(s, t)) => self.open_tab_menu(s, t),
                    Some(UiAction::GroupHeader(s, g)) => self.open_group_menu(s, g),
                    Some(UiAction::Row(List::Overview, i)) => {
                        if let Some(&(s, t)) = self.overview_rows().get(i) {
                            self.open_tab_menu(s, t);
                        }
                    }
                    Some(UiAction::PaneBody(i)) => {
                        self.forward_mouse(i, &m);
                    }
                    Some(UiAction::PathClick(i)) => self.modal = Modal::PathMenu(i),
                    _ => {}
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                match region.map(|r| r.action) {
                    Some(UiAction::PaneBody(i)) | Some(UiAction::FocusPane(i)) => {
                        if !self.forward_mouse(i, &m) {
                            self.panes[i].cur_mut().scroll_by(if up { 3 } else { -3 });
                        }
                    }
                    Some(UiAction::Row(list, _)) => self.scroll_list(list, if up { -1 } else { 1 }),
                    Some(UiAction::SelectTab(s, _))
                    | Some(UiAction::SidebarToggle(s))
                    | Some(UiAction::TabsScroll(s, _)) => {
                        self.scroll_tabs(s, if up { -1 } else { 1 })
                    }
                    _ => self.scroll_view(if up { -1 } else { 1 }),
                }
            }
            MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {
                if let Some(UiAction::PaneBody(i)) = region.map(|r| r.action) {
                    self.forward_mouse(i, &m);
                }
            }
            _ => {}
        }
    }

    /// Pass a mouse event to the claude in pane `i` if it asked for mouse
    /// reporting. Returns true when forwarded.
    fn forward_mouse(&mut self, i: usize, m: &MouseEvent) -> bool {
        let Some(term) = self.term_rects.get(i).copied() else {
            return false;
        };
        if !crate::hits::contains(term, m.column, m.row) || self.view != View::Grid {
            return false;
        }
        let t = self.panes[i].cur_mut();
        let mode = t
            .parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .mouse_protocol_mode();
        if mode == vt100::MouseProtocolMode::None || !t.is_running() {
            return false;
        }
        if let Some(b) = encode_mouse(m, m.column - term.x, m.row - term.y) {
            t.write(&b);
            return true;
        }
        false
    }

    fn scroll_view(&mut self, d: isize) {
        match self.view {
            View::Settings => self.scroll_list(List::Settings, d),
            View::Sessions => self.move_session(d),
            View::Dashboard => self.move_account(d),
            View::Overview => self.move_overview(d),
            View::Learned => {
                self.learned_ui.sel = (self.learned_ui.sel as isize + d).max(0) as usize
            }
            View::Prompt => self.prompt_ui.sel = (self.prompt_ui.sel as isize + d).max(0) as usize,
            View::TabHistory => {
                let n = self.th.rows().len();
                self.th.sel =
                    (self.th.sel as isize + d).clamp(0, n.saturating_sub(1) as isize) as usize;
            }
            View::Loops => {
                self.sel_loop = (self.sel_loop as isize + d)
                    .clamp(0, self.loops.len().saturating_sub(1) as isize)
                    as usize
            }
            View::Grid | View::LiveMap => {}
        }
    }

    fn scroll_list(&mut self, list: List, d: isize) {
        match list {
            List::Overview => self.move_overview(d),
            List::Loops => {
                self.sel_loop = (self.sel_loop as isize + d)
                    .clamp(0, self.loops.len().saturating_sub(1) as isize)
                    as usize
            }
            List::Sessions => self.move_session(d),
            List::Dashboard => self.move_account(d),
            List::Approvals => {
                if let Modal::Approvals(s) = &mut self.modal {
                    *s = (*s as isize + d).max(0) as usize;
                }
            }
            List::Picker => {
                if let Modal::NewTab(pk) = &mut self.modal {
                    pk.move_sel(d);
                }
            }
            List::Palette => {
                if let Modal::Palette(pl) = &mut self.modal {
                    pl.sel = (pl.sel as isize + d).max(0) as usize;
                }
            }
            List::SettingsSection => {
                self.settings_section = (self.settings_section as isize + d)
                    .rem_euclid(crate::settings::SECTIONS.len() as isize)
                    as usize;
                self.settings_sel = 0;
            }
            List::Settings => {
                let n = self.settings_rows().len();
                self.settings_sel =
                    ((self.settings_sel as isize + d).max(0) as usize).min(n.saturating_sub(1));
            }
            List::AccountMenu => {
                if let Modal::AccountMenu(_, s) = &mut self.modal {
                    *s = (*s as isize + d).max(0) as usize;
                }
            }
        }
    }

    fn key_cmd(&mut self, c: char) {
        self.on_command(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    /// Buttons and fields of the New Tab dialog.
    pub fn picker_click(&mut self, p: crate::hits::PickerUi) {
        use crate::hits::PickerUi;
        use crate::picker::Mode;
        if let PickerUi::RemoveRecent(i) = p {
            let removed = match &mut self.modal {
                Modal::NewTab(pk) => pk.remove_recent(i).map(|path| {
                    (
                        path,
                        self.cfg
                            .accounts
                            .get(pk.account)
                            .map(|a| a.name.clone())
                            .unwrap_or_default(),
                    )
                }),
                _ => None,
            };
            if let Some((path, acct)) = removed {
                let ps = path.to_string_lossy().into_owned();
                self.recents
                    .retain(|r| !(r.path == ps && r.account == acct));
                self.state_dirty = true;
                if let Modal::NewTab(pk) = &mut self.modal {
                    pk.sel = None;
                }
            }
            return;
        }
        if let PickerUi::AllAccounts = p {
            let (a, all) = match &self.modal {
                Modal::NewTab(pk) => (pk.account, !pk.all_accounts),
                _ => return,
            };
            let rows = self.recent_rows_for(a, all);
            if let Modal::NewTab(pk) = &mut self.modal {
                pk.all_accounts = all;
                pk.recents = rows;
                pk.sel = None;
            }
            return;
        }
        let Modal::NewTab(pk) = &mut self.modal else {
            return;
        };
        match p {
            PickerUi::NameField => {
                pk.sel = None;
                pk.mode = Mode::Name;
            }
            PickerUi::CreateFolder => pk.create_folder = !pk.create_folder,
            PickerUi::GitInit => pk.git_init = !pk.git_init,
            PickerUi::OpenExisting => {
                pk.mode = Mode::OpenExisting(crate::picker::tilde(&pk.base.to_string_lossy()) + "/")
            }
            PickerUi::EditBase => {
                pk.mode = Mode::Base(crate::picker::tilde(&pk.base.to_string_lossy()))
            }
            PickerUi::Create => self.new_tab_enter(),
            PickerUi::RemoveRecent(_) | PickerUi::AllAccounts => {}
        }
    }

    /// Scroll a pane's tab list.
    pub fn scroll_tabs(&mut self, s: usize, d: isize) {
        if let Some(slot) = self.panes.get_mut(s) {
            let n = slot.tabs.len().saturating_sub(1) as isize;
            let base = if slot.sidebar_scroll == crate::slot::FOLLOW {
                slot.active
            } else {
                slot.sidebar_scroll
            };
            slot.sidebar_scroll = (base as isize + d).clamp(0, n) as usize;
        }
    }

    /// Entries of a pane's account menu: (label, action).
    pub fn account_menu_entries(&self, slot: usize) -> Vec<(String, UiAction)> {
        let mut v = vec![];
        for (i, a) in self.cfg.accounts.iter().enumerate() {
            let here = self.panes[slot].account == Some(i);
            let state = if self.accounts[i].login.logged_in() {
                ""
            } else {
                "  (not logged in)"
            };
            v.push((
                format!("{} {}{state}", if here { "●" } else { " " }, a.display()),
                UiAction::SwitchAccount(slot, i),
            ));
        }
        if let Some(a) = self.panes[slot].account {
            let cur = self.cfg.mode_for(a);
            for m in crate::config::PERMISSION_MODES {
                let mark = if crate::config::permission_badge(&cur)
                    == crate::config::permission_badge(m)
                {
                    "●"
                } else {
                    " "
                };
                v.push((
                    format!("{mark} Permissions: {m}"),
                    UiAction::SetPerm(slot, m),
                ));
            }
        }
        if self.panes[slot].account.is_some() {
            v.push((
                "  Split pane (another pane for this account)".into(),
                UiAction::SplitPane(slot),
            ));
            v.push((
                "  Hide pane (keeps running in the background)".into(),
                UiAction::HidePane(slot),
            ));
            let acct = self.panes[slot].account;
            if self.panes.iter().filter(|s| s.account == acct).count() > 1 {
                v.push(("  Close this pane".into(), UiAction::ClosePane(slot)));
            }
        }
        // Move the pane: neighbours, or straight to a slot.
        let vis = self.visible_panes();
        if vis.len() > 1 && !self.panes[slot].hidden {
            for (l, c) in [("←", 0u8), ("→", 1), ("↑", 2), ("↓", 3)] {
                v.push((
                    format!("  Move pane {l} (C-a Shift+arrow)"),
                    UiAction::MovePane(slot, c),
                ));
            }
            let me = self.slot_number(slot);
            for n in 1..=vis.len().min(9) {
                if n != me {
                    v.push((
                        format!("  Move pane to slot {n}"),
                        UiAction::MovePane(slot, 10 + n as u8),
                    ));
                }
            }
        }
        v.push(("  Close window…".into(), UiAction::CloseWindow(slot)));
        if self.hidden_count() > 0 {
            v.push((
                format!("  Show {} hidden pane(s)", self.hidden_count()),
                UiAction::ShowHidden,
            ));
        }
        if let Some(a) = self.panes[slot].account {
            v.push((
                format!(
                    "{} Show email",
                    if self.show_email_for(a) { "●" } else { " " }
                ),
                UiAction::Key('e'),
            ));
            v.push((
                format!(
                    "{} Privacy mode (hide all emails)",
                    if self.privacy() { "●" } else { " " }
                ),
                UiAction::Key('E'),
            ));
        }
        if let Some(a) = self.panes[slot].account {
            let on = self.cfg.trust_for(a);
            v.push((
                format!("{} Auto trust folders", if on { "●" } else { " " }),
                UiAction::ToggleTrust(slot),
            ));
        }
        let pos = self.tab_pos(slot);
        for p in [
            crate::slot::TabPos::Left,
            crate::slot::TabPos::Top,
            crate::slot::TabPos::Right,
        ] {
            let mark = if p == pos { "●" } else { " " };
            v.push((
                format!("{mark} Tabs: {}", p.label()),
                UiAction::TabPos(slot, Some(p)),
            ));
        }
        if self.panes[slot].tab_pos.is_some() {
            v.push((
                "  Tabs: use the default".into(),
                UiAction::TabPos(slot, None),
            ));
        }
        if let Some(a) = self.panes[slot].account {
            v.push((
                "  Color…".into(),
                UiAction::OpenColor(crate::color_pick::Target::Account(a)),
            ));
            v.push(("Log in this account".into(), UiAction::Login(slot)));
            if self.accounts[a].login.logged_in() {
                v.push(("Log out of this account".into(), UiAction::Logout(a)));
            }
        }
        v.push(("Add account…".into(), UiAction::Key('A')));
        v
    }

    /// Run a clicked action.
    pub fn click(&mut self, action: UiAction, double: bool, m: &MouseEvent) {
        match action {
            // The button of the view on screen toggles back home.
            UiAction::Key(c) if self.active_menu_key() == Some(c) => self.go_home(),
            UiAction::Key(c) => {
                // Buttons in popups (help, account menu) close them first.
                if matches!(self.modal, Modal::Help | Modal::AccountMenu(..)) {
                    self.modal = Modal::None;
                }
                self.key_cmd(c)
            }
            UiAction::FocusPane(i) => {
                self.focus = i;
                self.view = View::Grid;
                // A click in a pane gives it the keys.
                self.assistant.focused = false;
            }
            UiAction::AssistantFocus => self.assistant.focused = true,
            UiAction::PaneHeader(i) => {
                self.focus = i;
                self.view = View::Grid;
                self.pane_drag = Some((i, false));
            }
            UiAction::MovePane(p, code) => {
                self.modal = Modal::None;
                let r = match code {
                    0 => self.move_pane(p, -1, 0),
                    1 => self.move_pane(p, 1, 0),
                    2 => self.move_pane(p, 0, -1),
                    3 => self.move_pane(p, 0, 1),
                    c => self.move_pane_to_slot(p, (c - 10) as usize),
                };
                match r {
                    Ok(to) => {
                        self.flash(format!("Moved the pane to slot {}", self.slot_number(to)))
                    }
                    Err(e) => self.flash(format!("Can't move it: {e}")),
                }
            }
            UiAction::PaneBody(i) => {
                self.focus = i;
                self.forward_mouse(i, m);
            }
            UiAction::SelectTab(s, t) => {
                self.tab_drag = Some((s, t, false));
                self.jump_to(s, t);
                if double {
                    self.open_move_picker(s, t, false);
                }
            }
            UiAction::MoveRow(i) => {
                if let Modal::MoveTab(p) = &mut self.modal {
                    p.sel = i;
                }
                if double {
                    self.confirm_move_picker();
                }
            }
            UiAction::MoveCopy(c) => {
                if let Modal::MoveTab(p) = &mut self.modal {
                    p.copy = c;
                }
            }
            UiAction::MoveFolder => {
                if let Modal::MoveTab(p) = &mut self.modal {
                    p.same_folder = !p.same_folder;
                }
            }
            UiAction::MoveGo => self.confirm_move_picker(),
            UiAction::MoveBusy(i) => self.move_busy_choice(i),
            UiAction::TabMenuItem(i) => {
                if let Modal::TabMenu(s, t, _) = self.modal {
                    self.tab_menu_run(s, t, i);
                }
            }
            UiAction::GroupHeader(s, g) => {
                self.focus = s;
                self.toggle_group(s, g);
            }
            UiAction::GroupMenuItem(i) => {
                if let Modal::GroupMenu(s, g, _) = self.modal {
                    self.group_menu_run(s, g, i);
                }
            }
            UiAction::GroupPickItem(i) => {
                if let Modal::GroupPick(s, t, _) = self.modal {
                    self.group_pick_run(s, t, i);
                }
            }
            UiAction::SettingsGroup(g) => self.toggle_settings_group(g),
            UiAction::GroupMoveTo(i) => {
                if let Modal::GroupMove(s, g, _) = self.modal {
                    self.move_group_to(s, g, i);
                }
            }
            UiAction::GrokTest => {
                self.flash("Testing the Grok voice login...");
                crate::voice::grok_tts::check_in_background(self.cfg.voice.grok.clone());
            }
            UiAction::OpenGrokLogins => self.open_grok_logins(),
            UiAction::GrokLoginAct(i, act) => self.grok_login_act(i, act),
            UiAction::RetryDevices => {
                crate::settings::start_audio_lists(&self.cfg.voice, true);
            }
            UiAction::SttTest => {
                self.flash("Recording 3 s for Grok recognition: say something");
                crate::voice::grok_stt::test_recognition(self.cfg.voice.clone());
            }
            UiAction::GrokRemoveKey => match crate::voice::grok_tts::remove_key() {
                Ok(()) => self.flash("xAI API key removed"),
                Err(e) => self.flash(format!("Could not remove it: {e}")),
            },
            UiAction::SortTabs(by) => {
                let s = self.focus;
                self.modal = Modal::None;
                self.set_tab_sort(s, by);
            }
            UiAction::UndoTabMove => self.undo_tab_move(),
            UiAction::LoopJump => self.jump_to_loop(),
            UiAction::FreeNow => self.free_now(),
            UiAction::AssistantNew => self.reset_assistant(),
            UiAction::AssistantSend => self.send_assistant_input(),
            UiAction::SetupInstall(id) => self.setup_click(&id),
            UiAction::SetupWhisper(m) => {
                self.setup.whisper_choice = Some(m);
                self.deps_changed();
            }
            UiAction::SetupDontShow => {
                self.cfg.setup_dont_show = !self.cfg.setup_dont_show;
                let _ = crate::settings::write(
                    &crate::config::Config::path(),
                    &crate::settings::Key::Global("setup_dont_show"),
                    Some(toml_edit::value(self.cfg.setup_dont_show)),
                );
                self.config_mtime = crate::app::config_mtime();
            }
            UiAction::AssistantChip(i) => {
                if !self.assistant.expanded.remove(&i) {
                    self.assistant.expanded.insert(i);
                }
            }
            UiAction::AssistantHistory => self.toggle_assistant_history(),
            UiAction::MuteToggle => self.toggle_mute(),
            UiAction::SpeakerToggle => self.toggle_speaker(),
            UiAction::FailoverMove => {
                self.accept_failover(None, false);
            }
            UiAction::FailoverDismiss => self.decline_failover(),
            UiAction::SessHarness(k) => {
                self.sess_harness = if self.sess_harness == Some(k) {
                    None
                } else {
                    Some(k)
                };
                self.sel_session = 0;
            }
            UiAction::TakeOverDo(k) => {
                if let Modal::TakeOver(id) = self.modal.clone() {
                    self.modal = Modal::None;
                    self.bring_here(&id, k);
                }
            }
            UiAction::ModalCancel if matches!(self.modal, Modal::TakeOver(_)) => {
                self.modal = Modal::None
            }
            UiAction::SessHeadless => {
                self.sess_headless = !self.sess_headless;
                self.sel_session = 0;
                self.state_dirty = true;
            }
            UiAction::SessSort(i) => self.sort_sessions_by(
                crate::sess_sort::KEYS[(i as usize).min(crate::sess_sort::KEYS.len() - 1)],
            ),
            UiAction::SessSubagents => {
                self.sess_subagents = !self.sess_subagents;
                self.sess_sub_rows.clear();
                if self.sess_subagents {
                    self.kick_index(true);
                    let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
                    self.sess_sub_rows = ix
                        .rows
                        .iter()
                        .filter(|r| r.info.parent.is_some())
                        .map(|r| (r.src, r.info.clone()))
                        .collect();
                }
            }
            UiAction::MenuOpen(id) => {
                if matches!(&self.modal, Modal::Menu(o) if o.id == id) {
                    self.modal = Modal::None;
                } else {
                    self.open_menu(id);
                }
            }
            UiAction::MenuRow(id, i) => self.menu_click(id, i),
            UiAction::Menu(c) => {
                self.modal = Modal::None;
                self.menu_cmd(c);
            }
            UiAction::AddAcct(a) => self.on_add_account_click(a),
            UiAction::ColorPick(usize::MAX) => {}
            UiAction::ColorPick(i) => {
                if let Modal::ColorPick(t, _) = self.modal.clone() {
                    self.apply_color(t, i);
                }
            }
            UiAction::OpenColor(t) => self.open_color_pick(t),
            UiAction::EmptySlotMenu(k) => {
                self.modal = if self.modal == Modal::EmptySlotMenu(k) {
                    Modal::None
                } else {
                    Modal::EmptySlotMenu(k)
                }
            }
            UiAction::NewPaneFor(a) => {
                self.modal = Modal::None;
                self.new_pane_for(a);
            }
            UiAction::SidebarEdge(i) if double => {
                if let Some(p) = self.panes.get_mut(i) {
                    p.sidebar_w = None;
                    self.state_dirty = true;
                }
            }
            UiAction::SidebarEdge(i) => self.sidebar_drag = Some(i),
            UiAction::PathClick(i) => {
                if double {
                    self.open_in_finder(i);
                } else {
                    self.copy_path(i);
                }
            }
            UiAction::PathMenuItem(i, k) => {
                self.modal = Modal::None;
                match k {
                    0 => self.open_in_finder(i),
                    1 => self.copy_path(i),
                    _ => self.terminal_here(i),
                }
            }
            UiAction::HistRow(i) => {
                if let Some(h) = self.assistant.history.as_mut() {
                    h.sel = i;
                    h.open = h.shown().get(i).map(|s| s.id.clone());
                    h.confirm_delete = false;
                }
            }
            UiAction::HistResume => {
                if let Some(id) = self.assistant.history.as_ref().and_then(|h| h.open.clone()) {
                    self.resume_conversation(&id);
                }
            }
            UiAction::HistDelete => {
                let open = self
                    .assistant
                    .history
                    .as_ref()
                    .and_then(|h| h.open.clone().map(|o| (o, h.confirm_delete)));
                match open {
                    Some((id, true)) => self.delete_conversation(&id),
                    Some(_) => {
                        if let Some(h) = self.assistant.history.as_mut() {
                            h.confirm_delete = true;
                        }
                    }
                    None => {}
                }
            }
            UiAction::HistBack => {
                if let Some(h) = self.assistant.history.as_mut() {
                    h.open = None;
                    h.confirm_delete = false;
                }
            }
            UiAction::RenameTab(s, t) => {
                self.modal = Modal::None;
                self.start_rename(s, t);
            }
            UiAction::MoveName => {
                if let Modal::MoveTab(p) = &mut self.modal {
                    p.name_edit = true;
                }
            }
            UiAction::MoveRenameOnly => {
                if let Modal::MoveTab(p) = self.modal.clone() {
                    self.apply_picker_name(&p);
                    self.modal = Modal::None;
                }
            }
            UiAction::LoopStop => self.stop_selected_loop(),
            UiAction::LoopStopAll => self.modal = Modal::ConfirmStopLoops,
            UiAction::SuggestMove(s, a) => {
                let t = self.panes[s].active;
                self.request_move(s, t, a, false, true);
            }
            UiAction::SidebarToggle(s) => self.toggle_sidebar(s),
            UiAction::Picker(p) => self.picker_click(p),
            UiAction::SettingStep(i, d) => self.setting_step(i, d),
            UiAction::SettingReset(i) => {
                self.settings_sel = i;
                self.setting_reset(i);
            }
            UiAction::SettingEdit(i) => self.setting_edit(i),
            UiAction::OpenConfig => self.open_config_in_editor(),
            UiAction::RunDoctor => self.run_doctor_tab(),
            UiAction::UpdateCheck => self.check_for_update(),
            UiAction::UpdateSkip => self.skip_update(),
            UiAction::Install(dock) => {
                let exe = crate::install::real_exe()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "godterm".into());
                let mut args = vec!["install".to_string()];
                if dock {
                    args.push("--dock".into());
                }
                let p = self.focus;
                self.run_in_new_tab(p, &exe, args);
                self.view = crate::app::View::Grid;
            }
            UiAction::LoginAccount(a) => {
                self.login_for_account(a);
            }
            UiAction::RemoveAccount(a) => self.modal = Modal::ConfirmRemoveAccount(a),
            UiAction::ToggleTrust(s) => {
                self.modal = Modal::None;
                self.toggle_trust(s);
            }
            UiAction::SetPerm(s, mode) => {
                self.modal = Modal::None;
                if let Some(a) = self.panes[s].account {
                    self.set_permission_mode(s, a, mode, false);
                }
            }
            UiAction::TabsScroll(s, d) => self.scroll_tabs(s, d),
            UiAction::TabPos(s, p) => {
                self.modal = Modal::None;
                self.set_tab_pos(s, p);
            }
            UiAction::CloseTab(s, t) => self.request_close(s, t),
            UiAction::CloseWindow(s) => {
                self.modal = Modal::None;
                self.request_close_window(s);
            }
            UiAction::ResumeListening => self.resume_listening("clicked"),
            UiAction::LearnedKey(c) => self.on_learned_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            )),
            UiAction::LearnedRow(i) => self.learned_ui.sel = i,
            UiAction::LearnedToggle(i) => self.learned_toggle(i),
            UiAction::OpenLearned => {
                self.view = crate::app::View::Grid;
                self.open_learned_view();
            }
            UiAction::PromptKey(c) => self.on_prompt_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            )),
            UiAction::PromptRow(i) => self.prompt_ui.sel = i,
            UiAction::OpenPrompt => {
                self.view = crate::app::View::Grid;
                self.open_prompt_view();
            }
            UiAction::ThKey(c) => self.on_tab_history_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            )),
            UiAction::ThRow(i) => {
                self.th.sel = i;
                if double {
                    self.th_action(0);
                }
            }
            UiAction::ReopenClosed => {
                if let Err(e) = self.reopen_closed() {
                    self.flash(format!("Nothing to reopen: {e}"));
                }
            }
            UiAction::NewTab(s) => self.new_tab_prompt(s),
            UiAction::Zoom(s) => {
                self.focus = s;
                self.zoom = !self.zoom;
            }
            UiAction::Restart(s) => {
                self.focus = s;
                let k = self.panes[s].cur().relaunch_kind();
                self.launch(s, k);
            }
            UiAction::AccountMenu(s) => {
                self.focus = s;
                self.modal = Modal::AccountMenu(s, 0);
            }
            UiAction::UsageOf(a) => {
                self.sel_account = a;
                self.view = View::Dashboard;
            }
            UiAction::LoginPane(s) => {
                self.focus = s;
                if let Some(a) = self.panes[s].account {
                    self.login_for_account(a);
                }
            }
            UiAction::Answer(s, t, c) => {
                let msg = self.answer_tab(s, t, c);
                self.flash(msg);
            }
            UiAction::Jump(s, t) => {
                self.modal = Modal::None;
                self.jump_to(s, t);
            }
            UiAction::Row(list, i) => self.click_row(list, i, double),
            // off -> push to talk -> wake word -> open mic -> off
            UiAction::VoiceCycle => {
                if self.voice.engine.is_none() {
                    self.start_voice(false);
                    self.flash("Voice: push to talk (Mic button or Ctrl-a space)");
                } else if !self.voice.always_on {
                    self.toggle_wake_mode();
                } else if !self.voice.open_mic {
                    self.start_open_mic();
                } else {
                    self.leave_open_mic(false);
                    self.voice.info = None;
                }
            }
            UiAction::VoiceMenu => {
                // From the status bar chip: hang the menu from it.
                let at = self.mouse_pos;
                self.modal = Modal::Menu(crate::menus::Open::new(crate::menus::MenuId::Voice, at));
            }
            UiAction::VoiceMode(m) => {
                self.modal = Modal::None;
                self.set_voice_mode(m);
            }
            UiAction::MenuOverflow => self.modal = Modal::MenuOverflow,
            UiAction::OverflowItem(i) => {
                self.modal = Modal::None;
                let item = self.menu_overflow.borrow().get(i).map(|x| x.1.clone());
                if let Some(a) = item {
                    let m = MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: 0,
                        row: 0,
                        modifiers: crossterm::event::KeyModifiers::NONE,
                    };
                    self.click(a, false, &m);
                }
            }
            UiAction::Mic => self.push_to_talk(),
            UiAction::VoicePreview => self.preview_voice(),
            UiAction::TrainWake => self.start_wake_training(),
            UiAction::TrainVoice => self.start_voice_training(),
            UiAction::MicModes => self.open_mic_modes(),
            UiAction::SplitPane(p) => {
                self.modal = Modal::None;
                self.split_pane(p)
            }
            UiAction::HidePane(p) => {
                self.modal = Modal::None;
                self.hide_pane(p)
            }
            UiAction::ClosePane(p) => {
                self.modal = Modal::None;
                self.close_pane(p)
            }
            UiAction::ShowHidden => self.show_hidden(),
            UiAction::Home => self.go_home(),
            UiAction::SessSource(i) => {
                if let Some(c) = self.source_chips().get(i).copied() {
                    self.set_source(c);
                }
            }
            UiAction::SessKey(c) => {
                let _ = self.on_sessions_key2(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            UiAction::SessTarget(i) => self.choose_target(i),
            UiAction::SessConflict(c) => self.set_conflict(
                [
                    crate::session_ops::Conflict::Skip,
                    crate::session_ops::Conflict::Overwrite,
                    crate::session_ops::Conflict::NewId,
                ][c.min(2) as usize],
            ),
            UiAction::SessConfirm => self.confirm_op(),
            UiAction::Border => {
                if double {
                    self.reset_ratios();
                    self.flash("Panes back to equal sizes");
                } else {
                    self.dragging = self.border_at(m.column, m.row);
                }
            }
            UiAction::Train(t) => match t {
                crate::hits::TrainUi::Start | crate::hits::TrainUi::Retrain => self.train_begin(),
                crate::hits::TrainUi::Skip => self.train_skip(),
                crate::hits::TrainUi::Save => self.train_save(),
                crate::hits::TrainUi::Reset => self.train_reset(),
            },
            UiAction::ModalOk => self.modal_ok(),
            UiAction::ModalCancel if self.modal == Modal::WakeTrain => self.train_close(),
            UiAction::ModalCancel if matches!(self.modal, Modal::MoveTab(_)) => {
                self.cancel_move_picker()
            }
            UiAction::ModalCancel
                if matches!(
                    self.modal,
                    Modal::SessTarget(_) | Modal::SessConfirm | Modal::SessConflict(_)
                ) =>
            {
                self.cancel_op()
            }
            UiAction::ModalCancel => self.modal = Modal::None,
            UiAction::TourNext => {
                if let Modal::Tour(n) = self.modal {
                    if n + 1 < TOUR.len() {
                        self.modal = Modal::Tour(n + 1);
                    } else {
                        self.finish_tour();
                    }
                }
            }
            UiAction::TourSkip => self.finish_tour(),
            UiAction::TourStart => {
                self.view = View::Grid;
                self.modal = Modal::Tour(0);
            }
            UiAction::SwitchAccount(s, a) => {
                self.modal = Modal::None;
                self.assign(s, a);
                self.focus = s;
                self.view = View::Grid;
            }
            UiAction::Login(s) => {
                self.modal = Modal::None;
                if let Some(a) = self.panes[s].account {
                    self.login_for_account(a);
                }
            }
            UiAction::Logout(a) => {
                self.modal = Modal::None;
                self.logout_account(a);
            }
        }
    }

    fn click_row(&mut self, list: List, i: usize, double: bool) {
        match list {
            List::SettingsSection => {
                self.settings_section = i;
                self.settings_sel = 0;
            }
            List::Settings => {
                self.settings_sel = i;
                if double {
                    self.setting_edit(i);
                }
            }
            List::Overview => {
                self.sel_overview = i;
                // Double click: move the tab to another account (Enter
                // still jumps to it).
                if double {
                    if let Some(&(s, t)) = self.overview_rows().get(i) {
                        self.open_move_picker(s, t, false);
                    }
                }
            }
            List::Loops => {
                self.sel_loop = i;
                if double {
                    self.jump_to_loop();
                }
            }
            List::Sessions => {
                self.sel_session = i;
                if double {
                    self.open_selected_session();
                }
            }
            List::Dashboard => {
                self.sel_account = i;
                if double {
                    let p = self.pane_for_account(i);
                    self.assign(p, i);
                    self.focus = p;
                    self.view = View::Grid;
                }
            }
            List::Approvals => {
                self.modal = Modal::Approvals(i);
                if double {
                    if let Some(&(s, t)) = self.waiting_tabs().get(i) {
                        self.modal = Modal::None;
                        self.jump_to(s, t);
                    }
                }
            }
            List::Picker => {
                if let Modal::NewTab(pk) = &mut self.modal {
                    pk.sel = Some(i);
                    pk.mode = crate::picker::Mode::Name;
                    if double {
                        let (slot, choice) = (pk.slot, pk.chosen());
                        self.modal = Modal::None;
                        if let Some(c) = choice {
                            self.open_tab(slot, c);
                        }
                    }
                }
            }
            List::Palette => {
                if let Modal::Palette(pl) = &mut self.modal {
                    pl.sel = i;
                    if double {
                        let pl = pl.clone();
                        self.modal = Modal::None;
                        self.run_palette(pl);
                    }
                }
            }
            List::AccountMenu => {
                if let Modal::AccountMenu(slot, _) = self.modal {
                    self.modal = Modal::AccountMenu(slot, i);
                    // Menus act on a single click.
                    if let Some((_, a)) = self.account_menu_entries(slot).get(i).cloned() {
                        let m = MouseEvent {
                            kind: MouseEventKind::Down(MouseButton::Left),
                            column: 0,
                            row: 0,
                            modifiers: KeyModifiers::NONE,
                        };
                        self.modal = Modal::None;
                        self.click(a, false, &m);
                    }
                }
            }
        }
    }

    /// [OK] in a dialog: the same as Enter.
    pub fn modal_ok(&mut self) {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        match &self.modal {
            Modal::None => {}
            Modal::Help | Modal::Tour(_) => {
                if matches!(self.modal, Modal::Tour(_)) {
                    self.finish_tour();
                } else {
                    self.modal = Modal::None;
                }
            }
            Modal::ConfirmQuit => self.quit = true,
            Modal::ConfirmStopLoops => {
                self.modal = Modal::None;
                self.stop_all_loops();
            }
            Modal::ConfirmClose => {
                self.modal = Modal::None;
                self.close_tab_now();
            }
            Modal::ConfirmCloseWindow(s) => {
                let s = *s;
                self.modal = Modal::None;
                self.close_window(s);
            }
            Modal::ConfirmBypass(..) | Modal::OfferRestart(_) | Modal::ConfirmRemoveAccount(_) => {
                self.on_key(enter)
            }
            Modal::AccountMenu(_, sel) => {
                let sel = *sel;
                self.click_row(List::AccountMenu, sel, false);
            }
            _ => self.on_key(enter),
        }
    }

    pub fn finish_tour(&mut self) {
        self.modal = Modal::None;
        let _ = std::fs::write(crate::config::app_home().join("tour_done"), "1");
    }

    /// Show the tour on the very first launch.
    pub fn maybe_start_tour(&mut self) {
        if !crate::config::app_home().join("tour_done").exists() && self.modal == Modal::None {
            self.modal = Modal::Tour(0);
        }
    }

    /// Log a slot's account out (`claude auth logout` in its config dir).
    pub fn logout_account(&mut self, a: usize) {
        let acfg = self.cfg.accounts[a].clone();
        let dir = acfg.config_dir();
        let h = acfg.harness();
        let bin = match h {
            crate::harness::Harness::Claude => self.cfg.claude_bin(),
            crate::harness::Harness::Grok => h.bin(self.cfg.grok_bin.as_deref()),
        };
        let label = acfg.display().to_string();
        for s in self.panes.iter_mut() {
            if s.account == Some(a) {
                s.kill_all();
            }
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let args: &[&str] = match h {
                crate::harness::Harness::Claude => &["auth", "logout"],
                crate::harness::Harness::Grok => &["logout"],
            };
            let _ = std::process::Command::new(bin)
                .args(args)
                .env(h.home_env(), &dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let snap = crate::app::snapshot_of(&acfg, false);
            let _ = tx.send(crate::app::AppEvent::Status(a, Box::new(snap)));
        });
        crate::log::info(&format!("logout {label}"));
        self.flash(format!("Logging out {label}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_encoding() {
        let m = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(encode_mouse(&m, 4, 2).unwrap(), b"\x1b[<0;5;3M");
        let up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            ..m
        };
        assert_eq!(encode_mouse(&up, 0, 0).unwrap(), b"\x1b[<0;1;1m");
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            modifiers: KeyModifiers::CONTROL,
            ..m
        };
        assert_eq!(encode_mouse(&wheel, 9, 9).unwrap(), b"\x1b[<81;10;10M");
    }
}
