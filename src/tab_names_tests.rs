//! Tabs are named by their project or task when GodTerm reports on them:
//! the name order, the user's own name and the home folder skipped, the
//! account added only on a clash, and attention notices that never name
//! only an account and a tab number. Temp folders only.

use std::path::{Path, PathBuf};

use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::Activity;
use crate::project_memory::{folder_key, Store};
use crate::tab_names::meaningful;

fn test_app(tag: &str) -> (App, PathBuf) {
    let home = std::env::temp_dir().join(format!("godterm-names-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    (app, home)
}

fn tab_in(app: &mut App, slot: usize, dir: &Path, name: Option<&str>) -> u64 {
    std::fs::create_dir_all(dir).unwrap();
    let t = app.panes[slot].add_tab(dir.to_path_buf());
    let tb = &mut app.panes[slot].tabs[t];
    tb.custom_name = name.map(str::to_string);
    tb.uid
}

/// "Account 2 tab three finished", "Account 1 needs approval".
fn only_account_and_number(text: &str) -> bool {
    let rest = text
        .trim_end_matches(" finished")
        .trim_end_matches(" needs approval");
    let w: Vec<&str> = rest.split_whitespace().collect();
    matches!(
        w.as_slice(),
        ["Account", _] | ["Account", _, "tab", _] | ["pane", _] | ["pane", _, "tab", _]
    )
}

#[test]
fn names_come_in_order_tab_name_description_project_task() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("order");
    let mut store = Store::default();
    // 1. A meaningful tab name wins over everything.
    let hyper = home.join("work").join("hyper");
    let a = tab_in(&mut app, 0, &hyper, Some("hyper demo"));
    store
        .entry("project", &folder_key(&hyper))
        .aliases
        .push("trading room bot".into());
    assert_eq!(app.tab_base_name(&store, a).unwrap(), "hyper demo");
    // 2. A generic folder: the saved alias, else the description.
    let bot = home.join("bot").join("src");
    let b = tab_in(&mut app, 0, &bot, None);
    store
        .entry("project", &folder_key(&bot))
        .aliases
        .push("trading room bot".into());
    assert_eq!(
        app.tab_base_name(&store, b).unwrap(),
        "the trading room bot"
    );
    let dash = home.join("dash").join("code");
    let c = tab_in(&mut app, 0, &dash, None);
    store.entry("project", &folder_key(&dash)).description =
        "Billing dashboard for the shop. Built with Next.".into();
    assert_eq!(
        app.tab_base_name(&store, c).unwrap(),
        "the billing dashboard for the shop"
    );
    // 3. Nothing saved: the subproject's name from the project index.
    let root = home.join("acme");
    let sub = root.join("packages").join("api").join("src");
    let d = tab_in(&mut app, 0, &sub, None);
    app.project_index.lock().unwrap().roots.insert(
        root.clone(),
        crate::project_index::RootMap {
            root: root.clone(),
            entries: vec![crate::project_index::Entry {
                rel: "packages/api".into(),
                name: Some("@acme/billing-api".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    assert_eq!(app.tab_base_name(&store, d).unwrap(), "billing-api");
    // 4. Nothing about the folder: what the tab was asked to do.
    let tmp = home.join("tmp");
    let e = tab_in(&mut app, 0, &tmp, None);
    app.register_follow_up(e, "Fix the login bug on checkout");
    assert_eq!(
        app.tab_base_name(&store, e).unwrap(),
        "the tab working on 'Fix the login bug on'"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_users_name_and_home_folder_are_not_names() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(!meaningful("alice", Some("alice")));
    assert!(!meaningful("Alice", Some("alice")));
    assert!(!meaningful("~", None));
    assert!(!meaningful("", None));
    assert!(!meaningful("src", None));
    assert!(meaningful("hyper", Some("alice")));
    let (mut app, home) = test_app("home");
    let store = Store::default();
    // A tab in the home folder, named after it: not "alice".
    let t = app.panes[0].add_tab(crate::config::home_dir());
    let uid = app.panes[0].tabs[t].uid;
    let user = crate::config::home_dir()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let n = app.tab_base_name(&store, uid).unwrap();
    assert_ne!(n, user);
    assert_eq!(n, "a fresh tab");
    // Its task names it instead.
    app.register_follow_up(uid, "Summarize the trading room logs");
    let n = app.tab_base_name(&store, uid).unwrap();
    assert!(n.contains("Summarize the trading room logs"), "{n}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_account_is_added_only_when_two_tabs_sound_the_same() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("clash");
    let a = tab_in(&mut app, 0, &home.join("one"), Some("api server"));
    let b = tab_in(&mut app, 1, &home.join("two"), Some("api server"));
    let c = tab_in(&mut app, 0, &home.join("three"), Some("hyper demo"));
    let acct = |app: &App, slot: usize| {
        app.cfg.accounts[app.panes[slot].account.unwrap()]
            .display()
            .to_string()
    };
    assert_eq!(
        app.spoken_tab_name(a),
        format!("api server on {}", acct(&app, 0))
    );
    assert_eq!(
        app.spoken_tab_name(b),
        format!("api server on {}", acct(&app, 1))
    );
    assert_eq!(app.spoken_tab_name(c), "hyper demo");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn attention_notices_name_the_project_not_account_and_tab_number() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("attention");
    app.cfg.notifications = false;
    let fin = tab_in(&mut app, 1, &home.join("hyper"), None);
    let wait = tab_in(&mut app, 1, &home.join("bot").join("src"), None);
    app.register_follow_up(wait, "Deploy the trading room bot");
    let (s, t) = app.find_tab(wait).unwrap();
    app.panes[s].tabs[t].activity = Activity::Permission;
    let fs = app.find_tab(fin).unwrap();
    app.attention.push(crate::app::Attention {
        slot: fs.0,
        tab: fs.1,
        what: Activity::Ready,
        uid: fin,
        from: Activity::Working,
    });
    app.attention.push(crate::app::Attention {
        slot: s,
        tab: t,
        what: Activity::Permission,
        uid: wait,
        from: Activity::Working,
    });
    app.voice_tick();
    // Nothing is said by GodTerm itself: both are events for the
    // assistant, named by project or task.
    assert!(
        !app.told.borrow().ring.iter().any(|e| e.kind == "attention"),
        "the announcer says nothing"
    );
    let said: Vec<String> = app
        .assistant
        .events
        .events
        .iter()
        .map(|e| e.line())
        .collect();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        said.iter()
            .any(|x| x.contains("(hyper,") && x.contains("finished")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|x| x.contains("Deploy the trading room bot") && x.contains("approval")),
        "{said:?}"
    );
    for e in &app.assistant.events.events {
        assert!(!only_account_and_number(&e.tab), "{}", e.tab);
    }
    assert!(only_account_and_number("Account 3 tab three finished"));
    let _ = std::fs::remove_dir_all(&home);
}
