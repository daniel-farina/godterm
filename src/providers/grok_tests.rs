//! The grok backend with stand in groks (never the real one): the effort
//! and the removed tools in its argv, the primer that opens each session
//! so every real turn is a resume, and what happens when the primer
//! fails, hangs or is interrupted.

use super::*;
use std::sync::mpsc::{channel, Receiver};

/// A turn of fake grok output: a use_tool call, the answer, the result.
const TURN: &str = r#"{"type":"system","subtype":"init","session_id":"s1","model":"grok-4.7-build-fast","tools":["search_tool","use_tool"]}
{"type":"assistant","message":{"id":"m0","content":[{"type":"tool_use","id":"c1","name":"use_tool","input":{"tool_name":"godterm__get_state","tool_input":{}}}]}}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Two tabs are working."}}}
{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Two tabs are working."}]}}
{"type":"result","subtype":"success","is_error":false,"result":"Two tabs are working.","total_cost_usd":0.004}
"#;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("godterm-grokbe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("home/work")).unwrap();
    d
}

fn backend(d: &Path, bin: &Path) -> (GrokBackend, Receiver<AppEvent>) {
    let (tx, rx) = channel();
    let b = GrokBackend::new(
        Launch {
            bin: bin.to_string_lossy().into_owned(),
            home: d.join("home"),
            pass_env: vec![],
        },
        "grok-4.7-build-fast".into(),
        effort_for("low"),
        "You are GodTerm's assistant.".into(),
        tx,
        7,
    );
    (b, rx)
}

/// The events of one turn, up to its Done.
fn turn(rx: &Receiver<AppEvent>, secs: u64) -> Vec<BrainEvent> {
    let mut evs = vec![];
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if let Ok(AppEvent::Assistant(gen, ev)) = rx.recv_timeout(Duration::from_millis(100)) {
            assert_eq!(gen, 7);
            let done = matches!(ev, BrainEvent::Done { .. });
            evs.push(ev);
            if done {
                break;
            }
        }
    }
    evs
}

fn lines(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A stand in grok as a shell script: `primer` is what it does for the
/// primer, otherwise it prints the canned turn. Every call's argv is
/// appended to args.txt first.
#[cfg(unix)]
fn script(d: &Path, primer: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let canned = d.join("turn.jsonl");
    std::fs::write(&canned, TURN).unwrap();
    let body = format!(
        "#!/bin/sh\necho \"$*\" >> '{args}'\ncase \"$*\" in\n  *{PRIMER}*) {primer} ;;\nesac\ncat '{canned}'\n",
        args = d.join("args.txt").display(),
        canned = canned.display(),
    );
    // Written under another name and renamed: never open while it runs.
    let tmp = d.join("grok.tmp");
    std::fs::write(&tmp, body).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = d.join("grok");
    std::fs::rename(&tmp, &p).unwrap();
    p
}

#[test]
fn effort_and_removed_tools_in_the_argv() {
    // Grok has no "none"; anything unknown is the quickest, low.
    assert_eq!(effort_for("low"), "low");
    assert_eq!(effort_for("high"), "high");
    assert_eq!(effort_for("none"), "low");
    assert_eq!(effort_for(""), "low");
    for e in PROVIDER.caps.efforts {
        assert_eq!(effort_for(e), *e, "every effort offered is one grok takes");
    }
    let a = turn_args("m", "low", "prompt", "s1", true, "hi").join(" ");
    assert!(a.contains("--reasoning-effort low"), "{a}");
    assert!(
        a.contains("--resume s1") && !a.contains("--session-id"),
        "{a}"
    );
    // The shell goes by both names: both removed, and still denied.
    let removed = turn_args("m", "low", "p", "s1", false, "hi");
    let i = removed
        .iter()
        .position(|x| x == "--disallowed-tools")
        .unwrap();
    let list: Vec<&str> = removed[i + 1].split(',').collect();
    assert!(list.contains(&"run_terminal_cmd") && list.contains(&"run_terminal_command"));
    assert!(removed.join(" ").contains("--deny Bash(*)"));
}

#[test]
fn the_first_turn_is_primed_then_every_turn_resumes() {
    let d = dir("prime");
    let canned = d.join("turn.jsonl");
    std::fs::write(&canned, TURN).unwrap();
    let args = d.join("args.txt");
    let stub = crate::test_stub::claude(
        &d.join("bin"),
        &[
            ("print_file", canned.display().to_string()),
            ("args_to", args.display().to_string()),
        ],
    );
    let (mut b, rx) = backend(&d, &stub);
    b.send("what is running").unwrap();
    let evs = turn(&rx, 10);
    assert!(
        evs.contains(&BrainEvent::Tool("get_state".into(), serde_json::json!({}))),
        "{evs:?}"
    );
    assert!(
        matches!(evs.last(), Some(BrainEvent::Done { error: false, .. })),
        "{evs:?}"
    );
    // The primer's own output never reaches the app: one Done per turn.
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "nothing after the Done"
    );
    let l = lines(&args);
    assert_eq!(l.len(), 2, "{l:?}");
    let sid = b.session.clone();
    // The primer opens the session, with GodTerm's prompt and the effort.
    assert!(
        l[0].contains(&format!("-p {PRIMER}")) && l[0].contains(&format!("--session-id {sid}")),
        "{}",
        l[0]
    );
    assert!(l[0].contains("--system-prompt-override You are GodTerm"));
    // The turn itself resumes it.
    assert!(
        l[1].contains("-p what is running") && l[1].contains(&format!("--resume {sid}")),
        "{}",
        l[1]
    );
    assert!(l[1].contains("--reasoning-effort low") && !l[1].contains("--session-id"));
    // The next turn: no primer, a resume.
    b.send("and now").unwrap();
    let evs = turn(&rx, 10);
    assert!(matches!(
        evs.last(),
        Some(BrainEvent::Done { error: false, .. })
    ));
    let l = lines(&args);
    assert_eq!(l.len(), 3, "{l:?}");
    assert!(l[2].contains("-p and now") && l[2].contains(&format!("--resume {sid}")));
    drop(b);
    let _ = std::fs::remove_dir_all(d);
}

#[cfg(unix)]
#[test]
fn a_failed_primer_lets_the_turn_open_the_session() {
    let d = dir("fail");
    let bin = script(&d, "echo 'Error: unknown command' >&2; exit 1");
    let (mut b, rx) = backend(&d, &bin);
    b.send("hello").unwrap();
    let evs = turn(&rx, 10);
    assert!(
        matches!(evs.last(), Some(BrainEvent::Done { error: false, .. })),
        "{evs:?}"
    );
    let l = lines(&d.join("args.txt"));
    assert_eq!(l.len(), 2, "{l:?}");
    let sid = b.session.clone();
    assert!(l[1].contains("-p hello") && l[1].contains(&format!("--session-id {sid}")));
    // Later turns resume as always.
    b.send("again").unwrap();
    turn(&rx, 10);
    assert!(lines(&d.join("args.txt"))[2].contains(&format!("--resume {sid}")));
    drop(b);
    let _ = std::fs::remove_dir_all(d);
}

#[cfg(unix)]
#[test]
fn a_hung_primer_is_stopped_and_the_turn_goes_on() {
    let d = dir("hung");
    let bin = script(&d, "exec sleep 30");
    let (mut b, rx) = backend(&d, &bin);
    let t0 = Instant::now();
    b.send("hello").unwrap();
    let evs = turn(&rx, 15);
    assert!(
        matches!(evs.last(), Some(BrainEvent::Done { error: false, .. })),
        "{evs:?}"
    );
    assert!(t0.elapsed() < PRIMER_TIMEOUT + Duration::from_secs(5));
    let l = lines(&d.join("args.txt"));
    assert!(l.len() == 2 && l[1].contains("--session-id"), "{l:?}");
    drop(b);
    let _ = std::fs::remove_dir_all(d);
}

#[cfg(unix)]
#[test]
fn interrupting_while_priming_stops_the_turn() {
    let d = dir("intr");
    let bin = script(&d, "exec sleep 30");
    let (mut b, rx) = backend(&d, &bin);
    b.send("hello").unwrap();
    let t0 = Instant::now();
    while b.pid() == 0 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(b.pid(), 0, "the primer runs");
    b.interrupt().unwrap();
    let evs = turn(&rx, 5);
    assert!(
        matches!(evs.last(), Some(BrainEvent::Done { error: true, text, .. }) if text == "stopped"),
        "{evs:?}"
    );
    // The turn never started.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(lines(&d.join("args.txt")).len(), 1);
    drop(b);
    let _ = std::fs::remove_dir_all(d);
}
