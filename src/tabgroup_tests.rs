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

fn key(app: &mut crate::app::App, code: crossterm::event::KeyCode) {
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
    )));
}

fn typed(app: &mut crate::app::App, s: &str) {
    for c in s.chars() {
        key(app, crossterm::event::KeyCode::Char(c));
    }
}

fn tab_rows(app: &crate::app::App, s: usize) -> Vec<String> {
    app.list_rows(s, false)
        .iter()
        .filter_map(|r| match r {
            crate::tab_groups::Row::Tab(t) => Some(app.panes[s].tabs[*t].name()),
            _ => None,
        })
        .collect()
}

/// The search field atop a tab list: Ctrl-a / or a click, typing
/// filters (names, the session's own title, folder, status, fuzzy), "2
/// of 7", Up / Down pick, Esc clears; and by voice.
#[test]
fn search_filters_the_list_and_counts() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("search");
    app.focus = 1;
    let n = app.panes[1].tabs.len();
    assert_eq!(n, 7);
    let t = screen(&mut app);
    assert!(t.contains("⌕ search"), "{t}");
    assert!(t.contains("↓manual ▾"), "{t}");
    // Ctrl-a / gives the field the keys; typing filters.
    app.on_command(crossterm::event::KeyEvent::new(
        KeyCode::Char('/'),
        KeyModifiers::NONE,
    ));
    assert_eq!(app.tab_search, Some(1));
    typed(&mut app, "exp");
    assert_eq!(app.panes[1].filter, "exp");
    assert_eq!(tab_rows(&app, 1), ["exp1", "exp2"]);
    let t = screen(&mut app);
    assert!(t.contains("2 of 7"), "{t}");
    // The keys stay out of the terminal; Down picks a match, Enter
    // keeps it and gives the keys back.
    key(&mut app, KeyCode::Down);
    let picked = app.panes[1].cur().name();
    assert!(picked.starts_with("exp"), "{picked}");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.tab_search, None);
    assert_eq!(app.panes[1].filter, "exp", "the filter stays until cleared");
    // Esc in the field clears it.
    app.focus_tab_search(1);
    key(&mut app, KeyCode::Esc);
    assert!(app.panes[1].filter.is_empty() && app.tab_search.is_none());
    assert_eq!(tab_rows(&app, 1).len(), 7);
    // Fuzzy: a misspelling, the status, the folder.
    app.panes[1].filter = "dashbord".into();
    assert_eq!(tab_rows(&app, 1), ["dashboard"]);
    let w = app.panes[1]
        .tabs
        .iter()
        .position(|t| t.name() == "worker")
        .unwrap();
    app.panes[1].tabs[w].activity = crate::pane::Activity::Working;
    app.panes[1].filter = "working".into();
    assert_eq!(tab_rows(&app, 1), ["worker"]);
    // The session's own title (Claude's or Grok's) is searched too.
    let api = app.panes[1]
        .tabs
        .iter()
        .position(|t| t.name() == "api")
        .unwrap();
    app.panes[1].tabs[api].session_id = Some("sess-botmesh".into());
    app.native_titles.borrow_mut().insert(
        "sess-botmesh".into(),
        (std::time::Instant::now(), Some("Bot mesh refactor".into())),
    );
    app.panes[1].filter = "botmesh".into();
    assert_eq!(tab_rows(&app, 1), ["api"]);
    // A click on the field focuses it; the × clears.
    app.panes[1].filter.clear();
    let _ = screen(&mut app);
    let field = app
        .last_hits
        .find(&crate::hits::UiAction::TabSearch(1))
        .unwrap()
        .rect;
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: field.x + 2,
            row: field.y,
            modifiers: KeyModifiers::NONE,
        },
    )));
    assert_eq!(app.tab_search, Some(1));
    typed(&mut app, "web");
    assert_eq!(tab_rows(&app, 1), ["web"]);
    let _ = screen(&mut app);
    assert!(app
        .last_hits
        .find(&crate::hits::UiAction::TabSearchClear(1))
        .is_some());
    key(&mut app, KeyCode::Esc);
    // Voice: "find the tab about botmesh".
    let v = app.control_call("search_tabs", &json!({"text": "bot mesh"}));
    assert_eq!(v["result"]["tabs"].as_array().unwrap().len(), 1, "{v}");
    assert!(v["result"]["say"].as_str().unwrap().starts_with("Found it"));
    assert_eq!((app.focus, app.panes[1].active), (1, api));
    assert_eq!(app.panes[1].filter, "bot mesh");
    app.control_call("search_tabs", &json!({"text": ""}));
    assert!(app.panes.iter().all(|p| p.filter.is_empty()));
    let _ = std::fs::remove_dir_all(home);
}

/// Pinned tabs first in every sort; each sort mode; sorting inside
/// groups; the dropdown and the sort_tabs tool use the same modes.
#[test]
fn pins_sort_modes_and_groups() {
    use crate::pane::Activity;
    use crate::tab_groups::TabSort;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("sorts");
    app.focus = 1;
    let ix = |app: &crate::app::App, n: &str| {
        app.panes[1]
            .tabs
            .iter()
            .position(|t| t.name() == n)
            .unwrap()
    };
    // The first tab (the account's folder) out of the way: closed.
    let first = app.panes[1].tabs[0].name();
    let _ = first;
    app.panes[1].close_tab(0, home.clone());
    let (api, web, worker, dash, e1, e2) = (
        ix(&app, "api"),
        ix(&app, "web"),
        ix(&app, "worker"),
        ix(&app, "dashboard"),
        ix(&app, "exp1"),
        ix(&app, "exp2"),
    );
    let now = std::time::SystemTime::now();
    for (k, t) in app.panes[1].tabs.iter_mut().enumerate() {
        t.opened = now - std::time::Duration::from_secs(100 - k as u64);
    }
    app.panes[1].tabs[worker].activity = Activity::Working;
    app.panes[1].tabs[e1].activity = Activity::Permission;
    app.panes[1].tabs[dash].pinned = true;
    let order = |app: &crate::app::App| tab_rows(app, 1);
    // Manual: the pinned one first, then the drag order.
    assert_eq!(
        order(&app),
        ["dashboard", "api", "web", "worker", "exp1", "exp2"]
    );
    app.set_tab_sort(1, TabSort::Name);
    assert_eq!(
        order(&app),
        ["dashboard", "api", "exp1", "exp2", "web", "worker"]
    );
    // Status: blocked, then working, then idle.
    app.set_tab_sort(1, TabSort::State);
    assert_eq!(order(&app)[1..3], ["exp1", "worker"]);
    // Most active: working first.
    app.set_tab_sort(1, TabSort::Active);
    assert_eq!(order(&app)[1], "worker");
    // Created: newest first.
    app.set_tab_sort(1, TabSort::Opened);
    assert_eq!(
        order(&app),
        ["dashboard", "exp2", "exp1", "worker", "web", "api"]
    );
    // Most recent: the last input first.
    app.set_tab_sort(1, TabSort::Recent);
    app.panes[1].tabs[web].last_input =
        std::time::Instant::now() + std::time::Duration::from_secs(60);
    *app.panes[1].order_cache.borrow_mut() = None;
    assert_eq!(order(&app)[1], "web");
    // Groups are sorted inside; the pinned one stays on top.
    app.panes[1].group_tabs(&[e1, e2, api], "lab", None);
    app.set_tab_sort(1, TabSort::Name);
    let rows = app.list_rows(1, false);
    assert_eq!(rows[0], crate::tab_groups::Row::Tab(dash));
    assert_eq!(rows[1], crate::tab_groups::Row::Group(0));
    assert_eq!(
        rows[2..5],
        [
            crate::tab_groups::Row::Tab(api),
            crate::tab_groups::Row::Tab(e1),
            crate::tab_groups::Row::Tab(e2)
        ]
    );
    // A search inside a folded group shows its matches.
    app.panes[1].groups[0].collapsed = true;
    app.panes[1].filter = "exp2".into();
    assert_eq!(
        app.list_rows(1, false),
        [
            crate::tab_groups::Row::Group(0),
            crate::tab_groups::Row::Tab(e2)
        ]
    );
    app.panes[1].filter.clear();
    // The dropdown atop the list opens the sort menu for this pane.
    app.focus = 0;
    let _ = screen(&mut app);
    let r = app
        .last_hits
        .find(&crate::hits::UiAction::TabSortMenu(1))
        .unwrap()
        .rect;
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: r.x + 1,
            row: r.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    )));
    assert!(
        matches!(&app.modal, crate::app::Modal::Menu(o) if o.id == crate::menus::MenuId::SortTabs)
    );
    assert_eq!(app.focus, 1);
    let labels: Vec<String> = crate::menus::entries(&app, crate::menus::MenuId::SortTabs)
        .iter()
        .map(|e| e.label.clone())
        .collect();
    for want in [
        "Manual",
        "Most active",
        "Most recent",
        "Name",
        "Status",
        "Created",
    ] {
        assert!(labels.iter().any(|l| l.starts_with(want)), "{labels:?}");
    }
    app.modal = crate::app::Modal::None;
    // The tool takes the same modes, as said.
    for (by, want) in [
        ("most recent", TabSort::Recent),
        ("most active", TabSort::Active),
        ("status", TabSort::State),
        ("created", TabSort::Opened),
        ("name", TabSort::Name),
        ("manual", TabSort::Manual),
    ] {
        let v = app.control_call("sort_tabs", &json!({"account": 2, "by": by}));
        assert!(v["result"]["say"].is_string(), "{v}");
        assert_eq!(app.panes[1].sort, want, "{by}");
    }
    let _ = std::fs::remove_dir_all(home);
}

/// The side list's width: Ctrl-a < / > in steps, kept between the minimum
/// and the maximum; wide enough, each tab shows the session's own title
/// under our name; narrow rows stay as they were; by voice too.
#[test]
fn sidebar_resize_keys_and_wide_details() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("wide");
    app.focus = 1;
    let api = app.panes[1]
        .tabs
        .iter()
        .position(|t| t.name() == "api")
        .unwrap();
    app.panes[1].tabs[api].session_id = Some("sess-wide".into());
    app.native_titles.borrow_mut().insert(
        "sess-wide".into(),
        (
            std::time::Instant::now(),
            Some("Refactor the botmesh router".into()),
        ),
    );
    app.panes[1].select(api);
    // Narrow (the automatic width): today's rows, no session title.
    let t = screen(&mut app);
    let w0 = app.sidebar_now(1);
    assert!(w0 < crate::ui::SIDEBAR_WIDE_AT, "{w0}");
    assert!(!t.contains("Refactor the"), "{t}");
    assert!(t.lines().any(|l| l.contains(" api")), "{t}");
    // Ctrl-a > widens by 4, Ctrl-a < narrows, within the bounds.
    let cmd = |app: &mut crate::app::App, c: char| {
        app.on_command(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    };
    cmd(&mut app, '>');
    assert_eq!(app.panes[1].sidebar_w, Some(w0 + 4));
    for _ in 0..20 {
        cmd(&mut app, '>');
    }
    let max = app.sidebar_max(1);
    assert!(max <= crate::ui::SIDEBAR_MAX && max > w0, "{max}");
    assert_eq!(app.panes[1].sidebar_w, Some(max));
    for _ in 0..20 {
        cmd(&mut app, '<');
    }
    assert_eq!(app.panes[1].sidebar_w, Some(crate::ui::SIDEBAR_MIN));
    // Kept per pane in the saved state.
    assert_eq!(
        app.snapshot_state().slots[1].sidebar_w,
        Some(crate::ui::SIDEBAR_MIN)
    );
    // "show tab details": wide, our name and the session's title.
    let v = app.control_call(
        "resize_tab_list",
        &json!({"width": "details", "account": 2}),
    );
    assert!(
        v["result"]["say"].as_str().unwrap().contains("title"),
        "{v}"
    );
    let t = screen(&mut app);
    assert!(app.sidebar_now(1) >= crate::ui::SIDEBAR_WIDE_AT);
    let rows: Vec<&str> = t.lines().collect();
    let at = rows
        .iter()
        .position(|l| l.contains("Refactor the botmesh"))
        .unwrap_or_else(|| panic!("{t}"));
    assert!(rows[at - 1].contains("api"), "{t}");
    // "make the tab list wider" / "narrower", and back to automatic.
    let before = app.panes[1].sidebar_w.unwrap();
    app.control_call("resize_tab_list", &json!({"width": "narrower"}));
    assert_eq!(app.panes[1].sidebar_w, Some(before - 8));
    app.control_call("resize_tab_list", &json!({"width": "wider"}));
    assert_eq!(app.panes[1].sidebar_w, Some(before));
    app.control_call("resize_tab_list", &json!({"width": "no details"}));
    assert_eq!(app.panes[1].sidebar_w, None);
    let t = screen(&mut app);
    assert!(!t.contains("Refactor the"), "{t}");
    // A collapsed list opens with Ctrl-a >.
    app.panes[1].sidebar_collapsed = true;
    cmd(&mut app, '>');
    assert!(!app.panes[1].sidebar_collapsed);
    let _ = std::fs::remove_dir_all(home);
}
