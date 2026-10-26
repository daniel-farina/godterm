//! Settings > Sync: the state repo, the last sync, what waits, machines,
//! conversation repos, warnings (projects not backed up among them).
//! Read only; `godterm sync now` or the assistant's sync_now syncs.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .sync_lines()
        .into_iter()
        .map(|l| {
            let warn = l.starts_with("not backed up")
                || l.starts_with("warning")
                || l.starts_with("error")
                || l.contains("failed");
            let color = if warn { theme::CLAY } else { theme::FG };
            Line::from(Span::styled(format!("  {l}"), Style::default().fg(color)))
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        ),
    );
}
