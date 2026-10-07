//! Every path of the wake word: for each mode (wake, open mic, push to
//! talk, paused, after a resume, while the assistant talks, muted):
//! (a) the wake word alone answers and opens the follow up window, and
//! the next utterance runs; (b) the wake word with a command runs it;
//! (c) a command without the wake word runs only in open mic, push to
//! talk or the follow up window; (d) noise is ignored.

use crate::config::testing::LOCK as ENV_LOCK;
use crate::voice::wake_profile::UttStats;
use crate::voice::VoiceEvent;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-wake-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();
    // A stub brain: requests reach it, nothing real runs.
    let stub = crate::test_stub::claude(&home.join("brain"), &[]);
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    app.accounts[0].login.source = Some(CredSource::Keychain);
    app.cfg.voice.wake_words = vec!["hey god".into(), "god term".into()];
    (app, home)
}

const LONG: UttStats = UttStats {
    ms: 1500,
    mean_db: -30.0,
    peak_db: -20.0,
    speech_ms: 1300,
    stt_ms: 0,
    conf: None,
    no_speech: None,
    logprob: None,
};

fn hear(app: &mut crate::app::App, text: &str, ptt: bool) {
    // A short clip for the wake word alone: no length guard may drop it.
    let stats = if text.split_whitespace().count() <= 2 {
        UttStats {
            ms: 650,
            speech_ms: 420,
            ..LONG
        }
    } else {
        LONG
    };
    app.on_voice(VoiceEvent::Heard {
        text: text.into(),
        ptt,
        stats,
    });
}

/// The requests the assistant got, in order.
fn asked(app: &crate::app::App) -> Vec<String> {
    app.assistant
        .log
        .iter()
        .filter(|e| e.who == crate::app_assistant::Who::User)
        .map(|e| e.text.clone())
        .collect()
}

fn done_turn(app: &mut crate::app::App) {
    app.on_brain(crate::assistant::BrainEvent::Done {
        text: "Okay.".into(),
        cost: None,
        error: false,
    });
}

/// (a) to (d) in the current mode. `plain_runs`: a command without the
/// wake word runs here.
fn matrix(app: &mut crate::app::App, mode: &str, plain_runs: bool, ptt: bool) {
    // (a) the wake word alone: an answer and the follow up window.
    let n = app.voice.said_wake.len();
    hear(app, "Hey God.", ptt);
    assert_eq!(
        app.voice.said_wake.len(),
        n + 1,
        "{mode}: the wake word alone answers"
    );
    assert_eq!(app.voice.said_wake.last().unwrap(), "Yes?", "{mode}");
    assert_eq!(app.voice.action.as_deref(), Some("listening…"), "{mode}");
    assert!(
        app.voice.wake_until.is_some(),
        "{mode}: follow up window open"
    );
    assert!(
        asked(app).is_empty() || !asked(app).last().unwrap().is_empty(),
        "{mode}: nothing empty sent"
    );
    hear(app, "what is running in botmesh", false);
    assert_eq!(
        asked(app).last().map(String::as_str),
        Some("what is running in botmesh"),
        "{mode}: the follow up runs"
    );
    done_turn(app);
    // (b) the wake word with a command.
    hear(app, "God term, which tabs are waiting", ptt);
    assert_eq!(
        asked(app).last().map(String::as_str),
        Some("which tabs are waiting"),
        "{mode}: wake plus command"
    );
    done_turn(app);
    // (c) a command without the wake word, outside the window.
    app.voice.wake_until = None;
    let before = asked(app).len();
    hear(app, "how much usage is left on account two", ptt);
    assert_eq!(
        asked(app).len() > before,
        plain_runs,
        "{mode}: a plain command"
    );
    if plain_runs {
        done_turn(app);
    }
    // (d) noise.
    app.voice.wake_until = None;
    let before = asked(app).len();
    hear(app, "Thank you.", ptt);
    assert_eq!(asked(app).len(), before, "{mode}: noise is ignored");
}

#[test]
fn wake_word_mode() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("wake");
    app.voice.always_on = true;
    app.voice.open_mic = false;
    matrix(&mut app, "wake", false, false);
    assert_eq!(app.voice.action.as_deref(), Some("(no wake word, ignored)"));
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn open_mic_mode() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("open");
    app.start_open_mic();
    assert!(app.voice.open_mic);
    matrix(&mut app, "open mic", true, false);
    assert_eq!(app.voice.action.as_deref(), Some("(noise, ignored)"));
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn push_to_talk_mode() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("ptt");
    app.voice.always_on = false;
    app.voice.open_mic = false;
    // The wake word alone in a push to talk clip: "Yes?", and the mic
    // opens again for the command once that is said.
    hear(&mut app, "hey god", true);
    assert_eq!(app.voice.said_wake.last().unwrap(), "Yes?");
    assert!(
        app.voice.listen_after_speech,
        "listens again after the reply"
    );
    matrix(&mut app, "push to talk", true, true);
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn paused_then_wake_resumes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("paused");
    app.voice.always_on = true;
    // During a pause other speech is dropped.
    app.pause_listening(Some(60), None);
    hear(&mut app, "what is running in botmesh", false);
    assert!(app.paused() && asked(&app).is_empty());
    hear(&mut app, "Thank you.", false);
    assert!(app.paused());
    // The wake word alone resumes and answers "I'm back."
    hear(&mut app, "Hey god", false);
    assert!(!app.paused());
    assert_eq!(app.voice.said_wake.last().unwrap(), "I'm back.");
    assert!(app.voice.wake_until.is_some());
    hear(&mut app, "what is running in botmesh", false);
    assert_eq!(asked(&app).last().unwrap(), "what is running in botmesh");
    done_turn(&mut app);
    // The wake word with a command resumes and runs it.
    app.pause_listening(Some(60), None);
    hear(&mut app, "hey god, which tabs are waiting", false);
    assert!(!app.paused());
    assert_eq!(asked(&app).last().unwrap(), "which tabs are waiting");
    done_turn(&mut app);
    // "resume" resumes.
    app.pause_listening(Some(60), None);
    hear(&mut app, "resume", false);
    assert!(!app.paused());
    // Push to talk resumes.
    app.pause_listening(Some(60), None);
    hear(&mut app, "how much usage is left", true);
    assert!(!app.paused());
    assert_eq!(asked(&app).last().unwrap(), "how much usage is left");
    done_turn(&mut app);
    // The pause runs out on its own: the next wake word alone works.
    app.pause_listening(Some(60), None);
    app.voice.paused.as_mut().unwrap().until =
        std::time::Instant::now() - std::time::Duration::from_secs(1);
    app.pause_tick();
    assert!(!app.paused());
    matrix(&mut app, "after resume", false, false);
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn while_the_assistant_talks() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("talking");
    app.voice.always_on = true;
    // A reply is under way (barge-in brought this utterance).
    app.ask_assistant("what is everyone working on");
    assert!(app.assistant.busy);
    hear(&mut app, "hey god", false);
    assert_eq!(app.voice.said_wake.last().unwrap(), "Yes?");
    assert!(app.voice.wake_until.is_some());
    done_turn(&mut app);
    matrix(&mut app, "during talk back", false, false);
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn muted_hears_nothing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("muted");
    app.voice.always_on = true;
    app.set_muted(true);
    for (t, ptt) in [
        ("hey god", false),
        ("hey god, which tabs are waiting", false),
        ("which tabs are waiting", true),
        ("Thank you.", false),
    ] {
        hear(&mut app, t, ptt);
    }
    assert!(asked(&app).is_empty() && app.voice.said_wake.is_empty());
    assert!(app.voice.action.as_deref().unwrap().contains("Ctrl-a X"));
    // The Voice menu says how to unmute, and keeps the choice for then.
    app.set_voice_mode(3);
    assert!(app.voice.muted && !app.voice.open_mic);
    assert!(app
        .flash
        .as_ref()
        .unwrap()
        .0
        .contains("unmute with Ctrl-a X"));
    assert_eq!(app.voice.muted_prev, 3);
    let _ = std::fs::remove_dir_all(home);
}
