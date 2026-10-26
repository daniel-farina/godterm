//! GodTerm's own jobs end to end (jobs.rs, app_jobs.rs): the scheduler
//! firing with an injected clock, missed runs and their policy, a target
//! that was closed or archived woken first, busy tabs, failures said, the
//! assistant's tools and their confirmation flow, archiving a tab that
//! owns jobs, the Loops & schedules section (render, collapse, hit
//! tests), and how GodTerm starts (panel open, mic off unless it was on).
//! Nothing real runs: tabs are stubs and homes are test homes.

use crate::app::App;
use crate::app_jobs::{SchedKind, OP_ADOPT, OP_DELETE, OP_PAUSE, OP_RUN};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, term, test_app};
use crate::control::Stage;
use crate::hits::UiAction;
use crate::jobs::{CatchUp, Job, Target};
use crate::loops::{Confidence, Kind, Loop};
use crate::pane::{Activity, PaneState};
use serde_json::{json, Value};

const T0: i64 = 1_800_000_000;

/// A running, idle tab on pane `s` with a session; returns its index.
pub(crate) fn running(app: &mut App, s: usize, name: &str, sid: &str) -> usize {
    let dir = app.cfg.accounts[app.panes[s].account.unwrap()].work_dir();
    let t = app.panes[s].add_tab(dir);
    let tab = &mut app.panes[s].tabs[t];
    tab.custom_name = Some(name.into());
    tab.session_id = Some(sid.into());
    tab.state = PaneState::Running;
    tab.activity = Activity::Ready;
    t
}

pub(crate) fn job_for(app: &App, s: usize, t: usize, name: &str, prompt: &str, every: u32) -> Job {
    Job {
        id: String::new(),
        name: name.into(),
        prompt: prompt.into(),
        cron: None,
        every_min: Some(every),
        at: None,
        target: app.job_target_of(s, t),
        created_by: "assistant".into(),
        enabled: true,
        catch_up: CatchUp::Once,
        created_at: T0,
        last_run: None,
        last_result: None,
        next_run: Some(T0 + i64::from(every) * 60),
        end_at: None,
        warned_end: false,
        runs: 0,
        finished: None,
        adopted_from: None,
        machine: None,
    }
}

pub(crate) fn setup(tag: &str) -> (App, std::path::PathBuf) {
    let (mut app, home) = test_app(tag);
    app.job_clock = Some(T0);
    (app, home)
}

pub(crate) fn add(
    app: &mut App,
    s: usize,
    t: usize,
    name: &str,
    prompt: &str,
    every: u32,
) -> String {
    let j = job_for(app, s, t, name, prompt, every);
    app.jobs.add(j)
}

pub(crate) fn deliveries_for(app: &App, text: &str) -> Vec<(u64, u64)> {
    app.deliveries
        .iter()
        .filter(|d| d.text == text)
        .map(|d| (d.id, d.uid))
        .collect()
}

#[test]
fn the_scheduler_fires_a_due_job_with_an_injected_clock() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-fire");
    let t = running(&mut app, 0, "billing", "sess-b");
    let uid = app.panes[0].tabs[t].uid;
    let id = add(&mut app, 0, t, "status", "status of the build?", 15);
    // Not due yet.
    app.job_clock = Some(T0 + 14 * 60);
    app.jobs_tick();
    assert!(deliveries_for(&app, "status of the build?").is_empty());
    // Due: delivered to that tab, through the ordinary deliveries.
    app.job_clock = Some(T0 + 15 * 60 + 2);
    app.jobs_tick();
    let d = deliveries_for(&app, "status of the build?");
    assert_eq!(d.len(), 1, "one delivery");
    assert_eq!(d[0].1, uid);
    let j = app.jobs.find(&id).unwrap();
    assert_eq!(j.runs, 1);
    assert_eq!(j.last_run, Some(T0 + 15 * 60 + 2));
    assert_eq!(j.last_result.as_ref().unwrap().state, "sent");
    assert_eq!(
        j.next_run,
        Some(T0 + 30 * 60),
        "the rhythm, not the lateness"
    );
    // Persisted: a fresh load sees the run.
    let on_disk = crate::jobs::Store::load();
    assert_eq!(on_disk.find(&id).unwrap().runs, 1);
    // Not again before the next time.
    app.job_clock = Some(T0 + 20 * 60);
    app.jobs_tick();
    assert_eq!(deliveries_for(&app, "status of the build?").len(), 1);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_busy_tab_gets_it_queued_after_its_work() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-busy");
    let t = running(&mut app, 0, "builder", "sess-w");
    app.panes[0].tabs[t].activity = Activity::Working;
    let id = add(&mut app, 0, t, "poll", "how far along?", 5);
    app.job_clock = Some(T0 + 5 * 60);
    app.jobs_tick();
    let d = app
        .deliveries
        .iter()
        .find(|d| d.text == "how far along?")
        .expect("queued");
    assert!(
        matches!(d.stage, Stage::Queued),
        "waits for the tab: {:?}",
        d.stage
    );
    let r = app.jobs.find(&id).unwrap().last_result.clone().unwrap();
    assert!(r.detail.contains("queued after"), "{}", r.detail);
    // A second period passes with the first still waiting: no pile up.
    app.job_clock = Some(T0 + 10 * 60);
    app.jobs_tick();
    assert_eq!(deliveries_for(&app, "how far along?").len(), 1);
    let r = app.jobs.find(&id).unwrap().last_result.clone().unwrap();
    assert_eq!(r.state, "skipped");
    assert!(r.detail.contains("previous run"), "{}", r.detail);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn missed_runs_follow_the_catch_up_policy() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-missed");
    let t = running(&mut app, 0, "nightly", "sess-n");
    let once = add(&mut app, 0, t, "once", "run once please", 60);
    let skip = add(&mut app, 0, t, "skip", "skip me", 60);
    app.jobs.find_mut(&skip).unwrap().catch_up = CatchUp::Skip;
    // GodTerm was off for ten hours.
    app.job_clock = Some(T0 + 10 * 3600 + 30);
    app.jobs_tick();
    assert_eq!(
        deliveries_for(&app, "run once please").len(),
        1,
        "ten missed hours, one run"
    );
    assert!(deliveries_for(&app, "skip me").is_empty());
    let o = app.jobs.find(&once).unwrap();
    assert!(
        o.last_result
            .as_ref()
            .unwrap()
            .detail
            .contains("missed run"),
        "{:?}",
        o.last_result
    );
    let s = app.jobs.find(&skip).unwrap();
    assert_eq!(s.last_result.as_ref().unwrap().state, "skipped");
    assert_eq!(s.runs, 0);
    assert_eq!(s.next_run, Some(T0 + 11 * 3600), "back on its rhythm");
    assert_eq!(o.next_run, s.next_run);
    // The skip was said to the assistant.
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("missed a run") && e.text.contains("skipped")));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_job_survives_a_restart_and_catches_up_on_the_first_tick() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-restart");
    let t = running(&mut app, 0, "keep", "sess-k");
    add(&mut app, 0, t, "daily", "daily report", 1440);
    app.jobs.save().unwrap();
    drop(app);
    // A new process: the store is read from disk; two days later.
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    let mut next = App::new(cfg, tx);
    assert_eq!(next.jobs.jobs.len(), 1);
    next.job_clock = Some(T0 + 2 * 86_400 + 50);
    next.jobs_tick();
    // The tab is not open any more: it was woken (reopened on its session).
    assert_eq!(
        next.deliveries
            .iter()
            .filter(|d| d.text == "daily report")
            .count(),
        1
    );
    assert_eq!(next.jobs.jobs[0].runs, 1);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_closed_target_is_reopened_on_its_session_then_the_prompt_is_delivered() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-closed");
    let t = running(&mut app, 1, "ops", "sess-ops");
    let id = add(&mut app, 1, t, "ops check", "any alerts?", 30);
    app.close_tab_at(1, t);
    assert!(app.panes[1]
        .tabs
        .iter()
        .all(|x| x.session_id.as_deref() != Some("sess-ops")));
    let shown = (app.focus, app.view);
    app.job_clock = Some(T0 + 30 * 60 + 1);
    app.jobs_tick();
    let d = deliveries_for(&app, "any alerts?");
    assert_eq!(d.len(), 1, "delivered after the wake");
    let (s, i) = app.find_tab(d[0].1).expect("the reopened tab");
    assert_eq!(s, 1, "on its own account's pane");
    assert_eq!(
        app.panes[s].tabs[i].kind,
        crate::pane::LaunchKind::Resume("sess-ops".into())
    );
    assert_eq!((app.focus, app.view), shown, "the screen was not taken");
    let r = app.jobs.find(&id).unwrap().last_result.clone().unwrap();
    assert!(r.detail.contains("reopened"), "{}", r.detail);
    // The target follows the new tab.
    assert_eq!(app.jobs.find(&id).unwrap().target.uid, Some(d[0].1));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn an_archived_target_is_restored_from_the_archive_then_delivered() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-archived");
    let t = running(&mut app, 0, "old", "sess-old");
    let id = add(&mut app, 0, t, "weekly", "weekly summary", 60);
    // Archiving keeps the job and says it will wake the tab.
    let uid = app.panes[0].tabs[t].uid;
    let note = app.jobs_wake_note(uid).unwrap();
    assert!(
        note.contains("1 job (weekly) will wake it when it runs"),
        "{note}"
    );
    app.archive_now_flash(0, t, false);
    let flashed = app.flash.as_ref().map(|f| f.0.clone()).unwrap_or_default();
    assert!(flashed.contains("will wake it"), "{flashed}");
    assert_eq!(app.archive.entries.len(), 1);
    assert_eq!(app.jobs.jobs.len(), 1, "the job stays");
    assert_eq!(
        app.job_target_state(&app.jobs.find(&id).unwrap().target),
        "archived"
    );
    app.job_clock = Some(T0 + 61 * 60);
    app.jobs_tick();
    assert!(app.archive.entries.is_empty(), "restored");
    let d = deliveries_for(&app, "weekly summary");
    assert_eq!(d.len(), 1);
    assert!(app.find_tab(d[0].1).is_some());
    let r = app.jobs.find(&id).unwrap().last_result.clone().unwrap();
    assert!(
        r.detail.contains("restored from the archive"),
        "{}",
        r.detail
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_run_that_cannot_open_its_tab_is_a_failed_receipt_that_is_said() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-fail");
    // A target with no session and no tab: nothing to resume.
    let mut j = job_for(&app, 0, 0, "lost", "hello?", 5);
    j.target = Target {
        uid: Some(99_999),
        session_id: None,
        provider: "claude".into(),
        account: app.cfg.accounts[0].name.clone(),
        cwd: "/nowhere".into(),
        label: "gone".into(),
    };
    let id = app.jobs.add(j);
    app.job_clock = Some(T0 + 5 * 60);
    app.jobs_tick();
    let r = app.jobs.find(&id).unwrap().last_result.clone().unwrap();
    assert_eq!(r.state, "failed");
    assert!(r.detail.contains("nothing to resume"), "{}", r.detail);
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("\"lost\"") && e.text.contains("failed")));
    // It runs again at its next time; a dropped delivery fails the run too.
    let t = running(&mut app, 0, "x", "sess-x");
    let j2 = add(&mut app, 0, t, "drops", "will drop", 5);
    app.job_clock = Some(T0 + 10 * 60);
    app.jobs_tick();
    let did = app
        .deliveries
        .iter_mut()
        .find(|d| d.text == "will drop")
        .unwrap();
    did.stage = Stage::Failed("the tab never became ready".into());
    app.jobs_tick();
    let r = app.jobs.find(&j2).unwrap().last_result.clone().unwrap();
    assert_eq!(r.state, "failed", "{r:?}");
    assert!(r.detail.contains("never became ready"), "{}", r.detail);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_receipt_that_ends_done_is_the_result() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-done");
    let t = running(&mut app, 0, "x", "sess-x");
    let id = add(&mut app, 0, t, "ok", "do it", 5);
    app.job_clock = Some(T0 + 5 * 60);
    app.jobs_tick();
    let d = app
        .deliveries
        .iter_mut()
        .find(|d| d.text == "do it")
        .unwrap();
    d.stage = Stage::Delivered;
    d.rc.receipt = crate::receipts::Receipt::Done;
    app.jobs_tick();
    let r = app.jobs.find(&id).unwrap().last_result.clone().unwrap();
    assert_eq!(r.state, "done");
    assert!(app.job_runs.is_empty(), "no longer followed");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_one_time_job_runs_once_and_an_end_date_is_said() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-end");
    let t = running(&mut app, 0, "x", "sess-x");
    let once = add(&mut app, 0, t, "once", "just once", 5);
    {
        let j = app.jobs.find_mut(&once).unwrap();
        j.every_min = None;
        j.at = Some(T0 + 600);
        j.next_run = Some(T0 + 600);
    }
    let ends = add(&mut app, 0, t, "ends", "until friday", 60);
    app.jobs.find_mut(&ends).unwrap().end_at = Some(T0 + 3 * 86_400);
    app.job_clock = Some(T0 + 601);
    app.jobs_tick();
    assert_eq!(deliveries_for(&app, "just once").len(), 1);
    assert!(!app.jobs.find(&once).unwrap().active());
    // Two days and 20 hours in: the warning, once.
    app.job_clock = Some(T0 + 3 * 86_400 - 4 * 3600);
    app.jobs_tick();
    let warned = |app: &App| {
        app.assistant
            .log
            .iter()
            .filter(|e| e.text.contains("\"ends\"") && e.text.contains("will stop then"))
            .count()
    };
    assert_eq!(warned(&app), 1);
    app.job_clock = Some(T0 + 3 * 86_400 - 3 * 3600);
    app.jobs_tick();
    assert_eq!(warned(&app), 1, "once");
    // Past the end: stopped and said, not silently dropped.
    app.job_clock = Some(T0 + 3 * 86_400 + 5);
    app.jobs_tick();
    let j = app.jobs.find(&ends).unwrap();
    assert_eq!(j.finished.as_deref(), Some("ended"));
    assert!(app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("reached its end date")));
    let _ = std::fs::remove_dir_all(home);
}

// ---- the assistant's tools ----

fn call(app: &mut App, tool: &str, args: Value) -> Value {
    app.control_call(tool, &args)
}

/// The user's yes in a later turn.
fn yes(app: &mut App) {
    app.assistant_turn += 1;
    app.assistant.last_user = "yes".into();
}

#[test]
fn create_job_asks_once_then_creates_and_the_state_lists_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-create");
    let t = running(&mut app, 0, "billing", "sess-b");
    let tid = crate::control::tab_id(app.panes[0].tabs[t].uid);
    // Needs a tab, a prompt and a schedule.
    let v = call(
        &mut app,
        "create_job",
        json!({"prompt": "x", "every": "5 minutes"}),
    );
    assert_eq!(v["ok"], json!(false), "{v}");
    let v = call(&mut app, "create_job", json!({"tab": tid, "prompt": "x"}));
    assert!(v["error"].as_str().unwrap().contains("schedule"), "{v}");
    let v = call(
        &mut app,
        "create_job",
        json!({"tab": tid, "prompt": "x", "cron": "every morning"}),
    );
    assert!(v["error"].as_str().unwrap().contains("cron"), "{v}");
    let args = json!({"tab": tid, "name": "morning status", "prompt": "status of billing?",
        "cron": "0 9 * * 1-5", "catch_up": "skip", "until": "in 30 days"});
    let v = call(&mut app, "create_job", args);
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let q = v["question"].as_str().unwrap();
    assert!(
        q.contains("morning status") && q.contains("billing") && q.contains("until"),
        "{q}"
    );
    assert!(app.jobs.jobs.is_empty(), "nothing yet");
    let tok = v["token"].as_str().unwrap().to_string();
    yes(&mut app);
    let v = call(&mut app, "create_job", json!({"confirm_token": tok}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(app.jobs.jobs.len(), 1);
    let j = &app.jobs.jobs[0];
    assert_eq!(j.cron.as_deref(), Some("0 9 * * 1-5"));
    assert_eq!(j.catch_up, CatchUp::Skip);
    assert_eq!(j.created_by, "assistant");
    assert_eq!(j.end_at, Some(T0 + 30 * 86_400));
    assert!(j.next_run.unwrap() > T0);
    assert_eq!(j.target.session_id.as_deref(), Some("sess-b"));
    assert!(v["result"]["say"]
        .as_str()
        .unwrap()
        .contains("survives restarts"));
    // Persisted.
    assert_eq!(crate::jobs::Store::load().jobs.len(), 1);
    // get_state has the summary; list_jobs the detail.
    let st = call(&mut app, "get_state", json!({}));
    assert_eq!(st["result"]["jobs"]["count"], json!(1), "{st}");
    assert_eq!(
        st["result"]["jobs"]["jobs"][0]["name"],
        json!("morning status")
    );
    let l = call(&mut app, "list_jobs", json!({}));
    let job = &l["result"]["jobs"][0];
    assert_eq!(job["tab"]["is"], json!("open"));
    assert_eq!(job["schedule"], json!("0 9 * * 1-5"), "{l}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn update_pause_needs_no_question_but_changing_the_prompt_does() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-update");
    let t = running(&mut app, 0, "billing", "sess-b");
    let id = add(&mut app, 0, t, "poll", "old prompt", 10);
    let v = call(
        &mut app,
        "update_job",
        json!({"job": "poll", "action": "pause"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(!app.jobs.find(&id).unwrap().enabled);
    // Paused jobs do not fire.
    app.job_clock = Some(T0 + 3600);
    app.jobs_tick();
    assert!(deliveries_for(&app, "old prompt").is_empty());
    // Resume: the next regular time, nothing missed while paused.
    let v = call(
        &mut app,
        "update_job",
        json!({"job": id, "action": "resume"}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(app.jobs.find(&id).unwrap().next_run, Some(T0 + 3600 + 600));
    // A new prompt asks.
    let v = call(
        &mut app,
        "update_job",
        json!({"job": id, "prompt": "new prompt"}),
    );
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    assert_eq!(app.jobs.find(&id).unwrap().prompt, "old prompt");
    let tok = v["token"].as_str().unwrap().to_string();
    yes(&mut app);
    let v = call(&mut app, "update_job", json!({"confirm_token": tok}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(app.jobs.find(&id).unwrap().prompt, "new prompt");
    // A new schedule too; "nothing to change" is an error.
    let v = call(
        &mut app,
        "update_job",
        json!({"job": id, "every": "2 hours"}),
    );
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let v = call(&mut app, "update_job", json!({"job": id}));
    assert!(
        v["error"].as_str().unwrap().contains("nothing to change"),
        "{v}"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn delete_asks_and_run_job_now_sends_at_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-delete");
    let t = running(&mut app, 0, "billing", "sess-b");
    let id = add(&mut app, 0, t, "poll", "poll it", 10);
    let before = app.jobs.find(&id).unwrap().next_run;
    let v = call(&mut app, "run_job_now", json!({"job": "poll"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(deliveries_for(&app, "poll it").len(), 1);
    assert_eq!(
        app.jobs.find(&id).unwrap().next_run,
        before,
        "its schedule is unchanged"
    );
    let v = call(&mut app, "delete_job", json!({"job": id}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    assert!(v["question"]
        .as_str()
        .unwrap()
        .contains("Delete the job \"poll\""));
    assert_eq!(app.jobs.jobs.len(), 1);
    let tok = v["token"].as_str().unwrap().to_string();
    yes(&mut app);
    let v = call(&mut app, "delete_job", json!({"confirm_token": tok}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(app.jobs.jobs.is_empty());
    assert!(crate::jobs::Store::load().jobs.is_empty());
    let v = call(&mut app, "run_job_now", json!({"job": "poll"}));
    assert!(v["error"].as_str().unwrap().contains("no job"), "{v}");
    let _ = std::fs::remove_dir_all(home);
}

pub(crate) fn session_loop(id: &str, cron: &str, prompt: &str) -> Loop {
    Loop {
        id: id.into(),
        kind: Kind::Cron,
        cron: Some(cron.into()),
        prompt: prompt.into(),
        recurring: true,
        durable: false,
        created: Some(chrono::Utc::now() - chrono::Duration::hours(30)),
        last_fire: Some(chrono::Utc::now() - chrono::Duration::minutes(3)),
        fires: 4,
        wake_at: None,
        deleted: false,
        confidence: Confidence::High,
        every: None,
    }
}

pub(crate) fn loop_row(app: &App, s: usize, t: usize, lp: Loop) -> crate::app_loops::LoopRow {
    crate::app_loops::LoopRow {
        uid: app.panes[s].tabs[t].uid,
        account: app.panes[s].account,
        tab_name: app.panes[s].tabs[t].name(),
        lp,
    }
}

#[test]
fn adopt_loop_turns_a_session_loop_into_a_job_and_cancels_the_original() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-adopt");
    let t = running(&mut app, 0, "billing", "sess-b");
    let tid = crate::control::tab_id(app.panes[0].tabs[t].uid);
    let row = loop_row(
        &app,
        0,
        t,
        session_loop("e0e3839a", "*/15 * * * *", "continue the map"),
    );
    app.loops = vec![row];
    // list_jobs shows it as a session loop that dies with its session.
    let l = call(&mut app, "list_jobs", json!({}));
    let sl = &l["result"]["session_loops"][0];
    assert_eq!(sl["kind"], json!("session"), "{l}");
    assert_eq!(sl["dies_with_session"], json!(true));
    assert!(sl["expires"].as_str().unwrap().starts_with("in "));
    let v = call(&mut app, "adopt_loop", json!({"tab": tid}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    assert!(v["question"]
        .as_str()
        .unwrap()
        .contains("cancelled so it does not fire twice"));
    let tok = v["token"].as_str().unwrap().to_string();
    yes(&mut app);
    let v = call(&mut app, "adopt_loop", json!({"confirm_token": tok}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let j = &app.jobs.jobs[0];
    assert_eq!(j.cron.as_deref(), Some("*/15 * * * *"));
    assert_eq!(j.prompt, "continue the map");
    assert_eq!(j.adopted_from.as_deref(), Some("e0e3839a"));
    assert_eq!(j.target.session_id.as_deref(), Some("sess-b"));
    // The tab is asked to cancel its own loop.
    assert!(app
        .stop_reqs
        .iter()
        .any(|q| q.ids == vec!["e0e3839a".to_string()]));
    // A dynamic loop has no schedule of its own.
    let mut wake = session_loop("w1", "", "keep going");
    wake.kind = Kind::Wakeup;
    wake.cron = None;
    app.loops = vec![loop_row(&app, 0, t, wake)];
    let v = call(&mut app, "adopt_loop", json!({"tab": tid}));
    assert!(
        v["error"].as_str().unwrap().contains("no fixed schedule"),
        "{v}"
    );
    let v = call(
        &mut app,
        "adopt_loop",
        json!({"tab": tid, "every": "20 minutes"}),
    );
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn archive_tabs_says_the_jobs_will_wake_the_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-archive-tool");
    let t = running(&mut app, 0, "billing", "sess-b");
    let tid = crate::control::tab_id(app.panes[0].tabs[t].uid);
    add(&mut app, 0, t, "nightly", "night check", 60);
    let v = call(&mut app, "archive_tabs", json!({"tab": tid}));
    let tok = match v["token"].as_str() {
        Some(t) => t.to_string(),
        None => String::new(),
    };
    let v = if tok.is_empty() {
        v
    } else {
        yes(&mut app);
        call(&mut app, "archive_tabs", json!({"confirm_token": tok}))
    };
    assert_eq!(v["ok"], json!(true), "{v}");
    let say = v["result"]["say"].as_str().unwrap();
    assert!(
        say.contains("GodTerm jobs were kept") && say.contains("will wake it"),
        "{say}"
    );
    assert_eq!(v["result"]["jobs_kept"].as_array().unwrap().len(), 1);
    assert_eq!(app.jobs.jobs.len(), 1);
    let _ = std::fs::remove_dir_all(home);
}

// ---- the section ----

pub(crate) fn screen(t: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
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

pub(crate) fn find(app: &App, a: &UiAction) -> Option<ratatui::layout::Rect> {
    app.last_hits
        .regions
        .iter()
        .rev()
        .find(|r| &r.action == a)
        .map(|r| r.rect)
}

fn section_setup(tag: &str) -> (App, std::path::PathBuf, String) {
    let (mut app, home) = setup(tag);
    app.assistant.show = true;
    let t = running(&mut app, 0, "billing", "sess-b");
    let id = add(&mut app, 0, t, "morning status", "status of billing?", 30);
    let row = loop_row(
        &app,
        0,
        t,
        session_loop("e0e3839a", "*/15 * * * *", "continue the map"),
    );
    let mut dur = session_loop("d1", "0 9 * * *", "daily durable thing");
    dur.durable = true;
    let drow = loop_row(&app, 0, t, dur);
    app.loops = vec![row, drow];
    (app, home, id)
}

#[test]
fn the_section_lists_managed_session_and_durable_rows_open_by_default() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, id) = section_setup("sect-draw");
    assert!(app.sched_open, "open by default");
    let mut t = term();
    draw(&mut app, &mut t);
    let s = screen(&t);
    assert!(s.contains("▾ Loops & schedules"), "{s}");
    assert!(s.contains("1 managed · 1 session · 1 durable"), "{s}");
    for want in [
        "managed",
        "session",
        "durable",
        "morning status",
        // The name is what gets cut first on a narrow panel.
        "continue the",
        "every 15 minutes",
    ] {
        assert!(s.contains(want), "{want} is shown:\n{s}");
    }
    // It sits above the conversation: the header row is above the composer.
    let head = s
        .lines()
        .position(|l| l.contains("Loops & schedules"))
        .unwrap();
    let composer = s.lines().position(|l| l.contains("Send ─╯")).unwrap();
    assert!(head < composer);
    let rows = app.sched_rows();
    assert_eq!(
        rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        vec![SchedKind::Managed, SchedKind::Session, SchedKind::Durable]
    );
    assert_eq!(rows[0].key, id);
    assert!(
        rows[1]
            .expires
            .as_deref()
            .unwrap()
            .starts_with("expires in"),
        "{:?}",
        rows[1].expires
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn collapsing_hides_the_rows_and_is_kept_in_the_state() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _) = section_setup("sect-collapse");
    let mut t = term();
    draw(&mut app, &mut t);
    assert!(screen(&t).contains("continue the"));
    click(&mut app, &UiAction::SchedToggle);
    assert!(!app.sched_open);
    draw(&mut app, &mut t);
    let s = screen(&t);
    assert!(s.contains("▸ Loops & schedules"), "{s}");
    assert!(!s.contains("continue the"), "{s}");
    // Saved with the state, and a fresh app comes back collapsed.
    let st = app.snapshot_state();
    assert!(st.sched_collapsed);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut next = App::new(crate::config::Config::default(), tx);
    assert!(next.sched_open);
    next.restore(st);
    assert!(!next.sched_open);
    // Open again: the default state serializes to nothing.
    click(&mut app, &UiAction::SchedToggle);
    assert!(app.sched_open && !app.snapshot_state().sched_collapsed);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn clicking_a_row_jumps_to_its_tab_and_the_buttons_act() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, id) = section_setup("sect-click");
    let other = running(&mut app, 0, "other", "sess-o");
    app.panes[0].select(other);
    let billing = app.panes[0]
        .tabs
        .iter()
        .position(|t| t.name() == "billing")
        .unwrap();
    assert_ne!(app.panes[0].active, billing);
    let mut t = term();
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SchedRow(id.clone()));
    assert_eq!(
        app.modal,
        crate::app::Modal::SchedCard(id.clone()),
        "a click opens the card"
    );
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SchedCardAct(crate::app_jobs::OP_OPEN));
    assert_eq!(
        app.panes[0].active, billing,
        "Open tab jumped to the job's tab"
    );
    assert_eq!(app.modal, crate::app::Modal::None);
    // Run now: a delivery to that tab.
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SchedAct(id.clone(), OP_RUN));
    assert_eq!(deliveries_for(&app, "status of billing?").len(), 1);
    // Pause, then resume.
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SchedAct(id.clone(), OP_PAUSE));
    assert!(!app.jobs.find(&id).unwrap().enabled);
    draw(&mut app, &mut t);
    assert!(screen(&t).contains("paused"));
    click(&mut app, &UiAction::SchedAct(id.clone(), OP_PAUSE));
    assert!(app.jobs.find(&id).unwrap().enabled);
    // Delete takes two clicks.
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SchedAct(id.clone(), OP_DELETE));
    assert!(app.jobs.find(&id).is_some());
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SchedAct(id.clone(), OP_DELETE));
    assert!(app.jobs.find(&id).is_none());
    // A session loop has no bare adopt button: its card's Manage with
    // GodTerm makes it a job.
    draw(&mut app, &mut t);
    let key = format!("{}:e0e3839a", app.panes[0].tabs[billing].uid);
    assert!(find(&app, &UiAction::SchedAct(key.clone(), OP_ADOPT)).is_none());
    click(&mut app, &UiAction::SchedRow(key));
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SchedCardAct(OP_ADOPT));
    assert_eq!(app.jobs.jobs.len(), 1);
    assert_eq!(app.jobs.jobs[0].prompt, "continue the map");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_row_for_an_archived_target_says_so_and_does_not_jump() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("sect-archived");
    app.assistant.show = true;
    let t = running(&mut app, 0, "gone-soon", "sess-g");
    let id = add(&mut app, 0, t, "keep alive", "ping", 60);
    app.archive_tab(0, t, "user", false).unwrap();
    let r = &app.sched_rows()[0];
    assert!(r.tab.contains("archived"), "{}", r.tab);
    assert!(r.uid.is_none());
    app.sched_open(&id);
    let f = app.flash.as_ref().map(|f| f.0.clone()).unwrap_or_default();
    assert!(f.contains("not open"), "{f}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn many_rows_fill_the_panel_height_and_a_small_screen_still_fits() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("sect-many");
    app.assistant.show = true;
    let t = running(&mut app, 0, "billing", "sess-b");
    for k in 0..9 {
        add(
            &mut app,
            0,
            t,
            &format!("job number {k}"),
            &format!("p{k}"),
            10 + k,
        );
    }
    // A tall panel shows them all: one tab header and its nine rows.
    let mut tm = term();
    draw(&mut app, &mut tm);
    let s = screen(&tm);
    assert!(s.contains("job number 8"), "{s}");
    assert!(s.contains("9 jobs"), "{s}");
    assert!(
        !s.contains(" of 10"),
        "no scroll indicator when all fit:\n{s}"
    );
    // 80x24, docked: the section keeps the composer and some chat.
    let mut small = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    small.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let s = screen(&small);
    assert!(s.contains("Loops & schedules"), "{s}");
    assert!(find(&app, &UiAction::AssistantAnswer(true)).is_none());
    // Overlay mode works the same.
    app.cfg.assistant.panel = "overlay".into();
    small.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(screen(&small).contains("Loops & schedules"));
    // A very short screen drops it before the composer.
    let mut tiny = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
    tiny.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(find(&app, &UiAction::AssistantFocus).is_some());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_prompt_is_not_sent_into_a_question_dialog() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-question");
    let t = running(&mut app, 0, "asker", "sess-q");
    add(&mut app, 0, t, "check", "run the check", 5);
    app.job_clock = Some(T0 + 5 * 60);
    app.jobs_tick();
    // The paste is in; then a question opens: Enter would answer it.
    let screen = std::fs::read_to_string(format!(
        "{}/tests/fixtures/screens/ask_question.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    app.panes[0].tabs[t]
        .parser
        .lock()
        .unwrap()
        .process(format!("\x1b[2J\x1b[H{}", screen.replace('\n', "\r\n")).as_bytes());
    let d = app
        .deliveries
        .iter_mut()
        .find(|d| d.text == "run the check")
        .unwrap();
    d.stage = Stage::Pasted(std::time::Instant::now());
    app.deliveries_tick();
    let d = app
        .deliveries
        .iter()
        .find(|d| d.text == "run the check")
        .unwrap();
    assert!(
        matches!(&d.stage, Stage::Failed(w) if w.contains("question")),
        "{:?}",
        d.stage
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_moved_tabs_jobs_follow_it_to_its_new_id_and_account() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = setup("jobs-move");
    let t = running(&mut app, 0, "mover", "sess-m");
    let uid = app.panes[0].tabs[t].uid;
    let id = add(&mut app, 0, t, "check", "run the check", 5);
    let t2 = running(&mut app, 0, "other", "sess-o");
    let other = add(&mut app, 0, t2, "o", "x", 5);
    app.jobs_follow_move(uid, 9001, "account3");
    let j = app.jobs.find(&id).unwrap();
    assert_eq!(j.target.uid, Some(9001));
    assert_eq!(j.target.account, "account3");
    assert_ne!(app.jobs.find(&other).unwrap().target.uid, Some(9001));
    assert_eq!(
        crate::jobs::Store::load().find(&id).unwrap().target.uid,
        Some(9001)
    );
    let _ = std::fs::remove_dir_all(home);
}
