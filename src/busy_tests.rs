use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::{App, AppEvent};
use crate::app_busy::Mode;
use crate::app_followup::FuState;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::{Activity, LaunchKind};

const SID: &str = "d34c6258-af8c-417b-8e66-53936c24b49f";

struct T {
    app: App,
    rx: Receiver<AppEvent>,
    home: PathBuf,
    args: PathBuf,
    stdin: PathBuf,
    envs: PathBuf,
    uid: u64,
    slot: usize,
}

impl Drop for T {
    fn drop(&mut self) {
        for p in &mut self.app.panes {
            p.kill_all();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn setup(tag: &str, opts: &[(&str, &str)], activity: Activity) -> T {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-busy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    let args = home.join("args.txt");
    let stdin = home.join("stdin.txt");
    let envs = home.join("env.txt");
    let mut o: Vec<(&str, String)> = vec![
        ("args_to", args.display().to_string()),
        ("stdin_to", stdin.display().to_string()),
        ("stdin_append", "1".into()),
        ("env_to", envs.display().to_string()),
        ("sleep_s", "40".into()),
    ];
    for (k, v) in opts {
        o.push((k, v.to_string()));
    }
    let stub = crate::test_stub::claude(&home.join("bin"), &o);
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let slot = app.pane_for_account(2);
    app.panes[slot].cur_mut().cwd = cwd;
    app.panes[slot].cur_mut().session_id = Some(SID.into());
    app.launch_tab(slot, 0, LaunchKind::Resume(SID.into()));
    let uid = app.panes[slot].tabs[0].uid;
    let mut t = T {
        app,
        rx,
        home,
        args,
        stdin,
        envs,
        uid,
        slot,
    };

    let a = t.args.clone();
    assert!(t.pump(10, |_| std::fs::read_to_string(&a).is_ok()));
    t.app.panes[slot].tabs[0].activity = activity;
    t
}

impl T {
    fn pump(&mut self, secs: u64, mut done: impl FnMut(&mut App) -> bool) -> bool {
        let t0 = Instant::now();
        loop {
            while let Ok(ev) = self.rx.try_recv() {
                if matches!(ev, AppEvent::PaneExited(..)) {
                    self.app.handle(ev);
                }
            }
            self.app.control_tick();
            if done(&mut self.app) {
                return true;
            }
            if t0.elapsed() > Duration::from_secs(secs) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&mut self, args: Value) -> Value {
        let mut a = json!({"_client": "brain", "tab": format!("t{}", self.uid)});
        for (k, v) in args.as_object().unwrap() {
            a[k] = v.clone();
        }
        self.app.control_call("send_prompt", &a)
    }

    fn delivery(&self, id: u64) -> crate::control::Delivery {
        self.app
            .deliveries
            .iter()
            .find(|d| d.id == id)
            .unwrap()
            .clone()
    }

    fn last_id(&self) -> u64 {
        self.app.deliveries.last().unwrap().id
    }

    fn screen(&mut self, text: &str) {
        let tab = &self.app.panes[self.slot].tabs[0];
        tab.parser
            .lock()
            .unwrap()
            .process(format!("\x1b[2J\x1b[H{}", text.replace('\n', "\r\n")).as_bytes());
    }

    fn lines(p: &PathBuf) -> Vec<String> {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn said(&mut self, s: &str) {
        self.app.assistant_turn += 1;
        self.app.assistant.last_user = s.into();
    }
}

fn has(p: &PathBuf, needle: &str) -> bool {
    std::fs::read(p)
        .map(|b| String::from_utf8_lossy(&b).contains(needle))
        .unwrap_or(false)
}

fn followup(app: &App) -> Option<crate::app_followup::FollowUp> {
    app.assistant.follow_ups.last().cloned()
}

fn last_receipt(a: &App) -> Option<crate::receipts::Receipt> {
    a.external_receipt(a.deliveries.last()?)
}

#[test]
fn a_question_to_a_busy_tab_uses_the_side_channel() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("side", &[], Activity::Working);
    let q = "What are you working on right now?";
    t.said("ask the coordinator what it is working on");
    let v = t.send(json!({"text": q}));
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(v["will_report"], json!(true), "{v}");
    let id = t.last_id();
    assert_eq!(t.delivery(id).rc.mode, Mode::Side);
    assert_eq!(t.app.queued_for(t.uid), 0, "nothing queued behind the work");

    let stdin = t.stdin.clone();
    assert!(
        t.pump(5, |_| has(&stdin, &format!("/btw {q}"))),
        "{:?}",
        std::fs::read(&stdin)
    );

    t.screen(&format!("> /btw {q}"));
    assert!(t.pump(5, |_| has(&stdin, "\n")));

    t.screen(&format!("working hard\n  /btw {q}\n  Answering…"));
    assert!(t.pump(5, |a| last_receipt(a)
        == Some(crate::receipts::Receipt::Started)));
    t.screen(&format!(
        "working hard\n  /btw {q}\n  I am fixing the build; two tests are left.\n\n  Press Esc to dismiss\n> "
    ));
    assert!(t.pump(8, |a| matches!(
        followup(a).map(|f| f.state),
        Some(FuState::Answered { .. })
    )));
    let FuState::Answered { text, .. } = followup(&t.app).unwrap().state else {
        unreachable!()
    };
    assert!(text.contains("I am fixing the build"), "{text}");
    assert!(text.contains("side answer"), "{text}");

    assert!(t.pump(2, |_| has(&stdin, "\u{1b}")));

    t.pump(3, |a| a.deliveries_json()[0]["state"] == "done");
    let dj = t.app.deliveries_json();
    assert_eq!(dj[0]["mode"], "side", "{dj}");
    assert_eq!(dj[0]["state"], "done", "{dj}");

    assert_eq!(t.app.busy.fork_ids.len(), 0);
    assert!(!has(&t.args, "--bg"));
}

#[test]
fn work_for_a_busy_tab_forks_a_background_session() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup(
        "bg",
        &[("bg_print", "Started background session 11111111")],
        Activity::Working,
    );
    let task = "Add a dark mode toggle to the settings page";
    t.said("have the coordinator add a dark mode toggle");
    let v = t.send(json!({"text": task, "expect_reply": false}));
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(v["will_report"], json!(true), "{v}");
    let id = t.last_id();
    assert_eq!(t.delivery(id).rc.mode, Mode::Background);
    assert_eq!(t.app.queued_for(t.uid), 0);

    let args = t.args.clone();
    assert!(t.pump(10, |_| T::lines(&args).iter().any(|l| l.contains("--bg"))));
    let line = T::lines(&t.args)
        .into_iter()
        .find(|l| l.contains("--bg"))
        .unwrap();

    assert!(
        line.starts_with("--bg ")
            && line.contains(&format!(" --resume {SID} --fork-session --session-id ")),
        "{line}"
    );
    assert!(line.ends_with(task), "{line}");
    let new_id = line
        .split(" --session-id ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_string();
    assert_ne!(new_id, SID);

    let canon = |p: &std::path::Path| {
        let c = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        c.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_lowercase()
    };
    let want = (
        canon(&t.app.panes[t.slot].tabs[0].cwd),
        canon(&t.app.cfg.accounts[2].config_dir()),
    );
    let seen: Vec<(String, String)> = T::lines(&t.envs)
        .iter()
        .filter_map(|l| l.split_once('|'))
        .map(|(a, b)| {
            (
                canon(std::path::Path::new(a)),
                canon(std::path::Path::new(b)),
            )
        })
        .collect();
    assert!(
        seen.iter().filter(|l| *l == &want).count() >= 2,
        "{want:?} in {seen:?}"
    );

    assert!(t.pump(5, |a| last_receipt(a)
        == Some(crate::receipts::Receipt::Received)));
    let dj = t.app.deliveries_json();
    assert_eq!(dj[0]["mode"], "background", "{dj}");
    assert_eq!(
        dj[0]["background_session"],
        json!(new_id[..8].to_string()),
        "{dj}"
    );
    let st = t.app.control_call("get_state", &json!({}));
    let tabs = st.to_string();
    assert!(
        tabs.contains("background_sessions") && tabs.contains(&new_id[..8]),
        "{tabs}"
    );

    let dir = t.app.cfg.accounts[2].config_dir();
    let cwd = t.app.panes[t.slot].tabs[0].cwd.clone();
    let proj = dir
        .join("projects")
        .join(crate::state::encode_project_dir(&cwd));
    std::fs::create_dir_all(&proj).unwrap();
    let write_fork = |reply: &str| {
        let lines = [
            json!({"type": "user", "cwd": cwd, "sessionId": new_id, "message": {"role": "user", "content": task}}),
            json!({"type": "assistant", "sessionId": new_id, "message": {"role": "assistant", "content": [{"type": "text", "text": reply}]}}),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(proj.join(format!("{new_id}.jsonl")), body).unwrap();
    };
    write_fork("working on it");
    t.app.panes[t.slot].tabs[0].session_id = None;
    t.app.detect_sessions();
    assert_ne!(
        t.app.panes[t.slot].tabs[0].session_id.as_deref(),
        Some(new_id.as_str()),
        "the fork is not the tab's session"
    );

    let job = dir.join("jobs").join(&new_id[..8]);
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(
        job.join("state.json"),
        json!({"state": "working", "sessionId": new_id, "cwd": cwd}).to_string(),
    )
    .unwrap();
    assert!(t.pump(5, |a| last_receipt(a)
        == Some(crate::receipts::Receipt::Started)));
    write_fork("The dark mode toggle is in settings.rs and tested.");
    std::fs::write(
        job.join("state.json"),
        json!({"state": "idle", "sessionId": new_id, "cwd": cwd}).to_string(),
    )
    .unwrap();
    assert!(t.pump(10, |a| matches!(
        followup(a).map(|f| f.state),
        Some(FuState::Answered { .. })
    )));
    let FuState::Answered { text, .. } = followup(&t.app).unwrap().state else {
        unreachable!()
    };
    assert!(
        text.contains("dark mode toggle is in settings.rs"),
        "{text}"
    );
    assert!(text.contains("background session"), "{text}");
    t.pump(3, |a| a.deliveries_json()[0]["state"] == "done");
    assert_eq!(t.app.deliveries_json()[0]["state"], "done");
}

#[test]
fn queue_mode_is_unchanged() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("queue", &[("bg_print", "x")], Activity::Working);
    t.said("tell it to add a toggle");
    let v = t.send(json!({"text": "What is the status?", "mode": "queue"}));
    assert!(v["error"].is_null(), "{v}");
    let id = t.last_id();
    let d = t.delivery(id);
    assert_eq!(d.rc.mode, Mode::Queue);
    assert!(!d.rc.mode.external());
    assert_eq!(t.app.queued_for(t.uid), 1, "waits for the tab");
    assert!(t.app.busy.runs.is_empty());
    t.pump(1, |_| false);
    assert!(!has(&t.stdin, "btw") && !has(&t.args, "--bg"));
    assert!(!has(&t.stdin, "What is the status"), "not typed while busy");

    t.app.panes[t.slot].tabs[0].activity = Activity::Ready;
    let stdin = t.stdin.clone();
    assert!(t.pump(5, |_| has(&stdin, "What is the status?")));
    assert_eq!(t.app.deliveries_json()[0]["mode"], "queue");
    assert!(t.app.deliveries_state_lines().contains("[queue]"));
}

#[test]
fn an_idle_tab_is_unchanged() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("idle", &[("bg_print", "x")], Activity::Ready);
    for (i, text) in ["What is the status?", "Add a toggle to the page"]
        .iter()
        .enumerate()
    {
        let v = t.send(json!({"text": text}));
        assert!(v["error"].is_null(), "{v}");
        let d = t.delivery(t.last_id());
        assert_eq!(d.rc.mode, Mode::Direct, "{i}");
        let stdin = t.stdin.clone();
        assert!(t.pump(5, |_| has(&stdin, text)), "{text}");
        t.app.deliveries.clear();
    }
    assert!(!has(&t.stdin, "/btw") && !has(&t.args, "--bg"));
}

#[test]
fn explicit_modes_and_the_permission_case() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("explicit", &[("bg_print", "x")], Activity::Ready);
    let v = t.send(json!({"text": "run the tests", "mode": "background"}));
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(t.delivery(t.last_id()).rc.mode, Mode::Background);
    t.app.panes[t.slot].tabs[0].activity = Activity::Permission;
    let v = t.send(json!({"text": "what are you waiting for?", "mode": "side"}));
    assert!(v["error"].is_null(), "{v}");
    let d = t.delivery(t.last_id());
    assert_eq!(d.rc.mode, Mode::Background);
    assert!(d.rc.note.unwrap().contains("approval"));
    let v = t.send(json!({"text": "x", "mode": "later"}));
    assert!(
        v["error"].as_str().unwrap_or("").contains("mode must be"),
        "{v}"
    );
}

#[test]
fn a_failed_background_launch_falls_back_to_queue() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("bgfail", &[("bg_fail", "not logged in")], Activity::Working);
    t.said("have it add a toggle");
    let v = t.send(json!({"text": "Add a toggle to the page", "expect_reply": false}));
    assert!(v["error"].is_null(), "{v}");
    let id = t.last_id();
    assert!(t.pump(10, |a| a
        .deliveries
        .iter()
        .any(|d| d.id == id && d.rc.mode == Mode::Queue)));
    let d = t.delivery(id);
    let note = d.rc.note.clone().unwrap();
    assert!(
        note.contains("did not start") && note.contains("queued instead"),
        "{note}"
    );
    assert_eq!(t.app.queued_for(t.uid), 1);
    assert!(t.app.busy.runs.is_empty());
    assert_eq!(followup(&t.app).unwrap().external, None);
    t.app.panes[t.slot].tabs[0].activity = Activity::Ready;
    let stdin = t.stdin.clone();
    assert!(t.pump(5, |_| has(&stdin, "Add a toggle to the page")));
}

#[test]
fn the_side_channel_missing_falls_back_to_queue() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut t = setup("nobtw", &[], Activity::Working);
    let q = "What are you doing right now?";
    let v = t.send(json!({"text": q}));
    assert!(v["error"].is_null(), "{v}");
    let id = t.last_id();
    assert_eq!(t.delivery(id).rc.mode, Mode::Side);
    let stdin = t.stdin.clone();
    assert!(t.pump(5, |_| has(&stdin, "/btw")));
    t.screen("Unknown command: /btw\n> ");
    assert!(t.pump(5, |a| a
        .deliveries
        .iter()
        .any(|d| d.id == id && d.rc.mode == Mode::Queue)));
    assert!(t.delivery(id).rc.note.unwrap().contains("/btw"));
    assert_eq!(t.app.queued_for(t.uid), 1);
    drop(t);

    let mut t = setup("nopanel", &[], Activity::Working);
    let v = t.send(json!({"text": q}));
    assert!(v["error"].is_null(), "{v}");
    let id = t.last_id();
    t.screen("working...\n> ");
    assert!(t.pump(8, |a| a
        .deliveries
        .iter()
        .any(|d| d.id == id && d.rc.mode == Mode::Queue)));
    assert!(t.delivery(id).rc.note.unwrap().contains("did not show up"));
}

#[test]
fn a_grok_tab_queues_with_a_reason() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("grok", &[], Activity::Working);
    t.app.cfg.accounts[2].harness = "grok".into();
    let v = t.send(json!({"text": "What are you doing?"}));
    assert!(v["error"].is_null(), "{v}");
    let d = t.delivery(t.last_id());
    assert_eq!(d.rc.mode, Mode::Queue);
    assert!(d.rc.note.unwrap().contains("Grok"));
    assert!(t.app.busy.runs.is_empty());
    assert!(!has(&t.args, "--bg"));
}
