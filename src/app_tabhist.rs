//! View ▾ > Tab history: every tab opened and closed, grouped by day,
//! with search, sort, and Reopen / Open folder / Copy session id.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;

use crate::app::{App, View};
use crate::hits::UiAction;
use crate::tab_history::Record;
use crate::theme::{self, DIM, FAINT, FG, SEL_BG};

#[derive(Debug, Clone, Default)]
pub struct TabHistUi {
    /// Every record, newest first (one per event).
    pub all: Vec<Record>,
    pub sel: usize,
    pub filter: String,
    pub searching: bool,
    /// 0 newest first, 1 oldest first, 2 by name.
    pub sort: u8,
}

impl TabHistUi {
    /// The rows shown: filtered by the search (misheard names are fine).
    pub fn rows(&self) -> Vec<Record> {
        let f = crate::tab_history::Filter {
            text: (!self.filter.trim().is_empty()).then(|| self.filter.clone()),
            limit: usize::MAX,
            ..Default::default()
        };
        let mut v =
            crate::tab_history::query(&self.all.iter().rev().cloned().collect::<Vec<_>>(), &f);
        match self.sort {
            1 => v.reverse(),
            2 => v.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(b.at.cmp(&a.at))
            }),
            _ => {}
        }
        v
    }
}

impl App {
    pub fn open_tab_history_view(&mut self) {
        self.view = if self.view == View::TabHistory {
            View::Grid
        } else {
            View::TabHistory
        };
        if self.view == View::TabHistory {
            let mut all = crate::tab_history::read_all();
            all.reverse();
            self.th = TabHistUi {
                all,
                ..Default::default()
            };
        }
    }

    fn th_selected(&self) -> Option<Record> {
        self.th.rows().get(self.th.sel).cloned()
    }

    pub fn th_action(&mut self, a: u8) {
        let Some(r) = self.th_selected() else { return };
        match a {
            0 => match self.reopen_record(&r.id) {
                Ok(_) => self.view = View::Grid,
                Err(e) => self.flash(e),
            },
            1 => {
                let p = std::path::PathBuf::from(&r.cwd);
                if !p.is_dir() {
                    self.flash(format!("{} is gone", crate::config::tilde(&p)));
                } else if !cfg!(test) && std::env::var_os("GODTERM_NO_OPEN").is_none() {
                    let _ = std::process::Command::new("open").arg(&p).spawn();
                    self.flash(format!("Opened {}", crate::config::tilde(&p)));
                }
            }
            _ => match &r.session_id {
                Some(id) => {
                    if !cfg!(test) && !crate::demo::active() {
                        crate::procs::copy_to_clipboard(id.clone());
                    }
                    self.flash(format!("Copied session id {id}"));
                }
                None => self.flash("That tab never had a session"),
            },
        }
    }

    pub fn on_tab_history_key(&mut self, k: KeyEvent) {
        let n = self.th.rows().len();
        if self.th.searching {
            match k.code {
                KeyCode::Esc => {
                    self.th.searching = false;
                    self.th.filter.clear();
                }
                KeyCode::Enter => self.th.searching = false,
                KeyCode::Backspace => {
                    self.th.filter.pop();
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.th.filter.clear()
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.th.filter.push(c);
                    self.th.sel = 0;
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.view = View::Grid,
            KeyCode::Up | KeyCode::Char('k') => self.th.sel = self.th.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.th.sel = (self.th.sel + 1).min(n.saturating_sub(1))
            }
            KeyCode::Char('/') => {
                self.th.searching = true;
                self.th.filter.clear();
            }
            KeyCode::Char('s') => {
                self.th.sort = (self.th.sort + 1) % 3;
                self.th.sel = 0;
            }
            KeyCode::Enter | KeyCode::Char('o') => self.th_action(0),
            KeyCode::Char('f') => self.th_action(1),
            KeyCode::Char('c') => self.th_action(2),
            _ => {}
        }
    }
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    if area.height < 4 {
        return;
    }
    let buf = f.buffer_mut();
    let limit = area.x + area.width;
    let rows = app.th.rows();
    let mut hits = app.hits.borrow_mut();
    // Header: title, search, sort and actions.
    let sort = ["newest first", "oldest first", "by name"][app.th.sort.min(2) as usize];
    let mut x = crate::hits::text(
        buf,
        area.x,
        area.y,
        limit,
        &format!(
            " Tab history: {}  ",
            crate::control::plural(rows.len(), "record")
        ),
        Style::default().fg(FG).add_modifier(Modifier::BOLD),
    );
    let search = if app.th.searching || !app.th.filter.is_empty() {
        format!(
            "/ {}{}",
            app.th.filter,
            if app.th.searching { "▏" } else { "" }
        )
    } else {
        "/ search".into()
    };
    for (label, a, hint, c) in [
        (
            search.as_str(),
            UiAction::ThKey('/'),
            "Search names and folders (/; misheard names are fine)",
            theme::SLATE,
        ),
        (
            &format!("Sort: {sort}") as &str,
            UiAction::ThKey('s'),
            "Newest, oldest or by name (s)",
            theme::STONE,
        ),
        (
            "Reopen",
            UiAction::ThKey('o'),
            "Resume its session in a tab (Enter)",
            theme::SAGE,
        ),
        (
            "Open folder",
            UiAction::ThKey('f'),
            "Open its folder in Finder (f)",
            theme::STONE,
        ),
        (
            "Copy session id",
            UiAction::ThKey('c'),
            "Copy its session id (c)",
            theme::STONE,
        ),
    ] {
        x = crate::hits::button(
            buf,
            &mut hits,
            app.mouse_pos,
            x,
            area.y,
            limit,
            label,
            a,
            hint,
            c,
        ) + 1;
    }
    // Rows grouped by day, the selected one in view.
    let body_top = area.y + 2;
    let height = area.bottom().saturating_sub(body_top) as usize;
    let mut lines: Vec<(Option<usize>, String)> = vec![];
    let mut day = String::new();
    for (i, r) in rows.iter().enumerate() {
        let d = r.at.chars().take(10).collect::<String>();
        if d != day && app.th.sort != 2 {
            lines.push((None, d.clone()));
            day = d;
        }
        let time = r.at.chars().skip(11).take(5).collect::<String>();
        let acct = app
            .cfg
            .accounts
            .iter()
            .find(|a| a.name == r.account)
            .map(|a| a.display().to_string())
            .unwrap_or_else(|| r.account.clone());
        let by =
            r.by.as_deref()
                .map(|b| format!(" by {b}"))
                .unwrap_or_default();
        lines.push((
            Some(i),
            format!(
                "{time}  {:<8} {:<24} {:<14} {:<36} {}{by}",
                r.event,
                crate::paths::middle(&r.name, 24),
                crate::paths::middle(&acct, 14),
                crate::paths::shorten_path(&crate::config::tilde(std::path::Path::new(&r.cwd)), 36),
                r.session_id
                    .as_deref()
                    .map(|s| &s[..s.len().min(8)])
                    .unwrap_or("-"),
            ),
        ));
    }
    let sel_line = lines
        .iter()
        .position(|(i, _)| *i == Some(app.th.sel))
        .unwrap_or(0);
    let first = sel_line.saturating_sub(height.saturating_sub(2));
    for (k, (i, text)) in lines.iter().skip(first).take(height).enumerate() {
        let y = body_top + k as u16;
        match i {
            None => {
                crate::hits::text(
                    buf,
                    area.x + 1,
                    y,
                    limit,
                    text,
                    Style::default()
                        .fg(theme::SAND)
                        .add_modifier(Modifier::BOLD),
                );
            }
            Some(i) => {
                let on = *i == app.th.sel;
                let st = if on {
                    Style::default().fg(FG).bg(SEL_BG)
                } else {
                    Style::default().fg(DIM)
                };
                crate::hits::text(
                    buf,
                    area.x + 2,
                    y,
                    limit,
                    &format!("{} {text}", if on { "›" } else { " " }),
                    st,
                );
                hits.add(
                    Rect::new(area.x, y, area.width, 1),
                    UiAction::ThRow(*i),
                    "Select (double click: reopen)",
                );
            }
        }
    }
    if rows.is_empty() {
        crate::hits::text(
            buf,
            area.x + 2,
            body_top,
            limit,
            if app.th.filter.is_empty() {
                "No tabs recorded yet."
            } else {
                "Nothing matches the search."
            },
            Style::default().fg(FAINT),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_and_searches() {
        let r = |name: &str, at: &str| Record {
            id: name.into(),
            name: name.into(),
            at: at.into(),
            event: "close".into(),
            cwd: format!("/u/{name}"),
            ..Default::default()
        };
        let ui = TabHistUi {
            all: vec![
                r("zeta", "2026-10-06T10:00:00+00:00"),
                r("alpha", "2026-10-05T10:00:00+00:00"),
            ],
            ..Default::default()
        };
        assert_eq!(ui.rows()[0].name, "zeta");
        let ui2 = TabHistUi {
            sort: 2,
            ..ui.clone()
        };
        assert_eq!(ui2.rows()[0].name, "alpha");
        let ui3 = TabHistUi {
            filter: "zeeta".into(),
            ..ui
        };
        assert_eq!(ui3.rows().len(), 1);
    }
}
