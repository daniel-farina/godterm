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

    let v = app.control_call("speaker", &json!({"speak_typed": true}));
    assert_eq!(v["ok"], json!(true), "{v}");
    answer(&mut app, false, "All quiet.");
    assert_eq!(app.assistant.spoken.last().unwrap(), "All quiet.");

    let t = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(t.contains("speak_typed = true"), "{t}");

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

    let v = app.control_call("speak", &json!({"text": "Hello there."}));
    assert!(v.to_string().contains("shown as text"), "{v}");
    assert!(app.assistant.log.iter().any(|e| e.text == "Hello there."));

    let t = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(t.contains("speaker_muted = true"), "{t}");

    let v = app.control_call("speaker", &json!({"muted": false}));
    assert_eq!(v["result"]["say"], json!("Okay, I'm talking again."));
    answer(&mut app, true, "Back.");
    assert_eq!(app.assistant.spoken, vec!["Back."]);

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

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = std::time::Instant::now();
    while !f() {
        assert!(
            t.elapsed() < std::time::Duration::from_secs(10),
            "waited for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn silent_preview(app: &mut crate::app::App) -> std::sync::Arc<crate::voice::player::Mix> {
    let (sp, mix) = crate::voice::tts::Speaker::start_silent(&app.cfg.voice);
    app.voice.preview = Some(sp);
    mix
}

fn said(app: &crate::app::App) -> Vec<String> {
    app.voice.preview.as_ref().unwrap().said()
}

fn muting_cuts_the_reply(app: &mut crate::app::App, mix: &crate::voice::player::Mix) {
    wait_until("the first audio", || mix.pending() >= 4800);
    assert!(mix.render(1200).iter().any(|x| *x > 0.0), "it is playing");
    assert!(!said(app).iter().any(|s| s.contains("Second")), "mid reply");

    let v = app.control_call("speaker", &json!({"muted": true}));
    assert_eq!(v["ok"], json!(true), "{v}");
    wait_until("the queue to empty", || mix.pending() == 0);
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(mix.pending(), 0, "the rest is not synthesized");
    assert!(
        mix.render(1200).iter().all(|x| *x == 0.0),
        "silence at once"
    );
    assert!(!said(app).iter().any(|s| s.contains("Second")));

    let v = app.control_call("speaker", &json!({"muted": false}));
    assert_eq!(v["ok"], json!(true), "{v}");
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(mix.pending(), 0, "the old reply does not come back");
    assert!(!said(app).iter().any(|s| s.contains("Second")));
}

#[test]
fn muting_mid_reply_stops_the_rest_of_an_assistant_reply() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("midreply");
    app.cfg.assistant.spoken_sentences = 3;
    let mix = silent_preview(&mut app);
    answer(
        &mut app,
        true,
        "The build passed on every account. Second, the deploy waits for you.",
    );
    assert_eq!(app.assistant.spoken.len(), 1);
    muting_cuts_the_reply(&mut app, &mix);

    answer(&mut app, true, "A fresh answer.");
    wait_until("the next reply", || mix.pending() >= 1200);
    assert!(said(&app).iter().any(|s| s.contains("fresh")));
    app.voice.preview.as_ref().unwrap().stop();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn muting_mid_reply_stops_the_rest_of_a_direct_reply() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("middirect");
    app.cfg.voice.direct_reply = "full".into();
    let mix = silent_preview(&mut app);
    app.direct_say_reply(
        "api",
        "The tests pass on the branch. Second, the lint step still fails.",
    );
    muting_cuts_the_reply(&mut app, &mix);
    app.voice.preview.as_ref().unwrap().stop();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_volume_change_mid_reply_is_heard_at_once_and_mute_still_stops_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("midvolume");
    app.cfg.voice.tts_volume = 0.8;
    app.cfg.assistant.spoken_sentences = 3;
    let mix = silent_preview(&mut app);
    let (v, _ctl) = crate::voice::Voice::fake(false);
    answer(
        &mut app,
        true,
        "The build passed on every account. Second, the deploy waits for you.",
    );
    wait_until("the first audio", || mix.pending() >= 4800);
    let a = mix.render(1200);
    assert!(a.iter().all(|x| (x - 0.8).abs() < 1e-6), "{:?}", &a[..4]);

    app.voice.engine = Some(v);
    let r = app.control_call("speaker", &json!({"volume": 40}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert!(!app.cfg.voice.speaker_muted, "only the volume changed");
    assert!(
        app.voice.preview.is_some(),
        "the speaker that is talking is kept"
    );
    assert!((app.voice.engine.as_ref().unwrap().tts_volume() - 0.4).abs() < 1e-6);
    let b = mix.render(2400);
    assert!(
        b[600..].iter().all(|x| (x - 0.4).abs() < 1e-6),
        "what was queued at 80% plays at 40%: {:?}",
        &b[600..604]
    );

    let r = app.control_call("speaker", &json!({"volume": 0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert!(!app.cfg.voice.speaker_muted, "zero does not flip the mute");
    assert!(app.cfg.voice.tts, "zero does not turn talk back off");
    wait_until("more audio", || mix.pending() >= 2400);
    assert!(mix.render(2400)[600..].iter().all(|x| *x == 0.0));

    let stops = app.voice.engine.as_ref().unwrap().speech_stops();
    app.control_call("speaker", &json!({"muted": true}));
    assert_eq!(app.voice.engine.as_ref().unwrap().speech_stops(), stops + 1);
    wait_until("the queue to empty", || mix.pending() == 0);
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        mix.pending(),
        0,
        "mute after a volume change still stops it"
    );
    app.voice.engine = None;
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

    let v = app.control_call(
        "set_setting",
        &json!({"key": "assistant.style", "value": "chatty"}),
    );
    assert_eq!(
        app.cfg.assistant.style, "concise",
        "not changed without a yes: {v}"
    );

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

    assert_eq!(app.cfg.assistant.panel, "docked");
    app.assistant.show = true;
    draw(&mut app, &mut t);
    assert_ne!(pty_sizes(&app), closed, "docked resizes the panes");
    let right = app.pane_rects.iter().map(|r| r.x + r.width).max().unwrap();
    assert!(right <= 200 - 44, "no pane under the panel");

    app.set_panel_mode("overlay").unwrap();
    app.assistant.show = false;
    draw(&mut app, &mut t);
    assert_eq!(pty_sizes(&app), closed);
    let under = t.backend().buffer().clone();
    app.assistant.show = true;
    draw(&mut app, &mut t);
    assert_eq!(pty_sizes(&app), closed, "opening it resizes nothing");
    assert!(app.panel_overlay.get());

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

    assert_eq!(
        t.backend().buffer()[(pr.x - 1, pr.y + 3)].bg,
        crate::app_talkback::SHADOW
    );

    app.on_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: pr.x + 5,
        row: pr.y + 6,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert!(app.assistant_has_focus());

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

    for a in 1..app.cfg.accounts.len() {
        app.cfg
            .closed_accounts
            .push(app.cfg.accounts[a].name.clone());
    }
    let mut wide = term(220, 40);
    draw(&mut app, &mut wide);
    assert!(!app.panel_overlay.get(), "220 wide: docked");

    let mut narrow = term(120, 40);
    draw(&mut app, &mut narrow);
    assert!(app.panel_overlay.get(), "120 wide: overlay");
    draw(&mut app, &mut wide);
    assert!(!app.panel_overlay.get(), "back to docked");

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
        format!(" assistant · Claude · {name} · 5h 89% · wk 3% · sonnet ▾ ")
    );

    let warn: Vec<&str> = p
        .iter()
        .filter(|(_, t)| *t == 2)
        .map(|(s, _)| s.as_str())
        .collect();
    assert_eq!(warn, vec!["wk 3%"]);

    let full = joined(&p).chars().count();
    let p = crate::ui_assistant::title_parts(&app, full - 2);
    assert_eq!(
        joined(&p),
        format!(" assistant · Claude · {name} · wk 3% · sonnet ▾ ")
    );
    let p = crate::ui_assistant::title_parts(&app, full - 12);
    assert_eq!(joined(&p), format!(" Claude · {name} · wk 3% · sonnet ▾ "));

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

    app.cfg.assistant.provider = "grok".into();
    let gh = crate::providers::grok::home();
    std::fs::create_dir_all(&gh).unwrap();
    std::fs::write(gh.join("auth.json"), "{\"key\":\"x\"}").unwrap();
    app.own_usage.insert("grok", Ok(u));
    let p = crate::ui_assistant::title_parts(&app, 200);
    let t: String = p.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        t,
        " assistant · Grok · wk 13% · Build 20% · grok-4.7-build-fast ▾ "
    );

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

    let e1 = es.iter().find(|e| e.label == d(1)).unwrap();
    assert!(e1.info.iter().any(|(t, tone)| t == "wk 3%" && *tone == 2));
    assert!(e1.info.iter().any(|(t, tone)| t == "5h 89%" && *tone == 0));

    let best = info("The one with the most left");
    assert!(best.starts_with(&format!("now {} · ", d(0))), "{best}");

    assert_eq!(info("Haiku"), "quickest");
    assert_eq!(info("Sonnet 5.5"), "balanced");
    assert_eq!(info("Opus"), "thorough");
    assert_eq!(info("Fable"), "most capable");
    let g = crate::providers::by_id("grok");
    assert_eq!(g.models[0].2, "quickest");

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

#[test]
fn enter_bursts_are_timed_when_read_not_when_handled() {
    use crate::app::AppEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("burst");
    app.assistant.show = true;
    app.assistant.focused = true;
    let key = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);

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

    for c in "ab".chars() {
        app.handle(AppEvent::KeyRead(key(KeyCode::Char(c)), true));
    }
    app.handle(AppEvent::KeyRead(key(KeyCode::Enter), true));
    app.handle(AppEvent::KeyRead(key(KeyCode::Char('c')), true));
    assert_eq!(app.assistant.input, "ab\nc");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn grok_panel_needs_no_account_and_names_the_chosen_model() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("grokpanel");
    let joined = |p: &[(String, u8)]| p.iter().map(|(t, _)| t.as_str()).collect::<String>();
    app.cfg.assistant.provider = "grok".into();

    let acct = crate::ui_assistant::brain_account(&app);
    let (note, login) = crate::ui_assistant::login_note(&app, acct);
    assert!(login && note.contains("Grok needs its own login"), "{note}");
    assert!(joined(&crate::ui_assistant::title_parts(&app, 200)).contains("not signed in"));

    let gh = crate::providers::grok::home();
    std::fs::create_dir_all(&gh).unwrap();
    std::fs::write(gh.join("auth.json"), "{\"key\":\"x\"}").unwrap();
    let (note, login) = crate::ui_assistant::login_note(&app, None);
    assert!(!login && !note.contains("log in"), "{note}");
    let t = joined(&crate::ui_assistant::title_parts(&app, 200));
    assert!(
        !t.contains("not signed in") && !t.contains("account"),
        "{t}"
    );

    app.switch_assistant(None, None, Some("grok-4.6")).unwrap();
    assert!(joined(&crate::ui_assistant::title_parts(&app, 200)).contains("grok-4.6 ▾"));

    app.switch_assistant(Some("claude"), None, None).unwrap();
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = None;
    }
    let (note, _) = crate::ui_assistant::login_note(&app, crate::ui_assistant::brain_account(&app));
    assert_eq!(note, "log in a Claude account to use it");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_status_bar_names_grok_when_it_runs_the_assistant() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("grokchip");
    let chip = |app: &crate::app::App| {
        crate::ui::status_chips(app)
            .into_iter()
            .map(|c| c.text)
            .find(|t| t.starts_with("AI: "))
    };
    assert!(chip(&app).is_some_and(|t| !t.contains("Grok")));
    app.cfg.assistant.provider = "grok".into();
    assert_eq!(chip(&app).as_deref(), Some("AI: Grok · not signed in"));
    let gh = crate::providers::grok::home();
    std::fs::create_dir_all(&gh).unwrap();
    std::fs::write(gh.join("auth.json"), "{\"key\":\"x\"}").unwrap();
    let v = json!({
        "creditUsagePercent": 87.0,
        "currentPeriod": {"start": "2026-10-02T14:16:00Z", "end": "2099-10-09T14:16:00Z"},
    });
    app.own_usage
        .insert("grok", Ok(crate::harness::grok::parse_billing(&v)));
    assert_eq!(chip(&app).as_deref(), Some("AI: Grok · wk 13%"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_web_state_follows_the_speaker_and_marks_itself_for_a_push() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("webspk");
    let v = app.web_state()["voice"].clone();
    assert_eq!(v["speaker_muted"], json!(false));
    assert_eq!(v["typed_spoken"], json!(false));

    app.toggle_speaker();
    app.set_tts_volume(0.4);
    app.set_speak_typed(true);
    let v = app.web_state()["voice"].clone();
    assert_eq!(v["speaker_muted"], json!(true), "{v}");
    assert_eq!(v["volume_pct"], json!(40), "{v}");
    assert_eq!(v["typed_spoken"], json!(true), "{v}");

    let r = app.control_call("speaker", &json!({"muted": false, "volume": 70}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let v = app.web_state()["voice"].clone();
    assert_eq!(v["speaker_muted"], json!(false));
    assert_eq!(v["volume_pct"], json!(70));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_speaker_reads_muted_while_talk_back_is_off_and_unmuting_turns_it_on() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("spktts");
    crate::settings::write(
        &crate::config::Config::path(),
        &crate::settings::Key::Global("voice.tts"),
        Some(toml_edit::value(false)),
    )
    .unwrap();
    app.reload_config_now();
    assert!(!app.cfg.voice.tts);
    assert!(!app.cfg.voice.speaker_muted);

    let v = app.web_state()["voice"].clone();
    assert_eq!(v["speaker_muted"], json!(true), "{v}");
    let r = app.control_call("speaker", &json!({"volume": 60}));
    assert_eq!(r["result"]["speaker"]["muted"], json!(true), "{r}");
    assert!(!app.cfg.voice.tts, "a volume change leaves talk back off");

    let r = app.control_call("speaker", &json!({"muted": false}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["result"]["speaker"]["muted"], json!(false), "{r}");
    assert!(app.cfg.voice.tts, "unmuting turned talk back on");
    assert!(!app.cfg.voice.speaker_muted);
    let t = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(t.contains("tts = true"), "{t}");
    let v = app.web_state()["voice"].clone();
    assert_eq!(v["speaker_muted"], json!(false), "{v}");
    assert_eq!(v["volume_pct"], json!(60), "{v}");

    let r = app.control_call("speaker", &json!({"muted": true}));
    assert_eq!(r["result"]["speaker"]["muted"], json!(true), "{r}");
    assert!(app.cfg.voice.tts, "muting leaves talk back on");
    let r = app.control_call("speaker", &json!({"muted": false}));
    assert_eq!(r["result"]["speaker"]["muted"], json!(false), "{r}");
    let _ = std::fs::remove_dir_all(&home);
}

const OPTIONS_REPLY: &str = "I looked into the cache bug. The lock is taken twice on the hot path. Fixing it means touching the writer and the reader. The tests cover the writer only. The reader has no tests at all.\n\nOptions:\n1. Fix the reader now\n2. Add reader tests first\n\nWhich one do you want?";

const OPTIONS_ASKED: &str =
    "Options. One: Fix the reader now. Two: Add reader tests first. Which one do you want?";

#[test]
fn the_talk_level_is_unset_by_default_and_nothing_changes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("leveldefault");
    use crate::app_talkback::TalkLevel;
    assert_eq!(app.cfg.voice.talk_level, "");
    assert_eq!(TalkLevel::chosen(&app.cfg), None);
    assert_eq!(TalkLevel::of(&app.cfg), TalkLevel::Normal);
    assert_eq!(
        TalkLevel::spoken_max(&app.cfg),
        app.cfg.assistant.spoken_sentences
    );
    let line = app.speaker_state_line();
    assert!(!line.contains("spoken_level"), "{line}");
    assert!(line.ends_with("stops hearing)\n"), "{line}");

    app.direct_say_reply("api", OPTIONS_REPLY);
    assert_eq!(
        app.direct.said.last().unwrap(),
        &crate::app_direct::spoken_reply(OPTIONS_REPLY, 40)
    );
    assert!(app.direct.said.last().unwrap().contains("no tests at all"));

    let s = app.find_setting("voice.talk_level", None).unwrap();
    assert_eq!(crate::settings::display(&app.cfg, &s), "normal");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_talk_level_shapes_a_direct_reply_and_always_says_the_options() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("leveldirect");
    let mut lens = vec![];
    for (level, body) in [
        ("brief", "I looked into the cache bug."),
        (
            "normal",
            "I looked into the cache bug. The lock is taken twice on the hot path. Fixing it means touching the writer and the reader.",
        ),
        (
            "detailed",
            "I looked into the cache bug. The lock is taken twice on the hot path. Fixing it means touching the writer and the reader. The tests cover the writer only. The reader has no tests at all.",
        ),
    ] {
        let v = app.control_call("speaker", &json!({"talk_level": level}));
        assert_eq!(v["ok"], json!(true), "{v}");
        assert_eq!(v["result"]["speaker"]["talk_level"], json!(level));
        app.direct_say_reply("api", OPTIONS_REPLY);
        let said = app.direct.said.last().unwrap().clone();
        assert_eq!(said, format!("{body} {OPTIONS_ASKED}"), "{level}");
        lens.push(said.len());
    }
    assert!(lens[0] < lens[1] && lens[1] < lens[2], "{lens:?}");
    let shown: Vec<&str> = app
        .assistant
        .log
        .iter()
        .filter(|e| e.text.starts_with("api: "))
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(shown.len(), 3);
    assert!(
        shown.iter().all(|t| *t == shown[0]),
        "the text on screen does not change"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_assistant_is_told_the_talk_level_and_it_is_set_at_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("levelprompt");
    use crate::app_talkback::TalkLevel;
    let v = app.control_call(
        "set_setting",
        &json!({"key": "voice.talk_level", "value": "brief"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(v["result"].get("needs_confirmation").is_none(), "{v}");
    assert_eq!(app.cfg.voice.talk_level, "brief");
    let t = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(t.contains("talk_level = \"brief\""), "{t}");
    let line = app.speaker_state_line();
    assert!(line.contains("spoken_level: brief"), "{line}");
    assert!(line.contains("one short sentence"), "{line}");
    assert!(app.state_preamble().contains("spoken_level: brief"));
    assert_eq!(
        TalkLevel::spoken_max(&app.cfg),
        app.cfg.assistant.spoken_sentences
    );

    app.set_talk_level(TalkLevel::Detailed);
    let line = app.speaker_state_line();
    assert!(line.contains("spoken_level: detailed"), "{line}");
    assert_eq!(TalkLevel::spoken_max(&app.cfg), 0);
    answer(&mut app, true, "One. Two. Three. Four.");
    assert_eq!(app.assistant.spoken, vec!["One. Two. Three. Four."]);

    app.set_talk_level(TalkLevel::Normal);
    assert!(!app.speaker_state_line().contains("spoken_level"));
    app.assistant.spoken.clear();
    answer(&mut app, true, "One. Two. Three. Four.");
    assert_eq!(app.assistant.spoken, vec!["One. Two."]);

    let v = app.control_call("speaker", &json!({"talk_level": "loud"}));
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.contains("brief, normal or detailed")),
        "{v}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
