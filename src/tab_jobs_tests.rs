//! Jobs for the agents inside tabs (tab_jobs.rs): the tab client's token,
//! its scoping to its own tab's jobs and loops, the assistant-only tools
//! staying out of reach, the user's yes in GodTerm's own dialog, and the
//! launch arguments of Claude and Grok tabs. Nothing real runs: tabs are
//! stubs and homes are test homes.

use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
#[cfg(windows)]
use uds_windows::UnixStream;

use crate::app::{App, AppEvent, Modal};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::control::tab_id;
use crate::harness::Harness;
use crate::jobs_tests::{add, loop_row, running, session_loop, setup};
use crate::tab_jobs::{self, HINT, TAB_TOOLS};
use serde_json::{json, Value};

const SHARED: &str = "shared-rw-token";

/// A call from the agent of tab `uid`, the way the socket hands it over.
fn tab_call(app: &mut App, uid: u64, tool: &str, args: Value) -> Value {
    let mut a = args;
    a["_client"] = json!(format!("tab:{uid}"));
    app.control_call(tool, &a)
}

/// Two running tabs on pane 0: (app, home, uid of billing, uid of web).
fn two_tabs(tag: &str) -> (App, std::path::PathBuf, u64, u64) {
    let (mut app, home) = setup(tag);
    let a = running(&mut app, 0, "billing", "sess-b");
    let b = running(&mut app, 0, "web", "sess-w");
    let (ua, ub) = (app.panes[0].tabs[a].uid, app.panes[0].tabs[b].uid);
    (app, home, ua, ub)
}

/// A job of the tab `uid` has.
fn job_of(app: &mut App, uid: u64, name: &str, prompt: &str) -> String {
    let (s, t) = app.find_tab(uid).unwrap();
    add(app, s, t, name, prompt, 15)
}

/// A session loop of the tab `uid`.
fn loop_of(app: &App, uid: u64, id: &str, prompt: &str) -> crate::app_loops::LoopRow {
    let (s, t) = app.find_tab(uid).unwrap();
    loop_row(app, s, t, session_loop(id, "*/15 * * * *", prompt))
}

/// The user answers the dialog on screen.
fn answer(app: &mut App, yes: bool) {
    app.tab_jobs_tick();
    let Modal::ConfirmTabJob(tok) = std::mem::replace(&mut app.modal, Modal::None) else {
        panic!("no job dialog is open");
    };
    app.tab_ask_answer(&tok, yes);
}

// ---- the token and the socket ----

#[test]
fn a_tab_token_names_one_tab_of_one_launch() {
    let t7 = tab_jobs::tab_token(SHARED, 7);
    assert_eq!(t7, tab_jobs::tab_token(SHARED, 7));
    assert_ne!(t7, tab_jobs::tab_token(SHARED, 8), "another tab");
    assert_ne!(
        t7,
        tab_jobs::tab_token("another-launch", 7),
        "another launch"
    );
    assert_ne!(t7, SHARED, "never the control token itself");
    assert!(tab_jobs::token_ok(SHARED, 7, &t7));
    assert!(!tab_jobs::token_ok(SHARED, 8, &t7));
    assert!(!tab_jobs::token_ok(SHARED, 7, SHARED));
    assert!(!tab_jobs::token_ok(SHARED, 7, ""));
}

#[test]
fn authorize_checks_the_token_the_tab_and_the_tool() {
    let t7 = tab_jobs::tab_token(SHARED, 7);
    let ok = |tool: &str, tab: u64, token: &str| {
        tab_jobs::authorize(
            SHARED,
            &json!({"client": "tab", "tab": tab, "token": token, "tool": tool}),
        )
    };
    assert_eq!(ok("create_job", 7, &t7), Ok(7));
    // Another tab's id with this tab's token, or no tab at all.
    assert_eq!(ok("create_job", 8, &t7), Err("bad token".into()));
    assert_eq!(ok("create_job", 0, &t7), Err("bad token".into()));
    // The control tokens are not tab tokens.
    assert_eq!(ok("create_job", 7, SHARED), Err("bad token".into()));
    // Tools of the assistant, the settings, the keychain: no.
    for tool in [
        "get_state",
        "set_setting",
        "close_tabs",
        "send_prompt",
        "run_job_now",
        "list_tabs",
    ] {
        let e = ok(tool, 7, &t7).unwrap_err();
        assert!(e.contains("not available to a tab"), "{tool}: {e}");
    }
    // A tab cannot answer a confirmation by itself.
    let r = tab_jobs::authorize(
        SHARED,
        &json!({"client": "tab", "tab": 7, "token": t7, "tool": "create_job", "user_says": "yes"}),
    );
    assert!(r.unwrap_err().contains("come from the user"));
}

#[test]
fn the_socket_hands_a_tab_call_over_as_that_tab_and_refuses_the_rest() {
    let (mut client, server) = UnixStream::pair().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<AppEvent>();
    let t = std::thread::spawn(move || crate::control::handle_for_tests(server, tx, SHARED, "ro"));
    let mut rd = BufReader::new(client.try_clone().unwrap());
    let mut ask = move |req: Value| {
        writeln!(client, "{req}").unwrap();
        let mut line = String::new();
        rd.read_line(&mut line).unwrap();
        line
    };
    let t7 = tab_jobs::tab_token(SHARED, 7);
    // The control token (read-write or read-only) is no tab token.
    for token in [SHARED, "ro"] {
        let l = ask(
            json!({"client": "tab", "tab": 7, "token": token, "tool": "create_job", "args": {}}),
        );
        assert!(l.contains("bad token"), "{l}");
    }
    // A tab token does not work as another client kind either.
    let l = ask(json!({"client": "brain", "token": t7, "tool": "get_state", "args": {}}));
    assert!(l.contains("bad token"), "{l}");
    let l = ask(json!({"token": t7, "tool": "get_state", "args": {}}));
    assert!(l.contains("bad token"), "{l}");
    // Its own tab's token, a tool it has: handed on, as that tab, with
    // whatever `_client` the agent put in its arguments replaced.
    let answer = std::thread::spawn(move || {
        let AppEvent::Control(tool, args, reply) = rx.recv().unwrap() else {
            panic!("not a control event")
        };
        reply
            .send(json!({"ok": true, "result": {"seen": tool}}))
            .unwrap();
        args
    });
    let l = ask(
        json!({"client": "tab", "tab": 7, "token": t7, "tool": "list_jobs",
        "args": {"_client": "brain", "status": "active"}}),
    );
    assert!(l.contains("list_jobs"), "{l}");
    let args = answer.join().unwrap();
    assert_eq!(args["_client"], json!("tab:7"), "{args}");
    assert_eq!(args["status"], json!("active"));
    // A tool a tab does not have never reaches the app.
    let l = ask(json!({"client": "tab", "tab": 7, "token": t7, "tool": "get_state", "args": {}}));
    assert!(l.contains("not available to a tab"), "{l}");
    drop(ask);
    t.join().unwrap();
}

// ---- scoping ----

#[test]
fn a_tab_creates_a_job_for_itself_after_the_users_yes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, _) = two_tabs("tabjobs-create");
    let v = tab_call(
        &mut app,
        ua,
        "create_job",
        json!({"name": "poll", "prompt": "check the deploy", "every": "30 minutes"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["result"]["state"], json!("waiting_for_user"), "{v}");
    assert!(app.jobs.jobs.is_empty(), "nothing before the yes");
    // Shown against that tab.
    app.tab_jobs_tick();
    let Modal::ConfirmTabJob(tok) = app.modal.clone() else {
        panic!("{:?}", app.modal)
    };
    let (tab, q) = app.tab_ask_text(&tok).unwrap();
    assert!(tab.contains("billing"), "{tab}");
    assert!(
        q.contains("billing") && q.contains("check the deploy"),
        "{q}"
    );
    app.tab_ask_answer(&tok, true);
    assert_eq!(app.jobs.jobs.len(), 1);
    let j = &app.jobs.jobs[0];
    assert_eq!(j.target.uid, Some(ua));
    assert_eq!(j.created_by, "tab");
    assert_eq!(j.every_min, Some(30));
    // It outlives the call: persisted for the next launch.
    assert_eq!(crate::jobs::Store::load().jobs.len(), 1);
    // And the agent can see it, with the answer it got.
    let l = tab_call(&mut app, ua, "list_jobs", json!({}));
    assert_eq!(l["result"]["jobs"][0]["name"], json!("poll"), "{l}");
    assert!(l["result"]["your_requests"][0]
        .as_str()
        .unwrap()
        .starts_with("approved"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_no_or_a_closed_dialog_creates_nothing_and_the_agent_hears_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, _) = two_tabs("tabjobs-no");
    tab_call(
        &mut app,
        ua,
        "create_job",
        json!({"prompt": "one", "every": "1 hour"}),
    );
    answer(&mut app, false);
    assert!(app.jobs.jobs.is_empty());
    // The dialog closed another way (the Cancel button) counts as a no.
    tab_call(
        &mut app,
        ua,
        "create_job",
        json!({"prompt": "two", "every": "1 hour"}),
    );
    app.tab_jobs_tick();
    assert!(matches!(app.modal, Modal::ConfirmTabJob(_)));
    app.modal = Modal::None;
    app.tab_jobs_tick();
    assert!(app.jobs.jobs.is_empty());
    assert!(app.tab_jobs.asks.is_empty());
    let l = tab_call(&mut app, ua, "list_jobs", json!({}));
    let said = l["result"]["your_requests"].to_string();
    assert!(said.matches("declined by the user").count() == 2, "{said}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn naming_another_tab_or_anything_outside_the_tab_is_rejected() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, ub) = two_tabs("tabjobs-reject");
    let other = tab_id(ub);
    for args in [
        json!({"prompt": "x", "every": "1 hour", "tab": other}),
        json!({"prompt": "x", "every": "1 hour", "tab": "web"}),
        json!({"prompt": "x", "every": "1 hour", "tab": [other]}),
        json!({"prompt": "x", "every": "1 hour", "archived": "abc"}),
        json!({"prompt": "x", "every": "1 hour", "account": 2}),
        json!({"prompt": "x", "every": "1 hour", "confirm_token": "deadbeef"}),
        json!({"prompt": "x", "every": "1 hour", "reissue_token": "deadbeef"}),
        json!({"prompt": "x", "every": "1 hour", "_only_uid": ub}),
    ] {
        let v = tab_call(&mut app, ua, "create_job", args.clone());
        assert_eq!(v["ok"], json!(false), "{args}: {v}");
        assert!(
            app.tab_jobs.asks.is_empty(),
            "{args}: nothing was even asked"
        );
    }
    // Its own id (any case) or "this" is fine, and means the same tab.
    for t in [tab_id(ua), tab_id(ua).to_uppercase(), "this".to_string()] {
        let v = tab_call(
            &mut app,
            ua,
            "create_job",
            json!({"prompt": "x", "every": "1 hour", "tab": t}),
        );
        assert_eq!(v["result"]["state"], json!("waiting_for_user"), "{t}: {v}");
    }
    assert!(app.tab_jobs.asks.iter().all(|k| k.uid == ua));
    assert!(app.jobs.jobs.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn only_the_five_job_tools_work_for_a_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, _) = two_tabs("tabjobs-tools");
    for tool in [
        "get_state",
        "list_tabs",
        "send_prompt",
        "close_tabs",
        "set_setting",
        "run_job_now",
        "read_tab",
        "read_file",
        "enable_remote_control",
        "edit_system_prompt",
    ] {
        let v = tab_call(&mut app, ua, tool, json!({}));
        assert_eq!(v["ok"], json!(false), "{tool}: {v}");
        assert!(
            v["error"]
                .as_str()
                .unwrap()
                .contains("not available to a tab"),
            "{tool}: {v}"
        );
    }
    // Not a tab that is gone, either.
    let v = tab_call(&mut app, 999_999, "list_jobs", json!({}));
    assert_eq!(v["error"], json!("this tab is gone"));
    // The tab's own MCP server lists exactly the five, trimmed of the
    // arguments that name another tab or confirm.
    let list = tab_jobs::tab_tool_list();
    let names: Vec<&str> = list.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), TAB_TOOLS.len(), "{names:?}");
    for t in TAB_TOOLS {
        assert!(names.contains(t), "{t}");
    }
    for t in &list {
        let props = t["inputSchema"]["properties"].as_object().unwrap();
        for k in [
            "tab",
            "archived",
            "account",
            "confirm_token",
            "reissue_token",
        ] {
            assert!(!props.contains_key(k), "{}: {k}", t["name"]);
        }
    }
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_tab_mcp_server_lists_five_tools_and_refuses_the_others_before_the_socket() {
    let calls = std::cell::Cell::new(0);
    let mut call = |_: &str, _: &Value| {
        calls.set(calls.get() + 1);
        Ok(json!({"ok": true, "result": {}}))
    };
    let tools = crate::mcp::handle_as(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        &mut call,
        true,
    )
    .unwrap();
    assert_eq!(
        tools["result"]["tools"].as_array().unwrap().len(),
        TAB_TOOLS.len()
    );
    let r = crate::mcp::handle_as(
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "get_state", "arguments": {}}}),
        &mut call,
        true,
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], json!(true), "{r}");
    let r = crate::mcp::handle_as(
        &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "list_jobs", "arguments": {}}}),
        &mut call,
        true,
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], json!(false), "{r}");
    assert_eq!(calls.get(), 1, "only the job tool went on");
    // The assistant's server is unchanged: it has every tool.
    let all = crate::mcp::handle(
        &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}),
        &mut call,
    )
    .unwrap();
    assert!(all["result"]["tools"].as_array().unwrap().len() > 40);
}

#[test]
fn list_jobs_shows_only_the_tabs_own_jobs_and_loops() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, ub) = two_tabs("tabjobs-list");
    job_of(&mut app, ua, "billing poll", "mine");
    job_of(&mut app, ub, "web secret", "theirs");
    app.loops = vec![
        loop_of(&app, ua, "aaaaaaaa", "my loop"),
        loop_of(&app, ub, "bbbbbbbb", "their loop"),
    ];
    let l = tab_call(&mut app, ua, "list_jobs", json!({}));
    let all = l.to_string();
    assert!(
        all.contains("billing poll") && all.contains("my loop"),
        "{all}"
    );
    assert!(
        !all.contains("web secret") && !all.contains("their loop"),
        "{all}"
    );
    assert!(!all.contains(&tab_id(ub)), "no other tab id: {all}");
    // The assistant still sees both.
    let a = app
        .control_call("list_jobs", &json!({"_client": "brain"}))
        .to_string();
    assert!(a.contains("web secret") && a.contains("their loop"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn update_and_delete_reach_only_the_tabs_own_jobs() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, ub) = two_tabs("tabjobs-edit");
    let mine = job_of(&mut app, ua, "billing poll", "mine");
    let theirs = job_of(&mut app, ub, "web poll", "theirs");
    // Another tab's job, by id, prefix or name: the same "no such job",
    // which says nothing about it.
    for key in [theirs.clone(), theirs[..4].to_string(), "web".to_string()] {
        for (tool, args) in [
            ("update_job", json!({"job": key, "action": "pause"})),
            ("delete_job", json!({"job": key})),
        ] {
            let v = tab_call(&mut app, ua, tool, args);
            assert_eq!(v["ok"], json!(false), "{tool} {key}: {v}");
            assert!(
                v["error"]
                    .as_str()
                    .unwrap()
                    .starts_with("no job of this tab"),
                "{v}"
            );
            assert!(!v.to_string().contains("web poll"), "{v}");
        }
    }
    assert!(app.jobs.find(&theirs).unwrap().enabled);
    // Its own: pause applies at once, a new prompt and a delete ask.
    let v = tab_call(
        &mut app,
        ua,
        "update_job",
        json!({"job": "billing", "action": "pause"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(!app.jobs.find(&mine).unwrap().enabled);
    let v = tab_call(
        &mut app,
        ua,
        "update_job",
        json!({"job": mine, "prompt": "new words"}),
    );
    assert_eq!(v["result"]["state"], json!("waiting_for_user"), "{v}");
    assert_eq!(app.jobs.find(&mine).unwrap().prompt, "mine");
    answer(&mut app, true);
    assert_eq!(app.jobs.find(&mine).unwrap().prompt, "new words");
    // Moving it to another tab is not something it can ask for.
    let v = tab_call(
        &mut app,
        ua,
        "update_job",
        json!({"job": mine, "tab": "web"}),
    );
    assert_eq!(v["ok"], json!(false), "{v}");
    let v = tab_call(&mut app, ua, "delete_job", json!({"job": mine}));
    assert_eq!(v["result"]["state"], json!("waiting_for_user"), "{v}");
    assert!(app.jobs.find(&mine).is_some());
    answer(&mut app, true);
    assert!(app.jobs.find(&mine).is_none());
    assert!(app.jobs.find(&theirs).is_some());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn adopt_loop_takes_only_the_tabs_own_session_loop() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, ub) = two_tabs("tabjobs-adopt");
    app.loops = vec![
        loop_of(&app, ua, "aaaaaaaa", "my loop"),
        loop_of(&app, ub, "bbbbbbbb", "their loop"),
    ];
    // Their loop id, or their tab: no.
    let v = tab_call(&mut app, ua, "adopt_loop", json!({"loop": "bbbbbbbb"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    let v = tab_call(&mut app, ua, "adopt_loop", json!({"tab": tab_id(ub)}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(app.tab_jobs.asks.is_empty());
    // Its own: asks, then the job is made for this tab from that loop.
    let v = tab_call(&mut app, ua, "adopt_loop", json!({}));
    assert_eq!(v["result"]["state"], json!("waiting_for_user"), "{v}");
    answer(&mut app, true);
    assert_eq!(app.jobs.jobs.len(), 1);
    let j = &app.jobs.jobs[0];
    assert_eq!(j.target.uid, Some(ua));
    assert_eq!(j.prompt, "my loop");
    assert_eq!(j.created_by, "tab");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_tab_cannot_flood_the_user_with_requests() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, _) = two_tabs("tabjobs-flood");
    let same = || json!({"prompt": "same", "every": "1 hour"});
    tab_call(&mut app, ua, "create_job", same());
    tab_call(&mut app, ua, "create_job", same());
    assert_eq!(app.tab_jobs.asks.len(), 1, "the same request is asked once");
    for i in 0..10 {
        tab_call(
            &mut app,
            ua,
            "create_job",
            json!({"prompt": format!("p{i}"), "every": "1 hour"}),
        );
    }
    assert_eq!(app.tab_jobs.asks.len(), 5, "at most five wait at once");
    let v = tab_call(
        &mut app,
        ua,
        "create_job",
        json!({"prompt": "more", "every": "1 hour"}),
    );
    assert_eq!(v["ok"], json!(false), "{v}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_waiting_request_does_not_cover_another_dialog_and_expires() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, ua, _) = two_tabs("tabjobs-wait");
    app.modal = Modal::Help;
    tab_call(
        &mut app,
        ua,
        "create_job",
        json!({"prompt": "x", "every": "1 hour"}),
    );
    app.tab_jobs_tick();
    assert!(
        matches!(app.modal, Modal::Help),
        "the user's own dialog stays"
    );
    app.modal = Modal::None;
    app.tab_jobs_tick();
    assert!(matches!(app.modal, Modal::ConfirmTabJob(_)));
    // An unanswered one expires.
    app.tab_jobs.asks[0].at = std::time::Instant::now() - std::time::Duration::from_secs(700);
    app.tab_jobs_tick();
    assert!(app.tab_jobs.asks.is_empty());
    assert!(matches!(app.modal, Modal::None));
    assert!(app.jobs.jobs.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

// ---- the launch ----

fn exe() -> std::path::PathBuf {
    "/usr/local/bin/godterm".into()
}

#[test]
fn a_claude_launch_gets_the_server_the_tools_and_the_hint() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, home) = setup("tabjobs-claude");
    let mut args: Vec<String> = vec!["--dangerously-skip-permissions".into()];
    let x = tab_jobs::extend_launch(Harness::Claude, &exe(), SHARED, 12, &mut args).unwrap();
    assert!(x.env.is_empty());
    assert!(
        !args.iter().any(|a| a == "--strict-mcp-config"),
        "the user's own MCP servers stay"
    );
    let i = args.iter().position(|a| a == "--mcp-config").unwrap();
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&args[i + 1]).unwrap()).unwrap();
    let s = &cfg["mcpServers"]["godterm"];
    assert_eq!(s["args"], json!(["mcp"]));
    assert_eq!(s["env"]["GODTERM_CLIENT"], json!("tab"));
    assert_eq!(s["env"]["GODTERM_TAB"], json!("12"));
    let tok = s["env"]["GODTERM_TOKEN"].as_str().unwrap();
    assert!(tab_jobs::token_ok(SHARED, 12, tok));
    assert_ne!(tok, SHARED, "never the control token");
    let j = args
        .iter()
        .position(|a| a == "--append-system-prompt")
        .unwrap();
    assert_eq!(args[j + 1], HINT);
    assert!(HINT.contains("create_job") && HINT.contains("CronCreate") && HINT.contains("/loop"));
    let k = args.iter().position(|a| a == "--allowedTools").unwrap();
    assert_eq!(args[k + 1], "mcp__godterm__create_job");
    assert!(args.contains(&"mcp__godterm__adopt_loop".to_string()));
    assert!(!args.iter().any(|a| a == "mcp__godterm__get_state"));
    // The file holds a token: private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&args[i + 1])
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_users_own_flags_and_a_resume_are_left_as_they_were() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, home) = setup("tabjobs-resume");
    let before: Vec<String> = [
        "--resume",
        "abc-123",
        "--dangerously-skip-permissions",
        "--mcp-config",
        "/home/me/mine.json",
        "--append-system-prompt",
        "be brief",
        "--fork-session",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut args = before.clone();
    tab_jobs::extend_launch(Harness::Claude, &exe(), SHARED, 3, &mut args).unwrap();
    // Everything that was there is still there, in place, ahead of ours.
    let mut merged = before.clone();
    merged[6] = format!("be brief\n\n{HINT}");
    assert_eq!(&args[..before.len()], &merged[..], "{args:?}");
    assert_eq!(
        args.iter()
            .filter(|a| *a == "--append-system-prompt")
            .count(),
        1
    );
    assert_eq!(
        args.iter().filter(|a| *a == "--mcp-config").count(),
        2,
        "theirs and ours"
    );
    assert!(args.iter().any(|a| a == "/home/me/mine.json"));
    // The `=` form is merged too.
    let mut args: Vec<String> = vec!["--append-system-prompt=be brief".into()];
    tab_jobs::extend_launch(Harness::Claude, &exe(), SHARED, 3, &mut args).unwrap();
    assert_eq!(
        args[0],
        format!("--append-system-prompt=be brief\n\n{HINT}")
    );
    assert_eq!(
        args.iter()
            .filter(|a| a.starts_with("--append-system-prompt"))
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_launch_adds_the_tools_only_while_the_switch_is_on() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("tabjobs-switch");
    assert!(app.cfg.tabs.jobs_tools, "on by default");
    let cfg: crate::config::Config = toml::from_str("[tabs]\njobs_tools = false\n").unwrap();
    assert!(!cfg.tabs.jobs_tools);
    let cfg: crate::config::Config = toml::from_str("").unwrap();
    assert!(cfg.tabs.jobs_tools);
    let dir = home.join("slot");
    std::fs::create_dir_all(&dir).unwrap();
    let mut args = vec!["--resume".to_string(), "abc".to_string()];
    let env = app.tab_jobs_launch(Harness::Claude, 5, &dir, &mut args, false);
    assert!(env.is_empty());
    assert!(args.contains(&"--mcp-config".to_string()) && args.len() > 2);
    app.cfg.tabs.jobs_tools = false;
    let mut args = vec!["--resume".to_string(), "abc".to_string()];
    let env = app.tab_jobs_launch(Harness::Claude, 5, &dir, &mut args, false);
    assert!(env.is_empty());
    assert_eq!(
        args,
        vec!["--resume".to_string(), "abc".to_string()],
        "unchanged when off"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_grok_launch_gets_the_hint_its_identity_and_the_server_in_its_slot_config() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("tabjobs-grok");
    let slot = home.join("grok-slot");
    std::fs::create_dir_all(&slot).unwrap();
    // The user's own servers and settings in that slot are kept.
    std::fs::write(
        slot.join("config.toml"),
        "[ui]\nscreen_mode = \"minimal\"\n\n[mcp_servers.mine]\ncommand = \"/bin/mine\"\n",
    )
    .unwrap();
    let mut args: Vec<String> = vec![
        "--resume".into(),
        "sess-1".into(),
        "--rules".into(),
        "be brief".into(),
    ];
    let env = app.tab_jobs_launch(Harness::Grok, 21, &slot, &mut args, false);
    assert_eq!(&args[..2], ["--resume", "sess-1"]);
    let i = args.iter().position(|a| a == "--rules").unwrap();
    assert_eq!(args[i + 1], format!("be brief\n\n{HINT}"));
    assert_eq!(args.iter().filter(|a| *a == "--rules").count(), 1);
    assert!(!args
        .iter()
        .any(|a| a.contains("mcp-config") || a.contains("strict")));
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(get("GODTERM_TAB").as_deref(), Some("21"));
    assert!(tab_jobs::token_ok(
        SHARED_TEST(),
        21,
        &get("GODTERM_TOKEN").unwrap()
    ));
    let doc: toml::Value =
        toml::from_str(&std::fs::read_to_string(slot.join("config.toml")).unwrap()).unwrap();
    assert_eq!(doc["ui"]["screen_mode"].as_str(), Some("minimal"));
    assert_eq!(
        doc["mcp_servers"]["mine"]["command"].as_str(),
        Some("/bin/mine")
    );
    let g = &doc["mcp_servers"]["godterm"];
    assert_eq!(g["args"][0].as_str(), Some("mcp"));
    assert_eq!(g["env"]["GODTERM_CLIENT"].as_str(), Some("tab"));
    // The tab's token is not in the shared config file, only in its env.
    assert!(g["env"].get("GODTERM_TOKEN").is_none());
    assert!(!std::fs::read_to_string(slot.join("config.toml"))
        .unwrap()
        .contains(&get("GODTERM_TOKEN").unwrap()));
    // Switched off: our entry goes, theirs stays.
    app.cfg.tabs.jobs_tools = false;
    let mut args: Vec<String> = vec![];
    assert!(app
        .tab_jobs_launch(Harness::Grok, 21, &slot, &mut args, false)
        .is_empty());
    assert!(args.is_empty());
    let doc: toml::Value =
        toml::from_str(&std::fs::read_to_string(slot.join("config.toml")).unwrap()).unwrap();
    assert!(doc["mcp_servers"].get("godterm").is_none());
    assert!(doc["mcp_servers"].get("mine").is_some());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_grok_server_of_the_users_own_called_godterm_is_not_overwritten() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (app, home) = setup("tabjobs-grok-own");
    let slot = home.join("grok-own");
    std::fs::create_dir_all(&slot).unwrap();
    let own = "[mcp_servers.godterm]\ncommand = \"/opt/my-godterm\"\n";
    std::fs::write(slot.join("config.toml"), own).unwrap();
    let mut args = vec![];
    app.tab_jobs_launch(Harness::Grok, 2, &slot, &mut args, false);
    assert_eq!(
        std::fs::read_to_string(slot.join("config.toml")).unwrap(),
        own
    );
    let _ = std::fs::remove_dir_all(home);
}

#[allow(non_snake_case)]
fn SHARED_TEST() -> &'static str {
    "test-shared-token"
}

#[test]
fn the_tab_server_entry_matches_what_the_socket_expects() {
    let s = tab_jobs::mcp_server(Path::new("/x/godterm"), SHARED, 9);
    // The client kind and tab it sends are what `authorize` checks.
    let token = s["env"]["GODTERM_TOKEN"].as_str().unwrap();
    let req = json!({"client": s["env"]["GODTERM_CLIENT"], "tab": 9, "token": token, "tool": "list_jobs"});
    assert_eq!(tab_jobs::authorize(SHARED, &req), Ok(9));
}

#[test]
fn the_hint_says_how_to_drive_the_browser_safely() {
    for want in [
        "--tab-id",
        "wait-for-element",
        "request-help",
        "tab borrow",
        "blocked CDP access",
        "--help",
    ] {
        assert!(HINT.contains(want), "hint lacks {want}");
    }
}
