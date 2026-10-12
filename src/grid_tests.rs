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

#[test]
fn collapse_gives_siblings_the_space_and_keeps_tabs_running() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("collapse");
    let stub = crate::test_stub::claude(&home.join("bin"), &[("sleep_s", "30".into())]);
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    app.launch_tab(0, 0, crate::pane::LaunchKind::Normal);
    assert!(app.panes[0].tabs[0].is_running());
    let _ = draw(&mut app, 200, 60);
    let size_before = app.panes[0].tabs[0].pty_size();
    let area = ratatui::layout::Rect::new(0, 0, 200, 50);
    let rect_of = |app: &crate::app::App, acct: usize| {
        let (vis, a) = app.arrange(area);
        a.placed
            .iter()
            .find(|p| app.panes[vis[p.index]].account == Some(acct))
            .map(|p| p.rect)
            .unwrap()
    };
    assert_eq!(rect_of(&app, 2).height, 25);
    // "collapse the top two" in a 2x2.
    let v = app.control_call("collapse_accounts", &json!({"accounts": "top two"}));
    let say = v["result"]["say"].as_str().unwrap();
    assert!(say.starts_with("Collapsed "), "{v}");
    assert!(app.account_collapsed(0) && app.account_collapsed(1));
    assert!(!app.account_collapsed(2) && !app.account_collapsed(3));
    // The bottom two fill the height under one row of strips.
    assert_eq!(rect_of(&app, 0).height, 1);
    assert_eq!(rect_of(&app, 1).height, 1);
    assert_eq!(rect_of(&app, 2), ratatui::layout::Rect::new(0, 1, 100, 49));
    assert_eq!(
        rect_of(&app, 3),
        ratatui::layout::Rect::new(100, 1, 100, 49)
    );
    // Drawn as a strip with the account and its tab count.
    let t = draw(&mut app, 200, 60);
    let name0 = app.cfg.accounts[0].display().to_string();
    assert!(
        t.lines()
            .any(|l| l.contains("▸") && l.contains(&name0) && l.contains("1 tab")),
        "{t}"
    );
    // The PTY keeps running at its size: no resize while collapsed.
    assert!(
        app.panes[0].tabs[0].is_running(),
        "collapsing never stops a tab"
    );
    assert_eq!(app.panes[0].tabs[0].pty_size(), size_before);
    // The state block and config.toml know it; kept across a restart.
    assert!(
        app.state_preamble().contains("collapsed a1 a2"),
        "{}",
        app.state_preamble()
    );
    let cfg = crate::config::Config::load_or_init().unwrap();
    assert_eq!(
        cfg.collapsed_accounts,
        vec![
            app.cfg.accounts[0].name.clone(),
            app.cfg.accounts[1].name.clone()
        ]
    );
    let (tx, _rx) = std::sync::mpsc::channel();
    let again = crate::app::App::new(cfg, tx);
    assert!(again.pane_folded(0) && !again.pane_folded(2));
    // A click on the strip expands it.
    let strip = app
        .last_hits
        .find(&crate::hits::UiAction::FoldAccount(0))
        .unwrap()
        .rect;
    assert_eq!(strip.height, 1);
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: strip.x + 3,
            row: strip.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    )));
    assert!(!app.account_collapsed(0));
    assert_eq!(app.focus, 0);
    // Every target close_accounts takes: all except one, a provider.
    app.control_call(
        "collapse_accounts",
        &json!({"accounts": "all", "except": 4}),
    );
    assert_eq!(
        (0..4).map(|a| app.account_collapsed(a)).collect::<Vec<_>>(),
        [true, true, true, false]
    );
    app.control_call("expand_accounts", &json!({"accounts": "grok"}));
    assert!(!app.account_collapsed(2) && app.account_collapsed(0));
    // Everything collapsed: stacked strips and a hint.
    let v = app.control_call("collapse_accounts", &json!({"accounts": "all"}));
    assert!(
        v["result"]["say"]
            .as_str()
            .unwrap()
            .contains("Everything is collapsed"),
        "{v}"
    );
    let t = draw(&mut app, 200, 60);
    assert!(t.contains("Every pane is collapsed"), "{t}");
    let ys: Vec<u16> = (0..4).map(|a| rect_of(&app, a).y).collect();
    assert_eq!(ys, vec![0, 1, 2, 3]);
    assert!(app.panes[0].tabs[0].is_running());
    // Ctrl-a _ on the focused pane toggles it.
    app.focus = 1;
    app.on_command(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('_'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(!app.account_collapsed(1));
    app.control_call("expand_accounts", &json!({"accounts": "all"}));
    assert!((0..4).all(|a| !app.account_collapsed(a)));
    for p in &mut app.panes {
        p.kill_all();
    }
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn positions_name_accounts() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (app, home) = test_app("positions");
    let t = |s: &str| app.grid_targets(&json!(s)).unwrap();
    assert_eq!(t("top two"), vec![0, 1]);
    assert_eq!(t("the top ones"), vec![0, 1]);
    assert_eq!(t("bottom row"), vec![2, 3]);
    assert_eq!(t("left"), vec![0, 2]);
    assert_eq!(t("right two"), vec![1, 3]);
    assert_eq!(t("top 1"), vec![0]);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn focusing_a_collapsed_pane_expands_it_only_when_set() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("collapsefocus");
    let _ = draw(&mut app, 200, 60);
    app.control_call("collapse_accounts", &json!({"accounts": 2}));
    let key = |c: char, m: crossterm::event::KeyModifiers| {
        crate::app::AppEvent::Input(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char(c), m),
        ))
    };
    let none = crossterm::event::KeyModifiers::NONE;
    let ctrl = crossterm::event::KeyModifiers::CONTROL;
    // Default: focus by number leaves it collapsed.
    app.handle(key('a', ctrl));
    app.handle(key('2', none));
    assert_eq!(app.focus, 1);
    assert!(app.account_collapsed(1));
    // With the setting on, focusing it expands it.
    app.cfg.expand_collapsed_on_focus = true;
    app.handle(key('a', ctrl));
    app.handle(key('1', none));
    app.handle(key('a', ctrl));
    app.handle(key('2', none));
    assert!(!app.account_collapsed(1));
    let _ = std::fs::remove_dir_all(home);
}
