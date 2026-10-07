//! Tab groups, pins and sorting: the tools (with groups as targets), the
//! list's rendering, persistence and dragging a tab onto a group.

use serde_json::json;

use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-groups-{tag}-{}", std::process::id()));
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
    for n in ["api", "web", "worker", "dashboard", "exp1", "exp2"] {
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

fn screen(app: &mut crate::app::App) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(160, 50)).unwrap();
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

/// The voice examples: "group these three tabs as api", "pin the
/// dashboard tab", "color the api group green", "sort account two by
/// most recent", "close everything except pinned"; and groups as targets.
#[test]
fn tools_with_groups_pins_and_sorts() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("tools");
    let say = |app: &mut crate::app::App, t: &str| {
        app.assistant_turn += 1;
        app.assistant.last_user = t.into();
    };
    say(&mut app, "group these three tabs as api");
    let tabs = json!([id(&app, "api"), id(&app, "web"), id(&app, "worker")]);
    let v = app.control_call("group_tabs", &json!({"tab": tabs, "group": "api"}));
    assert_eq!(v["result"]["say"], json!("Grouped 3 tabs as api."), "{v}");
    let v = app.control_call(
        "group_tabs",
        &json!({"tab": [id(&app, "exp1"), id(&app, "exp2")], "group": "experiments"}),
    );
    assert!(v["error"].is_null(), "{v}");
    let v = app.control_call(
        "set_group_color",
        &json!({"group": "the api", "color": "green"}),
    );
    assert_eq!(
        v["result"]["say"],
        json!("The api group is sage now."),
        "{v}"
    );
    let v = app.control_call("pin_tab", &json!({"tab": "dashboard"}));
    assert_eq!(v["result"]["pinned"], json!(true), "{v}");
    let v = app.control_call("sort_tabs", &json!({"account": 2, "by": "most recent"}));
    assert_eq!(v["result"]["sort"], json!("recent"), "{v}");
    assert_eq!(app.panes[1].sort, crate::tab_groups::TabSort::Recent);
    // get_state carries group, pinned and color.
    let st = app.control_call("get_state", &json!({}));
    let tabs = st["result"]["tabs"].as_array().cloned().unwrap_or_default();
    let api = tabs.iter().find(|t| t["name"] == "api").unwrap();
    assert_eq!(
        (api["group"].clone(), api["color"].clone()),
        (json!("api"), json!("sage"))
    );
    assert!(tabs
        .iter()
        .any(|t| t["name"] == "dashboard" && t["pinned"] == true));
    // Close everything except pinned: the question leaves dashboard out.
    say(&mut app, "close everything except pinned");
    let before = app.panes[1].tabs.len();
    let v = app.control_call("close_tabs", &json!({"account": 2}));
    let q = v["question"].as_str().unwrap();
    assert!(q.contains(&(before - 1).to_string()), "{q}");
    // A pinned tab named alone still asks.
    say(&mut app, "close the dashboard tab");
    let v = app.control_call("close_tabs", &json!({"tab": id(&app, "dashboard")}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    // A group as the target: one question, then all of it.
    say(&mut app, "close the experiments group");
    let v = app.control_call("close_tabs", &json!({"group": "experiments"}));
    assert!(
        v["question"].as_str().unwrap().starts_with("Close 2 tabs"),
        "{v}"
    );
    let tok = v["token"].as_str().unwrap().to_string();
    say(&mut app, "yes");
    let v = app.control_call("close_tabs", &json!({"confirm_token": tok}));
    assert_eq!(v["result"]["closed"], json!(2), "{v}");
    // Collapse, rename, ungroup; unknown group names say what there is.
    app.control_call("collapse_group", &json!({"group": "api"}));
    assert!(app.panes[1].groups[0].collapsed);
    app.control_call("rename_group", &json!({"group": "api", "name": "backend"}));
    assert_eq!(
        app.panes[1]
            .tabs
            .iter()
            .filter(|t| t.group.as_deref() == Some("backend"))
            .count(),
        3
    );
    let v = app.control_call("ungroup", &json!({"group": "nothing like it"}));
    assert!(v["error"].as_str().unwrap().contains("backend"), "{v}");
    let v = app.control_call(
        "set_tab_color",
        &json!({"tab": id(&app, "api"), "color": "red"}),
    );
    assert_eq!(v["result"]["color"], json!("clay"));
    assert_eq!(
        app.panes[1]
            .accent_of(
                app.panes[1]
                    .tabs
                    .iter()
                    .position(|t| t.name() == "api")
                    .unwrap()
            )
            .as_deref(),
        Some("clay")
    );
    // Moving a group: its tabs go, and the group with them.
    say(&mut app, "move the backend group to account one");
    let v = app.control_call("move_tab", &json!({"group": "backend", "to": 1}));
    assert!(v["error"].is_null(), "{v}");
    assert!(
        app.panes[0].groups.iter().any(|g| g.name == "backend"),
        "{:?}",
        app.panes[0].groups
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Groups, pins and the sort survive a restart; the list renders headers
/// (collapsed and expanded), the pin marker and color bars; dropping a
/// dragged tab on a header adds it to the group.
#[test]
fn list_renders_persists_and_takes_drops() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("render");
    app.focus = 1;
    let p = &mut app.panes[1];
    let ix = |p: &crate::slot::Slot, n: &str| p.tabs.iter().position(|t| t.name() == n).unwrap();
    let (a, w) = (ix(p, "api"), ix(p, "web"));
    p.group_tabs(&[a, w], "api work", Some("sage".into()));
    let d = ix(p, "dashboard");
    p.tabs[d].pinned = true;
    p.sort = crate::tab_groups::TabSort::Name;
    let t = screen(&mut app);
    assert!(t.contains("▾ api work (2) ●"), "{t}");
    assert!(t.contains("▴"), "pin marker: {t}");
    assert!(t.contains("▎"), "color bar: {t}");
    assert!(t.contains("↓name"), "sort indicator: {t}");
    app.panes[1].groups[0].collapsed = true;
    let t = screen(&mut app);
    assert!(t.contains("▸ api work (2)"), "{t}");
    // Persisted.
    let st = app.snapshot_state();
    let (mut fresh, h2) = test_app("render2");
    crate::config::testing::set_home(&home);
    fresh.restore(st);
    let fp = &fresh.panes[1];
    assert_eq!(fp.groups[0].name, "api work");
    assert!(fp.groups[0].collapsed);
    assert_eq!(fp.sort, crate::tab_groups::TabSort::Name);
    assert!(fp.tabs.iter().any(|t| t.name() == "dashboard" && t.pinned));
    assert_eq!(
        fp.tabs
            .iter()
            .filter(|t| t.group.as_deref() == Some("api work"))
            .count(),
        2
    );
    // Drag "worker" onto the group's header.
    app.panes[1].groups[0].collapsed = false;
    screen(&mut app);
    let find = |app: &crate::app::App, f: &dyn Fn(&crate::hits::UiAction) -> bool| {
        app.last_hits
            .regions
            .iter()
            .find(|r| f(&r.action))
            .map(|r| r.rect)
            .unwrap()
    };
    let worker = app.panes[1]
        .tabs
        .iter()
        .position(|t| t.name() == "worker")
        .unwrap();
    let from = find(&app, &|x| *x == crate::hits::UiAction::SelectTab(1, worker));
    let to = find(&app, &|x| {
        matches!(x, crate::hits::UiAction::GroupHeader(1, 0))
    });
    let ev = |k, x: u16, y: u16| {
        crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
            crossterm::event::MouseEvent {
                kind: k,
                column: x,
                row: y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        ))
    };
    app.handle(ev(
        MouseEventKind::Down(MouseButton::Left),
        from.x + 4,
        from.y,
    ));
    app.handle(ev(MouseEventKind::Drag(MouseButton::Left), to.x + 4, to.y));
    app.handle(ev(MouseEventKind::Up(MouseButton::Left), to.x + 4, to.y));
    assert_eq!(app.panes[1].tabs[worker].group.as_deref(), Some("api work"));
    // Right click on the header: its menu; Ungroup.
    app.handle(ev(MouseEventKind::Down(MouseButton::Right), to.x + 4, to.y));
    assert_eq!(app.modal, crate::app::Modal::GroupMenu(1, 0, 0));
    app.group_menu_run(1, 0, 5);
    assert!(app.panes[1].groups.is_empty());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&h2);
}
