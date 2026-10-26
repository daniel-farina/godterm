//! A tab at its prompt with subagents, shells or tasks still running is
//! busy, not idle: get_state, the assistant's snapshot, the idle selector,
//! archive sweeps, the memory saver, moves and quick closes all treat it
//! so. It still takes prompts (that is "ready for a prompt", a separate
//! thing).

use crate::app::{App, Modal};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::test_app;
use crate::pane::{Activity, PaneState};
use serde_json::json;

/// A running tab at its prompt on pane `s`, with a session.
fn ready_tab(app: &mut App, s: usize, name: &str) -> usize {
    let dir = app.cfg.accounts[app.panes[s].account.unwrap()].work_dir();
    let t = app.panes[s].add_tab(dir);
    let tab = &mut app.panes[s].tabs[t];
    tab.custom_name = Some(name.into());
    tab.session_id = Some(format!("sess-{name}"));
    tab.state = PaneState::Running;
    tab.activity = Activity::Ready;
    t
}

/// What runs under the tab, as its activity found it (the 2 s cache).
fn background(app: &App, s: usize, t: usize, what: Option<&str>) {
    app.bg_cache.borrow_mut().insert(
        app.panes[s].tabs[t].uid,
        (std::time::Instant::now(), what.map(str::to_string)),
    );
}

fn tab_row(v: &serde_json::Value, uid: u64) -> serde_json::Value {
    v["result"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(format!("t{uid}")))
        .cloned()
        .unwrap()
}

#[test]
fn background_work_is_busy_in_state_and_snapshot() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("bgbusy-state");
    let bg = ready_tab(&mut app, 0, "bgtab");
    let idle = ready_tab(&mut app, 0, "idletab");
    background(&app, 0, bg, Some("2 subagents, 1 shell"));
    background(&app, 0, idle, None);
    assert!(app.tab_busy(0, bg));
    assert!(!app.tab_busy(0, idle));
    assert_eq!(app.state_word(0, bg), "background");
    assert_eq!(app.state_word(0, idle), "ready");
    let (bu, iu) = (app.panes[0].tabs[bg].uid, app.panes[0].tabs[idle].uid);
    let v = app.control_call("get_state", &json!({}));
    let r = tab_row(&v, bu);
    assert_eq!(r["state"], "background", "{r}");
    assert_eq!(r["busy"], true, "{r}");
    assert_eq!(r["background_work"], "2 subagents, 1 shell", "{r}");
    assert_eq!(r["accepts_prompts"], true, "{r}");
    let r = tab_row(&v, iu);
    assert_eq!(r["state"], "ready", "{r}");
    assert!(r["busy"].is_null() && r["background_work"].is_null(), "{r}");
    // The assistant's snapshot says busy, never "ready, background".
    let p = app.state_preamble();
    assert!(
        p.contains("WORKING in background (2 subagents, 1 shell), not idle"),
        "{p}"
    );
    assert!(!p.contains("ready, background"), "{p}");
    // The system prompt says background tabs are busy.
    let sp = crate::sysprompt::SECTIONS
        .iter()
        .map(|s| s.default)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sp.contains("never idle, finished or free"), "{sp}");
    assert!(sp.contains("work_status"), "{sp}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn claude_footer_alone_counts_as_background_work() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("bgbusy-footer");
    let t = ready_tab(&mut app, 0, "footer");
    background(&app, 0, t, None);
    assert!(!app.tab_busy(0, t));
    app.panes[0].tabs[t].bg_wait = true;
    assert_eq!(
        app.tab_background_work(0, t).as_deref(),
        Some("background work")
    );
    assert!(app.tab_busy(0, t));
    assert_eq!(app.state_word(0, t), "background");
    // Working is busy too, without counting as background work.
    app.panes[0].tabs[t].bg_wait = false;
    app.panes[0].tabs[t].activity = Activity::Working;
    assert!(app.tab_busy(0, t) && app.tab_background_work(0, t).is_none());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_idle_selector_leaves_background_tabs_out() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("bgbusy-select");
    let bg = ready_tab(&mut app, 0, "bgtab");
    let idle = ready_tab(&mut app, 0, "idletab");
    background(&app, 0, bg, Some("1 shell"));
    background(&app, 0, idle, None);
    let names = |app: &App, sel: &str| -> Vec<String> {
        app.tab_targets(&json!({"tab": sel}), true)
            .unwrap_or_default()
            .into_iter()
            .map(|(s, t)| app.panes[s].tabs[t].name())
            .collect()
    };
    for sel in ["idle", "ready"] {
        let n = names(&app, sel);
        assert!(n.contains(&"idletab".to_string()), "{sel}: {n:?}");
        assert!(!n.contains(&"bgtab".to_string()), "{sel}: {n:?}");
    }
    assert_eq!(names(&app, "background"), vec!["bgtab".to_string()]);
    assert!(names(&app, "busy").contains(&"bgtab".to_string()));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn archive_sweeps_skip_background_tabs_unless_forced() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("bgbusy-archive");
    let bg = ready_tab(&mut app, 0, "bgtab");
    let idle = ready_tab(&mut app, 0, "idletab");
    background(&app, 0, bg, Some("1 subagent"));
    background(&app, 0, idle, None);
    app.archive_clock = Some(chrono::Utc::now().timestamp() + 5 * 86_400);
    let (ok, skipped) = app.archive_select(&json!({"idle_for_hours": 48})).unwrap();
    let ok: Vec<String> = ok
        .iter()
        .map(|&(s, t)| app.panes[s].tabs[t].name())
        .collect();
    assert!(ok.contains(&"idletab".to_string()), "{ok:?}");
    assert!(!ok.contains(&"bgtab".to_string()), "{ok:?}");
    let why = skipped
        .iter()
        .find(|(s, t, _)| app.panes[*s].tabs[*t].name() == "bgtab")
        .map(|(_, _, w)| w.clone())
        .expect("listed as skipped");
    assert!(
        why.contains("busy: work still runs in the background (1 subagent)"),
        "{why}"
    );
    let (ok, _) = app
        .archive_select(&json!({"idle_for_hours": 48, "force": true}))
        .unwrap();
    assert!(ok
        .iter()
        .any(|&(s, t)| app.panes[s].tabs[t].name() == "bgtab"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn memory_saver_and_quick_close_leave_background_tabs_alone() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("bgbusy-saver");
    let bg = ready_tab(&mut app, 0, "bgtab");
    let idle = ready_tab(&mut app, 0, "idletab");
    // Neither is the active tab, so neither is on screen.
    let other = ready_tab(&mut app, 0, "front");
    app.panes[0].active = other;
    background(&app, 0, bg, Some("1 shell"));
    background(&app, 0, idle, None);
    let zero = std::time::Duration::ZERO;
    assert!(app.suspendable(0, idle, zero));
    assert!(
        !app.suspendable(0, bg, zero),
        "pausing would kill its shell"
    );
    // "close it" closes an idle tab at once, but asks for a busy one.
    app.assistant.last_user = "close it".into();
    assert!(!app.closes_without_a_yes((0, bg)));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn moves_wait_for_background_work() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("bgbusy-move");
    let t = ready_tab(&mut app, 0, "bgtab");
    background(&app, 0, t, Some("1 shell"));
    let uid = app.panes[0].tabs[t].uid;
    app.request_move(0, t, 1, false, true);
    assert!(matches!(app.modal, Modal::MoveBusy(_)), "asks first");
    app.move_busy_choice(false);
    assert_eq!(app.queued_moves.len(), 1);
    // Still running in the background: the queued move waits.
    app.tabmove_tick();
    assert_eq!(app.queued_moves.len(), 1);
    assert!(app.find_tab(uid).is_some());
    // The move tool queues it too.
    app.queued_moves.clear();
    let v = app.control_call("move_tab", &json!({"tab": format!("t{uid}"), "to": 2}));
    assert_eq!(app.queued_moves.len(), 1, "{v}");
    let _ = std::fs::remove_dir_all(home);
}
