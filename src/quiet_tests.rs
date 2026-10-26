//! Tab answers that come back: a question taken into a busy tab's running
//! turn, the reply after the send, several questions one turn answers,
//! the wait limit (still working, or idle with nothing found), and
//! send_prompt's will_report. The quiet gate: updates held while the user
//! talks, just spoke, the assistant speaks, then merged at the quiet
//! moment; the gate re-arms; urgent ones wait only for the sentence.
//! A stub claude and temp transcripts; nothing real is touched.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_followup::FuState;
use crate::app_quiet::Voice;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::Activity;

fn test_app(tag: &str) -> (App, PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-quiet-{tag}-{}", std::process::id()));
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

/// A brain stub that records what it is sent.
fn with_brain(app: &mut App, home: &Path) -> PathBuf {
    let rec = home.join("brain-in.txt");
    let stub = crate::test_stub::claude(
        &home.join("brain"),
        &[
            ("stdin_to", rec.display().to_string()),
            ("stdin_append", "1".into()),
        ],
    );
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    rec
}

/// A tab named "claudego" with a transcript path.
fn tab(app: &mut App, home: &Path, sid: &str) -> (u64, PathBuf) {
    let proj = home.join("newtabs").join("claudego");
    std::fs::create_dir_all(&proj).unwrap();
    let t = app.panes[0].add_tab(proj.clone());
    let tb = &mut app.panes[0].tabs[t];
    tb.session_id = Some(sid.into());
    tb.custom_name = Some("claudego".into());
    tb.pending = Some(crate::pane::LaunchKind::Normal);
    tb.activity = Activity::Ready;
    let uid = tb.uid;
    let a = app.panes[0].account.unwrap();
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&proj));
    std::fs::create_dir_all(&dir).unwrap();
    (uid, dir.join(format!("{sid}.jsonl")))
}

fn set_activity(app: &mut App, uid: u64, a: Activity) {
    let (s, t) = app.find_tab(uid).unwrap();
    app.panes[s].tabs[t].activity = a;
}

fn stamp(secs_from_now: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs_from_now))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn write(path: &Path, lines: &[Value]) {
    let body: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    std::fs::write(path, body.join("\n") + "\n").unwrap();
}

/// A long turn that started before the questions; each question is taken
/// into it the way claude stores a message sent mid turn (the queue entry
/// removed as absorbed_mid_turn, then a queued_command attachment), and
/// with `reply` the turn ends with that text.
fn busy_turn(path: &Path, questions: &[&str], reply: Option<&str>) {
    let mut l = vec![
        json!({"type": "user", "message": {"role": "user", "content": "Build the voice restore feature."}, "timestamp": stamp(-600)}),
        json!({"type": "assistant", "message": {"id": "m1", "content": [{"type": "text", "text": "Working on it."}, {"type": "tool_use", "id": "x1", "name": "Bash", "input": {}}]}, "timestamp": stamp(-590)}),
    ];
    for (i, q) in questions.iter().enumerate() {
        l.push(json!({"type": "queue-operation", "operation": "enqueue", "timestamp": stamp(-5), "content": q}));
        l.push(json!({"type": "queue-operation", "operation": "remove", "timestamp": stamp(-5), "content": q, "reason": "absorbed_mid_turn"}));
        l.push(json!({"type": "attachment", "isSidechain": false, "attachment": {"type": "queued_command", "prompt": q, "commandMode": "prompt", "humanTurn": true}, "timestamp": stamp(-5)}));
        l.push(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": format!("x{}", i + 1), "content": "ok"}]}, "timestamp": stamp(-4)}));
        l.push(json!({"type": "assistant", "message": {"id": format!("m{}", i + 2), "content": [{"type": "tool_use", "id": format!("x{}", i + 2), "name": "Bash", "input": {}}]}, "timestamp": stamp(-4)}));
    }
    if let Some(r) = reply {
        l.push(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "xz", "content": "ok"}]}, "timestamp": stamp(1)}));
        l.push(json!({"type": "assistant", "message": {"id": "mz", "content": [{"type": "text", "text": r}]}, "timestamp": stamp(2)}));
    }
    write(path, &l);
}

/// A question sent while the tab was busy lands inside its running turn;
/// it is found there and reported when the turn ends.
#[test]
fn a_question_queued_into_a_busy_turn_is_reported() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("queued");
    let rec = with_brain(&mut app, &home);
    let (uid, path) = tab(&mut app, &home, "7777aaaa-0000-4000-8000-000000000001");
    let q = "Alice asks: will the microphone come back on after a restart?";
    set_activity(&mut app, uid, Activity::Working);
    app.register_follow_up(uid, q);
    busy_turn(&path, &[q], None);
    app.follow_ups_tick();
    assert!(app.assistant.follow_ups[0].seen_working);
    // Ready between tools, the turn not over: no answer yet.
    set_activity(&mut app, uid, Activity::Ready);
    app.follow_ups_tick();
    assert_eq!(app.assistant.follow_ups[0].state, FuState::Waiting);
    assert!(!app.assistant.busy);
    busy_turn(
        &path,
        &[q],
        Some("Yes: the voice mode is saved and comes back at launch."),
    );
    app.assistant.follow_ups[0].checked = None;
    app.follow_ups_tick();
    assert!(app.assistant.busy && app.assistant.system_turn);
    std::thread::sleep(Duration::from_millis(300));
    let sent = std::fs::read_to_string(&rec).unwrap_or_default();
    assert!(
        sent.contains("answered your earlier question") && sent.contains("comes back at launch"),
        "{sent}"
    );
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

/// No trace of the question text, but the tab worked after the send and
/// is ready: the reply written after the send is the answer. One written
/// before it is not.
#[test]
fn the_reply_after_the_send_is_the_answer() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("fallback");
    let (uid, path) = tab(&mut app, &home, "7777aaaa-0000-4000-8000-000000000002");
    app.cfg.claude_bin = Some(home.join("no-such-claude").display().to_string());
    app.register_follow_up(uid, "Is the build green?");
    app.assistant.follow_ups[0].seen_working = true;
    let turn = |at: String| {
        vec![
            json!({"type": "user", "message": {"role": "user", "content": "something else"}, "timestamp": stamp(-900)}),
            json!({"type": "assistant", "message": {"id": "a", "content": [{"type": "text", "text": "The build is green; all 812 tests pass."}]}, "timestamp": at}),
        ]
    };
    write(&path, &turn(stamp(-300)));
    app.follow_ups_tick();
    assert_eq!(
        app.assistant.follow_ups[0].state,
        FuState::Waiting,
        "written before the send"
    );
    write(&path, &turn(stamp(3)));
    app.assistant.follow_ups[0].checked = None;
    app.follow_ups_tick();
    match &app.assistant.follow_ups[0].state {
        FuState::Answered { text, .. } => assert!(text.contains("812 tests")),
        s => panic!("{s:?}"),
    }
    // Without seeing it work, the reply is not taken on time alone.
    let found = crate::app_followup::find_answer(
        &crate::sessions::follow_turns(&path, 40),
        "Is the build green?",
        chrono::Utc::now().timestamp(),
        false,
    );
    assert!(found.is_none());
    let _ = std::fs::remove_dir_all(&home);
}

/// Several questions to one busy tab, all taken into the same turn: one
/// report tells them together.
#[test]
fn several_questions_one_turn_one_report() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("several");
    let rec = with_brain(&mut app, &home);
    let (uid, path) = tab(&mut app, &home, "7777aaaa-0000-4000-8000-000000000003");
    let q1 = "Alice's change request: say Done when a task is finished.";
    let q2 = "Alice's refinement: start the reply with Done, then what was done.";
    set_activity(&mut app, uid, Activity::Working);
    app.register_follow_up(uid, q1);
    app.register_follow_up(uid, q2);
    app.follow_ups_tick();
    assert_eq!(
        app.assistant
            .follow_ups
            .iter()
            .filter(|f| f.state == FuState::Waiting)
            .count(),
        2,
        "the second question does not cancel the first"
    );
    busy_turn(
        &path,
        &[q1, q2],
        Some("Done: replies now start with Done. Commit 748dc06."),
    );
    set_activity(&mut app, uid, Activity::Ready);
    app.follow_ups_tick();
    assert!(app.assistant.busy, "one turn");
    assert!(app
        .assistant
        .follow_ups
        .iter()
        .all(|f| matches!(f.state, FuState::Reported { .. })));
    std::thread::sleep(Duration::from_millis(300));
    let sent = std::fs::read_to_string(&rec).unwrap_or_default();
    assert!(sent.contains("your earlier questions"), "{sent}");
    assert_eq!(sent.matches("Commit 748dc06").count(), 1, "{sent}");
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

/// At the wait limit a tab still working keeps being waited for, and the
/// user hears so once; a ready tab with nothing found is said plainly.
#[test]
fn the_wait_limit_never_drops_silently() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("limit");
    let (uid, path) = tab(&mut app, &home, "7777aaaa-0000-4000-8000-000000000004");
    app.cfg.assistant.answer_wait_min = 1;
    app.register_follow_up(uid, "What is the status of the headphone tap?");
    app.assistant.follow_ups[0].sent = Instant::now() - Duration::from_secs(61);
    set_activity(&mut app, uid, Activity::Working);
    app.follow_ups_tick();
    app.follow_ups_tick();
    assert_eq!(app.assistant.follow_ups[0].state, FuState::Waiting);
    let told: Vec<&String> = app
        .assistant
        .quiet
        .said
        .iter()
        .filter(|s| {
            s.contains("claudego is still working on your question; I'll tell you when it's done.")
        })
        .collect();
    assert_eq!(told.len(), 1, "{:?}", app.assistant.quiet.said);
    // Past the hard cap: stopped, and said.
    app.assistant.follow_ups[0].sent =
        Instant::now() - crate::app_followup::HARD_CAP - Duration::from_secs(1);
    app.follow_ups_tick();
    assert_eq!(app.assistant.follow_ups[0].state, FuState::TimedOut);
    assert!(app
        .assistant
        .quiet
        .said
        .last()
        .unwrap()
        .contains("stopped waiting"));
    // Idle at the limit with nothing found.
    write(
        &path,
        &[
            json!({"type": "user", "message": {"role": "user", "content": "other"}, "timestamp": stamp(-900)}),
        ],
    );
    app.register_follow_up(uid, "Did the deploy finish?");
    let n = app.assistant.follow_ups.len() - 1;
    app.assistant.follow_ups[n].sent = Instant::now() - Duration::from_secs(61);
    set_activity(&mut app, uid, Activity::Ready);
    app.follow_ups_tick();
    assert_eq!(app.assistant.follow_ups[n].state, FuState::TimedOut);
    assert_eq!(
        app.assistant.quiet.said.last().unwrap(),
        "claudego finished but I couldn't find its answer; read_tab has its last reply."
    );
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("couldn't find its answer")));
    let _ = std::fs::remove_dir_all(&home);
}

/// send_prompt says whether the answer will be reported; a question sent
/// without expect_reply is still followed.
#[test]
fn send_prompt_says_will_report() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("willreport");
    let (uid, _) = tab(&mut app, &home, "7777aaaa-0000-4000-8000-000000000005");
    let tid = format!("t{uid}");
    let v = app.control_call(
        "send_prompt",
        &json!({"_client": "brain", "tab": tid, "text": "What is left on the release?", "expect_reply": true}),
    );
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(v["will_report"], json!(true), "{v}");
    assert!(v["report_say"]
        .as_str()
        .unwrap()
        .contains("I'll tell you when claudego answers"));
    let v = app.control_call(
        "send_prompt",
        &json!({"_client": "brain", "tab": tid, "text": "Run the formatter.", "expect_reply": false}),
    );
    assert_eq!(v["will_report"], json!(false), "{v}");
    assert!(v["report_say"].as_str().unwrap().contains("can ask later"));
    // No flag, but a question: followed.
    let n = app.assistant.follow_ups.len();
    let v = app.control_call(
        "send_prompt",
        &json!({"_client": "brain", "tab": tid, "text": "Is the build green?"}),
    );
    assert_eq!(v["will_report"], json!(true), "{v}");
    assert_eq!(app.assistant.follow_ups.len(), n + 1);
    assert_eq!(app.assistant.follow_ups[n].question, "Is the build green?");
    // The tool description and the system prompt say when to promise.
    let p: String = crate::sysprompt::SECTIONS
        .iter()
        .map(|x| x.default)
        .collect();
    assert!(p.contains("only when the send_prompt result says will_report true"));
    let _ = std::fs::remove_dir_all(&home);
}

/// Held while the user talks (a partial, push to talk held), within N
/// seconds of what they said or typed, and while talk back plays; then
/// merged into one line at the quiet moment.
#[test]
fn updates_wait_for_a_quiet_moment_and_merge() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("gate");
    app.cfg.assistant.update_quiet_s = 4;
    app.voice.partial = Some("so what I want is".into());
    app.say_update(Voice::Reply, "hyper finished.");
    assert!(app.assistant.quiet.said.is_empty(), "not over a partial");
    app.voice.partial = None;
    app.voice.holding = Some(Instant::now());
    app.quiet_tick();
    assert!(
        app.assistant.quiet.said.is_empty(),
        "not while push to talk is held"
    );
    app.voice.holding = None;
    app.quiet_user_spoke();
    app.say_update(Voice::Reply, "claudego answered.");
    app.quiet_tick();
    assert!(
        app.assistant.quiet.said.is_empty(),
        "within N seconds of speech"
    );
    app.assistant.quiet.spoke_at = Some(Instant::now() - Duration::from_secs(5));
    app.quiet_user_typed();
    app.quiet_tick();
    assert!(
        app.assistant.quiet.said.is_empty(),
        "within N seconds of typing"
    );
    app.assistant.quiet.typed_at = Some(Instant::now() - Duration::from_secs(5));
    app.assistant.quiet.fake_tts = true;
    app.quiet_tick();
    assert!(
        app.assistant.quiet.said.is_empty(),
        "not while talk back plays"
    );
    app.assistant.quiet.fake_tts = false;
    app.assistant.busy = true;
    app.quiet_tick();
    assert!(app.assistant.quiet.said.is_empty(), "not mid turn");
    app.assistant.busy = false;
    app.quiet_tick();
    assert_eq!(
        app.assistant.quiet.said,
        vec!["hyper finished. claudego answered.".to_string()],
        "merged, once"
    );
    assert!(app.assistant.quiet.queue.is_empty());
    // The gate re-arms when the user speaks again.
    app.quiet_user_spoke();
    app.say_update(Voice::Announce, "t3 needs approval");
    assert_eq!(app.assistant.quiet.said.len(), 1);
    app.assistant.quiet.spoke_at = Some(Instant::now() - Duration::from_secs(5));
    app.quiet_tick();
    assert_eq!(
        app.assistant.quiet.said.last().unwrap(),
        "t3 needs approval"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A tab's answer waits too, and the updates that waited are told in the
/// same turn.
#[test]
fn answers_wait_for_quiet_and_carry_held_updates() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("gateturn");
    let rec = with_brain(&mut app, &home);
    let (uid, path) = tab(&mut app, &home, "7777aaaa-0000-4000-8000-000000000006");
    let q = "Is the release tagged?";
    app.register_follow_up(uid, q);
    app.assistant.follow_ups[0].seen_working = true;
    write(
        &path,
        &[
            json!({"type": "user", "message": {"role": "user", "content": q}, "timestamp": stamp(1)}),
            json!({"type": "assistant", "message": {"id": "a", "content": [{"type": "text", "text": "Yes, v2.1 is tagged."}]}, "timestamp": stamp(2)}),
        ],
    );
    app.quiet_user_spoke();
    app.say_update(Voice::Progress, "Remote Control is on.");
    app.follow_ups_tick();
    assert!(!app.assistant.busy, "the user just spoke");
    app.assistant.quiet.spoke_at = Some(Instant::now() - Duration::from_secs(5));
    app.follow_ups_tick();
    assert!(app.assistant.busy);
    assert!(app.assistant.quiet.queue.is_empty());
    std::thread::sleep(Duration::from_millis(300));
    let sent = std::fs::read_to_string(&rec).unwrap_or_default();
    assert!(
        sent.contains("v2.1 is tagged")
            && sent
                .contains("Also tell the user, briefly, in the same reply: Remote Control is on."),
        "{sent}"
    );
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

/// An urgent update (moving now because the limit is hit) skips the quiet
/// wait but never cuts into the user's sentence.
#[test]
fn urgent_updates_wait_only_for_the_sentence() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("urgent");
    app.voice.partial = Some("and then".into());
    app.quiet_user_spoke();
    app.say_update(Voice::Announce, "hyper finished");
    app.say_urgent(
        Voice::Announce,
        "My account hit its usage limit, so I moved myself to Account 2.",
    );
    app.quiet_tick();
    assert!(app.assistant.quiet.said.is_empty(), "not mid sentence");
    app.voice.partial = None;
    app.quiet_tick();
    assert_eq!(
        app.assistant.quiet.said,
        vec!["My account hit its usage limit, so I moved myself to Account 2.".to_string()]
    );
    assert_eq!(
        app.assistant.quiet.queue.len(),
        1,
        "the ordinary one still waits"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A new tab's session transcript, named like `tab` does.
fn new_tab_transcript(app: &mut App, uid: u64, sid: &str) -> PathBuf {
    let (s, t) = app.find_tab(uid).unwrap();
    let a = app.panes[s].account.unwrap();
    let cwd = app.panes[s].tabs[t].cwd.clone();
    app.panes[s].tabs[t].session_id = Some(sid.into());
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&cwd));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{sid}.jsonl"))
}

fn uid_of(v: &Value) -> u64 {
    v["tab"].as_str().unwrap()[1..].parse().unwrap()
}

/// A question asked through open_tab comes back: will_report is true,
/// the wait starts with the tab's first turn, and its reply is the answer.
#[test]
fn an_open_tab_question_is_reported() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("opentabq");
    let q = "Is a dehumidifier a major appliance?";
    let v = app.control_call(
        "open_tab",
        &json!({"_client": "brain", "account": 1, "name": "appliance", "prompt": q}),
    );
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(v["will_report"], json!(true), "{v}");
    assert!(v["report_say"].as_str().unwrap().contains("I'll tell you"));
    let uid = uid_of(&v);
    let f = app.assistant.follow_ups.last().unwrap();
    assert_eq!((f.uid, f.question.as_str()), (uid, q));
    assert!(f.await_start);
    // Still coming up: the wait has not started.
    set_activity(&mut app, uid, Activity::Starting);
    app.follow_ups_tick();
    assert!(app.assistant.follow_ups.last().unwrap().await_start);
    // The prompt is in and the turn runs.
    for d in app.deliveries.iter_mut().filter(|d| d.uid == uid) {
        d.stage = crate::control::Stage::Delivered;
    }
    set_activity(&mut app, uid, Activity::Working);
    app.follow_ups_tick();
    let f = app.assistant.follow_ups.last().unwrap();
    assert!(!f.await_start && f.seen_working && f.state == FuState::Waiting);
    let path = new_tab_transcript(&mut app, uid, "7777aaaa-0000-4000-8000-0000000000a1");
    write(
        &path,
        &[
            json!({"type": "user", "message": {"role": "user", "content": q}, "timestamp": stamp(1)}),
            json!({"type": "assistant", "message": {"id": "a", "content": [{"type": "text", "text": "Usually not: a portable dehumidifier counts as a small appliance."}]}, "timestamp": stamp(2)}),
        ],
    );
    set_activity(&mut app, uid, Activity::Ready);
    app.assistant.follow_ups.last_mut().unwrap().checked = None;
    app.follow_ups_tick();
    match &app.assistant.follow_ups.last().unwrap().state {
        FuState::Answered { text, .. } | FuState::Reported { text, .. } => {
            assert!(text.contains("small appliance"), "{text}")
        }
        s => panic!("{s:?}"),
    }
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

/// A plain task through open_tab is not reported back, unless
/// expect_reply says it is a question for the user.
#[test]
fn an_open_tab_task_is_not_reported_unless_asked() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("opentabtask");
    app.assistant.last_user = "make me a calculator app".into();
    let v = app.control_call(
        "open_tab",
        &json!({"_client": "brain", "account": 1, "name": "calc", "prompt": "Build a simple calculator web app."}),
    );
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(v["will_report"], json!(false), "{v}");
    assert!(app.assistant.follow_ups.is_empty());
    let v = app.control_call(
        "open_tab",
        &json!({"_client": "brain", "account": 1, "name": "sizes", "prompt": "List the five largest files in this repo.", "expect_reply": true}),
    );
    assert_eq!(v["will_report"], json!(true), "{v}");
    assert_eq!(app.assistant.follow_ups.len(), 1);
    assert_eq!(app.assistant.follow_ups[0].uid, uid_of(&v));
    // No prompt: nothing to report, and nothing is said about it.
    let v = app.control_call(
        "open_tab",
        &json!({"_client": "brain", "account": 1, "name": "empty"}),
    );
    assert!(v["will_report"].is_null(), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}
