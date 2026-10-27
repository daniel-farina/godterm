use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::test_app;
use crate::pane::{Activity, PaneState};

fn ready_tab(app: &mut App, name: &str) -> (usize, PathBuf) {
    let a = app.panes[0].account.unwrap();
    let proj = app.cfg.accounts[a].work_dir().join(name);
    std::fs::create_dir_all(&proj).unwrap();
    let t = app.panes[0].add_tab(proj.clone());
    let tab = &mut app.panes[0].tabs[t];
    tab.custom_name = Some(name.into());
    tab.session_id = Some(format!("sess-{name}"));
    tab.state = PaneState::Running;
    tab.activity = Activity::Ready;
    let dir = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&proj));
    std::fs::create_dir_all(&dir).unwrap();
    (t, dir.join(format!("sess-{name}.jsonl")))
}

fn stamp(secs_ago: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(secs_ago))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn write(path: &std::path::Path, lines: &[Value]) {
    let body: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    std::fs::write(path, body.join("\n") + "\n").unwrap();
}

fn set_mtime(p: &std::path::Path, ago: Duration) {
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(SystemTime::now() - ago)
        .unwrap();
}

fn finished_turn(path: &std::path::Path, prompt: &str, reply: &str, secs_ago: i64) {
    write(
        path,
        &[
            json!({"type": "user", "message": {"role": "user", "content": prompt}, "timestamp": stamp(secs_ago + 60)}),
            json!({"type": "assistant", "message": {"id": "m1", "content": [{"type": "tool_use", "id": "x1", "name": "Bash", "input": {"command": "cargo test"}}]}, "timestamp": stamp(secs_ago + 50)}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "x1", "content": "ok"}]}, "timestamp": stamp(secs_ago + 40)}),
            json!({"type": "assistant", "message": {"id": "m2", "content": [{"type": "text", "text": reply}]}, "timestamp": stamp(secs_ago)}),
        ],
    );
    set_mtime(path, Duration::from_secs(secs_ago.max(0) as u64));
}

fn subagent(transcript: &std::path::Path, id: &str, task: &str, ago: Duration) {
    let dir = transcript.with_extension("").join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{id}.jsonl"));
    write(
        &p,
        &[
            json!({"type": "user", "isSidechain": true, "agentType": "general-purpose", "message": {"role": "user", "content": task}, "timestamp": stamp(36 * 60)}),
            json!({"type": "assistant", "isSidechain": true, "message": {"id": "s1", "content": [{"type": "tool_use", "id": "y1", "name": "Bash", "input": {"command": "cargo test --release"}}]}, "timestamp": stamp(ago.as_secs() as i64)}),
        ],
    );
    set_mtime(&p, ago);
}

const AGENTS_SCREEN: &str = "⏺ Waiting for 3 background agents to finish\r\n  ⎿  ○ general-purpose 36m 26s\r\n     ○ general-purpose 36m 26s\r\n     ○ Explore 2m 5s\r\n\r\n────────────────────────────\r\n❯ \r\n────────────────────────────\r\n  ? for shortcuts\r\n";

fn entries<'a>(v: &'a Value, group: &str) -> Vec<&'a Value> {
    v["result"][group]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn names(v: &Value, group: &str) -> Vec<String> {
    entries(v, group)
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_tab_with_running_subagents_is_working_everywhere() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("workstatus");

    let (g, gp) = ready_tab(&mut app, "godterm");
    finished_turn(
        &gp,
        "Run the status work",
        "Waiting for the agents to finish.",
        20 * 60,
    );
    subagent(
        &gp,
        "agent-1",
        "Run the regression groups",
        Duration::from_secs(20 * 60),
    );
    app.panes[0].tabs[g]
        .parser
        .lock()
        .unwrap()
        .process(AGENTS_SCREEN.as_bytes());

    let (h, _) = ready_tab(&mut app, "hyper");
    let hu = app.panes[0].tabs[h].uid;
    app.limit_hit.insert(hu);

    let (x, xp) = ready_tab(&mut app, "scout");
    finished_turn(
        &xp,
        "Clean the cache",
        "Done: the cache is clean. Should I also drop the old builds?",
        5 * 60,
    );

    let (n, _) = ready_tab(&mut app, "notes");
    app.panes[0].tabs[n].activity_since = Instant::now() - Duration::from_secs(2 * 3600);

    let a = app.tab_activity(0, g);
    assert_eq!(a.subagents_running, 3, "{a:?}");
    assert!(
        a.state.starts_with("background (3 subagents"),
        "{}",
        a.state
    );
    assert_eq!(
        a.subagents_line().unwrap(),
        "3 subagents (general-purpose (36 min), general-purpose (36 min), Explore (2 min))"
    );
    assert_eq!(app.state_word(0, g), "background");
    assert!(app.tab_busy(0, g));

    let st = app.control_call("get_state", &json!({}));
    let gu = app.panes[0].tabs[g].uid;
    let row = st["result"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(crate::control::tab_id(gu)))
        .unwrap()
        .clone();
    assert_eq!(row["state"], "background", "{row}");
    assert_eq!(row["background_work"], "3 subagents", "{row}");
    let pre = app.state_preamble();
    assert!(
        pre.contains("WORKING in background (3 subagents (general-purpose (36 min)"),
        "{pre}"
    );

    let v = app.control_call("work_status", &json!({}));
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(names(&v, "needs_you"), ["hyper", "scout"]);
    assert_eq!(names(&v, "working"), ["godterm"]);
    assert_eq!(names(&v, "finished"), Vec::<String>::new());
    assert_eq!(names(&v, "idle"), ["notes"]);
    let gt = entries(&v, "working")[0];
    assert_eq!(gt["group"], "working");
    assert_eq!(gt["state"], "background");
    assert!(
        gt["working_through"]
            .as_str()
            .unwrap()
            .starts_with("3 subagents (general-purpose (36 min)"),
        "{gt}"
    );
    assert_eq!(gt["subagents"].as_array().unwrap().len(), 3);
    assert_eq!(gt["last_line"], "Waiting for the agents to finish.");
    assert_eq!(gt["last_prompt"], "Run the status work");
    assert_eq!(gt["minutes_since_activity"], 0);
    let hy = entries(&v, "needs_you")[0];
    assert!(
        hy["needs"].as_str().unwrap().contains("usage limit"),
        "{hy}"
    );
    let xs = entries(&v, "needs_you")[1];
    assert!(xs["needs"].as_str().unwrap().contains("question"), "{xs}");
    assert!(xs["last_line"]
        .as_str()
        .unwrap()
        .starts_with("Done: the cache is clean."));
    assert!(
        v["result"]["summary"]
            .as_str()
            .unwrap()
            .starts_with("2 need you, 1 working (1 through agents), 1 idle"),
        "{}",
        v["result"]["summary"]
    );
    assert!(!v["result"]["off"].as_array().unwrap().is_empty());
    assert!(v["result"].get("since").is_none());

    let (lines, rest) = App::work_digest(&v["result"], 0, 3);
    assert_eq!(lines.len(), 3);
    assert!(
        lines[0].starts_with("hyper needs you: usage limit hit"),
        "{lines:?}"
    );
    assert!(
        lines[2].starts_with("godterm is working through 3 subagents (general-purpose (36 min)"),
        "{lines:?}"
    );
    assert_eq!(rest, 0);

    let v2 = app.control_call("work_status", &json!({"since": "last"}));
    assert_eq!(
        v2["result"]["since"]["changed"],
        json!([]),
        "{}",
        v2["result"]["since"]
    );
    assert_eq!(
        v2["result"]["since"]["unchanged"],
        app.all_tabs().len(),
        "{}",
        v2["result"]["since"]
    );
    finished_turn(&xp, "Drop them", "Done: the old builds are gone.", 10);
    let v3 = app.control_call("work_status", &json!({"since": "last"}));
    let xu = crate::control::tab_id(app.panes[0].tabs[x].uid);
    assert_eq!(
        v3["result"]["since"]["changed"],
        json!([xu]),
        "{}",
        v3["result"]["since"]
    );
    let xs = entries(&v3, "finished")[0];
    assert_eq!(xs["changed"], true);
    assert_eq!(xs["was"], "needs_you");
    assert_eq!(names(&v3, "needs_you"), ["hyper"]);

    let one = app.control_call("work_status", &json!({"tab": "godterm"}));
    assert_eq!(names(&one, "working"), ["godterm"]);
    assert!(entries(&one, "needs_you").is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn agents_count_from_transcripts_or_the_screen_alone() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("workstatus-src");
    let (t, p) = ready_tab(&mut app, "quiet");
    finished_turn(&p, "Go", "Working through two agents.", 25 * 60);
    subagent(&p, "agent-a", "Task a", Duration::from_secs(25 * 60));
    subagent(&p, "agent-b", "Task b", Duration::from_secs(3 * 60));
    let a = app.tab_activity(0, t);
    assert_eq!(a.subagents_running, 1, "{a:?}");
    assert_eq!(a.subagents[0].task.as_deref(), Some("Task b"));
    assert_eq!(a.subagents[0].tool.as_deref(), Some("Bash"));
    assert_eq!(app.state_word(0, t), "background");
    let (s, _) = ready_tab(&mut app, "screen-only");
    app.panes[0].tabs[s]
        .parser
        .lock()
        .unwrap()
        .process(AGENTS_SCREEN.as_bytes());
    let a = app.tab_activity(0, s);
    assert_eq!(a.subagents_running, 3, "{a:?}");
    assert_eq!(a.subagents[2].name.as_deref(), Some("Explore"));
    assert_eq!(app.state_word(0, s), "background");
    let _ = std::fs::remove_dir_all(&home);
}

const LIMIT_SCREEN: &str = "⏺ Waiting for 3 background agents to finish\r\n  ⎿  ○ general-purpose 52m 3s\r\n     ○ general-purpose 52m 3s\r\n     ○ Explore 50m 1s\r\n\r\n  Claude usage limit reached. Your limit will reset at 7pm.\r\n\r\n────────────────────────────\r\n❯ \r\n────────────────────────────\r\n";

fn stopped_agent(transcript: &std::path::Path, id: &str, age: Duration) {
    let dir = transcript.with_extension("").join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{id}.jsonl"));
    write(
        &p,
        &[
            json!({"type": "user", "isSidechain": true, "agentType": "general-purpose", "message": {"role": "user", "content": "Check the CI runner"}, "timestamp": stamp(3600)}),
            json!({"type": "assistant", "isSidechain": true, "message": {"id": "s1", "content": [{"type": "tool_use", "id": "y1", "name": "Bash", "input": {"command": "gh run list"}}]}}),
            json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "x"}}),
            json!({"type": "api-request", "isSidechain": true}),
        ],
    );
    set_mtime(&p, age);
}

fn tab_row(app: &mut App, uid: u64) -> Value {
    let st = app.control_call("get_state", &json!({}));
    st["result"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(crate::control::tab_id(uid)))
        .cloned()
        .unwrap()
}

#[test]
fn a_tab_stopped_at_its_limit_moves_at_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("limitmove");
    let (t, p) = ready_tab(&mut app, "hyper");
    finished_turn(&p, "Rotate the keys", "Waiting for the agents.", 50 * 60);
    for id in ["agent-a02c", "agent-a8de", "agent-a5a8"] {
        stopped_agent(&p, id, Duration::from_secs(50 * 60));
    }

    let a = app.tab_activity(0, t);
    assert_eq!(a.subagents_running, 0, "{a:?}");
    assert_eq!(app.state_word(0, t), "ready");

    app.panes[0].tabs[t]
        .parser
        .lock()
        .unwrap()
        .process(LIMIT_SCREEN.as_bytes());
    assert!(app.tab_at_limit(0, t));
    let a = app.tab_activity(0, t);
    assert_eq!(a.subagents_running, 0, "{a:?}");
    assert_eq!(app.state_word(0, t), "ready");
    assert!(!app.tab_has_running_work(0, t));
    assert!(app.tab_background_work(0, t).is_none());
    let uid = app.panes[0].tabs[t].uid;
    let w = app.control_call("work_status", &json!({}));
    assert!(
        !names(&w, "working").contains(&"hyper".to_string()),
        "never working through agents: {w}"
    );

    let id = crate::control::tab_id(uid);
    let v = app.control_call("move_tab", &json!({"tab": id, "to": 2}));
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["result"]["moved"].as_array().unwrap().len(), 1, "{v}");
    assert!(app.queued_moves.is_empty());
    assert!(v["result"]["after_current_work"]
        .as_array()
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn stale_agents_do_not_hold_a_move_and_the_limit_mark_counts() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("stalemove");
    let (t, p) = ready_tab(&mut app, "stale");
    finished_turn(&p, "Go", "Done.", 50 * 60);
    stopped_agent(&p, "agent-x", Duration::from_secs(50 * 60));
    assert!(!app.tab_has_running_work(0, t));
    let uid = app.panes[0].tabs[t].uid;
    let v = app.control_call(
        "move_tab",
        &json!({"tab": crate::control::tab_id(uid), "to": 2}),
    );
    assert_eq!(v["result"]["moved"].as_array().unwrap().len(), 1, "{v}");

    let (t2, p2) = ready_tab(&mut app, "marked");
    finished_turn(&p2, "Go", "Waiting.", 60);
    subagent(&p2, "agent-r", "Run it", Duration::from_secs(60));
    assert_eq!(app.tab_activity(0, t2).subagents_running, 1);
    let uid2 = app.panes[0].tabs[t2].uid;
    app.limit_hit.insert(uid2);
    assert_eq!(app.tab_activity(0, t2).subagents_running, 0);
    assert!(!app.tab_has_running_work(0, t2));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_queued_move_says_why_shows_in_the_list_and_can_be_forced() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("busymove");
    let (t, p) = ready_tab(&mut app, "godterm");
    finished_turn(&p, "Run it", "Waiting for the agents.", 60);
    subagent(
        &p,
        "agent-1",
        "Run the regression groups",
        Duration::from_secs(60),
    );
    subagent(
        &p,
        "agent-2",
        "Check the mobile batch",
        Duration::from_secs(90),
    );
    let uid = app.panes[0].tabs[t].uid;
    let id = crate::control::tab_id(uid);
    let v = app.control_call("move_tab", &json!({"tab": id, "to": 2}));
    assert_eq!(v["ok"], true, "{v}");
    let r = &v["result"];
    assert_eq!(r["after_current_work"], json!([id]), "{v}");
    assert!(r["moved"].as_array().unwrap().is_empty());
    let say = r["say"].as_str().unwrap();
    assert!(
        say.contains("godterm is busy (2 subagents (")
            && say.contains("running)")
            && say.contains("moves when that ends"),
        "{say}"
    );
    assert!(r["waiting_for"][0]["why"]
        .as_str()
        .unwrap()
        .contains("2 subagents"));

    app.control_call("move_tab", &json!({"tab": id, "to": 2}));
    assert_eq!(app.queued_moves.len(), 1);

    let row = tab_row(&mut app, uid);
    assert!(
        row["move_pending"]["waiting_for"]
            .as_str()
            .unwrap()
            .contains("2 subagents"),
        "{row}"
    );
    assert_eq!(
        row["move_pending"]["to"],
        json!(app.cfg.accounts[1].display().to_string())
    );
    assert!(app.move_pending(uid).is_some());

    app.tabmove_tick();
    assert_eq!(app.queued_moves.len(), 1);
    assert!(app.find_tab(uid).is_some());

    let before = app.pending_moves.len();
    let v = app.control_call("move_now", &json!({"tab": id, "to": 2}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let q = v["question"].as_str().unwrap();
    assert!(
        q.contains("now?") && q.contains("stops what runs") && q.contains("2 subagents"),
        "{q}"
    );
    assert_eq!(app.pending_moves.len(), before, "nothing moved yet");
    let tok = v["token"].as_str().unwrap().to_string();
    app.assistant_turn += 1;
    app.assistant.last_user = "yes".into();
    let v = app.control_call("move_now", &json!({"confirm_token": tok}));
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["result"]["moved"].as_array().unwrap().len(), 1, "{v}");
    assert!(app.queued_moves.is_empty(), "the queued move is dropped");
    assert!(app.pending_moves.len() > before);
    let _ = std::fs::remove_dir_all(&home);
}
