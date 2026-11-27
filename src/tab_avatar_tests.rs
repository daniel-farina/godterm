use serde_json::json;

use crate::config::testing::LOCK as ENV_LOCK;
use crate::tab_avatar as av;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";
const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 8 8\"><rect width=\"8\" height=\"8\" fill=\"#7a9\"/></svg>";

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-avatar-{tag}-{}", std::process::id()));
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
    for n in ["alpha", "beta"] {
        let d = home.join(n);
        std::fs::create_dir_all(&d).unwrap();
        app.panes[1].add_tab(d);
    }
    (app, home)
}

fn id(app: &crate::app::App, name: &str) -> String {
    let t = app.panes[1].tabs.iter().find(|t| t.name() == name).unwrap();
    crate::control::tab_id(t.uid)
}

fn avatar_of(app: &crate::app::App, name: &str) -> Option<String> {
    app.panes[1]
        .tabs
        .iter()
        .find(|t| t.name() == name)
        .unwrap()
        .avatar
        .clone()
}

#[test]
fn names_are_checked() {
    assert!(av::valid("gen:12"));
    assert!(!av::valid("gen:"));
    assert!(!av::valid("gen:1x"));
    assert!(av::valid("0123456789abcdef.png"));
    assert!(!av::valid("../0123456789abcdef.png"));
    assert!(!av::valid("0123456789abcdef.exe"));
    assert!(!av::valid("0123456789ABCDEF.png"));
    assert!(!av::valid("short.png"));
}

#[test]
fn store_checks_type_and_size() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    crate::config::testing::set_home(
        &std::env::temp_dir().join(format!("godterm-avatar-store-{}", std::process::id())),
    );
    let n = av::store(PNG).unwrap();
    assert!(av::valid_file(&n) && n.ends_with(".png"));
    assert_eq!(av::store(PNG).unwrap(), n);
    assert_eq!(av::read_stored(&n).unwrap(), PNG);
    assert!(av::store(b"").is_err());
    assert!(av::store(b"MZ not an image").is_err());
    let mut big = PNG.to_vec();
    big.resize(av::MAX_BYTES + 1, 0);
    assert!(av::store(&big).unwrap_err().to_string().contains("512 KB"));
    assert!(av::store(SVG.as_bytes()).unwrap().ends_with(".svg"));
    for bad in [
        "<svg><script>alert(1)</script></svg>",
        "<svg onload=\"x()\"></svg>",
        "<svg><foreignObject/></svg>",
        "<svg><image href=\"https://x.test/a.png\"/></svg>",
        "<html></html>",
    ] {
        assert!(av::store(bad.as_bytes()).is_err(), "{bad}");
    }
    assert!(av::read_stored("../../etc/passwd").is_none());
}

#[test]
fn files_must_be_inside_the_folder() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, home) = test_app("inside");
    let proj = home.join("alpha");
    std::fs::write(proj.join("logo.png"), PNG).unwrap();
    std::fs::write(home.join("outside.png"), PNG).unwrap();
    let roots = vec![proj.clone()];
    assert!(av::from_file(&proj.join("logo.png"), &roots).is_ok());
    assert!(av::from_file(&home.join("outside.png"), &roots).is_err());
    assert!(av::from_file(&proj.join("../outside.png"), &roots).is_err());
    assert!(av::from_file(&proj.join("missing.png"), &roots).is_err());
    std::fs::write(proj.join("notes.png"), b"plain text").unwrap();
    assert!(av::from_file(&proj.join("notes.png"), &roots).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(home.join("outside.png"), proj.join("link.png")).unwrap();
        assert!(av::from_file(&proj.join("link.png"), &roots).is_err());
    }
}

#[test]
fn the_logo_is_found_by_name_and_place() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_app, home) = test_app("find");
    let proj = home.join("beta");
    assert!(av::find_logo(&proj).is_none());
    std::fs::create_dir_all(proj.join("public")).unwrap();
    std::fs::write(proj.join("public/favicon.png"), PNG).unwrap();
    assert!(av::find_logo(&proj)
        .unwrap()
        .ends_with("public/favicon.png"));
    std::fs::create_dir_all(proj.join("docs")).unwrap();
    std::fs::write(proj.join("docs/Logo-dark.svg"), SVG).unwrap();
    assert!(av::find_logo(&proj)
        .unwrap()
        .ends_with("docs/Logo-dark.svg"));
    std::fs::write(proj.join("logo.png"), PNG).unwrap();
    assert!(av::find_logo(&proj).unwrap().ends_with("beta/logo.png"));
    std::fs::write(proj.join("logotype-notes.png"), PNG).unwrap();
    assert!(av::find_logo(&proj).unwrap().ends_with("beta/logo.png"));
}

#[test]
fn the_tool_sets_regenerates_and_clears() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("tool");
    let tab = id(&app, "alpha");
    let v = app.control_call("set_tab_avatar", &json!({"tab": tab}));
    assert!(v["error"].as_str().unwrap().contains("from_project"), "{v}");
    let v = app.control_call("set_tab_avatar", &json!({"tab": tab, "from_project": true}));
    assert!(v["error"].as_str().unwrap().contains("no logo"), "{v}");

    std::fs::write(home.join("alpha/logo.png"), PNG).unwrap();
    let v = app.control_call("set_tab_avatar", &json!({"tab": tab, "from_project": true}));
    assert!(v["error"].is_null(), "{v}");
    let file = avatar_of(&app, "alpha").unwrap();
    assert!(av::valid_file(&file));
    assert_eq!(v["result"]["avatar"], json!(file));
    assert_eq!(avatar_of(&app, "beta"), None);

    let v = app.control_call(
        "set_tab_avatar",
        &json!({"tab": tab, "path": "../outside.png"}),
    );
    assert!(v["error"].is_string(), "{v}");
    let v = app.control_call("set_tab_avatar", &json!({"tab": tab, "path": "/etc/hosts"}));
    assert!(v["error"].is_string(), "{v}");
    assert_eq!(avatar_of(&app, "alpha"), Some(file));

    let v = app.control_call("set_tab_avatar", &json!({"tab": tab, "data": "AAAA"}));
    assert!(v["error"].as_str().unwrap().contains("web app"), "{v}");

    let v = app.control_call("set_tab_avatar", &json!({"tab": tab, "regenerate": true}));
    assert!(
        av::is_generated(v["result"]["avatar"].as_str().unwrap()),
        "{v}"
    );
    let v = app.control_call("set_tab_avatar", &json!({"tab": tab, "clear": true}));
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(avatar_of(&app, "alpha"), None);

    let state = app.control_call("get_state", &json!({}));
    assert!(!state.to_string().contains("\"avatar\""), "{state}");
    app.control_call("set_tab_avatar", &json!({"tab": tab, "from_project": true}));
    let state = app.control_call("get_state", &json!({}));
    assert!(state.to_string().contains("\"avatar\""), "{state}");
}

#[test]
fn an_agent_can_only_change_its_own_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("scope");
    std::fs::write(home.join("alpha/icon.png"), PNG).unwrap();
    std::fs::write(home.join("beta/icon.png"), PNG).unwrap();
    let uid = |n: &str| {
        app.panes[1]
            .tabs
            .iter()
            .find(|t| t.name() == n)
            .unwrap()
            .uid
    };
    let (ua, ub) = (uid("alpha"), uid("beta"));
    let v = app.tab_call(
        ua,
        "set_tab_avatar",
        &json!({"tab": crate::control::tab_id(ub), "from_project": true}),
    );
    assert!(v["error"].as_str().unwrap().contains("own tab"), "{v}");
    assert_eq!(avatar_of(&app, "beta"), None);
    let v = app.tab_call(ua, "set_tab_avatar", &json!({"from_project": true}));
    assert!(v["error"].is_null(), "{v}");
    assert!(avatar_of(&app, "alpha").is_some());
    assert_eq!(avatar_of(&app, "beta"), None);
    let v = app.tab_call(ua, "set_tab_avatar", &json!({"data": "AAAA"}));
    assert!(v["error"].as_str().unwrap().contains("takes no"), "{v}");
    let v = app.tab_call(ua, "set_tab_avatar", &json!({"clear": true}));
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(avatar_of(&app, "alpha"), None);
    assert!(crate::tab_jobs::tab_tool_list()
        .iter()
        .any(|t| t["name"] == "set_tab_avatar"));
}

#[test]
fn the_avatar_travels_with_a_closed_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("closed");
    std::fs::write(home.join("alpha/logo.png"), PNG).unwrap();
    app.control_call(
        "set_tab_avatar",
        &json!({"tab": id(&app, "alpha"), "from_project": true}),
    );
    let t = app.panes[1]
        .tabs
        .iter()
        .position(|t| t.name() == "alpha")
        .unwrap();
    app.panes[1].tabs[t].session_id = Some("s1".into());
    let c = app.snapshot_tab(1, t, 0).unwrap();
    assert_eq!(c.avatar, avatar_of(&app, "alpha"));
    let text = serde_json::to_string(&c).unwrap();
    let back: crate::closed::ClosedTab = serde_json::from_str(&text).unwrap();
    assert_eq!(back.avatar, c.avatar);
    let old: crate::closed::ClosedTab =
        serde_json::from_str(&text.replace("\"avatar\"", "\"x\"")).unwrap();
    assert_eq!(old.avatar, None);
}
