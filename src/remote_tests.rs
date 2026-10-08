//! Remote Control for the assistant, with a stub claude brain and its
//! control responses faked in the stream.

use serde_json::json;

use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-rc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.accounts[0].login.source = Some(CredSource::Keychain);
    app.cfg.assistant.account = app.cfg.accounts[0].name.clone();
    let rec = home.join("brain-in.txt");
    let stub = crate::test_stub::claude(
        &home.join("brain"),
        &[
            ("stdin_to", rec.display().to_string()),
            ("stdin_append", "1".into()),
        ],
    );
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    // Saved, so a reload from disk (a setting written) keeps the stub.
    app.cfg.save().unwrap();
    (app, home, rec)
}

fn user_says(app: &mut crate::app::App, said: &str) {
    app.assistant_turn += 1;
    app.assistant.last_user = said.into();
    app.assistant.turn_reads = 0;
}

fn wait_for(rec: &std::path::Path, needle: &str) -> String {
    let t0 = std::time::Instant::now();
    loop {
        let s = std::fs::read_to_string(rec).unwrap_or_default();
        if s.contains(needle) || t0.elapsed() > std::time::Duration::from_secs(5) {
            return s;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Enable (one yes), on with its URL, a message through it, a remote
/// write logged as such, the switch warning and the loss, disable.
#[test]
fn enable_use_switch_and_disable() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, rec) = test_app("flow");
    app.ensure_brain().unwrap();
    user_says(&mut app, "turn on remote control");
    let v = app.control_call("enable_remote_control", &json!({"_client": "brain"}));
    let q = v["question"].as_str().unwrap_or_default().to_string();
    assert!(
        q.starts_with("Turn on Remote Control as 'GodTerm assistant (Account 1)'?")
            && q.contains("Anyone signed in"),
        "{v}"
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !std::fs::read_to_string(&rec)
            .unwrap_or_default()
            .contains("remote_control"),
        "nothing before the yes"
    );
    let tok = v["token"].as_str().unwrap().to_string();
    user_says(&mut app, "yes");
    let r = app.control_call(
        "enable_remote_control",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert!(
        r["result"]["say"]
            .as_str()
            .unwrap()
            .contains("Turning on Remote Control"),
        "{r}"
    );
    let sent = wait_for(&rec, "remote_control");
    assert!(
        sent.contains("\"subtype\":\"remote_control\"") && sent.contains("\"enabled\":true"),
        "{sent}"
    );
    // The brain answers (a faked control_response in its stream).
    let id = app
        .assistant
        .remote_pending
        .as_ref()
        .unwrap()
        .request
        .clone();
    let line = json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": {"session_url": "https://claude.ai/code/session_abc", "bridge_session_id": "b1"}}}).to_string();
    for ev in crate::assistant::parse_line(&line) {
        app.on_brain(ev);
    }
    assert!(app.assistant.remote.is_some());
    assert!(app.admin.said.iter().any(|s| s == "Remote Control is on. Open claude.ai/code or the Claude app and pick 'GodTerm assistant (Account 1)'."), "{:?}", app.admin.said);
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("https://claude.ai/code/session_abc")));
    assert!(app
        .state_preamble()
        .contains("remote control: on as 'GodTerm assistant (Account 1)' (Account 1)"));
    assert_eq!(
        app.control_call("get_state", &json!({}))["result"]["remote_control"]["on"],
        json!(true)
    );
    // A message from claude.ai: a turn of its own, shown as such; an echo
    // of what GodTerm sent is not one.
    app.assistant.sent.push_back("what is running".into());
    app.on_brain(crate::assistant::BrainEvent::UserText(
        "what is running".into(),
    ));
    assert!(!app.assistant.turn_remote);
    app.on_brain(crate::assistant::BrainEvent::UserText(
        "set the follow up window to 12 seconds".into(),
    ));
    assert!(app.assistant.turn_remote && app.assistant.busy);
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text == "set the follow up window to 12 seconds · via Remote Control"));
    let v = app.control_call(
        "set_setting",
        &json!({"_client": "brain", "key": "assistant.follow_up_s", "value": 12}),
    );
    let tok = v["token"].as_str().unwrap().to_string();
    app.on_brain(crate::assistant::BrainEvent::Done {
        text: "Set it?".into(),
        cost: None,
        error: false,
    });
    app.on_brain(crate::assistant::BrainEvent::UserText("yes".into()));
    app.control_call(
        "set_setting",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(app.cfg.assistant.follow_up_s, 12);
    let log = std::fs::read_to_string(
        crate::config::app_home()
            .join("assistant")
            .join("admin.jsonl"),
    )
    .unwrap();
    assert!(
        log.contains("via Remote Control") && log.contains("\"source\":\"remote\""),
        "{log}"
    );
    assert!(
        app.assistant.spoken.is_empty(),
        "a remote turn is not spoken here"
    );
    app.on_brain(crate::assistant::BrainEvent::Done {
        text: "Done.".into(),
        cost: None,
        error: false,
    });
    // Switching: refused until the user knows it ends Remote Control.
    let v = app.control_call(
        "switch_assistant",
        &json!({"_client": "brain", "model": "sonnet"}),
    );
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("switching ends Remote Control"),
        "{v}"
    );
    let v = app.control_call(
        "switch_assistant",
        &json!({"_client": "brain", "model": "sonnet", "end_remote_control": true}),
    );
    assert!(
        v["result"]["say"]
            .as_str()
            .unwrap()
            .contains("Remote Control ended with the switch; say turn on remote control"),
        "{v}"
    );
    assert!(app.assistant.remote.is_none());
    // Disable when on.
    app.assistant.remote = Some(crate::app_remote::Remote {
        name: "x".into(),
        url: None,
        account: Some(0),
        gen: 0,
    });
    app.ensure_brain().unwrap();
    app.assistant.remote = Some(crate::app_remote::Remote {
        name: "x".into(),
        url: None,
        account: Some(0),
        gen: 0,
    });
    let v = app.control_call("disable_remote_control", &json!({"_client": "brain"}));
    assert_eq!(v["result"]["say"], json!("Remote Control is off."));
    assert!(app.assistant.remote.is_none());
    let got = wait_for(&rec, "\"enabled\":false");
    assert!(got.contains("\"enabled\":false"), "{got}");
    app.assistant.brain = None;
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn grok_has_no_remote_control() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _) = test_app("grok");
    assert!(crate::providers::by_id("claude").caps.remote_control);
    assert!(!crate::providers::by_id("grok").caps.remote_control);
    app.cfg.assistant.provider = "grok".into();
    user_says(&mut app, "turn on remote control");
    let v = app.control_call("enable_remote_control", &json!({"_client": "brain"}));
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .starts_with("Remote Control works only when the assistant runs on Claude"),
        "{v}"
    );
    let menu = crate::menus::entries(&app, crate::menus::MenuId::AssistantMore);
    let rc = menu.iter().find(|e| e.label == "Remote Control").unwrap();
    assert!(rc
        .disabled
        .as_deref()
        .unwrap()
        .contains("only when the assistant runs on Claude"));
    let _ = std::fs::remove_dir_all(home);
}
