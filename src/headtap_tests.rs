use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;

use serde_json::json;

use crate::app::{App, AppEvent};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::media_keys::{Backend, Cmd, Gate, MediaKeys, Press};
use crate::voice::{Ctl, Voice};

type Calls = Rc<RefCell<Vec<String>>>;

struct Fake(Calls);

impl Backend for Fake {
    fn pump(&mut self, listening: bool, _speaking: bool) {
        self.0
            .borrow_mut()
            .push(format!("pump listening={listening}"));
    }
    fn release(&mut self) {
        self.0.borrow_mut().push("release".into());
    }
}

fn test_app(tag: &str) -> (App, std::path::PathBuf, Calls) {
    let home = std::env::temp_dir().join(format!("godterm-tap-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.cfg.save().unwrap();
    let calls: Calls = Default::default();
    let c = Rc::clone(&calls);
    app.media = MediaKeys::with_factory(move |_tx| {
        c.borrow_mut().push("claim".into());
        Some(Box::new(Fake(Rc::clone(&c))) as Box<dyn Backend>)
    });
    (app, home, calls)
}

fn tap_on_app(tag: &str, speaking: bool) -> (App, std::path::PathBuf, Calls, Receiver<Ctl>) {
    let (mut app, home, calls) = test_app(tag);
    app.set_headphone_tap(true);
    let (v, rx) = Voice::fake(speaking);
    app.voice.engine = Some(v);
    app.voice.always_on = true;
    (app, home, calls, rx)
}

fn press(app: &mut App, cmd: Cmd) {
    app.media.gate = Gate::default();
    app.handle(AppEvent::MediaKey(Press {
        cmd,
        source: "test",
    }));
}

fn sent(rx: &Receiver<Ctl>) -> Vec<String> {
    rx.try_iter()
        .map(|c| match c {
            Ctl::Mic(on) => format!("mic {on}"),
            Ctl::PushToTalk => "push to talk".into(),
            Ctl::StopTalk => "stop talk".into(),
            Ctl::Shutdown => "shutdown".into(),
            _ => "other".into(),
        })
        .collect()
}

fn stops(app: &App) -> u64 {
    app.voice.engine.as_ref().unwrap().speech_stops()
}

#[test]
fn the_setting_claims_and_gives_back_the_remote_commands() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, calls) = test_app("setting");

    app.tick();
    assert!(!app.media.claimed());
    assert!(calls.borrow().is_empty());
    press(&mut app, Cmd::TogglePlayPause);
    assert!(app.voice.engine.is_none() && !app.voice.muted);

    app.set_headphone_tap(true);
    assert!(app.media.claimed());
    assert_eq!(*calls.borrow(), vec!["claim"]);
    let t = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(t.contains("headphone_tap = true"), "{t}");
    app.media_pump();
    assert!(calls.borrow().iter().any(|c| c.starts_with("pump")));

    app.set_headphone_tap(false);
    assert!(!app.media.claimed());
    assert_eq!(calls.borrow().last().map(String::as_str), Some("release"));

    app.cfg.voice.headphone_tap = true;
    app.tick();
    assert!(app.media.claimed());

    app.shutdown();
    assert!(!app.media.claimed());
    assert_eq!(calls.borrow().last().map(String::as_str), Some("release"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_assistant_turns_it_on_by_voice() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls) = test_app("voicecmd");

    let v = app.control_call(
        "set_setting",
        &json!({"key": "voice.headphone_tap", "value": true}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(v.to_string().contains("Headphone tap is on"), "{v}");
    assert!(app.cfg.voice.headphone_tap && app.media.claimed());
    let v = app.control_call(
        "set_setting",
        &json!({"key": "voice.headphone_tap", "value": "off"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(!app.cfg.voice.headphone_tap && !app.media.claimed());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_tap_toggles_the_mic() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, rx) = tap_on_app("toggle", false);
    assert!(app.tap_listening());
    press(&mut app, Cmd::TogglePlayPause);
    assert!(app.voice.muted, "the mic is off");
    assert!(app.voice.engine.is_some(), "the engine is kept");
    assert_eq!(sent(&rx), vec!["mic false"]);
    assert!(!app.tap_listening());
    press(&mut app, Cmd::TogglePlayPause);
    assert!(!app.voice.muted);
    assert_eq!(sent(&rx), vec!["mic true"]);
    assert!(
        app.voice.wake_until.is_some(),
        "the next words need no wake word"
    );
    assert!(app.tap_listening());

    press(&mut app, Cmd::Play);
    assert!(app.voice.muted);
    let _ = sent(&rx);
    app.toggle_mute();
    assert!(!app.voice.muted && app.voice.engine.is_some());
    assert_eq!(sent(&rx), vec!["mic true"]);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn pause_only_turns_the_mic_off() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, rx) = tap_on_app("pause", false);
    press(&mut app, Cmd::Pause);
    assert!(app.voice.muted);
    let _ = sent(&rx);

    press(&mut app, Cmd::Pause);
    press(&mut app, Cmd::Stop);
    assert!(app.voice.muted, "still off");
    assert!(sent(&rx).is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn one_press_twice_counts_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, _rx) = tap_on_app("debounce", false);
    app.media.gate = Gate::default();

    app.handle(AppEvent::MediaKey(Press {
        cmd: Cmd::TogglePlayPause,
        source: "remote",
    }));
    app.handle(AppEvent::MediaKey(Press {
        cmd: Cmd::TogglePlayPause,
        source: "tap",
    }));
    assert!(app.voice.muted, "off once, not off and on again");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_tap_off_during_talk_back_lets_it_finish() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, rx) = tap_on_app("offtts", true);
    let before = stops(&app);
    press(&mut app, Cmd::TogglePlayPause);
    assert!(app.voice.muted);
    assert_eq!(stops(&app), before, "what is being said is not cut off");
    assert!(
        app.voice.engine.as_ref().unwrap().speaking(),
        "still talking"
    );
    assert_eq!(sent(&rx), vec!["mic false"], "only the mic, no shutdown");

    app.handle(AppEvent::Voice(crate::voice::VoiceEvent::BargeIn {
        speech_ms: 300,
        stop_ms: 5,
    }));
    assert_eq!(stops(&app), before);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_tap_on_during_talk_back_does_not_stop_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, rx) = tap_on_app("ontts", true);
    press(&mut app, Cmd::TogglePlayPause);
    let _ = sent(&rx);
    let before = stops(&app);
    press(&mut app, Cmd::TogglePlayPause);
    assert!(!app.voice.muted, "listening");
    assert_eq!(stops(&app), before, "the reply goes on");
    assert_eq!(sent(&rx), vec!["mic true"]);

    press(&mut app, Cmd::TogglePlayPause);
    assert!(app.voice.muted);
    let _ = sent(&rx);
    app.voice.always_on = false;
    press(&mut app, Cmd::TogglePlayPause);
    assert!(!app.voice.muted);
    assert!(app.voice.engine.is_some(), "not restarted over the reply");
    assert!(
        app.voice.open_mic_after_speech,
        "open mic once the reply ends"
    );
    assert_eq!(sent(&rx), vec!["mic true"], "no push to talk while talking");
    assert_eq!(stops(&app), before);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_tap_wakes_it_from_paused_or_asleep() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, _rx) = tap_on_app("wake", false);
    app.pause_listening(Some(120), None);
    assert!(app.paused() && !app.tap_listening());
    press(&mut app, Cmd::TogglePlayPause);
    assert!(!app.paused(), "resumed");
    assert!(app.voice.wake_until.is_some(), "and ready for a command");
    assert!(!app.voice.muted);

    app.voice.asleep = true;
    app.voice.wake_until = None;
    assert!(!app.tap_listening());
    press(&mut app, Cmd::Play);
    assert!(!app.voice.asleep);
    assert!(app.voice.wake_until.is_some());

    app.stop_voice();
    app.cfg.voice.mode = "wake".into();
    assert!(!app.tap_listening());
    press(&mut app, Cmd::TogglePlayPause);
    assert!(
        app.voice.always_on && app.voice.open_mic,
        "a tap always starts open mic, whatever voice.mode says"
    );
    assert!(app.voice.wake_until.is_some());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_tap_never_switches_open_mic_to_push_to_talk() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, _rx) = tap_on_app("openmic", false);

    app.cfg.voice.mode = "push".into();
    app.voice.open_mic = true;
    app.stop_voice();
    assert!(app.voice.engine.is_none());
    press(&mut app, Cmd::TogglePlayPause);

    assert!(
        app.voice.open_mic && app.voice.always_on,
        "open mic, not push to talk"
    );

    let (v, _rx2) = Voice::fake(false);
    app.voice.engine = Some(v);

    for _ in 0..3 {
        press(&mut app, Cmd::TogglePlayPause);
        assert!(app.voice.muted);
        assert_eq!(app.voice.muted_prev, 3, "remembered as open mic");
        press(&mut app, Cmd::TogglePlayPause);
        assert!(!app.voice.muted);
        assert!(app.voice.open_mic && app.voice.always_on);
    }

    press(&mut app, Cmd::TogglePlayPause);
    app.voice.always_on = false;
    app.voice.open_mic = false;
    press(&mut app, Cmd::Play);
    assert!(app.voice.open_mic && app.voice.always_on);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_mic_that_ends_by_itself_reopens_in_the_same_mode() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, _rx) = tap_on_app("reopen", false);
    app.cfg.voice.mode = "push".into();
    app.voice.open_mic = true;
    app.voice_mode_tick();
    assert_eq!(app.voice.last_on_mode, 3);

    app.voice.started_at = Some(std::time::Instant::now());
    app.on_voice(crate::voice::VoiceEvent::Status(
        crate::voice::VoiceStatus::Off(Some("microphone stream ended".into())),
    ));
    assert!(app.voice.reopen.is_none(), "no reopen loop after a reload");

    app.voice.started_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
    app.on_voice(crate::voice::VoiceEvent::Status(
        crate::voice::VoiceStatus::Off(Some("microphone stream ended".into())),
    ));
    let (_, mode, tries) = app.voice.reopen.expect("a reopen is planned");
    assert_eq!((mode, tries), (3, 1));

    app.voice.reopen = Some((std::time::Instant::now(), 3, 1));
    app.voice.open_mic = false;
    app.voice.always_on = false;
    app.voice_mode_tick();
    assert!(app.voice.open_mic && app.voice.always_on, "open mic again");

    app.voice.muted = true;
    app.voice.reopen = None;
    app.on_voice(crate::voice::VoiceEvent::Status(
        crate::voice::VoiceStatus::Off(Some("microphone stream ended".into())),
    ));
    assert!(app.voice.reopen.is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_headset_mute_gesture_closes_the_capture_only() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, rx) = tap_on_app("mutegesture", true);
    app.voice.open_mic = true;
    assert!(app.tap_listening());
    let before = stops(&app);
    let p =
        crate::voice::capture::helper_press(&crate::voice::capture::HelperEvent::InputMute(true))
            .unwrap();
    app.media.gate = Gate::default();
    app.handle(AppEvent::MediaKey(p));
    assert!(app.voice.muted && !app.voice.open_mic, "mic off");
    assert_eq!(
        sent(&rx),
        vec!["mic false"],
        "the capture closes, no shutdown"
    );
    assert!(app.voice.engine.is_some(), "the engine stays");
    assert_eq!(stops(&app), before, "the reply goes on");

    let p =
        crate::voice::capture::helper_press(&crate::voice::capture::HelperEvent::InputMute(true))
            .unwrap();
    app.media.gate = Gate::default();
    app.handle(AppEvent::MediaKey(p));
    assert!(app.voice.muted);
    assert!(sent(&rx).is_empty());

    press(&mut app, Cmd::TogglePlayPause);
    assert!(!app.voice.muted && app.voice.open_mic);
    assert_eq!(sent(&rx), vec!["mic true"]);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_mute_by_key_or_voice_closes_the_capture_only() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, rx) = tap_on_app("mutekey", true);
    app.voice.open_mic = true;
    let before = stops(&app);
    app.toggle_mute();
    assert!(app.voice.muted && app.voice.engine.is_some());
    assert_eq!(sent(&rx), vec!["mic false"]);
    assert_eq!(stops(&app), before, "the reply goes on");
    assert_eq!(app.voice_mode_now(), "off");
    app.toggle_mute();
    assert!(!app.voice.muted && app.voice.open_mic, "back to open mic");
    assert_eq!(sent(&rx), vec!["mic true"]);

    app.set_muted(true);
    assert_eq!(sent(&rx), vec!["mic false"]);
    assert_eq!(stops(&app), before);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn no_reopen_after_a_tap_off() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, rx) = tap_on_app("noreopen", false);
    app.voice.open_mic = true;

    app.voice.started_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
    app.voice_mode_tick();
    app.on_voice(crate::voice::VoiceEvent::Status(
        crate::voice::VoiceStatus::Off(Some("microphone stream ended".into())),
    ));
    assert!(app.voice.reopen.is_some(), "planned");
    press(&mut app, Cmd::TogglePlayPause);
    assert!(app.voice.muted);
    assert!(app.voice.reopen.is_none(), "the tap off drops it");
    let _ = sent(&rx);

    app.voice.reopen = Some((std::time::Instant::now(), 3, 1));
    app.voice_mode_tick();
    assert!(app.voice.muted && app.voice.engine.is_some());
    assert!(sent(&rx).is_empty(), "no restart, no mic");

    app.voice.reopen = None;
    app.on_voice(crate::voice::VoiceEvent::Status(
        crate::voice::VoiceStatus::Off(Some("microphone stream ended".into())),
    ));
    assert!(app.voice.reopen.is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_sleeping_open_mic_stays_open_mic() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _calls, _rx) = tap_on_app("sleepopen", false);
    app.voice.open_mic = true;
    app.voice_mode_tick();

    app.cfg.voice.open_mic_sleep_min = 1;
    app.voice.last_heard = std::time::Instant::now() - std::time::Duration::from_secs(120);
    let (v, _rx2) = Voice::fake(false);
    app.voice.engine = Some(v);
    app.open_mic_tick();
    assert!(!app.voice.open_mic && app.voice.open_mic_asleep);
    let (v, _rx3) = Voice::fake(false);
    app.voice.engine = Some(v);

    assert_eq!(app.voice_mode_now(), "open");
    app.voice_mode_tick();
    assert_eq!(app.voice.last_on_mode, 3);

    app.voice.wake_to_open_mic = true;
    app.voice_mode_tick();
    assert!(app.voice.open_mic && !app.voice.open_mic_asleep);

    let (v, _rx4) = Voice::fake(false);
    app.voice.engine = Some(v);
    app.set_voice_mode(2);
    assert!(!app.voice.open_mic_asleep);
    let _ = std::fs::remove_dir_all(&home);
}
