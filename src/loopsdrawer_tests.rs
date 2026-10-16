//! The loops drawer (app_loopsdrawer, ui_loopsdrawer): the button and
//! its count, opening and closing (the PTY resized once each way), the
//! list with next runs, the buttons, Claude's and Grok's jobs parsed from
//! fixtures, the empty states and a small terminal. No real claude or
//! grok runs; homes are test homes.

use crate::app_loops::LoopRow;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;
use crate::loops::{Confidence, Kind, Loop, Scanner};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::json;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home =
        std::env::temp_dir().join(format!("godterm-loopsdrawer-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.cfg.grok_bin = Some(crate::test_stub::true_bin());
    for a in &mut app.accounts {
        a.login.source = Some(CredSource::Keychain);
    }
    app.cfg.accounts[1].harness = "grok".into();
    (app, home)
}

fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    let a = buf.area;
    let mut out = String::new();
    for y in a.y..a.y + a.height {
        for x in a.x..a.x + a.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn cron(id: &str, expr: &str, prompt: &str) -> Loop {
    Loop {
        id: id.into(),
        kind: Kind::Cron,
        cron: Some(expr.into()),
        prompt: prompt.into(),
        recurring: true,
        durable: false,
        created: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        last_fire: Some(chrono::Utc::now() - chrono::Duration::minutes(3)),
        fires: 4,
        wake_at: None,
        deleted: false,
        confidence: Confidence::High,
        every: None,
    }
}

/// Two loops on pane 1's tab.
fn with_loops(app: &mut crate::app::App) -> u64 {
    let uid = app.panes[0].tabs[0].uid;
    app.loops = vec![
        LoopRow {
            uid,
            account: Some(0),
            tab_name: app.panes[0].tabs[0].name(),
            lp: cron("a1b2c3d4", "*/5 * * * *", "check the deploy"),
        },
        LoopRow {
            uid,
            account: Some(0),
            tab_name: app.panes[0].tabs[0].name(),
            lp: cron("e5f6a7b8", "0 9 * * *", "summarize the night"),
        },
    ];
    uid
}

fn draw(app: &mut crate::app::App, term: &mut Terminal<TestBackend>) -> String {
    term.draw(|f| crate::ui::draw(f, app)).unwrap();
    buffer_text(term.backend().buffer())
}

fn region(app: &crate::app::App, a: &UiAction) -> Option<ratatui::layout::Rect> {
    app.last_hits
        .regions
        .iter()
        .rev()
        .find(|r| &r.action == a)
        .map(|r| r.rect)
}

#[test]
fn the_button_counts_the_tabs_loops() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("count");
    with_loops(&mut app);
    let mut term = Terminal::new(TestBackend::new(240, 50)).unwrap();
    let text = draw(&mut app, &mut term);
    assert!(text.contains("⟳ loops (2)"), "{text}");
    assert!(text.contains("⟳ loops (0)"), "the other panes: {text}");
    // At 0 it is dim.
    let r = region(&app, &UiAction::LoopsDrawer(1)).expect("pane 2's button");
    let buf = term.backend().buffer();
    assert_eq!(buf[(r.x + 3, r.y)].fg, crate::theme::FAINT);
    let r0 = region(&app, &UiAction::LoopsDrawer(0)).unwrap();
    assert_eq!(buf[(r0.x + 3, r0.y)].fg, crate::theme::FG);
    // It sits on the model bar's row, at its right end.
    let m = region(&app, &UiAction::TabModel(0, 0)).unwrap();
    assert_eq!(r0.y, m.y);
    assert!(r0.x > m.x);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_drawer_opens_and_closes_with_one_resize_each_way() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("open");
    with_loops(&mut app);
    let mut term = Terminal::new(TestBackend::new(240, 50)).unwrap();
    draw(&mut app, &mut term);
    let closed = app.panes[0].tabs[0].pty_size();
    let term0 = app.term_rects[0];
    // A click on the button opens it: the terminal shrinks by its rows.
    let b = region(&app, &UiAction::LoopsDrawer(0)).unwrap();
    app.click(
        UiAction::LoopsDrawer(0),
        false,
        &crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: b.x,
            row: b.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    );
    assert!(app.loops_drawer_open(0));
    let text = draw(&mut app, &mut term);
    let open = app.panes[0].tabs[0].pty_size();
    assert_eq!(
        open.0,
        closed.0 - crate::app_loopsdrawer::DEFAULT_ROWS,
        "one resize to make room"
    );
    assert_eq!(
        app.term_rects[0].height,
        term0.height - crate::app_loopsdrawer::DEFAULT_ROWS
    );
    assert!(text.contains("⟳ loops on"), "{text}");
    // Frames after that leave the PTY alone.
    draw(&mut app, &mut term);
    draw(&mut app, &mut term);
    assert_eq!(app.panes[0].tabs[0].pty_size(), open);
    // Esc closes it (it has the keys right after opening), and the
    // terminal gets its rows back.
    let esc = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    );
    app.on_key(esc);
    assert!(!app.loops_drawer_open(0));
    draw(&mut app, &mut term);
    assert_eq!(app.panes[0].tabs[0].pty_size(), closed);
    // Ctrl-a ; toggles it; a key for the tab gives the keys back, so Esc
    // then goes to the tab, not the drawer.
    app.focus = 0;
    app.on_command(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char(';'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(app.loops_drawer_open(0) && app.loops_drawer_focus == Some(0));
    app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.loops_drawer_focus, None);
    app.on_key(esc);
    assert!(app.loops_drawer_open(0), "Esc went to the tab");
    // The × closes it.
    draw(&mut app, &mut term);
    let rows = app
        .last_hits
        .regions
        .iter()
        .filter(|r| r.action == UiAction::LoopsDrawer(0))
        .count();
    assert_eq!(rows, 2, "the row's button and the drawer's ×");
    // Dragging its top edge up makes it taller.
    let edge = region(&app, &UiAction::LoopsDrawerEdge(0)).unwrap();
    app.drag_loops_drawer(0, edge.y - 3);
    assert_eq!(
        app.loops_drawer[&0],
        crate::app_loopsdrawer::DEFAULT_ROWS + 3
    );
    app.toggle_loops_drawer(0);
    assert!(!app.loops_drawer_open(0));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn it_lists_the_loops_with_their_next_run_and_the_buttons_work() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("list");
    let uid = with_loops(&mut app);
    app.toggle_loops_drawer(0);
    let mut term = Terminal::new(TestBackend::new(240, 50)).unwrap();
    let text = draw(&mut app, &mut term);
    let lines: Vec<&str> = text.lines().collect();
    let k = lines
        .iter()
        .position(|l| l.contains("check the deploy"))
        .unwrap_or_else(|| panic!("{text}"));
    let (line, facts) = (lines[k], lines[k + 1]);
    assert!(line.contains("Claude every 5 minutes"), "{line}");
    assert!(line.contains("Run now") && line.contains("Cancel") && line.contains("Copy"));
    assert!(facts.contains("next in "), "{facts}");
    assert!(facts.contains("last 3m ago, 4 runs"), "{facts}");
    assert!(facts.contains("armed"), "{facts}");
    assert!(text.contains("daily at 09:00"), "{text}");
    // The countdown: "in m:ss" under an hour.
    let now = chrono::Utc::now();
    assert_eq!(
        crate::app_loopsdrawer::countdown(now + chrono::Duration::seconds(192), now),
        "in 3:12"
    );
    // Run now: the prompt goes to the tab as a request (once it is idle).
    assert!(region(
        &app,
        &UiAction::LoopAction(0, 0, crate::app_loopsdrawer::OP_RUN)
    )
    .is_some());
    app.loop_action(0, 0, crate::app_loopsdrawer::OP_RUN);
    assert!(
        app.deliveries
            .iter()
            .any(|d| d.uid == uid && d.text == "check the deploy"),
        "run now"
    );
    // Cancel asks first, then asks the tab (typed in once it is idle).
    app.loop_action(0, 0, crate::app_loopsdrawer::OP_CANCEL);
    assert!(app.stop_reqs.is_empty());
    let text = draw(&mut app, &mut term);
    assert!(text.contains("Cancel?"), "{text}");
    app.loop_action(0, 0, crate::app_loopsdrawer::OP_CANCEL);
    assert!(app
        .stop_reqs
        .iter()
        .any(|q| q.uid == uid && q.ids == ["a1b2c3d4"]));
    let text = draw(&mut app, &mut term);
    assert!(text.contains("cancel asked"), "{text}");
    // Copy: its details.
    app.loop_action(0, 1, crate::app_loopsdrawer::OP_COPY);
    let c = app.last_copied.clone().unwrap_or_default();
    assert!(
        c.contains("e5f6a7b8") && c.contains("summarize the night"),
        "{c}"
    );
    // The assistant opens it on a tab.
    app.toggle_loops_drawer(0);
    let v = app.control_call(
        "show_tab_loops",
        &json!({"tab": crate::control::tab_id(uid)}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(app.loops_drawer_open(0));
    assert_eq!(v["result"]["loops"].as_array().unwrap().len(), 2);
    assert!(v["result"]["loops"][0]["next"]
        .as_str()
        .unwrap()
        .starts_with("next in"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn claude_and_grok_jobs_are_read_from_their_sessions() {
    // Claude Code: CronCreate and its result in the transcript, a fire.
    let mut sc = Scanner::default();
    for l in include_str!("../tests/fixtures/claude_loops.jsonl").lines() {
        sc.entry(&serde_json::from_str(l).unwrap());
    }
    let l = &sc.loops()[0];
    assert_eq!(
        (l.id.as_str(), l.cron.as_deref(), l.fires),
        ("a1b2c3d4", Some("*/5 * * * *"), 1)
    );
    assert_eq!(l.cadence(), "every 5 minutes");
    // Grok Build: scheduler_create (and an update) and scheduler_delete
    // in its chat history.
    let mut sc = Scanner::default();
    for l in include_str!("../tests/fixtures/grok_scheduler.jsonl").lines() {
        sc.entry(&serde_json::from_str(l).unwrap());
    }
    let live: Vec<&Loop> = sc.loops().iter().filter(|l| !l.deleted).collect();
    assert_eq!(live.len(), 1, "{:?}", sc.loops());
    assert_eq!(live[0].id, "019ff7d0a2b4");
    assert_eq!(live[0].cadence(), "every 16 minutes", "the update");
    assert_eq!(live[0].prompt, "look at the build and report, quietly");
    assert!(live[0].next_fire(chrono::Utc::now()).is_none());
    assert!(sc
        .loops()
        .iter()
        .any(|l| l.id == "019ff7d0ffff" && l.deleted));
    // Grok's cancel uses its own tool.
    assert!(
        crate::app_loops::grok_stop_prompt(&["019ff7d0a2b4".into()]).contains("scheduler_delete")
    );
}

#[test]
fn empty_states_say_what_was_looked_at() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("empty");
    app.toggle_loops_drawer(0);
    app.toggle_loops_drawer(1);
    let mut term = Terminal::new(TestBackend::new(240, 50)).unwrap();
    let text = draw(&mut app, &mut term);
    assert!(text.contains("The tab is not running"), "{text}");
    assert!(text.contains("Claude: no /loop or cron jobs"), "{text}");
    assert!(text.contains("Grok: no scheduled tasks"), "{text}");
    // A Grok row in the drawer is marked Grok, with Grok's interval.
    let uid = app.panes[1].tabs[0].uid;
    let mut l = cron("019ff7d0a2b4", "", "look at the build");
    l.cron = None;
    l.every = Some("every 10 minutes".into());
    l.last_fire = None;
    l.fires = 0;
    app.loops = vec![LoopRow {
        uid,
        account: Some(1),
        tab_name: "g".into(),
        lp: l,
    }];
    let text = draw(&mut app, &mut term);
    let lines: Vec<&str> = text.lines().collect();
    let k = lines
        .iter()
        .position(|x| x.contains("look at the build"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(lines[k].contains("Grok every 10 minutes"), "{}", lines[k]);
    assert!(
        lines[k + 1].contains("not run yet") && lines[k + 1].contains("not recorded by Grok"),
        "{}",
        lines[k + 1]
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_small_terminal_caps_the_drawer() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("small");
    with_loops(&mut app);
    app.focus = 0;
    app.zoom = true;
    app.toggle_loops_drawer(0);
    app.loops_drawer.insert(0, crate::app_loopsdrawer::MAX_ROWS);
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let text = draw(&mut app, &mut term);
    let t = app.term_rects[0];
    assert!(
        t.height >= crate::app_loopsdrawer::MIN_TERM,
        "{t:?}\n{text}"
    );
    let d = app
        .loops_drawer_geo
        .borrow()
        .get(&0)
        .copied()
        .expect("drawn");
    assert!(
        d.height >= crate::app_loopsdrawer::MIN_ROWS
            && d.height <= crate::app_loopsdrawer::MAX_ROWS,
        "{d:?}"
    );
    assert_eq!(d.y, t.y + t.height, "right under the terminal");
    assert!(text.contains("check the deploy"), "{text}");
    // Too small for both: no drawer, the terminal keeps its rows.
    let mut tiny = Terminal::new(TestBackend::new(80, 12)).unwrap();
    draw(&mut app, &mut tiny);
    assert!(app.loops_drawer_geo.borrow().get(&0).is_none());
    let _ = std::fs::remove_dir_all(&home);
}
