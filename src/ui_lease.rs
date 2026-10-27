use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app_lease::{active_words, TabLease};
use crate::pane::Pane;
use crate::theme::{self, BAR_BG, DIM, FG};

pub fn draw(f: &mut Frame, pane: &Pane, area: Rect) -> bool {
    let Some(l) = &pane.lease else {
        return false;
    };
    let key = |k: &str, w: &str| {
        vec![
            Span::styled(
                format!(" {k} "),
                Style::default().fg(BAR_BG).bg(theme::STONE),
            ),
            Span::styled(format!(" {w}   "), Style::default().fg(FG)),
        ]
    };
    let mut lines: Vec<Line> = vec![Line::raw("")];
    match l {
        TabLease::Waiting { .. } => {
            lines.push(Line::styled(
                "  Checking who owns this conversation, then pulling its latest copy...",
                Style::default().fg(DIM),
            ));
        }
        TabLease::ReadOnly(r) => {
            let now = chrono::Utc::now().timestamp_millis();
            let head = if r.resumable && r.note.is_some() {
                String::new()
            } else if r.beat_ms > 0 {
                format!("Owned by {}, {}", r.label, active_words(r.beat_ms, now))
            } else if !r.label.is_empty() {
                format!("Owned by {}", r.label)
            } else {
                String::new()
            };
            if !head.is_empty() {
                lines.push(Line::styled(
                    format!("  {head}"),
                    Style::default().fg(FG).add_modifier(Modifier::BOLD),
                ));
            }
            if let Some(n) = &r.note {
                lines.push(Line::styled(
                    format!("  {n}"),
                    Style::default().fg(theme::SAND),
                ));
            }
            if r.offline {
                lines.push(Line::styled(
                    "  Offline: as last seen",
                    Style::default().fg(theme::CLAY),
                ));
            }
            if r.requested {
                lines.push(Line::styled(
                    format!(
                        "  Asked {} for it: it comes here once that turn ends",
                        r.label
                    ),
                    Style::default().fg(DIM),
                ));
            }
            if let Some(b) = &r.busy {
                lines.push(Line::styled(format!("  {b}"), Style::default().fg(DIM)));
            }
            lines.push(Line::raw(""));
            if r.confirm {
                lines.push(Line::styled(
                    format!("  Take over now? {} stops writing it; anything it never pushed becomes a fork there. y takes it, any other key keeps it as is", r.label),
                    Style::default().fg(theme::SAND),
                ));
            } else {
                let mut row = vec![Span::raw("  ")];
                if r.resumable {
                    row.extend(key("Enter", "Resume here"));
                } else {
                    if !r.requested {
                        row.extend(key("r", "Request take over"));
                    }
                    row.extend(key("T", "Take over now"));
                }
                if r.offline && !r.resumable {
                    row.extend(key("o", "Work offline anyway"));
                }
                if let Some(fk) = &r.fork {
                    row.extend(key(
                        "f",
                        &format!("Open fork {}", crate::paths::head(fk, 8)),
                    ));
                }
                lines.push(Line::from(row));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "  Read only: the conversation as synced",
                Style::default().fg(theme::FAINT),
            ));
            for (u, a) in &r.turns {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("  > {}", one_line(u, 400)),
                    Style::default().fg(theme::SLATE),
                ));
                if !a.is_empty() {
                    lines.push(Line::styled(
                        format!("    {}", one_line(a, 900)),
                        Style::default().fg(FG),
                    ));
                }
            }
        }
        _ => return false,
    }
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    true
}

fn one_line(s: &str, max: usize) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::paths::head(&t, max).to_string()
}

pub fn banner(f: &mut Frame, pane: &Pane, area: Rect) {
    let Some(msg) = pane.lease.as_ref().and_then(|l| l.banner()) else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let w = (msg.chars().count() as u16).min(area.width);
    let r = Rect::new(area.x, area.y, w, 1);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(msg).style(Style::default().fg(BAR_BG).bg(theme::SAND)),
        r,
    );
}
