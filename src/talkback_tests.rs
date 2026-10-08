//! The speaker mute and typed versus spoken replies, the panel's docked /
//! overlay / auto modes, and the usage buckets in the panel's title and
//! its account menu.

use serde_json::json;

use crate::assistant::BrainEvent as B;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-talk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.cfg.save().unwrap();
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    (app, home)
}

fn term(w: u16, h: u16) -> ratatui::Terminal<ratatui::backend::TestBackend> {
    ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap()
}

fn draw(
    app: &mut crate::app::App,
    t: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) -> String {
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

/// One answer from the brain, as a turn that came in `heard` (voice) or
/// typed.
fn answer(app: &mut crate::app::App, heard: bool, text: &str) {
    app.assistant.turn_spoken = heard;
    app.assistant.busy = true;
    app.on_brain(B::Text(text.into()));
    app.on_brain(B::Done {
        text: String::new(),
        cost: None,
        error: false,
    });
}

fn usage(five: f64, week: f64) -> crate::usage::Usage {
    crate::usage::parse_usage(&format!(
        r#"{{"five_hour":{{"utilization":{five},"resets_at":"2099-10-09T17:29:00+00:00"}},"seven_day":{{"utilization":{week},"resets_at":"2099-10-13T08:00:00+00:00"}}}}"#
    ))
    .unwrap()
}

#[test]
fn spoken_turns_are_spoken_typed_ones_only_with_the_setting() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("typed");
    assert!(!app.cfg.assistant.speak_typed, "default off");
    answer(&mut app, false, "Two tabs are working.");
    assert!(
        app.assistant.spoken.is_empty(),
        "a typed message: text only"
    );
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("Two tabs are working.")));
    answer(&mut app, true, "One tab is waiting.");
    assert_eq!(app.assistant.spoken, vec!["One tab is waiting."]);
    // The setting: typed ones are spoken too.
    let v = app.control_call("speaker", &json!({"speak_typed": true}));
    assert_eq!(v["ok"], json!(true), "{v}");
    answer(&mut app, false, "All quiet.");
    assert_eq!(app.assistant.spoken.last().unwrap(), "All quiet.");
    // Kept in config.toml.
    let t = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(t.contains("speak_typed = true"), "{t}");
    // How the request came in decides: a typed one in start_turn.
    app.cfg.assistant.speak_typed = false;
    app.ask_assistant_from("what's up", None);
    assert!(!app.assistant.turn_spoken);
    app.ask_assistant_from("what's up", Some("what's up"));
    assert!(app.assistant.turn_spoken);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn muted_speaker_says_nothing_but_the_text_shows() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("mute");
    // "Be quiet": the speaker tool, at once, no yes; the mic keeps on.
    let v = app.control_call("speaker", &json!({"muted": true}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(v["result"].get("needs_confirmation").is_none());
    assert!(app.cfg.voice.speaker_muted);
    assert!(!app.voice.muted, "the mic is not muted");
    assert!(!app.paused(), "listening is not paused");
    answer(&mut app, true, "Spoken but silent.");
    assert!(app.assistant.spoken.is_empty());
    assert!(
        app.assistant.timing.first_audio.is_none(),
        "no audio started"
    );
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("Spoken but silent.")));
    // The speak tool shows the text instead.
    let v = app.control_call("speak", &json!({"text": "Hello there."}));
    assert!(v.to_string().contains("shown as text"), "{v}");
    assert!(app.assistant.log.iter().any(|e| e.text == "Hello there."));
    // Kept across a restart.
    let t = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(t.contains("speaker_muted = true"), "{t}");
    // "Speak again".
    let v = app.control_call("speaker", &json!({"muted": false}));
    assert_eq!(v["result"]["say"], json!("Okay, I'm talking again."));
    answer(&mut app, true, "Back.");
    assert_eq!(app.assistant.spoken, vec!["Back."]);
    // The state block says so.
    app.set_speaker_muted(true);
    let line = app.speaker_state_line();
    assert!(line.contains("speaker: muted"), "{line}");
    assert!(line.contains("typed replies spoken: no"), "{line}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn muting_mid_sentence_stops_the_player() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("midspeech");
    // A speaker that never loads a model (no engine): its queue and stop
    // are what count.
    app.voice.preview = Some(crate::voice::tts::Speaker::idle());
    assert!(app.voice.preview.as_ref().unwrap().speaking());
    let stops = app.voice.preview.as_ref().unwrap().stops();
    app.toggle_speaker();
    assert!(app.cfg.voice.speaker_muted);
    assert_eq!(
        app.voice.preview.as_ref().unwrap().stops(),
        stops + 1,
        "cut off at once"
    );
    assert!(!app.voice.preview.as_ref().unwrap().speaking());
    app.voice.preview = None;
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn volume_and_set_setting_run_at_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("volume");
    let v = app.control_call("speaker", &json!({"volume": "50%"}));
    assert_eq!(v["result"]["say"], json!("Volume 50%."), "{v}");
    assert!((app.cfg.voice.tts_volume - 0.5).abs() < 1e-6);
    assert_eq!(crate::app_talkback::parse_volume(&json!(0.3)), Some(0.3));
    assert_eq!(crate::app_talkback::parse_volume(&json!("half")), Some(0.5));
    assert_eq!(crate::app_talkback::parse_volume(&json!(150)), None);
    // set_setting on these keys: no confirmation.
    let v = app.control_call(
        "set_setting",
        &json!({"key": "voice.speaker_muted", "value": true}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(app.cfg.voice.speaker_muted);
    let v = app.control_call(
        "set_setting",
        &json!({"key": "assistant.panel", "value": "overlay"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(app.cfg.assistant.panel, "overlay");
    // Other keys still ask.
    let v = app.control_call(
        "set_setting",
        &json!({"key": "assistant.style", "value": "chatty"}),
    );
    assert_eq!(
        app.cfg.assistant.style, "concise",
        "not changed without a yes: {v}"
    );
    // Ctrl-a + and - nudge it.
    app.nudge_volume(0.1);
    assert!((app.cfg.voice.tts_volume - 0.6).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn panel_shows_the_speaker_toggle_and_the_status_chip() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("toggle");
    app.assistant.show = true;
    let mut t = term(160, 40);
    let s = draw(&mut app, &mut t);
    assert!(s.contains("sound on"), "{s}");
    let r = app
        .last_hits
        .find(&UiAction::SpeakerToggle)
        .map(|r| r.rect)
        .expect("a clickable toggle");
    app.on_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: r.x,
        row: r.y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert!(app.cfg.voice.speaker_muted);
    let s = draw(&mut app, &mut t);
    assert!(s.contains("● silent"), "{s}");
    assert!(s.contains("SILENT"), "the status bar chip");
    // Ctrl-a O toggles back.
    app.on_command(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('O'),
        crossterm::event::KeyModifiers::SHIFT,
    ));
    assert!(!app.cfg.voice.speaker_muted);
    let _ = std::fs::remove_dir_all(&home);
}

fn pty_sizes(app: &crate::app::App) -> Vec<(u16, u16)> {
    app.panes.iter().map(|p| p.cur().pty_size()).collect()
}

#[test]
fn docked_makes_room_overlay_keeps_pty_sizes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("overlay");
    let mut t = term(200, 50);
    draw(&mut app, &mut t);
    let closed = pty_sizes(&app);
    // Docked (the default): the panes make room.
    assert_eq!(app.cfg.assistant.panel, "docked");
    app.assistant.show = true;
    draw(&mut app, &mut t);
    assert_ne!(pty_sizes(&app), closed, "docked resizes the panes");
    let right = app.pane_rects.iter().map(|r| r.x + r.width).max().unwrap();
    assert!(right <= 200 - 44, "no pane under the panel");
    // Overlay: the PTYs keep the size they had with it closed.
    app.set_panel_mode("overlay").unwrap();
    app.assistant.show = false;
    draw(&mut app, &mut t);
    assert_eq!(pty_sizes(&app), closed);
    let under = t.backend().buffer().clone();
    app.assistant.show = true;
    draw(&mut app, &mut t);
    assert_eq!(pty_sizes(&app), closed, "opening it resizes nothing");
    assert!(app.panel_overlay.get());
    // Its cells hide the pane beneath: every cell is the panel's own.
    let pr = app
        .last_hits
        .find(&UiAction::AssistantFocus)
        .map(|r| r.rect)
        .unwrap();
    let buf = t.backend().buffer();
    let mut same = 0;
    for y in pr.y + 1..pr.y + pr.height - 1 {
        for x in pr.x + 1..pr.x + pr.width - 1 {
            let (a, b) = (&under[(x, y)], &buf[(x, y)]);
            assert_eq!(
                b.bg != ratatui::style::Color::Reset,
                true,
                "({x},{y}) not filled"
            );
            if a.symbol() != " " && a == b {
                same += 1;
            }
        }
    }
    assert_eq!(same, 0, "pane cells show through");
    // The shadow column left of it.
    assert_eq!(
        t.backend().buffer()[(pr.x - 1, pr.y + 3)].bg,
        crate::app_talkback::SHADOW
    );
    // A click inside goes to the panel, not the pane under it.
    app.on_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: pr.x + 5,
        row: pr.y + 6,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert!(app.assistant_has_focus());
    // No pane cursor through it once it gives the keys back.
    app.assistant.focused = false;
    draw(&mut app, &mut t);
    if let Some((cx, _)) = app.want_cursor.get() {
        assert!(cx + 1 < pr.x, "cursor at {cx} under the panel");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn auto_docks_while_panes_stay_wide_and_flips_on_resize() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("auto");
    let v = app.control_call("assistant_panel", &json!({"mode": "auto"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    app.set_layout("columns");
    app.assistant.show = true;
    // One pane only, wide screen: docked.
    for a in 1..app.cfg.accounts.len() {
        app.cfg
            .closed_accounts
            .push(app.cfg.accounts[a].name.clone());
    }
    let mut wide = term(220, 40);
    draw(&mut app, &mut wide);
    assert!(!app.panel_overlay.get(), "220 wide: docked");
    // Narrow: the pane would drop under 80 columns, so it floats.
    let mut narrow = term(120, 40);
    draw(&mut app, &mut narrow);
    assert!(app.panel_overlay.get(), "120 wide: overlay");
    draw(&mut app, &mut wide);
    assert!(!app.panel_overlay.get(), "back to docked");
    // The ⋯ menu has the three modes.
    let es = crate::menus::entries(&app, crate::menus::MenuId::AssistantMore);
    let labels: Vec<&str> = es.iter().map(|e| e.label.as_str()).collect();
    for l in ["Panel: docked", "Panel: floating", "Panel: auto"] {
        assert!(labels.contains(&l), "{labels:?}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn title_shows_provider_and_buckets_and_drops_whole_pieces() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("title");
    app.cfg.assistant.account = app.cfg.accounts[1].name.clone();
    app.accounts[1].usage = Some(usage(11.0, 97.0));
    let joined = |p: &[(String, u8)]| p.iter().map(|(t, _)| t.as_str()).collect::<String>();
    let p = crate::ui_assistant::title_parts(&app, 200);
    let name = app.cfg.accounts[1].display().to_string();
    assert_eq!(
        joined(&p),
        format!(" assistant · Claude · {name} · 5h 89% · wk 3% · haiku ▾ ")
    );
    // The binding bucket is low: a warning tone, on it alone.
    let warn: Vec<&str> = p
        .iter()
        .filter(|(_, t)| *t == 2)
        .map(|(s, _)| s.as_str())
        .collect();
    assert_eq!(warn, vec!["wk 3%"]);
    // Narrow: the 5h bucket goes, then "assistant"; the binding one stays.
    let full = joined(&p).chars().count();
    let p = crate::ui_assistant::title_parts(&app, full - 2);
    assert_eq!(
        joined(&p),
        format!(" assistant · Claude · {name} · wk 3% · haiku ▾ ")
    );
    let p = crate::ui_assistant::title_parts(&app, full - 12);
    assert_eq!(joined(&p), format!(" Claude · {name} · wk 3% · haiku ▾ "));
    // Not low: no warning.
    app.accounts[1].usage = Some(usage(11.0, 30.0));
    let p = crate::ui_assistant::title_parts(&app, 200);
    assert!(p.iter().all(|(_, t)| *t != 2));
    assert!(joined(&p).contains("5h 89% · wk 70%"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn grok_buckets_and_a_provider_with_no_usage() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("grokbuckets");
    let v = json!({
        "creditUsagePercent": 87.0,
        "currentPeriod": {"start": "2026-10-02T14:16:00Z", "end": "2099-10-09T14:16:00Z"},
        "productUsage": [{"product": "GrokBuild", "usagePercent": 80.0}],
    });
    let u = crate::harness::grok::parse_billing(&v);
    let g = crate::providers::by_id("grok");
    let b = (g.usage)(&u);
    let texts: Vec<String> = b.iter().map(crate::providers::bucket_text).collect();
    assert_eq!(texts, vec!["wk 13%", "Build 20%"]);
    assert!(b[0].binding && !b[1].binding);
    // As the assistant's own login: in the title.
    app.cfg.assistant.provider = "grok".into();
    app.own_usage.insert("grok", Ok(u));
    let p = crate::ui_assistant::title_parts(&app, 200);
    let t: String = p.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        t,
        " assistant · Grok · wk 13% · Build 20% · grok-4.7-build-fast ▾ "
    );
    // A provider that reports nothing: no buckets, no numbers.
    let none = crate::usage::Usage::default();
    assert!((g.usage)(&none).is_empty());
    app.own_usage.insert("grok", Ok(none));
    let p = crate::ui_assistant::title_parts(&app, 200);
    let t: String = p.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(t, " assistant · Grok · grok-4.7-build-fast ▾ ");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn account_menu_rows_say_more() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("godterm-talk-menu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let mut cfg = crate::config::Config::default();
    let mut five = cfg.accounts[0].clone();
    five.name = "account5".into();
    five.label = "Account 5".into();
    cfg.accounts.push(five);
    cfg.accounts[3].harness = "grok".into();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.save().unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(cfg, tx);
    for a in [0, 1, 3, 4] {
        app.accounts[a].login.source = Some(crate::creds::CredSource::Keychain);
    }
    app.accounts[1].usage = Some(usage(11.0, 97.0));
    app.accounts[0].usage = Some(usage(0.0, 10.0));
    app.accounts[4].usage = Some(usage(100.0, 10.0));
    let es = crate::menus::entries(&app, crate::menus::MenuId::AssistantAccount);
    let info = |label: &str| -> String {
        let e = es
            .iter()
            .find(|e| e.label == label)
            .unwrap_or_else(|| panic!("no row {label}"));
        e.info.iter().map(|(t, _)| t.as_str()).collect()
    };
    let d = |a: usize| app.cfg.accounts[a].display().to_string();
    assert!(
        info("Claude").starts_with("4 accounts · best: "),
        "{}",
        info("Claude")
    );
    assert!(info("Grok").starts_with("own login · "));
    let a1 = info(&d(1));
    assert!(a1.starts_with("5h 89% · wk 3%  ⚠ weekly resets "), "{a1}");
    assert!(info(&d(0)).starts_with("5h 100% · wk 90%"));
    assert_eq!(info(&d(2)), "logged out");
    assert_eq!(info(&d(3)), "Grok account");
    assert!(
        info(&d(4)).starts_with("out of usage until "),
        "{}",
        info(&d(4))
    );
    for a in [2, 3, 4] {
        assert!(es
            .iter()
            .find(|e| e.label == d(a))
            .unwrap()
            .disabled
            .is_some());
    }
    // The binding bucket is marked (and a warning when low).
    let e1 = es.iter().find(|e| e.label == d(1)).unwrap();
    assert!(e1.info.iter().any(|(t, tone)| t == "wk 3%" && *tone == 2));
    assert!(e1.info.iter().any(|(t, tone)| t == "5h 89%" && *tone == 0));
    // "The one with the most left" says which one that is now.
    let best = info("The one with the most left");
    assert!(best.starts_with(&format!("now {} · ", d(0))), "{best}");
    // Models with a word each.
    assert_eq!(info("Haiku"), "quickest");
    assert_eq!(info("Sonnet"), "balanced");
    assert_eq!(info("Opus"), "most capable");
    let g = crate::providers::by_id("grok");
    assert_eq!(g.models[0].2, "quickest");
    // Drawn: the info column fits, cut with … before the names.
    let d1 = d(1);
    app.assistant.show = true;
    app.open_menu(crate::menus::MenuId::AssistantAccount);
    let mut t = term(70, 40);
    let s = draw(&mut app, &mut t);
    assert!(s.contains(&d1), "{s}");
    let cut = crate::ui::fit_parts(&[("5h 89% · wk 3%".into(), 0)], 8);
    let cut: String = cut.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(cut, "5h 89% …");
    let _ = std::fs::remove_dir_all(&home);
}

/// Enter is a new line only when the input thread read it in a burst (a
/// paste typed out); handled late, as when a busy frame bunches keys up,
/// a typed Enter still sends.
#[test]
fn enter_bursts_are_timed_when_read_not_when_handled() {
    use crate::app::AppEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("burst");
    app.assistant.show = true;
    app.assistant.focused = true;
    let key = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
    // Typed slowly, handled all at once: the Enter sends.
    for c in "hi".chars() {
        app.handle(AppEvent::KeyRead(key(KeyCode::Char(c)), false));
    }
    app.handle(AppEvent::KeyRead(key(KeyCode::Enter), false));
    assert!(app.assistant.input.is_empty(), "sent");
    assert_eq!(
        app.assistant.input_hist.last().map(String::as_str),
        Some("hi")
    );
    app.assistant.brain = None;
    app.assistant.busy = false;
    // A paste typed out: the Enter inside it is a new line.
    for c in "ab".chars() {
        app.handle(AppEvent::KeyRead(key(KeyCode::Char(c)), true));
    }
    app.handle(AppEvent::KeyRead(key(KeyCode::Enter), true));
    app.handle(AppEvent::KeyRead(key(KeyCode::Char('c')), true));
    assert_eq!(app.assistant.input, "ab\nc");
    let _ = std::fs::remove_dir_all(&home);
}
