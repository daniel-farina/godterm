//! Picking a color: an account's (account ▾ > Color…, saved to its
//! `color` in config.toml) or one tab's accent (tab menu > Accent color…,
//! saved in state.json). Default plus the muted swatches.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::{App, Modal};
use crate::hits::UiAction;
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Account(usize),
    /// A tab, by uid.
    Tab(u64),
    /// A tab group: (pane, group).
    Group(usize, usize),
}

/// Choices: 0 is Default, then theme::SWATCHES.
pub fn count() -> usize {
    theme::SWATCHES.len() + 1
}

impl App {
    pub fn open_color_pick(&mut self, t: Target) {
        let cur = match t {
            Target::Account(a) => self.cfg.accounts.get(a).map(|c| c.color.clone()),
            Target::Tab(uid) => self
                .find_tab(uid)
                .and_then(|(s, i)| self.panes[s].tabs[i].accent.clone()),
            Target::Group(s, g) => self
                .panes
                .get(s)
                .and_then(|p| p.groups.get(g))
                .and_then(|g| g.color.clone()),
        };
        let sel = cur
            .and_then(|c| theme::SWATCHES.iter().position(|s| *s == c))
            .map(|i| i + 1)
            .unwrap_or(0);
        self.modal = Modal::ColorPick(t, sel);
    }

    /// Apply choice `i` (0: Default) to the target.
    pub fn apply_color(&mut self, t: Target, i: usize) {
        self.modal = Modal::None;
        let pick =
            (i > 0).then(|| theme::SWATCHES[(i - 1).min(theme::SWATCHES.len() - 1)].to_string());
        match t {
            Target::Account(a) if a < self.cfg.accounts.len() => {
                let c =
                    pick.unwrap_or_else(|| theme::ROTATION[a % theme::ROTATION.len()].to_string());
                self.cfg.accounts[a].color = c.clone();
                if let Err(e) = self.save_account_key(a, "color", toml_edit::value(c.as_str())) {
                    self.flash(format!("Could not save the color: {e:#}"));
                    return;
                }
                self.config_mtime = crate::app::config_mtime();
                self.flash(format!("{} is {c} now", self.cfg.accounts[a].display()));
            }
            Target::Tab(uid) => {
                if let Some((s, ti)) = self.find_tab(uid) {
                    self.panes[s].tabs[ti].accent = pick.clone();
                    self.state_dirty = true;
                    self.flash(match pick {
                        Some(c) => format!("Tab accent: {c}"),
                        None => "Tab accent: the account's color".into(),
                    });
                }
            }
            Target::Group(s, g) => {
                if let Some(grp) = self.panes.get_mut(s).and_then(|p| p.groups.get_mut(g)) {
                    grp.color = pick.clone();
                    let n = grp.name.clone();
                    self.state_dirty = true;
                    self.flash(match pick {
                        Some(c) => format!("Group {n}: {c} (its tabs' default accent)"),
                        None => format!("Group {n}: no color"),
                    });
                }
            }
            _ => {}
        }
    }

    pub fn on_color_pick_key(&mut self, k: KeyEvent) {
        let Modal::ColorPick(t, sel) = self.modal.clone() else {
            return;
        };
        let n = count();
        match k.code {
            KeyCode::Esc => self.modal = Modal::None,
            KeyCode::Left | KeyCode::Up => self.modal = Modal::ColorPick(t, (sel + n - 1) % n),
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                self.modal = Modal::ColorPick(t, (sel + 1) % n)
            }
            KeyCode::Enter => self.apply_color(t, sel),
            KeyCode::Char('d') => self.apply_color(t, 0),
            KeyCode::Char(c @ '0'..='9') => {
                // 1..9 the first swatches, 0 the tenth.
                let i = if c == '0' {
                    10
                } else {
                    c as usize - '0' as usize
                };
                if i < n {
                    self.apply_color(t, i);
                }
            }
            _ => {}
        }
    }
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &App, t: Target, sel: usize) {
    use crate::hits::text;
    let w = 60u16.min(area.width);
    let h = 7u16.min(area.height);
    if w < 20 || h < 4 {
        return;
    }
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    for y in r.y..r.y + r.height {
        for x in r.x..r.x + r.width {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(BAR_BG));
            }
        }
    }
    let title = match t {
        Target::Account(a) => format!(
            " Color for {} ",
            app.cfg
                .accounts
                .get(a)
                .map(|c| c.display())
                .unwrap_or("account")
        ),
        Target::Tab(uid) => {
            let name = app
                .find_tab(uid)
                .map(|(s, i)| app.panes[s].tabs[i].name())
                .unwrap_or_default();
            format!(" Accent for tab {name} ")
        }
        Target::Group(s, g) => format!(
            " Color for group {} ",
            app.panes
                .get(s)
                .and_then(|p| p.groups.get(g))
                .map(|g| g.name.as_str())
                .unwrap_or("")
        ),
    };
    let bst = Style::default().fg(DIM).bg(BAR_BG);
    for x in r.x..r.x + r.width {
        for y in [r.y, r.y + r.height - 1] {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char('─');
                c.set_style(bst);
            }
        }
    }
    for y in r.y..r.y + r.height {
        for x in [r.x, r.x + r.width - 1] {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char('│');
                c.set_style(bst);
            }
        }
    }
    for (x, y, ch) in [
        (r.x, r.y, "╭"),
        (r.x + r.width - 1, r.y, "╮"),
        (r.x, r.y + r.height - 1, "╰"),
        (r.x + r.width - 1, r.y + r.height - 1, "╯"),
    ] {
        if let Some(c) = buf.cell_mut((x, y)) {
            c.set_symbol(ch);
            c.set_style(bst);
        }
    }
    let limit = r.x + r.width - 2;
    let mut hits = app.hits.borrow_mut();
    hits.add(r, UiAction::ColorPick(usize::MAX), "");
    text(
        buf,
        r.x + 2,
        r.y,
        limit,
        &title,
        Style::default()
            .fg(FG)
            .bg(BAR_BG)
            .add_modifier(Modifier::BOLD),
    );
    let y = r.y + 2;
    let mut x = r.x + 2;
    let default_c = match t {
        Target::Account(a) => theme::parse_color(theme::ROTATION[a % theme::ROTATION.len()]),
        Target::Tab(uid) => app
            .find_tab(uid)
            .map(|(s, _)| app.account_color(app.panes[s].account))
            .unwrap_or(theme::STONE),
        Target::Group(s, _) => app
            .panes
            .get(s)
            .map(|p| app.account_color(p.account))
            .unwrap_or(theme::STONE),
    };
    for i in 0..count() {
        let (label, color, name) = if i == 0 {
            ("Default".to_string(), default_c, "default")
        } else {
            let n = theme::SWATCHES[i - 1];
            ("●".to_string(), theme::parse_color(n), n)
        };
        let s = if i == sel {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        };
        let wd = s.chars().count() as u16;
        if x + wd > limit {
            break;
        }
        let st = Style::default()
            .fg(color)
            .bg(if i == sel { SEL_BG } else { BAR_BG });
        text(buf, x, y, limit, &s, st);
        let key = match i {
            0 => "d".to_string(),
            10 => "0".to_string(),
            k => k.to_string(),
        };
        hits.add(
            Rect::new(x, y, wd, 1),
            UiAction::ColorPick(i),
            format!("{name} ({key})"),
        );
        x += wd + 1;
    }
    let name = if sel == 0 {
        "default".to_string()
    } else {
        theme::SWATCHES[sel - 1].to_string()
    };
    text(
        buf,
        r.x + 2,
        y + 2,
        limit,
        &format!("{name} · ←/→ choose, Enter applies, 1-9 0 pick, Esc cancels"),
        Style::default().fg(FAINT).bg(BAR_BG),
    );
}
