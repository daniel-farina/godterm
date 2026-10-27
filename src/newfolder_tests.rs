use serde_json::json;

use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-newdir-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    (app, home)
}

fn work(tag: &str) -> std::path::PathBuf {
    let w = std::env::temp_dir().join(format!("godterm-work-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&w);
    std::fs::create_dir_all(&w).unwrap();
    w
}

#[test]
fn a_missing_folder_whose_parent_exists_is_made_and_opened() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("made");
    let parent = work("made");
    let dir = parent.join("ai-brain");
    let v = app.control_call(
        "open_tab",
        &json!({"account": 4, "dir": dir.to_string_lossy()}),
    );
    assert!(dir.is_dir(), "made: {v}");
    let shown = crate::config::tilde(&dir);
    assert_eq!(
        v["say"],
        json!(format!(
            "Created {shown} and opened a session there on Account 4."
        )),
        "{v}"
    );
    let slot = app.pane_for_account(3);
    let t = app.panes[slot].cur();
    assert_eq!(
        std::fs::canonicalize(&t.cwd).unwrap(),
        std::fs::canonicalize(&dir).unwrap()
    );

    let v = app.control_call(
        "open_tab",
        &json!({"account": 4, "dir": dir.to_string_lossy()}),
    );
    assert!(v.get("created").is_none(), "{v}");
    let _ = std::fs::remove_dir_all(&parent);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn more_missing_asks_once_then_makes_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("deep");
    let base = work("deep");
    let dir = base.join("acme/ai-brain");
    let v = app.control_call(
        "open_tab",
        &json!({"account": 2, "dir": dir.to_string_lossy()}),
    );
    let e = v["error"].as_str().unwrap_or("");
    assert!(e.contains("ask the user once") && e.contains("acme"), "{v}");
    assert!(!base.join("acme").exists(), "nothing made before the yes");

    let v = app.control_call(
        "open_tab",
        &json!({"account": 2, "dir": dir.to_string_lossy(), "create": true}),
    );
    assert!(dir.is_dir(), "{v}");
    assert!(v["say"].as_str().unwrap().starts_with("Created "), "{v}");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn never_outside_home_system_or_private_places() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("guard");
    for (d, want) in [
        (
            "/opt/godterm-test-newdir",
            "only made inside the home folder",
        ),
        (
            "/usr/local/godterm-test-newdir",
            "only made inside the home folder",
        ),
    ] {
        let v = app.control_call("open_tab", &json!({"account": 1, "dir": d, "create": true}));
        assert!(v["error"].as_str().unwrap_or("").contains(want), "{d}: {v}");
        assert!(!std::path::Path::new(d).exists());
    }

    let p = home.join("accounts/new-thing");
    let v = app.control_call(
        "open_tab",
        &json!({"account": 1, "dir": p.to_string_lossy(), "create": true}),
    );
    assert!(
        v["error"].as_str().unwrap_or("").starts_with("refused"),
        "{v}"
    );
    assert!(!p.exists());

    let base = work("file");
    std::fs::write(base.join("notes.txt"), "x").unwrap();
    let v = app.control_call(
        "open_tab",
        &json!({"account": 1, "dir": base.join("notes.txt").to_string_lossy()}),
    );
    assert!(
        v["error"].as_str().unwrap_or("").contains("is a file"),
        "{v}"
    );
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&home);
}
