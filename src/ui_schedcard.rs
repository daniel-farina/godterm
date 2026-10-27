use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};
use ratatui::Frame;

use crate::app::App;
use crate::app_sched::{kind_color, BtnTone, SchedCard};
use crate::hits::{text, UiAction};
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};
use crate::ui_assistant::cells;

const LABEL_W: u16 = 10;

pub fn wrap_words(s: &str, w: u16) -> Vec<String> {
    let w = w.max(4);
    let mut out = vec![];
    for para in s.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            loop {
                let lw = cells(&line);
                let ww = cells(&word);
                if lw == 0 && ww > w {
                    let mut head = String::new();
                    for ch in word.chars() {
                        if cells(&head) + cells(&ch.to_string()) > w {
                            break;
                        }
                        head.push(ch);
                    }
                    word = word[head.len()..].to_string();
                    out.push(head);
                    continue;
                }
                if lw == 0 {
                    line = word;
                } else if lw + 1 + ww <= w {
                    line.push(' ');
                    line.push_str(&word);
                } else {
                    out.push(std::mem::take(&mut line));
                    continue;
                }
                break;
            }
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) && out.len() > 1 {
        out.pop();
    }
    out
}

fn tone_colors(t: BtnTone) -> (Color, Color) {
    match t {
        BtnTone::Primary => (theme::SAGE, theme::SAGE),
        BtnTone::Neutral => (theme::STONE, theme::STONE),
        BtnTone::Danger => (theme::MUTED_RED, theme::MUTED_RED),
    }
}

fn layout(
    mut buf: Option<&mut Buffer>,
    app: &App,
    c: &SchedCard,
    inner: Rect,
    hits: &mut crate::hits::Hits,
) -> u16 {
    let x = inner.x + 1;
    let end = inner.x + inner.width.saturating_sub(1);
    let w = end.saturating_sub(x);
    let bottom = inner.y + inner.height;
    let mut y = inner.y;
    let put = |buf: &mut Option<&mut Buffer>, y: u16, cx: u16, s: &str, st: Style| -> u16 {
        match buf {
            Some(b) if y < bottom => text(b, cx, y, end, s, st),
            _ => cx + cells(s),
        }
    };

    let mut cx = x;
    for (b, col) in &c.badges {
        let s = format!(" {b} ");
        if cx + cells(&s) > end {
            break;
        }
        cx = put(&mut buf, y, cx, &s, Style::default().fg(*col).bg(SEL_BG)) + 1;
    }
    y += 1;

    for l in wrap_words(&c.name, w) {
        put(
            &mut buf,
            y,
            x,
            &l,
            Style::default()
                .fg(FG)
                .bg(BAR_BG)
                .add_modifier(Modifier::BOLD),
        );
        y += 1;
    }
    y += 1;

    put(
        &mut buf,
        y,
        x,
        "What it sends",
        Style::default().fg(DIM).bg(BAR_BG),
    );
    y += 1;
    let prompt = if c.prompt.trim().is_empty() {
        "(no prompt)".to_string()
    } else {
        c.prompt.clone()
    };
    for l in wrap_words(&prompt, w.saturating_sub(2)) {
        put(&mut buf, y, x + 2, &l, Style::default().fg(FG).bg(BAR_BG));
        y += 1;
    }
    y += 1;

    for (label, t, col) in &c.facts {
        put(&mut buf, y, x, label, Style::default().fg(DIM).bg(BAR_BG));
        for (k, l) in wrap_words(t, w.saturating_sub(LABEL_W))
            .into_iter()
            .enumerate()
        {
            put(
                &mut buf,
                y + k as u16,
                x + LABEL_W,
                &l,
                Style::default().fg(*col).bg(BAR_BG),
            );
            if k > 0 {
                y += 1;
            }
        }
        y += 1;
    }
    y += 1;

    let mut bx = x;
    for b in &c.buttons {
        let bw = cells(&b.label) + 2;
        if bx > x && bx + bw > end {
            bx = x;
            y += 2;
        }
        if let Some(bf) = buf.as_deref_mut() {
            if y < bottom {
                let (rest, fill) = tone_colors(b.tone);
                let next = crate::hits::button(
                    bf,
                    hits,
                    app.mouse_pos,
                    bx,
                    y,
                    end,
                    &b.label,
                    UiAction::SchedCardAct(b.op),
                    &b.hint,
                    fill,
                );
                let hovered = app
                    .mouse_pos
                    .is_some_and(|(mc, mr)| mr == y && mc >= bx && mc < bx + bw);
                if !hovered {
                    for cx in bx..(bx + bw).min(end) {
                        if let Some(cell) = bf.cell_mut((cx, y)) {
                            let mut st = Style::default().fg(rest).bg(SEL_BG);
                            if b.tone == BtnTone::Primary {
                                st = st.add_modifier(Modifier::BOLD);
                            }
                            cell.set_style(st);
                        }
                    }
                }
                bx = next + 1;
                continue;
            }
        }
        bx += bw + 2;
    }
    y += 2;
    if let Some(n) = &c.note {
        for l in wrap_words(n, w) {
            put(&mut buf, y, x, &l, Style::default().fg(FAINT).bg(BAR_BG));
            y += 1;
        }
    }

    let keys: Vec<String> = c
        .buttons
        .iter()
        .filter(|b| b.key != 'q')
        .map(|b| {
            let what = match b.op {
                crate::app_jobs::OP_ADOPT => "manage".to_string(),
                _ => b.label.to_lowercase(),
            };
            format!("{} {what}", b.key)
        })
        .chain(std::iter::once("Esc close".to_string()))
        .collect();
    for l in wrap_words(&keys.join(" · "), w) {
        put(&mut buf, y, x, &l, Style::default().fg(FAINT).bg(BAR_BG));
        y += 1;
    }
    y - inner.y
}

pub fn draw(f: &mut Frame, area: Rect, app: &App, key: &str) {
    let Some(c) = app.sched_card(key) else {
        crate::ui_chrome::draw_message(
            f,
            area,
            app,
            "loop details",
            &["It is not there any more: it was stopped, deleted or it expired.".into()],
            &[("Close", UiAction::ModalCancel, "Close (Esc)")],
            theme::STONE,
        );
        return;
    };

    let panel = app
        .hits
        .borrow()
        .find(&UiAction::AssistantFocus)
        .map(|p| p.rect)
        .filter(|p| p.width >= 50 && p.height >= 20);
    let area = panel.unwrap_or(area);
    let w = area.width.saturating_sub(4).min(86).max(30.min(area.width));
    let mut scratch = crate::hits::Hits::default();
    let probe = Rect::new(0, 0, w.saturating_sub(2), u16::MAX / 2);
    let need = layout(None, app, &c, probe, &mut scratch) + 2;
    let h = need.min(area.height);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let buf = f.buffer_mut();
    Clear.render(r, buf);
    let title = match c.kind {
        crate::app_jobs::SchedKind::Managed => " job details ",
        _ => " loop details ",
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(kind_color(c.kind)))
        .title(ratatui::text::Span::styled(title, Style::default().fg(FG)))
        .style(Style::default().bg(BAR_BG))
        .render(r, buf);
    let mut hits = app.hits.borrow_mut();

    hits.add(r, UiAction::SchedCardInside, "");
    let inner = Rect::new(
        r.x + 1,
        r.y + 1,
        r.width.saturating_sub(2),
        r.height.saturating_sub(2),
    );
    layout(Some(buf), app, &c, inner, &mut hits);

    let right = r.x + r.width - 1;
    crate::hits::button(
        buf,
        &mut hits,
        app.mouse_pos,
        right.saturating_sub(4),
        r.y,
        right,
        "x",
        UiAction::ModalCancel,
        "Close (Esc)",
        theme::STONE,
    );
}
