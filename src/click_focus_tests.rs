//! Clicks outside the assistant panel take the keys from it: a tab in a
//! tab list, a pane, the menu bar. Only a click inside its own rect
//! (docked or floating, this frame's) focuses it, and a click on the
//! floating panel never reaches the pane beneath.

use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, term, test_app};
use crate::hits::UiAction;

fn open_panel(app: &mut crate::app::App, mode: &str) {
    app.cfg.assistant.panel = mode.into();
    app.assistant.show = true;
    app.assistant.focused = true;
}

fn type_x(app: &mut crate::app::App) {
    app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    ));
}

#[test]
fn a_tab_click_selects_it_and_takes_the_keys() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for mode in ["docked", "overlay"] {
        let (mut app, home) = test_app("tabclick");
        let cwd = app.panes[0].cur().cwd.clone();
        app.panes[0].add_tab(cwd);
        app.panes[0].active = 0;
        app.focus = 1;
        open_panel(&mut app, mode);
        let mut t = term();
        draw(&mut app, &mut t);
        assert!(app.assistant_has_focus());
        click(&mut app, &UiAction::SelectTab(0, 1));
        assert_eq!((app.focus, app.panes[0].active), (0, 1), "{mode}");
        assert!(
            !app.assistant_has_focus(),
            "{mode}: the keys left the panel"
        );
        assert!(app.assistant.show, "the panel stays open");
        // Typed keys and pastes go to the session, not the panel input.
        type_x(&mut app);
        app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Paste(
            "pasted".into(),
        )));
        assert!(app.assistant.input.is_empty(), "{mode}");
        // Its border dims (rounded, not the thick focused one).
        draw(&mut app, &mut t);
        let r = app.last_hits.find(&UiAction::AssistantFocus).unwrap().rect;
        assert_eq!(t.backend().buffer()[(r.x, r.y)].symbol(), "╭", "{mode}");
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn pane_and_panel_clicks_focus_what_was_clicked() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for mode in ["docked", "overlay"] {
        let (mut app, home) = test_app("paneclick");
        app.focus = 1;
        open_panel(&mut app, mode);
        let mut t = term();
        draw(&mut app, &mut t);
        // A pane: it gets the keys.
        let p = app.pane_rects[0];
        app.on_mouse_event(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: p.x + p.width / 2,
            row: p.y + p.height / 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(app.focus, 0, "{mode}");
        assert!(!app.assistant_has_focus(), "{mode}");
        // The menu bar too.
        app.assistant.focused = true;
        draw(&mut app, &mut t);
        click(&mut app, &UiAction::Home);
        assert!(!app.assistant_has_focus(), "{mode}: menu bar");
        // Inside the panel: the panel.
        draw(&mut app, &mut t);
        let pr = app.last_hits.find(&UiAction::AssistantFocus).unwrap().rect;
        app.on_mouse_event(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: pr.x + pr.width / 2,
            row: pr.y + pr.height / 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(app.assistant_has_focus(), "{mode}");
        type_x(&mut app);
        assert_eq!(
            app.assistant.input,
            "x",
            "{mode}: view {:?} modal {:?} history {}",
            app.view,
            app.modal,
            app.assistant.history.is_some()
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn the_floating_panel_keeps_its_clicks() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("overlayclick");
    app.focus = 0;
    open_panel(&mut app, "overlay");
    app.assistant.focused = false;
    let mut t = term();
    draw(&mut app, &mut t);
    let panel = app.last_hits.find(&UiAction::AssistantFocus).unwrap().rect;
    // A point of the panel over a pane.
    let (i, pr) = app
        .pane_rects
        .iter()
        .enumerate()
        .find(|(_, r)| r.x + r.width > panel.x + 4 && r.width > 0)
        .map(|(i, r)| (i, *r))
        .expect("a pane under the floating panel");
    assert_ne!(i, app.focus);
    let (cx, cy) = (panel.x + 4, pr.y + pr.height / 2);
    app.on_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: cx,
        row: cy,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert_eq!(app.focus, 0, "the pane beneath got nothing");
    assert!(app.assistant_has_focus(), "the panel took it");
    let _ = std::fs::remove_dir_all(&home);
}
