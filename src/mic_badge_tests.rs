use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{App, Modal};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;

type Term = ratatui::Terminal<ratatui::backend::TestBackend>;

fn test_app(tag: &str) -> (App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-mic-{tag}-{}", std::process::id()));
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

    for p in app.panes.iter_mut() {
        for t in p.tabs.iter_mut() {
            t.suspended = true;
        }
    }
    (app, home)
}

fn draw(app: &mut App, t: &mut Term) {
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
}

fn term(w: u16, h: u16) -> Term {
    ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap()
}

fn row(t: &Term, y: u16) -> String {
    let b = t.backend().buffer();
    (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
}

fn find(app: &App, a: &UiAction) -> Option<Rect> {
    app.last_hits
        .regions
        .iter()
        .rev()
        .find(|r| &r.action == a)
        .map(|r| r.rect)
}

fn click_at(app: &mut App, x: u16, y: u16) {
    app.last_click = None;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.on_mouse_event(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }
}

fn click(app: &mut App, a: &UiAction) {
    let r = find(app, a).unwrap_or_else(|| panic!("{a:?} is not on screen"));
    click_at(app, r.x + r.width / 2, r.y);
}

#[test]
fn only_the_pane_with_the_mic_has_the_badge() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("badge");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    assert!(app
        .last_hits
        .regions
        .iter()
        .all(|r| !matches!(r.action, UiAction::MicMenu(_))));
    app.enter_direct(1, 0).unwrap();
    draw(&mut app, &mut t);
    let r = find(&app, &UiAction::MicMenu(1)).expect("badge on pane 1");
    assert_eq!(r.y, app.pane_rects[1].y, "on the header row");
    assert!(find(&app, &UiAction::MicMenu(0)).is_none());
    let text = row(&t, r.y);
    assert!(text.contains("● mic"), "{text}");

    app.voice.muted = true;
    draw(&mut app, &mut t);
    assert!(row(&t, r.y).contains("● mic · muted"), "{}", row(&t, r.y));
    app.voice.muted = false;
    app.voice.status = crate::voice::VoiceStatus::Listening;
    draw(&mut app, &mut t);
    assert!(
        row(&t, r.y).contains("● mic · listening"),
        "{}",
        row(&t, r.y)
    );

    app.end_direct("test", false);
    draw(&mut app, &mut t);
    assert!(app
        .last_hits
        .regions
        .iter()
        .all(|r| !matches!(r.action, UiAction::MicMenu(_))));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_narrow_header_keeps_the_badge_short() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("narrow");
    app.enter_direct(0, 0).unwrap();
    app.voice.muted = true;
    let mut t = term(80, 24);
    draw(&mut app, &mut t);
    let r = find(&app, &UiAction::MicMenu(0)).expect("badge at 80x24");
    assert!(row(&t, r.y).contains("● mic"), "{}", row(&t, r.y));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_menu_moves_the_mic_to_another_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("move");
    let mut t = term(200, 50);
    app.enter_direct(0, 0).unwrap();
    let from = app.panes[0].tabs[0].uid;
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::MicMenu(0));
    assert_eq!(app.modal, Modal::MicMenu(0));
    draw(&mut app, &mut t);
    let targets = app.mic_targets();
    assert!(!targets.is_empty());
    assert!(targets
        .iter()
        .all(|(u, label)| *u != from && label.contains(" · ")));
    assert!(find(&app, &UiAction::MicToAssistant).is_some());
    let menu: String = (0..50).map(|y| row(&t, y)).collect::<Vec<_>>().join("\n");
    assert!(menu.contains("move the mic to"), "{menu}");
    assert!(menu.contains("bring the mic back"), "{menu}");

    let (uid, _) = targets[0].clone();
    click(&mut app, &UiAction::MicMoveTo(uid));
    assert_eq!(app.modal, Modal::None);
    assert!(app.direct_with(uid));
    assert!(!app.direct_with(from));
    draw(&mut app, &mut t);
    let (s, _) = app.find_tab(uid).unwrap();
    assert!(find(&app, &UiAction::MicMenu(s)).is_some());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_menu_brings_the_mic_back_and_the_badge_toggles_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("back");
    let mut t = term(200, 50);
    app.enter_direct(0, 0).unwrap();
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::MicMenu(0));
    assert_eq!(app.modal, Modal::MicMenu(0));

    draw(&mut app, &mut t);
    click(&mut app, &UiAction::MicMenu(0));
    assert_eq!(app.modal, Modal::None);
    click(&mut app, &UiAction::MicMenu(0));
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::MicToAssistant);
    assert!(app.direct.on.is_none());
    assert_eq!(app.modal, Modal::None);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_menu_has_number_keys() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("keys");
    app.enter_direct(0, 0).unwrap();
    let first = app.mic_targets()[0].0;
    app.modal = Modal::MicMenu(0);
    app.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    assert!(app.direct_with(first));
    app.modal = Modal::MicMenu(0);
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.modal, Modal::None);
    assert!(app.direct_with(first), "Esc changes nothing");
    app.modal = Modal::MicMenu(0);
    app.on_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE));
    assert!(app.direct.on.is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_assistant_says_where_the_mic_is() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("panel");
    app.assistant.show = true;
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    let all = |t: &Term| (0..50).map(|y| row(t, y)).collect::<Vec<_>>().join("\n");
    assert!(!all(&t).contains("mic at"));
    app.enter_direct(1, 0).unwrap();
    let name = app.direct.on.as_ref().unwrap().name.clone();
    draw(&mut app, &mut t);
    let s = all(&t);
    assert!(
        s.contains(&format!("mic at {name} · click to bring back")),
        "{s}"
    );
    click(&mut app, &UiAction::MicToAssistant);
    assert!(app.direct.on.is_none());
    draw(&mut app, &mut t);
    assert!(!all(&t).contains("mic at"));
    let _ = std::fs::remove_dir_all(&home);
}
