use std::path::PathBuf;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{App, AppEvent};
use crate::app_input::InputGeo;
use crate::assistant_images::testing::{png, FakeClipboard};
use crate::assistant_images::ClipImage;
use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (App, PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-input-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    app.assistant.show = true;
    app.assistant.focused = true;

    app.key_burst = Some(false);
    (app, home)
}

fn press(app: &mut App, code: KeyCode, m: KeyModifiers) {
    app.on_key(KeyEvent::new(code, m));
}

fn key(app: &mut App, code: KeyCode) {
    press(app, code, KeyModifiers::NONE);
}

fn ctrl(app: &mut App, c: char) {
    press(app, KeyCode::Char(c), KeyModifiers::CONTROL);
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

fn caret(app: &mut App) -> usize {
    app.assistant.ed().pos()
}

fn sel(app: &mut App) -> Option<(usize, usize)> {
    app.assistant.ed().selection()
}

fn paste(app: &mut App, s: &str) {
    app.handle(AppEvent::Input(Event::Paste(s.into())));
}

#[test]
fn caret_keys_and_edits_at_the_caret() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("keys");
    typed(&mut app, "hello world");
    for _ in 0..5 {
        key(&mut app, KeyCode::Left);
    }
    typed(&mut app, "big ");
    assert_eq!(app.assistant.input, "hello big world");
    assert_eq!(caret(&mut app), 10);

    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(caret(&mut app), 6);
    press(&mut app, KeyCode::Char('b'), KeyModifiers::ALT);
    assert_eq!(caret(&mut app), 0);
    press(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
    assert_eq!(caret(&mut app), 5);
    press(&mut app, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(caret(&mut app), 9);
    key(&mut app, KeyCode::Home);
    assert_eq!(caret(&mut app), 0);
    key(&mut app, KeyCode::End);
    assert_eq!(caret(&mut app), 15);
    key(&mut app, KeyCode::Home);
    ctrl(&mut app, 'e');
    assert_eq!(caret(&mut app), 15);
    assert_eq!(app.assistant.input, "hello big world", "Ctrl-E only moved");

    ctrl(&mut app, 'w');
    assert_eq!(app.assistant.input, "hello big ");
    press(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
    assert_eq!(app.assistant.input, "hello ");
    typed(&mut app, "there you");
    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.assistant.input, "hello thereyou");
    key(&mut app, KeyCode::Delete);
    assert_eq!(app.assistant.input, "hello thereou");

    ctrl(&mut app, 'k');
    assert_eq!(app.assistant.input, "hello there");
    key(&mut app, KeyCode::Left);
    ctrl(&mut app, 'u');
    assert_eq!(app.assistant.input, "e");
    assert_eq!(caret(&mut app), 0);

    ctrl(&mut app, 'a');
    assert!(app.prefix);
    app.prefix = false;
    assert_eq!(app.assistant.input, "e");

    assert!(!app.on_assistant_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn selection_is_replaced_and_esc_drops_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("sel");
    typed(&mut app, "make it red");
    for _ in 0..3 {
        press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    }
    assert_eq!(sel(&mut app), Some((8, 11)));
    typed(&mut app, "blue");
    assert_eq!(app.assistant.input, "make it blue");

    press(
        &mut app,
        KeyCode::Left,
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    );
    assert_eq!(sel(&mut app), Some((8, 12)));
    paste(&mut app, "green");
    assert_eq!(app.assistant.input, "make it green");

    press(&mut app, KeyCode::Home, KeyModifiers::SHIFT);
    assert_eq!(sel(&mut app), Some((0, 13)));
    key(&mut app, KeyCode::Esc);
    assert_eq!(sel(&mut app), None);
    assert!(app.assistant_has_focus());
    assert_eq!(app.assistant.input, "make it green");
    key(&mut app, KeyCode::Esc);
    assert!(
        !app.assistant_has_focus(),
        "a second Esc gives the keys back"
    );
    app.assistant.focused = true;

    key(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.assistant.input, "make it gree");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn paste_goes_in_at_the_caret() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("paste");
    typed(&mut app, "ab");
    key(&mut app, KeyCode::Left);
    paste(&mut app, "X\r\nY");
    assert_eq!(app.assistant.input, "aX\nYb");
    assert_eq!(caret(&mut app), 4);
    typed(&mut app, "!");
    assert_eq!(app.assistant.input, "aX\nY!b");

    press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
    assert_eq!(app.assistant.input, "aX\nY!\nb");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn up_and_down_walk_rows_then_the_history() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("updown");
    app.assistant.input_hist = vec!["older".into(), "newest".into()];

    key(&mut app, KeyCode::Up);
    assert_eq!(app.assistant.input, "newest");
    assert_eq!(caret(&mut app), 6);
    key(&mut app, KeyCode::Up);
    assert_eq!(app.assistant.input, "older");
    key(&mut app, KeyCode::Down);
    assert_eq!(app.assistant.input, "newest");
    key(&mut app, KeyCode::Down);
    assert_eq!(app.assistant.input, "");

    app.assistant.input_geo.set(Some(InputGeo {
        x0: 0,
        right: 12,
        tx: 2,
        y: 0,
        h: 3,
        w: 10,
        top: 0,
    }));
    typed(&mut app, "abcdefghijklmno");
    press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
    typed(&mut app, "second");
    key(&mut app, KeyCode::Up);
    assert_eq!(caret(&mut app), 15, "the end of the short row above");
    key(&mut app, KeyCode::Up);
    assert_eq!(caret(&mut app), 6, "the wrapped row above, same column");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    assert_eq!(caret(&mut app), 22, "back down, the column kept");
    assert_eq!(app.assistant.hist_pos, None);
    key(&mut app, KeyCode::Down);
    assert_eq!(
        app.assistant.input, "abcdefghijklmno\nsecond",
        "no history down"
    );
    key(&mut app, KeyCode::Home);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Up);
    assert_eq!(caret(&mut app), 0);

    key(&mut app, KeyCode::Up);
    assert_eq!(app.assistant.input, "newest");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn wide_characters_and_emoji_edit_whole() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("wide");
    typed(&mut app, "漢字");
    paste(&mut app, "👍🏽");
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Left);
    typed(&mut app, "x");
    assert_eq!(app.assistant.input, "漢x字👍🏽");
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(
        app.assistant.input, "漢x字",
        "the emoji and its skin tone go together"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn image_chips_go_in_at_the_caret_and_stay_whole() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("chips");
    let c = FakeClipboard::default();
    *c.holds.lock().unwrap() = Some(ClipImage::Png(png(64, 32)));
    app.assistant.clipboard = Some(Box::new(c));
    typed(&mut app, "ab");
    key(&mut app, KeyCode::Left);
    ctrl(&mut app, 'v');
    assert_eq!(app.assistant.input, "a [image 1: 64x32] b");
    assert_eq!(caret(&mut app), 19, "after the chip and its space");
    let file = app.assistant.pending_images[0].path.clone();
    assert!(file.is_file());

    key(&mut app, KeyCode::Left);
    assert_eq!(caret(&mut app), 18);
    key(&mut app, KeyCode::Left);
    assert_eq!(caret(&mut app), 2);

    key(&mut app, KeyCode::Delete);
    assert_eq!(app.assistant.input, "a b");
    assert!(app.assistant.pending_images.is_empty() && !file.exists());

    paste(&mut app, "");
    assert_eq!(app.assistant.input, "a [image 2: 64x32] b");
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(
        app.assistant.input, "a [image 2: 64x32] ",
        "a character after it"
    );
    key(&mut app, KeyCode::Backspace);
    assert_eq!(
        app.assistant.input, "a ",
        "Backspace after the chip's space"
    );
    assert!(app.assistant.pending_images.is_empty());
    ctrl(&mut app, 'v');
    let imgs = app.take_input_images(&app.assistant.input.clone());
    assert_eq!(imgs.len(), 1);
    let _ = std::fs::remove_dir_all(&home);
}

fn draw(app: &mut App, t: &mut ratatui::Terminal<ratatui::backend::TestBackend>) {
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
}

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    app.on_mouse_event(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

#[test]
fn the_panel_draws_the_caret_and_a_click_places_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("render");
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    draw(&mut app, &mut t);
    let g = app.assistant.input_geo.get().expect("the input is drawn");
    let w = g.w;

    let first = format!("漢{}", "a".repeat(w - 2));
    paste(&mut app, &format!("{first}end"));
    draw(&mut app, &mut t);
    let g = app.assistant.input_geo.get().unwrap();
    assert_eq!(g.h, 3, "three rows at least");
    assert_eq!(app.want_cursor.get(), Some((g.tx + 3, g.y + 1)));

    key(&mut app, KeyCode::Home);
    draw(&mut app, &mut t);
    assert_eq!(app.want_cursor.get(), Some((g.tx, g.y)));
    let b = t.backend().buffer();
    assert_eq!(b[(g.tx - 2, g.y)].symbol(), "›");
    assert_eq!(b[(g.tx, g.y)].symbol(), "漢");
    assert_eq!(b[(g.tx + 2, g.y)].symbol(), "a");
    assert_eq!(b[(g.tx, g.y + 1)].symbol(), "e");

    key(&mut app, KeyCode::Right);
    draw(&mut app, &mut t);
    assert_eq!(app.want_cursor.get(), Some((g.tx + 2, g.y)));

    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        g.tx + 1,
        g.y + 1,
    );
    assert_eq!(caret(&mut app), first.len() + 1);
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        g.tx + 1,
        g.y + 1,
    );
    draw(&mut app, &mut t);
    assert_eq!(app.want_cursor.get(), Some((g.tx + 1, g.y + 1)));

    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        g.tx + 2,
        g.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        g.tx + 1,
        g.y + 1,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        g.tx + 1,
        g.y + 1,
    );
    assert_eq!(sel(&mut app), Some((3, first.len() + 1)));
    assert!(app.selection.is_none(), "not the conversation's selection");
    draw(&mut app, &mut t);
    let b = t.backend().buffer();
    assert_eq!(b[(g.tx + 2, g.y)].bg, crate::theme::SELECT_BG);
    assert_eq!(b[(g.tx, g.y + 1)].bg, crate::theme::SELECT_BG);
    assert_ne!(b[(g.tx, g.y)].bg, crate::theme::SELECT_BG, "not before it");
    assert_ne!(
        b[(g.tx + 1, g.y + 1)].bg,
        crate::theme::SELECT_BG,
        "not after it"
    );
    typed(&mut app, "-");
    assert_eq!(app.assistant.input, "漢-nd");

    app.assistant.set_input(
        (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    draw(&mut app, &mut t);
    let g = app.assistant.input_geo.get().unwrap();
    assert_eq!((g.h, g.top), (8, 2));
    for _ in 0..8 {
        key(&mut app, KeyCode::Up);
    }
    draw(&mut app, &mut t);
    let g = app.assistant.input_geo.get().unwrap();
    assert_eq!(g.top, 1);
    assert_eq!(app.want_cursor.get().map(|c| c.1), Some(g.y));
    let b = t.backend().buffer();
    assert_eq!(b[(g.tx + 5, g.y)].symbol(), "2");
    let _ = std::fs::remove_dir_all(&home);
}
