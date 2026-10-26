//! The Accounts screen: frames in and out, confirm flows, the login code,
//! and the long-lived token: caught from a tab's output, kept out of
//! every frame and the log.

use super::*;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::web::tap::Out;
use crate::web::{Client, Hub, ToApp};
use std::sync::Arc;

fn app(tag: &str) -> (App, std::path::PathBuf, Arc<Hub>, Arc<Client>) {
    let (mut app, home) = crate::confirm_click_tests::test_app(&format!("acct-{tag}"));
    // Pane output flows only while someone listens for its events.
    let (etx, erx) = std::sync::mpsc::channel();
    app.tx = etx;
    std::mem::forget(erx);
    let (tx, _rx) = std::sync::mpsc::channel();
    let hub = Hub::new(0, "x".repeat(64), vec![], None, tx);
    let c = hub.add();
    c.ready.store(true, std::sync::atomic::Ordering::Relaxed);
    app.web = Some(crate::web::app_web::WebSide::new(Arc::clone(&hub)));
    (app, home, hub, c)
}

fn frames(c: &Arc<Client>) -> Vec<Value> {
    let mut out = vec![];
    while let Some(f) = c.out.pop(Duration::from_millis(20)) {
        if let Out::Text(t) = f {
            out.push(serde_json::from_str(&t).unwrap());
        }
    }
    out
}

fn ask(app: &mut App, c: &Arc<Client>, v: Value) -> Value {
    app.web_event(ToApp::Accounts {
        client: c.id,
        v: v.clone(),
    });
    let fs = frames(c);
    let r = fs
        .into_iter()
        .rfind(|f| f["t"] == "accounts" && f["id"] == v["id"])
        .unwrap_or_else(|| panic!("no answer to {v}"));
    r
}

#[test]
fn view_round_trip_lists_every_account_without_secrets() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _hub, c) = app("view");
    let r = ask(
        &mut app,
        &c,
        json!({"t": "accounts", "op": "watch", "id": 1}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let list = r["view"]["accounts"].as_array().unwrap();
    assert_eq!(list.len(), app.cfg.accounts.len());
    let a = &list[0];
    for k in [
        "label",
        "harness",
        "color",
        "state",
        "tabs",
        "assistant",
        "buckets",
    ] {
        assert!(!a[k].is_null(), "{k} in {a}");
    }
    assert!(a["color"].as_str().unwrap().starts_with('#'));
    assert_eq!(r["view"]["agents"].as_array().unwrap().len(), 6);
    assert!(app.acct.watchers.contains(&c.id));
    // Unknown ops and accounts answer an error, not a panic.
    let r = ask(&mut app, &c, json!({"op": "nope", "id": 2}));
    assert_eq!(r["ok"], false);
    let r = ask(&mut app, &c, json!({"op": "login", "account": 99, "id": 3}));
    assert_eq!(r["error"], "no such account");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn destructive_ops_ask_first_and_a_confirm_id_works_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _hub, c) = app("confirm");
    let label0 = app.cfg.accounts[0].display().to_string();
    // Log out: a question, nothing done yet.
    let r = ask(&mut app, &c, json!({"op": "logout", "account": 1, "id": 1}));
    assert_eq!(r["ok"], true, "{r}");
    let q = &r["confirm"];
    assert!(q["question"].as_str().unwrap().contains(&label0), "{q}");
    let id = q["id"].as_str().unwrap().to_string();
    assert!(
        app.accounts[0].login.logged_in(),
        "not logged out before the yes"
    );
    // A no drops it; the same id cannot be used again.
    let r = ask(
        &mut app,
        &c,
        json!({"op": "confirm", "confirm_id": id, "answer": "no", "id": 2}),
    );
    assert_eq!(r["said"], "Nothing changed");
    let r = ask(
        &mut app,
        &c,
        json!({"op": "confirm", "confirm_id": id, "id": 3}),
    );
    assert_eq!(r["ok"], false);
    // Remove with the folder: removing is one yes, the folder another.
    let dir = app.cfg.accounts[1].config_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let r = ask(
        &mut app,
        &c,
        json!({"op": "remove", "account": 2, "delete_data": true, "id": 4}),
    );
    let q = r["confirm"]["question"].as_str().unwrap();
    assert!(q.contains("asked next"), "{q}");
    let id = r["confirm"]["id"].as_str().unwrap().to_string();
    let r = ask(
        &mut app,
        &c,
        json!({"op": "confirm", "confirm_id": id, "id": 5}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(dir.is_dir(), "the folder stays until its own yes");
    assert!(app.acct.removed.contains(&app.cfg.accounts[1].name));
    let q2 = r["confirm"]["question"].as_str().unwrap();
    assert!(q2.contains("Trash"), "{q2}");
    // A no to the folder keeps it.
    let id2 = r["confirm"]["id"].as_str().unwrap().to_string();
    let r = ask(
        &mut app,
        &c,
        json!({"op": "confirm", "confirm_id": id2, "answer": "no", "id": 6}),
    );
    assert_eq!(r["ok"], true);
    assert!(dir.is_dir());
    // The removed account takes no more actions.
    let r = ask(&mut app, &c, json!({"op": "login", "account": 2, "id": 7}));
    assert_eq!(r["error"], "that account was removed");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn rename_and_color_write_config_by_name() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _hub, c) = app("rename");
    let r = ask(
        &mut app,
        &c,
        json!({"op": "rename", "account": 1, "label": "Work Max", "id": 1}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(app.cfg.accounts[0].display(), "Work Max");
    let r = ask(
        &mut app,
        &c,
        json!({"op": "rename", "account": 1, "label": " ", "id": 2}),
    );
    assert_eq!(r["ok"], false);
    let r = ask(
        &mut app,
        &c,
        json!({"op": "color", "account": 1, "color": "teal", "id": 3}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(app.cfg.accounts[0].color, "teal");
    let r = ask(
        &mut app,
        &c,
        json!({"op": "color", "account": 1, "color": "#ff00ff", "id": 4}),
    );
    assert_eq!(r["ok"], false, "only the calm swatches");
    let _ = std::fs::remove_dir_all(home);
}

/// A login started from the web: the URL is read off its tab, the code
/// typed into that tab, and the code never reaches the log.
#[test]
fn a_login_shows_its_url_and_takes_a_code() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _hub, c) = app("login");
    app.accounts[0].login.source = None;
    let stub = crate::test_stub::claude(
        &home.join("stub"),
        &[
            ("fake_login", "1".into()),
            ("login_code", "GOOD-CODE-77".into()),
        ],
    );
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    let r = ask(&mut app, &c, json!({"op": "login", "account": 1, "id": 1}));
    assert_eq!(r["ok"], true, "{r}");
    let uid = r["tab"].as_u64().unwrap();
    // The stub prints the URL and asks for the code.
    let mut flow = Value::Null;
    for _ in 0..200 {
        app.acct.last_tick = None;
        app.acct_tick();
        flow = app.accounts_view()["accounts"][0]["flow"].clone();
        if flow["wants_code"] == true {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (s0, t0) = app.find_tab(uid).unwrap();
    assert_eq!(flow["stage"], "code", "{flow}\n{}", app.tab_screen(s0, t0));
    assert!(flow["url"]
        .as_str()
        .unwrap()
        .starts_with("https://claude.ai/oauth/authorize?code=true"));
    assert_eq!(flow["tab"], uid);
    // The pushed view reached the watcher too.
    let r = ask(
        &mut app,
        &c,
        json!({"op": "code", "account": 1, "code": "GOOD-CODE-77", "id": 2}),
    );
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..300 {
        app.pending_enters_tick();
        app.acct.last_tick = None;
        app.acct_tick();
        if app.accounts_view()["accounts"][0]["flow"]["stage"] == "done" {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(app.accounts_view()["accounts"][0]["flow"]["stage"], "done");
    let log = std::fs::read_to_string(crate::log::path()).unwrap_or_default();
    assert!(!log.contains("GOOD-CODE-77"), "the code is never logged");
    // Codes with control characters are refused.
    let r = ask(
        &mut app,
        &c,
        json!({"op": "code", "account": 1, "code": "a\nb", "id": 3}),
    );
    assert_eq!(r["ok"], false);
    let _ = std::fs::remove_dir_all(home);
}

/// The token tab: the token is caught, stored, and no frame, screen or log
/// line ever has it.
#[test]
fn a_token_never_reaches_a_frame_a_screen_or_the_log() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, hub, c) = app("token");
    // A test service of its own in the keychain (removed below), so the
    // real `godterm-token` items are never touched.
    let service = format!("godterm-token-test-acct-{}", std::process::id());
    std::env::set_var("GODTERM_TOKEN_SERVICE", &service);
    let tok = crate::acct_tokens::TEST_TOKEN;
    // A stand in for `claude setup-token`: it prints the token wrapped.
    let script = home.join("setup-token.sh");
    let wrapped: String = tok
        .as_bytes()
        .chunks(50)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("\\033[39m\\r\\n\\033[33m");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'Long-lived authentication token created successfully!\\r\\n\\r\\n\\033[33m{wrapped}\\033[39m\\r\\n\\r\\nStore this token securely.\\r\\n'\nsleep 1\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    app.cfg.claude_bin = Some(script.to_string_lossy().into_owned());
    // The browser watches the token's tab too (as if it opened it).
    let r = ask(
        &mut app,
        &c,
        json!({"op": "token_create", "account": 1, "id": 1}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let uid = r["tab"].as_u64().unwrap();
    let (s, t) = app.find_tab(uid).unwrap();
    let tap = Arc::clone(&app.panes[s].tabs[t].web);
    let sub = crate::web::tap::Outbox::new();
    tap.subscribe(c.id, &sub);
    let mut done = Value::Null;
    for _ in 0..300 {
        app.acct.last_tick = None;
        app.acct_tick();
        done = app.accounts_view()["accounts"][0]["flow"].clone();
        if done["stage"] == "done" || done["stage"] == "failed" {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let keychain_ok = done["stage"] == "done";
    if !keychain_ok {
        eprintln!("keychain unavailable here: {done}");
    } else {
        assert!(
            done["note"].as_str().unwrap().contains("sk-ant-…"),
            "{done}"
        );
        app.acct.meta_at = None;
        let v = app.accounts_view();
        assert_eq!(v["accounts"][0]["token"]["last4"], "yzAA", "{v}");
        assert_eq!(
            crate::acct_tokens::store()
                .get(&app.cfg.accounts[0].name)
                .as_deref(),
            Some(tok)
        );
    }
    // The screen has dots, not the token.
    let screen = app.tab_screen(s, t);
    assert!(!screen.contains("FAKEtoken"), "{screen}");
    assert!(screen.contains("sk-ant-•"), "{screen}");
    // Every frame this browser got: views, the tab's output.
    let mut all = String::new();
    for f in frames(&c) {
        all.push_str(&f.to_string());
    }
    while let Some(f) = sub.pop(Duration::from_millis(20)) {
        match f {
            Out::Text(t) => all.push_str(&t),
            Out::Output(_, b) => all.push_str(&String::from_utf8_lossy(&b)),
            _ => {}
        }
    }
    let _ = hub;
    assert!(
        !all.contains("FAKEtoken") && !all.contains(&tok[20..60]),
        "a frame had the token"
    );
    let log = std::fs::read_to_string(crate::log::path()).unwrap_or_default();
    assert!(!log.contains(&tok[20..60]), "the log had the token");
    if keychain_ok {
        // Copy and delete both ask; delete removes the keychain item.
        let r = ask(
            &mut app,
            &c,
            json!({"op": "token_copy", "account": 1, "id": 2}),
        );
        let id = r["confirm"]["id"].as_str().unwrap().to_string();
        let r = ask(
            &mut app,
            &c,
            json!({"op": "confirm", "confirm_id": id, "id": 3}),
        );
        assert_eq!(r["ok"], true, "{r}");
        assert!(
            !r.to_string().contains(&tok[20..60]),
            "the reply has no token"
        );
        assert_eq!(
            crate::acct_tokens::TEST_CLIP.lock().unwrap().as_deref(),
            Some(tok)
        );
        let r = ask(
            &mut app,
            &c,
            json!({"op": "token_delete", "account": 1, "id": 4}),
        );
        let id = r["confirm"]["id"].as_str().unwrap().to_string();
        let r = ask(
            &mut app,
            &c,
            json!({"op": "confirm", "confirm_id": id, "id": 5}),
        );
        assert_eq!(r["ok"], true, "{r}");
        assert!(r["said"].as_str().unwrap().contains("revoke"), "{r}");
        assert!(crate::acct_tokens::store()
            .get(&app.cfg.accounts[0].name)
            .is_none());
    }
    let _ = crate::acct_tokens::store().delete(&app.cfg.accounts[0].name);
    std::env::remove_var("GODTERM_TOKEN_SERVICE");
    let _ = std::fs::remove_dir_all(home);
}

/// The assistant has no tool that reads or moves a token.
#[test]
fn no_control_tool_touches_tokens() {
    for t in crate::control::TOOLS {
        let n = t.name.to_lowercase();
        assert!(!n.contains("token") || n.contains("count"), "{}", t.name);
    }
}
