//! The "told the user" log: every channel records into it, the next
//! brain turn's state carries it once, UI hints stay out, the ring is
//! bounded, and "finished" notices come only from a real Working to
//! Ready transition. No speaker ever starts; a stub brain and a temp home.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::{Activity, PaneState};
use crate::told::{Channel, Source};

fn test_app(tag: &str) -> (App, PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-told-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.accounts[0].login.source = Some(CredSource::Keychain);
    (app, home)
}

fn texts(app: &App) -> Vec<(String, Vec<Channel>)> {
    app.told
        .borrow()
        .ring
        .iter()
        .map(|e| (e.text.clone(), e.channels.clone()))
        .collect()
}

#[test]
fn each_channel_records_into_told_user() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("channels");
    app.told.borrow_mut().fake_speaker = true;
    app.cfg.voice.speak_confirm = true;
    app.flash("Account 2 is not logged in");
    app.speak("approved");
    app.notify_user("godterm", "Account 3 needs approval");
    app.note("Remote Control dropped");
    app.tell_user("update", "An update is ready", Channel::Panel);
    let t = texts(&app);
    let has = |s: &str, c: Channel| t.iter().any(|(x, cs)| x == s && cs.contains(&c));
    assert!(has("Account 2 is not logged in", Channel::Flash), "{t:?}");
    assert!(has("approved", Channel::Spoken), "{t:?}");
    assert!(
        has("Account 3 needs approval", Channel::Notification),
        "{t:?}"
    );
    assert!(has("Remote Control dropped", Channel::Panel), "{t:?}");
    assert!(has("An update is ready", Channel::Panel), "{t:?}");
    // A muted speaker says nothing, so nothing is recorded as spoken.
    app.cfg.voice.speaker_muted = true;
    app.speak("denied");
    assert!(!texts(&app).iter().any(|(x, _)| x == "denied"));
    // The same line on two channels is one message.
    app.cfg.voice.speaker_muted = false;
    app.tell_user_from(
        "attention",
        "Account 1 finished",
        &[Channel::Flash, Channel::Notification],
        Source::default(),
    );
    let n = texts(&app)
        .iter()
        .filter(|(x, _)| x == "Account 1 finished")
        .count();
    assert_eq!(n, 1);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn held_speech_is_recorded_with_its_source_when_said() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("quiet");
    app.told.borrow_mut().fake_speaker = true;
    app.cfg.voice.announce = true;
    // The user just spoke: the update waits.
    app.quiet_user_spoke();
    let src = Source {
        tab: Some(42),
        account: Some(0),
        fact: Some("t42 state working to ready".into()),
    };
    app.told_from("attention", src.clone(), |a| {
        a.say_update(
            crate::app_quiet::Voice::Announce,
            "the trading room bot finished",
        )
    });
    assert!(texts(&app).is_empty(), "not said yet");
    app.assistant.quiet.spoke_at = Some(Instant::now() - Duration::from_secs(600));
    app.quiet_tick();
    let log = app.told.borrow();
    let e = log.ring.back().expect("said");
    assert_eq!(e.text, "the trading room bot finished");
    assert_eq!(e.channels, vec![Channel::Spoken]);
    assert_eq!(e.kind, "attention");
    assert_eq!(e.src, src);
    drop(log);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn told_user_reaches_the_next_brain_turn_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("turn");
    let rec = home.join("brain-in.txt");
    let stub = crate::test_stub::claude(
        &home.join("brain"),
        &[
            ("stdin_to", rec.display().to_string()),
            ("stdin_append", "1".into()),
        ],
    );
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    app.tell_user_from(
        "attention",
        "the hyper demo finished",
        &[Channel::Flash],
        Source {
            tab: Some(2079),
            account: Some(2),
            fact: Some("t2079 state working to waiting for input".into()),
        },
    );
    let snap = app.state_preamble();
    assert!(snap.contains("told_user"), "{snap}");
    assert!(snap.contains("the hyper demo finished"), "{snap}");
    assert!(
        snap.contains("t2079") && snap.contains("working to"),
        "{snap}"
    );
    // get_state has it too.
    let st = app.control_call("get_state", &serde_json::json!({}));
    assert_eq!(
        st["result"]["told_user"][0]["text"],
        serde_json::json!("the hyper demo finished"),
        "{st}"
    );
    app.ask_assistant("what is going on");
    std::thread::sleep(Duration::from_millis(300));
    let sent = std::fs::read_to_string(&rec).unwrap_or_default();
    assert!(
        sent.contains("the hyper demo finished") && sent.contains("t2079"),
        "{sent}"
    );
    // Sent once: the next turn's state starts after it.
    let snap = app.state_preamble();
    assert!(!snap.contains("told_user"), "{snap}");
    let st = app.control_call("get_state", &serde_json::json!({}));
    assert_eq!(
        st["result"]["told_user"][0]["since_your_last_turn"],
        serde_json::json!(false)
    );
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn ui_hints_stay_out_of_the_brain_context() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("hints");
    app.flash("Copied 40 characters");
    app.flash("Layout: grid (Ctrl-a L for the next one)");
    // Recorded...
    assert_eq!(app.told.borrow().ring.len(), 2);
    assert!(app.told.borrow().ring.iter().all(|e| e.low));
    // ...but never sent.
    let snap = app.state_preamble();
    assert!(
        !snap.contains("Copied") && !snap.contains("told_user"),
        "{snap}"
    );
    let st = app.control_call("get_state", &serde_json::json!({}));
    assert_eq!(st["result"]["told_user"], serde_json::json!([]));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_ring_is_bounded() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("ring");
    for i in 0..200 {
        app.flash(format!("message {i}"));
    }
    let log = app.told.borrow();
    assert_eq!(log.ring.len(), crate::told::RING);
    assert_eq!(log.ring.back().unwrap().text, "message 199");
    drop(log);
    // One turn's state carries only the newest few.
    let snap = app.state_preamble();
    assert!(snap.contains("message 199") && !snap.contains("message 160\""));
    assert!(snap.contains("earlier (get_state told_user)"), "{snap}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn finished_needs_a_real_working_to_ready_transition() {
    use crate::app::attention_worthy;
    let long = Duration::from_secs(5);
    assert!(attention_worthy(Activity::Working, Activity::Ready, long));
    // A launch or a restore: Starting first, never a finish.
    assert!(!attention_worthy(Activity::Starting, Activity::Ready, long));
    assert!(!attention_worthy(Activity::Idle, Activity::Ready, long));
    assert!(!attention_worthy(Activity::Exited, Activity::Ready, long));
    // A one tick flicker of the spinner is not a turn.
    assert!(!attention_worthy(
        Activity::Working,
        Activity::Ready,
        Duration::from_millis(100)
    ));
    assert!(attention_worthy(
        Activity::Working,
        Activity::Permission,
        Duration::ZERO
    ));
    assert!(!attention_worthy(
        Activity::Starting,
        Activity::Permission,
        long
    ));
}

fn show(app: &mut App, slot: usize, tab: usize, screen: &str) {
    let t = &mut app.panes[slot].tabs[tab];
    t.parser
        .lock()
        .unwrap()
        .process(format!("\x1b[2J\x1b[H{screen}").as_bytes());
}

#[test]
fn attention_fires_only_on_the_tabs_real_transition() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("attention");
    app.cfg.notifications = false;
    // A background tab (pane 2), running, just launched or restored.
    let slot = 1;
    let t = &mut app.panes[slot].tabs[0];
    t.state = PaneState::Running;
    t.set_activity(Activity::Starting);
    let uid = t.uid;
    app.focus = 0;
    // Starting to Ready (a restore coming up): nothing.
    show(&mut app, slot, 0, "? for shortcuts");
    app.tick();
    app.voice_tick();
    assert!(app.attention.is_empty());
    assert!(!texts(&app).iter().any(|(x, _)| x.contains("finished")));
    // A flicker of the spinner: nothing.
    show(&mut app, slot, 0, "esc to interrupt");
    app.tick();
    assert_eq!(app.panes[slot].tabs[0].activity, Activity::Working);
    show(&mut app, slot, 0, "? for shortcuts");
    app.tick();
    app.voice_tick();
    assert!(!texts(&app).iter().any(|(x, _)| x.contains("finished")));
    // A real turn: Working held, then Ready.
    show(&mut app, slot, 0, "esc to interrupt");
    app.tick();
    app.panes[slot].tabs[0].activity_since = Instant::now() - Duration::from_secs(5);
    show(&mut app, slot, 0, "? for shortcuts");
    // The tick sees the transition and announces it (voice_tick).
    app.tick();
    let log = app.told.borrow();
    let e = log
        .ring
        .iter()
        .find(|e| e.text.ends_with("finished"))
        .expect("finished notice");
    assert_eq!(e.kind, "attention");
    assert_eq!(e.src.tab, Some(uid));
    let fact = e.src.fact.clone().unwrap();
    assert!(fact.contains("working to waiting for input"), "{fact}");
    drop(log);
    // A notice for approval that is no longer asked is stale: dropped.
    app.attention.push(crate::app::Attention {
        slot,
        tab: 0,
        what: Activity::Permission,
        uid,
        from: Activity::Working,
    });
    app.voice_tick();
    assert!(!texts(&app)
        .iter()
        .any(|(x, _)| x.contains("needs approval")));
    for p in &mut app.panes {
        for t in &mut p.tabs {
            t.state = PaneState::Idle;
        }
    }
    let _ = std::fs::remove_dir_all(&home);
}
