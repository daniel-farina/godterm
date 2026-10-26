//! Drawing the Git section (app_gitpanel): the browser dialog's look, with
//! the actions as clickable rows and the buttons on the bottom edge.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui::Frame;

use crate::app::App;
use crate::app_gitpanel::GitDlg;
use crate::hits::{contains, text};
use crate::tab_browser::{DlgView, Tone};
use crate::theme;

fn tone_color(t: Tone) -> Color {
    match t {
        Tone::Normal => theme::FG,
        Tone::Dim => theme::DIM,
        Tone::Good => theme::SAGE,
        Tone::Warn => theme::SAND,
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App, d: &GitDlg) {
    draw_view(f, area, app, &app.git_dialog_view(d));
}

/// A Git dialog (the Git section, the accounts and rules editor). Taller
/// than the screen: scrolled so the selected row stays in view.
pub fn draw_view(f: &mut Frame, area: Rect, app: &App, v: &DlgView) {
    let w = 104u16.min(area.width);
    let inner_w = w.saturating_sub(4) as usize;
    let mut rs = crate::ui_browser::rows(v, inner_w);
    let h = (rs.len() as u16 + 3).min(area.height);
    let room = h.saturating_sub(3) as usize;
    if rs.len() > room {
        let sel = rs
            .iter()
            .position(|(t, _, _)| t.starts_with('›'))
            .unwrap_or(0);
        let skip = sel.saturating_sub(room / 2).min(rs.len() - room);
        rs.drain(..skip);
    }
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::SLATE))
            .title(format!(" {} ", v.title))
            .style(Style::default().bg(theme::BAR_BG).fg(theme::FG)),
        r,
    );
    let buf = f.buffer_mut();
    let limit = r.x + r.width - 1;
    for (k, (t, tone, li)) in rs.iter().enumerate() {
        let y = r.y + 1 + k as u16;
        if y >= r.y + r.height - 1 {
            break;
        }
        let rect = Rect::new(r.x + 1, y, r.width - 2, 1);
        let action = v.lines[*li].action.clone();
        let hovered =
            action.is_some() && app.mouse_pos.is_some_and(|(c, row)| contains(rect, c, row));
        let selected = action.is_some() && t.starts_with('›');
        let mut st = Style::default().fg(tone_color(*tone)).bg(theme::BAR_BG);
        if selected {
            st = st.bg(theme::SELECT_BG);
        } else if hovered {
            st = st.bg(theme::SEL_BG);
        }
        if action.is_some() {
            for x in rect.x..rect.x + rect.width {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_char(' ');
                    c.set_style(st);
                }
            }
        }
        text(buf, r.x + 2, y, limit, t, st);
        if let Some(a) = action {
            app.hits
                .borrow_mut()
                .add(rect, a, "Press it (asks before changing anything)");
        }
    }
    let buttons: Vec<(&str, crate::hits::UiAction, &str)> = v
        .buttons
        .iter()
        .map(|(l, a, h)| (l.as_str(), a.clone(), h.as_str()))
        .collect();
    crate::ui_chrome::modal_chrome(buf, app, r, &buttons);
}

/// Settings > GitHub: what is set now, and the button to the editor.
pub fn draw_cfg_section(f: &mut Frame, app: &App, area: Rect) {
    let buf = f.buffer_mut();
    let limit = area.x + area.width;
    let bottom = area.y + area.height;
    let mut y = area.y + 1;
    text(
        buf,
        area.x + 1,
        y,
        limit,
        "GitHub accounts and repos",
        Style::default().fg(theme::FG),
    );
    y += 2;
    let mut hits = app.hits.borrow_mut();
    crate::hits::button(
        buf,
        &mut hits,
        app.mouse_pos,
        area.x + 1,
        y,
        limit,
        "Edit owners, rules and bindings",
        crate::hits::UiAction::GitCfgOpen,
        "Open the editor (Enter); saving asks first",
        theme::SLATE,
    );
    drop(hits);
    y += 2;
    for (t, tone) in app.gitcfg_summary() {
        for line in crate::ui_browser::wrap(&t, area.width.saturating_sub(3) as usize) {
            if y >= bottom {
                return;
            }
            text(
                buf,
                area.x + 1,
                y,
                limit,
                &line,
                Style::default().fg(tone_color(tone)),
            );
            y += 1;
        }
    }
}
