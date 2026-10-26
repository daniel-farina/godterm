//! The tab panel (double click on a tab): Move on top, the tab's options
//! below; the right click menu lists the same options.

use crate::app::Modal;
use crate::app_tabopts::TabAct;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, term, test_app};
use crate::hits::UiAction;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(app: &mut crate::app::App, c: KeyCode) {
    app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
}

fn text(t: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let b = t.backend().buffer();
    let mut s = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            s.push_str(b[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn double_click_tab(
    app: &mut crate::app::App,
    t: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    s: usize,
    i: usize,
) {
    draw(app, t);
    click(app, &UiAction::SelectTab(s, i));
    click(app, &UiAction::SelectTab(s, i));
}

#[test]
fn double_click_opens_move_and_tab_sections() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("tabopt-open");
    app.panes[0].add_tab(home.clone());
    let mut t = term();
    double_click_tab(&mut app, &mut t, 0, 1);
    assert!(
        matches!(&app.modal, Modal::MoveTab(p) if p.slot == 0 && p.tab == 1 && p.opt.is_none()),
        "{:?}",
        app.modal
    );
    draw(&mut app, &mut t);
    let s = text(&t);
    for want in [
        "Move to another account",
        "Tab",
        "Name",
        "Session title",
        "Color",
        "Pin",
        "Group",
        "Duplicate to...",
        "Copy session to...",
        "Close tab",
    ] {
        assert!(s.contains(want), "{want} missing:\n{s}");
    }
    // The right click menu offers the same list.
    let menu: Vec<TabAct> = app.tab_actions(0, 1).iter().map(|r| r.act).collect();
    let panel: Vec<TabAct> = app.panel_rows(0, 1).iter().map(|r| r.act).collect();
    assert_eq!(
        menu.iter()
            .filter(|a| **a != TabAct::Move && **a != TabAct::Git)
            .count(),
        panel.len()
    );
    app.modal = Modal::None;
    app.open_tab_menu(0, 1);
    draw(&mut app, &mut t);
    let s = text(&t);
    for r in app.tab_actions(0, 1) {
        assert!(s.contains(&r.label), "{} missing from the menu", r.label);
    }
    // A click on a menu row runs it: Pin.
    let pin = app
        .tab_actions(0, 1)
        .iter()
        .position(|r| r.act == TabAct::Pin)
        .unwrap();
    click(&mut app, &UiAction::TabMenuItem(pin));
    assert!(app.panes[0].tabs[1].pinned);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn rename_color_pin_group_and_close_from_the_panel() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("tabopt-act");
    app.panes[0].add_tab(home.clone());
    let mut t = term();
    double_click_tab(&mut app, &mut t, 0, 1);
    draw(&mut app, &mut t);
    // Rename: click the field, type, Enter saves; Esc cancels another edit.
    click(&mut app, &UiAction::MoveName);
    for _ in 0..80 {
        key(&mut app, KeyCode::Backspace);
    }
    for c in "api".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panes[0].tabs[1].name(), "api");
    assert!(matches!(app.modal, Modal::MoveTab(_)), "the panel stays");
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::MoveName);
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::Esc);
    assert!(matches!(&app.modal, Modal::MoveTab(p) if p.name == "api" && !p.name_edit));
    assert_eq!(app.panes[0].tabs[1].name(), "api");
    // Color: a swatch click (sand is the 2nd swatch), then none by keys.
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::TabOptColor(2));
    assert_eq!(app.panes[0].tabs[1].accent.as_deref(), Some("sand"));
    assert!(matches!(app.modal, Modal::MoveTab(_)));
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panes[0].tabs[1].accent, None);
    // Pin toggles, by keys: down to the Pin row, Enter, Enter.
    let rows = app.panel_rows(0, 1);
    let pin = rows.iter().position(|r| r.act == TabAct::Pin).unwrap();
    if let Modal::MoveTab(p) = &mut app.modal {
        p.opt = Some(pin.saturating_sub(1));
    }
    key(&mut app, KeyCode::Down);
    assert!(matches!(&app.modal, Modal::MoveTab(p) if p.opt == Some(pin)));
    key(&mut app, KeyCode::Enter);
    assert!(app.panes[0].tabs[1].pinned);
    key(&mut app, KeyCode::Enter);
    assert!(!app.panes[0].tabs[1].pinned);
    // Group: put it in an existing group by a click.
    let uid0 = app.panes[0].tabs[0].uid;
    app.add_to_group(0, &[uid0], "web", None);
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::TabOptGroup(0));
    assert_eq!(app.panes[0].tabs[1].group.as_deref(), Some("web"));
    assert!(matches!(app.modal, Modal::MoveTab(_)));
    // New group... asks for a name.
    let n = app.group_choices(0).len();
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::TabOptGroup(n - 2));
    assert!(matches!(app.modal, Modal::GroupName { .. }));
    // Close goes through the confirm path: a pinned tab asks first.
    app.modal = Modal::None;
    app.panes[0].tabs[1].pinned = true;
    double_click_tab(&mut app, &mut t, 0, 1);
    let close = app
        .panel_rows(0, 1)
        .iter()
        .position(|r| r.act == TabAct::Close)
        .unwrap();
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::TabOpt(close));
    assert_eq!(app.modal, Modal::ConfirmClose);
    assert_eq!(app.panes[0].tabs.len(), 2, "not closed before the yes");
    app.modal = Modal::None;
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_panel_fits_80x24() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("tabopt-small");
    app.panes[0].add_tab(home.clone());
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    app.open_tab_panel(0, 1);
    draw(&mut app, &mut t);
    let fits = |app: &crate::app::App, h: u16| {
        app.last_hits
            .regions
            .iter()
            .all(|r| r.rect.x + r.rect.width <= 80 && r.rect.y + r.rect.height <= h)
    };
    assert!(fits(&app, 24));
    let m = app.panel_rows(0, 1).len();
    // 80x24 holds both sections here: every row is on screen.
    assert!(app.last_hits.find(&UiAction::TabOpt(m - 1)).is_some());
    assert!(app.last_hits.find(&UiAction::TabPanelSec(true)).is_none());
    // Shorter: the sections become tabs.
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 16)).unwrap();
    let on_screen = |app: &crate::app::App| fits(app, 16);
    draw(&mut app, &mut t);
    assert!(on_screen(&app));
    assert!(app.last_hits.find(&UiAction::TabPanelSec(true)).is_some());
    // Tab walks down into the options and the list scrolls to the focus.
    let n = app.move_targets(0).len();
    for _ in 0..(n + m + 2) {
        key(&mut app, KeyCode::Tab);
    }
    assert!(matches!(&app.modal, Modal::MoveTab(p) if p.opt == Some(m - 1)));
    draw(&mut app, &mut t);
    assert!(on_screen(&app));
    assert!(app.last_hits.find(&UiAction::TabOpt(m - 1)).is_some());
    assert!(text(&t).contains("Close tab"));
    // The section tab goes back to Move.
    click(&mut app, &UiAction::TabPanelSec(false));
    assert!(matches!(&app.modal, Modal::MoveTab(p) if p.opt.is_none()));
    draw(&mut app, &mut t);
    assert!(on_screen(&app));
    // Tiny terminals draw nothing off screen either.
    let mut tiny = ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 8)).unwrap();
    draw(&mut app, &mut tiny);
    let _ = std::fs::remove_dir_all(&home);
}
