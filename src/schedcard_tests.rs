//! The Loops & schedules detail card and the section's layout
//! (app_sched.rs, ui_schedcard.rs, ui_schedules.rs): the cron in words,
//! what the card says for a session loop and a managed job, its buttons
//! routing (Manage with GodTerm adopts and cancels the session copy,
//! delete and stop ask twice), duplicate loops, grouping by tab, the
//! height based row count and scrolling. Nothing real runs: tabs are
//! stubs, homes are test homes, no speaker.

use crate::app::{App, Modal};
use crate::app_jobs::{OP_ADOPT, OP_DELETE, OP_OPEN, OP_PAUSE, OP_RUN, OP_STOP};
use crate::app_sched::{
    clamp_scroll, cron_words, plan, sched_lines, scroll_words, SchedLine, MANAGE_NOTE, OP_CLOSE,
};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, term};
use crate::hits::UiAction;
use crate::jobs_tests::{
    add, deliveries_for, find, loop_row, running, screen, session_loop, setup,
};
use crate::loops::{describe_cron, Kind, Scanner};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

fn kev(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn flash(app: &App) -> String {
    app.flash.as_ref().map(|f| f.0.clone()).unwrap_or_default()
}

const LONG: &str = "Freebots team quick check: read the overnight logs, list every failing bot with its last error, and post a short summary for the morning standup";

/// A panel with one tab ("hacker sim") running a 7 2,4,6 * * * cron.
fn card_setup(tag: &str) -> (App, std::path::PathBuf, usize, String) {
    let (mut app, home) = setup(tag);
    app.assistant.show = true;
    let t = running(&mut app, 0, "hacker sim", "sess-h");
    let row = loop_row(&app, 0, t, session_loop("ab12cd34", "7 2,4,6 * * *", LONG));
    let key = format!("{}:ab12cd34", app.panes[0].tabs[t].uid);
    app.loops = vec![row];
    (app, home, t, key)
}

#[test]
fn crons_in_plain_words() {
    assert_eq!(
        describe_cron("7 2,4,6 * * *"),
        "at 2:07, 4:07 and 6:07 every day"
    );
    assert_eq!(describe_cron("*/15 * * * *"), "every 15 minutes");
    assert_eq!(describe_cron("5 * * * *"), "every hour at :05");
    assert_eq!(describe_cron("0 */2 * * *"), "every 2 hours at :00");
    assert_eq!(describe_cron("0 9 * * 1-5"), "at 9:00 on weekdays");
    assert_eq!(describe_cron("0 10 * * 0,6"), "at 10:00 on weekends");
    assert_eq!(
        describe_cron("0 9 * * 1,4"),
        "at 9:00 on Mondays and Thursdays"
    );
    assert_eq!(
        describe_cron("30 8 1 * *"),
        "at 8:30 on day 1 of every month"
    );
    assert_eq!(describe_cron("40 4 1 10 *"), "at 4:40 on Oct 1");
    assert_eq!(
        describe_cron("*/5 * * * 1-5"),
        "every 5 minutes, on weekdays"
    );
    assert_eq!(describe_cron("bad"), "bad");
    assert_eq!(
        cron_words("7 2,4,6 * * *", true),
        "Schedule: cron 7 2,4,6 * * *, i.e. at 2:07, 4:07 and 6:07 every day"
    );
    assert!(cron_words("12 6 30 9 *", false).starts_with("One time: cron 12 6 30 9 *"));
}

#[test]
fn a_session_loop_card_says_what_it_is_who_runs_it_and_when() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t, key) = card_setup("card-session");
    let uid = app.panes[0].tabs[t].uid;
    let c = app.sched_card(&key).expect("a card");
    let txt = c.text();
    for want in [
        "session",
        "schedule",
        LONG,
        "Kind: Schedule: cron 7 2,4,6 * * *, i.e. at 2:07, 4:07 and 6:07 every day",
        &format!("Owner: Runs inside the session in tab t{uid} hacker sim"),
        "It stops when that session ends",
        "Next run: ",
        "Last run: Ran ",
        "4 runs so far",
        "session crons expire by themselves 7 days after they are made",
        "Id: ab12cd34",
    ] {
        assert!(txt.contains(want), "{want:?} in:\n{txt}");
    }
    let labels: Vec<&str> = c.buttons.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Manage with GodTerm",
            "Run now",
            "Stop",
            "Open tab",
            "Close"
        ]
    );
    assert_eq!(c.note.as_deref(), Some(MANAGE_NOTE));
    assert!(!txt.contains('\u{2014}'), "no em dashes");
    // Drawn: a click on the row opens it, the whole prompt wraps.
    let mut tm = term();
    draw(&mut app, &mut tm);
    assert!(find(&app, &UiAction::SchedRow(key.clone())).is_some());
    let s = screen(&tm);
    let row = s
        .lines()
        .find(|l| l.contains("Freebots team"))
        .expect("the row is drawn");
    assert!(!row.contains(" + "), "no bare + on a session row: {row}");
    assert!(row.contains(" details "), "a labeled chip instead: {row}");
    click(&mut app, &UiAction::SchedRow(key.clone()));
    assert_eq!(app.modal, Modal::SchedCard(key.clone()));
    draw(&mut app, &mut tm);
    let s = screen(&tm);
    for want in [
        "loop details",
        "Manage with GodTerm",
        "Run now",
        "Open tab",
        "morning standup",
        "at 2:07, 4:07 and 6:07",
    ] {
        assert!(s.contains(want), "{want:?} on screen:\n{s}");
    }
    for op in [OP_ADOPT, OP_RUN, OP_STOP, OP_OPEN, OP_CLOSE] {
        assert!(
            find(&app, &UiAction::SchedCardAct(op)).is_some(),
            "button {op}"
        );
    }
    // Esc closes it.
    app.on_key(key_esc());
    assert_eq!(app.modal, Modal::None);
    let _ = std::fs::remove_dir_all(home);
}

fn key_esc() -> KeyEvent {
    kev(KeyCode::Esc)
}

#[test]
fn manage_with_godterm_adopts_and_stops_the_session_copy() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t, key) = card_setup("card-adopt");
    let uid = app.panes[0].tabs[t].uid;
    let mut tm = term();
    draw(&mut app, &mut tm);
    click(&mut app, &UiAction::SchedRow(key.clone()));
    draw(&mut app, &mut tm);
    click(&mut app, &UiAction::SchedCardAct(OP_ADOPT));
    assert_eq!(app.jobs.jobs.len(), 1);
    let j = app.jobs.jobs[0].clone();
    assert_eq!(j.prompt, LONG);
    assert_eq!(j.cron.as_deref(), Some("7 2,4,6 * * *"));
    assert_eq!(j.adopted_from.as_deref(), Some("ab12cd34"));
    // cancel=true: the tab is asked to CronDelete its own copy.
    assert!(
        app.stop_reqs
            .iter()
            .any(|q| q.uid == uid && q.ids == vec!["ab12cd34".to_string()]),
        "{:?}",
        app.stop_reqs
    );
    let f = flash(&app);
    assert!(f.starts_with("Now managed by GodTerm: "), "{f}");
    assert!(f.ends_with("The session's own copy was stopped."), "{f}");
    // The card now shows the managed job.
    assert_eq!(app.modal, Modal::SchedCard(j.id.clone()));
    let txt = app.sched_card(&j.id).unwrap().text();
    assert!(txt.contains("Managed by GodTerm"), "{txt}");
    assert!(
        txt.contains("taken over from the session loop ab12cd34"),
        "{txt}"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_managed_job_card_runs_pauses_and_deletes_after_a_confirm() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("card-managed");
    app.assistant.show = true;
    let t = running(&mut app, 0, "billing", "sess-b");
    let id = add(&mut app, 0, t, "morning status", "status of billing?", 30);
    let mut tm = term();
    draw(&mut app, &mut tm);
    click(&mut app, &UiAction::SchedRow(id.clone()));
    let c = app.sched_card(&id).unwrap();
    let labels: Vec<&str> = c.buttons.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels, ["Run now", "Pause", "Delete", "Open tab", "Close"]);
    let txt = c.text();
    for want in [
        "managed",
        "Kind: Recurring every 30 minutes",
        "Owner: Managed by GodTerm",
        "Last run: Not run yet",
        "Ends: No end date",
        &format!("Id: job {id}"),
    ] {
        assert!(txt.contains(want), "{want:?} in:\n{txt}");
    }
    draw(&mut app, &mut tm);
    assert!(screen(&tm).contains("job details"));
    // Run now.
    click(&mut app, &UiAction::SchedCardAct(OP_RUN));
    assert_eq!(deliveries_for(&app, "status of billing?").len(), 1);
    // Pause (by key), then the card offers Resume and a paused badge.
    app.on_key(kev(KeyCode::Char('p')));
    assert!(!app.jobs.find(&id).unwrap().enabled);
    let c = app.sched_card(&id).unwrap();
    assert!(c.buttons.iter().any(|b| b.label == "Resume"));
    assert!(c.badges.iter().any(|b| b.0 == "paused"));
    // Delete asks first: the label changes and the job stays.
    draw(&mut app, &mut tm);
    click(&mut app, &UiAction::SchedCardAct(OP_DELETE));
    assert!(app.jobs.find(&id).is_some());
    assert!(app
        .sched_card(&id)
        .unwrap()
        .buttons
        .iter()
        .any(|b| b.label == "Click again to delete"));
    draw(&mut app, &mut tm);
    assert!(screen(&tm).contains("Click again to delete"));
    click(&mut app, &UiAction::SchedCardAct(OP_DELETE));
    assert!(app.jobs.find(&id).is_none());
    assert_eq!(app.modal, Modal::None, "the card closes with its job");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_failed_managed_job_has_a_failed_badge() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("card-failed");
    let t = running(&mut app, 0, "billing", "sess-b");
    let id = add(&mut app, 0, t, "nightly", "check", 60);
    app.jobs.find_mut(&id).unwrap().last_result = Some(crate::jobs::RunResult {
        state: "failed".into(),
        detail: "the tab would not start".into(),
        at: crate::jobs::now_ts() - 60,
    });
    let c = app.sched_card(&id).unwrap();
    assert!(c
        .badges
        .iter()
        .any(|(b, col)| b == "last run failed" && *col == crate::theme::MUTED_RED));
    assert!(c.text().contains("failed"));
    assert!(c.text().contains("the tab would not start"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn stop_and_run_now_on_a_session_loop() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, t, key) = card_setup("card-stop");
    let uid = app.panes[0].tabs[t].uid;
    app.open_sched_card(&key);
    app.sched_card_act(OP_RUN);
    assert_eq!(
        deliveries_for(&app, LONG).len(),
        1,
        "run now sends its prompt"
    );
    // Stop asks twice.
    app.sched_card_act(OP_STOP);
    assert!(app.stop_reqs.is_empty());
    assert!(app
        .sched_card(&key)
        .unwrap()
        .buttons
        .iter()
        .any(|b| b.label == "Click again to stop"));
    app.sched_card_act(OP_STOP);
    assert!(app.stop_reqs.iter().any(|q| q.uid == uid));
    assert_eq!(app.modal, Modal::None);
    assert!(app.jobs.jobs.is_empty(), "stop does not adopt");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_dynamic_loop_has_no_manage_button_and_says_why() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("card-wakeup");
    let t = running(&mut app, 0, "builder", "sess-w");
    let mut lp = session_loop("wakeup", "", "/loop keep building");
    lp.kind = Kind::Wakeup;
    lp.cron = None;
    lp.wake_at = Some(chrono::Utc::now() + chrono::Duration::minutes(20));
    app.loops = vec![loop_row(&app, 0, t, lp)];
    let key = format!("{}:wakeup", app.panes[0].tabs[t].uid);
    let c = app.sched_card(&key).unwrap();
    assert!(c.badges.iter().any(|b| b.0 == "loop"));
    assert!(c.text().contains("Kind: Loop: wakes itself dynamically"));
    assert!(!c.buttons.iter().any(|b| b.op == OP_ADOPT));
    assert!(c.note.unwrap().contains("no fixed schedule"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_same_loop_found_twice_is_one_row_and_twins_show_their_ids() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // A transcript that repeats its CronCreate (a resumed session).
    let mut sc = Scanner::default();
    for _ in 0..2 {
        sc.entry(&serde_json::json!({"type":"assistant","timestamp":"2026-10-14T10:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"CronCreate","input":{"cron":"7 2,4,6 * * *","prompt":"Freebots team q","recurring":true}}]}}));
        sc.entry(&serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Scheduled recurring job ab12cd34 (7 2,4,6 * * *). Session-only."}]}}));
    }
    assert_eq!(sc.loops().len(), 1);
    // Rows found twice (two scans) are deduped by (tab, id).
    let (mut app, home) = setup("card-dupes");
    app.assistant.show = true;
    let t = running(&mut app, 0, "hacker sim", "sess-h");
    let a = loop_row(
        &app,
        0,
        t,
        session_loop("ab12cd34", "7 2,4,6 * * *", "Freebots team q"),
    );
    let v = crate::app_loops::dedupe_loops(vec![a.clone(), a.clone()]);
    assert_eq!(v.len(), 1);
    app.on_loops(vec![a.clone(), a.clone()]);
    assert_eq!(app.loops.len(), 1);
    // Two real crons with the same schedule and prompt: ids tell them apart.
    let b = loop_row(
        &app,
        0,
        t,
        session_loop("ef56ab78", "7 2,4,6 * * *", "Freebots team q"),
    );
    app.loops = vec![a, b];
    let rows = app.sched_rows();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].label.ends_with("#ab12cd34"), "{}", rows[0].label);
    assert!(rows[1].label.ends_with("#ef56ab78"), "{}", rows[1].label);
    let key = format!("{}:ab12cd34", app.panes[0].tabs[t].uid);
    let txt = app.sched_card(&key).unwrap().text();
    assert!(
        txt.contains("another loop with the same schedule and prompt (ef56ab78)"),
        "{txt}"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn rows_group_by_tab_and_a_group_folds() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("card-group");
    app.assistant.show = true;
    let a = running(&mut app, 0, "hacker sim", "sess-a");
    let b = running(&mut app, 0, "notes", "sess-n");
    let id = add(&mut app, 0, a, "digest", "digest please", 60);
    app.loops = vec![
        loop_row(&app, 0, a, session_loop("aa000001", "*/15 * * * *", "one")),
        loop_row(&app, 0, a, session_loop("aa000002", "0 9 * * *", "two")),
        loop_row(&app, 0, b, session_loop("bb000001", "0 9 * * *", "lonely")),
    ];
    // A job with no tab at all goes under GodTerm.
    let mut lone = crate::jobs_tests::job_for(&app, 0, a, "orphan", "x", 30);
    lone.target = crate::jobs::Target::default();
    app.jobs.add(lone);
    let (rows, lines) = app.sched_view();
    let ua = app.panes[0].tabs[a].uid;
    let group = format!("t{ua}");
    let g0 = lines
        .iter()
        .position(|l| matches!(l, SchedLine::Group { .. }))
        .expect("a group header");
    match &lines[g0] {
        SchedLine::Group {
            key,
            label,
            count,
            folded,
        } => {
            assert_eq!(key, &group);
            assert_eq!(label, &format!("t{ua} hacker sim"));
            assert_eq!(count, "1 job · 2 loops");
            assert!(!folded);
        }
        l => panic!("a group header: {l:?}"),
    }
    // Its job first, then its loops, indented.
    let members: Vec<(&str, bool)> = lines[g0 + 1..g0 + 4]
        .iter()
        .map(|l| match l {
            SchedLine::Row { idx, indent } => (rows[*idx].id.as_str(), *indent),
            _ => panic!("{l:?}"),
        })
        .collect();
    assert_eq!(
        members,
        [(id.as_str(), true), ("aa000001", true), ("aa000002", true)]
    );
    // The others are plain rows (one each).
    assert_eq!(lines.len(), 6, "{lines:?}");
    let plain: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < g0 || *i > g0 + 3)
        .map(|(_, l)| l)
        .map(|l| match l {
            SchedLine::Row { idx, indent: false } => rows[*idx].group.as_str(),
            _ => panic!("{l:?}"),
        })
        .collect();
    assert!(plain.contains(&"godterm"), "{plain:?}");
    // Drawn with aligned columns: the tab column and the header.
    let mut tm = term();
    draw(&mut app, &mut tm);
    let s = screen(&tm);
    assert!(
        s.contains(&format!("t{ua} hacker sim · 1 job · 2 loops")),
        "{s}"
    );
    // Folding by a click.
    click(&mut app, &UiAction::SchedGroup(group.clone()));
    let lines = sched_lines(&app.sched_rows(), &app.sched_ui.folded);
    assert_eq!(lines.len(), 3);
    assert!(lines
        .iter()
        .any(|l| matches!(l, SchedLine::Group { folded: true, .. })));
    draw(&mut app, &mut tm);
    click(&mut app, &UiAction::SchedGroup(group));
    assert_eq!(app.sched_view().1.len(), 6);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_row_count_follows_the_panel_height() {
    // 40% of the panel, at least 3, within the room left.
    assert_eq!(plan(true, 30, 50, 40), (20, 21));
    assert_eq!(plan(true, 30, 10, 40), (4, 5));
    assert_eq!(plan(true, 30, 5, 40), (3, 4), "at least 3");
    assert_eq!(plan(true, 2, 50, 40), (2, 3));
    assert_eq!(plan(true, 30, 50, 5), (4, 5), "the room left wins");
    assert_eq!(plan(false, 30, 50, 40), (0, 1));
    assert_eq!(plan(true, 0, 50, 40), (0, 1));
    assert_eq!(plan(true, 5, 50, 1), (0, 1));
    assert_eq!(plan(true, 5, 50, 0), (0, 0));
}

#[test]
fn the_section_scrolls_within_bounds_by_wheel_and_keys() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(clamp_scroll(100, 14, 7), 7);
    assert_eq!(clamp_scroll(3, 5, 7), 0);
    assert_eq!(scroll_words(2, 14, 7).as_deref(), Some("3 to 9 of 14"));
    assert_eq!(scroll_words(0, 5, 7), None);
    let (mut app, home) = setup("card-scroll");
    app.assistant.show = true;
    for k in 0..14 {
        let t = running(&mut app, 0, &format!("tab {k}"), &format!("sess-{k}"));
        let lp = session_loop(&format!("cc{k:06}"), "*/15 * * * *", &format!("loop {k}"));
        let r = loop_row(&app, 0, t, lp);
        app.loops.push(r);
    }
    let mut tm = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
    draw(&mut app, &mut tm);
    let (shown, total) = app.sched_ui.view.get();
    assert_eq!(total, 14);
    assert!((3..14).contains(&shown), "{shown}");
    let s = screen(&tm);
    assert!(s.contains(&format!("1 to {shown} of 14")), "{s}");
    // The wheel over a row scrolls, never past the end.
    let first = app.sched_rows()[0].key.clone();
    let r = find(&app, &UiAction::SchedRow(first)).unwrap();
    for _ in 0..30 {
        app.on_mouse_event(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: r.x + 2,
            row: r.y,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert_eq!(app.sched_ui.scroll, 14 - shown);
    draw(&mut app, &mut tm);
    assert!(screen(&tm).contains(&format!("{} to 14 of 14", 14 - shown + 1)));
    for _ in 0..30 {
        app.sched_scroll_by(-1);
    }
    assert_eq!(app.sched_ui.scroll, 0);
    // Keys after a click in the section: Down moves and scrolls along,
    // Enter opens the card.
    draw(&mut app, &mut tm);
    let first = app.sched_rows()[0].key.clone();
    click(&mut app, &UiAction::SchedRow(first));
    app.on_key(key_esc());
    assert_eq!(app.modal, Modal::None);
    assert!(app.sched_ui.focus);
    assert_eq!(app.sched_ui.sel, 0);
    for _ in 0..20 {
        app.on_key(kev(KeyCode::Down));
    }
    assert_eq!(app.sched_ui.sel, 13);
    assert_eq!(app.sched_ui.scroll, 14 - shown);
    app.on_key(kev(KeyCode::Enter));
    assert!(matches!(app.modal, Modal::SchedCard(_)));
    app.on_key(key_esc());
    app.on_key(key_esc());
    assert!(!app.sched_ui.focus, "Esc lets the keys go");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_loops_view_opens_the_same_card() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _t, key) = card_setup("card-view");
    app.view = crate::app::View::Loops;
    app.on_key(kev(KeyCode::Enter));
    assert_eq!(app.modal, Modal::SchedCard(key.clone()));
    let mut tm = term();
    draw(&mut app, &mut tm);
    assert!(screen(&tm).contains("Manage with GodTerm"));
    assert!(find(&app, &UiAction::SchedCardAct(OP_PAUSE)).is_none());
    app.sched_card_act(OP_CLOSE);
    assert_eq!(app.modal, Modal::None);
    let _ = std::fs::remove_dir_all(home);
}
