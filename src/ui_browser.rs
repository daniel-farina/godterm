//! Drawing the Browser dialog (tab_browser): a small box with the Chrome
//! profiles as clickable rows, a status line and buttons.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui::Frame;

use crate::app::App;
use crate::hits::{contains, text};
use crate::tab_browser::{BrowserDlg, DlgView, Tone};
use crate::theme;

fn tone_color(t: Tone) -> Color {
    match t {
        Tone::Normal => theme::FG,
        Tone::Dim => theme::DIM,
        Tone::Good => theme::SAGE,
        Tone::Warn => theme::SAND,
    }
}

/// Break `s` into rows of at most `w` columns, at spaces where it can.
pub fn wrap(s: &str, w: usize) -> Vec<String> {
    if s.chars().count() <= w || w < 8 {
        return vec![s.to_string()];
    }
    let mut rows = vec![];
    let mut cur = String::new();
    for word in s.split(' ') {
        let need = cur.chars().count() + word.chars().count() + usize::from(!cur.is_empty());
        if need > w && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// The dialog's rows after wrapping: (text, tone, line index it came from).
pub fn rows(v: &DlgView, w: usize) -> Vec<(String, Tone, usize)> {
    let mut out = vec![];
    for (i, l) in v.lines.iter().enumerate() {
        // A profile row keeps one row (it is cut, not wrapped).
        if l.action.is_some() {
            out.push((l.text.chars().take(w).collect(), l.tone, i));
            continue;
        }
        for r in wrap(&l.text, w) {
            out.push((r, l.tone, i));
        }
    }
    out
}

pub fn draw(f: &mut Frame, area: Rect, app: &App, d: &BrowserDlg) {
    let Some(v) = app.browser_dialog_view(d) else {
        return;
    };
    let w = 84u16.min(area.width);
    let inner_w = w.saturating_sub(4) as usize;
    let rs = rows(&v, inner_w);
    let h = (rs.len() as u16 + 3).min(area.height);
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
                .add(rect, a, "Use this Chrome profile for the tab");
        }
    }
    let buttons: Vec<(&str, crate::hits::UiAction, &str)> = v
        .buttons
        .iter()
        .map(|(l, a, h)| (l.as_str(), a.clone(), h.as_str()))
        .collect();
    crate::ui_chrome::modal_chrome(buf, app, r, &buttons);
}
