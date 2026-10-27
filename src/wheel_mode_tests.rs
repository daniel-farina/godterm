use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, term, test_app};
use crate::hits::UiAction;

const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1006h";
const ALT_ON: &[u8] = b"\x1b[?1049h";

fn mouse(app: &mut crate::app::App, kind: MouseEventKind, x: u16, y: u16) {
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    )));
}

fn pane_with(tag: &str, history: usize, modes: &[u8]) -> (crate::app::App, std::path::PathBuf) {
    let (mut app, home) = test_app(tag);
    let t = app.panes[0].cur_mut();
    t.state = crate::pane::PaneState::Running;
    let mut p = t.parser.lock().unwrap();
    for i in 1..=history {
        p.process(format!("line {i}\r\n").as_bytes());
    }
    p.process(modes);
    drop(p);
    t.last_input = Instant::now() - Duration::from_secs(3600);
    (app, home)
}

fn body(app: &crate::app::App) -> (u16, u16) {
    let r = app.term_rects[0];
    (r.x + r.width / 2, r.y + r.height / 2)
}

fn wrote(_app: &crate::app::App) -> bool {
    crate::pane::SENT.with(|s| !s.borrow().is_empty())
}

fn click_pane(app: &mut crate::app::App, t: &mut ratatui::Terminal<ratatui::backend::TestBackend>) {
    draw(app, t);
    let (x, y) = body(app);
    mouse(app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(app, MouseEventKind::Up(MouseButton::Left), x, y);
    draw(app, t);
}

#[test]
fn a_pane_that_reports_the_mouse_on_the_main_screen_still_scrolls_its_history() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = pane_with("wheel-main-mouse", 400, MOUSE_ON);
    let mut t = term();
    click_pane(&mut app, &mut t);
    let (x, y) = body(&app);

    app.panes[0].cur_mut().last_input = Instant::now() - Duration::from_secs(3600);
    crate::pane::SENT.with(|s| s.borrow_mut().clear());
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    assert!(
        app.panes[0].cur().scroll > 0,
        "the wheel scrolls the history right after a click"
    );
    assert!(!wrote(&app), "and tells the agent nothing");
    for _ in 0..80 {
        mouse(&mut app, MouseEventKind::ScrollDown, x, y);
    }
    assert_eq!(app.panes[0].cur().scroll, 0);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_wheel_scrolls_right_after_selecting_a_tab_with_the_mouse_on() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = pane_with("wheel-tab-select", 400, MOUSE_ON);
    let cwd = app.panes[0].cur().cwd.clone();
    app.panes[0].add_tab(cwd);
    app.panes[0].active = 0;
    let mut t = term();
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 1));
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 0));
    draw(&mut app, &mut t);
    let (x, y) = body(&app);
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    assert!(app.panes[0].cur().scroll > 0, "no typing needed first");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_agent_on_the_alternate_screen_with_the_mouse_on_gets_the_wheel() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut modes = MOUSE_ON.to_vec();
    modes.extend_from_slice(ALT_ON);
    let (mut app, home) = pane_with("wheel-alt-mouse", 0, &modes);
    let mut t = term();
    draw(&mut app, &mut t);
    let (x, y) = body(&app);
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    assert!(wrote(&app), "forwarded to the agent as a mouse event");
    assert_eq!(app.panes[0].cur().scroll, 0);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_agent_on_the_alternate_screen_without_the_mouse_gets_arrow_keys() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = pane_with("wheel-alt-keys", 0, ALT_ON);
    let mut t = term();
    draw(&mut app, &mut t);
    let (x, y) = body(&app);
    assert!(!wrote(&app));
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    assert!(wrote(&app), "the wheel is not lost: it goes in as Up keys");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_main_screen_agent_with_the_mouse_on_and_no_history_still_gets_the_wheel() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = pane_with("wheel-main-nohistory", 0, MOUSE_ON);
    let mut t = term();
    draw(&mut app, &mut t);
    let (x, y) = body(&app);
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    assert!(wrote(&app), "nothing of ours to scroll: the agent decides");
    assert_eq!(app.panes[0].cur().scroll, 0);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_plain_pane_scrolls_as_before_and_a_drag_still_selects() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = pane_with("wheel-plain", 400, b"");
    let mut t = term();
    click_pane(&mut app, &mut t);
    let (x, y) = body(&app);
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    assert!(app.panes[0].cur().scroll > 0);
    assert!(!wrote(&app));

    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x + 6, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 6, y);
    assert!(app.selection.as_ref().is_some_and(|s| s.moved));
    let _ = std::fs::remove_dir_all(&home);
}
