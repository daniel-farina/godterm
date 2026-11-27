use super::*;
use serde_json::json;
use std::time::{Duration, SystemTime};

#[cfg_attr(not(unix), allow(dead_code))]
fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gt-agy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn payload(state: &str, id: &str) -> Value {
    json!({
        "cwd": "/work/proj",
        "session_id": id,
        "conversation_id": id,
        "conversation_title": "Fix the build",
        "model": {"id": "Gemini 4 Argon (Medium)", "display_name": "Gemini 4 Argon (Medium)", "effort": "medium"},
        "workspace": {"current_dir": "/work/proj/sub", "project_dir": "/work/proj"},
        "agent_state": state,
        "plan_tier": "Pro",
        "email": "dev@example.com",
        "quota": {
            "gemini-weekly": {"remaining_fraction": 0.75, "reset_time": "2099-07-06T07:50:32Z", "reset_in_seconds": 560580},
            "gemini-five-hour": {"remaining_fraction": 0.1, "reset_time": "2099-07-01T07:50:32Z"},
            "credits": {"remaining_amount": 12},
            "off": {"remaining_fraction": 0.5, "disabled": true}
        }
    })
}

#[test]
fn the_state_payload_is_read() {
    let s = parse_state(&payload("working", "12345678-abcd-ef01-2345-6789abcdef01"));
    assert_eq!(s.agent_state, "working");
    assert_eq!(s.conversation_id, "12345678-abcd-ef01-2345-6789abcdef01");
    assert_eq!(s.title.as_deref(), Some("Fix the build"));
    assert_eq!(s.email.as_deref(), Some("dev@example.com"));
    assert_eq!(s.plan.as_deref(), Some("Pro"));
    assert_eq!(s.model.as_deref(), Some("Gemini 4 Argon (Medium)"));
    assert_eq!(s.cwd, "/work/proj");
    assert_eq!(activity(&s), Some(Activity::Working));

    let before = parse_state(&json!({"agent_state": "idle", "conversation_id": "", "model": null}));
    assert!(before.conversation_id.is_empty() && before.model.is_none());
    assert_eq!(activity(&before), Some(Activity::Ready));

    let odd = parse_state(&json!({"agent_state": "idle", "conversation_id": "--model x"}));
    assert!(odd.conversation_id.is_empty(), "never an option");

    let mut ask = payload("tool_use", "a1");
    ask["tool_confirmation_pending"] = json!(true);
    assert_eq!(activity(&parse_state(&ask)), Some(Activity::Permission));
    for other in ["authenticating", "initializing", "error", "reviewing", ""] {
        assert_eq!(activity(&parse_state(&json!({"agent_state": other}))), None);
    }
}

#[test]
fn state_and_screen_are_blended() {
    let now = SystemTime::now();
    let started = now - Duration::from_secs(60);
    let st = |name: &str, age: u64| {
        Some((
            now - Duration::from_secs(age),
            parse_state(&json!({"agent_state": name})),
        ))
    };
    assert_eq!(
        blend_with(None, started, now, Activity::Ready),
        Activity::Ready
    );
    assert_eq!(
        blend_with(st("working", 1), started, now, Activity::Ready),
        Activity::Working
    );
    assert_eq!(
        blend_with(st("idle", 1), started, now, Activity::Starting),
        Activity::Ready
    );
    assert_eq!(
        blend_with(st("working", 1), started, now, Activity::Permission),
        Activity::Permission,
        "a prompt on screen wins"
    );
    assert_eq!(
        blend_with(st("working", 30), started, now, Activity::Ready),
        Activity::Ready,
        "an old state loses to the screen"
    );
    assert_eq!(
        blend_with(st("idle", 120), started, now, Activity::Working),
        Activity::Working,
        "a state from before this launch is ignored"
    );
    let mut ask = json!({"agent_state": "tool_use", "tool_confirmation_pending": true});
    assert_eq!(
        blend_with(
            Some((now - Duration::from_secs(40), parse_state(&ask))),
            started,
            now,
            Activity::Working
        ),
        Activity::Permission
    );
    ask["tool_confirmation_pending"] = json!(false);
    assert_eq!(
        blend_with(
            Some((now, parse_state(&ask))),
            started,
            now,
            Activity::Ready
        ),
        Activity::Working
    );
}

#[cfg(unix)]
#[test]
fn the_account_home_links_the_real_one() {
    let root = temp("links");
    let (real, home) = (root.join("real"), root.join("acct"));
    std::fs::create_dir_all(real.join(".ssh")).unwrap();
    std::fs::create_dir_all(real.join(".gemini/antigravity-cli")).unwrap();
    std::fs::create_dir_all(real.join(".Trash")).unwrap();
    std::fs::write(real.join(".gitconfig"), "[user]\n").unwrap();
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    assert_eq!(link_home(&real, &home), 2);
    assert_eq!(
        std::fs::read_link(home.join(".ssh")).unwrap(),
        real.join(".ssh")
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".gitconfig")).unwrap(),
        "[user]\n"
    );
    assert!(!home.join(".Trash").exists());
    assert!(
        std::fs::symlink_metadata(home.join(".gemini"))
            .unwrap()
            .is_dir(),
        "the CLI's own folder is never the real one"
    );
    assert_eq!(link_home(&real, &home), 0, "nothing new the second time");

    std::fs::remove_file(real.join(".gitconfig")).unwrap();
    std::fs::write(real.join("notes.txt"), "x").unwrap();
    assert_eq!(link_home(&real, &home), 1);
    assert!(std::fs::symlink_metadata(home.join(".gitconfig")).is_err());
    assert!(home.join("notes.txt").is_file());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn login_account_usage_and_logout() {
    let home = temp("login");
    prepare_home(&home);
    assert!(isolated(&home) && data_dir(&home).is_dir() && config_dir(&home).is_dir());
    assert!(!logged_in(&home));
    assert!(usage(&Value::Null).is_none());

    std::fs::write(token_path(&home), "{\"opaque\":true}").unwrap();
    assert!(logged_in(&home));
    assert_eq!(account(&home), Account::default());

    let s = parse_state(&payload("idle", "aaaaaaaa-0000-4000-8000-000000000001"));
    assert!(note(&home, &s), "the account record changed");
    assert!(!note(&home, &s), "and is not rewritten");
    let rec = account(&home);
    assert_eq!(rec.email.as_deref(), Some("dev@example.com"));
    assert_eq!(rec.plan.as_deref(), Some("Pro"));
    let known = known_sessions(&home);
    assert_eq!(
        known["aaaaaaaa-0000-4000-8000-000000000001"]["cwd"],
        "/work/proj"
    );

    let u = usage(&rec.quota).unwrap();
    assert_eq!(
        u.windows.len(),
        2,
        "amount only and disabled buckets are left out"
    );
    let wk = u.get("quota:gemini-weekly").unwrap();
    assert_eq!((wk.short.as_str(), wk.left().round()), ("wk", 75.0));
    assert_eq!(u.get("quota:gemini-five-hour").unwrap().short, "5h");
    let b = crate::providers::buckets_of(&u, QUOTA_KEYS);
    assert_eq!(b.len(), 2);
    assert!(b.iter().any(|x| x.binding && x.label == "5h"));

    let mut a = crate::config::Config::default().accounts.remove(0);
    a.harness = "antigravity".into();
    a.config_dir = Some(home.to_string_lossy().into_owned());
    let snap = crate::app::snapshot_of(&a, true);
    assert!(snap.login.logged_in() && snap.login.can_start());
    assert_eq!(snap.profile.email.as_deref(), Some("dev@example.com"));
    assert_eq!(snap.login.subscription.as_deref(), Some("Pro"));
    assert_eq!(snap.usage.unwrap().unwrap().windows.len(), 2);

    assert!(logout(&home));
    assert!(!logged_in(&home) && account(&home) == Account::default());
    let out = crate::app::snapshot_of(&a, true);
    assert!(!out.login.logged_in() && out.login.can_start() && out.usage.is_none());
    assert!(out.profile.email.is_none());
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
#[test]
fn conversations_are_listed_with_title_and_folder() {
    let home = temp("sessions");
    let id = "082373f3-544d-44f1-9ef6-8ef24df95eb8";
    let p = transcript_of(&home, id);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let lines = [
        json!({"step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-26T17:09:06Z", "content": "<USER_REQUEST>\nhello there\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: x.\n</ADDITIONAL_METADATA>"}),
        json!({"step_index": 1, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-26T17:09:07Z", "model": "gemini-3.1-pro-low", "input_tokens": 10, "cache_read_tokens": 4, "output_tokens": 5, "content": "all good"}),
        json!({"step_index": 2, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-26T17:10:06Z", "content": "<USER_REQUEST>\nand now the tests\n</USER_REQUEST>"}),
    ];
    let body: Vec<String> = lines.iter().map(Value::to_string).collect();
    std::fs::write(&p, body.join("\n")).unwrap();
    std::fs::create_dir_all(data_dir(&home).join("brain/empty-one")).unwrap();

    assert_eq!(list_transcripts(&home), vec![p.clone()]);
    assert!(is_transcript(&p) && !is_transcript(&p.with_file_name("transcript_full.jsonl")));

    let s = parse_transcript(&p).unwrap();
    assert_eq!(s.id, id);
    assert_eq!(s.messages, 3);
    assert_eq!(s.first_prompt.as_deref(), Some("hello there"));
    assert_eq!(s.last_prompt.as_deref(), Some("and now the tests"));
    assert_eq!(s.title.as_deref(), Some("hello there"));
    assert_eq!(s.model.as_deref(), Some("gemini-3.1-pro-low"));
    assert_eq!(
        (s.tokens.input, s.tokens.output, s.tokens.cache_read),
        (10, 5, 4)
    );
    assert!(s.cwd.is_empty() && s.created.is_some());

    std::fs::create_dir_all(data_dir(&home).join("cache")).unwrap();
    std::fs::write(
        data_dir(&home).join("cache/last_conversations.json"),
        json!({"/work/other": "x", "/work/last": id}).to_string(),
    )
    .unwrap();
    assert_eq!(parse_transcript(&p).unwrap().cwd, "/work/last");

    std::fs::create_dir_all(data_dir(&home).join("annotations")).unwrap();
    std::fs::write(
        data_dir(&home).join(format!("annotations/{id}.pbtxt")),
        "title:\"Say \\\"hi\\\" back\"",
    )
    .unwrap();
    note(&home, &parse_state(&payload("idle", id)));
    let s = parse_transcript(&p).unwrap();
    assert_eq!(s.title.as_deref(), Some("Say \"hi\" back"));
    assert_eq!(s.cwd, "/work/proj", "what the tab reported wins");
    assert_eq!(user_text("plain"), Some("plain".into()));
    assert_eq!(annotation_title("pinned:true"), None);
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
#[test]
fn the_godterm_server_is_added_without_touching_other_servers() {
    let home = temp("mcp");
    prepare_home(&home);
    let exe = Path::new("/opt/godterm");
    let p = config_dir(&home).join("mcp_config.json");
    assert!(!write_mcp(&home, exe, false), "nothing to remove");
    assert!(!p.exists());

    std::fs::write(
        &p,
        json!({"mcpServers": {"mine": {"command": "x"}}, "other": 1}).to_string(),
    )
    .unwrap();
    assert!(write_mcp(&home, exe, true));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["mine"]["command"], "x");
    assert_eq!(v["other"], 1);
    assert_eq!(v["mcpServers"]["godterm"]["command"], "/opt/godterm");
    assert_eq!(v["mcpServers"]["godterm"]["env"]["GODTERM_CLIENT"], "tab");
    assert!(v["mcpServers"]["godterm"]["env"]
        .get("GODTERM_TOKEN")
        .is_none());

    assert!(!write_mcp(&home, exe, false));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert!(v["mcpServers"].get("godterm").is_none() && v["mcpServers"]["mine"].is_object());

    std::fs::write(
        &p,
        json!({"mcpServers": {"godterm": {"command": "theirs"}}}).to_string(),
    )
    .unwrap();
    assert!(
        !write_mcp(&home, exe, true),
        "a server of the same name is theirs"
    );
    assert!(std::fs::read_to_string(&p).unwrap().contains("theirs"));

    std::fs::write(&p, "not json").unwrap();
    assert!(!write_mcp(&home, exe, true));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "not json");
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
fn fake_agy(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("agy");
    let out = dir.join("seen");
    let script = format!(
        "#!/bin/sh\n{{\n  for a in \"$@\"; do printf 'arg=%s\\n' \"$a\"; done\n  printf 'HOME=%s\\n' \"$HOME\"\n  printf 'TOKENS=%s\\n' \"$AGY_CLI_FORCE_FILE_TOKEN_STORAGE\"\n  printf 'STATE=%s\\n' \"$GODTERM_AGY_STATE\"\n  printf 'TAB=%s\\n' \"$GODTERM_TAB\"\n}} > '{}.tmp'\nmv '{}.tmp' '{}'\nsleep 30\n",
        out.display(),
        out.display(),
        out.display()
    );
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[cfg(unix)]
#[test]
fn a_tab_launches_the_cli_isolated_and_follows_its_conversation() {
    use crate::harness::Harness;
    use crate::pane::LaunchKind;
    let _g = crate::config::testing::LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("agy-launch");
    let bin = fake_agy(&home.join("fakebin"));
    let seen = home.join("fakebin/seen");
    app.cfg.antigravity_bin = Some(bin.to_string_lossy().into_owned());
    app.cfg.accounts[0].harness = "antigravity".into();
    app.cfg.accounts[0].permission_mode = Some("accept-edits".into());
    app.cfg.accounts[0].args = vec![
        "--model".into(),
        "flash".into(),
        "--effort".into(),
        "high".into(),
    ];
    app.accounts[0].login = crate::app::snapshot_of(&app.cfg.accounts[0], false).login;
    let dir = app.cfg.accounts[0].config_dir();
    assert!(!app.accounts[0].login.logged_in() && app.accounts[0].login.can_start());

    let old = "0b41029a-fa18-4e1c-9056-be0d8286ff8e";
    app.launch(0, LaunchKind::Resume(old.into()));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !seen.is_file() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let text = std::fs::read_to_string(&seen).expect("the stand-in agy ran");
    let args: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("arg="))
        .collect();
    let uid = app.panes[0].cur().uid;
    let state = state_path(uid);
    assert_eq!(
        args[..8],
        [
            "--mode",
            "accept-edits",
            "--model",
            "flash",
            "--effort",
            "high",
            "--conversation",
            old
        ]
    );
    assert_eq!(args[8], "--settings");
    let settings: Value = serde_json::from_str(args[9]).unwrap();
    let cmd = settings["title"]["command"].as_str().unwrap().to_string();
    assert_eq!(settings["title"]["type"], "command");
    assert!(
        text.contains(&format!("HOME={}\n", dir.display())),
        "{text}"
    );
    assert!(text.contains("TOKENS=1\n"));
    assert!(text.contains(&format!("STATE={}\n", state.display())));
    assert!(
        text.contains(&format!("TAB={uid}\n")),
        "tab tools know their tab: {text}"
    );
    assert!(data_dir(&dir).is_dir());
    let mcp = std::fs::read_to_string(config_dir(&dir).join("mcp_config.json")).unwrap();
    assert!(mcp.contains("\"godterm\""));
    assert_eq!(app.panes[0].cur().harness, Harness::Antigravity);
    assert_eq!(app.panes[0].cur().session_id.as_deref(), Some(old));

    let run_title = |body: &Value| {
        use std::io::Write;
        let mut c = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .env(STATE_ENV, &state)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin
            .take()
            .unwrap()
            .write_all(body.to_string().as_bytes())
            .unwrap();
        assert!(c.wait().unwrap().success());
    };
    let new = "4fb8d75d-5c44-4c08-ac6b-bace56865cbc";
    run_title(&payload("working", new));
    assert!(state.is_file() && !state.with_extension("json.tmp").exists());
    app.antigravity_tick();
    assert_eq!(
        app.panes[0].cur().session_id.as_deref(),
        Some(new),
        "the id follows /clear and a fresh conversation"
    );
    assert_eq!(
        app.panes[0].cur_mut().update_activity().map(|x| x.1),
        Some(Activity::Working)
    );
    assert_eq!(account(&dir).email.as_deref(), Some("dev@example.com"));
    assert_eq!(known_sessions(&dir)[new]["cwd"], "/work/proj");

    let mut ask = payload("tool_use", new);
    ask["tool_confirmation_pending"] = json!(true);
    run_title(&ask);
    assert_eq!(
        app.panes[0].cur_mut().update_activity().map(|x| x.1),
        Some(Activity::Permission)
    );
    run_title(&payload("idle", new));
    assert_eq!(
        app.panes[0].cur_mut().update_activity().map(|x| x.1),
        Some(Activity::Ready)
    );

    app.panes[0].kill_all();
    std::fs::remove_file(&seen).unwrap();
    app.cfg.accounts[0].args = vec!["--settings".into(), "{}".into()];
    app.launch(0, LaunchKind::Login);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !seen.is_file() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let text = std::fs::read_to_string(&seen).expect("the login tab ran agy");
    let args: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("arg="))
        .collect();
    assert_eq!(
        args,
        ["--mode", "accept-edits", "--settings", "{}"],
        "their own settings are kept"
    );
    assert!(text.contains("STATE=\n") && text.contains("TOKENS=1\n") && text.contains("TAB=\n"));
    assert!(
        !state.exists(),
        "an old state never speaks for a new launch"
    );
    app.shutdown();
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
fn conversation(home: &Path, id: &str, prompt: &str, at: &str, title: Option<&str>) -> PathBuf {
    let p = transcript_of(home, id);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let lines = [
        json!({"step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": "DONE", "created_at": at, "content": format!("<USER_REQUEST>\n{prompt}\n</USER_REQUEST>")}),
        json!({"step_index": 1, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": at, "model": "gemini-3.1-pro-low", "input_tokens": 10, "output_tokens": 5, "content": "done"}),
    ];
    let body: Vec<String> = lines.iter().map(Value::to_string).collect();
    std::fs::write(&p, body.join("\n")).unwrap();
    std::fs::write(p.with_file_name("transcript_full.jsonl"), "full").unwrap();
    let data = data_dir(home);
    std::fs::create_dir_all(data.join("conversations")).unwrap();
    std::fs::write(
        data.join("conversations").join(format!("{id}.db")),
        format!("db of {id}"),
    )
    .unwrap();
    if let Some(t) = title {
        std::fs::create_dir_all(data.join("annotations")).unwrap();
        std::fs::write(
            data.join("annotations").join(format!("{id}.pbtxt")),
            format!("title:\"{t}\""),
        )
        .unwrap();
    }
    p
}

#[cfg(unix)]
fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let t = e.file_type().unwrap();
            if t.is_dir() {
                walk(root, &p, out);
            } else {
                let body = if t.is_file() {
                    std::fs::read(&p).unwrap()
                } else {
                    b"link".to_vec()
                };
                out.push((
                    p.strip_prefix(root).unwrap().to_string_lossy().into_owned(),
                    body,
                ));
            }
        }
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out.sort();
    out
}

#[cfg(unix)]
#[test]
fn a_plain_terminal_conversation_is_indexed_and_copied_exactly() {
    use crate::app_sessions::Src;
    use crate::harness::Harness;
    let native = temp("native");
    let acct = temp("native-acct");
    let id = "5d0c2a9e-1111-4222-8333-444455556666";
    let other = "5d0c2a9e-9999-4222-8333-444455556666";
    let mine = "77777777-1111-4222-8333-444455556666";
    let p = conversation(
        &native,
        id,
        "tidy the acme importer",
        "2026-11-20T10:00:00Z",
        Some("Acme importer tidy up"),
    );
    conversation(
        &native,
        other,
        "something else",
        "2026-11-21T10:00:00Z",
        None,
    );
    conversation(
        &acct,
        mine,
        "started in a tab",
        "2026-11-22T10:00:00Z",
        None,
    );
    let data = data_dir(&native);
    std::fs::write(
        data.join("conversations").join(format!("{id}.db-wal")),
        "wal",
    )
    .unwrap();
    std::fs::write(data.join(TOKEN_FILE), "secret token").unwrap();
    std::fs::write(data.join("brain").join(id).join(TOKEN_FILE), "secret token").unwrap();
    std::os::unix::fs::symlink(
        data.join(TOKEN_FILE),
        data.join("brain").join(id).join("link"),
    )
    .unwrap();
    std::fs::write(data.join("conversation_summaries.db"), "all summaries").unwrap();
    let at = chrono::DateTime::parse_from_rfc3339("2026-11-20T10:00:01Z")
        .unwrap()
        .timestamp_millis();
    let history = [
        json!({"display": "tidy the acme importer", "timestamp": at - 86_400_000, "workspace": "/work/older"}),
        json!({"display": "tidy the acme importer", "timestamp": at, "workspace": "/work/acme-import"}),
        json!({"display": "unrelated", "timestamp": at, "workspace": "/work/unrelated"}),
    ];
    let body: Vec<String> = history.iter().map(Value::to_string).collect();
    std::fs::write(data.join("history.jsonl"), body.join("\n")).unwrap();

    let s = parse_transcript(&p).unwrap();
    assert_eq!(s.title.as_deref(), Some("Acme importer tidy up"));
    assert_eq!(s.first_prompt.as_deref(), Some("tidy the acme importer"));
    assert_eq!(
        s.cwd, "/work/acme-import",
        "the folder comes from the history"
    );
    assert!(has_conversations(&native) && has_conversation(&native, id));

    let ix = crate::session_index::Shared::default();
    let sources = [
        (
            Src::Account(0),
            Harness::Antigravity,
            acct.clone(),
            "Gravity".to_string(),
        ),
        (
            Src::MainAgy,
            Harness::Antigravity,
            native.clone(),
            NATIVE_LABEL.to_string(),
        ),
    ];
    crate::session_index::rebuild(&ix, &sources);
    {
        let g = ix.lock().unwrap();
        assert_eq!(g.rows.len(), 3);
        let row = g.rows.iter().find(|r| r.info.id == id).unwrap();
        assert_eq!((row.src, row.source.as_str()), (Src::MainAgy, NATIVE_LABEL));
        let v = crate::session_index::row_json(row, None, false);
        assert_eq!(v["plain_terminal"], true);
        assert_eq!(v["title"], "Acme importer tidy up");
        assert_eq!(v["harness"], "antigravity");
        let own = g.rows.iter().find(|r| r.info.id == mine).unwrap();
        assert_eq!((own.src, own.source.as_str()), (Src::Account(0), "Gravity"));
        assert!(crate::session_index::row_json(own, None, false)["plain_terminal"].is_null());
    }

    let before = tree(&native);
    let had = tree(&acct);
    let n = copy_conversation(&native, &acct, id).unwrap();
    assert_eq!(tree(&native), before, "the original is untouched");
    let mut new: Vec<String> = tree(&acct)
        .into_iter()
        .filter(|f| !had.contains(f))
        .map(|f| f.0)
        .collect();
    new.sort();
    let mut want = vec![
        format!(".gemini/antigravity-cli/annotations/{id}.pbtxt"),
        format!(".gemini/antigravity-cli/brain/{id}/.system_generated/logs/transcript.jsonl"),
        format!(".gemini/antigravity-cli/brain/{id}/.system_generated/logs/transcript_full.jsonl"),
        format!(".gemini/antigravity-cli/conversations/{id}.db"),
        format!(".gemini/antigravity-cli/conversations/{id}.db-wal"),
        format!(".gemini/config/projects/{DEFAULT_PROJECT}.json"),
    ];
    want.sort();
    assert_eq!(new, want);
    assert_eq!(n, 5);
    assert!(!tree(&acct)
        .iter()
        .any(|(name, body)| name.contains(TOKEN_FILE) || body == b"secret token"));
    assert_eq!(
        std::fs::read(
            data_dir(&acct)
                .join("conversations")
                .join(format!("{id}.db"))
        )
        .unwrap(),
        format!("db of {id}").into_bytes()
    );

    remember(&acct, id, &s.cwd, s.title.as_deref());
    let copied = parse_transcript(&transcript_of(&acct, id)).unwrap();
    assert_eq!(copied.cwd, "/work/acme-import");
    assert_eq!(copied.title.as_deref(), Some("Acme importer tidy up"));

    assert!(copy_conversation(&native, &acct, id)
        .unwrap_err()
        .contains("already has"));
    assert!(copy_conversation(&native, &acct, "../escape").is_err());
    assert!(copy_conversation(&native, &acct, "no-such-conversation")
        .unwrap_err()
        .contains("no saved state"));
    assert!(copy_conversation(&native, &native, other).is_err());
    let vault = PathBuf::from(crate::guard::VAULT).join("agy-home");
    let after = tree(&acct);
    assert!(copy_conversation(&vault, &acct, other)
        .unwrap_err()
        .contains("vault"));
    assert!(copy_conversation(&native, &vault, other)
        .unwrap_err()
        .contains("vault"));
    assert_eq!(tree(&acct), after);
    assert_eq!(tree(&native), before);

    let _g = crate::config::testing::LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *TEST_NATIVE_HOME.lock().unwrap_or_else(|e| e.into_inner()) = Some(acct.clone());
    let refused = copy_conversation(&native, &acct, other);
    *TEST_NATIVE_HOME.lock().unwrap_or_else(|e| e.into_inner()) = None;
    assert!(
        refused.unwrap_err().contains("own home"),
        "nothing is ever written into the plain terminal's home"
    );
    for d in [native, acct] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[cfg(unix)]
#[test]
fn a_plain_terminal_conversation_is_listed_and_resumed_on_an_account() {
    let _g = crate::config::testing::LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("agy-native");
    let bin = fake_agy(&home.join("fakebin"));
    let seen = home.join("fakebin/seen");
    app.cfg.antigravity_bin = Some(bin.to_string_lossy().into_owned());
    app.cfg.accounts[0].harness = "antigravity".into();
    app.accounts[0].login = crate::app::snapshot_of(&app.cfg.accounts[0], false).login;
    let dir = app.cfg.accounts[0].config_dir();
    let native = home.join("plain-home");
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let id = "5d0c2a9e-1111-4222-8333-444455556666";
    let mine = "77777777-1111-4222-8333-444455556666";
    conversation(
        &native,
        id,
        "tidy the acme importer",
        "2026-11-20T10:00:00Z",
        Some("Acme importer tidy up"),
    );
    std::fs::write(data_dir(&native).join(TOKEN_FILE), "secret token").unwrap();
    std::fs::create_dir_all(data_dir(&native).join("cache")).unwrap();
    std::fs::write(
        data_dir(&native).join("cache/last_conversations.json"),
        json!({work.to_string_lossy(): id}).to_string(),
    )
    .unwrap();
    conversation(&dir, mine, "started in a tab", "2026-11-22T10:00:00Z", None);
    *TEST_NATIVE_HOME.lock().unwrap_or_else(|e| e.into_inner()) = Some(native.clone());
    let _m = crate::config::testing::main_dirs(&home.join("no-claude"), None);
    app.kick_index(true);

    let v = app.control_call(
        "sessions",
        &json!({"harness": "antigravity", "limit": 20, "include_headless": true}),
    );
    let rows = v["result"]["sessions"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2, "{v}");
    let plain = rows.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(plain["source"], NATIVE_LABEL);
    assert_eq!(plain["plain_terminal"], true);
    assert_eq!(plain["title"], "Acme importer tidy up");
    assert!(
        plain["project"].as_str().unwrap().ends_with("work"),
        "{plain}"
    );
    let own = rows.iter().find(|r| r["id"] == mine).unwrap();
    assert!(own["plain_terminal"].is_null() && own["title"] == "started in a tab");
    assert_eq!(v["result"]["plain_antigravity"], 1);
    assert!(v["result"]["harnesses"]
        .as_array()
        .unwrap()
        .contains(&json!("antigravity")));
    assert!(
        v["result"]["say"]
            .as_str()
            .unwrap()
            .contains("2 Antigravity sessions"),
        "{v}"
    );

    let v = app.control_call("sessions", &json!({"source": 1, "include_headless": true}));
    assert_eq!(
        v["result"]["matched"], 1,
        "the account lists only its own: {v}"
    );
    let v = app.control_call(
        "sessions",
        &json!({"source": "main", "text": "acme importer", "include_headless": true}),
    );
    assert_eq!(v["result"]["sessions"][0]["id"], id, "{v}");
    let v = app.control_call(
        "list_lost_sessions",
        &json!({"harness": "antigravity", "account": "main", "query": "importer", "include_headless": true}),
    );
    assert_eq!(v["result"]["sessions"][0]["session_id"], id, "{v}");
    assert_eq!(v["result"]["sessions"][0]["account_label"], NATIVE_LABEL);
    assert_eq!(v["result"]["sessions"][0]["plain_terminal"], true);

    let before = tree(&native);
    let v = app.control_call("open_session", &json!({"id": id, "account": 1}));
    let ask = v["needs_confirmation"].as_str().unwrap_or_default();
    assert!(
        ask.contains("into Account 1") && ask.contains("original stays") && v["ok"] == false,
        "{v}"
    );
    assert_eq!(v["args"]["copy"], true);
    assert!(!has_conversation(&dir, id) && !seen.exists());

    let wait = |seen: &Path| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !seen.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::read_to_string(seen).expect("the stand-in agy ran")
    };
    let v = app.control_call(
        "open_session",
        &json!({"id": id, "account": 1, "copy": true}),
    );
    assert_eq!(v["result"]["opened"], id, "{v}");
    assert!(
        v["result"]["say"]
            .as_str()
            .unwrap()
            .contains("original is untouched"),
        "{v}"
    );
    let text = wait(&seen);
    assert!(
        text.contains(&format!("arg=--conversation\narg={id}\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!("HOME={}\n", dir.display())),
        "{text}"
    );
    assert!(has_conversation(&dir, id));
    assert!(!token_path(&dir).exists(), "no credential comes along");
    assert_eq!(tree(&native), before, "the original is untouched");
    let tab = app.panes[app.focus].cur();
    assert_eq!(tab.session_id.as_deref(), Some(id));
    assert_eq!(
        tab.cwd.canonicalize().unwrap(),
        work.canonicalize().unwrap()
    );

    for p in &mut app.panes {
        p.kill_all();
    }
    std::fs::remove_file(&seen).unwrap();
    let v = app.control_call("open_session", &json!({"id": mine}));
    assert_eq!(v["result"]["opened"], mine, "{v}");
    let text = wait(&seen);
    assert!(
        text.contains(&format!("arg=--conversation\narg={mine}\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!("HOME={}\n", dir.display())),
        "{text}"
    );

    *TEST_NATIVE_HOME.lock().unwrap_or_else(|e| e.into_inner()) = None;
    app.shutdown();
    let _ = std::fs::remove_dir_all(home);
}
