use serde_json::{json, Value};
#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;

use crate::config::testing::LOCK as ENV_LOCK;
use crate::tab_browser_tests::{setup, uid_of};

fn call(app: &mut crate::app::App, tool: &str, client: &str, args: Value) -> Value {
    let mut a = args;
    a["_client"] = json!(client);
    app.control_call(tool, &a)
}

#[test]
fn agents_may_not_press_the_open_buttons() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("cd-refuse");
    let tab = crate::control::tab_id(uid_of(&app, t));
    for tool in ["open_in_claude_desktop", "open_desktop"] {
        for client in ["brain".to_string(), format!("tab:{}", uid_of(&app, t))] {
            let v = call(&mut app, tool, &client, json!({"tab": tab}));
            assert_eq!(v["ok"], false, "{tool} {client}: {v}");
        }
    }
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(target_os = "macos")]
#[test]
fn a_claude_tab_resumes_its_session_in_claude_desktop_from_its_folder() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !crate::app_claude_desktop::installed() {
        return;
    }
    let (mut app, home, t) = setup("cd-run");
    let log = home.join("desktop.log");
    let stub = home.join("claude-stub.sh");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho \"$@\" > {0}\npwd >> {0}\necho \"$CLAUDE_CONFIG_DIR\" >> {0}\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    let tab = crate::control::tab_id(uid_of(&app, t));
    let v = call(
        &mut app,
        "open_in_claude_desktop",
        "web",
        json!({"tab": tab}),
    );
    assert_eq!(v["resumed"], true, "{v}");
    let got = std::fs::read_to_string(&log).unwrap();
    assert!(got.starts_with("--desktop --resume sess-w\n"), "{got}");
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(target_os = "macos")]
#[test]
fn a_failing_claude_desktop_is_reported_not_hidden() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !crate::app_claude_desktop::installed() {
        return;
    }
    let (mut app, home, t) = setup("cd-fail");
    let stub = home.join("claude-fail.sh");
    std::fs::write(&stub, "#!/bin/sh\necho 'no such session' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    let tab = crate::control::tab_id(uid_of(&app, t));
    let v = call(
        &mut app,
        "open_in_claude_desktop",
        "web",
        json!({"tab": tab}),
    );
    assert_eq!(v["ok"], false, "{v}");
    assert!(v.to_string().contains("no such session"), "{v}");
    let _ = std::fs::remove_dir_all(home);
}
