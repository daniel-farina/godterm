use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{draw, term, test_app};
use crate::hits::UiAction;

fn setup(tag: &str) -> (App, std::path::PathBuf) {
    test_app(tag)
}

fn find(app: &App, a: &UiAction) -> Option<ratatui::layout::Rect> {
    app.last_hits
        .regions
        .iter()
        .rev()
        .find(|r| &r.action == a)
        .map(|r| r.rect)
}

#[test]
fn the_panel_is_open_at_start_whatever_it_was_and_the_mode() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("start-panel");
    assert!(!app.assistant.show, "closed until start opens it");
    for mode in ["docked", "overlay", "auto"] {
        app.cfg.assistant.panel = mode.into();
        app.assistant.show = false;
        app.assistant.win = crate::app_panelwin::PanelWin::Max;
        app.open_panel_at_start();
        assert!(app.assistant.show, "{mode}");
        assert_eq!(app.assistant.win, crate::app_panelwin::PanelWin::Normal);
        assert!(!app.assistant.focused, "the keys stay with the grid");
        let mut t = term();
        draw(&mut app, &mut t);
        assert!(
            find(&app, &UiAction::AssistantFocus).is_some(),
            "{mode} draws it"
        );
    }

    app.cfg.assistant.mode = "off".into();
    app.assistant.show = false;
    app.open_panel_at_start();
    assert!(!app.assistant.show);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_mic_starts_off_unless_it_was_on_at_the_last_quit() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("start-mic");

    app.cfg.voice.enabled = true;
    assert_eq!(app.startup_voice(false), None);

    app.rt_voice = Some("off".into());
    assert_eq!(app.startup_voice(false), None);

    for m in ["open", "wake", "push"] {
        app.rt_voice = Some(m.into());
        assert_eq!(app.startup_voice(false).as_deref(), Some(m));
    }

    app.rt_voice = None;
    app.cfg.voice.mode = "wake".into();
    assert_eq!(app.startup_voice(true).as_deref(), Some("wake"));
    app.rt_voice = Some("nonsense".into());
    assert_eq!(app.startup_voice(false), None);

    app.voice_at_quit = Some("open".into());
    app.save_state();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut next = App::new(crate::config::Config::default(), tx);
    next.restore(crate::state::AppState::load().unwrap());
    assert_eq!(next.startup_voice(false).as_deref(), Some("open"));

    app.voice_at_quit = Some("off".into());
    app.save_state();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut again = App::new(crate::config::Config::default(), tx);
    again.restore(crate::state::AppState::load().unwrap());
    assert_eq!(again.startup_voice(false), None);
    let _ = std::fs::remove_dir_all(home);
}
