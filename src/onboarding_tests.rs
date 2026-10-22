//! The voice onboarding: the first run decision, the phrases, the step
//! machine, the install offer when voice pieces are missing, going on
//! without voice, and what the welcome and the step card show. Temp
//! homes only; nothing is installed, downloaded, heard or played.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, View};
use crate::app_onboarding::{Mark, Phase};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::onboarding::{self, classify, due, Outcome, Said, StepId, Steps, STEPS};

fn test_app(tag: &str) -> (App, std::path::PathBuf) {
    let home = std::env::temp_dir().join(format!("godterm-onb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    // Every voice piece is looked for in the test home only.
    let c = home.join("cache");
    app.cfg.voice.model = c
        .join("whisper/ggml-large-v3-turbo.bin")
        .display()
        .to_string();
    app.cfg.voice.kokoro_model = c.join("kokoro/kokoro-v1.0.onnx").display().to_string();
    app.cfg.voice.kokoro_voices = c.join("kokoro/voices-v1.0.bin").display().to_string();
    app.cfg.voice.ffmpeg = "ffmpeg".into();
    app.cfg.voice.whisper_server = "whisper-server".into();
    app.cfg.voice.whisper_cli = "whisper-cli".into();
    app.cfg.voice.espeak = "espeak-ng".into();
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.setup.bin_dir = Some(home.join("empty-bin"));
    app.setup.cache_dir = Some(c);
    app.setup.os = Some(crate::deps::Os::Mac);
    app.onb.apple_for_test = Some(false);
    (app, home)
}

/// Every voice piece present (fake programs and model files).
fn voice_ready(app: &mut App, home: &std::path::Path) {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for b in ["ffmpeg", "whisper-server", "espeak-ng"] {
        std::fs::write(bin.join(b), "#!/bin/sh\n").unwrap();
    }
    app.setup.bin_dir = Some(bin);
    for p in [
        &app.cfg.voice.model,
        &app.cfg.voice.kokoro_model,
        &app.cfg.voice.kokoro_voices,
    ] {
        let p = std::path::PathBuf::from(p);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(2_000_000).unwrap();
    }
    app.deps_changed();
}

fn key(app: &mut App, code: KeyCode) {
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Key(
        KeyEvent::new(code, KeyModifiers::NONE),
    )));
}

fn hear(app: &mut App, text: &str) -> bool {
    let n = crate::voice::grammar::normalize(text);
    app.onboarding_heard(text, &n)
}

fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    let a = buf.area;
    let mut out = String::new();
    for y in a.y..a.y + a.height {
        for x in a.x..a.x + a.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn render(app: &mut App) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 44)).unwrap();
    term.draw(|f| crate::ui::draw(f, app)).unwrap();
    buffer_text(term.backend().buffer())
}

#[test]
fn first_run_decision() {
    // A brand new home: once.
    assert!(due(false, true, None, false));
    // Existing users (a config was there): never on their own.
    assert!(!due(false, false, None, false));
    // Already done, skipped or stopped: never again on its own.
    for m in ["done 0.2.12", "skipped 0.2.12", "stopped 0.2.12"] {
        assert!(!due(false, true, Some(m), false));
    }
    // Tests, smoke runs, no terminal: never.
    assert!(!due(false, true, None, true));
    // --onboarding and a replay: always.
    assert!(due(true, false, Some("done"), false));
    assert!(due(true, false, None, true));
}

#[test]
fn the_marker_is_written_once_it_ends() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("marker");
    assert_eq!(onboarding::read_marker(), None);
    app.start_voice_onboarding(false);
    key(&mut app, KeyCode::Esc);
    assert!(app.onb.on.is_none());
    assert!(onboarding::read_marker().unwrap().starts_with("skipped"));
    // The marker lives in the test home, never the real one.
    assert!(onboarding::marker_path().starts_with(&home));
    // And due() reads it: no second time on its own.
    assert!(!due(
        false,
        true,
        onboarding::read_marker().as_deref(),
        false
    ));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn phrases_count_only_as_whole_utterances() {
    assert_eq!(classify("Yes."), Said::Yes);
    assert_eq!(classify("yeah, let's go"), Said::Other);
    assert_eq!(classify("Yes please"), Said::Yes);
    assert_eq!(classify("okay, so what now"), Said::Other);
    assert_eq!(classify("ok thank you"), Said::Yes);
    assert_eq!(classify("I heard you"), Said::Yes);
    assert_eq!(classify("No thanks"), Said::No);
    assert_eq!(classify("Skip."), Said::Skip);
    assert_eq!(classify("skip this step please"), Said::Skip);
    assert_eq!(classify("Next"), Said::Next);
    assert_eq!(classify("go back"), Said::Back);
    assert_eq!(classify("say that again"), Said::Repeat);
    assert_eq!(classify("Stop onboarding."), Said::Stop);
    assert_eq!(classify("um, skip the tour"), Said::Stop);
    // Requests stay requests.
    assert_eq!(classify("skip the tests in payments"), Said::Other);
    assert_eq!(classify("log in a new Claude account"), Said::Other);
    assert_eq!(classify("stop"), Said::Other);
    assert_eq!(classify(""), Said::Other);
    assert!(onboarding::is_start("Start the voice tour"));
    assert!(onboarding::is_start("voice onboarding"));
    assert!(!onboarding::is_start("what is voice onboarding"));
    assert!(onboarding::is_phrases("What can I say?"));
    assert_eq!(onboarding::voice_name("af_heart"), "Heart");
    assert_eq!(onboarding::meter(-60.0, 4), "░░░░");
    assert_eq!(onboarding::meter(0.0, 4), "████");
}

#[test]
fn the_step_machine() {
    let mut s = Steps::default();
    assert_eq!(s.current(), Some(StepId::Ask));
    assert_eq!(s.done(), Some(StepId::ClaudeLogin));
    assert_eq!(s.skip(), Some(StepId::Project));
    assert_eq!(s.back(), Some(StepId::ClaudeLogin));
    assert_eq!(s.marks[1], Mark::Todo);
    assert_eq!(s.done(), Some(StepId::Project));
    assert_eq!(s.done(), Some(StepId::GrokLogin));
    assert_eq!(s.skip(), Some(StepId::Ui));
    assert_eq!(s.done(), None);
    assert_eq!(s.current(), None);
    assert_eq!(s.count(Mark::Done), 4);
    assert_eq!(s.count(Mark::Skipped), 1);
    // Past the end nothing moves.
    assert_eq!(s.skip(), None);
    assert_eq!(s.count(Mark::Done) + s.count(Mark::Skipped), STEPS.len());
}

#[test]
fn missing_pieces_are_offered_and_voice_can_be_left_off() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("missing");
    let n = app.onb_needs();
    assert!(!n.can_hear);
    let ids: Vec<&str> = n.install.iter().map(|x| x.0).collect();
    assert!(
        ids.contains(&"whisper-model") && ids.contains(&"kokoro"),
        "{ids:?}"
    );
    // No package manager here: those are by hand, never sudo'd.
    assert!(n.manual.iter().any(|m| m.0 == "ffmpeg"), "{:?}", n.manual);
    // The speaker lock model is not part of it.
    assert!(!ids.contains(&"speaker"));
    assert!(n.bytes > 1_000_000_000);

    app.start_voice_onboarding(false);
    assert_eq!(app.onb_phase(), Some(Phase::Welcome));
    assert_eq!(app.onb.said[0], onboarding::HOOK_SPOKEN);
    // Enter: what voice needs, and the offer (nothing installed yet).
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.onb_phase(), Some(Phase::Install));
    let offer = app.onb.said.last().unwrap();
    assert!(offer.contains("Shall I install it?"), "{offer}");
    assert!(app.setup.running.is_empty(), "nothing runs before the yes");
    let screen = render(&mut app);
    assert!(screen.contains("Voice needs a few pieces"), "{screen}");
    assert!(screen.contains("Whisper model"));
    assert!(screen.contains("by hand"));
    assert!(screen.contains("Continue without voice"));
    // c: on without voice, the steps by typing.
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(app.onb_phase(), Some(Phase::Lesson));
    assert!(!app.onb.on.as_ref().unwrap().voice);
    assert_eq!(app.view, View::Grid);
    assert!(app.onb.said.last().unwrap().contains("Type:"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn nothing_installable_explains_and_goes_on_without_voice() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("manual");
    voice_ready(&mut app, &home);
    // Only espeak-ng is missing, and there is no package manager.
    std::fs::remove_file(home.join("bin/espeak-ng")).unwrap();
    app.deps_changed();
    let n = app.onb_needs();
    assert!(n.install.is_empty() && n.can_hear && !n.kokoro);
    app.start_voice_onboarding(false);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.onb_phase(), Some(Phase::Install));
    assert!(app.onb.said.last().unwrap().contains("can't install"));
    let screen = render(&mut app);
    assert!(screen.contains("cannot install these here"), "{screen}");
    // Enter (nothing to install) goes on without voice.
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.onb_phase(), Some(Phase::Lesson));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn spoken_walk_through_with_skips_and_completion_persisted() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("walk");
    voice_ready(&mut app, &home);
    app.start_voice_onboarding(true);
    // Noise is no answer; "yes" turns voice on: everything is there, so
    // straight to the mic check.
    assert!(hear(&mut app, "you"));
    assert_eq!(app.onb_phase(), Some(Phase::Welcome));
    assert!(hear(&mut app, "Yes."));
    assert_eq!(app.onb_phase(), Some(Phase::Mic));
    assert!(app.voice.open_mic, "open mic: no wake word needed");
    // Anything heard passes the mic check.
    assert!(hear(&mut app, "hello GodTerm"));
    assert_eq!(app.onb_phase(), Some(Phase::Speaker));
    assert!(app.onb.said.last().unwrap().contains("This is Heart"));
    assert!(hear(&mut app, "yes"));
    assert_eq!(app.onb_phase(), Some(Phase::Lesson));
    assert!(app.onb.said.last().unwrap().contains("how do loops work"));

    // Step 1: the assistant answered (a turn ran and ended).
    assert!(!app.onb_step_met());
    app.assistant_turn += 1;
    assert!(app.onb_step_met());
    app.onboarding_tick();
    assert_eq!(
        app.onb.on.as_ref().unwrap().steps.current(),
        Some(StepId::ClaudeLogin)
    );
    // A request goes on to the assistant, untouched here: no account is
    // added by the onboarding itself.
    let before = app.cfg.accounts.len();
    assert!(!hear(&mut app, "log in a new Claude account called Work"));
    assert_eq!(app.cfg.accounts.len(), before);
    assert!(app
        .onboarding_turn_note()
        .unwrap()
        .contains("needs_confirmation"));
    // A Claude account added (after its yes, by the tool): done.
    let mut a = app.cfg.accounts[0].clone();
    a.name = "work".into();
    app.cfg.accounts.push(a);
    app.accounts.push(Default::default());
    app.onboarding_tick();
    assert_eq!(
        app.onb.on.as_ref().unwrap().steps.current(),
        Some(StepId::Project)
    );
    // "skip" skips, "go back" goes back, "next" skips again.
    assert!(hear(&mut app, "skip"));
    assert_eq!(
        app.onb.on.as_ref().unwrap().steps.current(),
        Some(StepId::GrokLogin)
    );
    assert!(hear(&mut app, "go back"));
    assert_eq!(
        app.onb.on.as_ref().unwrap().steps.current(),
        Some(StepId::Project)
    );
    assert!(hear(&mut app, "next"));
    // A Grok account: done.
    let mut g = app.cfg.accounts[0].clone();
    g.name = "grok".into();
    g.harness = "grok".into();
    app.cfg.accounts.push(g);
    app.accounts.push(Default::default());
    app.onboarding_tick();
    assert_eq!(
        app.onb.on.as_ref().unwrap().steps.current(),
        Some(StepId::Ui)
    );
    // The lesson card shows the list with its marks.
    let screen = render(&mut app);
    assert!(screen.contains("voice tour · step 5 of 5"), "{screen}");
    assert!(screen.contains("✓ Ask how something works"));
    assert!(screen.contains("– Create a project in account 1"));
    assert!(screen.contains("▸ Change the layout or open loops"));
    assert!(screen.contains("“switch to the columns layout”"));
    // The layout changed: done, the recap.
    app.rt_layout = Some("columns".into());
    app.onboarding_tick();
    assert_eq!(app.onb_phase(), Some(Phase::Recap));
    assert!(render(&mut app).contains("phrases worth remembering"));
    // Enter: done, recorded, into the UI (the tour is not needed now).
    key(&mut app, KeyCode::Enter);
    assert!(app.onb.on.is_none());
    assert!(onboarding::read_marker().unwrap().starts_with("done"));
    assert!(crate::config::app_home().join("tour_done").exists());
    // The card comes back with Ctrl-a J, and closes with Esc.
    app.onboarding_ctrl_j();
    assert!(render(&mut app).contains("voice phrases"));
    key(&mut app, KeyCode::Esc);
    assert!(!app.onb.card);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn stop_onboarding_ends_it_and_typed_phrases_work() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("stop");
    app.start_voice_onboarding(false);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(app.onb_phase(), Some(Phase::Lesson));
    // Typed to the assistant: "skip" skips the step, not a request.
    assert!(app.onboarding_typed("skip"));
    assert_eq!(
        app.onb.on.as_ref().unwrap().steps.current(),
        Some(StepId::ClaudeLogin)
    );
    assert!(!app.onboarding_typed("how do loops work?"));
    assert!(hear(&mut app, "stop onboarding"));
    assert!(app.onb.on.is_none());
    assert!(onboarding::read_marker().unwrap().starts_with("stopped"));
    // Started again by voice: "voice onboarding" (push to talk here).
    assert!(hear(&mut app, "voice onboarding"));
    assert_eq!(app.onb_phase(), Some(Phase::Welcome));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_welcome_screen_and_its_reminder() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("welcome");
    app.start_voice_onboarding(false);
    let screen = render(&mut app);
    assert!(screen.contains(onboarding::HOOK_TITLE), "{screen}");
    assert!(screen.contains("Enter  Turn voice on"));
    assert!(screen.contains("Esc  Skip"));
    assert!(screen.contains("Nothing changes without your yes"));
    // Before any UI: none of the grid shows through.
    assert!(!screen.contains("Account 1"), "{screen}");
    assert!(!screen.contains(onboarding::REMINDER));
    // The buttons are the only things to click.
    assert!(app
        .last_hits
        .regions
        .iter()
        .all(|r| matches!(r.action, crate::hits::UiAction::Onboarding(_))));
    // 20 s without an answer: a gentle reminder, said and shown, no skip.
    if let Some(o) = &mut app.onb.on {
        o.phase_at -= Duration::from_secs(onboarding::REMIND_AFTER_S + 1);
    }
    app.onboarding_tick();
    assert_eq!(app.onb_phase(), Some(Phase::Welcome));
    assert!(app.onb.said.last().unwrap().starts_with("Still here?"));
    assert!(render(&mut app).contains("Still here? Press Enter to set up voice"));
    // A click on Skip: the normal UI.
    let r = app
        .last_hits
        .find(&crate::hits::UiAction::Onboarding(
            crate::app_onboarding::BTN_SKIP_ALL,
        ))
        .unwrap()
        .rect;
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: r.x + 1,
            row: r.y,
            modifiers: KeyModifiers::NONE,
        },
    )));
    assert!(app.onb.on.is_none());
    assert_eq!(
        onboarding::read_marker().unwrap().split(' ').next(),
        Some(Outcome::Skipped.word())
    );
    let ui = render(&mut app);
    assert!(!ui.contains(onboarding::HOOK_TITLE), "{ui}");
    // Skipped: the normal UI, with the first run tour.
    assert!(ui.contains("Account 2"), "{ui}");
    assert!(ui.contains("tour 1/"), "{ui}");
    let _ = std::fs::remove_dir_all(home);
}
