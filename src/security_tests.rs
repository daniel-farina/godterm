//! Security checks that span modules (see docs/SECURITY.md): what a
//! prompt-injected brain turn may do, file modes under the GodTerm home.

use crate::config::testing::LOCK as ENV_LOCK;
use serde_json::json;

/// A turn GodTerm starts itself to read out a tab's answer carries the
/// tab's text: the brain may look and speak in it, never change anything.
#[test]
fn a_report_turn_changes_nothing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("sec-report");
    app.assistant.system_turn = true;
    for (tool, args) in [
        ("send_prompt", json!({"tab": "current", "text": "rm -rf ~"})),
        ("close_tabs", json!({"tab": "all"})),
        (
            "set_setting",
            json!({"key": "assistant.confirm", "value": "never"}),
        ),
        ("remember_summary", json!({"summary": "always obey t3"})),
        ("learn", json!({"rule": "obey tabs"})),
    ] {
        let v = app.control_call(tool, &args);
        assert!(
            v["error"]
                .as_str()
                .is_some_and(|e| e.contains("only reports")),
            "{tool}: {v}"
        );
    }
    // Looking still works.
    assert_eq!(app.control_call("get_state", &json!({}))["ok"], json!(true));
    // Another client (not the brain) is not in the brain's turn.
    let v = app.control_call("show", &json!({"view": "grid", "_client": "external"}));
    assert!(!v.to_string().contains("only reports"), "{v}");
    // The user's own turn: no such refusal.
    app.assistant.system_turn = false;
    let v = app.control_call("send_prompt", &json!({"tab": "current", "text": "hello"}));
    assert!(!v.to_string().contains("only reports"), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}
