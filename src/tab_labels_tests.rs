//! Tab row labels: each from its real state, in precedence, "finished"
//! only from a real Working to Ready turn the user did not see, cleared
//! when they look, kept across a save and restore, in get_state, and
//! drawn at the end of the row (shortened when the list is narrow).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::{Activity, PaneState};
use crate::tab_labels::TabLabel;

fn test_app(tag: &str) -> (App, PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-labels-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let cfg = crate::config::Config {
        claude_bin: Some(crate::test_stub::true_bin()),
        notifications: false,
        ..Default::default()
    };
    let mut app = App::new(cfg, tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    (app, home)
}

fn show(app: &mut App, slot: usize, tab: usize, screen: &str) {
    let t = &mut app.panes[slot].tabs[tab];
    t.parser
        .lock()
        .unwrap()
        .process(format!("\x1b[2J\x1b[H{screen}").as_bytes());
}

fn idle_all(app: &mut App) {
    for p in &mut app.panes {
        for t in &mut p.tabs {
            t.state = PaneState::Idle;
        }
    }
}

/// A real turn in tab `t` of pane `s`: Working held 5 s, then Ready.
fn real_turn(app: &mut App, s: usize, t: usize) {
    show(app, s, t, "esc to interrupt");
    app.tick();
    assert_eq!(app.panes[s].tabs[t].activity, Activity::Working);
    app.panes[s].tabs[t].activity_since = Instant::now() - Duration::from_secs(5);
    show(app, s, t, "? for shortcuts");
    app.tick();
}

fn pending(plan: serde_json::Value) -> crate::control::Pending {
    crate::control::Pending {
        token: "tok".into(),
        tool: "close_tabs".into(),
        plan,
        summary: "Close it?".into(),
        turn: 0,
        at: Instant::now(),
    }
}

#[test]
fn each_label_comes_from_its_real_state_in_precedence() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("each");
    let uid = app.panes[1].tabs[0].uid;
    app.focus = 0;
    assert_eq!(app.tab_label(1, 0), None);
    // Queued: a delivery waiting for it.
    app.deliver(uid, "hello");
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Queued));
    // Finished beats queued.
    app.panes[1].tabs[0].finished = true;
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Finished));
    // Error: claude exited with a code.
    app.panes[1].tabs[0].state = PaneState::Exited(Some(1));
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Error));
    // A clean exit is no error.
    app.panes[1].tabs[0].state = PaneState::Exited(Some(0));
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Finished));
    app.panes[1].tabs[0].state = PaneState::Failed("no claude".into());
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Error));
    // Limit: the failover scan saw it on a running tab.
    app.panes[1].tabs[0].state = PaneState::Running;
    show(&mut app, 1, 0, "You've hit your limit · resets 5pm");
    app.failover_scan();
    assert!(app.limit_hit.contains(&uid));
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Limit));
    // Confirm: an answerable question names it (uids or uid).
    app.pending_confirms
        .push(pending(serde_json::json!({"uids": [uid]})));
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Confirm));
    app.pending_confirms[0].plan = serde_json::json!({"uid": uid, "key": "x"});
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Confirm));
    // Another tab's question is not this one's.
    app.pending_confirms[0].plan = serde_json::json!({"uids": [uid + 1000]});
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Limit));
    app.pending_confirms[0].plan = serde_json::json!({"uids": [uid]});
    // Blocked beats everything.
    app.panes[1].tabs[0].set_activity(Activity::Permission);
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Blocked));
    // Answered: down the list again.
    app.panes[1].tabs[0].set_activity(Activity::Ready);
    app.pending_confirms.clear();
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Limit));
    show(&mut app, 1, 0, "? for shortcuts");
    app.failover_scan();
    assert_eq!(app.tab_label(1, 0), Some(TabLabel::Finished));
    idle_all(&mut app);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn finished_only_from_a_real_turn_and_cleared_when_seen() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("finished");
    let (s, t) = (1, 0);
    app.focus = 0;
    let tab = &mut app.panes[s].tabs[t];
    tab.state = PaneState::Running;
    tab.set_activity(Activity::Starting);
    // A start or restore (Starting to Ready): nothing.
    show(&mut app, s, t, "? for shortcuts");
    app.tick();
    assert_eq!(app.panes[s].tabs[t].activity, Activity::Ready);
    assert!(!app.panes[s].tabs[t].finished);
    // A spinner flicker: nothing.
    show(&mut app, s, t, "esc to interrupt");
    app.tick();
    show(&mut app, s, t, "? for shortcuts");
    app.tick();
    assert!(!app.panes[s].tabs[t].finished);
    // A real turn out of view: finished.
    real_turn(&mut app, s, t);
    assert!(app.panes[s].tabs[t].finished);
    assert_eq!(app.tab_label(s, t), Some(TabLabel::Finished));
    // Working again clears it.
    show(&mut app, s, t, "esc to interrupt");
    app.tick();
    assert!(!app.panes[s].tabs[t].finished);
    app.panes[s].tabs[t].activity_since = Instant::now() - Duration::from_secs(5);
    show(&mut app, s, t, "? for shortcuts");
    app.tick();
    assert!(app.panes[s].tabs[t].finished);
    // Focusing it (a click on its row goes through jump_to) clears it.
    app.jump_to(s, t);
    assert!(!app.panes[s].tabs[t].finished);
    assert_eq!(app.tab_label(s, t), None);
    // A turn that ends while the user watches it is never marked.
    real_turn(&mut app, s, t);
    assert!(!app.panes[s].tabs[t].finished);
    // Marked, then the user switches to it by keys: the next tick clears.
    app.focus = 0;
    real_turn(&mut app, s, t);
    assert!(app.panes[s].tabs[t].finished);
    app.focus = s;
    app.tick();
    assert!(!app.panes[s].tabs[t].finished);
    idle_all(&mut app);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn finished_survives_a_save_and_restore_and_the_rest_do_not() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("persist");
    app.panes[1].tabs[0].finished = true;
    let uid = app.panes[1].tabs[0].uid;
    let st = app.snapshot_state();
    let json = serde_json::to_string(&st).unwrap();
    assert!(json.contains("\"finished\":true"), "{json}");
    let back: crate::state::AppState = serde_json::from_str(&json).unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut fresh = App::new(app.cfg.clone(), tx);
    fresh.restore(back);
    let (s, t) = fresh.find_tab(uid).expect("restored");
    assert!(fresh.panes[s].tabs[t].finished);
    fresh.focus = if s == 0 { 1 } else { 0 };
    assert_eq!(fresh.tab_label(s, t), Some(TabLabel::Finished));
    // Unmarked tabs save no field at all.
    app.panes[1].tabs[0].finished = false;
    let json = serde_json::to_string(&app.snapshot_state()).unwrap();
    assert!(!json.contains("finished"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn get_state_and_the_turn_state_carry_the_label() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("getstate");
    app.focus = 0;
    app.panes[1].tabs[0].finished = true;
    let uid = app.panes[1].tabs[0].uid;
    let v = app.state_json();
    let tab = v["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == crate::control::tab_id(uid))
        .unwrap()
        .clone();
    assert_eq!(tab["label"], "finished");
    let other = v["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] != crate::control::tab_id(uid))
        .unwrap()
        .clone();
    assert!(other.get("label").is_none());
    assert!(app.state_preamble().contains("[finished]"));
    let _ = std::fs::remove_dir_all(&home);
}

/// The text of row `y` of the buffer.
fn row_text(t: &ratatui::Terminal<ratatui::backend::TestBackend>, y: u16) -> String {
    let b = t.backend().buffer();
    (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
}

fn find_row(
    t: &ratatui::Terminal<ratatui::backend::TestBackend>,
    needle: &str,
) -> Option<(u16, String)> {
    let h = t.backend().buffer().area.height;
    (0..h)
        .map(|y| (y, row_text(t, y)))
        .find(|(_, r)| r.contains(needle))
}

fn one_pane(app: &mut App) {
    app.zoom = true;
    app.focus = 0;
    let cwd = app.panes[0].cur().cwd.clone();
    for _ in 0..3 {
        app.panes[0].add_tab(cwd.clone());
    }
    app.panes[0].tabs[1].custom_name = Some("hyperdemo".into());
    app.panes[0].tabs[2].custom_name = Some("waiter".into());
    app.panes[0].active = 0;
    app.panes[0].tabs[1].finished = true;
    app.panes[0].tabs[2].set_activity(Activity::Permission);
}

#[test]
fn the_label_is_drawn_in_the_row_wide_and_narrow() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("render");
    one_pane(&mut app);
    // Wide list: the full words, on the tab's own row.
    app.panes[0].sidebar_w = Some(40);
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    crate::confirm_click_tests::draw(&mut app, &mut t);
    let (y, row) = find_row(&t, "hyperdemo").expect("the row");
    assert!(row.contains(" finished "), "{row}");
    let (_, row) = find_row(&t, "waiter").expect("the row");
    assert!(row.contains(" blocked "), "{row}");
    // The row is tinted (not inverted): its background is not the plain one.
    let b = t.backend().buffer();
    let bg = (0..b.area.width)
        .map(|x| b[(x, y)].bg)
        .find(|c| *c != ratatui::style::Color::Reset);
    assert!(bg.is_some(), "a tinted row");
    // Narrow list: the short form.
    app.panes[0].sidebar_w = Some(crate::ui::SIDEBAR_MIN);
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    crate::confirm_click_tests::draw(&mut app, &mut t);
    let dump = (0..12)
        .map(|y| row_text(&t, y))
        .collect::<Vec<_>>()
        .join("\n");
    let (_, row) = find_row(&t, "hyp…").expect(&dump);
    assert!(row.contains(" done ") && !row.contains("finished"), "{row}");
    let (_, row) = find_row(&t, "waiter").expect("the row");
    assert!(row.contains(" blk ") || row.contains('●'), "{row}");
    // The list folded to numbers: the header counts them.
    app.panes[0].sidebar_collapsed = true;
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    crate::confirm_click_tests::draw(&mut app, &mut t);
    assert!(
        find_row(&t, "1 blocked · 1 finished").is_some(),
        "{}",
        (0..4)
            .map(|y| row_text(&t, y))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn clicking_a_finished_tab_focuses_it_and_clears_the_label() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("click");
    one_pane(&mut app);
    app.panes[0].sidebar_w = Some(40);
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    crate::confirm_click_tests::draw(&mut app, &mut t);
    crate::confirm_click_tests::click(&mut app, &crate::hits::UiAction::SelectTab(0, 1));
    assert_eq!((app.focus, app.panes[0].active), (0, 1));
    assert!(!app.panes[0].tabs[1].finished);
    crate::confirm_click_tests::draw(&mut app, &mut t);
    let (_, row) = find_row(&t, "hyperdemo").expect("the row");
    assert!(!row.contains("finished"), "{row}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn pills_shorten_with_the_room() {
    use crate::tab_labels::{fit, Fit};
    assert_eq!(fit(TabLabel::Finished, 30, 6), Fit::Full);
    assert_eq!(fit(TabLabel::Finished, 13, 6), Fit::Short);
    assert_eq!(fit(TabLabel::Finished, 8, 6), Fit::Dot);
    assert_eq!(TabLabel::Finished.pill(Fit::Short), " done ");
    assert_eq!(TabLabel::Blocked.pill(Fit::Short), " blk ");
    assert_eq!(TabLabel::Error.pill(Fit::Dot), "●");
    let s = crate::tab_labels::summary(&[(TabLabel::Finished, 2), (TabLabel::Blocked, 1)], false);
    assert_eq!(s, "2 finished · 1 blocked");
}

#[test]
fn handed_to_background_agents_is_not_finished_until_they_are_done() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("background");
    let (s, t) = (1, 0);
    app.focus = 0;
    let tab = &mut app.panes[s].tabs[t];
    tab.state = PaneState::Running;
    tab.set_activity(Activity::Starting);
    show(&mut app, s, t, "? for shortcuts");
    app.tick();
    // A real turn that ends with agents still running in the background.
    show(&mut app, s, t, "esc to interrupt");
    app.tick();
    app.panes[s].tabs[t].activity_since = Instant::now() - Duration::from_secs(5);
    show(&mut app, s, t, ">\n  2 background tasks · ? for shortcuts");
    app.tick();
    assert_eq!(app.panes[s].tabs[t].activity, Activity::Ready);
    assert!(
        !app.panes[s].tabs[t].finished,
        "not finished: work still runs"
    );
    let finished_said = |app: &App| {
        app.told
            .borrow()
            .ring
            .iter()
            .any(|r| r.kind == "attention" && r.text.ends_with("finished"))
    };
    assert!(!finished_said(&app), "no finished announcement yet");
    assert_eq!(app.tab_label(s, t), Some(TabLabel::Background));
    // Still running a while later: still not finished.
    app.tick();
    assert_eq!(app.tab_label(s, t), Some(TabLabel::Background));
    // The background work is done: finished now, and announced.
    show(&mut app, s, t, ">\n  ? for shortcuts");
    app.tick();
    assert!(app.panes[s].tabs[t].finished);
    assert_eq!(app.tab_label(s, t), Some(TabLabel::Finished));
    assert!(finished_said(&app), "announced once the work is done");
    idle_all(&mut app);
    let _ = std::fs::remove_dir_all(&home);
}
