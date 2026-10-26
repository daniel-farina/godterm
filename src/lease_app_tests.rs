//! Conversation ownership in the app (app_lease.rs): a resume waits for
//! the lease, a session owned elsewhere opens read only, a lost lease
//! blocks input and stops the CLI after its turn, jobs fire only on the
//! owner, a moved tab keeps its lease, and pulled archive and jobs
//! changes are applied by the app itself. The lease worker is not
//! running: its commands are read from `lease.sent` and its events are
//! handed in.

use crate::app::{App, AppEvent};
use crate::app_lease::{StopStage, TabLease};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::jobs_tests::{add, deliveries_for, running, setup};
use crate::pane::{Activity, LaunchKind, PaneState};
use crate::sync::lease::{rfc, Beat, Board, Lease, Status};
use crate::sync::takeover::{Cmd, Event};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const T0: i64 = 1_800_000_000;

fn on(app: &mut App) {
    app.lease.test_on = true;
    app.lease.me = "me-id".into();
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn board(sid: &str, owner: &str, beat_ms: i64) -> Board {
    let mut b = Board::default();
    b.leases.insert(
        sid.into(),
        Lease {
            owner: owner.into(),
            gen: 1,
            acquired: rfc(beat_ms),
            ..Default::default()
        },
    );
    b.beats.insert(
        owner.into(),
        Beat {
            at: rfc(beat_ms),
            label: if owner == "me-id" { "Here" } else { "MacBook" }.into(),
        },
    );
    b
}

fn live_elsewhere() -> Status {
    Status::Live {
        owner: "mac-id".into(),
        label: "MacBook".into(),
        beat_ms: now_ms() - 60_000,
        takeover: None,
    }
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn jobs_fire_only_on_the_machine_that_owns_their_conversation() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("lease-jobs");
    on(&mut app);
    let t = running(&mut app, 0, "billing", "sess-b");
    app.panes[0].tabs[t].lease = Some(TabLease::Owned {
        sid: "sess-b".into(),
        offline: false,
    });
    let id = add(&mut app, 0, t, "status", "status of the build?", 15);
    // Another live machine owns the conversation: paused here.
    app.lease.board = Some(board("sess-b", "mac-id", now_ms()));
    app.job_clock = Some(T0 + 15 * 60 + 2);
    app.jobs_tick();
    assert!(deliveries_for(&app, "status of the build?").is_empty());
    let r = app.jobs.find(&id).unwrap().last_result.clone().unwrap();
    assert_eq!(r.state, "skipped");
    assert!(r.detail.contains("MacBook owns"), "{}", r.detail);
    assert!(app
        .lease_lines()
        .iter()
        .any(|l| l.starts_with("Paused jobs: 1")));
    // Owned here: it runs.
    app.lease.board = Some(board("sess-b", "me-id", now_ms()));
    app.job_clock = Some(T0 + 30 * 60 + 2);
    app.jobs_tick();
    assert_eq!(deliveries_for(&app, "status of the build?").len(), 1);
    // A job with no conversation runs only on its own machine.
    let mut j = app.jobs.find(&id).cloned().unwrap();
    j.target.session_id = None;
    j.machine = Some("mac-id".into());
    assert!(app.lease_job_block(&j).unwrap().contains("runs on mac-id"));
    j.machine = Some("me-id".into());
    assert!(app.lease_job_block(&j).is_none());
    // New jobs remember this machine.
    assert_eq!(app.job_machine(), Some("me-id".to_string()));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_resume_waits_for_the_lease_and_a_session_owned_elsewhere_is_read_only() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("lease-gate");
    on(&mut app);
    let dir = app.cfg.accounts[app.panes[0].account.unwrap()].work_dir();
    let t = app.panes[0].add_tab(dir);
    app.panes[0].tabs[t].session_id = Some("sess-x".into());
    app.launch_tab(0, t, LaunchKind::Resume("sess-x".into()));
    // Nothing started: the lease (and a pull) first.
    assert!(!app.panes[0].tabs[t].is_running());
    assert!(
        matches!(&app.panes[0].tabs[t].lease, Some(TabLease::Waiting { sid, .. }) if sid == "sess-x")
    );
    assert_eq!(
        app.lease.sent,
        vec![Cmd::Acquire {
            sid: "sess-x".into(),
            acct: app.cfg.accounts[app.panes[0].account.unwrap()].name.clone(),
            pull: true
        }]
    );
    // MacBook has it, live: read only, with who and when.
    app.handle(AppEvent::Lease(Event::Held {
        sid: "sess-x".into(),
        status: live_elsewhere(),
        offline: false,
    }));
    let Some(TabLease::ReadOnly(r)) = &app.panes[0].tabs[t].lease else {
        panic!("{:?}", app.panes[0].tabs[t].lease)
    };
    assert_eq!(r.label, "MacBook");
    assert!(!r.resumable);
    let j = app.lease_json(0, t).unwrap();
    assert_eq!(j["state"], "read_only");
    assert_eq!(j["owner"], "MacBook");
    assert_eq!(j["active"], "active 1 min ago");
    // The view says so on screen.
    app.panes[0].active = t;
    let mut term = crate::confirm_click_tests::term();
    crate::confirm_click_tests::draw(&mut app, &mut term);
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        screen.contains("Owned by MacBook, active 1 min ago"),
        "{screen}"
    );
    assert!(screen.contains("Request take over") && screen.contains("Take over now"));
    // Typing goes nowhere; r asks for it.
    app.focus = 0;
    app.on_grid_key(key('x'));
    assert!(!app.panes[0].tabs[t].is_running());
    app.on_grid_key(key('r'));
    assert!(matches!(app.lease.sent.last(), Some(Cmd::Request { sid, .. }) if sid == "sess-x"));
    // Take over now asks first; y says yes.
    app.on_grid_key(key('T'));
    assert!(matches!(&app.panes[0].tabs[t].lease, Some(TabLease::ReadOnly(r)) if r.confirm));
    app.on_grid_key(key('y'));
    assert!(matches!(app.lease.sent.last(), Some(Cmd::TakeOverNow { sid, .. }) if sid == "sess-x"));
    // Ours now: it resumes.
    app.handle(AppEvent::Lease(Event::Acquired {
        sid: "sess-x".into(),
        offline: false,
        pulled: true,
        forked: None,
        warn: None,
    }));
    assert!(
        matches!(&app.panes[0].tabs[t].lease, Some(TabLease::Owned { sid, offline: false }) if sid == "sess-x")
    );
    assert_eq!(
        app.panes[0].tabs[t].kind,
        LaunchKind::Resume("sess-x".into())
    );
    // The web app sees no banner for an owned tab; an assistant call to
    // the buttons' tool is refused.
    assert!(app.lease_json(0, t).is_none());
    let uid = app.panes[0].tabs[t].uid;
    let v = app.control_call(
        "sync_lease",
        &serde_json::json!({"tab": uid, "op": "take_over"}),
    );
    assert!(v.to_string().contains("user's own button"), "{v}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_lost_lease_blocks_input_lets_the_turn_end_then_stops_and_forks() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("lease-lost");
    on(&mut app);
    let t = running(&mut app, 0, "api", "sess-l");
    app.lease.granted.insert("sess-l".into());
    app.panes[0].tabs[t].lease = Some(TabLease::Owned {
        sid: "sess-l".into(),
        offline: false,
    });
    app.panes[0].tabs[t].activity = Activity::Working;
    app.lease_tick();
    assert!(app.lease.held.lock().unwrap().contains_key("sess-l"));
    app.handle(AppEvent::Lease(Event::Lost {
        sid: "sess-l".into(),
        label: "MacBook".into(),
        conflict: false,
    }));
    // Input is blocked at once; the running turn goes on.
    let tab = &app.panes[0].tabs[t];
    assert!(matches!(&tab.lease, Some(TabLease::Stopping { label, .. }) if label == "MacBook"));
    assert!(tab
        .lease
        .as_ref()
        .unwrap()
        .banner()
        .unwrap()
        .contains("input is blocked"));
    assert!(!app.lease.granted.contains("sess-l"));
    app.lease_tick();
    assert!(matches!(
        &app.panes[0].tabs[t].lease,
        Some(TabLease::Stopping {
            stage: StopStage::WaitIdle(_),
            ..
        })
    ));
    // The turn ended: interrupts follow (never a kill).
    app.panes[0].tabs[t].activity = Activity::Ready;
    app.lease_tick();
    assert!(matches!(
        &app.panes[0].tabs[t].lease,
        Some(TabLease::Stopping {
            stage: StopStage::Signal { .. },
            ..
        })
    ));
    // The CLI exited: what it never pushed becomes a fork.
    app.panes[0].tabs[t].state = PaneState::Exited(Some(0));
    app.lease_tick();
    assert!(
        matches!(app.lease.sent.last(), Some(Cmd::Fork { sid, owner: false, label, .. }) if sid == "sess-l" && label == "MacBook")
    );
    assert!(matches!(
        &app.panes[0].tabs[t].lease,
        Some(TabLease::ReadOnly(_))
    ));
    app.handle(AppEvent::Lease(Event::Forked {
        sid: "sess-l".into(),
        fork: Some("1a2b3c4d-0000-4000-8000-000000000001".into()),
        label: "MacBook".into(),
        owner: false,
    }));
    let j = app.lease_json(0, t).unwrap();
    assert_eq!(
        j["note"],
        "Moved to MacBook; your unsent part is fork 1a2b3c4d"
    );
    // f opens the fork in a new tab (on its own lease).
    let n = app.panes[0].tabs.len();
    app.focus = 0;
    app.panes[0].active = t;
    app.on_grid_key(key('f'));
    assert_eq!(app.panes[0].tabs.len(), n + 1);
    assert!(
        matches!(app.lease.sent.last(), Some(Cmd::Acquire { sid, .. }) if sid.starts_with("1a2b3c4d"))
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_request_for_ours_stops_after_the_turn_then_releases() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("lease-asked");
    on(&mut app);
    let t = running(&mut app, 0, "api", "sess-r");
    app.lease.granted.insert("sess-r".into());
    app.panes[0].tabs[t].lease = Some(TabLease::Owned {
        sid: "sess-r".into(),
        offline: false,
    });
    app.lease_tick();
    app.handle(AppEvent::Lease(Event::Asked {
        sid: "sess-r".into(),
        label: "Linux".into(),
    }));
    assert!(matches!(
        &app.panes[0].tabs[t].lease,
        Some(TabLease::Stopping { .. })
    ));
    app.panes[0].tabs[t].state = PaneState::Exited(Some(0));
    app.lease_tick();
    assert!(matches!(app.lease.sent.last(), Some(Cmd::Release { sid, .. }) if sid == "sess-r"));
    app.handle(AppEvent::Lease(Event::Released {
        sid: "sess-r".into(),
        to: Some("Linux".into()),
    }));
    let j = app.lease_json(0, t).unwrap();
    assert_eq!(j["note"], "Released to Linux with every line pushed");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_moved_tab_keeps_its_lease_and_a_new_session_id_takes_its_own() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("lease-move");
    on(&mut app);
    assert!(app.cfg.accounts.len() >= 2, "two accounts to move between");
    let t = running(&mut app, 0, "api", "sess-m");
    app.panes[0].tabs[t].state = PaneState::Exited(Some(0));
    app.lease.granted.insert("sess-m".into());
    app.panes[0].tabs[t].lease = Some(TabLease::Owned {
        sid: "sess-m".into(),
        offline: false,
    });
    // Its transcript, in the source account.
    let src = app.cfg.accounts[app.panes[0].account.unwrap()].config_dir();
    let cwd = app.panes[0].tabs[t].cwd.clone();
    let p = src
        .join("projects")
        .join(crate::state::encode_project_dir(&cwd))
        .join("sess-m.jsonl");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{\"type\":\"user\",\"sessionId\":\"sess-m\"}\n").unwrap();
    let target = (0..app.cfg.accounts.len())
        .find(|a| {
            Some(*a) != app.panes[0].account
                && app.cfg.accounts[*a].harness() == crate::harness::Harness::Claude
        })
        .unwrap();
    app.move_tab_now(0, t, target, false, true);
    // The target tab resumed the same session at once: no new lease asked.
    let (s2, t2) = app
        .panes
        .iter()
        .enumerate()
        .find_map(|(s, p)| {
            (Some(target) == p.account)
                .then(|| {
                    p.tabs
                        .iter()
                        .position(|x| x.session_id.as_deref() == Some("sess-m"))
                })
                .flatten()
                .map(|t| (s, t))
        })
        .expect("moved tab");
    assert!(
        matches!(&app.panes[s2].tabs[t2].lease, Some(TabLease::Owned { sid, .. }) if sid == "sess-m")
    );
    assert_eq!(
        app.panes[s2].tabs[t2].kind,
        LaunchKind::Resume("sess-m".into())
    );
    assert!(app.lease.sent.is_empty(), "{:?}", app.lease.sent);
    // A new session id seen in a tab (a copy, a duplicate): its own lease.
    app.lease_claim(s2, t2, "sess-new");
    app.lease_claim(s2, t2, "sess-new");
    assert_eq!(app.lease.sent.len(), 1);
    assert!(
        matches!(&app.lease.sent[0], Cmd::Acquire { sid, pull: false, .. } if sid == "sess-new")
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn pulled_archive_and_jobs_changes_are_applied_by_the_app_itself() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("lease-apply");
    let t = running(&mut app, 0, "api", "sess-a");
    let id = add(&mut app, 0, t, "first", "a", 15);
    app.jobs.save().unwrap();
    // The app adds a job after the sync read jobs.json...
    let id2 = add(&mut app, 0, t, "second", "b", 15);
    app.jobs.save().unwrap();
    // ...and the sync hands over a rename of the first from another
    // machine (the old way wrote the file it read, losing "second").
    let mut changed = serde_json::to_value(app.jobs.find(&id).unwrap()).unwrap();
    changed["name"] = serde_json::json!("first, renamed elsewhere");
    let mut set = std::collections::BTreeMap::new();
    set.insert(id.clone(), Some(changed));
    let (tx, rx) = std::sync::mpsc::channel();
    app.handle(AppEvent::SyncApply(
        crate::sync::StoreApply { store: "jobs", set },
        tx,
    ));
    assert!(rx.recv().unwrap());
    assert_eq!(app.jobs.find(&id).unwrap().name, "first, renamed elsewhere");
    assert!(app.jobs.find(&id2).is_some(), "the app's own change stays");
    let disk = crate::jobs::Store::load();
    assert_eq!(disk.find(&id).unwrap().name, "first, renamed elsewhere");
    assert!(disk.find(&id2).is_some());
    // An archive entry removed elsewhere goes; a store it does not hold is
    // refused (the sync then tries again).
    let mut set = std::collections::BTreeMap::new();
    set.insert("nope".to_string(), None);
    let (tx, rx) = std::sync::mpsc::channel();
    app.handle(AppEvent::SyncApply(
        crate::sync::StoreApply {
            store: "archive",
            set,
        },
        tx,
    ));
    assert!(rx.recv().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    app.handle(AppEvent::SyncApply(
        crate::sync::StoreApply {
            store: "projects",
            set: Default::default(),
        },
        tx,
    ));
    assert!(!rx.recv().unwrap());
    let _ = std::fs::remove_dir_all(home);
}
