use serde_json::json;
use std::time::Duration;

use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-stale-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();

    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.accounts[0].login.source = Some(CredSource::Keychain);
    app.accounts[1].login.source = Some(CredSource::File);
    (app, home)
}

#[test]
fn confirmation_expires_across_sleep_and_leaves_the_snapshot() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    crate::clock::reset();
    let (mut app, home) = test_app("confirm-sleep");
    let v = app.control_call("close_tabs", &json!({"tab": "all"}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let tok = v["token"].as_str().unwrap().to_string();
    assert!(app.state_preamble().contains("PENDING close_tabs"));

    crate::clock::simulate_sleep(Duration::from_secs(300));
    let snap = app.state_preamble();
    assert!(!snap.contains("PENDING"), "{snap}");
    assert!(snap.contains("expired question"), "{snap}");
    let st = app.control_call("get_state", &json!({}));
    assert_eq!(
        st["result"]["pending_confirmations"][0]["answerable"],
        json!(false)
    );

    app.assistant_turn += 1;
    app.assistant.last_user = "yes".into();
    let before: usize = app.panes.iter().map(|p| p.tabs.len()).sum();
    let r = app.control_call("close_tabs", &json!({"confirm_token": tok}));
    assert!(r["error"].as_str().unwrap_or("").contains("expired"), "{r}");
    assert_eq!(
        app.panes.iter().map(|p| p.tabs.len()).sum::<usize>(),
        before
    );
    crate::clock::reset();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn pause_ends_across_sleep() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    crate::clock::reset();
    let (mut app, home) = test_app("pause-sleep");
    app.control_call("pause_listening", &json!({"seconds": 300}));
    assert!(app.paused());
    crate::clock::simulate_sleep(Duration::from_secs(3600));
    assert_eq!(app.pause_left(), Some(0));
    app.pause_tick();
    assert!(!app.paused());
    crate::clock::reset();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn expired_confirmation_does_not_take_the_next_yes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    crate::clock::reset();
    let (mut app, home) = test_app("confirm-yes");
    app.cfg.assistant.mode = "on".into();
    app.panes[0].add_tab(home.clone());
    let v = app.control_call("close_tabs", &json!({"tab": "all"}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");

    app.modal = crate::app::Modal::ConfirmClose;
    assert_eq!(app.run_instant(crate::instant::Instant::Approve), None);

    app.pending_confirms[0].at = std::time::Instant::now() - Duration::from_secs(180);
    let tabs = app.panes[app.focus].tabs.len();
    assert_eq!(
        app.run_instant(crate::instant::Instant::Approve).as_deref(),
        Some("closed the tab")
    );
    assert!(app.panes[app.focus].tabs.len() < tabs || tabs == 1);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn tab_ids_survive_restart_and_are_never_reused() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("uids");
    let work = home.join("newtabs/gas");
    std::fs::create_dir_all(&work).unwrap();
    app.panes[1].add_tab(work.clone());
    let gas = app.panes[1].tabs.last().unwrap().uid;

    app.panes[1].add_tab(home.clone());
    let spent = app.panes[1].tabs.pop().unwrap().uid;
    app.save_state();

    let saved = crate::state::AppState::load().unwrap();
    assert!(saved.next_uid.unwrap() > spent);
    let (mut fresh, h2) = test_app("uids2");
    crate::config::testing::set_home(&home);
    fresh.restore(saved);
    let (s, t) = fresh.find_tab(gas).expect("t{gas} is still a tab");
    assert_eq!(fresh.panes[s].tabs[t].cwd, work);
    assert!(fresh.find_tab(spent).is_none());
    fresh.panes[0].add_tab(home.clone());
    assert!(fresh.panes[0].tabs.last().unwrap().uid > spent);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&h2);
}

#[test]
fn snapshot_states_ui_modes_every_turn() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("modes");
    let snap = app.state_preamble();
    assert!(
        snap.contains("voice off")
            && snap.contains("not zoomed")
            && snap.contains("no question waits for a yes")
            && snap.contains("no prompt queued or sending")
            && !snap.contains("MUTED")
            && !snap.contains("privacy ON"),
        "{snap}"
    );
    app.voice.muted = true;
    app.zoom = true;
    app.rt_privacy = Some(true);
    let snap = app.state_preamble();
    assert!(
        snap.contains("mic MUTED") && snap.contains("ZOOMED") && snap.contains("privacy ON"),
        "{snap}"
    );

    app.voice.muted = false;
    app.zoom = false;
    let snap = app.state_preamble();
    assert!(!snap.contains("MUTED") && snap.contains("not zoomed"));
    let st = app.control_call("get_state", &serde_json::json!({}));
    assert!(st["result"]["modes"]
        .as_str()
        .unwrap()
        .contains("privacy ON"));

    app.accounts[0].usage =
        Some(crate::usage::parse_usage(include_str!("../tests/fixtures/usage.json")).unwrap());
    app.accounts[0].usage_good_at = Some(chrono::Utc::now() - chrono::Duration::hours(3));
    assert!(app.state_preamble().contains("(as of 3h ago)"));

    assert!(crate::assistant::system_prompt("brief").contains("The state is the truth now"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn remembered_tab_ids_resolve_by_session_not_id() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("remembered");
    let gas = home.join("GAS");
    let other = home.join("other");
    std::fs::create_dir_all(&gas).unwrap();
    std::fs::create_dir_all(&other).unwrap();

    app.panes[1].add_tab(gas.clone());
    let gas_tab = app.panes[1].tabs.last_mut().unwrap();
    gas_tab.session_id = Some("6c7bf0d3-1833-4f50-9b46-5ee6c711144f".into());
    let gas_uid = gas_tab.uid;
    app.panes[0].add_tab(other.clone());
    let reused = app.panes[0].tabs.last().unwrap().uid;

    let c = crate::assistant_history::ConvLog::new();
    c.write(
        "user",
        json!({"text": "open the gas session on account two"}),
    );
    c.write(
        "tool",
        json!({"name": "open_session", "args": {"id": "6c7bf0d3-1833-4f50-9b46-5ee6c711144f", "account": 2},
               "result": format!("{{\"ok\":true,\"result\":{{\"opened\":\"6c7bf0d3-1833-4f50-9b46-5ee6c711144f\",\"tab\":\"t{reused}\"}}}}")}),
    );
    c.write(
        "reply",
        json!({"text": format!("Opened it in tab t{reused}.")}),
    );
    let refs = crate::assistant_memory::recent_tab_refs(2);
    assert!(
        refs.iter().any(|r| r.tab == format!("t{reused}")
            && r.session.as_deref() == Some("6c7bf0d3-1833-4f50-9b46-5ee6c711144f")),
        "{refs:?}"
    );
    let note = app.remembered_tabs_now(&refs);
    assert!(
        note.contains(&format!("t{reused} is now")) && note.contains(&format!("is t{gas_uid} on")),
        "{note}"
    );

    let b = crate::assistant_memory::block(2).unwrap();
    assert!(
        b.contains("today ") && (b.contains(" AM") || b.contains(" PM")),
        "{b}"
    );

    let v = app.control_call(
        "send_prompt",
        &json!({"tab": format!("t{reused}"), "text": "shut down the processes"}),
    );
    let e = v["error"].as_str().unwrap_or("");
    assert!(e.contains(&format!("t{gas_uid}")), "{v}");
    let again = app.control_call(
        "send_prompt",
        &json!({"tab": format!("t{reused}"), "text": "shut down the processes"}),
    );
    assert!(
        !again["error"].as_str().unwrap_or("").contains("refused: t"),
        "{again}"
    );

    let note = app.remembered_tabs_now(&[crate::assistant_memory::TabRef {
        tab: format!("t{gas_uid}"),
        session: Some("6c7bf0d3-1833-4f50-9b46-5ee6c711144f".into()),
        folder: None,
    }]);
    assert!(note.is_empty(), "{note}");
    assert!(app.assistant.stale_ids.is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn account_is_not_out_after_its_reset_passed() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("reset-passed");
    let at = |h: i64| (chrono::Utc::now() + chrono::Duration::hours(h)).to_rfc3339();
    let usage = |weekly_reset: String| {
        crate::usage::parse_usage(&format!(
            r#"{{"five_hour":{{"utilization":10.0,"resets_at":"{}"}},"seven_day":{{"utilization":100.0,"resets_at":"{weekly_reset}"}}}}"#,
            at(2)
        ))
        .unwrap()
    };
    app.accounts[0].usage = Some(usage(at(5)));
    assert!(app.state_preamble().contains("OUT (weekly limit"));
    assert!(app.out_warning(0).is_some());

    app.accounts[0].usage = Some(usage(at(-1)));
    app.accounts[0].usage_err = Some(crate::usage::UsageError::RateLimited(None));
    assert!(!app.state_preamble().contains("OUT (weekly limit"));
    assert!(app.out_warning(0).is_none());
    let st = app.control_call("get_state", &json!({}));
    assert_eq!(
        st["result"]["accounts"][0]["available"],
        json!(true),
        "{st}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn usage_after_reset_reads_reset_refreshing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("reset-raw");
    app.accounts[0].usage = Some(
        crate::usage::parse_usage(r#"{"five_hour":{"utilization":97.0,"resets_at":"2001-01-01T00:00:00+00:00"},"seven_day":{"utilization":40.0,"resets_at":"2099-01-02T00:00:00+00:00"}}"#)
            .unwrap(),
    );
    assert_eq!(app.accounts[0].five_hour_left(), Some(100.0));
    assert!(
        app.state_preamble().contains("5h reset, refreshing"),
        "{}",
        app.state_preamble()
    );
    let st = app.control_call("get_state", &json!({}));
    assert_eq!(
        st["result"]["accounts"][0]["five_hour_reset_passed"],
        json!(true)
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn memory_marks_old_pending_questions() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, home) = test_app("old-pending");
    let dir = crate::assistant_history::dir();
    std::fs::create_dir_all(&dir).unwrap();
    let at = (chrono::Local::now() - chrono::Duration::hours(1)).to_rfc3339();
    let write = |name: &str, lines: &[serde_json::Value]| {
        let p = dir.join(format!("{name}.jsonl"));
        let text: String = lines
            .iter()
            .map(|l| {
                let mut l = l.clone();
                l["at"] = json!(at);
                format!("{l}\n")
            })
            .collect();
        std::fs::write(&p, text).unwrap();
        p
    };
    let confirm = write(
        "2026-10-06-130000-aaaaaa",
        &[
            json!({"kind": "user", "text": "close all the tabs"}),
            json!({"kind": "tool", "name": "close_tabs", "args": {"tab": "all"}, "result": "{\"ok\":false,\"needs_confirmation\":true}"}),
            json!({"kind": "reply", "text": "Close 9 tabs?"}),
        ],
    );
    let s = crate::assistant_memory::remember(&confirm).unwrap().summary;
    assert!(
        !s.contains("Close 9 tabs?"),
        "an expired confirmation is not carried: {s}"
    );
    let question = write(
        "2026-10-06-130500-bbbbbb",
        &[
            json!({"kind": "user", "text": "which tab is the api"}),
            json!({"kind": "reply", "text": "Do you mean the one on account 2?"}),
        ],
    );
    let s = crate::assistant_memory::remember(&question)
        .unwrap()
        .summary;
    assert!(
        s.contains("Left open then") && s.contains("may be resolved"),
        "{s}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn tab_ids_are_reserved_on_disk_ahead() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, home) = test_app("uid-reserve");

    let p = (0..40)
        .map(|_| crate::pane::Pane::new(home.clone()))
        .last()
        .unwrap();
    let on_disk = crate::pane::reserved_on_disk();
    assert!(on_disk > p.uid, "{on_disk} > {}", p.uid);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn undo_toast_and_barge_in_end_across_sleep() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    crate::clock::reset();
    let (mut app, home) = test_app("toast-sleep");
    app.undo_toast = Some((std::time::Instant::now(), "Closed 2 tabs".into()));
    assert_eq!(app.toast_text(), Some("Closed 2 tabs"));
    app.assistant.cut_off = Some(std::time::Instant::now());
    assert!(app.take_cut_off());
    assert!(!app.take_cut_off(), "used once");
    app.assistant.cut_off = Some(std::time::Instant::now());
    crate::clock::simulate_sleep(Duration::from_secs(3600));
    assert_eq!(app.toast_text(), None);
    assert!(!app.take_cut_off());
    crate::clock::reset();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn old_tab_history_ids_resolve_by_session() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("old-history");
    let acct = app.cfg.accounts[0].name.clone();
    let rec = |id: &str, tab: &str, name: &str, sid: &str| crate::tab_history::Record {
        id: id.into(),
        at: chrono::Local::now().to_rfc3339(),
        event: "close".into(),
        account: acct.clone(),
        tab: tab.into(),
        name: name.into(),
        cwd: home.join("newtabs").to_string_lossy().into_owned(),
        session_id: Some(sid.into()),
        ..Default::default()
    };

    crate::tab_history::append(&rec(
        "aaaa1111",
        "t5",
        "gas",
        "11111111-aaaa-4000-8000-000000000001",
    ));
    crate::tab_history::append(&rec(
        "bbbb2222",
        "t5",
        "api",
        "22222222-bbbb-4000-8000-000000000002",
    ));
    crate::tab_history::append(&rec(
        "cccc3333",
        "t7",
        "docs",
        "33333333-cccc-4000-8000-000000000003",
    ));
    let v = app.control_call("reopen_tab", &json!({"id": "t5"}));
    let e = v["error"].as_str().unwrap_or("");
    assert!(
        e.contains("more than one")
            && e.contains("'gas'")
            && e.contains("'api'")
            && e.contains("aaaa1111"),
        "{v}"
    );

    let v = app.control_call("reopen_tab", &json!({"id": "t7"}));
    assert_eq!(v["result"]["reopened"], json!(true), "{v}");
    assert!(app
        .panes
        .iter()
        .flat_map(|p| p.tabs.iter())
        .any(|t| t.session_id.as_deref() == Some("33333333-cccc-4000-8000-000000000003")));
    let v = app.control_call("reopen_tab", &json!({"id": "t99"}));
    assert!(
        v["error"].as_str().unwrap_or("").contains("no tab history"),
        "{v}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
