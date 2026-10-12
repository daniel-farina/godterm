//! The wheel after a click: a click on a tab in a pane's tab list, or a
//! click inside a pane, never stops the wheel from scrolling that pane's
//! history; the wheel over the tab list scrolls the list.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, term, test_app};
use crate::hits::UiAction;

fn mouse(app: &mut crate::app::App, kind: MouseEventKind, x: u16, y: u16) {
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    )));
}

/// Pane 0's tab with a few hundred lines of history (written straight
/// into its screen, as its PTY reader does).
fn with_history(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    let (mut app, home) = test_app(tag);
    let t = app.panes[0].cur_mut();
    t.state = crate::pane::PaneState::Running;
    let mut p = t.parser.lock().unwrap();
    for i in 1..=400 {
        p.process(format!("line {i}\r\n").as_bytes());
    }
    drop(p);
    (app, home)
}

fn body_point(app: &crate::app::App, p: usize) -> (u16, u16) {
    let r = app.term_rects[p];
    (r.x + r.width / 2, r.y + r.height / 2)
}

fn wheel_up_scrolls(
    app: &mut crate::app::App,
    t: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    what: &str,
) {
    draw(app, t);
    let (x, y) = body_point(app, 0);
    let before = app.panes[0].cur().scroll;
    mouse(app, MouseEventKind::ScrollUp, x, y);
    assert!(
        app.panes[0].cur().scroll > before,
        "{what}: the wheel scrolls the pane ({} -> {})",
        before,
        app.panes[0].cur().scroll
    );
    // Back down to the live view.
    for _ in 0..40 {
        mouse(app, MouseEventKind::ScrollDown, x, y);
    }
    assert_eq!(app.panes[0].cur().scroll, 0, "{what}");
}

#[test]
fn the_wheel_scrolls_after_a_tab_click_and_a_pane_click() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = with_history("wheel");
    let cwd = app.panes[0].cur().cwd.clone();
    app.panes[0].add_tab(cwd);
    app.panes[0].active = 0;
    let mut t = term();
    // Before any click.
    wheel_up_scrolls(&mut app, &mut t, "before a click");
    // After a click on the other tab and back on this one.
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 1));
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 0));
    assert_eq!(app.panes[0].active, 0);
    wheel_up_scrolls(&mut app, &mut t, "after a tab click");
    // The memory tick then trims the tab that went to the background and
    // raises the one clicked: its history stays and the wheel scrolls.
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 1));
    app.mem_ticked = std::time::Instant::now() - std::time::Duration::from_secs(5);
    app.memory_tick();
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 0));
    app.mem_ticked = std::time::Instant::now() - std::time::Duration::from_secs(5);
    app.memory_tick();
    wheel_up_scrolls(&mut app, &mut t, "after a tab click and the memory tick");
    // After a click inside the pane.
    draw(&mut app, &mut t);
    let (x, y) = body_point(&app, 0);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
    wheel_up_scrolls(&mut app, &mut t, "after a pane click");
    // After a click in another pane, then back over this one.
    draw(&mut app, &mut t);
    let (x1, y1) = body_point(&app, 1);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x1, y1);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x1, y1);
    wheel_up_scrolls(&mut app, &mut t, "after a click in another pane");
    // The wheel over the tab list scrolls the list.
    for _ in 0..30 {
        let cwd = app.panes[0].cur().cwd.clone();
        app.panes[0].add_tab(cwd);
    }
    app.panes[0].active = 0;
    app.panes[0].sidebar_scroll = crate::slot::FOLLOW;
    draw(&mut app, &mut t);
    let r = app.last_hits.find(&UiAction::SelectTab(0, 1)).unwrap().rect;
    mouse(&mut app, MouseEventKind::ScrollDown, r.x + 2, r.y);
    assert_ne!(app.panes[0].sidebar_scroll, crate::slot::FOLLOW);
    assert!(app.panes[0].sidebar_scroll > 0, "the list scrolled");
    let _ = std::fs::remove_dir_all(&home);
}
