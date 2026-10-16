//! Drawing the model bar (app_tabmodel): `model  1 Haiku  2 Sonnet  3 Opus
//! · medium`, the current model on a calm slate fill, a queued switch
//! named at the right.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::App;
use crate::hits::{contains, text, UiAction};
use crate::theme;

/// Take the model bar's row off the bottom of pane `i`'s terminal area
/// (`interior_h`: the pane's rows inside its border). (terminal, bar row;
/// zero sized when there is no bar).
pub fn split(app: &App, i: usize, interior_h: u16, term: Rect) -> (Rect, Rect) {
    if term.height < 4 || !app.model_bar_on(i, interior_h) {
        return (term, Rect::new(term.x, term.y + term.height, term.width, 0));
    }
    (
        Rect::new(term.x, term.y, term.width, term.height - 1),
        Rect::new(term.x, term.y + term.height - 1, term.width, 1),
    )
}

/// What fits in a row `w` wide: (the "model" prefix, the number hints).
fn layout(labels: &[String], w: u16) -> (bool, bool) {
    let need = |prefix: bool, digits: bool| -> u16 {
        let p = if prefix { 7 } else { 1 };
        p + labels
            .iter()
            .map(|l| l.chars().count() as u16 + if digits { 4 } else { 2 } + 1)
            .sum::<u16>()
    };
    if need(true, true) <= w {
        (true, true)
    } else if need(false, true) <= w {
        (false, true)
    } else {
        (false, false)
    }
}

pub fn draw(buf: &mut Buffer, app: &App, i: usize, r: Rect) {
    if r.width == 0 || r.height == 0 {
        return;
    }
    let slot = &app.panes[i];
    let t = slot.active;
    let tab = &slot.tabs[t];
    let models = app.bar_models(i);
    let cur = app.tab_model_index(i, t, &models);
    let queued = app.queued_model(tab.uid).cloned();
    let bg = theme::BAR_BG;
    let y = r.y;
    let end = r.x + r.width;
    for x in r.x..end {
        if let Some(c) = buf.cell_mut((x, y)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(bg));
        }
    }
    // The loops drawer's button keeps the right end: "⟳ loops (N)",
    // dim at 0.
    let n = app.loops_in(tab.uid);
    let open = app.loops_drawer_open(i);
    let label = format!(" ⟳ loops ({n}) ");
    let lw = label.chars().count() as u16;
    let end = if r.width > lw + 24 {
        let lx = end - lw;
        let rect = Rect::new(lx, y, lw, 1);
        let hovered = app.mouse_pos.is_some_and(|(c, rr)| contains(rect, c, rr));
        let fill = if open {
            theme::SELECT_BG
        } else if hovered {
            theme::SEL_BG
        } else {
            bg
        };
        let fg = if n == 0 && !open {
            theme::FAINT
        } else {
            theme::FG
        };
        text(buf, lx, y, end, &label, Style::default().fg(fg).bg(fill));
        app.hits.borrow_mut().add(
            rect,
            UiAction::LoopsDrawer(i),
            if open {
                "Close the loops drawer (Ctrl-a ;)".to_string()
            } else {
                format!(
                    "This tab's loops ({n}): open the drawer to see, run or cancel them (Ctrl-a ;)"
                )
            },
        );
        lx.saturating_sub(1)
    } else {
        end
    };
    let labels: Vec<String> = models.iter().map(|m| m.label.clone()).collect();
    let (prefix, digits) = layout(&labels, end - r.x);
    let mut x = r.x;
    x = if prefix {
        text(
            buf,
            x,
            y,
            end,
            " model ",
            Style::default().fg(theme::DIM).bg(bg),
        )
    } else {
        x + 1
    };
    let mut hits = app.hits.borrow_mut();
    for (k, m) in models.iter().enumerate() {
        if x >= end {
            break;
        }
        let is_cur = cur == Some(k);
        let is_queued = queued.as_ref().is_some_and(|q| q.arg == m.arg);
        let w = m.label.chars().count() as u16 + if digits { 4 } else { 2 };
        let rect = Rect::new(x, y, w.min(end - x), 1);
        let hovered = app.mouse_pos.is_some_and(|(c, rr)| contains(rect, c, rr));
        let fill = if is_cur {
            theme::SELECT_BG
        } else if hovered {
            theme::SEL_BG
        } else {
            bg
        };
        let label_style = if is_cur {
            Style::default()
                .fg(theme::FG)
                .bg(fill)
                .add_modifier(Modifier::BOLD)
        } else if is_queued {
            Style::default()
                .fg(theme::SLATE)
                .bg(fill)
                .add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default().fg(theme::STONE).bg(fill)
        };
        let mut cx = text(buf, x, y, end, " ", Style::default().bg(fill));
        if digits {
            cx = text(
                buf,
                cx,
                y,
                end,
                &format!("{} ", k + 1),
                Style::default()
                    .fg(if is_cur { theme::DIM } else { theme::FAINT })
                    .bg(fill),
            );
        }
        cx = text(buf, cx, y, end, &m.label, label_style);
        text(buf, cx, y, end, " ", Style::default().bg(fill));
        let desc = if m.desc.is_empty() {
            String::new()
        } else {
            format!(" ({})", m.desc)
        };
        let hint = if is_cur {
            format!("{}{desc}: this tab's model now", m.label)
        } else {
            format!(
                "{}{desc}: click to switch this tab (/model {}, after its turn when busy); Ctrl-a # then {}",
                m.label,
                m.arg,
                k + 1
            )
        };
        hits.add(rect, UiAction::TabModel(i, k), hint);
        x = rect.x + rect.width + 1;
    }
    drop(hits);
    let dim = Style::default().fg(theme::DIM).bg(bg);
    if let Some(e) = &tab.effort {
        x = text(buf, x, y, end, &format!("· {e} "), dim);
    }
    if let Some(q) = queued {
        let why = if tab.activity == crate::pane::Activity::Working
            || tab.activity == crate::pane::Activity::Permission
        {
            "after this turn"
        } else {
            "when it is ready"
        };
        text(
            buf,
            x + 1,
            y,
            end,
            &format!("switching to {} {why}", q.label),
            Style::default().fg(theme::SLATE).bg(bg),
        );
    } else if cur.is_none() {
        if let Some(m) = &tab.model {
            text(buf, x + 1, y, end, &format!("now: {m}"), dim);
        }
    }
}
