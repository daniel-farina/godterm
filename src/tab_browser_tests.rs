use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};

use crate::app::{App, Modal};
use crate::browser::{self, Browser, ChromeProfile, Daemon, Env, ProfileExt, Snapshot};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, test_app};
use crate::hits::UiAction;
use crate::jobs_tests::running;
use crate::tab_browser::{
    self as tb, button_label, button_state, chrome_cmd, ext_state, match_browsers, plan_for,
    valid_profile_dir, BtnState, ExtState, OpenPlan, Os, Step,
};

const ID: &str = browser::EXTENSION_ID;

pub(crate) fn pe(dir: &str, name: &str, installed: bool, enabled: Option<bool>) -> ProfileExt {
    ProfileExt {
        profile: ChromeProfile {
            dir: dir.into(),
            name: name.into(),
            user_name: format!("{}@x.com", name.to_lowercase()),
            gaia_name: String::new(),
        },
        installed,
        enabled,
    }
}

#[test]
fn the_extension_state_of_a_profile() {
    assert_eq!(
        ext_state(&pe("Default", "Me", true, Some(true))),
        ExtState::Enabled
    );
    assert_eq!(
        ext_state(&pe("Default", "Me", true, Some(false))),
        ExtState::Disabled
    );
    assert_eq!(
        ext_state(&pe("Default", "Me", false, Some(false))),
        ExtState::Disabled
    );

    assert_eq!(
        ext_state(&pe("Default", "Me", true, None)),
        ExtState::Disabled
    );
    assert_eq!(
        ext_state(&pe("Default", "Me", false, None)),
        ExtState::Missing
    );

    assert_eq!(
        ext_state(&pe("Default", "Me", false, Some(true))),
        ExtState::Enabled
    );
}

#[test]
fn the_button_state_follows_the_tabs_profile_and_its_extension() {
    let ps = vec![
        pe("Default", "Me", true, Some(true)),
        pe("Profile 1", "Work", true, Some(false)),
        pe("Profile 2", "Bare", false, None),
    ];
    assert_eq!(button_state(None, &ps), BtnState::None);
    assert_eq!(button_state(Some("Default"), &ps), BtnState::Ready);
    assert_eq!(button_state(Some("Profile 1"), &ps), BtnState::NeedsEnable);
    assert_eq!(button_state(Some("Profile 2"), &ps), BtnState::Chosen);

    assert_eq!(button_state(Some("Profile 9"), &ps), BtnState::Chosen);
    assert_eq!(button_state(Some("Default"), &[]), BtnState::Chosen);
    assert_eq!(BtnState::Ready.word(), "ready");
    assert_eq!(BtnState::NeedsEnable.word(), "needs_enable");
    assert_eq!(BtnState::Chosen.word(), "needs_install");
    assert_eq!(BtnState::None.word(), "none");
}

#[test]
fn the_button_text_shows_the_state_in_plain_words() {
    assert_eq!(button_label(BtnState::None, ""), " browser ");
    assert_eq!(
        button_label(BtnState::Chosen, "Work"),
        " browser: Work (add) "
    );
    assert_eq!(
        button_label(BtnState::NeedsEnable, "Work"),
        " browser: Work (turn on) "
    );
    assert_eq!(button_label(BtnState::Ready, "Work"), " ✓ browser: Work ");
    assert!(!button_label(BtnState::Ready, "A very long profile name indeed").contains("indeed"));
    for s in [
        BtnState::None,
        BtnState::Chosen,
        BtnState::NeedsEnable,
        BtnState::Ready,
    ] {
        assert!(!button_label(s, "x").contains('—'), "no em dashes");

        let v = tb::button_labels(s, "Work");
        assert_eq!(v[0], button_label(s, "Work"));
        assert!(v
            .windows(2)
            .all(|w| w[0].chars().count() > w[1].chars().count()));
        assert!(v.iter().all(|l| l.contains("browser")));
    }
    assert_eq!(
        tb::button_labels(BtnState::Ready, "Work").last().unwrap(),
        " ✓ browser "
    );
}

#[test]
fn what_to_open_depends_on_the_extension() {
    assert_eq!(plan_for(ExtState::Missing), Some(OpenPlan::Store));
    assert_eq!(plan_for(ExtState::Disabled), Some(OpenPlan::Extensions));
    assert_eq!(plan_for(ExtState::Enabled), None);
    assert_eq!(OpenPlan::Store.url(), tb::STORE_URL);
    assert!(tb::STORE_URL.ends_with(ID));
    assert_eq!(OpenPlan::Extensions.url(), "chrome://extensions");
}

pub(crate) fn bsk(id: &str, label: &str) -> Browser {
    Browser {
        instance_id: id.into(),
        label: label.into(),
        browser_name: "chrome".into(),
        ..Default::default()
    }
}

#[test]
fn a_profiles_browser_is_found_by_the_label_or_as_the_only_one() {
    const L: tb::Links = tb::Links::new();
    let work = pe("Profile 1", "Work", true, Some(true)).profile;
    let all = vec![bsk("aaa", "Me"), bsk("bbb", " work ")];
    let (m, how) = match_browsers(&all, &work, 2, true, &L);
    assert_eq!(
        (m.len(), m[0].instance_id.as_str(), how),
        (1, "bbb", "label")
    );

    let all = vec![bsk("ccc", "profile 1"), bsk("ddd", "work@x.com")];
    let (m, _) = match_browsers(&all, &work, 2, true, &L);
    assert_eq!(m.len(), 2);

    let one = vec![bsk("eee", "")];
    assert_eq!(match_browsers(&one, &work, 1, true, &L).1, "only browser");
    assert!(match_browsers(&one, &work, 2, true, &L).0.is_empty());
    assert!(match_browsers(&one, &work, 1, false, &L).0.is_empty());

    let (m, how) = match_browsers(&[bsk("fff", "Me"), bsk("ggg", "Other")], &work, 2, true, &L);
    assert!(m.is_empty());
    assert_eq!(how, "unknown");
}

#[test]
fn the_chrome_command_per_platform() {
    let u = "https://example.com/a?b=1";
    let c = chrome_cmd(Os::Mac, u, Some("Profile 1")).unwrap();
    assert_eq!(c.prog, "open");
    assert_eq!(
        c.args,
        [
            "-na",
            "Google Chrome",
            "--args",
            "--profile-directory=Profile 1",
            u
        ]
    );

    let c = chrome_cmd(Os::Mac, tb::EXTENSIONS_URL, Some("Default")).unwrap();
    assert_eq!(
        c.args,
        [
            "-na",
            "Google Chrome",
            "--args",
            "--profile-directory=Default",
            "chrome://extensions"
        ]
    );
    let c = chrome_cmd(Os::Mac, u, None).unwrap();
    assert_eq!(c.args, ["-na", "Google Chrome", "--args", u]);
    let c = chrome_cmd(Os::Linux, u, Some("Profile 2")).unwrap();
    assert_eq!(
        (c.prog.as_str(), c.args.as_slice()),
        (
            "google-chrome",
            &["--profile-directory=Profile 2".to_string(), u.to_string()][..]
        )
    );
    let c = chrome_cmd(Os::Windows, u.split('?').next().unwrap(), Some("Default")).unwrap();
    assert_eq!(c.prog, "cmd");
    assert_eq!(c.args[..4], ["/C", "start", "", "chrome"]);
    assert_eq!(c.args[4], "--profile-directory=Default");

    assert!(chrome_cmd(Os::Windows, "https://x.com/?a=1&b=2", Some("Default")).is_none());

    for bad in [
        "",
        "../x",
        "a/b",
        "a\\b",
        "..",
        "x\"y",
        "--user-data-dir=/x",
    ] {
        assert!(!valid_profile_dir(bad), "{bad}");
        assert!(chrome_cmd(Os::Mac, u, Some(bad)).is_none(), "{bad}");
    }
    assert!(chrome_cmd(Os::Mac, "", Some("Default")).is_none());
    assert!(valid_profile_dir("Default") && valid_profile_dir("Profile 12"));
}

fn chrome(home: &Path) -> PathBuf {
    let root = home.join("chrome");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("Local State"),
        r#"{"profile":{"info_cache":{
            "Default":{"name":"Me","user_name":"me@x.com"},
            "Profile 1":{"name":"Work","user_name":"work@x.com"},
            "Profile 2":{"name":"Bare"}}}}"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("Default/Extensions").join(ID).join("0.3.3_0")).unwrap();
    set_pref(&root, "Default", Some(1));
    fs::create_dir_all(root.join("Profile 1/Extensions").join(ID)).unwrap();
    set_pref(&root, "Profile 1", Some(0));
    fs::create_dir_all(root.join("Profile 2")).unwrap();
    root
}

pub(crate) fn set_pref(root: &Path, dir: &str, state: Option<u64>) {
    let v = match state {
        Some(s) => json!({"extensions": {"settings": {ID: {"state": s}}}}),
        None => json!({}),
    };
    fs::create_dir_all(root.join(dir)).unwrap();
    fs::write(root.join(dir).join("Secure Preferences"), v.to_string()).unwrap();
}

fn env(root: &Path) -> Env {
    Env {
        bin_dirs: vec![],
        chrome_root: root.to_path_buf(),
        timeout: Duration::from_secs(2),
        skill_sources: vec![],
    }
}

pub(crate) fn setup(tag: &str) -> (App, PathBuf, usize) {
    let (mut app, home) = test_app(&format!("tabbrowser-{tag}"));
    let root = chrome(&home);
    app.browser.env = Some(env(&root));
    let t = running(&mut app, 0, "web", "sess-w");
    app.panes[0].active = t;
    app.cfg.tab_model_bar = true;
    refresh(&mut app);
    tb::LAUNCHED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    (app, home, t)
}

pub(crate) fn refresh(app: &mut App) {
    settle(app);
    let e = app.browser.env.clone().unwrap();
    app.browser.set_quick(tb::quick(&e, false));
}

pub(crate) fn settle(app: &App) {
    let t0 = std::time::Instant::now();
    while (app.browser.quick_busy.load(Ordering::SeqCst) || app.browser_busy())
        && t0.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn uid_of(app: &App, t: usize) -> u64 {
    app.panes[0].tabs[t].uid
}

pub(crate) fn launched() -> Vec<tb::Cmd> {
    tb::LAUNCHED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub(crate) fn key(app: &mut App, c: KeyCode) {
    app.on_browser_key(KeyEvent::new(c, KeyModifiers::NONE));
}

#[test]
fn the_binding_is_saved_with_the_tab_and_restored() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("persist");
    let other = running(&mut app, 0, "api", "sess-a");
    let uid = uid_of(&app, t);
    assert!(app.bind_tab_browser(uid, Some("Profile 1".into())));
    assert!(
        !app.bind_tab_browser(uid, Some("Profile 1".into())),
        "unchanged"
    );
    let st = app.snapshot_state();
    let saved: Vec<_> = st.slots.iter().flat_map(|s| &s.tabs).collect();
    assert_eq!(
        saved
            .iter()
            .filter(|x| x.browser.as_deref() == Some("Profile 1"))
            .count(),
        1
    );

    let text = serde_json::to_string(&st).unwrap();
    assert_eq!(text.matches("\"browser\"").count(), 1, "{text}");

    let (tx, _rx) = std::sync::mpsc::channel();
    let mut next = App::new(crate::config::Config::default(), tx);
    next.restore(st);
    let (s, i) = next.find_tab(uid).expect("same tab id");
    assert_eq!(next.panes[s].tabs[i].browser.as_deref(), Some("Profile 1"));
    assert_eq!(next.panes[s].tabs[i].name(), "web");
    let o = next.find_tab(uid_of(&app, other)).unwrap();
    assert_eq!(next.panes[o.0].tabs[o.1].browser, None);

    let old: crate::state::TabState = serde_json::from_str(r#"{"cwd":"/x"}"#).unwrap();
    assert_eq!(old.browser, None);

    assert!(!app.bind_tab_browser(uid_of(&app, other), Some("../../etc".into())));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_moved_tab_keeps_its_binding_and_the_others_do_not_get_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("move");
    let plain = running(&mut app, 0, "plain", "sess-p");
    app.panes[0].tabs[t].session_id = None;
    let uid = uid_of(&app, t);
    app.bind_tab_browser(uid, Some("Profile 1".into()));
    app.move_tab_now(0, t, 1, false, true);
    let new = *app.moved_ids.get(&uid).expect("a new tab id");
    assert_ne!(new, uid);
    let (s, i) = app.find_tab(new).unwrap();
    assert_eq!(app.panes[s].tabs[i].browser.as_deref(), Some("Profile 1"));

    assert_eq!(app.panes[0].tabs[plain].browser, None);
    let _ = fs::remove_dir_all(home);
}

pub(crate) fn snapshot(app: &App, browsers: Vec<Browser>) -> Snapshot {
    Snapshot {
        at: std::time::SystemTime::now(),
        bsk_path: None,
        version: Some("0.3.3".into()),
        daemon: Daemon::Stopped,
        browsers,
        doctor: None,
        harnesses: vec![],
        profiles: app.browser_profiles(),
        accounts: vec![],
    }
}

pub(crate) fn tab_call(app: &mut App, uid: u64, tool: &str) -> Value {
    app.control_call(tool, &json!({"_client": format!("tab:{uid}")}))
}

#[test]
fn a_tab_sees_only_its_own_profile_and_never_a_neighbours() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a) = setup("scope");
    let b = running(&mut app, 0, "api", "sess-b");
    let c = running(&mut app, 0, "docs", "sess-c");
    let (ua, ub, uc) = (uid_of(&app, a), uid_of(&app, b), uid_of(&app, c));
    app.bind_tab_browser(ua, Some("Profile 1".into()));
    app.bind_tab_browser(ub, Some("Default".into()));
    let s = snapshot(&app, vec![bsk("inst-me", "Me"), bsk("inst-work", "Work")]);
    app.browser.set(s);

    let v = tab_call(&mut app, ua, "list_browsers");
    assert_eq!(v["ok"], true, "{v}");
    let text = v.to_string();
    assert_eq!(v["result"]["bound_profile"]["profile_dir"], "Profile 1");
    assert_eq!(v["result"]["bound_profile"]["state"], "needs_enable");
    assert_eq!(v["result"]["count"], 1, "{v}");
    assert_eq!(v["result"]["browsers"][0]["instance_id"], "inst-work");
    assert!(
        !text.contains("inst-me") && !text.contains("me@x.com") && !text.contains("Default"),
        "{text}"
    );

    let v = tab_call(&mut app, ub, "list_browsers");
    assert_eq!(v["result"]["bound_profile"]["state"], "ready");
    assert_eq!(v["result"]["browsers"][0]["instance_id"], "inst-me");
    assert!(v["result"]["say"]
        .as_str()
        .unwrap()
        .contains("--browser inst-me"));
    assert!(!v.to_string().contains("inst-work"));

    let v = tab_call(&mut app, ub, "browser_status");
    let profs = v["result"]["chrome_profiles"].as_array().unwrap();
    assert_eq!(profs.len(), 1, "{v}");
    assert_eq!(profs[0]["profile_dir"], "Default");
    assert!(!v.to_string().contains("work@x.com"));

    let v = tab_call(&mut app, uc, "list_browsers");
    assert_eq!(v["ok"], true, "{v}");
    assert!(v["result"]["bound_profile"].is_null());
    assert!(v["result"]["say"].as_str().unwrap().contains("Browser"));

    let v = app.control_call(
        "list_browsers",
        &json!({"_client": format!("tab:{ua}"), "tab": tab_id_of(ub)}),
    );
    assert_eq!(v["ok"], false, "{v}");
    let _ = fs::remove_dir_all(home);
}

fn tab_id_of(uid: u64) -> String {
    crate::control::tab_id(uid)
}

#[test]
fn an_unclear_match_asks_the_user_instead_of_guessing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a) = setup("unclear");
    let ua = uid_of(&app, a);
    app.bind_tab_browser(ua, Some("Default".into()));

    let s = snapshot(&app, vec![bsk("x1", "one"), bsk("x2", "two")]);
    app.browser.set(s);
    let v = tab_call(&mut app, ua, "list_browsers");
    assert_eq!(v["result"]["count"], 0, "{v}");
    assert!(!v.to_string().contains("x1"));
    assert!(v["result"]["say"]
        .as_str()
        .unwrap()
        .contains("Browser name"));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn the_assistant_sees_the_binding_in_list_tabs_of_that_tab_only() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a) = setup("listtabs");
    let b = running(&mut app, 0, "api", "sess-b");
    app.bind_tab_browser(uid_of(&app, a), Some("Default".into()));
    let v = app.control_call("get_state", &json!({"_client": "brain"}));
    let rows = v["result"]["tabs"].as_array().cloned().unwrap_or_default();
    let find = |uid: u64| rows.iter().find(|r| r["id"] == tab_id_of(uid)).cloned();
    let ra = find(uid_of(&app, a)).unwrap_or_else(|| panic!("{v}"));
    assert_eq!(ra["browser"]["profile_dir"], "Default", "{ra}");
    assert_eq!(ra["browser"]["state"], "ready");
    assert!(find(uid_of(&app, b)).unwrap().get("browser").is_none());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn the_hint_tells_a_tab_to_use_only_its_browser() {
    assert!(crate::tab_jobs::HINT.contains("bound_profile"));
    assert!(crate::tab_jobs::HINT.contains("only that browser"));
    assert!(!crate::tab_jobs::HINT.contains('—'));
    let d = crate::tab_jobs::tab_description("list_browsers");
    assert!(d.contains("THIS tab"));
}

#[test]
fn links_open_in_the_default_profile_and_a_tab_uses_its_own() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("links");
    let url = "https://example.com/page";
    let open = |app: &mut App, extra: Value| {
        let mut a = json!({"_client": "brain", "paths": [url]});
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        app.control_call("open_path", &a)
    };

    let v = open(&mut app, json!({}));
    assert_eq!(v["ok"], true, "{v}");
    assert!(launched().is_empty());

    app.set_default_profile("Profile 1");
    open(&mut app, json!({}));
    assert_eq!(
        launched(),
        vec![chrome_cmd(Os::current(), url, Some("Profile 1")).unwrap()]
    );
    assert!(launched()[0]
        .args
        .iter()
        .any(|a| a == "--profile-directory=Profile 1"));

    let tid = tab_id_of(uid_of(&app, t));
    app.bind_tab_browser(uid_of(&app, t), Some("Default".into()));
    tb::LAUNCHED.lock().unwrap().clear();
    open(&mut app, json!({"tab": tid}));
    assert_eq!(
        launched(),
        vec![chrome_cmd(Os::current(), url, Some("Default")).unwrap()]
    );

    tb::LAUNCHED.lock().unwrap().clear();
    open(&mut app, json!({}));
    assert_eq!(
        launched(),
        vec![chrome_cmd(Os::current(), url, Some("Profile 1")).unwrap()]
    );

    app.set_default_profile("Profile 9");
    tb::LAUNCHED.lock().unwrap().clear();
    open(&mut app, json!({}));
    assert!(launched().is_empty());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn the_default_profile_is_written_to_config_toml() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _) = setup("config");
    app.set_default_profile("Profile 1");
    let text = fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(
        text.contains("[browser]") && text.contains("default_profile = \"Profile 1\""),
        "{text}"
    );
    let cfg = crate::config::Config::parse(&text).unwrap();
    assert_eq!(cfg.browser.default_profile, "Profile 1");

    app.browser_default_cycle();
    assert_eq!(app.cfg.browser.default_profile, "Profile 2");
    app.browser_default_cycle();
    assert_eq!(app.cfg.browser.default_profile, "");
    app.browser_default_cycle();
    assert_eq!(app.cfg.browser.default_profile, "Default");
    let text = fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(text.contains("default_profile = \"Default\""));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_profile_with_the_extension_on_is_ready_at_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("ready");
    app.open_browser_dialog(0);
    settle(&app);
    refresh(&mut app);
    assert!(
        matches!(&app.modal, Modal::Browser(d) if d.uid == uid_of(&app, t) && d.step == Step::Pick)
    );
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panes[0].tabs[t].browser.as_deref(), Some("Default"));
    assert_eq!(
        app.cfg.browser.default_profile, "Default",
        "the latest choice is the default"
    );
    assert!(
        matches!(&app.modal, Modal::Browser(d) if d.step == Step::Pick),
        "nothing to confirm"
    );
    assert!(launched().is_empty(), "nothing opened");
    assert_eq!(app.tab_browser_state(uid_of(&app, t)), BtnState::Ready);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_missing_extension_opens_the_store_in_that_profile_after_a_yes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("store");
    app.open_browser_dialog(0);
    settle(&app);
    refresh(&mut app);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panes[0].tabs[t].browser.as_deref(), Some("Profile 2"));
    assert_eq!(app.cfg.browser.default_profile, "Profile 2");
    let Modal::Browser(d) = app.modal.clone() else {
        panic!()
    };
    assert_eq!(d.step, Step::Confirm(OpenPlan::Store));
    assert!(launched().is_empty(), "Chrome is opened only after a yes");
    let view = app.browser_dialog_view(&d).unwrap();
    let said = view
        .lines
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(said.contains("press Add to Chrome yourself"), "{said}");
    assert!(!said.contains('—'));
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        launched(),
        vec![chrome_cmd(Os::current(), tb::STORE_URL, Some("Profile 2")).unwrap()]
    );
    assert_eq!(app.browser.pending.len(), 1);
    assert!(app
        .browser
        .note
        .as_deref()
        .unwrap()
        .contains("Press Add to Chrome"));
    assert_eq!(app.tab_browser_state(uid_of(&app, t)), BtnState::Chosen);

    tb::LAUNCHED.lock().unwrap().clear();
    app.open_browser_dialog(0);
    settle(&app);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Enter);
    let Modal::Browser(d) = app.modal.clone() else {
        panic!()
    };
    assert_eq!(d.step, Step::Confirm(OpenPlan::Extensions));
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.modal, Modal::None));
    assert!(launched().is_empty());
    assert_eq!(
        app.panes[0].tabs[t].browser.as_deref(),
        Some("Profile 1"),
        "the profile stays chosen"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_disabled_extension_opens_the_extensions_page_and_becomes_ready_when_turned_on() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("enable");
    let uid = uid_of(&app, t);
    app.open_browser_dialog(0);
    settle(&app);
    refresh(&mut app);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('y'));
    assert_eq!(
        launched(),
        vec![chrome_cmd(Os::current(), "chrome://extensions", Some("Profile 1")).unwrap()]
    );
    assert_eq!(app.tab_browser_state(uid), BtnState::NeedsEnable);

    refresh(&mut app);
    app.browser_poll_tick();
    settle(&app);
    assert_eq!(app.browser.pending.len(), 1);

    let root = app.browser.env.clone().unwrap().chrome_root;
    set_pref(&root, "Profile 1", Some(1));
    refresh(&mut app);
    app.browser_poll_tick();
    settle(&app);
    assert!(app.browser.pending.is_empty(), "no longer waiting");
    assert_eq!(app.tab_browser_state(uid), BtnState::Ready);
    assert!(
        app.browser.note.as_deref().unwrap().starts_with("Ready"),
        "{:?}",
        app.browser.note
    );

    app.browser_clear();
    assert_eq!(app.panes[0].tabs[t].browser, None);
    assert_eq!(app.tab_browser_state(uid), BtnState::None);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn nothing_is_looked_at_unless_the_dialog_or_a_wait_is_open() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _) = setup("quiet");
    app.browser.quick_at = None;
    app.browser_poll_tick();
    assert!(app.browser.quick_at.is_none(), "idle: no look");
    app.open_browser_dialog(0);
    settle(&app);
    assert!(app.browser.quick_at.is_some());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_bound_tab_gets_a_look_at_start_so_its_button_is_true() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("startlook");
    app.browser.quick_at = None;
    app.browser.quick.lock().unwrap().take();
    app.bind_tab_browser(uid_of(&app, t), Some("Default".into()));
    app.browser_poll_tick();
    settle(&app);
    assert!(app.browser.quick_at.is_some());
    assert_eq!(app.tab_browser_state(uid_of(&app, t)), BtnState::Ready);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_queued_model_switch_keeps_the_room_it_needs() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _) = setup("room");

    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 30)).unwrap();
    draw(&mut app, &mut term);
    let s = screen(&term);
    assert!(s.contains("Haiku") && s.contains("Opus"), "{s}");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn ctrl_a_i_and_the_palette_open_the_dialog() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("keys");
    app.on_command(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    assert!(matches!(&app.modal, Modal::Browser(d) if d.uid == uid_of(&app, t)));
    settle(&app);
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.modal, Modal::None));
    let (_, a) = crate::palette::ACTIONS
        .iter()
        .find(|(n, _)| n.starts_with("browser:"))
        .expect("a palette entry");
    assert_eq!(*a, crate::palette::Action::Key('i'));
    assert!(crate::ui::HELP.iter().any(|(k, _)| *k == "Ctrl-a i"));
    let _ = fs::remove_dir_all(home);
}

fn screen(t: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let b = t.backend().buffer();
    (0..b.area.height)
        .map(|y| {
            (0..b.area.width)
                .map(|x| b[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_button_sits_on_the_bottom_bar_shows_its_state_and_a_click_opens_the_dialog() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("button");
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(300, 50)).unwrap();
    draw(&mut app, &mut term);
    let s = screen(&term);
    assert!(s.contains(" browser "), "{s}");
    let r = app
        .last_hits
        .regions
        .iter()
        .find(|r| r.action == UiAction::TabBrowser(0))
        .unwrap_or_else(|| panic!("no Browser button"))
        .rect;

    let loops = app
        .last_hits
        .regions
        .iter()
        .find(|r| r.action == UiAction::LoopsDrawer(0))
        .unwrap()
        .rect;
    assert_eq!(r.y, loops.y);
    assert!(r.x + r.width <= loops.x, "{r:?} {loops:?}");

    app.bind_tab_browser(uid_of(&app, t), Some("Profile 1".into()));
    draw(&mut app, &mut term);
    assert!(screen(&term).contains("browser: Work (turn on)"));
    app.bind_tab_browser(uid_of(&app, t), Some("Default".into()));
    draw(&mut app, &mut term);
    assert!(screen(&term).contains("✓ browser: Me"));
    app.bind_tab_browser(uid_of(&app, t), Some("Profile 2".into()));
    draw(&mut app, &mut term);
    assert!(screen(&term).contains("browser: Bare (add)"));

    click(&mut app, &UiAction::TabBrowser(0));
    assert!(matches!(&app.modal, Modal::Browser(d) if d.uid == uid_of(&app, t)));
    settle(&app);
    refresh(&mut app);
    draw(&mut app, &mut term);
    let s = screen(&term);
    assert!(s.contains("Chrome profile for this tab"), "{s}");
    assert!(
        s.contains("installed, disabled") && s.contains("not installed"),
        "{s}"
    );
    assert!(s.contains("me@x.com") || s.contains("Me"), "{s}");

    click(&mut app, &UiAction::BrowserRow(0));
    assert_eq!(app.panes[0].tabs[t].browser.as_deref(), Some("Default"));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_narrow_pane_keeps_a_compact_browser_button_before_hiding_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("narrow");
    app.bind_tab_browser(uid_of(&app, t), Some("Default".into()));

    let mut seen: Vec<(u16, Option<String>)> = vec![];
    for w in 16..140u16 {
        app.hits.borrow_mut().clear();
        let area = ratatui::layout::Rect::new(0, 0, w, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        crate::ui_modelbar::draw(&mut buf, &app, 0, area);
        let r = app
            .hits
            .borrow()
            .regions
            .iter()
            .find(|r| r.action == UiAction::TabBrowser(0))
            .map(|r| r.rect);
        let label = r.map(|r| {
            (r.x..r.x + r.width)
                .map(|x| buf[(x, 0)].symbol().to_string())
                .collect::<String>()
        });
        seen.push((w, label));
    }
    let compact: Vec<u16> = seen
        .iter()
        .filter(|(_, l)| l.as_deref() == Some(tb::compact_label(BtnState::Ready)))
        .map(|(w, _)| *w)
        .collect();
    assert!(
        !compact.is_empty(),
        "the compact form is used somewhere: {seen:?}"
    );

    let first = seen.iter().position(|(_, l)| l.is_some()).unwrap();
    assert!(seen[first..].iter().all(|(_, l)| l.is_some()), "{seen:?}");

    let tiny = seen
        .iter()
        .find(|(_, l)| l.as_deref() == Some(" ✓ browser "))
        .map(|(w, _)| *w)
        .unwrap();
    assert!(compact[0] < tiny, "{compact:?} {tiny}");

    assert_eq!(tb::compact_label(BtnState::None), " web ");
    assert!(tb::compact_label(BtnState::Chosen).contains('!'));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn sign_in_and_remote_links_open_in_the_default_profile_unless_turned_off() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("loginlinks");
    let url = "https://claude.ai/oauth/authorize?x=1";
    let opened = || crate::admin::OPENED.lock().unwrap().clone();
    crate::admin::OPENED.lock().unwrap().clear();

    assert!(app.cfg.browser.login_links);
    app.open_service_link(url);
    assert!(launched().is_empty());
    assert_eq!(opened(), vec![url.to_string()]);

    app.set_default_profile("Profile 1");
    app.bind_tab_browser(uid_of(&app, t), Some("Default".into()));
    crate::admin::OPENED.lock().unwrap().clear();
    app.open_service_link(url);
    assert_eq!(
        launched(),
        vec![chrome_cmd(Os::current(), url, Some("Profile 1")).unwrap()]
    );
    assert!(opened().is_empty());

    tb::LAUNCHED.lock().unwrap().clear();
    app.open_service_link("file:///etc/passwd");
    assert!(launched().is_empty());

    app.toggle_login_links();
    assert!(!app.cfg.browser.login_links);
    let toml = fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(toml.contains("login_links = false"), "{toml}");
    tb::LAUNCHED.lock().unwrap().clear();
    crate::admin::OPENED.lock().unwrap().clear();
    app.open_service_link(url);
    assert!(launched().is_empty());
    assert_eq!(opened(), vec![url.to_string()]);

    app.toggle_login_links();
    app.assistant.remote_last_url = Some("https://claude.ai/code/session_x".into());
    tb::LAUNCHED.lock().unwrap().clear();
    app.open_remote_link();
    assert_eq!(launched().len(), 1, "{:?}", launched());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_bsk_call_reads_as_what_it_did() {
    use tb::bsk_summary as s;
    assert_eq!(
        s("bsk navigate https://www.example.com/login/").as_deref(),
        Some("navigate example.com/login")
    );
    assert_eq!(s("bsk --json click @e12").as_deref(), Some("click @e12"));
    assert_eq!(
        s("cd /w && BSK_X=1 ~/.local/bin/bsk tab create 'https://a.io'").as_deref(),
        Some("tab create a.io")
    );
    assert_eq!(s("bsk screenshot --full").as_deref(), Some("screenshot"));

    assert_eq!(
        s("bsk navigate https://a.io; bsk snapshot | head").as_deref(),
        Some("snapshot")
    );
    assert_eq!(s("echo bsk navigate x"), None);
    assert_eq!(s("ls -la"), None);
    assert_eq!(s("bskx navigate https://a.io"), None);
}

#[test]
fn the_last_browser_action_comes_from_the_transcript() {
    let line = |ts: &str, cmd: &str| {
        json!({"type": "assistant", "timestamp": ts, "message": {"content": [
            {"type": "text", "text": "Looking"},
            {"type": "tool_use", "name": "Bash", "input": {"command": cmd}}
        ]}})
        .to_string()
    };
    let tail = [
        line("2026-10-23T10:00:00Z", "bsk navigate https://a.io"),
        json!({"type": "user", "message": {"content": "bsk is great"}}).to_string(),
        line("2026-10-23T10:05:00Z", "bsk click @e3"),
        line("2026-10-23T10:06:00Z", "cargo test"),
    ]
    .join("\n");
    let a = tb::last_bsk_action(&tail).unwrap();
    assert_eq!(a.what, "click @e3");
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-23T10:08:30Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(tb::activity_words(&a, now), "click @e3 · 3 min ago");
    assert!(tb::last_bsk_action("not json\n{}").is_none());
}

#[test]
fn the_dialog_shows_the_tabs_last_browser_action_and_opens_its_chrome() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("activity");
    let uid = uid_of(&app, t);
    app.bind_tab_browser(uid, Some("Default".into()));

    let a = app.panes[0].account.unwrap();
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&app.panes[0].tabs[t].cwd));
    fs::create_dir_all(&dir).unwrap();
    let ts = chrono::Utc::now().to_rfc3339();
    let l = json!({"type": "assistant", "timestamp": ts, "message": {"content": [
        {"type": "tool_use", "name": "Bash", "input": {"command": "bsk navigate https://news.example.com"}}
    ]}});
    fs::write(dir.join("sess-w.jsonl"), format!("{l}\n")).unwrap();
    app.open_browser_dialog(0);
    settle(&app);
    refresh(&mut app);
    let Modal::Browser(d) = app.modal.clone() else {
        panic!("no dialog")
    };
    let v = app.browser_dialog_view(&d).unwrap();
    let text: Vec<String> = v.lines.iter().map(|l| l.text.clone()).collect();
    assert!(
        text.iter()
            .any(|l| l.contains("Last in the browser: navigate news.example.com")),
        "{text:?}"
    );

    let j = app.tab_browser_json(0, t).unwrap();
    assert!(j["last_action"]
        .as_str()
        .unwrap()
        .starts_with("navigate news.example.com"));

    assert!(v.buttons.iter().any(|b| b.1 == UiAction::BrowserOpenTab));
    tb::LAUNCHED.lock().unwrap().clear();
    key(&mut app, KeyCode::Char('o'));
    assert_eq!(
        launched(),
        vec![chrome_cmd(Os::current(), "chrome://newtab", Some("Default")).unwrap()]
    );

    app.modal = Modal::None;
    app.bind_tab_browser(uid, None);
    tb::LAUNCHED.lock().unwrap().clear();
    app.open_tab_chrome(uid);
    assert!(launched().is_empty());
    assert!(matches!(&app.modal, Modal::Browser(d) if d.uid == uid));
    assert!(crate::palette::ACTIONS
        .iter()
        .any(|(_, a)| *a == crate::palette::Action::BrowserOpenTab));
    let _ = fs::remove_dir_all(home);
}
