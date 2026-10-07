//! Closing and opening accounts in the grid, and any layout: by voice
//! tool, kept across a restart, never a logout, never a stopped tab.

use serde_json::json;

use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-grid-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    cfg.accounts[2].harness = "grok".into();
    cfg.accounts[3].harness = "grok".into();
    cfg.save().unwrap();
    let mut app = crate::app::App::new(cfg, tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    for a in 0..4 {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    (app, home)
}

fn shown_accounts(app: &crate::app::App) -> Vec<usize> {
    app.visible_panes()
        .iter()
        .filter_map(|&p| app.panes[p].account)
        .collect()
}

fn draw(app: &mut crate::app::App, w: u16, h: u16) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| crate::ui::draw(f, app)).unwrap();
    let b = term.backend().buffer();
    let mut s = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            s.push_str(b[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

#[test]
fn close_by_provider_then_reopen_and_restart() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("provider");
    assert_eq!(shown_accounts(&app), vec![0, 1, 2, 3]);
    let v = app.control_call("close_accounts", &json!({"accounts": "grok accounts"}));
    assert_eq!(
        v["result"]["say"],
        json!("Closed both Grok accounts. They stay logged in; say open Grok to bring them back.")
    );
    assert_eq!(shown_accounts(&app), vec![0, 1]);
    // Still configured and logged in; the state block says so.
    assert_eq!(app.cfg.accounts.len(), 4);
    assert!(app.accounts[2].login.logged_in() && app.accounts[3].login.logged_in());
    assert!(app
        .state_preamble()
        .contains("grid: open a1 a2, closed a3 a4"));
    // Kept across a restart.
    let cfg = crate::config::Config::load_or_init().unwrap();
    assert_eq!(
        cfg.closed_accounts,
        vec![
            app.cfg.accounts[2].name.clone(),
            app.cfg.accounts[3].name.clone()
        ]
    );
    let (tx, _rx) = std::sync::mpsc::channel();
    let again = crate::app::App::new(cfg, tx);
    assert_eq!(shown_accounts(&again), vec![0, 1]);
    // Reopen in place; "only" closes the rest; except.
    app.control_call("open_accounts", &json!({"accounts": "grok"}));
    assert_eq!(shown_accounts(&app), vec![0, 1, 2, 3]);
    app.control_call("open_accounts", &json!({"accounts": [2, 3], "only": true}));
    assert_eq!(shown_accounts(&app), vec![1, 2]);
    app.control_call("close_accounts", &json!({"accounts": "all", "except": 2}));
    assert_eq!(shown_accounts(&app), vec![1]);
    // Settings > Accounts still lists every account.
    app.view = crate::app::View::Settings;
    app.settings_section = crate::settings::SECTIONS
        .iter()
        .position(|(s, _)| *s == crate::settings::Section::Accounts)
        .unwrap();
    let t = draw(&mut app, 160, 60);
    for a in 0..4 {
        assert!(
            t.contains(&format!("Log in {}", app.cfg.accounts[a].display())),
            "{t}"
        );
    }
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn close_all_shows_a_hint_and_tabs_keep_running() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("closeall");
    // A running tab on account 1.
    let stub = crate::test_stub::claude(&home.join("bin"), &[("sleep_s", "30".into())]);
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    app.launch_tab(0, 0, crate::pane::LaunchKind::Normal);
    assert!(app.panes[0].tabs[0].is_running());
    let v = app.control_call("close_accounts", &json!({"accounts": "all"}));
    assert!(
        v["result"]["say"]
            .as_str()
            .unwrap()
            .contains("nothing is open now"),
        "{v}"
    );
    assert!(app.visible_panes().is_empty());
    let t = draw(&mut app, 140, 40);
    assert!(t.contains("Every account is closed"), "{t}");
    assert!(
        app.panes[0].tabs[0].is_running(),
        "closing never stops a tab"
    );
    app.control_call("open_accounts", &json!({"accounts": 1}));
    assert!(app.panes[0].tabs[0].is_running());
    assert_eq!(app.focus, 0);
    for p in &mut app.panes {
        p.kill_all();
    }
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn layouts_from_structured_descriptions() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("layouts");
    let area = ratatui::layout::Rect::new(0, 0, 200, 50);
    let rect_of = |app: &crate::app::App, acct: usize| {
        let (vis, a) = app.arrange(area);
        a.placed
            .iter()
            .find(|p| app.panes[vis[p.index]].account == Some(acct))
            .map(|p| p.rect)
    };
    // Account 1 big on the left, the rest stacked on the right.
    let v = app.control_call(
        "set_layout",
        &json!({"tree": {"split": "columns", "sizes": [2, 1], "children": [{"account": 1}, {"rest": true}]}}),
    );
    assert_eq!(v["result"]["say"], json!("Layout set: a1 | rest."), "{v}");
    assert_eq!(rect_of(&app, 0).unwrap().width, 133);
    assert!(rect_of(&app, 3).unwrap().x == 133);
    assert!(app.state_preamble().contains("layout custom a1 | rest"));
    // Kept in config.
    let cfg = crate::config::Config::load_or_init().unwrap();
    assert!(cfg.layout_tree.contains("\"rest\""));
    // Just account 2: the others are not drawn.
    app.control_call("set_layout", &json!({"tree": {"account": 2}}));
    assert_eq!(rect_of(&app, 1), Some(area));
    assert_eq!(rect_of(&app, 0), None);
    // Three rows, two columns, 2x2 as modes.
    app.control_call("set_layout", &json!({"mode": "rows"}));
    assert_eq!(app.layout_name(), "rows");
    app.control_call("set_layout", &json!({"mode": "grid", "grid": "2x2"}));
    assert_eq!(
        (app.layout_name().as_str(), app.cfg.grid.as_str()),
        ("grid", "2x2")
    );
    let bad = app.control_call(
        "set_layout",
        &json!({"tree": {"split": "diagonal", "children": [{"account": 1}]}}),
    );
    assert!(bad["error"].as_str().unwrap().contains("columns"), "{bad}");
    // A layout naming a closed account opens it.
    app.control_call("close_accounts", &json!({"accounts": 4}));
    app.control_call(
        "set_layout",
        &json!({"tree": {"split": "columns", "children": [{"account": 1}, {"account": 4}]}}),
    );
    assert!(!app.account_closed(3));
    // Too small to fit: the automatic grid instead, no crash.
    app.control_call(
        "set_layout",
        &json!({"tree": {"split": "columns", "children": [{"account": 1}, {"account": 2}, {"account": 3}, {"account": 4}]}}),
    );
    let (_, a) = app.arrange(ratatui::layout::Rect::new(0, 0, 60, 20));
    assert!(!a.placed.is_empty() && a.shape != "custom");
    let _ = draw(&mut app, 60, 20);
    let _ = std::fs::remove_dir_all(home);
}
