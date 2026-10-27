use ratatui::style::{Modifier, Style};
use std::time::Duration;

use crate::app::App;
use crate::theme;

pub const HALF_MS: u64 = 500;

pub fn lit_at(now_ms: u64) -> bool {
    (now_ms / HALF_MS).is_multiple_of(2)
}

pub fn now_ms(app: &App) -> u64 {
    app.attn_clock_ms.get().unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

pub fn style_for(waiting: usize, blink: bool, now_ms: u64) -> Option<Style> {
    if waiting == 0 {
        return None;
    }
    let (fg, bg) = if !blink || lit_at(now_ms) {
        (theme::ATTN_TEXT, theme::ATTN_RED)
    } else {
        (theme::ATTN_TEXT_DIM, theme::ATTN_RED_DIM)
    };
    Some(Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD))
}

pub fn approvals_style(app: &App) -> Option<Style> {
    style_for(
        app.waiting_tabs().len(),
        app.cfg.ui.blink_attention,
        now_ms(app),
    )
}

pub fn next_flip(app: &App) -> Option<Duration> {
    if !app.cfg.ui.blink_attention
        || app.voice.term_focused == Some(false)
        || app.waiting_tabs().is_empty()
    {
        return None;
    }
    let left = HALF_MS - now_ms(app) % HALF_MS;
    Some(Duration::from_millis(left.max(10)))
}
