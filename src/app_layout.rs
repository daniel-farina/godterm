//! Panes for any number of accounts: layout modes, paging, split and
//! hidden panes, focus by number, and dragging pane borders.

use crate::app::{App, View};
use crate::layout::{self, Arranged, Border, Mode};
use crate::slot::Slot;

impl App {
    /// Panes in the layout, in order (hidden ones left out).
    pub fn visible_panes(&self) -> Vec<usize> {
        (0..self.panes.len())
            .filter(|&i| {
                !self.panes[i].hidden
                    && !self.panes[i]
                        .account
                        .is_some_and(|a| self.account_closed(a))
            })
            .collect()
    }

    pub fn layout_name(&self) -> String {
        self.rt_layout
            .clone()
            .unwrap_or_else(|| self.cfg.layout.clone())
    }

    pub fn layout_mode(&self) -> Mode {
        layout::parse_mode(&self.layout_name(), &self.cfg.grid)
    }

    /// Arrange the visible panes in `area` around the focused one.
    pub fn arrange(&self, area: ratatui::layout::Rect) -> (Vec<usize>, Arranged) {
        let vis = self.visible_panes();
        let fi = vis.iter().position(|&p| p == self.focus).unwrap_or(0);
        if self.layout_name() == "custom" {
            if let Some(tree) = self.layout_tree() {
                let open: Vec<Option<usize>> = vis.iter().map(|&p| self.panes[p].account).collect();
                if let Some(rects) = crate::grid_layout::arrange(&tree, area, &open) {
                    let a = Arranged {
                        placed: rects
                            .into_iter()
                            .map(|(index, rect)| layout::Placed {
                                index,
                                page: 0,
                                rect,
                            })
                            .collect(),
                        pages: 1,
                        per_page: vis.len().max(1),
                        shape: "custom".into(),
                        borders: vec![],
                        empty: vec![],
                    };
                    return (vis, a);
                }
            }
        }
        let mode = if self.layout_name() == "custom" {
            Mode::Auto
        } else {
            self.layout_mode()
        };
        let a = layout::arrange(vis.len(), area, mode, fi, &self.ratios);
        (vis, a)
    }

    /// Ctrl-a L: the next layout mode.
    pub fn cycle_layout(&mut self) {
        let next = layout::next_mode(&self.layout_name());
        self.set_layout(next);
    }

    pub fn set_layout(&mut self, name: &str) {
        let name = if layout::MODES.contains(&name)
            || (name == "custom" && self.layout_tree().is_some())
        {
            name
        } else {
            "auto"
        };
        self.rt_layout = Some(name.to_string());
        self.state_dirty = true;
        self.view = View::Grid;
        let extra = if name == "grid" {
            format!(" {}", self.cfg.grid)
        } else {
            String::new()
        };
        self.flash(format!("Layout: {name}{extra} (Ctrl-a L for the next one)"));
    }

    /// How many pages the panes take.
    pub fn page_count(&self) -> usize {
        self.last_arranged.as_ref().map(|a| a.pages).unwrap_or(1)
    }

    /// Ctrl-a ] / [: focus the first pane of the next / previous page.
    pub fn page_by(&mut self, d: isize) {
        let Some(a) = self.last_arranged.clone() else {
            return;
        };
        if a.pages <= 1 {
            self.flash("Everything fits on one page");
            return;
        }
        let vis = self.visible_panes();
        let fi = vis.iter().position(|&p| p == self.focus).unwrap_or(0);
        let page = (fi / a.per_page) as isize;
        let next = (page + d).rem_euclid(a.pages as isize) as usize;
        if let Some(&p) = vis.get(next * a.per_page) {
            self.focus = p;
            self.view = View::Grid;
            self.flash(format!("Page {}/{}", next + 1, a.pages));
        }
    }

    /// Focus pane number `n` (1 based, in layout order; hidden panes come
    /// after the visible ones and are shown again).
    pub fn focus_number(&mut self, n: usize) {
        let mut order = self.visible_panes();
        order.extend((0..self.panes.len()).filter(|&i| self.panes[i].hidden));
        match n.checked_sub(1).and_then(|i| order.get(i)) {
            Some(&p) => {
                if self.panes[p].hidden {
                    self.panes[p].hidden = false;
                    self.state_dirty = true;
                }
                self.focus = p;
                self.view = View::Grid;
            }
            None => self.flash(format!("There is no pane {n} ({} panes)", order.len())),
        }
    }

    /// Every account has at least one pane (new accounts get one).
    pub fn ensure_panes(&mut self) {
        for a in 0..self.cfg.accounts.len() {
            if !self.panes.iter().any(|s| s.account == Some(a)) {
                let cwd = self.cfg.accounts[a].work_dir();
                self.panes.push(Slot::new(Some(a), cwd));
            }
        }
        if self.panes.is_empty() {
            self.panes.push(Slot::new(None, crate::config::home_dir()));
        }
        if self.focus >= self.panes.len() {
            self.focus = 0;
        }
    }

    /// A second pane for the same account, right after this one.
    pub fn split_pane(&mut self, p: usize) {
        let Some(a) = self.panes.get(p).and_then(|s| s.account) else {
            self.flash("Assign an account to this pane first");
            return;
        };
        let cwd = self.panes[p].cur().cwd.clone();
        let mut s = Slot::new(Some(a), cwd);
        s.tab_pos = self.panes[p].tab_pos;
        self.panes.insert(p + 1, s);
        self.focus = p + 1;
        self.view = View::Grid;
        self.state_dirty = true;
        self.flash(format!(
            "New pane for {} (close it from its account menu)",
            self.cfg.accounts[a].display()
        ));
    }

    /// Accounts by what is really left, most first (the empty cell's menu).
    pub fn accounts_by_left(&self) -> Vec<(usize, Option<f64>)> {
        let mut v: Vec<(usize, Option<f64>)> = (0..self.cfg.accounts.len())
            .map(|a| (a, self.accounts[a].effective_left()))
            .collect();
        v.sort_by(|x, y| {
            y.1.unwrap_or(-1.0)
                .partial_cmp(&x.1.unwrap_or(-1.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// A new pane for `a` (it fills the next free cell), focused, with the
    /// New tab dialog open to pick its folder.
    pub fn new_pane_for(&mut self, a: usize) {
        if a >= self.cfg.accounts.len() {
            return;
        }
        let mut s = Slot::new(Some(a), self.cfg.accounts[a].work_dir());
        s.tab_pos = None;
        self.panes.push(s);
        let p = self.panes.len() - 1;
        self.focus = p;
        self.view = View::Grid;
        self.state_dirty = true;
        self.new_tab_prompt(p);
    }

    /// Close a pane that has a sibling for the same account (or no account).
    pub fn close_pane(&mut self, p: usize) {
        let Some(slot) = self.panes.get(p) else {
            return;
        };
        let siblings = self
            .panes
            .iter()
            .filter(|s| s.account == slot.account)
            .count();
        if slot.account.is_some() && siblings < 2 {
            self.flash("The only pane of an account can be hidden, not closed");
            return;
        }
        let mut s = self.panes.remove(p);
        s.kill_all();
        self.focus = self.focus.min(self.panes.len() - 1);
        if self.focus > p {
            self.focus -= 1;
        }
        self.state_dirty = true;
        self.ensure_focus_visible();
        self.flash("Pane closed");
    }

    /// Hide a pane from the layout; its tabs keep running.
    pub fn hide_pane(&mut self, p: usize) {
        if self.visible_panes().len() <= 1 {
            self.flash("The last visible pane cannot be hidden");
            return;
        }
        if let Some(s) = self.panes.get_mut(p) {
            s.hidden = true;
        }
        self.state_dirty = true;
        self.ensure_focus_visible();
        let n = self.panes.iter().filter(|s| s.hidden).count();
        self.flash(format!(
            "Pane hidden, still running ({n} hidden: Show hidden in the menu bar)"
        ));
    }

    pub fn show_hidden(&mut self) {
        let n = self.panes.iter().filter(|s| s.hidden).count();
        for s in &mut self.panes {
            s.hidden = false;
        }
        self.state_dirty = true;
        self.flash(format!(
            "Showing {n} hidden pane{}",
            if n == 1 { "" } else { "s" }
        ));
    }

    pub fn hidden_count(&self) -> usize {
        self.panes.iter().filter(|s| s.hidden).count()
    }

    pub(crate) fn ensure_focus_visible(&mut self) {
        if !self.visible_panes().contains(&self.focus) {
            if let Some(&p) = self.visible_panes().first() {
                self.focus = p;
            }
        }
    }

    /// Tab: the next visible pane.
    pub fn focus_next_visible(&mut self) {
        let vis = self.visible_panes();
        if vis.is_empty() {
            return;
        }
        let i = vis
            .iter()
            .position(|&p| p == self.focus)
            .map_or(0, |i| (i + 1) % vis.len());
        self.focus = vis[i];
    }

    /// Arrow keys: the nearest pane on screen in that direction.
    pub(crate) fn move_focus(&mut self, dx: i32, dy: i32) {
        self.view = View::Grid;
        if let Some(i) = self.neighbor(self.focus, dx, dy) {
            self.focus = i;
        }
    }

    /// The pane on screen next to `from` in direction (dx, dy).
    pub fn neighbor(&self, from: usize, dx: i32, dy: i32) -> Option<usize> {
        let cur = self.pane_rects.get(from).copied().filter(|r| r.width > 0)?;
        let (cx, cy) = (
            cur.x as i32 + cur.width as i32 / 2,
            cur.y as i32 + cur.height as i32 / 2,
        );
        let mut best: Option<(i32, usize)> = None;
        for (i, r) in self.pane_rects.iter().enumerate() {
            if i == from || r.width == 0 || self.panes.get(i).is_none_or(|s| s.hidden) {
                continue;
            }
            let (x, y) = (
                r.x as i32 + r.width as i32 / 2,
                r.y as i32 + r.height as i32 / 2,
            );
            let (ddx, ddy) = (x - cx, y - cy);
            let along = ddx * dx + ddy * dy;
            if along <= 0 {
                continue;
            }
            let across = (ddx * dy).abs() + (ddy * dx).abs();
            let score = along + across * 2;
            if best.is_none_or(|(b, _)| score < b) {
                best = Some((score, i));
            }
        }
        best.map(|(_, i)| i)
    }

    /// Swap two panes' places (their tabs keep running). Focus follows
    /// the pane it was on.
    pub fn swap_panes(&mut self, a: usize, b: usize) {
        if a == b || a >= self.panes.len() || b >= self.panes.len() {
            return;
        }
        self.panes.swap(a, b);
        if self.pane_rects.len() > a.max(b) {
            self.pane_rects.swap(a, b);
        }
        let map = |i: usize| {
            if i == a {
                b
            } else if i == b {
                a
            } else {
                i
            }
        };
        self.focus = map(self.focus);
        for at in self.attention.iter_mut() {
            at.slot = map(at.slot);
        }
        // Dialogs hold pane numbers: close them rather than point astray.
        if !matches!(self.modal, crate::app::Modal::None) {
            self.modal = crate::app::Modal::None;
        }
        self.sidebar_drag = None;
        self.state_dirty = true;
    }

    /// Move pane `p` one place in a direction: the pane next to it on
    /// screen, or across a page edge the previous / next pane in order.
    /// Returns where it went.
    pub fn move_pane(&mut self, p: usize, dx: i32, dy: i32) -> Result<usize, String> {
        if self.panes.get(p).is_none_or(|s| s.hidden) {
            return Err("that pane is hidden".into());
        }
        let to = match self.neighbor(p, dx, dy) {
            Some(i) => i,
            None => {
                let vis = self.visible_panes();
                let at = vis.iter().position(|&i| i == p).unwrap_or(0);
                let back = dx < 0 || dy < 0;
                let j = if back {
                    at.checked_sub(1)
                } else {
                    (at + 1 < vis.len()).then_some(at + 1)
                };
                match j {
                    Some(j) => vis[j],
                    None => {
                        return Err(format!(
                            "pane {} is already at the {}",
                            at + 1,
                            if back { "start" } else { "end" }
                        ))
                    }
                }
            }
        };
        self.swap_panes(p, to);
        Ok(to)
    }

    /// Pane `p`'s slot number (1 based, in Ctrl-a 1..9 order).
    pub fn slot_number(&self, p: usize) -> usize {
        self.visible_panes()
            .iter()
            .position(|&i| i == p)
            .map(|i| i + 1)
            .unwrap_or(p + 1)
    }

    /// Move pane `p` to slot `n` (1 based, as Ctrl-a 1..9 counts).
    pub fn move_pane_to_slot(&mut self, p: usize, n: usize) -> Result<usize, String> {
        let vis = self.visible_panes();
        let to = *n
            .checked_sub(1)
            .and_then(|i| vis.get(i))
            .ok_or(format!("there is no slot {n} ({} panes shown)", vis.len()))?;
        self.swap_panes(p, to);
        Ok(to)
    }

    /// Mouse down on a pane border starts a drag.
    pub fn border_at(&self, col: u16, row: u16) -> Option<Border> {
        if self.view != View::Grid || self.zoom {
            return None;
        }
        self.last_arranged
            .as_ref()?
            .borders
            .iter()
            .find(|b| crate::hits::contains(b.rect, col, row))
            .cloned()
    }

    /// How pane `i`'s tab list is sized.
    pub fn side(&self, i: usize) -> crate::ui::Side {
        let s = &self.panes[i];
        crate::ui::Side {
            collapsed: s.sidebar_collapsed,
            want: s.sidebar_w,
            pct: crate::ui::width_pct(self.tab_list_width.as_deref()),
            many: s.tabs.len() > 9,
        }
    }

    /// Dragging the edge of pane `i`'s tab list to column `col`.
    pub fn drag_sidebar(&mut self, i: usize, col: u16) {
        let Some(r) = self.pane_rects.get(i).copied() else {
            return;
        };
        if r.width < 4 {
            return;
        }
        let (x0, x1) = (r.x + 1, r.x + r.width - 1);
        let w = match self.tab_pos(i) {
            crate::slot::TabPos::Left => col.saturating_sub(x0) + 1,
            crate::slot::TabPos::Right => x1.saturating_sub(col),
            crate::slot::TabPos::Top => return,
        };
        let w = w.clamp(crate::ui::SIDEBAR_MIN, crate::ui::SIDEBAR_MAX);
        if self.panes[i].sidebar_w != Some(w) {
            self.panes[i].sidebar_w = Some(w);
            self.state_dirty = true;
        }
    }

    /// Tabs ▾ > Tab list width: a preset for every pane (dragged widths reset).
    pub fn set_tab_list_width(&mut self, preset: &str) {
        self.tab_list_width = (preset != "normal").then(|| preset.to_string());
        for p in self.panes.iter_mut() {
            p.sidebar_w = None;
        }
        self.state_dirty = true;
        self.flash(format!("Tab lists: {preset}"));
    }

    /// Dragging a border to (col, row): new ratios for the current shape.
    pub fn drag_border(&mut self, b: &Border, col: u16, row: u16) {
        let Some(a) = &self.last_arranged else { return };
        let area = self.main_area;
        let shape = a.shape.clone();
        let entry = self.ratios.entry(shape.clone()).or_default();
        let (n_cols, n_rows) = shape_tracks(&shape);
        if b.vertical {
            entry.cols = layout::drag(&entry.cols, n_cols, area.x, area.width, b.i, col);
        } else {
            entry.rows = layout::drag(&entry.rows, n_rows, area.y, area.height, b.i, row);
        }
        self.state_dirty = true;
    }

    /// Back to equal sizes for the current shape.
    pub fn reset_ratios(&mut self) {
        if let Some(a) = &self.last_arranged {
            self.ratios.remove(&a.shape);
            self.state_dirty = true;
        }
    }
}

/// (columns, rows) of a shape key.
fn shape_tracks(shape: &str) -> (usize, usize) {
    match shape {
        "2+1" => (2, 2),
        "focus" => (2, 1),
        s => layout::parse_grid(s).unwrap_or((1, 1)),
    }
}

impl App {
    /// Home: back to the grid of every pane from anywhere, popups closed
    /// and zoom undone.
    pub fn go_home(&mut self) {
        if let crate::app::Modal::WakeTrain = self.modal {
            self.train_close();
        }
        self.modal = crate::app::Modal::None;
        self.view = View::Grid;
        self.zoom = false;
        self.ensure_focus_visible();
    }

    /// The menu key of what is on screen, so its button reads as the
    /// current tab (None on the grid).
    pub fn active_menu_key(&self) -> Option<char> {
        match self.modal {
            crate::app::Modal::Approvals(_) => return Some('y'),
            crate::app::Modal::Help => return Some('?'),
            _ => {}
        }
        match self.view {
            View::Grid => None,
            View::Overview => Some('o'),
            View::Dashboard => Some('d'),
            View::Sessions => Some('H'),
            View::Settings => Some(','),
            View::Loops => Some('@'),
            View::TabHistory => None,
            View::Learned | View::Prompt => None,
            View::LiveMap => Some('G'),
        }
    }

    /// Where the breadcrumb says we are.
    pub fn breadcrumb(&self) -> Vec<String> {
        let mut v = vec![];
        match self.view {
            View::Grid => {}
            View::Overview => v.push("Overview".into()),
            View::Dashboard => v.push("Dashboard".into()),
            View::Sessions => v.push("Sessions".into()),
            View::Loops => v.push("Loops".into()),
            View::TabHistory => v.push("Tab history".into()),
            View::Learned => v.push("Learned rules".into()),
            View::LiveMap => v.push("Live map".into()),
            View::Prompt => v.push("System prompt".into()),
            View::Settings => {
                v.push("Settings".into());
                let s = self.settings_section_kind();
                if let Some((_, name)) = crate::settings::SECTIONS.iter().find(|(k, _)| *k == s) {
                    v.push(name.to_string());
                }
            }
        }
        v
    }
}
