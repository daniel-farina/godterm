//! A pane header's window controls (minimize, maximize, close at the far
//! right) and its sideways collapse (the caret at the top left), with the
//! row and column strips they make.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use serde_json::json;

use crate::app::{App, Modal};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::grid_layout::{Fold, COL_W};
use crate::hits::UiAction;

fn test_app(tag: &str) -> (App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-pctl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.save().unwrap();
    let mut app = App::new(cfg, tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    (app, home)
}

type Term = ratatui::Terminal<ratatui::backend::TestBackend>;

fn term(w: u16, h: u16) -> Term {
    ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap()
}

fn draw(app: &mut App, t: &mut Term) {
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
}

fn cell(t: &Term, x: u16, y: u16) -> String {
    t.backend().buffer()[(x, y)].symbol().to_string()
}

/// The topmost region with this action.
fn region(app: &App, a: &UiAction) -> Rect {
    app.last_hits
        .regions
        .iter()
        .rev()
        .find(|r| &r.action == a)
        .unwrap_or_else(|| panic!("{a:?} is not on screen"))
        .rect
}

fn click_at(app: &mut App, x: u16, y: u16) {
    for kind in [
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
    ] {
        app.on_mouse_event(crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }
}

/// A control on pane `i`'s header row (the status bar has a zoom too).
fn ctl_region(app: &App, i: usize, a: &UiAction) -> Rect {
    let pane = app.pane_rects[i];
    app.last_hits
        .regions
        .iter()
        .rev()
        .find(|r| {
            &r.action == a
                && r.rect.y == pane.y
                && r.rect.x >= pane.x
                && r.rect.x < pane.x + pane.width
        })
        .unwrap_or_else(|| panic!("{a:?} is not on pane {i}'s header"))
        .rect
}

fn click_ctl(app: &mut App, i: usize, a: &UiAction) {
    let r = ctl_region(app, i, a);
    app.last_click = None;
    click_at(app, r.x + 1, r.y);
}

fn click(app: &mut App, a: &UiAction) {
    let r = region(app, a);
    // Two clicks in a row on one target read as a double click.
    app.last_click = None;
    click_at(app, r.x + r.width / 2, r.y);
}

/// The three controls of pane `i`: in order, 3 cells each, ending at the
/// header's right border; their glyphs.
fn controls(app: &App, t: &Term, i: usize, strip: bool) -> [String; 3] {
    let a = [
        UiAction::FoldAccount(i),
        UiAction::Zoom(i),
        UiAction::CloseWindow(i),
    ];
    let pane = app.pane_rects[i];
    let r: Vec<Rect> = a.iter().map(|x| ctl_region(app, i, x)).collect();
    for k in 0..3 {
        assert_eq!(r[k].width, 3, "{:?}", a[k]);
        assert_eq!(r[k].y, pane.y);
        if k > 0 {
            assert_eq!(r[k].x, r[k - 1].x + 3);
        }
    }
    let end = pane.x + pane.width - if strip { 0 } else { 1 };
    assert_eq!(r[2].x + 3, end, "far right");
    [0, 1, 2].map(|k| cell(t, r[k].x + 1, r[k].y))
}

#[test]
fn the_window_controls_sit_at_the_right_and_click() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("ctl");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    assert_eq!(controls(&app, &t, 0, false), ["_", "□", "×"]);
    // The old marks are gone: no zoom word, no ▾ fold link (minimize is "_").
    let row: String = (0..200).map(|x| cell(&t, x, 0)).collect();
    assert!(!row.contains("zoom"), "{row}");
    assert!(region(&app, &UiAction::FoldSideways(0)).x <= app.pane_rects[0].x + 2);
    // Maximize zooms; the glyph turns to restore; a click restores.
    click_ctl(&mut app, 0, &UiAction::Zoom(0));
    assert!(app.zoom);
    draw(&mut app, &mut t);
    assert_eq!(controls(&app, &t, 0, false)[1], "▣");
    click_ctl(&mut app, 0, &UiAction::Zoom(0));
    assert!(!app.zoom);
    // Minimize: a row strip with restore, maximize and close at its end.
    draw(&mut app, &mut t);
    click_ctl(&mut app, 0, &UiAction::FoldAccount(0));
    assert_eq!(app.account_fold(0), Fold::Row);
    draw(&mut app, &mut t);
    assert_eq!(app.pane_rects[0].height, 1);
    assert_eq!(controls(&app, &t, 0, true), ["▭", "□", "×"]);
    // Restore from the strip's control.
    click_ctl(&mut app, 0, &UiAction::FoldAccount(0));
    assert_eq!(app.account_fold(0), Fold::Open);
    // A click anywhere on a strip expands it too.
    draw(&mut app, &mut t);
    click_ctl(&mut app, 1, &UiAction::FoldAccount(1));
    draw(&mut app, &mut t);
    let s = app.pane_rects[1];
    click_at(&mut app, s.x + 4, s.y);
    assert_eq!(app.account_fold(1), Fold::Open);
    // Close keeps its confirmation.
    draw(&mut app, &mut t);
    click_ctl(&mut app, 0, &UiAction::CloseWindow(0));
    assert_eq!(app.modal, Modal::ConfirmCloseWindow(0));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn narrow_headers_keep_the_controls() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("narrow");
    for (w, h) in [(80u16, 24u16), (120, 30)] {
        let mut t = term(w, h);
        draw(&mut app, &mut t);
        for &i in &app.visible_panes() {
            if app.pane_rects[i].width == 0 {
                continue;
            }
            assert_eq!(controls(&app, &t, i, false), ["_", "□", "×"], "{w}x{h}");
        }
    }
    // Zoomed at 80x24 too.
    app.zoom = true;
    let mut t = term(80, 24);
    draw(&mut app, &mut t);
    let f = app.focus;
    assert_eq!(controls(&app, &t, f, false)[1], "▣");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn sideways_collapse_strip_persistence_and_voice() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("side");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    // A 2x2: pane 0 top left, pane 1 top right.
    let (r0, r1) = (app.pane_rects[0], app.pane_rects[1]);
    assert_eq!(r0.y, r1.y);
    let row_w = r0.width + r1.width;
    // The caret at the top left collapses sideways.
    click(&mut app, &UiAction::FoldSideways(0));
    assert_eq!(app.account_fold(0), Fold::Col);
    draw(&mut app, &mut t);
    let (s0, n1) = (app.pane_rects[0], app.pane_rects[1]);
    assert_eq!(s0.width, COL_W);
    assert_eq!(s0.height, r0.height);
    // Its row sibling takes the whole rest of the row.
    assert_eq!(n1.x, s0.x + COL_W);
    assert_eq!(n1.width, row_w - COL_W);
    // The strip: caret, number, dot, tab count, top to bottom.
    let col: Vec<String> = (0..6).map(|k| cell(&t, s0.x + 1, s0.y + k)).collect();
    assert_eq!(col[0], "▸", "{col:?}");
    assert_eq!(col[1], "1");
    assert_eq!(col[2], "●");
    let all: String = (0..6)
        .map(|k| {
            (0..COL_W - 1)
                .map(|x| cell(&t, s0.x + x, s0.y + k))
                .collect::<String>()
        })
        .collect();
    assert!(all.contains("1t"), "{all}");
    // The PTY keeps its size.
    let before = app.panes[0].tabs[0].pty_size();
    draw(&mut app, &mut t);
    assert_eq!(app.panes[0].tabs[0].pty_size(), before);
    // Kept in config.toml, apart from the row list; back after a restart.
    let cfg = crate::config::Config::load_or_init().unwrap();
    assert_eq!(
        cfg.collapsed_sideways,
        vec![app.cfg.accounts[0].name.clone()]
    );
    assert!(cfg.collapsed_accounts.is_empty());
    let (tx, _rx) = std::sync::mpsc::channel();
    let again = App::new(cfg, tx);
    assert_eq!(again.pane_fold(0), Fold::Col);
    // Both directions at once: account 4 minimized by voice, account 2
    // sideways by voice.
    app.control_call("collapse_accounts", &json!({"accounts": 4}));
    let v = app.control_call(
        "collapse_accounts",
        &json!({"accounts": 2, "direction": "column"}),
    );
    assert!(
        v["result"]["say"].as_str().unwrap().contains("sideways"),
        "{v}"
    );
    assert_eq!(app.account_fold(3), Fold::Row);
    assert_eq!(app.account_fold(1), Fold::Col);
    draw(&mut app, &mut t);
    assert_eq!(app.pane_rects[1].width, COL_W);
    assert_eq!(app.pane_rects[3].height, 1);
    for a in 0..4 {
        for b in 0..4 {
            if a != b {
                let (x, y) = (app.pane_rects[a], app.pane_rects[b]);
                assert_eq!(x.intersection(y).area(), 0, "{a} {b}: {x:?} {y:?}");
            }
        }
    }
    // A row strip folded the other way goes sideways (one list at a time).
    app.control_call(
        "collapse_accounts",
        &json!({"accounts": 4, "direction": "column"}),
    );
    assert_eq!(app.account_fold(3), Fold::Col);
    assert!(app.cfg.collapsed_accounts.is_empty());
    // Click anywhere on a column strip expands it.
    draw(&mut app, &mut t);
    let s = app.pane_rects[0];
    click_at(&mut app, s.x + 1, s.y + s.height - 2);
    assert_eq!(app.account_fold(0), Fold::Open);
    // Ctrl-a \ toggles the focused pane.
    app.focus = 2;
    app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE));
    assert_eq!(app.account_fold(2), Fold::Col);
    // The strips at 80x24: on screen, the caret on top.
    let mut small = term(80, 24);
    draw(&mut app, &mut small);
    for &i in &[1usize, 2, 3] {
        let r = app.pane_rects[i];
        assert!(r.x + r.width <= 80 && r.y + r.height <= 24, "{i}: {r:?}");
        assert_eq!(r.width, COL_W);
        assert_eq!(cell(&small, r.x + 1, r.y), "▸");
    }
    let open = app.pane_rects[0];
    assert!(open.width > 60, "the open pane takes the width: {open:?}");
    // The View menu offers it.
    let rows = crate::menus::entries(&app, crate::menus::MenuId::View);
    assert!(rows
        .iter()
        .any(|e| e.label.contains("Expand") || e.label.contains("sideways")));
    let _ = std::fs::remove_dir_all(&home);
}
