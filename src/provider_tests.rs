//! The assistant's providers with stub backends: the registry, a fake
//! headless grok (streaming-messages-json), tools through use_tool, the
//! built in tools removed, a switch that keeps the conversation, and a
//! refused login falling back to Claude.

use serde_json::json;

use crate::assistant::BrainEvent;
use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-prov-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.accounts[0].login.source = Some(CredSource::Keychain);
    (app, home)
}

/// A turn of fake grok output: deltas, a use_tool call, the answer.
const GROK_TURN: &str = r#"{"type":"system","subtype":"init","session_id":"s1","model":"grok-4.7-build-fast","tools":["run_terminal_command","search_tool","use_tool"],"mcp_servers":[{"name":"godterm","status":"connected"}]}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use"}}}
{"type":"assistant","message":{"id":"m0","content":[{"type":"tool_use","id":"c1","name":"use_tool","input":{"tool_name":"godterm__get_state","tool_input":{}}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"{\"ok\":true}"}]}}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Two tabs are working."}}}
{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Two tabs are working."}]}}
{"type":"result","subtype":"success","is_error":false,"result":"Two tabs are working.","total_cost_usd":0.004}
"#;

/// The events a grok brain sends for one turn (until its Done).
fn grok_turn(
    app: &mut crate::app::App,
    rx: &std::sync::mpsc::Receiver<crate::app::AppEvent>,
    text: &str,
) -> Vec<BrainEvent> {
    app.ask_assistant(text);
    let mut evs = vec![];
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(10) {
        if let Ok(crate::app::AppEvent::Assistant(_, ev)) =
            rx.recv_timeout(std::time::Duration::from_millis(200))
        {
            let done = matches!(ev, BrainEvent::Done { .. });
            evs.push(ev.clone());
            app.on_brain(ev);
            if done {
                break;
            }
        }
    }
    evs
}

#[test]
fn registry_lists_providers() {
    let ids: Vec<&str> = crate::providers::PROVIDERS.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec!["claude", "grok"]);
    let g = crate::providers::by_id("grok");
    assert!(g.harness.is_none() && g.own_home.is_some() && !g.caps.persistent);
    assert_eq!(
        g.models[0].0, "grok-4.7-build-fast",
        "the quickest by default"
    );
    assert_eq!(crate::providers::by_id("nope").id, "claude");
}

#[test]
fn grok_brain_turns_tools_and_switching() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("grok");
    let (tx, rx) = std::sync::mpsc::channel();
    app.tx = tx;
    // A fake grok that prints a canned turn and records its argv.
    let canned = home.join("turn.jsonl");
    std::fs::write(&canned, GROK_TURN).unwrap();
    let args = home.join("grok-args.txt");
    let stub = crate::test_stub::claude(
        &home.join("grokbin"),
        &[
            ("print_file", canned.display().to_string()),
            ("args_to", args.display().to_string()),
        ],
    );
    app.cfg.grok_bin = Some(stub.to_string_lossy().into_owned());
    // A first exchange on Claude, then the switch.
    app.assistant.log.push(crate::app_assistant::Entry {
        who: crate::app_assistant::Who::User,
        text: "remember the botmesh deploy".into(),
    });
    app.assistant.log.push(crate::app_assistant::Entry {
        who: crate::app_assistant::Who::Reply,
        text: "Noted, botmesh deploys at five.".into(),
    });
    let v = app.control_call("switch_assistant", &json!({"provider": "grok"}));
    let say = v["result"]["say"].as_str().unwrap().to_string();
    assert!(
        say.starts_with("Switched to Grok (its own Grok login, grok-4.7-build-fast)")
            && say.contains("login is missing"),
        "{say}"
    );
    assert_eq!(app.cfg.assistant.provider, "grok");
    // Not signed in: a clear error, no process.
    app.ask_assistant("what is running");
    assert!(
        app.assistant
            .log
            .iter()
            .any(|e| e.text.contains("assistant's Grok login is missing")),
        "{:?}",
        app.assistant.log
    );
    // Its own home gets a (fake) login; the user's Grok accounts are not used.
    let gh = crate::providers::grok::home();
    assert!(gh.starts_with(&home));
    std::fs::create_dir_all(&gh).unwrap();
    std::fs::write(gh.join("auth.json"), r#"{"fake": "test login"}"#).unwrap();
    app.assistant.carry = app.conversation_carry();
    let evs = grok_turn(&mut app, &rx, "what is running");
    // use_tool is the godterm tool; the answer arrives.
    assert!(
        evs.contains(&BrainEvent::Tool("get_state".into(), json!({}))),
        "{evs:?}"
    );
    assert!(
        matches!(evs.last(), Some(BrainEvent::Done { error: false, .. })),
        "{evs:?}"
    );
    assert_eq!(
        app.assistant
            .log
            .iter()
            .rev()
            .find(|e| e.who == crate::app_assistant::Who::Reply)
            .unwrap()
            .text,
        "Two tabs are working."
    );
    let argv = std::fs::read_to_string(&args).unwrap();
    // The conversation came along; built in tools are gone, the shell denied.
    assert!(
        argv.contains("remember the botmesh deploy") && argv.contains("botmesh deploys at five"),
        "{argv}"
    );
    assert!(
        argv.contains("--disallowed-tools run_terminal_command,read_file")
            && argv.contains("--deny Bash(*)")
            && argv.contains("--allow MCPTool(godterm__*)"),
        "{argv}"
    );
    assert!(argv.contains("--session-id") && argv.contains("-m grok-4.7-build-fast"));
    assert!(
        argv.contains("godterm__get_state"),
        "tool keys in the prompt"
    );
    // Its config: only godterm's MCP server, imports of other tools off.
    let cfg = std::fs::read_to_string(gh.join("config.toml")).unwrap();
    assert!(
        cfg.contains("[mcp_servers.godterm]")
            && cfg.contains("GODTERM_CLIENT = \"brain\"")
            && cfg.contains("mcps = false"),
        "{cfg}"
    );
    // The next turn resumes the same session.
    std::fs::remove_file(&args).unwrap();
    grok_turn(&mut app, &rx, "and now");
    let argv2 = std::fs::read_to_string(&args).unwrap();
    assert!(argv2.contains("--resume"), "{argv2}");
    // A shell call is never run by GodTerm: it is only a tool event, and
    // grok was told to deny it.
    let shell = crate::assistant::parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"run_terminal_command","input":{"command":"rm -rf /"}}]}}"#,
    );
    assert_eq!(
        shell,
        vec![BrainEvent::Tool(
            "run_terminal_command".into(),
            json!({"command": "rm -rf /"})
        )]
    );
    // Use Opus: back on Claude with that model, said once.
    let v = app.control_call(
        "switch_assistant",
        &json!({"provider": "claude", "model": "opus"}),
    );
    assert!(
        v["result"]["say"]
            .as_str()
            .unwrap()
            .starts_with("Switched to Claude"),
        "{v}"
    );
    assert_eq!(app.cfg.assistant.model, "opus");
    let st = app.control_call("get_state", &json!({}));
    assert_eq!(st["result"]["providers"][1]["provider"], "grok");
    assert!(app.state_preamble().contains("you run on: Claude"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_refused_grok_login_falls_back_to_claude() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("refused");
    let (tx, rx) = std::sync::mpsc::channel();
    app.tx = tx;
    let empty = home.join("empty.jsonl");
    std::fs::write(&empty, "").unwrap();
    let stub = crate::test_stub::claude(
        &home.join("grokbin"),
        &[
            ("print_file", empty.display().to_string()),
            (
                "stderr_line",
                "Error: API error (status 401 Unauthorized): session revoked".into(),
            ),
            ("exit_code", "1".into()),
        ],
    );
    app.cfg.grok_bin = Some(stub.to_string_lossy().into_owned());
    app.switch_assistant(Some("grok"), None, None).unwrap();
    let gh = crate::providers::grok::home();
    std::fs::create_dir_all(&gh).unwrap();
    std::fs::write(gh.join("auth.json"), r#"{"fake": "test login"}"#).unwrap();
    let evs = grok_turn(&mut app, &rx, "hello");
    assert!(
        matches!(evs.last(), Some(BrainEvent::Done { error: true, .. })),
        "{evs:?}"
    );
    assert_eq!(app.cfg.assistant.provider, "claude", "back on Claude");
    assert!(
        app.assistant
            .log
            .iter()
            .any(|e| e.text.contains("login was refused") && e.text.contains("401")),
        "{:?}",
        app.assistant.log
    );
    let _ = std::fs::remove_dir_all(home);
}
