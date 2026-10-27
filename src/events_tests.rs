use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::app::App;
use crate::app_events::EventKind;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::Activity;

fn test_app(tag: &str) -> (App, PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-events-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.cfg.notifications = false;
    app.accounts[0].login.source = Some(CredSource::Keychain);
    (app, home)
}

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

fn tab(app: &mut App, home: &Path, name: &str, last: &str) -> u64 {
    let proj = home.join("newtabs").join(name);
    std::fs::create_dir_all(&proj).unwrap();
    let t = app.panes[0].add_tab(proj.clone());
    let sid = format!("7777aaaa-0000-4000-8000-{:012}", t);
    let tb = &mut app.panes[0].tabs[t];
    tb.session_id = Some(sid.clone());
    tb.custom_name = Some(name.into());
    tb.activity = Activity::Ready;
    let uid = tb.uid;
    let a = app.panes[0].account.unwrap();
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&proj));
    std::fs::create_dir_all(&dir).unwrap();
    let line = json!({"type": "assistant", "message": {"id": "m1", "content": [{"type": "text", "text": last}], "stop_reason": "end_turn"}});
    std::fs::write(dir.join(format!("{sid}.jsonl")), format!("{line}\n")).unwrap();
    uid
}

fn attention(app: &mut App, uid: u64, what: Activity) {
    let (slot, tab) = app.find_tab(uid).unwrap();
    app.attention.push(crate::app::Attention {
        slot,
        tab,
        what,
        uid,
        from: Activity::Working,
    });
}

fn told_nothing(app: &App) {
    let log = app.told.borrow();
    let bad: Vec<String> = log
        .ring
        .iter()
        .filter(|e| {
            e.kind == "attention"
                || e.kind == "announcement"
                || (e.kind == "flash"
                    && (e.text.contains("finished") || e.text.contains("needs approval")))
        })
        .map(|e| format!("{}: {}", e.kind, e.text))
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
    assert!(
        app.assistant.quiet.queue.is_empty(),
        "{:?}",
        app.assistant.quiet.queue
    );
}

fn sent(rec: &Path) -> String {
    for _ in 0..30 {
        let s = std::fs::read_to_string(rec).unwrap_or_default();
        if !s.is_empty() {
            return s;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    String::new()
}

#[test]
fn a_finished_tab_is_not_announced_and_the_assistant_gets_the_event() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("finish");
    app.cfg.voice.announce = true;
    let rec = with_brain(&mut app, &home);
    let uid = tab(
        &mut app,
        &home,
        "minitest",
        "All 42 tests pass on the parser.",
    );
    attention(&mut app, uid, Activity::Ready);
    app.voice_tick();
    told_nothing(&app);

    let e = &app.assistant.events.events[0];
    assert_eq!(e.kind, EventKind::Finished);
    assert_eq!(e.uid, uid);
    assert_eq!(e.tab, "minitest");
    assert!(e.project.ends_with("minitest"), "{}", e.project);
    assert_eq!(e.text, "All 42 tests pass on the parser.");

    assert!(app.assistant.busy && app.assistant.system_turn);
    assert!(app.assistant.events.events[0].delivered);
    let s = sent(&rec);
    assert!(
        s.contains(&format!("t{uid} (minitest"))
            && s.contains("finished its turn")
            && s.contains("All 42 tests pass on the parser.")
            && s.contains("call ignore"),
        "{s}"
    );

    app.assistant.brain = None;
    app.assistant.busy = false;
    let snap = app.state_preamble();
    assert!(
        snap.contains("recent_tab_events") && snap.contains("All 42 tests pass"),
        "{snap}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn finished_work_is_batched_but_approvals_go_at_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("gap");
    with_brain(&mut app, &home);
    let a = tab(&mut app, &home, "alpha", "Done with the docs.");
    let b = tab(&mut app, &home, "beta", "Deployed.");
    let c = tab(&mut app, &home, "gamma", "Ready to apply?");

    app.assistant.events.last_turn = Some(Instant::now());
    app.tab_finished_event(a);
    app.tab_finished_event(b);
    app.events_tick();
    assert!(!app.assistant.busy, "finished work waits for the gap");
    assert!(app.assistant.events.events.iter().all(|e| !e.delivered));

    let (s, t) = app.find_tab(c).unwrap();
    app.panes[s].tabs[t].activity = Activity::Permission;
    app.tab_approval_event(c);
    app.events_tick();
    assert!(app.assistant.busy && app.assistant.system_turn);
    assert!(app.assistant.events.events.iter().all(|e| e.delivered));
    told_nothing(&app);
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn approvals_reach_the_user_through_the_assistant() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("approval");
    app.cfg.voice.announce = true;
    let rec = with_brain(&mut app, &home);
    let uid = tab(&mut app, &home, "deploybot", "Shall I run it?");
    let (s, t) = app.find_tab(uid).unwrap();
    app.panes[s].tabs[t].activity = Activity::Permission;
    attention(&mut app, uid, Activity::Permission);
    app.voice_tick();
    told_nothing(&app);
    assert!(matches!(
        app.assistant.events.events[0].kind,
        EventKind::Approval(_)
    ));
    assert!(app.assistant.busy && app.assistant.system_turn);
    let s = sent(&rec);
    assert!(
        s.contains(&format!("t{uid} (deploybot"))
            && s.contains("waits for the user's approval")
            && s.contains("an approval that waits for them"),
        "{s}"
    );
    app.assistant.brain = None;

    let (mut app2, home2) = test_app("approval-stale");
    let u2 = tab(&mut app2, &home2, "stale", "x");
    let (s2, t2) = app2.find_tab(u2).unwrap();
    app2.panes[s2].tabs[t2].activity = Activity::Permission;
    app2.tab_approval_event(u2);
    app2.panes[s2].tabs[t2].activity = Activity::Ready;
    app2.events_tick();
    assert!(!app2.assistant.busy);
    assert!(app2.assistant.events.events[0].delivered);
    let _ = std::fs::remove_dir_all(&home2);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_finish_a_pending_question_covers_is_left_to_its_report() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("followup");
    let uid = tab(&mut app, &home, "asked", "The answer is 7.");
    app.register_follow_up(uid, "What is the answer?");
    app.tab_finished_event(uid);
    app.events_tick();
    assert!(!app.assistant.busy, "the follow-up report tells it");
    assert!(app.assistant.events.events[0].delivered);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn direct_mode_keeps_its_own_voice_and_events_wait() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("direct");
    with_brain(&mut app, &home);
    let mine = tab(&mut app, &home, "mine", "Done.");
    let other = tab(&mut app, &home, "other", "Built.");
    app.direct.on = Some(crate::app_direct::DirectMode {
        uid: mine,
        name: "mine".into(),
        since: chrono::Local::now(),
        log: vec![],
        last_act: Activity::Ready,
        last_reply: None,
        reply_wait: None,
        last_sent: None,
        confirm: None,
    });
    assert!(app.direct_with(mine));

    attention(&mut app, mine, Activity::Ready);
    attention(&mut app, other, Activity::Ready);
    app.voice_tick();
    told_nothing(&app);
    let uids: Vec<u64> = app.assistant.events.events.iter().map(|e| e.uid).collect();
    assert_eq!(uids, vec![other]);

    assert!(!app.assistant.busy);
    assert!(!app.assistant.events.events[0].delivered);
    app.direct.on = None;
    app.events_tick();
    assert!(app.assistant.busy && app.assistant.events.events[0].delivered);
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn subagents_finishing_are_never_news() {
    let f = crate::updates::Facts {
        agents_done: 2,
        ..Default::default()
    };
    let said = crate::updates::say_project("hyper", &f, Default::default());
    assert!(
        said.as_ref().is_none_or(|(_, l)| !l.contains("subagent")),
        "{said:?}"
    );
    assert!(!f.is_event());
}

#[test]
fn update_mode_news_goes_to_the_assistant() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("updates");
    with_brain(&mut app, &home);
    app.update_event("hyper finished: all green.");
    assert_eq!(app.assistant.events.events[0].kind, EventKind::Update);

    app.assistant.events.last_turn = Some(Instant::now());
    app.events_tick();
    assert!(app.assistant.busy && app.assistant.events.events[0].delivered);
    told_nothing(&app);
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}
