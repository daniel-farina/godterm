//! Remote Control's honest outcome, its retry and self heal, keeping it
//! on, and the assistant's kept session (resume), with a stub claude that
//! answers remote_control requests and fails resumes on demand.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::{App, AppEvent};
use crate::config::testing::LOCK as ENV_LOCK;

struct T {
    app: App,
    rx: Receiver<AppEvent>,
    home: PathBuf,
    args: PathBuf,
    rec: PathBuf,
}

impl Drop for T {
    fn drop(&mut self) {
        self.app.assistant.brain = None;
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn setup(tag: &str, opts: &[(&str, &str)], prep: impl FnOnce(&mut crate::config::Config)) -> T {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-rk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    prep(&mut cfg);
    let mut app = App::new(cfg, tx);
    app.accounts[0].login.source = Some(CredSource::Keychain);
    app.cfg.assistant.account = app.cfg.accounts[0].name.clone();
    let rec = home.join("brain-in.txt");
    let args = home.join("brain-args.txt");
    let mut o: Vec<(&str, String)> = vec![
        ("stdin_to", rec.display().to_string()),
        ("stdin_append", "1".into()),
        ("args_to", args.display().to_string()),
    ];
    for (k, v) in opts {
        o.push((k, v.to_string()));
    }
    let stub = crate::test_stub::claude(&home.join("brain"), &o);
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    app.cfg.save().unwrap();
    T {
        app,
        rx,
        home,
        args,
        rec,
    }
}

impl T {
    /// Brain events in, ticks run, until `done` (or `secs`).
    fn pump(&mut self, secs: u64, mut done: impl FnMut(&mut App) -> bool) -> bool {
        let t0 = Instant::now();
        loop {
            while let Ok(ev) = self.rx.try_recv() {
                if matches!(ev, AppEvent::Assistant(..)) {
                    self.app.handle(ev);
                }
            }
            self.app.remote_tick();
            if done(&mut self.app) {
                return true;
            }
            if t0.elapsed() > Duration::from_secs(secs) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn args(&self) -> String {
        std::fs::read_to_string(&self.args).unwrap_or_default()
    }

    fn said(&mut self, s: &str) {
        self.app.assistant_turn += 1;
        self.app.assistant.last_user = s.into();
        self.app.assistant.turn_reads = 0;
    }

    /// "turn on remote control", yes: the confirmed call's waiting reply.
    fn enable_by_tool(&mut self) -> Receiver<Value> {
        self.app.ensure_brain().unwrap();
        self.said("turn on remote control");
        let v = self
            .app
            .control_call("enable_remote_control", &json!({"_client": "brain"}));
        let tok = v["token"].as_str().unwrap().to_string();
        self.said("yes");
        self.app.assistant.busy = true;
        let r = self.app.control_call(
            "enable_remote_control",
            &json!({"_client": "brain", "confirm_token": tok}),
        );
        assert!(r.get("_wait").is_some(), "the reply waits: {r}");
        assert!(r["result"].is_null(), "nothing success looking yet: {r}");
        self.app.pending_job.take().unwrap()
    }
}

fn wait_reply(t: &mut T, rx: &Receiver<Value>, secs: u64) -> Value {
    let mut got = None;
    t.pump(secs, |_| {
        if let Ok(v) = rx.try_recv() {
            got = Some(v);
        }
        got.is_some()
    });
    got.expect("the tool reply")
}

#[test]
fn honest_failure_then_restart_and_say_so() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("fail", &[("rc_answers", "fail")], |_| {});
    let rx = t.enable_by_tool();
    let gen0 = t.app.assistant.brain.as_ref().unwrap().gen;
    let v = wait_reply(&mut t, &rx, 30);
    // Two attempts failed: an error, marked pending, that says the truth.
    let e = v["error"].as_str().unwrap();
    assert!(
        e.contains("did not turn on") && e.contains("initialization failed"),
        "{v}"
    );
    assert_eq!(v["pending"], json!(true));
    assert!(
        v["say"].as_str().unwrap().contains("restarting my claude"),
        "{v}"
    );
    assert!(v["next"].as_str().unwrap().contains("NOT on"));
    assert!(v.get("session_url").is_none() && t.app.assistant.remote.is_none());
    assert!(t.app.remote_state_line().contains("NOT on yet"));
    assert_eq!(t.app.remote_indicator().as_deref(), Some("Remote…"));
    // The turn ends: the brain restarts, the last try fails, it is said.
    t.app.assistant.busy = false;
    assert!(t.pump(30, |a| a.assistant.remote_flow.is_none()));
    assert_ne!(
        t.app.assistant.brain.as_ref().unwrap().gen,
        gen0,
        "restarted"
    );
    assert!(
        t.app
            .admin
            .said
            .iter()
            .any(|s| s == "Remote Control did not turn on: Remote Control initialization failed."),
        "{:?}",
        t.app.admin.said
    );
    assert!(t.app.assistant.remote.is_none());
    assert_eq!(t.app.remote_indicator(), None);
    let sent = std::fs::read_to_string(&t.rec).unwrap();
    assert_eq!(sent.matches("\"subtype\":\"remote_control\"").count(), 3);
}

#[test]
fn retry_then_success_gives_the_link() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("retry", &[("rc_answers", "fail,ok")], |_| {});
    let rx = t.enable_by_tool();
    let v = wait_reply(&mut t, &rx, 30);
    assert_eq!(v["on"], json!(true), "{v}");
    assert_eq!(
        v["session_url"],
        json!("https://claude.ai/code/session_stub1")
    );
    let say = v["say"].as_str().unwrap();
    assert!(
        say.contains("second try") && say.contains("https://claude.ai/code/session_stub1"),
        "{say}"
    );
    // bridge_state connected follows: the indicator says Remote.
    assert!(t.pump(5, |a| a
        .assistant
        .remote
        .as_ref()
        .is_some_and(|r| r.bridge == "connected")));
    assert_eq!(t.app.remote_indicator().as_deref(), Some("Remote"));
    assert_eq!(
        t.app.control_call("get_state", &json!({}))["result"]["remote_control"]["url"],
        json!("https://claude.ai/code/session_stub1")
    );
    // The indicator opens the link.
    crate::admin::OPENED.lock().unwrap().clear();
    t.app.open_remote_link();
    assert!(crate::admin::OPENED
        .lock()
        .unwrap()
        .iter()
        .any(|u| u == "https://claude.ai/code/session_stub1"));
    // Reconnecting shows as such.
    t.app.on_bridge_state("reconnecting");
    assert_eq!(t.app.remote_indicator().as_deref(), Some("Remote…"));
}

#[test]
fn restart_heals_a_replaced_claude() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("heal", &[("rc_answers", "fail,fail,ok")], |_| {});
    let rx = t.enable_by_tool();
    let v = wait_reply(&mut t, &rx, 30);
    assert_eq!(v["pending"], json!(true), "{v}");
    t.app.assistant.busy = false;
    assert!(t.pump(30, |a| a.assistant.remote.is_some()));
    assert!(
        t.app
            .admin
            .said
            .iter()
            .any(|s| s.starts_with("Remote Control is on (after restarting my claude process)")),
        "{:?}",
        t.app.admin.said
    );
}

#[test]
fn kept_on_turns_on_at_start_and_not_when_off() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Off: a tick starts nothing.
    {
        let mut t = setup("keepoff", &[("rc_answers", "ok")], |_| {});
        for _ in 0..5 {
            t.app.assistant_tick();
        }
        assert!(t.app.assistant.brain.is_none() && t.app.assistant.remote_flow.is_none());
        t.app.ensure_brain().unwrap();
        t.pump(1, |_| false);
        assert!(!std::fs::read_to_string(&t.rec)
            .unwrap_or_default()
            .contains("remote_control"));
    }
    let mut t = setup("keepon", &[("rc_answers", "ok")], |c| {
        c.assistant.remote_control = true
    });
    // Launch: the brain comes up and Remote Control turns on by itself.
    t.app.assistant_tick();
    assert!(t.app.assistant.brain.is_some());
    assert!(t.pump(10, |a| a
        .assistant
        .remote
        .as_ref()
        .is_some_and(|r| r.connected())));
    assert_eq!(t.app.remote_indicator().as_deref(), Some("Remote"));
    // Quiet: shown, not spoken.
    assert!(t.app.admin.said.is_empty(), "{:?}", t.app.admin.said);
    // New: a new brain, and it is back on with a new link.
    t.app.reset_assistant();
    t.app.remote_tick();
    assert!(t.pump(10, |a| a
        .assistant
        .remote
        .as_ref()
        .is_some_and(
            |r| r.url.as_deref() == Some("https://claude.ai/code/session_stub1")
        )));
}

#[test]
fn kept_on_comes_back_after_a_drop_and_rests_on_grok() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("drop", &[("rc_answers", "ok")], |c| {
        c.assistant.remote_control = true
    });
    t.app.assistant_tick();
    assert!(t.pump(10, |a| a.assistant.remote.is_some()));
    t.app
        .on_brain(crate::assistant::BrainEvent::Bridge("disconnected".into()));
    assert!(t.app.assistant.remote.is_none());
    assert!(t.app.assistant.log.iter().any(|e| e
        .text
        .contains("dropped (disconnected); turning it back on")));
    assert!(t.pump(10, |a| a
        .assistant
        .remote
        .as_ref()
        .is_some_and(
            |r| r.url.as_deref() == Some("https://claude.ai/code/session_stub1")
        )));
    // On Grok it rests.
    t.app.assistant.remote = None;
    t.app.cfg.assistant.provider = "grok".into();
    assert!(!t.app.remote_kept_on());
    assert!(t.app.remote_state_line().contains("not on this provider"));
}

#[test]
fn keeping_it_on_needs_two_yeses() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("risk", &[], |_| {});
    t.said("keep remote control on");
    let v = t.app.control_call(
        "set_setting",
        &json!({"_client": "brain", "key": "assistant.remote_control", "value": true}),
    );
    let tok = v["token"].as_str().expect("asks").to_string();
    t.said("yes");
    let v = t.app.control_call(
        "set_setting",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    let q = v["question"].as_str().unwrap_or_default();
    assert!(
        q.starts_with("Are you sure?") && q.contains("claude.ai account"),
        "{v}"
    );
    assert!(!t.app.cfg.assistant.remote_control, "not yet");
    let tok = v["token"].as_str().unwrap().to_string();
    t.said("yes");
    t.app.control_call(
        "set_setting",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert!(t.app.cfg.assistant.remote_control);
    // Turning it off is not risky: one yes.
    t.said("keep remote control off");
    let v = t.app.control_call(
        "set_setting",
        &json!({"_client": "brain", "key": "assistant.remote_control", "value": false}),
    );
    let tok = v["token"].as_str().unwrap().to_string();
    t.said("yes");
    t.app.control_call(
        "set_setting",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert!(!t.app.cfg.assistant.remote_control);
}

// ---------- the kept session ----------

fn transcript(t: &T, id: &str, bytes: usize) -> PathBuf {
    let d = t.app.cfg.accounts[0]
        .config_dir()
        .join("projects")
        .join(crate::app_resume::project_slug(&crate::assistant::dir()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(format!("{id}.jsonl"));
    std::fs::write(&p, "x".repeat(bytes)).unwrap();
    p
}

fn kept_id() -> String {
    crate::app_resume::load().unwrap().id
}

fn restart(t: &mut T) {
    t.app.assistant.brain = None;
    let _ = std::fs::remove_file(&t.args);
    t.app.ensure_brain().unwrap();
    t.pump(1, |_| false);
}

#[test]
fn resume_uses_the_kept_id_and_new_starts_another() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("resume", &[], |_| {});
    t.app.ensure_brain().unwrap();
    t.pump(1, |_| false);
    let id = kept_id();
    assert!(
        t.args().contains(&format!("--session-id {id}")),
        "{}",
        t.args()
    );
    assert!(!t.args().contains("--no-session-persistence"));
    assert!(t.app.assistant.brain_fresh, "a new session gets the memory");
    // A restart (a crash, an update) continues it.
    let p = transcript(&t, &id, 100);
    restart(&mut t);
    assert!(t.args().contains(&format!("--resume {id}")), "{}", t.args());
    assert!(!t.app.assistant.brain_fresh, "no memory block on a resume");
    // Kept out of the account's sessions.
    let mut cache = crate::sessions::SessionCache::default();
    std::fs::write(
        &p,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
    )
    .unwrap();
    assert!(cache.scan(&t.app.cfg.accounts[0].config_dir()).is_empty());
    // New: a new session, recorded; the old transcript goes.
    t.app.reset_assistant();
    restart(&mut t);
    let id2 = kept_id();
    assert_ne!(id, id2);
    assert!(t.args().contains(&format!("--session-id {id2}")));
    assert!(!p.exists());
    // Too large: fresh.
    transcript(&t, &id2, (crate::app_resume::MAX_RESUME_BYTES + 1) as usize);
    restart(&mut t);
    assert_ne!(kept_id(), id2);
    assert!(t.args().contains("--session-id"));
}

#[test]
fn a_failed_resume_starts_fresh() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("resfail", &[("resume_fail", "1")], |_| {});
    crate::app_resume::save(&crate::app_resume::Kept {
        id: "11111111-2222-4333-8444-555555555555".into(),
        provider: "claude".into(),
        account: t.app.cfg.accounts[0].name.clone(),
        live: false,
    });
    transcript(&t, "11111111-2222-4333-8444-555555555555", 10);
    t.app.ensure_brain().unwrap();
    // claude exits: the next start is a fresh session with the memory.
    assert!(t.pump(10, |a| a.assistant.brain.is_none()));
    assert!(t.args().contains("--resume 11111111"));
    assert!(t.app.assistant.fresh_session);
    restart(&mut t);
    assert!(t.args().contains("--session-id"), "{}", t.args());
    assert_ne!(kept_id(), "11111111-2222-4333-8444-555555555555");
    assert!(t.app.assistant.brain_fresh);
    // Another account's session is not resumed.
    let id = kept_id();
    let mut k = crate::app_resume::load().unwrap();
    k.account = "someone-else".into();
    crate::app_resume::save(&k);
    transcript(&t, &id, 10);
    restart(&mut t);
    assert!(!t.args().contains("--resume"));
}

#[test]
fn after_a_crash_it_resumes_and_remote_control_returns() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("crash", &[("rc_answers", "ok")], |c| {
        c.assistant.remote_control = true
    });
    // The last run left its session live (no clean exit).
    let id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    crate::app_resume::save(&crate::app_resume::Kept {
        id: id.into(),
        provider: "claude".into(),
        account: t.app.cfg.accounts[0].name.clone(),
        live: true,
    });
    transcript(&t, id, 10);
    t.app.assistant_tick();
    assert!(t
        .app
        .assistant
        .log
        .iter()
        .any(|e| e.text.contains("did not exit cleanly")));
    assert!(t.pump(10, |a| a
        .assistant
        .remote
        .as_ref()
        .is_some_and(|r| r.connected())));
    assert!(t.args().contains(&format!("--resume {id}")), "{}", t.args());
    assert!(t.app.remote_json()["url"]
        .as_str()
        .unwrap()
        .starts_with("https://claude.ai/code/session_stub"));
    // A clean exit is not a crash.
    crate::app_resume::mark_clean_exit();
    assert!(!crate::app_resume::load().unwrap().live);
}

#[test]
fn resume_off_keeps_nothing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("resoff", &[], |c| c.assistant.resume = false);
    let id = "aaaaaaaa-bbbb-4ccc-8ddd-ffffffffffff";
    crate::app_resume::save(&crate::app_resume::Kept {
        id: id.into(),
        provider: "claude".into(),
        account: t.app.cfg.accounts[0].name.clone(),
        live: true,
    });
    transcript(&t, id, 10);
    t.app.assistant_tick();
    assert!(t.app.assistant.brain.is_none(), "no eager start");
    t.app.ensure_brain().unwrap();
    t.pump(1, |_| false);
    assert!(
        t.args().contains("--no-session-persistence"),
        "{}",
        t.args()
    );
    assert!(!t.args().contains("--resume") && !t.args().contains("--session-id"));
}
