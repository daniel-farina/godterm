use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

use crate::app::App;
use crate::app_assistant::Who;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;
use crate::voice::VoiceEvent;

fn test_app(tag: &str) -> (App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home =
        std::env::temp_dir().join(format!("godterm-brainstorm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let cfg = crate::config::Config {
        claude_bin: Some(crate::test_stub::true_bin()),
        ..Default::default()
    };
    cfg.save().unwrap();
    let mut app = App::new(cfg, tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }

    app.voice.open_mic = true;
    app.voice.always_on = true;
    (app, home)
}

fn hear(app: &mut App, text: &str) {
    app.on_voice(VoiceEvent::Heard {
        text: text.into(),
        ptt: false,
        stats: crate::voice::wake_profile::UttStats {
            ms: 2000,
            speech_ms: 1800,
            ..Default::default()
        },
    });
}

fn sent_to_brain(app: &App) -> Vec<String> {
    app.assistant
        .log
        .iter()
        .filter(|e| e.who == Who::User)
        .map(|e| e.text.clone())
        .collect()
}

fn key(app: &mut App, c: char) {
    app.on_command(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

#[test]
fn utterances_accumulate_and_send_it_sends_one_message() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("accumulate");
    hear(&mut app, "Brainstorm mode.");
    assert!(app.brainstorm_on());
    hear(&mut app, "So I want to rework the cache and");
    hear(&mut app, "the queue should retry three times.");
    hear(&mut app, "Hold on.");
    hear(&mut app, "Also the logs are too noisy.");
    assert!(sent_to_brain(&app).is_empty(), "{:?}", sent_to_brain(&app));
    assert!(!app.assistant.busy);
    assert!(
        !app.paused(),
        "\"hold on\" is part of the draft, not a pause"
    );
    let d = app.assistant.input.clone();
    assert!(
        d.contains("rework the cache and the queue should retry")
            && d.contains("Hold on.")
            && d.ends_with("too noisy."),
        "{d}"
    );

    let modes = app.modes_line();
    assert!(
        modes.contains("brainstorm: on, 22 words drafted"),
        "{modes}"
    );
    let chip = app.brainstorm_chip().unwrap();
    assert!(
        chip.starts_with("● brainstorm, ") && chip.ends_with(", 22 words"),
        "{chip}"
    );

    app.on_voice(VoiceEvent::BargeIn {
        speech_ms: 500,
        stop_ms: 10,
    });
    assert!(app.voice.wake_until.is_none());
    hear(&mut app, "Send it.");
    assert!(!app.brainstorm_on());
    let sent = sent_to_brain(&app);
    assert_eq!(sent.len(), 1, "one message: {sent:?}");
    assert!(
        sent[0].starts_with("So I want to rework the cache and the queue")
            && sent[0].ends_with("Also the logs are too noisy."),
        "{}",
        sent[0]
    );
    assert!(!sent[0].contains("Send it"));
    assert!(app.assistant.input.is_empty());
    assert_eq!(app.brainstorm.sent.len(), 1);

    let (text, heard, _) = app.assistant.last_turn.clone().unwrap();
    assert_eq!(text, sent[0]);
    assert!(heard.is_some());
    assert!(app.assistant.brainstorm_note.is_none());
    assert!(!app.modes_line().contains("brainstorm: on"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn im_done_with_the_api_does_not_send() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("notdone");
    app.start_brainstorm(true);
    hear(&mut app, "I'm done with the API part, now the database.");
    hear(&mut app, "We compute the hash and send it");
    assert!(app.brainstorm_on());
    assert!(sent_to_brain(&app).is_empty());
    assert!(app.assistant.input.contains("I'm done with the API part"));
    assert!(app.assistant.input.contains("hash and send it"));

    hear(&mut app, "The schema is fine. That's it.");
    assert!(app.brainstorm_on());
    assert_eq!(
        app.brainstorm.said.last().map(String::as_str),
        Some("Send it?")
    );
    hear(&mut app, "No, not yet.");
    assert!(app.brainstorm_on());
    assert!(sent_to_brain(&app).is_empty());

    hear(&mut app, "And the index. I'm done.");
    assert_eq!(
        app.brainstorm.said.last().map(String::as_str),
        Some("Send it?")
    );
    hear(&mut app, "Yes.");
    assert!(!app.brainstorm_on());
    let sent = sent_to_brain(&app);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("And the index. I'm done."), "{}", sent[0]);

    app.start_brainstorm(true);
    hear(&mut app, "One more thing: rename the crate. Send it.");
    assert!(!app.brainstorm_on());
    let sent = sent_to_brain(&app);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1], "One more thing: rename the crate.");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn cancel_and_undo() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("cancel");
    app.start_brainstorm(true);
    hear(&mut app, "First idea is a plugin system.");
    hear(&mut app, "Cancel brainstorm.");
    assert!(!app.brainstorm_on());
    assert!(app.assistant.input.is_empty());
    assert!(sent_to_brain(&app).is_empty());
    assert!(app.brainstorm_undo_ready());

    assert!(crate::ui_panelwin::quick_items(&app)
        .iter()
        .any(|q| q.label == "Undo discard"));
    hear(&mut app, "Undo.");
    assert!(app.brainstorm_on());
    assert_eq!(app.assistant.input, "First idea is a plugin system.");

    hear(&mut app, "Discard it.");
    assert!(app.assistant.input.is_empty());
    app.menu_cmd(crate::menus::Cmd::BrainstormUndo);
    assert!(app.brainstorm_on());
    assert_eq!(app.assistant.input, "First idea is a plugin system.");

    app.assistant.show = true;
    app.assistant.focused = true;
    app.on_assistant_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.brainstorm_on());
    assert_eq!(app.assistant.input, "First idea is a plugin system.");
    assert!(sent_to_brain(&app).is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn enter_sends_and_read_it_back() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("enter");
    app.start_brainstorm(true);
    hear(&mut app, "The first part is about tests.");
    hear(&mut app, "The second part is about docs.");
    hear(&mut app, "Read it back.");
    let said = app.brainstorm.said.last().cloned().unwrap_or_default();
    assert!(said.contains("about docs."), "{said}");
    assert!(!app.assistant.input.contains("Read it back"));
    app.assistant.focused = true;
    app.assistant.show = true;
    app.assistant.ed().insert(" Typed too.");
    app.on_assistant_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.brainstorm_on());
    let sent = sent_to_brain(&app);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].ends_with("about docs. Typed too."), "{}", sent[0]);

    app.start_brainstorm(true);
    hear(&mut app, "Lots of loose thoughts here.");
    app.assistant.brainstorm_note = None;
    app.assistant.busy = false;
    hear(&mut app, "Organize it.");
    assert_eq!(sent_to_brain(&app).len(), 2);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_card_shows_on_first_use_only() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("card");
    assert!(!app.cfg.voice.brainstorm_explained);
    let say = app.start_brainstorm(true);
    assert_eq!(say, crate::app_brainstorm::EXPLAIN_SPOKEN);
    assert!(app.brainstorm.card);
    assert!(app.cfg.voice.brainstorm_explained);
    let rows = crate::ui_assistant::rows(&app, 60);
    let text: String = rows
        .iter()
        .flat_map(|r| r.parts.iter().map(|p| p.0.clone()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("Brainstorm mode"), "{text}");
    assert!(text.contains("'cancel brainstorm' to discard"), "{text}");
    app.leave_brainstorm();
    assert!(!app.brainstorm.card);
    let say = app.start_brainstorm(true);
    assert_ne!(say, crate::app_brainstorm::EXPLAIN_SPOKEN);
    assert!(!app.brainstorm.card, "not the second time");

    app.menu_cmd(crate::menus::Cmd::BrainstormInfo);
    assert!(app.brainstorm.card);
    app.menu_cmd(crate::menus::Cmd::BrainstormInfo);
    assert!(!app.brainstorm.card);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_quick_bar_button_and_key() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("quick");
    let items = crate::ui_panelwin::quick_items(&app);
    let b = items.iter().find(|q| q.label == "Brainstorm").unwrap();
    assert_eq!(b.key, "C-a B");
    assert_eq!(b.action, UiAction::Key('B'));
    assert!(items.iter().any(|q| q.label == "Brainstorm"
        && q.info.as_ref().map(|i| &i.0)
            == Some(&UiAction::Menu(crate::menus::Cmd::BrainstormInfo))));
    key(&mut app, 'B');
    assert!(app.brainstorm_on());
    assert!(crate::ui_panelwin::quick_items(&app)
        .iter()
        .any(|q| q.label == "Brainstorm on"));

    app.assistant.show = true;
    hear(&mut app, "Some words here for the chip.");
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 50)).unwrap();
    t.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let b = t.backend().buffer();
    let mut s = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            s.push_str(b[(x, y)].symbol());
        }
        s.push('\n');
    }
    assert!(s.contains("Brainstorm on C-a B"), "{s}");
    assert!(
        s.contains("● brainstorm, 0s, 6 words") || s.contains("● brainstorm, 1s, 6 words"),
        "{s}"
    );

    let info = UiAction::Menu(crate::menus::Cmd::BrainstormInfo);
    let ir = app
        .last_hits
        .regions
        .iter()
        .rev()
        .find(|r| r.action == info)
        .map(|r| r.rect)
        .expect("the ⓘ is on screen");
    assert_eq!(app.region_at(ir.x, ir.y).map(|r| r.action), Some(info));

    assert!(crate::ui::HELP.iter().any(|(k, _)| *k == "Ctrl-a B"));
    assert_eq!(
        crate::ui_chrome::help_action("Ctrl-a B"),
        Some(UiAction::Key('B'))
    );
    key(&mut app, 'B');
    assert!(!app.brainstorm_on());

    let v = app.control_call("brainstorm", &json!({"on": true}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(app.brainstorm_on());
    assert!(v["result"]["say"]
        .as_str()
        .unwrap()
        .starts_with("Brainstorm mode."));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn spaced_out_replies_are_not_fragments() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("spaced");
    for _ in 0..4 {
        if let Some(t) = app.brainstorm.recent.back_mut() {
            *t = Instant::now() - Duration::from_secs(20);
        }
        assert!(!app.brainstorm_suggest_check("Yes, do that."));
    }
    assert!(app.brainstorm.suggested.is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_suggestion_and_its_rate_limit() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("suggest");
    let tips = |app: &App| {
        app.brainstorm
            .said
            .iter()
            .filter(|s| s.as_str() == crate::app_brainstorm::SUGGEST)
            .count()
    };
    hear(&mut app, "What is everyone working on?");
    assert_eq!(tips(&app), 0, "a finished question");
    hear(&mut app, "So I was thinking about the cache and");
    assert_eq!(tips(&app), 1);

    assert_eq!(sent_to_brain(&app).len(), 2);
    app.assistant.busy = false;
    hear(&mut app, "and also the queue, um");
    assert_eq!(tips(&app), 1, "rate limited");

    app.brainstorm.suggested = Some(Instant::now() - Duration::from_secs(400));
    app.brainstorm.recent.clear();
    for t in ["Rename the crate.", "Then fix the build.", "Then the docs."] {
        app.assistant.busy = false;
        hear(&mut app, t);
    }
    assert_eq!(tips(&app), 2, "three fragments in a row");

    let before = sent_to_brain(&app).len();
    hear(&mut app, "Wait, I'm not done.");
    assert_eq!(sent_to_brain(&app).len(), before);
    hear(&mut app, "Don't suggest that again.");
    assert!(!app.cfg.voice.brainstorm_suggest);
    assert_eq!(sent_to_brain(&app).len(), before);
    app.brainstorm.suggested = None;
    app.assistant.busy = false;
    hear(&mut app, "So the next thing is the cache and");
    assert_eq!(tips(&app), 2, "turned off");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn long_silences_keep_it_open_and_the_timeout_asks() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("timeout");
    app.cfg.voice.open_mic_sleep_min = 1;
    app.start_brainstorm(true);
    hear(&mut app, "A thought about the scheduler.");
    app.voice.last_heard = Instant::now() - Duration::from_secs(300);
    app.open_mic_tick();
    assert!(app.voice.open_mic, "no sleep mid brainstorm");

    app.brainstorm.on.as_mut().unwrap().last_at = Instant::now() - Duration::from_secs(300);
    app.brainstorm_tick();
    assert!(app.brainstorm_on());

    app.brainstorm.on.as_mut().unwrap().last_at = Instant::now() - Duration::from_secs(601);
    app.brainstorm_tick();
    assert!(!app.brainstorm_on());
    assert_eq!(app.assistant.input, "A thought about the scheduler.");
    assert!(app.brainstorm.said.last().unwrap().ends_with("Send it?"));
    assert!(sent_to_brain(&app).is_empty());
    hear(&mut app, "Yes, send it.");
    let sent = sent_to_brain(&app);
    assert_eq!(sent, vec!["A thought about the scheduler.".to_string()]);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn wake_word_mode_drafts_without_the_wake_word() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("wake");
    app.voice.open_mic = false;
    hear(&mut app, "Brainstorm mode.");
    assert!(!app.brainstorm_on(), "no wake word: not for us");
    hear(&mut app, "Hey god, brainstorm mode.");
    assert!(app.brainstorm_on());
    hear(&mut app, "The first thing is the login flow.");
    hear(&mut app, "Hey god.");
    hear(&mut app, "Hey god, the second is the cache.");
    assert_eq!(
        app.assistant.input,
        "The first thing is the login flow. the second is the cache."
    );
    hear(&mut app, "Hey god, send it.");
    assert_eq!(sent_to_brain(&app).len(), 1);
    let _ = std::fs::remove_dir_all(&home);
}
