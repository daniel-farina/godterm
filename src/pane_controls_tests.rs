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

    app.last_click = None;
    click_at(app, r.x + r.width / 2, r.y);
}

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

    let row: String = (0..200).map(|x| cell(&t, x, 0)).collect();
    assert!(!row.contains("zoom"), "{row}");
    assert!(region(&app, &UiAction::FoldSideways(0)).x <= app.pane_rects[0].x + 2);

    click_ctl(&mut app, 0, &UiAction::Zoom(0));
    assert!(app.zoom);
    draw(&mut app, &mut t);
    assert_eq!(controls(&app, &t, 0, false)[1], "▣");
    click_ctl(&mut app, 0, &UiAction::Zoom(0));
    assert!(!app.zoom);

    draw(&mut app, &mut t);
    click_ctl(&mut app, 0, &UiAction::FoldAccount(0));
    assert_eq!(app.account_fold(0), Fold::Row);
    draw(&mut app, &mut t);
    assert_eq!(app.pane_rects[0].height, 1);
    assert_eq!(controls(&app, &t, 0, true), ["▭", "□", "×"]);

    click_ctl(&mut app, 0, &UiAction::FoldAccount(0));
    assert_eq!(app.account_fold(0), Fold::Open);

    draw(&mut app, &mut t);
    click_ctl(&mut app, 1, &UiAction::FoldAccount(1));
    draw(&mut app, &mut t);
    let s = app.pane_rects[1];
    click_at(&mut app, s.x + 4, s.y);
    assert_eq!(app.account_fold(1), Fold::Open);

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

    let (r0, r1) = (app.pane_rects[0], app.pane_rects[1]);
    assert_eq!(r0.y, r1.y);
    let row_w = r0.width + r1.width;

    click(&mut app, &UiAction::FoldSideways(0));
    assert_eq!(app.account_fold(0), Fold::Col);
    draw(&mut app, &mut t);
    let (s0, n1) = (app.pane_rects[0], app.pane_rects[1]);
    assert_eq!(s0.width, COL_W);
    assert_eq!(s0.height, r0.height);

    assert_eq!(n1.x, s0.x + COL_W);
    assert_eq!(n1.width, row_w - COL_W);

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

    let before = app.panes[0].tabs[0].pty_size();
    draw(&mut app, &mut t);
    assert_eq!(app.panes[0].tabs[0].pty_size(), before);

    let cfg = crate::config::Config::load_or_init().unwrap();
    assert_eq!(
        cfg.collapsed_sideways,
        vec![app.cfg.accounts[0].name.clone()]
    );
    assert!(cfg.collapsed_accounts.is_empty());
    let (tx, _rx) = std::sync::mpsc::channel();
    let again = App::new(cfg, tx);
    assert_eq!(again.pane_fold(0), Fold::Col);

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

    app.control_call(
        "collapse_accounts",
        &json!({"accounts": 4, "direction": "column"}),
    );
    assert_eq!(app.account_fold(3), Fold::Col);
    assert!(app.cfg.collapsed_accounts.is_empty());

    draw(&mut app, &mut t);
    let s = app.pane_rects[0];
    click_at(&mut app, s.x + 1, s.y + s.height - 2);
    assert_eq!(app.account_fold(0), Fold::Open);

    app.focus = 2;
    app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE));
    assert_eq!(app.account_fold(2), Fold::Col);

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

    let rows = crate::menus::entries(&app, crate::menus::MenuId::View);
    assert!(rows
        .iter()
        .any(|e| e.label.contains("Expand") || e.label.contains("sideways")));
    let _ = std::fs::remove_dir_all(&home);
}

fn caret(app: &App, i: usize, grow: bool) -> Option<Rect> {
    let pane = app.pane_rects[i];
    app.last_hits
        .regions
        .iter()
        .rev()
        .find(|r| {
            r.action == UiAction::PaneWidth(i, grow)
                && r.rect.y == pane.y
                && r.rect.x >= pane.x
                && r.rect.x < pane.x + pane.width
        })
        .map(|r| r.rect)
}

fn click_caret(app: &mut App, i: usize, grow: bool) {
    let r = caret(app, i, grow).unwrap_or_else(|| panic!("no caret on pane {i}"));
    app.last_click = None;
    click_at(app, r.x + r.width - 1, r.y);
}

#[test]
fn width_carets_sit_before_the_controls_and_resize_per_shape() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("carets");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    let (n, w) = (
        caret(&app, 0, false).unwrap(),
        caret(&app, 0, true).unwrap(),
    );
    let close = ctl_region(&app, 0, &UiAction::FoldAccount(0));

    assert_eq!((n.width, w.width), (3, 3));
    assert_eq!(w.x, n.x + 3);
    assert_eq!(w.x + 3 + 1, close.x);
    assert_eq!(cell(&t, n.x + 1, n.y), "<");
    assert_eq!(cell(&t, w.x + 1, w.y), ">");
    let hint = app
        .last_hits
        .regions
        .iter()
        .find(|r| r.action == UiAction::PaneWidth(0, true))
        .unwrap()
        .hint
        .clone();
    assert!(hint.starts_with("Wider: "), "{hint}");
    let row = app.pane_rects[0].width + app.pane_rects[1].width;
    let before = app.pane_rects[0].width;
    click_caret(&mut app, 0, true);

    assert!(app.selection.is_none());
    assert_eq!(app.focus, 0);
    draw(&mut app, &mut t);
    let after = app.pane_rects[0].width;
    assert!(after > before, "{before} -> {after}");
    assert_eq!(after + app.pane_rects[1].width, row);

    assert_eq!(app.pane_rects[2].width, after);
    let shape = app.last_arranged.as_ref().unwrap().shape.clone();
    assert_eq!(shape, "2x2");
    let saved = app.ratios.get(&shape).unwrap().cols.clone();
    assert_eq!(saved.len(), 2);
    assert!(saved[0] > saved[1]);

    click_caret(&mut app, 1, false);
    draw(&mut app, &mut t);
    assert!(app.pane_rects[1].width < before);

    let r = caret(&app, 0, true).unwrap();
    app.last_click = None;
    for kind in [
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
    ] {
        app.on_mouse_event(crossterm::event::MouseEvent {
            kind,
            column: r.x + 1,
            row: r.y,
            modifiers: KeyModifiers::SHIFT,
        });
    }
    draw(&mut app, &mut t);
    assert_eq!(app.pane_rects[0].width, before);
    assert!(app.ratios.get(&shape).is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn width_carets_stop_at_the_minimum_and_double_click_resets() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("caretmin");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    for _ in 0..12 {
        click_caret(&mut app, 0, false);
        draw(&mut app, &mut t);
    }
    assert!(app.pane_rects[0].width >= 29, "{}", app.pane_rects[0].width);
    let at_min = app.pane_rects[0].width;
    click_caret(&mut app, 0, false);
    draw(&mut app, &mut t);
    assert_eq!(app.pane_rects[0].width, at_min);

    let r = caret(&app, 0, true).unwrap();
    app.last_click = None;
    click_at(&mut app, r.x, r.y);
    click_at(&mut app, r.x, r.y);
    draw(&mut app, &mut t);
    assert_eq!(app.pane_rects[0].width, app.pane_rects[1].width);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn width_carets_with_three_columns_and_the_keyboard() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("caret3");
    app.set_layout("columns");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    let cols = app.visible_panes().len();
    assert!(cols >= 3, "{cols}");
    let total: u16 = (0..cols).map(|i| app.pane_rects[i].width).sum();
    let w1 = app.pane_rects[1].width;
    let w2 = app.pane_rects[2].width;

    app.focus = 1;
    app.resize_pane_width(1, true);
    draw(&mut app, &mut t);
    assert!(app.pane_rects[1].width > w1);
    assert!(app.pane_rects[2].width < w2);
    assert_eq!(
        (0..cols).map(|i| app.pane_rects[i].width).sum::<u16>(),
        total
    );

    for name in ["pane wider", "pane narrower", "equal pane widths"] {
        assert!(
            crate::palette::ACTIONS.iter().any(|(n, _)| *n == name),
            "{name}"
        );
    }
    app.resize_pane_width(1, false);
    app.equal_pane_widths();
    draw(&mut app, &mut t);
    assert!(app.ratios.is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn width_carets_are_hidden_without_a_neighbour_or_when_zoomed() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("caretnone");
    let mut t = term(200, 50);
    app.set_layout("rows");
    draw(&mut app, &mut t);
    assert!(app
        .last_hits
        .regions
        .iter()
        .all(|r| !matches!(r.action, UiAction::PaneWidth(..))));
    app.set_layout("auto");
    draw(&mut app, &mut t);
    assert!(caret(&app, 0, true).is_some());
    app.zoom = true;
    draw(&mut app, &mut t);
    assert!(app
        .last_hits
        .regions
        .iter()
        .all(|r| !matches!(r.action, UiAction::PaneWidth(..))));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_tight_header_says_ready_or_approval_never_a_bare_waiting() {
    use crate::pane::Activity;
    use crate::ui_chrome::short_state;
    assert_eq!(short_state(Activity::Ready.label()), "ready");
    assert_eq!(short_state(Activity::Permission.label()), "approval");
    assert_eq!(short_state(Activity::Working.label()), "working");
    assert_eq!(short_state("exited 1"), "exited");
    for a in [
        Activity::Idle,
        Activity::Starting,
        Activity::Working,
        Activity::Permission,
        Activity::Ready,
        Activity::Exited,
    ] {
        assert_ne!(short_state(a.label()), "waiting", "{a:?}");
        assert_ne!(short_state(a.label()), "needs", "{a:?}");
    }
}

#[test]
fn the_palette_jumps_to_any_open_tab_by_name() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("palette-tabs");
    let dir = app.cfg.accounts[app.panes[2].account.unwrap()].work_dir();
    let t = app.panes[2].add_tab(dir);
    app.panes[2].tabs[t].custom_name = Some("billing-api".into());
    let uid = app.panes[2].tabs[t].uid;
    app.focus = 0;

    let none = crate::palette::Palette::default();
    assert!(crate::palette::items(&app, &none)
        .iter()
        .all(|(n, _)| !n.starts_with("go to tab")));

    let pl = crate::palette::Palette {
        input: "billing".into(),
        sel: 0,
    };
    let items = crate::palette::items(&app, &pl);
    let (label, action) = &items[0];
    assert_eq!(*action, crate::palette::Action::GoTab(uid), "{items:?}");
    let acct = app
        .account_cfg(app.panes[2].account.unwrap())
        .display()
        .to_string();
    assert!(
        label.starts_with("go to tab: billing-api · ") && label.contains(&acct),
        "{label}"
    );

    let pl2 = crate::palette::Palette {
        input: "bilapi".into(),
        sel: 0,
    };
    assert!(crate::palette::items(&app, &pl2)
        .iter()
        .any(|(_, a)| *a == crate::palette::Action::GoTab(uid)));

    let next = crate::palette::Palette {
        input: "next".into(),
        sel: 0,
    };
    assert_eq!(crate::palette::items(&app, &next)[0].0, "next tab");

    app.panes[2].hidden = true;
    app.panes[2].active = 0;
    app.modal = Modal::Palette(pl);
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.focus, 2);
    assert_eq!(app.panes[2].active, t);
    assert!(!app.panes[2].hidden);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn find_reads_history_and_screen_and_keeps_the_scroll() {
    let mut p = vt100::Parser::new(5, 40, 1000);
    for i in 1..=50 {
        p.process(format!("line {i}\r\n").as_bytes());
    }
    p.process(b"prompt> ");
    p.screen_mut().set_scrollback(7);
    let lines = crate::find_all::recent_lines(&mut p, 20);
    assert_eq!(p.screen().scrollback(), 7, "the view's scroll is put back");
    assert_eq!(lines.last().map(|l| l.trim()), Some("prompt>"));
    assert_eq!(lines.len(), 20);
    assert_eq!(lines[0].trim(), "line 32");
    let m = crate::find_all::matches_in(&lines, "LINE 4", 3);
    assert_eq!(
        m,
        vec![
            ("line 49".to_string(), 2),
            ("line 48".to_string(), 3),
            ("line 47".to_string(), 4)
        ]
    );
    assert!(
        crate::find_all::matches_in(&lines, "l", 3).is_empty(),
        "one letter: nothing"
    );
    let cut = crate::find_all::around(
        &format!("{}needle{}", "a".repeat(80), "b".repeat(80)),
        "needle",
        30,
    );
    assert!(cut.contains("needle") && cut.chars().count() <= 30, "{cut}");
}

#[test]
fn find_in_every_tab_lists_matches_and_goes_there() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("findall");
    let dir = app.cfg.accounts[app.panes[1].account.unwrap()].work_dir();
    let t = app.panes[1].add_tab(dir);
    app.panes[1].tabs[t].custom_name = Some("deploy".into());
    let uid = app.panes[1].tabs[t].uid;
    app.panes[1].tabs[t]
        .parser
        .lock()
        .unwrap()
        .process(b"building...\r\nerror[E0425]: cannot find value `x`\r\ndone\r\n");
    app.focus = 0;
    let key = |app: &mut App, c: KeyCode| app.on_key(KeyEvent::new(c, KeyModifiers::NONE));

    app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    key(&mut app, KeyCode::Char('*'));
    assert!(matches!(app.modal, Modal::FindAll(_)), "{:?}", app.modal);
    for c in "E0425".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    let Modal::FindAll(fa) = app.modal.clone() else {
        panic!()
    };
    assert_eq!(fa.hits.len(), 1, "{:?}", fa.hits);
    assert_eq!(fa.hits[0].uid, uid);
    assert!(fa.hits[0].line.contains("cannot find value"));
    assert!(
        fa.hits[0].place.ends_with("· deploy"),
        "{}",
        fa.hits[0].place
    );

    let mut t2 = term(160, 40);
    draw(&mut app, &mut t2);
    let s: String = {
        let b = t2.backend().buffer();
        (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        s.contains("find in every tab") && s.contains("E0425"),
        "{s}"
    );
    assert!(app
        .last_hits
        .regions
        .iter()
        .any(|r| r.action == UiAction::FindAllRow(0)));

    app.panes[1].active = 0;
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.modal, Modal::None);
    assert_eq!((app.focus, app.panes[1].active), (1, t));

    assert!(crate::palette::ACTIONS
        .iter()
        .any(|(_, a)| *a == crate::palette::Action::FindAll));
    assert!(crate::ui::HELP.iter().any(|(k, _)| *k == "Ctrl-a *"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn help_shows_every_key_on_a_wide_screen_that_is_not_tall() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("help-fit");
    app.modal = Modal::Help;
    for (w, h) in [(200u16, 50u16), (160, 45)] {
        let mut seen = std::collections::HashSet::new();
        app.help_scroll = 0;
        loop {
            let mut t = term(w, h);
            draw(&mut app, &mut t);
            let b = t.backend().buffer();
            let s: String = (0..b.area.height)
                .map(|y| {
                    (0..b.area.width)
                        .map(|x| b[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            for (k, _) in crate::ui::HELP {
                if s.contains(&format!("  {k}")) {
                    seen.insert(*k);
                }
            }
            let shown = app
                .last_hits
                .regions
                .iter()
                .filter(|r| r.hint.starts_with("Run: "))
                .count();
            assert!(shown > 0, "{w}x{h}: shown rows are clickable");
            if app.help_scroll >= app.help_max.get() {
                break;
            }
            app.help_scroll += 1;
        }
        for (k, _) in crate::ui::HELP {
            assert!(seen.contains(k), "{w}x{h}: {k} is never shown");
        }
    }
    let _ = std::fs::remove_dir_all(home);
}
