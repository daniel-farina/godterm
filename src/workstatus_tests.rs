//! The status picture: a tab at its prompt whose Agent tool subagents
//! still run (their transcripts mid call, written long ago; the screen's
//! "Waiting for N background agents" rows) is working, in get_state, in
//! the assistant's snapshot and in work_status; work_status groups
//! needs_you, working, finished and idle, says what each tab is doing,
//! and since "last" marks what changed.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::test_app;
use crate::pane::{Activity, PaneState};

/// A running tab at its prompt with a session: (index, transcript path).
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

/// A finished turn: the prompt, a tool call, its result, the reply.
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

/// A subagent transcript mid tool call, last written `ago`.
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
    // godterm: its turn ended with "Waiting for the agents", one subagent
    // transcript mid call written 20 minutes ago, and the screen shows
    // three running agents.
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
    // hyper: hit its usage limit.
    let (h, _) = ready_tab(&mut app, "hyper");
    let hu = app.panes[0].tabs[h].uid;
    app.limit_hit.insert(hu);
    // scout: finished five minutes ago with a question.
    let (x, xp) = ready_tab(&mut app, "scout");
    finished_turn(
        &xp,
        "Clean the cache",
        "Done: the cache is clean. Should I also drop the old builds?",
        5 * 60,
    );
    // notes: nothing for two hours.
    let (n, _) = ready_tab(&mut app, "notes");
    app.panes[0].tabs[n].activity_since = Instant::now() - Duration::from_secs(2 * 3600);
    // The first tabs of the test app are not running: off.

    // The activity sees the agents, named from the screen rows.
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
    // get_state (the web and TUI state) and the snapshot say so.
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

    // work_status: the groups, in order, with what each is doing.
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

    // The digest a voice answer is built from.
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

    // since "last": nothing changed, then scout moves on.
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
    // One tab only.
    let one = app.control_call("work_status", &json!({"tab": "godterm"}));
    assert_eq!(names(&one, "working"), ["godterm"]);
    assert!(entries(&one, "needs_you").is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

/// A subagent transcript silent for twenty minutes mid call still
/// counts without any screen rows (the Agent tool's agents write
/// nothing during a long tool call), and the screen alone counts too.
#[test]
fn agents_count_from_transcripts_or_the_screen_alone() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("workstatus-src");
    let (t, p) = ready_tab(&mut app, "quiet");
    finished_turn(&p, "Go", "Working through two agents.", 25 * 60);
    subagent(&p, "agent-a", "Task a", Duration::from_secs(25 * 60));
    subagent(&p, "agent-b", "Task b", Duration::from_secs(3 * 60));
    let a = app.tab_activity(0, t);
    assert_eq!(a.subagents_running, 2, "{a:?}");
    assert_eq!(a.subagents[0].task.as_deref(), Some("Task a"));
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
