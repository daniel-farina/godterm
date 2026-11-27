use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::Activity;

fn test_app(tag: &str) -> (App, PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-tabconv-{tag}-{}", std::process::id()));
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

fn transcript_dir(app: &App, proj: &Path) -> PathBuf {
    let a = app.panes[0].account.unwrap();
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(proj));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn tab(app: &mut App, home: &Path, name: &str, user: &str, last: &str) -> u64 {
    let proj = home.join("newtabs").join(name);
    std::fs::create_dir_all(&proj).unwrap();
    let t = app.panes[0].add_tab(proj.clone());
    let sid = format!("7777aaaa-0000-4000-8000-{:012}", t);
    let tb = &mut app.panes[0].tabs[t];
    tb.session_id = Some(sid.clone());
    tb.custom_name = Some(name.into());
    tb.activity = Activity::Ready;
    let uid = tb.uid;
    let dir = transcript_dir(app, &proj);
    let u = json!({"type": "user", "timestamp": "2026-10-28T10:00:00.000Z", "message": {"role": "user", "content": user}});
    let a = json!({"type": "assistant", "timestamp": "2026-10-28T10:00:05.000Z", "message": {"id": "m1", "content": [{"type": "text", "text": last}], "stop_reason": "end_turn"}});
    std::fs::write(dir.join(format!("{sid}.jsonl")), format!("{u}\n{a}\n")).unwrap();
    uid
}

fn recorded(rec: &Path) -> String {
    for _ in 0..60 {
        let s = std::fs::read_to_string(rec).unwrap_or_default();
        if !s.is_empty() {
            return s;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    String::new()
}

fn rig(tag: &str) -> (App, PathBuf, u64, u64) {
    let (mut app, home) = test_app(tag);
    let a = tab(
        &mut app,
        &home,
        "alphaship",
        "migrate the billing tables",
        "Migration 12 applied; 3 tables left.",
    );
    let b = tab(
        &mut app,
        &home,
        "betaharbor",
        "rotate the zebra keys",
        "Zebra keys rotated everywhere.",
    );
    (app, home, a, b)
}

#[test]
fn the_context_describes_this_tab_and_no_other() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (app, home, a, b) = rig("ctx");
    let c = app.tab_conv_context(a);
    assert!(
        c.starts_with("<tab_context>") && c.ends_with("</tab_context>"),
        "{c}"
    );
    assert!(
        c.contains("alphaship") && c.contains(&format!("t{a}")),
        "{c}"
    );
    assert!(c.contains("Migration 12 applied; 3 tables left."), "{c}");
    assert!(c.contains("migrate the billing tables"), "{c}");
    assert!(c.contains("state: ready"), "{c}");
    for leak in ["betaharbor", "zebra", "Zebra", &format!("t{b}")] {
        assert!(!c.contains(leak), "leaked {leak}: {c}");
    }
    let other = app.tab_conv_context(b);
    assert!(
        other.contains("betaharbor") && !other.contains("alphaship") && !other.contains("billing")
    );
    assert!(c.len() < 4500, "{}", c.len());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_working_tab_shows_its_screen_and_a_waiting_tab_what_it_asks() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, _b) = rig("states");
    let (s, t) = app.find_tab(a).unwrap();
    app.panes[s].tabs[t].activity = Activity::Working;
    app.panes[s].tabs[t]
        .parser
        .lock()
        .unwrap()
        .process(b"* Pondering the schema (12s . esc to interrupt)\r\n");
    let c = app.tab_conv_context(a);
    assert!(c.contains("state: working"), "{c}");
    assert!(
        c.contains("screen (last lines)") && c.contains("Pondering the schema"),
        "{c}"
    );

    app.panes[s].tabs[t].activity = Activity::Permission;
    app.panes[s].tabs[t].parser.lock().unwrap().process(
        b"\x1b[2J\x1b[HBash command\r\n\r\n   rm -rf build\r\n   Remove the build folder\r\n\r\nDo you want to proceed?\r\n\xe2\x9d\xaf 1. Yes\r\n  2. Yes, and don't ask again for rm commands\r\n  3. No, and tell Claude what to do differently (esc)\r\n",
    );
    let c = app.tab_conv_context(a);
    assert!(c.contains("WAITING FOR THE USER: permission:"), "{c}");
    assert!(c.contains("rm -rf build"), "{c}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn queued_prompts_and_notes_are_in_the_context() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, _b) = rig("notes");
    app.tab_conv_open(a).unwrap();
    let r = app.control_call("tab_note", &json!({"text": "prefers small commits"}));
    assert!(r["error"].is_null(), "{r}");
    let c = app.tab_conv_context(a);
    assert!(c.contains("your note 1: prefers small commits"), "{c}");
    let r = app.control_call("tab_context", &json!({"lines": 5}));
    assert!(
        r["result"]["context"]
            .as_str()
            .unwrap()
            .contains("alphaship"),
        "{r}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_tab_conversation_can_only_reach_its_own_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, b) = rig("scope");
    app.tab_conv_open(a).unwrap();
    let ida = format!("t{a}");
    let idb = format!("t{b}");

    for bad in [
        json!({"tab": idb, "text": "x"}),
        json!({"tab": "betaharbor", "text": "x"}),
        json!({"tab": "all", "text": "x"}),
        json!({"tab": [ida, idb], "text": "x"}),
    ] {
        let e = app.tab_conv_scope("send_prompt", &bad).unwrap_err();
        assert!(e.contains("only") && e.contains("alphaship"), "{bad}: {e}");
    }

    let ok = app
        .tab_conv_scope(
            "send_prompt",
            &json!({"text": "run the tests", "group": "g", "account": 2}),
        )
        .unwrap();
    assert_eq!(ok["tab"], json!(ida));
    assert_eq!(ok["mode"], json!("queue"));
    assert!(
        ok.get("group").is_none() && ok.get("account").is_none(),
        "{ok}"
    );
    let ok = app
        .tab_conv_scope(
            "send_prompt",
            &json!({"tab": "alphaship", "text": "x", "mode": "side"}),
        )
        .unwrap();
    assert_eq!(ok["tab"], json!(ida));
    assert_eq!(ok["mode"], json!("side"));
    let ok = app.tab_conv_scope("reload_tab", &json!({})).unwrap();
    assert_eq!(ok["id"], json!(ida));
    assert!(app
        .tab_conv_scope("reload_tab", &json!({"id": idb}))
        .is_err());
    let ok = app
        .tab_conv_scope("git_diff", &json!({"project": "/etc"}))
        .unwrap();
    assert!(
        ok["project"].as_str().unwrap().ends_with("alphaship"),
        "{ok}"
    );

    for tool in [
        "get_state",
        "work_status",
        "open_tab",
        "close_tabs",
        "archive_tabs",
        "move_tab",
        "set_setting",
        "tab_chat",
        "talk_to_tab",
        "read_file",
        "create_job",
        "switch_assistant",
    ] {
        let e = app.tab_conv_scope(tool, &json!({})).unwrap_err();
        assert!(e.contains("not available"), "{tool}: {e}");
    }

    let v = app.control_call("send_prompt", &json!({"tab": idb, "text": "hi"}));
    assert!(v["error"].as_str().unwrap_or("").contains("only"), "{v}");
    let v = app.control_call("get_state", &json!({}));
    assert!(
        v["error"].as_str().unwrap_or("").contains("not available"),
        "{v}"
    );
    assert!(app.deliveries.iter().all(|d| d.uid != b));

    let v = app.control_call("tab_chat", &json!({"tab": idb}));
    assert!(v["error"].is_string(), "{v}");
    assert_eq!(app.tab_conv_uid(), Some(a));

    let v = app.control_call("get_state", &json!({"_client": "web"}));
    assert!(
        v["error"].is_null(),
        "the user's own clients are not scoped: {v}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_conversation_is_kept_per_tab_and_comes_back() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, b) = rig("persist");
    app.assistant.log.push(crate::app_assistant::Entry::new(
        crate::app_assistant::Who::Reply,
        "global answer",
    ));

    app.tab_conv_open(a).unwrap();
    assert!(app.assistant.log.iter().all(|e| e.text != "global answer"));
    assert_eq!(app.tab_conv_json()["name"], json!("alphaship"));
    let conv = app.assistant.conv.clone().unwrap();
    conv.write(
        "user",
        json!({"text": "how long will it take", "via": "typed"}),
    );
    conv.write(
        "reply",
        json!({"text": "About ten minutes, three tables left."}),
    );
    app.control_call("tab_note", &json!({"text": "check the indexes after"}));

    let say = app.tab_conv_close("test");
    assert!(say.contains("Back to the assistant"));
    assert!(app.tab_conv_uid().is_none());
    assert!(app.assistant.log.iter().any(|e| e.text == "global answer"));
    assert!(app
        .assistant
        .log
        .iter()
        .all(|e| e.text != "how long will it take"));
    assert!(app.assistant.conv.is_none());

    app.tab_conv_open(b).unwrap();
    assert!(app
        .assistant
        .log
        .iter()
        .all(|e| !e.text.contains("ten minutes")));
    app.tab_conv_close("test");

    app.tab_conv_open(a).unwrap();
    let texts: Vec<String> = app.assistant.log.iter().map(|e| e.text.clone()).collect();
    assert!(
        texts.iter().any(|t| t == "how long will it take"),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.contains("ten minutes")), "{texts:?}");
    assert!(app
        .tab_conv_context(a)
        .contains("your note 1: check the indexes after"));
    assert!(app
        .assistant
        .carry
        .as_deref()
        .unwrap_or("")
        .contains("ten minutes"));
    let key = app.assistant.bound.as_ref().unwrap().key.clone();
    assert!(key.starts_with("s-7777aaaa"), "{key}");
    assert!(crate::tab_conv::history_path(&key).starts_with(crate::tab_conv::dir()));
    assert!(crate::assistant_history::list()
        .iter()
        .all(|s| !s.text.contains("ten minutes")));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_message_goes_to_the_assistant_with_this_tabs_context_not_into_the_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, _b) = rig("turn");
    let rec = with_brain(&mut app, &home);
    app.tab_conv_open(a).unwrap();
    app.ask_assistant("what are you doing right now");
    assert!(app.assistant.busy);
    let s = recorded(&rec);
    assert!(s.contains("what are you doing right now"), "{s}");
    assert!(
        s.contains("<tab_context>") && s.contains("alphaship") && s.contains("Migration 12"),
        "{s}"
    );
    assert!(!s.contains("<state>") && !s.contains("betaharbor"), "{s}");
    assert!(app.deliveries.is_empty());
    let prompt = std::fs::read_to_string(crate::assistant::dir().join("system-prompt.md")).unwrap();
    assert!(
        prompt.contains("liaison for ONE GodTerm tab") && prompt.contains("alphaship"),
        "{prompt}"
    );
    assert!(!prompt.contains("operator of GodTerm"));
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text == "what are you doing right now"));
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn raw_send_goes_straight_to_the_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, _b) = rig("raw");
    app.tab_conv_open(a).unwrap();
    app.tab_conv_send_raw("run the linter");
    assert!(!app.assistant.busy);
    let texts: Vec<String> = app.assistant.log.iter().map(|e| e.text.clone()).collect();
    assert!(texts.iter().any(|t| t == "run the linter"), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("Sent to the tab as is") || t.starts_with("Not sent to the tab")), "{texts:?}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn global_turns_wait_while_a_tab_conversation_is_open() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, _b) = rig("gate");
    let rec = with_brain(&mut app, &home);
    app.tab_conv_open(a).unwrap();
    app.ask_assistant_system("(events: t1 finished)", "an event for the global assistant");
    assert!(!app.assistant.busy);
    assert!(!rec.exists());
    app.tab_conv_close("test");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn state_changes_of_the_tab_appear_as_lines_and_a_closed_tab_ends_the_conversation() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, _b) = rig("events");
    let rec = with_brain(&mut app, &home);
    app.tab_conv_open(a).unwrap();
    let (s, t) = app.find_tab(a).unwrap();
    app.panes[s].tabs[t].activity = Activity::Working;
    app.tab_conv_tick();
    app.panes[s].tabs[t].activity = Activity::Ready;
    app.tab_conv_tick();
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text == "alphaship finished its turn"));
    assert!(app.assistant.busy && app.assistant.system_turn);
    let sent = recorded(&rec);
    assert!(
        sent.contains("Tab event from GodTerm") && sent.contains("<tab_context>"),
        "{sent}"
    );

    app.assistant.brain = None;
    app.assistant.busy = false;
    app.panes[s].tabs.remove(t);
    app.tab_conv_tick();
    assert!(app.tab_conv_uid().is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_tab_assistant_session_is_kept_and_resumed_by_id() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a, _b) = rig("session");
    app.tab_conv_open(a).unwrap();
    let provider = crate::providers::by_id("claude");
    let acct = app.panes[0].account;
    let cfgdir = app.cfg.accounts[acct.unwrap()].config_dir();
    let p1 = app.session_plan(provider, acct, &cfgdir).unwrap();
    assert!(!p1.resume);
    let proj = cfgdir.join("projects").join("tabconv-test");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join(format!("{}.jsonl", p1.id)), "{}\n").unwrap();
    let p2 = app.session_plan(provider, acct, &cfgdir).unwrap();
    assert!(p2.resume && p2.id == p1.id);
    assert!(
        crate::app_resume::load().is_none(),
        "the main assistant's kept session is untouched"
    );
    app.assistant.fresh_session = true;
    let p3 = app.session_plan(provider, acct, &cfgdir).unwrap();
    assert!(!p3.resume && p3.id != p1.id);
    let _ = std::fs::remove_dir_all(&home);
}
