use crate::app::{App, Modal};
use crate::app_tabopts::TabAct;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, term, test_app};
use crate::hits::UiAction;
use crate::pane::{Activity, LaunchKind, PaneState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

const DAY: i64 = 86_400;

fn text(t: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let b = t.backend().buffer();
    let mut s = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            s.push_str(b[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn running(app: &mut App, s: usize, name: &str, sid: &str) -> usize {
    let dir = app.cfg.accounts[app.panes[s].account.unwrap()].work_dir();
    let t = app.panes[s].add_tab(dir);
    let tab = &mut app.panes[s].tabs[t];
    tab.custom_name = Some(name.into());
    tab.session_id = Some(sid.into());
    tab.state = PaneState::Running;
    tab.activity = Activity::Ready;
    t
}

fn transcript(app: &App, s: usize, t: usize, mtime: i64) {
    let a = app.panes[s].account.unwrap();
    let tab = &app.panes[s].tabs[t];
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&tab.cwd));
    std::fs::create_dir_all(&dir).unwrap();
    let f = std::fs::File::create(dir.join(format!("{}.jsonl", tab.session_id.clone().unwrap())))
        .unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime as u64))
        .unwrap();
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn names(app: &App, s: usize) -> Vec<String> {
    app.panes[s].tabs.iter().map(|t| t.name()).collect()
}

#[test]
fn archive_then_restore_resumes_the_exact_session() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-roundtrip");
    let t = running(&mut app, 1, "bills", "sess-billing-1");
    app.panes[1].tabs[t].accent = Some("#7a9".into());
    app.panes[1].tabs[t].pinned = true;
    app.panes[1].tabs[t].model = Some("opus".into());
    app.panes[1].tabs[t].browser = Some("Profile 3".into());
    let cwd = app.panes[1].tabs[t].cwd.clone();
    let before = app.panes[1].tabs.len();

    let e = app.archive_tab(1, t, "user", false).expect("archived");
    assert_eq!(app.panes[1].tabs.len(), before - 1, "the pane tab closed");
    assert_eq!(e.session_id.as_deref(), Some("sess-billing-1"));
    assert_eq!(e.account, app.cfg.accounts[1].name);
    assert_eq!(e.provider, "claude");
    assert_eq!(e.model.as_deref(), Some("opus"));
    assert!(e.pinned && e.archived_at > 0 && e.last_activity > 0);
    assert_eq!(
        e.browser.as_deref(),
        Some("Profile 3"),
        "the Browser binding is kept"
    );

    let on_disk = crate::archive::Store::load();
    assert_eq!(on_disk.entries.len(), 1);
    assert_eq!(on_disk.entries[0].cwd, cwd.to_string_lossy());
    assert!(
        app.closed.is_empty(),
        "archiving is not the Undo stack's business"
    );

    let msg = app.restore_archived(&e.id, None).expect("restored");
    assert!(msg.contains("bills"), "{msg}");
    let back = app.panes[1]
        .tabs
        .iter()
        .find(|x| x.custom_name.as_deref() == Some("bills"))
        .expect("tab is back on its account's pane");
    assert_eq!(back.kind, LaunchKind::Resume("sess-billing-1".into()));
    assert_eq!(back.cwd, cwd);
    assert!(back.pinned);
    assert_eq!(back.accent.as_deref(), Some("#7a9"));
    assert_eq!(
        back.browser.as_deref(),
        Some("Profile 3"),
        "restored with its browser"
    );
    assert!(app.archive.entries.is_empty());
    assert!(crate::archive::Store::load().entries.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn restoring_onto_another_account_keeps_the_provider() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-onto");
    let t = running(&mut app, 0, "api", "sess-api");
    let e = app.archive_tab(0, t, "user", false).unwrap();

    app.cfg.accounts[3].harness = "grok".into();
    let err = app.restore_archived(&e.id, Some(3)).unwrap_err();
    assert!(err.contains("claude conversation"), "{err}");
    assert_eq!(app.archive.entries.len(), 1, "still archived");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn delete_from_archive_forgets_only_the_entry() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-forget");
    let t = running(&mut app, 0, "api", "sess-api");
    transcript(&app, 0, t, now());
    let file = app.transcript_path(0, t).expect("transcript");
    let e = app.archive_tab(0, t, "user", false).unwrap();
    assert!(app.forget_archived(&e.id).is_some());
    assert!(app.archive.entries.is_empty());
    assert!(file.is_file(), "the session's own file stays");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn busy_unsent_and_direct_tabs_are_refused_without_force() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-busy");
    let work = running(&mut app, 0, "working", "s-w");
    app.panes[0].tabs[work].activity = Activity::Working;
    let typed = running(&mut app, 0, "typed", "s-t");
    app.panes[0].tabs[typed].parser.lock().unwrap().process(
        "hi\r\n\r\n╭────────────╮\r\n│ > half a thought │\r\n╰────────────╯\r\n".as_bytes(),
    );
    let fresh = 0;
    let e = app.archive_tab(0, work, "user", false).unwrap_err();
    assert!(e.contains("working"), "{e}");
    let e = app.archive_tab(0, typed, "user", false).unwrap_err();
    assert!(e.contains("unsent text"), "{e}");
    let e = app.archive_tab(0, fresh, "user", true).unwrap_err();
    assert!(
        e.contains("nothing to resume"),
        "force never overrides: {e}"
    );
    assert!(app.archive.entries.is_empty());

    app.panes[0].active = work;
    app.request_archive(0, work);
    assert!(
        matches!(app.modal, Modal::ConfirmArchive(_)),
        "{:?}",
        app.modal
    );
    assert!(app.archive_lines(app.panes[0].tabs[work].uid)[1].contains("working"));
    app.modal_ok();
    assert_eq!(app.archive.entries.len(), 1);

    let direct = running(&mut app, 0, "direct", "s-d");
    app.direct.on = Some(crate::app_direct::DirectMode {
        uid: app.panes[0].tabs[direct].uid,
        name: "direct".into(),
        since: chrono::Local::now(),
        log: vec![],
        last_act: Activity::Ready,
        last_reply: None,
        reply_wait: None,
        last_sent: None,
        confirm: None,
    });
    let e = app.archive_tab(0, direct, "assistant", true).unwrap_err();
    assert!(e.contains("directly"), "{e}");
    let _ = std::fs::remove_dir_all(home);
}

fn idle_setup(tag: &str) -> (App, std::path::PathBuf, i64) {
    let (mut app, home) = test_app(tag);
    let real = now();
    app.archive_clock = Some(real + 3 * DAY);
    let old = running(&mut app, 0, "old", "s-old");
    transcript(&app, 0, old, real);
    let recent = running(&mut app, 0, "recent", "s-recent");
    transcript(&app, 0, recent, real + 5 * DAY / 2);
    running(&mut app, 0, "nofile", "s-nofile");
    let w = running(&mut app, 0, "working", "s-work");
    app.panes[0].tabs[w].activity = Activity::Working;
    let pin = running(&mut app, 0, "pinned-old", "s-pin");
    transcript(&app, 0, pin, real);
    app.panes[0].tabs[pin].pinned = true;
    (app, home, real)
}

#[test]
fn idle_selection_uses_the_injected_clock() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (app, home, _) = idle_setup("arch-idle");
    let (ok, skipped) = app.archive_select(&json!({"idle_for_hours": 48})).unwrap();
    let ok_names: Vec<String> = ok
        .iter()
        .map(|&(s, t)| app.panes[s].tabs[t].name())
        .collect();
    assert!(ok_names.contains(&"old".to_string()), "{ok_names:?}");
    assert!(ok_names.contains(&"nofile".to_string()), "{ok_names:?}");
    assert!(
        !ok_names.contains(&"recent".to_string()),
        "half a day is not 2 days"
    );
    assert!(
        !ok_names.contains(&"working".to_string()),
        "a working tab is not idle"
    );
    assert!(!ok_names.contains(&"pinned-old".to_string()));
    let why: Vec<String> = skipped
        .iter()
        .map(|(s, t, w)| format!("{}: {w}", app.panes[*s].tabs[*t].name()))
        .collect();
    assert!(
        why.iter()
            .any(|w| w.starts_with("pinned-old") && w.contains("pinned")),
        "{why:?}"
    );

    let (ok2, _) = app
        .archive_select(&json!({"older_than": "2 days"}))
        .unwrap();
    assert_eq!(ok.len(), ok2.len());
    let (ok3, _) = app.archive_select(&json!({"idle_for_hours": 1})).unwrap();
    assert!(ok3.len() > ok.len(), "an hour catches the recent one too");

    let (ok4, _) = app
        .archive_select(&json!({"idle_for_hours": 48, "include_pinned": true}))
        .unwrap();
    assert_eq!(ok4.len(), ok.len() + 1);

    assert!(app.archive_select(&json!({})).is_err());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn filters_by_name_provider_and_busy_tabs_are_skipped_with_why() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (app, home, _) = idle_setup("arch-filter");
    let (ok, skipped) = app
        .archive_select(&json!({"account": 1, "tab": "all"}))
        .unwrap();
    assert!(!ok.is_empty());
    assert!(
        skipped
            .iter()
            .any(|(s, t, w)| app.panes[*s].tabs[*t].name() == "working" && w.contains("working")),
        "{skipped:?}"
    );
    let (ok, _) = app
        .archive_select(&json!({"name": "nofile", "idle_for_hours": 48}))
        .unwrap();
    assert_eq!(ok.len(), 1);
    let (ok, _) = app
        .archive_select(&json!({"provider": "grok", "tab": "all"}))
        .unwrap();
    assert!(ok.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn archive_tabs_dry_run_then_plan_confirm_and_list_restore() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _) = idle_setup("arch-tool");
    let before = app.panes[0].tabs.len();

    let v = app.control_call(
        "archive_tabs",
        &json!({"idle_for_hours": 48, "dry_run": true}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["result"]["dry_run"], json!(true));
    assert_eq!(v["result"]["would_archive"].as_array().unwrap().len(), 2);
    assert!(v["result"]["skipped"].as_array().unwrap()[0]["reason"]
        .as_str()
        .unwrap()
        .contains("pinned"));
    assert_eq!(app.panes[0].tabs.len(), before);

    let v = app.control_call("archive_tabs", &json!({"idle_for_hours": 48}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let q = v["question"].as_str().unwrap();
    assert!(
        q.contains("Archive 2 tabs") && q.contains("pinned-old"),
        "{q}"
    );
    assert_eq!(app.panes[0].tabs.len(), before, "nothing yet");
    let tok = v["token"].as_str().unwrap().to_string();

    app.assistant_turn += 1;
    app.assistant.last_user = "yes".into();
    let v = app.control_call("archive_tabs", &json!({"confirm_token": tok}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["result"]["archived"], json!(2));
    assert!(v["result"]["say"]
        .as_str()
        .unwrap()
        .contains("frees memory"));
    assert!(v["result"]["note"]
        .as_str()
        .unwrap()
        .contains("Memory is freed"));
    assert_eq!(app.panes[0].tabs.len(), before - 2);
    assert!(!names(&app, 0).contains(&"old".to_string()));
    assert!(names(&app, 0).contains(&"recent".to_string()));
    assert_eq!(app.archive.entries.len(), 2);

    let st = app.control_call("get_state", &json!({}));
    assert_eq!(st["result"]["archived"]["count"], json!(2), "{st}");
    assert_eq!(
        st["result"]["archived"]["recent"].as_array().unwrap().len(),
        2
    );

    let l = app.control_call("list_archived", &json!({"name": "old"}));
    assert_eq!(l["result"]["total"], json!(1), "{l}");
    let id = l["result"]["archived"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let l = app.control_call("list_archived", &json!({"provider": "grok"}));
    assert_eq!(l["result"]["total"], json!(0));
    let r = app.control_call("restore_archived", &json!({"id": id}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["result"]["restored"], json!(1));
    assert!(app.panes[0]
        .tabs
        .iter()
        .any(|t| t.kind == LaunchKind::Resume("s-old".into())));
    assert_eq!(app.archive.entries.len(), 1);
    let r = app.control_call("restore_archived", &json!({"id": "nope"}));
    assert!(r["error"].as_str().unwrap().contains("not in the archive"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn archive_all_refuses_when_everything_is_busy() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-allbusy");
    let w = running(&mut app, 0, "working", "s-w");
    app.panes[0].tabs[w].activity = Activity::Working;
    let id = crate::control::tab_id(app.panes[0].tabs[w].uid);
    let v = app.control_call("archive_tabs", &json!({"tab": id}));
    assert!(
        v["error"].as_str().unwrap().contains("working"),
        "names why, archives nothing: {v}"
    );

    let v = app.control_call("archive_tabs", &json!({"tab": id, "force": true}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let _ = std::fs::remove_dir_all(home);
}

fn ctrl_a(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

#[test]
fn ctrl_a_f_and_the_tab_options_archive_the_active_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-keys");
    let t = running(&mut app, 0, "keyed", "s-key");
    app.panes[0].active = t;
    app.focus = 0;
    ctrl_a(&mut app, 'f');
    assert_eq!(app.archive.entries.len(), 1);
    assert_eq!(app.archive.entries[0].label, "keyed");

    let rows = app.tab_actions(0, 0);
    let r = rows.iter().find(|r| r.act == TabAct::Archive).unwrap();
    assert!(r.disabled.is_some(), "the empty placeholder tab");
    let t2 = running(&mut app, 0, "second", "s-2");
    let r = app
        .tab_actions(0, t2)
        .into_iter()
        .find(|r| r.act == TabAct::Archive)
        .unwrap();
    assert!(r.disabled.is_none());
    assert!(app
        .panel_rows(0, t2)
        .iter()
        .any(|r| r.act == TabAct::Archive));
    app.tab_act_run(0, t2, TabAct::Archive);
    assert_eq!(app.archive.entries.len(), 2);

    assert!(crate::ui::HELP.iter().any(|(k, _)| *k == "Ctrl-a f"));
    let items = crate::menus::entries(&app, crate::menus::MenuId::Tabs);
    assert!(items.iter().any(|e| e.label.starts_with("Archive tab")));
    assert!(items.iter().any(|e| e.label.starts_with("Archived (2)")));
    assert_eq!(
        crate::menus::entries(&app, crate::menus::MenuId::Archived).len(),
        2
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_archived_group_lists_and_hit_tests_in_the_tab_list() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-ui");
    let mut tm = term();
    let a = running(&mut app, 0, "bills", "s-bill");
    let b = running(&mut app, 0, "sockets", "s-ws");
    running(&mut app, 1, "other-account", "s-other");
    app.archive_tab(0, b, "user", false).unwrap();
    app.archive_tab(0, a, "user", false).unwrap();
    let other = app.panes[1].tabs.len() - 1;
    app.archive_tab(1, other, "user", false).unwrap();
    draw(&mut app, &mut tm);

    let s = text(&tm);
    assert!(s.contains("Archived (2)"), "{s}");
    assert!(!s.contains("bills"), "folded:\n{s}");

    click(&mut app, &UiAction::ArchivedHeader(0));
    draw(&mut app, &mut tm);
    let s = text(&tm);
    assert!(s.contains("bills") && s.contains("sockets"), "{s}");
    assert!(!s.contains("other-account"), "that one is under account 2");
    let id = app.archive_for_pane(0)[0].id.clone();
    let r = app
        .last_hits
        .find(&UiAction::ArchivedRow(0, id.clone()))
        .expect("row has a hit region")
        .rect;
    let head = app
        .last_hits
        .find(&UiAction::ArchivedHeader(0))
        .unwrap()
        .rect;
    assert_eq!(r.height, 1);
    assert!(r.y > head.y, "entries sit under the header");

    click(&mut app, &UiAction::ArchivedRow(0, id.clone()));
    assert_eq!(app.archive_sel.as_deref(), Some(id.as_str()));
    assert_eq!(app.archive.entries.len(), 3);
    click(&mut app, &UiAction::ArchivedRow(0, id.clone()));
    assert_eq!(app.archive.entries.len(), 2, "restored by the double click");
    assert!(app.panes[0]
        .tabs
        .iter()
        .any(|t| matches!(&t.kind, LaunchKind::Resume(s) if s == "s-bill" || s == "s-ws")));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn right_click_menu_restores_or_deletes_and_search_reaches_the_archive() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-menu");
    let mut tm = term();
    let a = running(&mut app, 0, "bills", "s-bill");
    let b = running(&mut app, 0, "sockets", "s-ws");
    app.archive_tab(0, b, "user", false).unwrap();
    app.archive_tab(0, a, "user", false).unwrap();

    app.panes[0].filter = "bill".into();
    draw(&mut app, &mut tm);
    let s = text(&tm);
    assert!(s.contains("Archived (1)") && s.contains("bills"), "{s}");
    assert!(!s.contains("sockets"));

    app.tab_search = Some(0);
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.panes[0]
        .tabs
        .iter()
        .any(|t| t.kind == LaunchKind::Resume("s-bill".into())));
    assert_eq!(app.archive.entries.len(), 1);

    app.panes[0].filter.clear();
    let id = app.archive.entries[0].id.clone();
    app.archive_collapsed = false;
    app.open_archive_menu(id.clone());
    draw(&mut app, &mut tm);
    let s = text(&tm);
    assert!(
        s.contains("Restore") && s.contains("Delete from archive"),
        "{s}"
    );
    click(&mut app, &UiAction::ArchMenuItem(1));
    assert!(app.archive.entries.is_empty(), "forgotten");
    assert!(matches!(app.modal, Modal::None));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_reopened_closed_tab_keeps_its_browser_binding() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("closed-browser");
    let t = running(&mut app, 1, "web", "sess-web-1");
    app.panes[1].tabs[t].browser = Some("Profile 2".into());
    let c = app.snapshot_tab(1, t, 0).expect("snapshot");
    assert_eq!(c.browser.as_deref(), Some("Profile 2"));

    let back: crate::closed::ClosedTab =
        serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back, c);
    app.panes[1].tabs.remove(t);
    app.panes[1].active = 0;
    app.reopen_closed_tab(back).expect("reopened");
    let tab = app.panes[1]
        .tabs
        .iter()
        .find(|x| x.custom_name.as_deref() == Some("web"))
        .expect("reopened tab");
    assert_eq!(tab.browser.as_deref(), Some("Profile 2"));

    let mut bad = c.clone();
    bad.browser = Some("../../etc".into());
    bad.name = Some("bad".into());
    bad.session_id = Some("sess-bad".into());
    app.reopen_closed_tab(bad).expect("reopened");
    let tab = app.panes[1]
        .tabs
        .iter()
        .find(|x| x.custom_name.as_deref() == Some("bad"))
        .expect("reopened bad");
    assert_eq!(tab.browser, None);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_tab_archived_from_the_web_app_says_so() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("arch-web");
    let real = now();
    app.archive_clock = Some(real + 3 * DAY);
    let t = running(&mut app, 0, "old", "s-old");
    transcript(&app, 0, t, real);
    let id = crate::control::tab_id(app.panes[0].tabs[t].uid);
    let v = app.control_call("archive_tabs", &json!({"tab": id, "_client": "web"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let l = app.control_call("list_archived", &json!({}));
    assert!(l.to_string().contains(r#""by":"web""#), "{l}");
    let _ = std::fs::remove_dir_all(home);
}
