//! More than one Chrome profile per tab, connected browsers the user
//! linked to a profile, and the web app's Browser buttons (tab_browser,
//! link_browser) that only the user may press. Chrome is never launched
//! and its real profiles are never read (the fixture of tab_browser_tests).

use std::fs;

use crossterm::event::KeyCode;
use serde_json::{json, Value};

use crate::app::Modal;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;
use crate::jobs_tests::running;
use crate::tab_browser::{
    self as tb, apply_bind, assign_browsers, clean_also, match_browsers, BindOp,
};
use crate::tab_browser_tests::{
    bsk, key, launched, pe, refresh, set_pref, settle, setup, snapshot, tab_call, uid_of,
};

fn links(pairs: &[(&str, &str)]) -> tb::Links {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn web(app: &mut crate::app::App, tool: &str, mut args: Value) -> Value {
    args["_client"] = json!("web");
    app.control_call(tool, &args)
}

// ---- the set of profiles ----

#[test]
fn the_set_of_profiles_changes_as_asked() {
    let (mut p, mut a) = (None, vec![]);
    let d = |s: &str| s.to_string();
    // The first one added is the primary.
    assert!(apply_bind(&mut p, &mut a, &BindOp::Add(d("Default"))));
    assert_eq!((p.as_deref(), a.len()), (Some("Default"), 0));
    assert!(apply_bind(&mut p, &mut a, &BindOp::Add(d("Profile 1"))));
    assert_eq!(a, vec![d("Profile 1")]);
    // No repeats, never the primary twice.
    assert!(!apply_bind(&mut p, &mut a, &BindOp::Add(d("Profile 1"))));
    assert!(!apply_bind(&mut p, &mut a, &BindOp::Add(d("Default"))));
    // Primary swaps: the old one stays as a more profile.
    assert!(apply_bind(&mut p, &mut a, &BindOp::Primary(d("Profile 1"))));
    assert_eq!(
        (p.as_deref(), a.clone()),
        (Some("Profile 1"), vec![d("Default")])
    );
    // Set replaces the primary only (what "Use profile" always did).
    assert!(apply_bind(&mut p, &mut a, &BindOp::Set(d("Profile 2"))));
    assert_eq!(
        (p.as_deref(), a.clone()),
        (Some("Profile 2"), vec![d("Default")])
    );
    // Removing the primary promotes the next one.
    assert!(apply_bind(&mut p, &mut a, &BindOp::Remove(d("Profile 2"))));
    assert_eq!((p.as_deref(), a.len()), (Some("Default"), 0));
    assert!(apply_bind(&mut p, &mut a, &BindOp::Add(d("Profile 1"))));
    assert!(apply_bind(&mut p, &mut a, &BindOp::Clear));
    assert_eq!((p, a.len()), (None, 0));
    // Ops from words; paths are refused.
    assert_eq!(
        BindOp::parse("add", Some("Profile 3")),
        Ok(BindOp::Add(d("Profile 3")))
    );
    assert!(BindOp::parse("add", Some("../x")).is_err());
    assert!(BindOp::parse("add", None).is_err());
    assert!(BindOp::parse("nope", Some("Default")).is_err());
    // Cleaning: folder names, no primary, no repeats, nothing alone.
    assert_eq!(
        clean_also(
            Some("Default"),
            &[d("Default"), d("Profile 1"), d("/etc"), d("Profile 1")]
        ),
        vec![d("Profile 1")]
    );
    assert!(clean_also(None, &[d("Profile 1")]).is_empty());
}

#[test]
fn several_profiles_are_saved_with_the_tab_and_old_state_still_loads() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("multi-persist");
    let uid = uid_of(&app, t);
    assert!(app.bind_tab_browsers(uid, &BindOp::Add("Default".into())));
    assert!(app.bind_tab_browsers(uid, &BindOp::Add("Profile 1".into())));
    assert_eq!(app.tab_browser_dirs(uid), vec!["Default", "Profile 1"]);
    let st = app.snapshot_state();
    let text = serde_json::to_string(&st).unwrap();
    assert!(
        text.contains(r#""browser":"Default""#) && text.contains(r#""browser_also":["Profile 1"]"#),
        "{text}"
    );
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut next = crate::app::App::new(crate::config::Config::default(), tx);
    next.restore(st);
    assert_eq!(next.tab_browser_dirs(uid), vec!["Default", "Profile 1"]);
    // A state file from before: one profile, nothing more.
    let old: crate::state::TabState =
        serde_json::from_str(r#"{"cwd":"/x","browser":"Profile 1"}"#).unwrap();
    assert_eq!(old.browser.as_deref(), Some("Profile 1"));
    assert!(old.browser_also.is_empty());
    // A one-profile tab writes no new key, so older GodTerms read it as before.
    app.bind_tab_browsers(uid, &BindOp::Remove("Profile 1".into()));
    let text = serde_json::to_string(&app.snapshot_state()).unwrap();
    assert!(!text.contains("browser_also"), "{text}");
    // Closing and reopening keeps them.
    app.bind_tab_browsers(uid, &BindOp::Add("Profile 2".into()));
    let c = app.snapshot_tab(0, t, 0).expect("a closed record");
    assert_eq!(c.browser_also, vec!["Profile 2".to_string()]);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_moved_tab_keeps_all_its_profiles() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("multi-move");
    app.panes[0].tabs[t].session_id = None;
    let uid = uid_of(&app, t);
    app.bind_tab_browsers(uid, &BindOp::Add("Default".into()));
    app.bind_tab_browsers(uid, &BindOp::Add("Profile 1".into()));
    app.move_tab_now(0, t, 1, false, true);
    let new = *app.moved_ids.get(&uid).expect("a new tab id");
    assert_eq!(app.tab_browser_dirs(new), vec!["Default", "Profile 1"]);
    let _ = fs::remove_dir_all(home);
}

// ---- matching connected browsers ----

#[test]
fn a_linked_label_is_that_profile_and_no_other() {
    let me = pe("Default", "Me", true, Some(true));
    let work = pe("Profile 1", "Work", true, Some(true));
    let all = vec![bsk("aaa", "Alice's Personal Browser"), bsk("bbb", "Work")];
    // Unlinked: the free text label names no profile.
    let (m, how) = match_browsers(&all, &me.profile, 2, true, &links(&[]));
    assert!(m.is_empty());
    assert_eq!(how, "unknown");
    // Linked (case and spaces do not matter).
    let l = links(&[(" alice's personal browser", "Default")]);
    let (m, how) = match_browsers(&all, &me.profile, 2, true, &l);
    assert_eq!(
        (m.len(), m[0].instance_id.as_str(), how),
        (1, "aaa", "linked")
    );
    // A label linked elsewhere is not guessed from its words.
    let l = links(&[("Work", "Default")]);
    assert!(match_browsers(&all, &work.profile, 2, true, &l)
        .0
        .is_empty());
    // Nor is it the lone browser of another profile.
    let one = vec![bsk("ccc", "Work")];
    assert!(match_browsers(&one, &work.profile, 1, true, &l)
        .0
        .is_empty());
    // Every browser gets its profile, or none.
    let ps = vec![
        me.clone(),
        work.clone(),
        pe("Profile 2", "Bare", false, None),
    ];
    let all = vec![
        bsk("aaa", "Alice's Personal Browser"),
        bsk("bbb", "work"),
        bsk("ccc", "Mystery"),
    ];
    let got = assign_browsers(
        &all,
        &ps,
        &links(&[("Alice's Personal Browser", "Default")]),
    );
    let short: Vec<(&str, Option<&str>, &str)> = got
        .iter()
        .map(|(b, d, h)| (b.instance_id.as_str(), d.as_deref(), *h))
        .collect();
    assert_eq!(
        short,
        vec![
            ("aaa", Some("Default"), "linked"),
            ("bbb", Some("Profile 1"), "label"),
            ("ccc", None, "unknown"),
        ]
    );
    // A label that names two profiles is not guessed.
    let twins = vec![
        pe("Default", "Same", true, Some(true)),
        pe("Profile 1", "Same", true, Some(true)),
    ];
    assert_eq!(
        assign_browsers(&[bsk("x", "same")], &twins, &links(&[]))[0].2,
        "ambiguous"
    );
    // A lone browser with one profile on.
    let got = assign_browsers(
        &[bsk("y", "")],
        &[me, pe("Profile 1", "Work", false, None)],
        &links(&[]),
    );
    assert_eq!(
        (got[0].1.as_deref(), got[0].2),
        (Some("Default"), "only browser")
    );
}

// ---- the tab's own tools ----

#[test]
fn a_tab_with_two_profiles_is_told_both_and_which_is_primary() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, a) = setup("multi-tools");
    let b = running(&mut app, 0, "api", "sess-b");
    let root = app.browser.env.clone().unwrap().chrome_root;
    set_pref(&root, "Profile 1", Some(1));
    refresh(&mut app);
    let (ua, ub) = (uid_of(&app, a), uid_of(&app, b));
    app.bind_tab_browsers(ua, &BindOp::Add("Profile 1".into()));
    app.bind_tab_browsers(ua, &BindOp::Add("Default".into()));
    app.bind_tab_browsers(ub, &BindOp::Add("Profile 2".into()));
    app.cfg.browser.links = links(&[("Alice's Personal Browser", "Default")]);
    let s = snapshot(
        &app,
        vec![
            bsk("inst-me", "Alice's Personal Browser"),
            bsk("inst-work", "Work"),
            bsk("inst-x", "Other"),
        ],
    );
    app.browser.set(s);
    let v = tab_call(&mut app, ua, "list_browsers");
    assert_eq!(v["ok"], true, "{v}");
    let r = &v["result"];
    assert_eq!(r["bound_profile"]["profile_dir"], "Profile 1");
    let bp = r["bound_profiles"].as_array().unwrap();
    assert_eq!(bp.len(), 2, "{r}");
    assert_eq!(
        (bp[0]["primary"].clone(), bp[1]["primary"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(bp[1]["profile_dir"], "Default");
    assert_eq!(bp[1]["match"], "linked");
    assert_eq!(r["count"], 2);
    let ids: Vec<&str> = r["browsers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["instance_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["inst-work", "inst-me"]);
    assert_eq!(r["browsers"][0]["primary"], true);
    assert_eq!(r["browsers"][1]["profile_dir"], "Default");
    let say = r["say"].as_str().unwrap();
    assert!(
        say.contains("--browser inst-work")
            && say.contains("--browser inst-me")
            && say.contains("primary")
            && say.contains("ask the user"),
        "{say}"
    );
    assert!(!v.to_string().contains("inst-x"));
    assert!(!say.contains('—'));
    // browser_status: both profiles, nothing of the other tab's.
    let v = tab_call(&mut app, ua, "browser_status");
    let profs = v["result"]["chrome_profiles"].as_array().unwrap();
    assert_eq!(profs.len(), 2, "{v}");
    assert!(!v.to_string().contains("Bare") && !v.to_string().contains("inst-x"));
    // The other tab sees only its own.
    let v = tab_call(&mut app, ub, "list_browsers");
    assert_eq!(v["result"]["bound_profiles"].as_array().unwrap().len(), 1);
    assert!(!v.to_string().contains("inst-me") && !v.to_string().contains("inst-work"));
    // The tab's agent cannot change its browser.
    for tool in ["tab_browser", "link_browser"] {
        let v = app.control_call(
            tool,
            &json!({"_client": format!("tab:{ua}"), "op": "add", "profile": "Profile 2", "label": "Other"}),
        );
        assert_eq!(v["ok"], false, "{tool}: {v}");
    }
    assert_eq!(app.tab_browser_dirs(ua), vec!["Profile 1", "Default"]);
    // list_tabs shows the more profiles too.
    let j = app.tab_browser_json(0, a).unwrap();
    assert_eq!(j["also"][0]["profile_dir"], "Default", "{j}");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn the_brains_list_browsers_says_whose_a_linked_browser_is() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _) = setup("multi-brain");
    app.cfg.browser.links = links(&[("Alice's Personal Browser", "Default")]);
    let s = snapshot(
        &app,
        vec![
            bsk("inst-me", "Alice's Personal Browser"),
            bsk("inst-x", "Other"),
        ],
    );
    app.browser.set(s);
    let v = app.control_call("list_browsers", &json!({"_client": "brain"}));
    assert_eq!(v["result"]["browsers"][0]["profile_dir"], "Default", "{v}");
    assert_eq!(v["result"]["browsers"][0]["match"], "linked");
    assert!(v["result"]["browsers"][1]["profile_dir"].is_null());
    let _ = fs::remove_dir_all(home);
}

// ---- the web app's buttons ----

#[test]
fn the_web_buttons_bind_clear_and_open_and_nobody_else_may_press_them() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("multi-web");
    let uid = uid_of(&app, t);
    let tab = crate::control::tab_id(uid);
    // The assistant and other clients are refused.
    for client in ["brain", "external"] {
        let v = app.control_call(
            "tab_browser",
            &json!({"_client": client, "tab": tab, "op": "set", "profile": "Default"}),
        );
        assert_eq!(v["ok"], false, "{client}: {v}");
        assert!(v.to_string().contains("user"), "{v}");
    }
    assert!(app.tab_browser_dirs(uid).is_empty());
    // The view: every profile, none bound yet.
    let v = web(&mut app, "tab_browser", json!({"tab": tab}));
    assert_eq!(v["ok"], true, "{v}");
    let r = &v["result"];
    assert_eq!(r["bound"].as_array().unwrap().len(), 0);
    assert_eq!(r["chrome_profiles"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(r["chrome_profiles"][0]["state"], "on");
    // Use for this tab: bound, and the default for links.
    let v = web(
        &mut app,
        "tab_browser",
        json!({"tab": tab, "op": "set", "profile": "Default"}),
    );
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(app.tab_browser_dirs(uid), vec!["Default"]);
    assert_eq!(app.cfg.browser.default_profile, "Default");
    assert_eq!(v["result"]["view"]["bound"][0]["profile_dir"], "Default");
    assert_eq!(v["result"]["view"]["chrome_profiles"][0]["primary"], true);
    // Also use, primary, remove.
    web(
        &mut app,
        "tab_browser",
        json!({"tab": tab, "op": "add", "profile": "Profile 1"}),
    );
    assert_eq!(app.tab_browser_dirs(uid), vec!["Default", "Profile 1"]);
    web(
        &mut app,
        "tab_browser",
        json!({"tab": tab, "op": "primary", "profile": "Profile 1"}),
    );
    assert_eq!(app.tab_browser_dirs(uid), vec!["Profile 1", "Default"]);
    // Open: a new tab in the primary, or a named bound profile only.
    tb::LAUNCHED.lock().unwrap().clear();
    let v = web(&mut app, "tab_browser", json!({"tab": tab, "op": "open"}));
    assert_eq!(v["ok"], true, "{v}");
    let v = web(
        &mut app,
        "tab_browser",
        json!({"tab": tab, "op": "open", "profile": "Default"}),
    );
    assert_eq!(v["ok"], true, "{v}");
    let v = web(
        &mut app,
        "tab_browser",
        json!({"tab": tab, "op": "open", "profile": "Profile 2"}),
    );
    assert_eq!(v["ok"], false, "not this tab's: {v}");
    let l = launched();
    assert_eq!(l.len(), 2);
    assert!(l[0]
        .args
        .iter()
        .any(|a| a == "--profile-directory=Profile 1"));
    assert!(l[1].args.iter().any(|a| a == "--profile-directory=Default"));
    web(
        &mut app,
        "tab_browser",
        json!({"tab": tab, "op": "remove", "profile": "Profile 1"}),
    );
    assert_eq!(app.tab_browser_dirs(uid), vec!["Default"]);
    // Clear, and bad input.
    let v = web(&mut app, "tab_browser", json!({"tab": tab, "op": "clear"}));
    assert!(
        v["result"]["say"].as_str().unwrap().contains("no browser"),
        "{v}"
    );
    assert!(app.tab_browser_dirs(uid).is_empty());
    let v = web(
        &mut app,
        "tab_browser",
        json!({"tab": tab, "op": "add", "profile": "../../x"}),
    );
    assert_eq!(v["ok"], false);
    let v = web(&mut app, "tab_browser", json!({"tab": tab, "op": "open"}));
    assert_eq!(v["ok"], false, "nothing to open: {v}");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn this_is_a_profile_links_a_connected_browser_in_config_toml() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("multi-link");
    let uid = uid_of(&app, t);
    let tab = crate::control::tab_id(uid);
    app.bind_tab_browser(uid, Some("Default".into()));
    let s = snapshot(
        &app,
        vec![
            bsk("inst-me", "Alice's Personal Browser"),
            bsk("inst-x", "Other"),
        ],
    );
    app.browser.set(s);
    // Unmatched at first: two browsers, neither named for a profile.
    let v = web(&mut app, "tab_browser", json!({"tab": tab}));
    assert_eq!(v["result"]["unmatched"], 2, "{v}");
    assert!(v["result"]["browsers"][0]["profile_dir"].is_null());
    // Only the user links.
    let v = app.control_call(
        "link_browser",
        &json!({"_client": "brain", "label": "Alice's Personal Browser", "profile": "Default"}),
    );
    assert_eq!(v["ok"], false, "{v}");
    let v = web(
        &mut app,
        "link_browser",
        json!({"tab": tab, "label": "Alice's Personal Browser", "profile": "Default"}),
    );
    assert_eq!(v["ok"], true, "{v}");
    let text = fs::read_to_string(crate::config::Config::path()).unwrap();
    let cfg = crate::config::Config::parse(&text).unwrap();
    assert_eq!(
        cfg.browser
            .links
            .get("Alice's Personal Browser")
            .map(String::as_str),
        Some("Default"),
        "{text}"
    );
    let view = &v["result"]["view"];
    assert_eq!(view["unmatched"], 1, "{view}");
    assert_eq!(view["browsers"][0]["profile_dir"], "Default");
    assert_eq!(view["browsers"][0]["match"], "linked");
    assert_eq!(view["browsers"][0]["this_tab"], true);
    assert_eq!(view["bound"][0]["browsers"][0], "inst-me");
    // The tab's agent now gets it.
    let v = tab_call(&mut app, uid, "list_browsers");
    assert_eq!(v["result"]["browsers"][0]["instance_id"], "inst-me", "{v}");
    // A label with dots and quotes survives; unlinking takes it out.
    web(
        &mut app,
        "link_browser",
        json!({"label": "a.b \"c\"", "profile": "Profile 1"}),
    );
    let cfg =
        crate::config::Config::parse(&fs::read_to_string(crate::config::Config::path()).unwrap())
            .unwrap();
    assert_eq!(cfg.browser.links.len(), 2);
    assert_eq!(cfg.browser.links["a.b \"c\""], "Profile 1");
    web(
        &mut app,
        "link_browser",
        json!({"label": "ALICE'S PERSONAL BROWSER"}),
    );
    let cfg =
        crate::config::Config::parse(&fs::read_to_string(crate::config::Config::path()).unwrap())
            .unwrap();
    assert_eq!(cfg.browser.links.len(), 1);
    assert!(!cfg.browser.links.contains_key("Alice's Personal Browser"));
    // Bad input.
    assert_eq!(
        web(&mut app, "link_browser", json!({"label": " "}))["ok"],
        false
    );
    assert_eq!(
        web(
            &mut app,
            "link_browser",
            json!({"label": "x", "profile": "/etc"})
        )["ok"],
        false
    );
    let _ = fs::remove_dir_all(home);
}

// ---- the terminal UI's dialog ----

#[test]
fn the_dialog_adds_and_removes_more_profiles() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t) = setup("multi-dialog");
    let uid = uid_of(&app, t);
    app.open_browser_dialog(0);
    settle(&app);
    refresh(&mut app);
    key(&mut app, KeyCode::Enter); // Default
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char('a')); // Profile 1, too
    assert_eq!(app.tab_browser_dirs(uid), vec!["Default", "Profile 1"]);
    let Modal::Browser(d) = app.modal.clone() else {
        panic!("no dialog")
    };
    let v = app.browser_dialog_view(&d).unwrap();
    let text: Vec<String> = v.lines.iter().map(|l| l.text.clone()).collect();
    assert!(
        text.iter()
            .any(|l| l.contains("Me") && l.contains("(this tab, primary)")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|l| l.contains("Work") && l.contains("(this tab too)")),
        "{text:?}"
    );
    assert!(text.iter().any(|l| l.starts_with("Also: Work")), "{text:?}");
    assert!(v
        .buttons
        .iter()
        .any(|b| b.0 == "Remove" && b.1 == UiAction::BrowserAlso));
    // The button takes it off again.
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 50)).unwrap();
    crate::confirm_click_tests::draw(&mut app, &mut term);
    crate::confirm_click_tests::click(&mut app, &UiAction::BrowserAlso);
    assert_eq!(app.tab_browser_dirs(uid), vec!["Default"]);
    // No browser clears them all.
    key(&mut app, KeyCode::Char('a'));
    key(&mut app, KeyCode::Char('c'));
    assert!(app.tab_browser_dirs(uid).is_empty());
    let _ = fs::remove_dir_all(home);
}
