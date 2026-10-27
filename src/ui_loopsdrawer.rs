use chrono::Utc;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::App;
use crate::app_loopsdrawer::{
    loop_facts, MAX_ROWS, MIN_ROWS, MIN_TERM, OP_CANCEL, OP_COPY, OP_RUN,
};
use crate::harness::Harness;
use crate::hits::{button, text, UiAction};
use crate::theme;

pub fn split(app: &App, i: usize, term: Rect) -> (Rect, Rect) {
    let none = (term, Rect::new(term.x, term.y + term.height, term.width, 0));
    let Some(want) = app.loops_drawer.get(&i).copied() else {
        return none;
    };
    let h = want
        .clamp(MIN_ROWS, MAX_ROWS)
        .min(term.height.saturating_sub(MIN_TERM));
    if h < MIN_ROWS || term.width < 20 {
        return none;
    }
    (
        Rect::new(term.x, term.y, term.width, term.height - h),
        Rect::new(term.x, term.y + term.height - h, term.width, h),
    )
}

pub fn draw(buf: &mut Buffer, app: &App, i: usize, r: Rect) {
    if r.width == 0 || r.height == 0 {
        app.loops_drawer_geo.borrow_mut().remove(&i);
        return;
    }
    app.loops_drawer_geo.borrow_mut().insert(i, r);
    let bg = theme::BAR_BG;
    let end = r.x + r.width;
    for y in r.y..r.y + r.height {
        for x in r.x..end {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(bg));
            }
        }
    }
    let slot = &app.panes[i];
    let tab = slot.cur();
    let focused = app.loops_drawer_focus == Some(i);
    let grok = app.pane_harness(i) == Some(Harness::Grok);
    let source = if grok { "Grok" } else { "Claude" };
    let mut hits = app.hits.borrow_mut();

    let edge = if focused { theme::SLATE } else { theme::FAINT };
    for x in r.x..end {
        if let Some(c) = buf.cell_mut((x, r.y)) {
            c.set_char('─');
            c.set_style(Style::default().fg(edge).bg(bg));
        }
    }
    hits.add(
        Rect::new(r.x, r.y, r.width, 1),
        UiAction::LoopsDrawerEdge(i),
        "Drag to resize the loops drawer",
    );
    let title = format!(" ⟳ loops on {} ", crate::sessions::snippet(&tab.name(), 30));
    text(
        buf,
        r.x + 1,
        r.y,
        end,
        &title,
        Style::default()
            .fg(theme::FG)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    );
    let close_w = 3;
    let new_w = 11;
    if r.width > close_w + new_w + title.chars().count() as u16 + 4 {
        let x = end - close_w - new_w - 2;
        button(
            buf,
            &mut hits,
            app.mouse_pos,
            x,
            r.y,
            end,
            "New loop…",
            UiAction::LoopsNew(i),
            "Type /loop into this tab to schedule a prompt (you finish it)",
            theme::SAGE,
        );
    }
    button(
        buf,
        &mut hits,
        app.mouse_pos,
        end - close_w - 1,
        r.y,
        end,
        "×",
        UiAction::LoopsDrawer(i),
        "Close the loops drawer (Esc, Ctrl-a ;)",
        theme::STONE,
    );
    let rows = app.drawer_loops(i);
    let dim = Style::default().fg(theme::DIM).bg(bg);
    let body = r.height.saturating_sub(1);
    if rows.is_empty() {
        let mut y = r.y + 1;
        let mut line = |s: &str, st: Style| {
            if y < r.y + r.height {
                text(buf, r.x + 2, y, end, s, st);
                y += 1;
            }
        };
        if !tab.is_running() {
            line(
                "The tab is not running: its loops show once it runs.",
                Style::default().fg(theme::FG).bg(bg),
            );
        } else {
            line(
                "No loops on this tab.",
                Style::default().fg(theme::FG).bg(bg),
            );
        }
        line(
            if grok {
                "Grok: no scheduled tasks in this session (scheduler_create, /loop)."
            } else {
                "Claude: no /loop or cron jobs in this session's transcript or .claude/scheduled_tasks.json."
            },
            dim,
        );
        return;
    }
    let now = Utc::now();

    let fits = (body / 2) as usize;
    let more = rows.len().saturating_sub(fits);
    let shown = if more > 0 {
        (body.saturating_sub(1) / 2) as usize
    } else {
        rows.len()
    };
    let more = rows.len() - shown;
    let sel = app.loops_drawer_sel.min(rows.len().saturating_sub(1));
    for (k, row) in rows.iter().enumerate().take(shown) {
        let y = r.y + 1 + 2 * k as u16;
        let selected = focused && k == sel;
        let rbg = if selected { theme::SEL_BG } else { bg };
        if selected {
            for yy in [y, y + 1] {
                for x in r.x..end {
                    if let Some(c) = buf.cell_mut((x, yy)) {
                        c.set_style(Style::default().bg(rbg));
                    }
                }
            }
        }
        hits.add(
            Rect::new(r.x, y, r.width, 2),
            UiAction::LoopRow(i, k),
            format!("Loop {} ({source})", row.lp.id),
        );

        let armed = app.loop_cancel_armed(row.uid, &row.lp.id);
        let asked = app.loop_cancel_asked(row.uid, &row.lp.id);
        let btns: [(&str, u8, &str, ratatui::style::Color); 3] = [
            (
                "Run now",
                OP_RUN,
                "Send this loop's prompt to the tab now (once it is idle)",
                theme::SAGE,
            ),
            (
                if armed { "Cancel?" } else { "Cancel" },
                OP_CANCEL,
                "Ask the tab to cancel this loop (typed in once it is idle); click twice",
                theme::CLAY,
            ),
            ("Copy", OP_COPY, "Copy this loop's details", theme::STONE),
        ];
        let bw: u16 = btns
            .iter()
            .map(|(l, ..)| l.chars().count() as u16 + 3)
            .sum();
        let room = r.width > bw + 20;
        let text_end = if room { end - bw - 1 } else { end };
        let mut x = r.x + 1;
        x = text(
            buf,
            x,
            y,
            text_end,
            &format!("{source} "),
            Style::default().fg(theme::SLATE).bg(rbg),
        );
        x = text(
            buf,
            x,
            y,
            text_end,
            &format!("{} ", row.lp.cadence()),
            Style::default().fg(theme::FG).bg(rbg),
        );
        let avail = text_end.saturating_sub(x) as usize;

        let inline = avail >= 16;
        if inline {
            text(
                buf,
                x,
                y,
                text_end,
                &format!(
                    "\"{}\"",
                    crate::sessions::snippet(&row.lp.prompt, avail.saturating_sub(2).min(80))
                ),
                Style::default().fg(theme::STONE).bg(rbg),
            );
        }
        if room {
            let mut bx = text_end + 1;
            for (label, op, hint, c) in btns {
                bx = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    bx,
                    y,
                    end,
                    label,
                    UiAction::LoopAction(i, k, op),
                    hint,
                    c,
                ) + 1;
            }
        }
        let (next, last, status) = loop_facts(row, now);
        let status = if asked {
            "cancel asked".to_string()
        } else {
            status
        };
        text(
            buf,
            r.x + 3,
            y + 1,
            end,
            &if inline {
                format!("{next} · {last} · {status}")
            } else {
                format!(
                    "\"{}\" · {next} · {last} · {status}",
                    crate::sessions::snippet(&row.lp.prompt, 24)
                )
            },
            Style::default().fg(theme::DIM).bg(rbg),
        );
    }
    if more > 0 {
        text(
            buf,
            r.x + 2,
            r.y + r.height - 1,
            end,
            &format!("+{more} more (Ctrl-a @ lists every loop)"),
            dim,
        );
    }
}
