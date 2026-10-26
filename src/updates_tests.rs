//! Update mode end to end: a tab's transcript in a temp home grows, the
//! clock is the test's, and what update mode says (or shows) is read
//! from its record and the assistant panel. No mic, no speaker, no brain.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::app::App;
use crate::app_assistant::Who;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::Activity;
use crate::voice::VoiceEvent;

struct Rig {
    app: App,
    home: std::path::PathBuf,
    transcript: std::path::PathBuf,
    t0: Instant,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn rig(tag: &str) -> Rig {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-updates-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
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
    let work = home.join("hyper");
    std::fs::create_dir_all(&work).unwrap();
    let a = app.panes[0].account.unwrap();
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&work));
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("sess-u.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            text_line("Old work that is not news.", Some("end_turn"))
        ),
    )
    .unwrap();
    {
        let tab = &mut app.panes[0].tabs[0];
        tab.cwd = work;
        tab.session_id = Some("sess-u".into());
        tab.custom_name = Some("hyper".into());
        tab.activity = Activity::Working;
    }
    let t0 = Instant::now();
    app.updates.clock = Some(t0);
    Rig {
        app,
        home,
        transcript,
        t0,
    }
}

fn text_line(t: &str, stop: Option<&str>) -> String {
    json!({"type": "assistant", "message": {"id": t, "content": [{"type": "text", "text": t}], "stop_reason": stop}})
        .to_string()
}

fn todo_line(done: usize, total: usize, now: &str) -> String {
    let todos: Vec<_> = (0..total)
        .map(|i| {
            let status = if i < done {
                "completed"
            } else if i == done {
                "in_progress"
            } else {
                "pending"
            };
            json!({"content": format!("step {i}"), "status": status, "activeForm": if i == done { now.to_string() } else { format!("Doing {i}") }})
        })
        .collect();
    json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": format!("todo{done}"), "name": "TodoWrite", "input": {"todos": todos}}]}})
        .to_string()
}

fn append(r: &Rig, lines: &[String]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&r.transcript)
        .unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

/// Run the tick at `secs` on the test clock.
fn tick_at(r: &mut Rig, secs: u64) {
    r.app.updates.clock = Some(r.t0 + Duration::from_secs(secs));
    r.app.updates_tick();
}

/// What update mode put out: asked roundups as panel notes, unasked news
/// as events for the assistant (it is not said by GodTerm itself).
fn notes(app: &App) -> Vec<String> {
    app.assistant
        .log
        .iter()
        .filter(|e| e.who == Who::Note && e.text.starts_with("Update "))
        .map(|e| e.text.clone())
        .chain(
            app.assistant
                .events
                .events
                .iter()
                .filter(|e| e.kind == crate::app_events::EventKind::Update)
                .map(|e| format!("Update (event): {}", e.text)),
        )
        .collect()
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

/// Progress waits for the roundup and comes as one entry; old work is no
/// news; nothing new means nothing said.
#[test]
fn progress_comes_in_the_roundup_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = rig("roundup");
    let line = r.app.set_update_mode(true, false);
    assert!(line.contains("hyper"), "{line}");
    assert!(r.app.updates.card, "first use explains it");
    let chip = r.app.updates_chip().unwrap();
    assert_eq!(chip, "● updates · 1 project · next in 4m");
    append(
        &r,
        &[
            todo_line(3, 7, "Applying the migration"),
            text_line("I updated the schema in /src/db/schema.rs.", None),
        ],
    );
    tick_at(&mut r, 10);
    assert!(
        r.app.updates.said.is_empty(),
        "progress waits for the roundup"
    );
    tick_at(&mut r, 241);
    assert_eq!(
        r.app.updates.said,
        vec![
            "hyper: three of seven steps done, now applying the migration. It updated the schema."
                .to_string()
        ]
    );
    let n = notes(&r.app);
    assert_eq!(n.len(), 1, "one entry per roundup");
    assert!(n[0].ends_with("It updated the schema."), "{}", n[0]);
    assert!(!n[0].contains("Old work"));
    // Nothing new: silence at the next roundup.
    tick_at(&mut r, 500);
    assert_eq!(r.app.updates.said.len(), 1);
    assert_eq!(notes(&r.app).len(), 1);
}

/// An event is said at once, but never within the minimum gap.
#[test]
fn events_respect_the_minimum_gap() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = rig("gap");
    r.app.set_update_mode(true, false);
    append(&r, &[text_line("The tests pass now.", Some("end_turn"))]);
    r.app.panes[0].tabs[0].activity = Activity::Ready;
    tick_at(&mut r, 6);
    assert_eq!(
        r.app.updates.said,
        vec!["hyper finished: the tests pass now.".to_string()]
    );
    // Waiting for approval 30 s later: held for the gap.
    r.app.panes[0].tabs[0].activity = Activity::Permission;
    tick_at(&mut r, 36);
    assert_eq!(r.app.updates.said.len(), 1);
    tick_at(&mut r, 67);
    assert_eq!(r.app.updates.said.len(), 2);
    assert_eq!(r.app.updates.said[1], "hyper is waiting for your approval.");
}

/// What the announcer said ("hyper finished") is not said again; the
/// news of what was done still is.
#[test]
fn announcements_are_not_repeated() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = rig("dedupe");
    r.app.set_update_mode(true, false);
    append(
        &r,
        &[text_line("The deploy finished the plan.", Some("end_turn"))],
    );
    r.app.panes[0].tabs[0].activity = Activity::Ready;
    let uid = r.app.panes[0].tabs[0].uid;
    r.app.updates_note_announced(uid, false);
    tick_at(&mut r, 6);
    assert_eq!(
        r.app.updates.said,
        vec!["hyper: the deploy finished the plan.".to_string()]
    );
    // An approval that was announced and nothing else: nothing to add.
    r.app.panes[0].tabs[0].activity = Activity::Permission;
    r.app.updates_note_announced(uid, true);
    tick_at(&mut r, 100);
    tick_at(&mut r, 400);
    assert_eq!(r.app.updates.said.len(), 1);
}

/// Paused, brainstorming, direct mode, the user mid sentence or just
/// spoken: nothing is said; it all comes once the floor is free.
#[test]
fn silence_rules_hold_updates_back() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = rig("silence");
    r.app.set_update_mode(true, false);
    append(&r, &[text_line("Done with the cache.", Some("end_turn"))]);
    r.app.panes[0].tabs[0].activity = Activity::Ready;
    // Paused.
    r.app.pause_listening(Some(60), None);
    tick_at(&mut r, 6);
    assert!(r.app.updates.said.is_empty());
    assert!(r.app.updates_chip().unwrap().ends_with("waiting (paused)"));
    r.app.resume_listening("test");
    // Brainstorming.
    r.app.start_brainstorm(false);
    tick_at(&mut r, 12);
    assert!(r.app.updates.said.is_empty());
    r.app.leave_brainstorm();
    // Direct mode with another tab.
    r.app.direct.on = Some(crate::app_direct::DirectMode {
        uid: 999_999,
        name: "other".into(),
        since: chrono::Local::now(),
        log: vec![],
        last_act: Activity::Ready,
        last_reply: None,
        reply_wait: None,
        last_sent: None,
        confirm: None,
    });
    tick_at(&mut r, 18);
    assert!(r.app.updates.said.is_empty());
    r.app.direct.on = None;
    // The user is mid sentence, then has just spoken.
    r.app.voice.partial = Some("so the".into());
    tick_at(&mut r, 24);
    assert!(r.app.updates.said.is_empty());
    r.app.voice.partial = None;
    r.app.quiet_user_spoke();
    tick_at(&mut r, 30);
    assert!(r.app.updates.said.is_empty());
    // Quiet again: the held news, once.
    r.app.assistant.quiet.spoke_at = Some(Instant::now() - Duration::from_secs(30));
    tick_at(&mut r, 36);
    assert_eq!(
        r.app.updates.said,
        vec!["hyper finished: done with the cache.".to_string()]
    );
}

/// Unasked news is never said by GodTerm itself, voice on or off: it is
/// an event for the assistant, which tells the user.
#[test]
fn voice_off_shows_text_and_voice_on_says_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = rig("voiceoff");
    r.app.cfg.voice.tts = false;
    r.app.set_update_mode(true, false);
    append(&r, &[text_line("All green.", Some("end_turn"))]);
    r.app.panes[0].tabs[0].activity = Activity::Ready;
    tick_at(&mut r, 6);
    let said = "hyper finished: all green.".to_string();
    assert_eq!(r.app.updates.said, vec![said.clone()]);
    assert!(notes(&r.app)[0].ends_with(&said));
    assert!(!r.app.assistant.quiet.said.contains(&said), "not spoken");
    // Talk back on: still not said by GodTerm, handed to the assistant.
    r.app.cfg.voice.tts = true;
    r.app.panes[0].tabs[0].activity = Activity::Working;
    tick_at(&mut r, 12);
    append(
        &r,
        &[text_line("Now the docs are written.", Some("end_turn"))],
    );
    r.app.panes[0].tabs[0].activity = Activity::Ready;
    tick_at(&mut r, 80);
    let second = "hyper finished: now the docs are written.".to_string();
    assert_eq!(r.app.updates.said.last(), Some(&second));
    assert!(
        !r.app.assistant.quiet.said.contains(&second),
        "not said directly"
    );
    assert!(
        notes(&r.app).iter().any(|n| n.ends_with(&second)),
        "an event"
    );
    // The speaker muted: shown only.
    r.app.cfg.voice.speaker_muted = true;
    assert!(!r.app.updates_spoken());
}

/// The phrases: on, a roundup on demand, off; "quiet" only while on.
#[test]
fn spoken_phrases_turn_it_on_and_off() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = rig("phrases");
    // On demand with the mode off: the recent past counts.
    hear(&mut r.app, "What's the latest?");
    assert!(!r.app.updates_on());
    let last = r.app.updates.said.last().cloned().unwrap_or_default();
    assert!(last.contains("hyper"), "{last}");
    hear(&mut r.app, "Keep me posted.");
    assert!(r.app.updates_on());
    // Asked again, nothing new: how things stand (never "no news" unasked).
    hear(&mut r.app, "Any updates?");
    let last = r.app.updates.said.last().cloned().unwrap();
    assert!(last.starts_with("No news since the last update."), "{last}");
    assert!(last.ends_with("Hyper is still working."), "{last}");
    hear(&mut r.app, "Quiet.");
    assert!(!r.app.updates_on());
    hear(&mut r.app, "Update mode.");
    assert!(r.app.updates_on());
    hear(&mut r.app, "Stop updates.");
    assert!(!r.app.updates_on());
    assert!(r.app.updates_chip().is_none());
    // The tool.
    let v = r.app.control_call("update_mode", &json!({"on": true}));
    assert!(r.app.updates_on(), "{v}");
    let v = r.app.control_call("update_mode", &json!({"now": true}));
    assert!(v.to_string().contains("No news"), "{v}");
    assert!(r.app.modes_line().contains("update mode: on"));
}

/// Codex, Grok and the other CLIs have no transcript here: their screen
/// state is the news.
#[test]
fn other_clis_report_screen_state() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = rig("codex");
    let a = r.app.panes[0].account.unwrap();
    r.app.cfg.accounts[a].harness = "codex".into();
    r.app.panes[0].tabs[0].activity = Activity::Ready;
    r.app.set_update_mode(true, false);
    // A transcript next to it is not read for this harness.
    append(&r, &[text_line("Should not be read.", Some("end_turn"))]);
    r.app.panes[0].tabs[0].activity = Activity::Working;
    tick_at(&mut r, 6);
    assert!(
        r.app.updates.said.is_empty(),
        "started is progress: the roundup"
    );
    r.app.panes[0].tabs[0].activity = Activity::Ready;
    tick_at(&mut r, 12);
    assert_eq!(r.app.updates.said, vec!["hyper finished.".to_string()]);
    r.app.panes[0].tabs[0].activity = Activity::Permission;
    tick_at(&mut r, 80);
    assert_eq!(
        r.app.updates.said.last().unwrap(),
        "hyper is waiting for your approval."
    );
}
