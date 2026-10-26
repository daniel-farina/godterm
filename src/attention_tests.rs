//! The Approvals button asks for attention: red, blinking slowly, on an
//! injected clock (nothing sleeps).

use crate::app::App;
use crate::attention::{approvals_style, lit_at, next_flip, style_for};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::{Activity, Pane};
use crate::theme;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;

fn app(tag: &str) -> App {
    let home = std::env::temp_dir().join(format!("godterm-attn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    App::new(crate::config::Config::default(), tx)
}

fn wait_tabs(app: &mut App, n: usize) {
    app.panes[0].tabs = (0..n).map(|_| Pane::new("/tmp".into())).collect();
    for t in &mut app.panes[0].tabs {
        t.activity = Activity::Permission;
    }
}

fn bg(s: ratatui::style::Style) -> Color {
    s.bg.unwrap()
}

/// The first cell of the "Approvals" label in a drawn bar, with its style.
fn drawn(app: &App) -> (String, ratatui::style::Style, u16) {
    let mut term = Terminal::new(TestBackend::new(120, 1)).unwrap();
    term.draw(|f| crate::ui_chrome::draw_menu_bar(f, app, f.area()))
        .unwrap();
    let buf = term.backend().buffer().clone();
    let row: String = (0..120).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    let at = row.find("Approvals").unwrap();
    let col = row[..at].chars().count() as u16;
    let label: String = row[at..].chars().take(12).collect();
    (label, buf[(col, 0)].style(), col)
}

#[test]
fn zero_waiting_is_the_idle_style() {
    assert!(style_for(0, true, 0).is_none());
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = app("zero");
    assert!(approvals_style(&a).is_none());
    let (label, st, _) = drawn(&a);
    assert_eq!(label, "Approvals  0");
    assert_eq!(st.fg, Some(theme::FAINT), "dim grey as today");
    assert_ne!(st.bg, Some(theme::ATTN_RED));
}

#[test]
fn waiting_is_red() {
    let st = style_for(2, true, 0).unwrap();
    assert_eq!(bg(st), theme::ATTN_RED);
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut a = app("red");
    a.attn_clock_ms.set(Some(0));
    wait_tabs(&mut a, 2);
    let (label, st, _) = drawn(&a);
    assert_eq!(label, "Approvals  2");
    assert_eq!(st.bg, Some(theme::ATTN_RED));
}

#[test]
fn the_blink_alternates_by_the_injected_clock() {
    assert!(lit_at(0) && lit_at(499));
    assert!(!lit_at(500) && !lit_at(999));
    assert!(lit_at(1000));
    let lit = style_for(1, true, 100).unwrap();
    let dim = style_for(1, true, 600).unwrap();
    assert_eq!(bg(lit), theme::ATTN_RED);
    assert_eq!(bg(dim), theme::ATTN_RED_DIM);
    assert_ne!(lit, dim);
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut a = app("blink");
    wait_tabs(&mut a, 1);
    a.attn_clock_ms.set(Some(10_100));
    assert_eq!(drawn(&a).1.bg, Some(theme::ATTN_RED));
    a.attn_clock_ms.set(Some(10_600));
    assert_eq!(drawn(&a).1.bg, Some(theme::ATTN_RED_DIM));
}

#[test]
fn the_quiet_phase_never_matches_the_idle_text_and_stays_readable() {
    let dim = style_for(1, true, 600).unwrap();
    assert_ne!(dim.fg, Some(theme::FAINT));
    assert_ne!(dim.bg, Some(theme::BAR_BG), "never fully invisible");
    assert_ne!(dim.fg, dim.bg);
    let lit = style_for(1, true, 0).unwrap();
    assert_ne!(lit.fg, Some(theme::FAINT));
    assert_ne!(lit.fg, lit.bg);
}

#[test]
fn switched_off_it_is_solid_red() {
    for t in [0, 500, 750, 1234, 99_999] {
        let st = style_for(3, false, t).unwrap();
        assert_eq!(bg(st), theme::ATTN_RED, "at {t}");
    }
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut a = app("off");
    let cfg: crate::config::Config = toml::from_str("[ui]\nblink_attention = false\n").unwrap();
    a.cfg.ui = cfg.ui;
    wait_tabs(&mut a, 1);
    a.attn_clock_ms.set(Some(600));
    assert_eq!(drawn(&a).1.bg, Some(theme::ATTN_RED));
    assert!(next_flip(&a).is_none(), "no redraw timer without blinking");
}

#[test]
fn the_switch_defaults_on_and_is_in_settings() {
    let cfg: crate::config::Config = toml::from_str("").unwrap();
    assert!(cfg.ui.blink_attention);
    let src = include_str!("settings.rs");
    assert!(src.contains("\"ui.blink_attention\""));
}

#[test]
fn label_and_hit_region_do_not_change() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut a = app("hit");
    a.attn_clock_ms.set(Some(0));
    let (idle_label, _, col0) = drawn(&a);
    let hit0 = a
        .hits
        .borrow()
        .at(col0, 0)
        .map(|h| (h.rect, h.action.clone()));
    wait_tabs(&mut a, 1);
    let (label, _, col1) = drawn(&a);
    let hit1 = a
        .hits
        .borrow()
        .at(col1, 0)
        .map(|h| (h.rect, h.action.clone()));
    assert_eq!(col0, col1);
    assert_eq!(&idle_label[..9], &label[..9]);
    assert_eq!(idle_label.chars().count(), label.chars().count());
    assert_eq!(hit0, hit1);
    assert_eq!(hit1.unwrap().1, crate::hits::UiAction::Key('y'));
}

#[test]
fn a_count_change_updates_the_style() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut a = app("count");
    a.attn_clock_ms.set(Some(0));
    assert!(approvals_style(&a).is_none());
    wait_tabs(&mut a, 1);
    assert!(approvals_style(&a).is_some());
    assert_eq!(drawn(&a).0, "Approvals  1");
    wait_tabs(&mut a, 4);
    assert_eq!(drawn(&a).0, "Approvals  4");
    for t in &mut a.panes[0].tabs {
        t.activity = Activity::Ready;
    }
    assert!(approvals_style(&a).is_none());
    assert_eq!(drawn(&a).1.fg, Some(theme::FAINT));
}

#[test]
fn the_timer_runs_only_while_it_blinks() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut a = app("timer");
    a.attn_clock_ms.set(Some(1_200));
    assert!(next_flip(&a).is_none(), "nothing waits");
    wait_tabs(&mut a, 1);
    assert_eq!(next_flip(&a).unwrap().as_millis(), 300);
    a.voice.term_focused = Some(false);
    assert!(next_flip(&a).is_none(), "window not in front");
    a.voice.term_focused = Some(true);
    assert!(next_flip(&a).is_some());
}

#[test]
fn the_red_is_muted_not_neon() {
    for c in [
        theme::ATTN_RED,
        theme::ATTN_RED_DIM,
        theme::ATTN_GLYPH,
        theme::ATTN_GLYPH_DIM,
    ] {
        let Color::Rgb(r, g, b) = c else { panic!() };
        let max = r.max(g).max(b) as f32;
        let min = r.min(g).min(b) as f32;
        let sat = (max - min) / max;
        assert!(r > g && r > b, "a red");
        assert!(max <= 215.0, "not bright: {c:?}");
        assert!(sat <= 0.65, "not saturated: {c:?} {sat}");
    }
}
