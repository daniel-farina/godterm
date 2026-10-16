//! The Loops & schedules section above the assistant chat: one line for
//! every running scheduled thing (GodTerm's own jobs, Claude's session
//! loops, durable ones, Grok's tasks), open by default and collapsible
//! (the choice is kept). Rows of one tab are grouped under a small header
//! when there are several; columns are aligned; as many rows show as fit
//! (up to about 40% of the panel) and the rest scroll. A click on a row
//! opens its detail card (ui_schedcard) with labeled buttons; managed
//! rows keep their small run, pause and delete buttons.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::app::App;
use crate::app_jobs::{SchedKind, SchedRow, Tone, OP_DELETE, OP_PAUSE, OP_RUN};
use crate::app_sched::{kind_color, plan, scroll_words, SchedLine};
use crate::hits::{contains, text, Hits, UiAction};
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};
use crate::ui_assistant::cells;

/// The name keeps at least this many cells.
const LABEL_MIN: u16 = 12;
/// A row button: " ▶ ".
const BTN_W: u16 = 3;
/// The buttons column: three buttons, or " details ".
const BTNS_W: u16 = 9;
/// The kind badge column ("managed ").
const BADGE_W: u16 = 8;

fn tone_color(t: Tone) -> Color {
    match t {
        Tone::Good => theme::SAGE,
        Tone::Bad => theme::MUTED_RED,
        Tone::Warn => theme::SAND,
        Tone::Plain => DIM,
    }
}

/// `s` cut to `w` cells (with "…" when cut).
pub fn fit(s: &str, w: u16) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if cells(&flat) <= w {
        return flat;
    }
    let mut out = String::new();
    for ch in flat.chars() {
        if cells(&out) + cells(&ch.to_string()) + 1 > w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// The columns after the name, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    Tab,
    Folder,
    Shape,
    Schedule,
    Next,
    Last,
}

/// Dropped first when the name would get too narrow.
const DROP_ORDER: [Col; 6] = [
    Col::Folder,
    Col::Shape,
    Col::Last,
    Col::Tab,
    Col::Schedule,
    Col::Next,
];

fn col_text(r: &SchedRow, c: Col) -> String {
    match c {
        Col::Tab => {
            if r.tab_id.is_empty() {
                r.tab.clone()
            } else {
                format!("{} {}", r.tab_id, r.tab)
            }
        }
        Col::Folder => {
            if r.folder.is_empty() {
                r.account.clone()
            } else {
                r.folder.clone()
            }
        }
        Col::Shape => r.shape().to_string(),
        Col::Schedule => r.cadence.clone(),
        Col::Next => r.next.clone(),
        Col::Last => r.last.clone(),
    }
}

fn col_cap(c: Col) -> u16 {
    match c {
        Col::Tab => 18,
        Col::Folder => 14,
        Col::Shape => 8,
        Col::Schedule => 22,
        Col::Next => 10,
        Col::Last => 16,
    }
}

/// Rows under a group header leave these to the header.
fn header_has(c: Col) -> bool {
    matches!(c, Col::Tab | Col::Folder)
}

/// Column widths for `rows` (each with whether it is indented under a
/// group header) in `w` cells (after the badge, before the buttons):
/// (name width, the columns shown with their widths).
pub fn columns(rows: &[(&SchedRow, bool)], w: u16) -> (u16, Vec<(Col, u16)>) {
    let all = [
        Col::Tab,
        Col::Folder,
        Col::Shape,
        Col::Schedule,
        Col::Next,
        Col::Last,
    ];
    let mut cols: Vec<(Col, u16)> = all
        .iter()
        .map(|&c| {
            let wd = rows
                .iter()
                .filter(|(_, indent)| !(*indent && header_has(c)))
                .map(|(r, _)| cells(&col_text(r, c)))
                .max()
                .unwrap_or(0)
                .min(col_cap(c));
            (c, wd)
        })
        .filter(|(_, wd)| *wd > 0)
        .collect();
    let used = |cols: &[(Col, u16)]| cols.iter().map(|(_, wd)| wd + 1).sum::<u16>();
    for d in DROP_ORDER {
        if w.saturating_sub(used(&cols)) >= LABEL_MIN {
            break;
        }
        cols.retain(|(c, _)| *c != d);
    }
    let name = w.saturating_sub(used(&cols)).max(1);
    (name, cols)
}

/// The buttons a managed row has: (glyph, op, hint).
fn buttons(r: &SchedRow, app: &App) -> Vec<(&'static str, u8, String)> {
    vec![
        (
            "▶",
            OP_RUN,
            "Run now: send this job's prompt to its tab (wakes it if it is closed or archived)"
                .into(),
        ),
        if r.paused {
            ("▷", OP_PAUSE, "Resume this job".into())
        } else {
            ("‖", OP_PAUSE, "Pause this job".into())
        },
        (
            "×",
            OP_DELETE,
            if app.sched_delete_armed(&r.key) {
                "Click again to delete this job for good".into()
            } else {
                "Delete this job (click twice)".into()
            },
        ),
    ]
}

/// Draw the section at (x, y), `w` wide, in at most `avail` rows of a
/// panel `panel_h` tall: a title row, the lines, and a divider row under
/// it that sets it off from the chat (all inside the rows returned, which
/// the chat never draws over). Returns the rows it took.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    buf: &mut Buffer,
    hits: &mut Hits,
    app: &App,
    x: u16,
    y: u16,
    w: u16,
    avail: u16,
    panel_h: u16,
) -> u16 {
    app.sched_ui.rect.set(Rect::default());
    let (rows, lines) = app.sched_view();
    // One row of `avail` is the divider.
    let (shown, h) = plan(
        app.sched_open,
        lines.len(),
        panel_h,
        avail.saturating_sub(1),
    );
    if h == 0 || w < 24 {
        return 0;
    }
    app.sched_ui.rect.set(Rect::new(x, y, w, h + 1));
    let scroll = crate::app_sched::clamp_scroll(app.sched_ui.scroll, lines.len(), shown);
    app.sched_ui.view.set((shown, lines.len()));
    let end = x + w;
    for yy in y..y + h {
        for xx in x..end {
            if let Some(c) = buf.cell_mut((xx, yy)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(BAR_BG));
            }
        }
    }
    // The divider under the section: a calm rule, so the chat below is
    // plainly another area.
    for xx in x..end {
        if let Some(c) = buf.cell_mut((xx, y + h)) {
            c.set_char('─');
            c.set_style(Style::default().fg(FAINT).bg(BAR_BG));
        }
    }
    // The header: ▾/▸, the title, a count per kind, the scroll, failures.
    let head_rect = Rect::new(x, y, w, 1);
    let hovered = app
        .mouse_pos
        .is_some_and(|(c, r)| contains(head_rect, c, r));
    // The title row is a band of its own (SEL_BG), the rows under it the
    // panel's background.
    let hbg = SEL_BG;
    for xx in x..end {
        if let Some(c) = buf.cell_mut((xx, y)) {
            c.set_style(Style::default().bg(hbg));
        }
    }
    let arrow = if app.sched_open { "▾" } else { "▸" };
    let mut cx = text(
        buf,
        x + 1,
        y,
        end,
        &format!("{arrow} Loops & schedules"),
        Style::default().fg(FG).bg(hbg).add_modifier(if hovered {
            Modifier::BOLD | Modifier::UNDERLINED
        } else {
            Modifier::BOLD
        }),
    );
    let count = |k: SchedKind| rows.iter().filter(|r| r.kind == k).count();
    let mut parts = vec![];
    for k in [
        SchedKind::Managed,
        SchedKind::Session,
        SchedKind::Durable,
        SchedKind::Grok,
    ] {
        let n = count(k);
        if n > 0 {
            parts.push(format!("{n} {}", k.badge()));
        }
    }
    let summary = if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(" · ")
    };
    cx = text(
        buf,
        cx + 1,
        y,
        end,
        &summary,
        Style::default().fg(DIM).bg(hbg),
    );
    let failed = rows.iter().filter(|r| r.tone == Tone::Bad).count();
    if failed > 0 {
        cx = text(
            buf,
            cx + 1,
            y,
            end,
            &format!("{failed} failed"),
            Style::default().fg(theme::MUTED_RED).bg(hbg),
        );
    }
    if app.sched_open {
        if let Some(s) = scroll_words(scroll, lines.len(), shown) {
            let sx = end.saturating_sub(cells(&s) + 1).max(cx + 1);
            text(
                buf,
                sx,
                y,
                end,
                &s,
                Style::default().fg(theme::SLATE).bg(hbg),
            );
        }
    }
    // The whole section (the divider too) is the wheel's: rows and buttons
    // laid over it win their clicks.
    hits.add(
        Rect::new(x, y, w, h + 1),
        UiAction::SchedArea,
        "The wheel scrolls the Loops & schedules section",
    );
    hits.add(
        head_rect,
        UiAction::SchedToggle,
        if app.sched_open {
            "Collapse the Loops & schedules section (the wheel scrolls it)"
        } else {
            "Open the Loops & schedules section: GodTerm's jobs and every session loop"
        },
    );
    if shown == 0 {
        return h + 1;
    }
    let grouped = lines.iter().any(|l| matches!(l, SchedLine::Group { .. }));
    let indent_w: u16 = if grouped { 2 } else { 0 };
    let name_x = x + 1 + indent_w + BADGE_W;
    let btn_x = end.saturating_sub(BTNS_W);
    let laid: Vec<(&SchedRow, bool)> = lines
        .iter()
        .filter_map(|l| match l {
            SchedLine::Row { idx, indent } => Some((&rows[*idx], *indent)),
            _ => None,
        })
        .collect();
    let (name_w, cols) = columns(&laid, btn_x.saturating_sub(name_x + 1));
    for (k, line) in lines.iter().enumerate().skip(scroll).take(shown) {
        let ly = y + 1 + (k - scroll) as u16;
        let sel = app.sched_ui.focus && app.sched_ui.sel == k;
        match line {
            SchedLine::Group {
                key,
                label,
                count,
                folded,
            } => {
                let rect = Rect::new(x, ly, w, 1);
                let hov = app.mouse_pos.is_some_and(|(c, r)| contains(rect, c, r));
                let bg = if hov || sel { SEL_BG } else { BAR_BG };
                for xx in x..end {
                    if let Some(c) = buf.cell_mut((xx, ly)) {
                        c.set_style(Style::default().bg(bg));
                    }
                }
                let a = if *folded { "▸" } else { "▾" };
                let gx = text(
                    buf,
                    x + 1,
                    ly,
                    end,
                    &format!("{a} {}", fit(label, w.saturating_sub(24))),
                    Style::default().fg(FG).bg(bg),
                );
                let gx = text(
                    buf,
                    gx,
                    ly,
                    end,
                    &format!(" · {count}"),
                    Style::default().fg(DIM).bg(bg),
                );
                // Its folder and account, if there is room.
                let first = rows.iter().find(|r| &r.group == key);
                if let Some(r) = first {
                    let wh: Vec<&str> = [r.folder.as_str(), r.account.as_str()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect();
                    let s = format!(" · {}", wh.join(" · "));
                    if !wh.is_empty() && gx + cells(&s) < end {
                        text(buf, gx, ly, end, &s, Style::default().fg(FAINT).bg(bg));
                    }
                }
                hits.add(
                    rect,
                    UiAction::SchedGroup(key.clone()),
                    if *folded {
                        "Show this tab's loops and jobs"
                    } else {
                        "Fold this tab's loops and jobs into one line"
                    },
                );
            }
            SchedLine::Row { idx, indent } => {
                let r = &rows[*idx];
                draw_row(
                    buf,
                    hits,
                    app,
                    r,
                    RowGeo {
                        x,
                        y: ly,
                        w,
                        badge_x: x + 1 + if *indent { indent_w } else { 0 },
                        name_x,
                        name_w,
                        btn_x,
                        sel,
                        indent: *indent,
                    },
                    &cols,
                );
            }
        }
    }
    h + 1
}

struct RowGeo {
    x: u16,
    y: u16,
    w: u16,
    badge_x: u16,
    name_x: u16,
    name_w: u16,
    btn_x: u16,
    sel: bool,
    indent: bool,
}

fn draw_row(
    buf: &mut Buffer,
    hits: &mut Hits,
    app: &App,
    r: &SchedRow,
    g: RowGeo,
    cols: &[(Col, u16)],
) {
    let (x, y, end) = (g.x, g.y, g.x + g.w);
    let rect = Rect::new(x, y, g.w, 1);
    let hov = app.mouse_pos.is_some_and(|(c, cy)| contains(rect, c, cy));
    let bg = if hov || g.sel { SEL_BG } else { BAR_BG };
    for xx in x..end {
        if let Some(c) = buf.cell_mut((xx, y)) {
            c.set_style(Style::default().bg(bg));
        }
    }
    let dim = r.paused;
    // The kind badge.
    text(
        buf,
        g.badge_x,
        y,
        g.name_x,
        r.kind.badge(),
        Style::default()
            .fg(if dim {
                theme::STONE
            } else if r.tone == Tone::Bad {
                theme::MUTED_RED
            } else {
                kind_color(r.kind)
            })
            .bg(bg),
    );
    text(
        buf,
        g.name_x,
        y,
        g.name_x + g.name_w,
        &fit(&r.label, g.name_w),
        Style::default().fg(if dim { DIM } else { FG }).bg(bg),
    );
    let mut cx = g.name_x + g.name_w + 1;
    for &(c, wd) in cols {
        let color = match c {
            Col::Next if dim => theme::STONE,
            Col::Next => theme::SLATE,
            Col::Last => tone_color(r.tone),
            Col::Shape if r.dynamic => theme::CLAY,
            Col::Shape => theme::STONE,
            _ => DIM,
        };
        if g.indent && header_has(c) {
            cx += wd + 1;
            continue;
        }
        text(
            buf,
            cx,
            y,
            cx + wd,
            &fit(&col_text(r, c), wd),
            Style::default().fg(color).bg(bg),
        );
        cx += wd + 1;
    }
    // The row: a click opens its card.
    let hint = match r.kind {
        SchedKind::Managed => format!(
            "Details of \"{}\" (managed by GodTerm): run, pause, delete, open its tab",
            r.label
        ),
        _ => {
            let mut s =
                "Details of this session loop: Manage with GodTerm, Run now, Stop, Open tab"
                    .to_string();
            if let Some(e) = &r.expires {
                s += &format!(" (it {e} with its session)");
            }
            s
        }
    };
    hits.add(
        Rect::new(x, y, g.btn_x.saturating_sub(x).max(1), 1),
        UiAction::SchedRow(r.key.clone()),
        hint.clone(),
    );
    if r.kind != SchedKind::Managed {
        // A plain labeled chip: the card has the actions.
        let rect = Rect::new(g.btn_x, y, BTNS_W.min(end - g.btn_x), 1);
        let h = app.mouse_pos.is_some_and(|(c, cy)| contains(rect, c, cy));
        text(
            buf,
            rect.x,
            y,
            end,
            " details ",
            Style::default()
                .fg(if h { FG } else { FAINT })
                .bg(if h { SEL_BG } else { bg }),
        );
        hits.add(rect, UiAction::SchedRow(r.key.clone()), hint);
        return;
    }
    for (k, (glyph, op, hint)) in buttons(r, app).into_iter().enumerate() {
        let rect = Rect::new(g.btn_x + BTN_W * k as u16, y, BTN_W, 1);
        let h = app.mouse_pos.is_some_and(|(c, cy)| contains(rect, c, cy));
        let armed = op == OP_DELETE && app.sched_delete_armed(&r.key);
        let st = if h || armed {
            Style::default()
                .fg(if armed { theme::MUTED_RED } else { FG })
                .bg(SEL_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(FAINT).bg(bg)
        };
        text(buf, rect.x, y, rect.x + BTN_W, &format!(" {glyph} "), st);
        hits.add(rect, UiAction::SchedAct(r.key.clone(), op), hint);
    }
}
