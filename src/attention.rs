//! Something needs your yes: the Approvals button turns brick red and
//! blinks slowly (500 ms lit, 500 ms quiet, never invisible). The phase
//! comes from a millisecond clock the tests can set, nothing sleeps, and
//! the redraw timer wakes only at a phase change, only while something
//! waits, and not at all while the terminal window is not in front.

use ratatui::style::{Modifier, Style};
use std::time::Duration;

use crate::app::App;
use crate::theme;

/// One half period of the blink.
pub const HALF_MS: u64 = 500;

/// Is the lit half showing at `now_ms`?
pub fn lit_at(now_ms: u64) -> bool {
    (now_ms / HALF_MS).is_multiple_of(2)
}

/// The clock for the blink: wall clock milliseconds, or the test's.
pub fn now_ms(app: &App) -> u64 {
    app.attn_clock_ms.get().unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// The pure style: None when nothing waits; solid lit red when the blink is
/// off; else lit or quiet red by the clock.
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

/// The style of the Approvals button right now.
pub fn approvals_style(app: &App) -> Option<Style> {
    style_for(
        app.waiting_tabs().len(),
        app.cfg.ui.blink_attention,
        now_ms(app),
    )
}

/// Time to the next phase change, only while it blinks: something waits,
/// the switch is on and the terminal is not known to be in the background.
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
