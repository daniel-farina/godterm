//! The assistant panel as a window: the width carets [< >] (and Ctrl-a {
//! / }, set_setting, voice, a drag on its edge), minimize to a strip,
//! maximize over the grid, close; the quick action bar above the input;
//! the composer (3 rows, a placeholder, growing); 80x24; docked and
//! overlay.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use serde_json::json;

use crate::app::{App, Modal};
use crate::app_panelwin::{PanelCtl, PanelWin, PANEL_MIN};
use crate::assistant_images::testing::{png, FakeClipboard};
use crate::assistant_images::ClipImage;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;

fn test_app(tag: &str) -> (App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-pwin-{tag}-{}", std::process::id()));
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
    app.assistant.show = true;
    app.assistant.focused = true;
    (app, home)
}

type Term = ratatui::Terminal<ratatui::backend::TestBackend>;

fn term(w: u16, h: u16) -> Term {
    ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap()
}

fn draw(app: &mut App, t: &mut Term) -> String {
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
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

fn line(t: &Term, y: u16) -> String {
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

fn panel(app: &App) -> Rect {
    find(app, &UiAction::AssistantFocus).expect("the panel is on screen")
}

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    app.on_mouse_event(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn click_at(app: &mut App, x: u16, y: u16) {
    app.last_click = None;
    mouse(app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(app, MouseEventKind::Up(MouseButton::Left), x, y);
}

fn click(app: &mut App, a: &UiAction) {
    let r = find(app, a).unwrap_or_else(|| panic!("{a:?} is not on screen"));
    click_at(app, r.x + r.width / 2, r.y);
}

fn ctl(c: PanelCtl) -> UiAction {
    UiAction::AssistantPanel(c)
}

fn saved(key: &str) -> String {
    let text = std::fs::read_to_string(crate::config::Config::path()).unwrap_or_default();
    text.lines()
        .find(|l| l.trim_start().starts_with(&format!("{key} =")))
        .map(|l| l.split('=').nth(1).unwrap_or("").trim().to_string())
        .unwrap_or_default()
}

/// The carets sit on the title row left of the window controls; < widens
/// (leftward) and > narrows by 8, clamped between 36 and what leaves the
/// panes 40; the width shows a moment and is kept in config.toml; Ctrl-a {
/// and }, set_setting, the voice tool and a drag on the edge do the same.
#[test]
fn carets_change_the_width_clamp_and_persist() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("carets");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    let p = panel(&app);
    assert_eq!(p.width, 76, "automatic: half the screen, at most 76");
    assert_eq!(p.x + p.width, 200, "at the right edge");
    let wider = find(&app, &ctl(PanelCtl::Wider)).unwrap();
    let narrower = find(&app, &ctl(PanelCtl::Narrower)).unwrap();
    let close = find(&app, &ctl(PanelCtl::Close)).unwrap();
    assert_eq!((wider.y, narrower.y, close.y), (p.y, p.y, p.y), "title row");
    assert!(wider.x < narrower.x && narrower.x + narrower.width < close.x);
    assert_eq!(
        line(&t, p.y).chars().skip(p.x as usize).collect::<String>().matches(" < > ").count(),
        1
    );
    click(&mut app, &ctl(PanelCtl::Wider));
    assert_eq!(app.cfg.assistant.panel_width, 84);
    let s = draw(&mut app, &mut t);
    let p = panel(&app);
    assert_eq!((p.x, p.width), (200 - 84, 84), "it grows leftward");
    assert!(s.contains(" 84 cols "), "the width shows a moment");
    assert_eq!(saved("panel_width"), "84");
    for _ in 0..10 {
        click(&mut app, &ctl(PanelCtl::Narrower));
        draw(&mut app, &mut t);
    }
    assert_eq!(app.cfg.assistant.panel_width, PANEL_MIN);
    assert_eq!(panel(&app).width, PANEL_MIN);
    for _ in 0..30 {
        click(&mut app, &ctl(PanelCtl::Wider));
        draw(&mut app, &mut t);
    }
    assert_eq!(app.cfg.assistant.panel_width, 160, "the panes keep 40");
    assert_eq!(saved("panel_width"), "160");
    // Ctrl-a } and {.
    app.on_command(KeyEvent::new(KeyCode::Char('}'), KeyModifiers::NONE));
    assert_eq!(app.cfg.assistant.panel_width, 152);
    app.on_command(KeyEvent::new(KeyCode::Char('{'), KeyModifiers::NONE));
    assert_eq!(app.cfg.assistant.panel_width, 160);
    // set_setting, at once (no yes).
    let v = app.control_call(
        "set_setting",
        &json!({"key": "assistant.panel_width", "value": 60}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(app.cfg.assistant.panel_width, 60);
    // "make the assistant wider".
    let v = app.control_call("assistant_panel", &json!({"width": "wider"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(app.cfg.assistant.panel_width, 68);
    let v = app.control_call("assistant_panel", &json!({"width": "auto"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(app.cfg.assistant.panel_width, 0);
    draw(&mut app, &mut t);
    assert_eq!(panel(&app).width, 76);
    // In Settings > Assistant.
    assert!(crate::settings::search(&app.cfg, "panel width")
        .iter()
        .any(|(_, s)| s.key == crate::settings::Key::Global("assistant.panel_width")));
    // A drag on its left edge, kept when it ends.
    let p = panel(&app);
    let edge = find(&app, &ctl(PanelCtl::Edge)).unwrap();
    assert_eq!((edge.x, edge.width), (p.x, 1));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        p.x,
        p.y + 5,
    );
    assert!(app.assistant.edge_drag);
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        p.x - 10,
        p.y + 5,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        p.x - 10,
        p.y + 5,
    );
    assert!(!app.assistant.edge_drag);
    assert_eq!(app.cfg.assistant.panel_width, 86);
    assert_eq!(saved("panel_width"), "86");
    draw(&mut app, &mut t);
    assert_eq!(panel(&app).width, 86);
    let _ = std::fs::remove_dir_all(&home);
}

/// _ collapses it to a 3 column strip at the right edge ("◂", the word,
/// the state dot) that gives the keys back and keeps voice on; a click on
/// the strip (or Ctrl-a .) restores it.
#[test]
fn minimize_renders_a_strip_and_a_click_restores() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("min");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    let docked_right = app.pane_rects.iter().map(|r| r.x + r.width).max().unwrap();
    assert!(docked_right <= 200 - 76);
    click(&mut app, &ctl(PanelCtl::Minimize));
    assert_eq!(app.assistant.win, PanelWin::Min);
    assert!(app.assistant.show, "still on: voice keeps working");
    assert!(!app.assistant_has_focus(), "the keys go back to the grid");
    draw(&mut app, &mut t);
    let strip = find(&app, &ctl(PanelCtl::Restore)).expect("the strip");
    assert_eq!((strip.x, strip.width), (197, 3));
    assert!(find(&app, &UiAction::AssistantFocus).is_none());
    assert!(app.assistant.input_geo.get().is_none());
    let b = t.backend().buffer();
    assert_eq!(b[(198, strip.y)].symbol(), "◂");
    let word: String = (strip.y + 2..strip.y + 11)
        .map(|y| b[(198, y)].symbol().to_string())
        .collect();
    assert_eq!(word, "assistant");
    assert!(["●", "○"].contains(&b[(198, strip.y + 12)].symbol()));
    // Docked, the panes take the room back.
    let right = app.pane_rects.iter().map(|r| r.x + r.width).max().unwrap();
    assert_eq!(right, 197);
    // A click on it restores.
    click_at(&mut app, strip.x + 1, strip.y + 6);
    assert_eq!(app.assistant.win, PanelWin::Normal);
    assert!(app.assistant_has_focus());
    draw(&mut app, &mut t);
    assert_eq!(panel(&app).width, 76);
    // Ctrl-a . restores too.
    app.panel_ctl(PanelCtl::Minimize);
    app.on_command(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE));
    assert_eq!(app.assistant.win, PanelWin::Normal);
    assert!(app.assistant.show && app.assistant_has_focus());
    // By voice.
    let v = app.control_call("assistant_panel", &json!({"window": "minimize"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(app.assistant.win, PanelWin::Min);
    let _ = std::fs::remove_dir_all(&home);
}

/// □ takes the whole grid area (floating: the panes keep their size) and
/// shows ▣, which restores it; × hides it.
#[test]
fn maximize_takes_the_grid_and_restores_and_close_hides() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("max");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    let sizes: Vec<(u16, u16)> = app.panes.iter().map(|p| p.cur().pty_size()).collect();
    let p0 = panel(&app);
    click(&mut app, &ctl(PanelCtl::Maximize));
    assert_eq!(app.assistant.win, PanelWin::Max);
    draw(&mut app, &mut t);
    let p = panel(&app);
    let (mx, mw) = app.assistant.main_geo.get();
    assert_eq!((p.x, p.width, p.y, p.height), (mx, mw, p0.y, p0.height));
    assert_eq!((mx, mw), (0, 200));
    let after: Vec<(u16, u16)> = app.panes.iter().map(|p| p.cur().pty_size()).collect();
    assert!(
        after
            .iter()
            .zip(&sizes)
            .all(|(a, b)| a.0 >= b.0 && a.1 >= b.1),
        "the panes are not squeezed under it"
    );
    let m = find(&app, &ctl(PanelCtl::Maximize)).unwrap();
    assert_eq!(t.backend().buffer()[(m.x + 1, m.y)].symbol(), "▣");
    assert!(find(&app, &ctl(PanelCtl::Wider)).is_none(), "no carets");
    click(&mut app, &ctl(PanelCtl::Maximize));
    assert_eq!(app.assistant.win, PanelWin::Normal);
    draw(&mut app, &mut t);
    assert_eq!(panel(&app), p0);
    let m = find(&app, &ctl(PanelCtl::Maximize)).unwrap();
    assert_eq!(t.backend().buffer()[(m.x + 1, m.y)].symbol(), "□");
    // The controls read _ □ × at the top right, before the corner.
    let r = find(&app, &ctl(PanelCtl::Minimize)).unwrap();
    let c = find(&app, &ctl(PanelCtl::Close)).unwrap();
    assert_eq!(c.x + 3, p0.x + p0.width - 1);
    assert_eq!(r.x + 6, c.x);
    let b = t.backend().buffer();
    assert_eq!(
        [b[(r.x + 1, r.y)].symbol(), b[(c.x + 1, c.y)].symbol()],
        ["_", "×"]
    );
    // The toolbar's ⋯ stays, without a second close.
    let more = find(
        &app,
        &UiAction::MenuOpen(crate::menus::MenuId::AssistantMore),
    )
    .unwrap();
    assert_eq!(more.y, p0.y + 1);
    assert!(!app
        .last_hits
        .regions
        .iter()
        .any(|r| r.action == UiAction::Key('.') && p0.intersects(r.rect)));
    // Close.
    click(&mut app, &ctl(PanelCtl::Close));
    assert!(!app.assistant.show && !app.assistant_has_focus());
    draw(&mut app, &mut t);
    assert!(find(&app, &UiAction::AssistantFocus).is_none());
    let _ = std::fs::remove_dir_all(&home);
}

/// The quick bar above the input: buttons with their keys, wired to the
/// app's own actions.
#[test]
fn quick_buttons_trigger_their_actions() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("quick");
    app.cfg.assistant.account = app.cfg.accounts[0].name.clone();
    let rec = home.join("brain-in.txt");
    let stub = crate::test_stub::claude(
        &home.join("brain"),
        &[
            ("stdin_to", rec.display().to_string()),
            ("stdin_append", "1".into()),
        ],
    );
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    let mut t = term(200, 50);
    let s = draw(&mut app, &mut t);
    let g = app.assistant.input_geo.get().unwrap();
    // The quick rows: right above the composer's frame.
    let in_bar = |r: &Rect| r.y + 3 >= g.y && r.y + 1 < g.y;
    let bar: Vec<UiAction> = app
        .last_hits
        .regions
        .iter()
        .filter(|r| in_bar(&r.rect))
        .map(|r| r.action.clone())
        .collect();
    for a in [
        UiAction::MuteToggle,
        UiAction::SpeakerToggle,
        UiAction::Key('v'),
        ctl(PanelCtl::Summarize),
        UiAction::Key('y'),
        UiAction::Key('K'),
        ctl(PanelCtl::AttachImage),
    ] {
        assert!(bar.contains(&a), "{a:?} not in the bar: {bar:?}");
    }
    for words in [
        "Mic C-a X",
        "Speaker C-a O",
        "Approvals 0 C-a y",
        "Direct C-a K",
    ] {
        assert!(s.contains(words), "{words}");
    }
    // (The voice strip under the grid comes and goes: the bar moves.)
    let quick = |app: &App, a: &UiAction| -> Rect {
        let g = app.assistant.input_geo.get().unwrap();
        app.last_hits
            .regions
            .iter()
            .rev()
            .find(|r| &r.action == a && r.rect.y + 3 >= g.y && r.rect.y + 1 < g.y)
            .unwrap_or_else(|| panic!("{a:?} not in the bar"))
            .rect
    };
    // Speaker.
    let r = quick(&app, &UiAction::SpeakerToggle);
    click_at(&mut app, r.x + 1, r.y);
    assert!(app.cfg.voice.speaker_muted);
    let s = draw(&mut app, &mut t);
    assert!(s.contains("Speaker off"), "{s}");
    // Mic.
    let r = quick(&app, &UiAction::MuteToggle);
    click_at(&mut app, r.x + 1, r.y);
    assert!(app.voice.muted);
    draw(&mut app, &mut t);
    // Approvals.
    let r = quick(&app, &UiAction::Key('y'));
    click_at(&mut app, r.x + 1, r.y);
    assert!(matches!(app.modal, Modal::Approvals(0)));
    app.modal = Modal::None;
    draw(&mut app, &mut t);
    // Attach image: the clipboard's image becomes a chip.
    let c = FakeClipboard::default();
    *c.holds.lock().unwrap() = Some(ClipImage::Png(png(20, 10)));
    app.assistant.clipboard = Some(Box::new(c));
    let r = quick(&app, &ctl(PanelCtl::AttachImage));
    click_at(&mut app, r.x + 1, r.y);
    assert_eq!(app.assistant.pending_images.len(), 1);
    assert!(app.assistant.input.starts_with("[image 1: 20x10]"));
    app.assistant.set_input(String::new());
    app.assistant.pending_images.clear();
    draw(&mut app, &mut t);
    // Summarize tabs: the canned request goes to the assistant.
    draw(&mut app, &mut t);
    let r = quick(&app, &ctl(PanelCtl::Summarize));
    click_at(&mut app, r.x + 1, r.y);
    let t0 = Instant::now();
    let mut got = String::new();
    while t0.elapsed() < Duration::from_secs(5) {
        got = std::fs::read_to_string(&rec).unwrap_or_default();
        if got.contains("working on") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(got.contains("What's everyone working on?"), "{got}");
    let _ = std::fs::remove_dir_all(&home);
}

/// The composer: a framed field 3 rows tall with the placeholder, growing
/// with the text up to assistant.input_rows; Send and the mic state on
/// its bottom edge.
#[test]
fn the_composer_is_three_rows_with_a_placeholder_and_grows() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("composer");
    let mut t = term(200, 50);
    let s = draw(&mut app, &mut t);
    let g = app.assistant.input_geo.get().unwrap();
    assert_eq!(g.h, 3);
    assert!(s.contains("Type or speak…  (Enter sends, Shift+Enter new line)"));
    let b = t.backend().buffer();
    let p = panel(&app);
    assert_eq!(b[(p.x + 1, g.y - 1)].symbol(), "╭");
    assert_eq!(b[(p.x + 1, g.y + 3)].symbol(), "╰");
    assert_eq!(b[(g.tx, g.y + 1)].bg, crate::theme::SEL_BG, "the field");
    assert_eq!(b[(p.x + 1, g.y)].fg, crate::theme::SLATE, "focused: slate");
    let send = find(&app, &UiAction::AssistantSend).unwrap();
    assert_eq!(send.y, g.y + 3, "Send on the bottom edge");
    assert!(line(&t, g.y + 3).contains("sound on"));
    assert_eq!(app.want_cursor.get(), Some((g.tx, g.y)));
    // Unfocused: a faint frame.
    app.assistant.focused = false;
    draw(&mut app, &mut t);
    assert_eq!(t.backend().buffer()[(p.x + 1, g.y)].fg, crate::theme::FAINT);
    app.assistant.focused = true;
    // It grows with the lines, up to 8.
    app.assistant
        .set_input("one\ntwo\nthree\nfour\nfive".into());
    draw(&mut app, &mut t);
    assert_eq!(app.assistant.input_geo.get().unwrap().h, 5);
    app.assistant.set_input(
        (1..=12)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    draw(&mut app, &mut t);
    let g = app.assistant.input_geo.get().unwrap();
    assert_eq!((g.h, g.top), (8, 4));
    // The setting.
    let v = app.control_call(
        "set_setting",
        &json!({"key": "assistant.input_rows", "value": 4}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    draw(&mut app, &mut t);
    assert_eq!(app.assistant.input_geo.get().unwrap().h, 4);
    // Editing still works: typing and Send.
    app.assistant.set_input(String::new());
    for c in "hi".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
    assert_eq!(app.assistant.input, "hoi");
    let _ = std::fs::remove_dir_all(&home);
}

/// No region of the panel sticks out of it, and none of its buttons
/// overlap.
fn assert_tidy(app: &App) {
    let p = panel(app);
    let mine: Vec<Rect> = app
        .last_hits
        .regions
        .iter()
        .filter(|r| {
            matches!(
                r.action,
                UiAction::AssistantPanel(_)
                    | UiAction::AssistantSend
                    | UiAction::SpeakerToggle
                    | UiAction::MuteToggle
                    | UiAction::AssistantNew
                    | UiAction::AssistantHistory
            ) || matches!(r.action, UiAction::Key('v' | 'y' | 'K'))
        })
        .filter(|r| r.rect.x >= p.x && r.rect.y >= p.y && r.rect.y < p.y + p.height)
        .map(|r| r.rect)
        .collect();
    for r in &mine {
        assert!(r.x + r.width <= p.x + p.width, "{r:?} past {p:?}");
    }
    for (i, a) in mine.iter().enumerate() {
        for b in &mine[i + 1..] {
            let edge = a.width == 1 || b.width == 1;
            if !edge && a.intersects(*b) {
                panic!("{a:?} overlaps {b:?}");
            }
        }
    }
}

/// 80x24: everything fits (one quick row at most, the carets, the
/// controls, a 3 row composer); shorter, the quick bar goes first; too
/// narrow for the title, the carets go, the controls stay.
#[test]
fn eighty_by_twenty_four_degrades_sensibly() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("small");
    let mut t = term(80, 24);
    draw(&mut app, &mut t);
    let p = panel(&app);
    assert_eq!(p.width, 44);
    assert_tidy(&app);
    let g = app.assistant.input_geo.get().unwrap();
    assert_eq!(g.h, 3);
    assert!(
        find(&app, &ctl(PanelCtl::Wider)).is_some(),
        "the carets fit"
    );
    assert!(find(&app, &ctl(PanelCtl::Close)).is_some());
    let quick_rows: std::collections::BTreeSet<u16> = app
        .last_hits
        .regions
        .iter()
        .filter(|r| r.action == UiAction::MuteToggle && r.rect.y > p.y + 1)
        .map(|r| r.rect.y)
        .collect();
    assert_eq!(quick_rows.len(), 1, "one quick row");
    // Short: the quick bar goes, the composer stays.
    let mut short = term(80, 15);
    draw(&mut app, &mut short);
    assert_tidy(&app);
    let p = panel(&app);
    assert!(!app
        .last_hits
        .regions
        .iter()
        .any(|r| r.action == ctl(PanelCtl::Summarize)));
    assert!(app.assistant.input_geo.get().unwrap().h >= 1);
    assert!(find(&app, &UiAction::AssistantSend).is_some());
    assert!(find(&app, &ctl(PanelCtl::Wider)).is_some());
    let _ = p;
    // At its narrowest (36 columns) the title gives up its pieces; the
    // carets and the controls stay.
    app.cfg.assistant.panel_width = 36;
    draw(&mut app, &mut t);
    assert_eq!(panel(&app).width, 36);
    assert_tidy(&app);
    assert!(find(&app, &ctl(PanelCtl::Wider)).is_some());
    // Narrower than the title needs (a tiny floating panel): the carets go.
    assert!(!crate::ui_panelwin::carets_fit(&app, 30));
    assert!(find(&app, &ctl(PanelCtl::Minimize)).is_some());
    assert!(find(&app, &ctl(PanelCtl::Close)).is_some());
    // The ⋯ menu still has the width.
    let es = crate::menus::entries(&app, crate::menus::MenuId::AssistantMore);
    assert!(es.iter().any(|e| e.label == "Wider panel"));
    let _ = std::fs::remove_dir_all(&home);
}

/// Docked, the panes make room for its width (and the strip's); floating,
/// they keep their size whatever its width.
#[test]
fn docked_and_overlay_both_resize_and_minimize() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("modes");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    app.nudge_panel_width(8);
    draw(&mut app, &mut t);
    let p = panel(&app);
    assert_eq!(p.width, 84);
    let right = app.pane_rects.iter().map(|r| r.x + r.width).max().unwrap();
    assert!(right <= p.x, "docked: no pane under it");
    // Overlay.
    app.set_panel_mode("overlay").unwrap();
    app.assistant.show = false;
    draw(&mut app, &mut t);
    let closed: Vec<(u16, u16)> = app.panes.iter().map(|p| p.cur().pty_size()).collect();
    app.assistant.show = true;
    draw(&mut app, &mut t);
    assert!(app.panel_overlay.get());
    assert_eq!(panel(&app).width, 84);
    click(&mut app, &ctl(PanelCtl::Narrower));
    draw(&mut app, &mut t);
    assert_eq!(panel(&app).width, 76);
    click(&mut app, &ctl(PanelCtl::Minimize));
    draw(&mut app, &mut t);
    assert!(find(&app, &ctl(PanelCtl::Restore)).is_some());
    let now: Vec<(u16, u16)> = app.panes.iter().map(|p| p.cur().pty_size()).collect();
    assert_eq!(now, closed, "floating: the panes keep their size");
    click(&mut app, &ctl(PanelCtl::Restore));
    draw(&mut app, &mut t);
    click(&mut app, &ctl(PanelCtl::Maximize));
    draw(&mut app, &mut t);
    assert_eq!(panel(&app).width, 200);
    let _ = std::fs::remove_dir_all(&home);
}
