//! Clicking the buttons of every confirmation a move can show: the move
//! picker, the busy tab dialog, the failover notice, the assistant's
//! question; with the panel closed, docked and floating.

use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;
use crate::pane::{Activity, PaneState};

pub(crate) fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-cclick-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.save().unwrap();
    let mut app = crate::app::App::new(cfg, tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    (app, home)
}

pub(crate) fn term() -> ratatui::Terminal<ratatui::backend::TestBackend> {
    ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 50)).unwrap()
}

pub(crate) fn draw(
    app: &mut crate::app::App,
    t: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
}

/// A real mouse click on the (topmost) region with this action.
pub(crate) fn click(app: &mut crate::app::App, a: &UiAction) -> (u16, u16) {
    let r = app
        .last_hits
        .regions
        .iter()
        .rev()
        .find(|r| &r.action == a)
        .unwrap_or_else(|| panic!("{a:?} is not on screen"))
        .rect;
    let (x, y) = (r.x + r.width / 2, r.y);
    for kind in [
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
    ] {
        app.on_mouse_event(crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
    }
    (x, y)
}

fn modes() -> [Option<&'static str>; 3] {
    [None, Some("docked"), Some("overlay")]
}

fn set_mode(app: &mut crate::app::App, m: Option<&str>) {
    match m {
        None => app.assistant.show = false,
        Some(m) => {
            app.assistant.show = true;
            app.cfg.assistant.panel = m.into();
        }
    }
}

#[test]
fn the_move_picker_buttons_click() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for m in modes() {
        let (mut app, home) = test_app("picker");
        set_mode(&mut app, m);
        let mut t = term();
        app.open_move_picker(0, 0, false);
        draw(&mut app, &mut t);
        click(&mut app, &UiAction::ModalCancel);
        assert!(app.modal == crate::app::Modal::None, "{m:?}: Cancel");
        assert!(app.pending_moves.is_empty());
        app.open_move_picker(0, 0, false);
        draw(&mut app, &mut t);
        click(&mut app, &UiAction::MoveGo);
        assert_eq!(app.pending_moves.len(), 1, "{m:?}: Move");
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn the_busy_dialog_buttons_click() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for m in modes() {
        let (mut app, home) = test_app("busy");
        set_mode(&mut app, m);
        app.panes[0].tabs[0].state = PaneState::Running;
        app.panes[0].tabs[0].activity = Activity::Working;
        let mut t = term();
        app.request_move(0, 0, 1, false, true);
        assert!(matches!(app.modal, crate::app::Modal::MoveBusy(_)));
        draw(&mut app, &mut t);
        click(&mut app, &UiAction::ModalCancel);
        assert!(app.modal == crate::app::Modal::None, "{m:?}: Cancel");
        assert!(app.queued_moves.is_empty());
        app.request_move(0, 0, 1, false, true);
        draw(&mut app, &mut t);
        click(&mut app, &UiAction::MoveBusy(false));
        assert_eq!(app.queued_moves.len(), 1, "{m:?}: Wait until idle");
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// Moving a saved session to another account (Sessions screen): the
/// target buttons and the Yes / Cancel of its confirmation, by mouse.
#[test]
fn the_session_move_confirmation_clicks() {
    use crate::app::Modal;
    use crate::app_sessions::{SourceSel, Src};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for m in modes() {
        let (mut app, home) = test_app("sessmove");
        set_mode(&mut app, m);
        let a1 = app.cfg.accounts[0].config_dir();
        let proj = a1.join("projects").join("-tmp-proj");
        std::fs::create_dir_all(&proj).unwrap();
        let id = "11111111-2222-4333-8444-555555555555";
        let file = proj.join(format!("{id}.jsonl"));
        std::fs::write(
            &file,
            format!("{{\"sessionId\":\"{id}\",\"cwd\":\"/tmp/proj\",\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"fix the flaky test\"}}}}\n"),
        )
        .unwrap();
        app.accounts[0].sessions = crate::sessions::SessionCache::default().scan(&a1);
        app.view = crate::app::View::Sessions;
        app.sess_source = SourceSel::One(Src::Account(0));
        app.sess_headless = true;
        app.sel_session = 0;
        let mut t = term();
        draw(&mut app, &mut t);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('m'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(matches!(app.modal, Modal::SessTarget(_)), "{m:?}");
        draw(&mut app, &mut t);
        click(&mut app, &UiAction::SessTarget(0));
        assert_eq!(app.modal, Modal::SessConfirm, "{m:?}");
        draw(&mut app, &mut t);
        // No cancels, nothing moves.
        click(&mut app, &UiAction::ModalCancel);
        assert_eq!(app.modal, Modal::None, "{m:?}: Cancel");
        assert!(file.is_file());
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('m'),
            crossterm::event::KeyModifiers::NONE,
        ));
        draw(&mut app, &mut t);
        click(&mut app, &UiAction::SessTarget(0));
        draw(&mut app, &mut t);
        click(&mut app, &UiAction::SessConfirm);
        assert_eq!(app.modal, Modal::None, "{m:?}: Yes");
        assert!(!file.exists(), "{m:?}: moved");
        let _ = std::fs::remove_dir_all(&home);
    }
}

fn usage(five_used: f64, week_used: f64) -> crate::usage::Usage {
    crate::usage::parse_usage(&format!(
        r#"{{"five_hour":{{"utilization":{five_used},"resets_at":"2099-10-09T17:29:00+00:00"}},"seven_day":{{"utilization":{week_used},"resets_at":"2099-10-16T13:00:00+00:00"}}}}"#
    ))
    .unwrap()
}

#[test]
fn the_failover_notice_clicks() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for m in modes() {
        for yes in [true, false] {
            let (mut app, home) = test_app("failover");
            set_mode(&mut app, m);
            for a in 0..app.accounts.len() {
                app.accounts[a].usage = Some(usage(10.0, 10.0));
            }
            app.accounts[0].usage = Some(usage(100.0, 10.0));
            app.panes[0].tabs[0].state = PaneState::Running;
            app.failover_scan();
            assert_eq!(app.failover.len(), 1);
            let mut t = term();
            draw(&mut app, &mut t);
            click(
                &mut app,
                &if yes {
                    UiAction::FailoverMove
                } else {
                    UiAction::FailoverDismiss
                },
            );
            assert_eq!(app.pending_moves.len(), usize::from(yes), "{m:?} yes={yes}");
            assert!(app.failover.is_empty());
            let _ = std::fs::remove_dir_all(&home);
        }
    }
}

#[test]
fn the_assistants_question_has_yes_and_no_buttons() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for m in [Some("docked"), Some("overlay")] {
        for yes in [true, false] {
            let (mut app, home) = test_app("ask");
            set_mode(&mut app, m);
            app.pending_confirms.push(crate::control::Pending {
                token: "tok".into(),
                tool: "move_tab".into(),
                plan: serde_json::json!({}),
                summary: "Move 3 tabs to Bravo".into(),
                turn: 0,
                at: std::time::Instant::now(),
            });
            let mut t = term();
            draw(&mut app, &mut t);
            let shown = {
                let b = t.backend().buffer();
                let mut s = String::new();
                for y in 0..b.area.height {
                    for x in 0..b.area.width {
                        s.push_str(b[(x, y)].symbol());
                    }
                    s.push('\n');
                }
                s
            };
            assert!(shown.contains("? Move 3 tabs to Bravo"), "{shown}");
            click(&mut app, &UiAction::AssistantAnswer(yes));
            let said = if yes { "yes" } else { "no" };
            assert_eq!(
                app.assistant.input_hist.last().map(String::as_str),
                Some(said),
                "{m:?}"
            );
            assert!(app.assistant.input.is_empty());
            let _ = std::fs::remove_dir_all(&home);
        }
    }
}

#[test]
fn the_keyboard_answers_every_dialog() {
    use crate::app::Modal;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let key =
        |app: &mut crate::app::App, c: KeyCode| app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
    let (mut app, home) = test_app("keys");
    // Even with the panel taking the keys, the dialog answers first.
    app.assistant.show = true;
    app.assistant.focused = true;
    app.panes[0].tabs[0].state = PaneState::Running;
    app.panes[0].tabs[0].activity = Activity::Working;
    for (k, queued) in [
        (KeyCode::Char('y'), true),
        (KeyCode::Enter, true),
        (KeyCode::Char('n'), false),
        (KeyCode::Esc, false),
    ] {
        app.queued_moves.clear();
        app.request_move(0, 0, 1, false, true);
        key(&mut app, k);
        assert_eq!(app.modal, Modal::None, "{k:?}");
        assert_eq!(!app.queued_moves.is_empty(), queued, "{k:?}");
        assert!(app.assistant.input.is_empty(), "not typed into the panel");
    }
    // The picker: Esc cancels, Enter moves.
    app.panes[0].tabs[0].activity = Activity::Ready;
    app.open_move_picker(0, 0, false);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.modal, Modal::None);
    assert!(app.pending_moves.is_empty());
    app.open_move_picker(0, 0, false);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.pending_moves.len(), 1);
    // The default button stands out (bold, underlined).
    app.request_move(1, 0, 0, false, true);
    if matches!(app.modal, Modal::MoveBusy(_)) {
        let mut t = term();
        draw(&mut app, &mut t);
        let r = app.last_hits.find(&UiAction::MoveBusy(false)).unwrap().rect;
        let c = &t.backend().buffer()[(r.x + 2, r.y)];
        assert!(c.modifier.contains(ratatui::style::Modifier::UNDERLINED));
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn no_click_reaches_the_pane_beneath_a_dialog() {
    use crate::app::Modal;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for m in modes() {
        let (mut app, home) = test_app("through");
        set_mode(&mut app, m);
        app.panes[0].tabs[0].state = PaneState::Running;
        app.panes[0].tabs[0].activity = Activity::Working;
        app.focus = 0;
        app.request_move(0, 0, 1, false, true);
        let mut t = term();
        draw(&mut app, &mut t);
        // A click on the second pane, outside the dialog: nothing happens
        // under it, the dialog stays.
        let p1 = app.pane_rects[1];
        app.on_mouse_event(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: p1.x + 2,
            row: p1.y + p1.height - 3,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(app.focus, 0, "{m:?}");
        assert!(matches!(app.modal, Modal::MoveBusy(_)), "{m:?}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
