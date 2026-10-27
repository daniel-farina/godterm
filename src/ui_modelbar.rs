use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::App;
use crate::hits::{contains, text, UiAction};
use crate::theme;

pub fn split(app: &App, i: usize, interior_h: u16, term: Rect) -> (Rect, Rect) {
    if term.height < 4 || !app.model_bar_on(i, interior_h) {
        return (term, Rect::new(term.x, term.y + term.height, term.width, 0));
    }
    (
        Rect::new(term.x, term.y, term.width, term.height - 1),
        Rect::new(term.x, term.y + term.height - 1, term.width, 1),
    )
}

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

#[allow(clippy::too_many_arguments)]
fn browser_button(
    buf: &mut Buffer,
    app: &App,
    i: usize,
    y: u16,
    left: u16,
    tight: u16,
    end: u16,
    bg: ratatui::style::Color,
) -> u16 {
    use crate::tab_browser::BtnState;
    let (state, labels) = app.browser_button(i);
    let room = end.saturating_sub(left);
    let compact = crate::tab_browser::compact_label(state);
    let label = match labels
        .into_iter()
        .find(|l| l.chars().count() as u16 <= room)
    {
        Some(l) => l,
        None if compact.chars().count() as u16 <= end.saturating_sub(tight) => compact.to_string(),
        None => return end,
    };
    let bw = label.chars().count() as u16;
    let bx = end - bw;
    let rect = Rect::new(bx, y, bw, 1);
    let hovered = app.mouse_pos.is_some_and(|(c, rr)| contains(rect, c, rr));
    let fill = if hovered { theme::SEL_BG } else { bg };
    let fg = match state {
        BtnState::None => theme::FAINT,
        BtnState::Chosen | BtnState::NeedsEnable => theme::SAND,
        BtnState::Ready => theme::SAGE,
    };
    text(buf, bx, y, end, &label, Style::default().fg(fg).bg(fill));
    let unsupported = app.pane_harness(i).and_then(|h| h.no_tab_tools_reason());
    let hint = match state {
        _ if unsupported.is_some() => format!("Browser: {}", unsupported.unwrap_or_default()),
        BtnState::None => "Browser: choose the Chrome profile this tab's agent drives with BrowserSkill (Ctrl-a i)".to_string(),
        BtnState::Chosen => "Browser: BrowserSkill is not in this tab's Chrome profile yet; click to add it (Ctrl-a i)".to_string(),
        BtnState::NeedsEnable => "Browser: BrowserSkill is in the profile but turned off; click to turn it on (Ctrl-a i)".to_string(),
        BtnState::Ready => "Browser: ready, this tab's agent can drive its Chrome profile; click to change (Ctrl-a i)".to_string(),
    };
    let hint = match app.tab_browser_activity(app.panes[i].cur().uid) {
        Some(a) => format!(
            "{hint} · last: {}",
            crate::tab_browser::activity_words(&a, chrono::Utc::now())
        ),
        None => hint,
    };
    app.hits
        .borrow_mut()
        .add(rect, UiAction::TabBrowser(i), hint);
    bx.saturating_sub(1)
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

    let models_w: u16 = models
        .iter()
        .map(|m| m.label.chars().count() as u16 + 5)
        .sum::<u16>()
        + 1;

    let tight_w: u16 = models
        .iter()
        .map(|m| m.label.chars().count() as u16 + 3)
        .sum::<u16>()
        + 1;

    let web_w = crate::tab_browser::compact_label(app.tab_browser_state(tab.uid))
        .chars()
        .count() as u16
        + 1;
    let n = app.loops_in(tab.uid);
    let open = app.loops_drawer_open(i);
    let full = format!(" ⟳ loops ({n}) ");
    let short = format!(" ⟳ {n} ");
    let (fw, sw) = (full.chars().count() as u16, short.chars().count() as u16);
    let label = if r.width > fw + 24 && r.width > tight_w + web_w + fw {
        Some(full)
    } else if r.width > sw + 18 && r.width > tight_w + web_w + sw {
        Some(short)
    } else {
        None
    };
    let lw = label.as_ref().map_or(0, |l| l.chars().count() as u16);
    let end = if let Some(label) = label {
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

    let end = if queued.is_none() {
        browser_button(buf, app, i, y, r.x + models_w, r.x + tight_w, end, bg)
    } else {
        end
    };
    if models.is_empty() {
        let who = app
            .pane_harness(i)
            .map(|h| h.label())
            .unwrap_or("This agent");
        let msg = format!(" {who}: the model is chosen inside the CLI");
        let short = format!(" {who}");
        let t = if (msg.chars().count() as u16) < end.saturating_sub(r.x) {
            msg
        } else {
            short
        };
        text(
            buf,
            r.x,
            y,
            end,
            &t,
            Style::default().fg(theme::FAINT).bg(bg),
        );
        return;
    }
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
