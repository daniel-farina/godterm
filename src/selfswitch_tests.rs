//! The assistant watching its own account: the warning once per window,
//! the move at the switch level (5 hour and weekly) or on a usage limit
//! error with the turn sent again, the choice of account, the session
//! copied and resumed there (or a fuller summary when that fails), the
//! account saved, Remote Control coming back, everyone out, ask and off.
//! A stub claude and fake usage numbers; nothing real is touched.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::app::{App, AppEvent};
use crate::assistant::BrainEvent;
use crate::config::testing::LOCK as ENV_LOCK;

fn usage(five_used: f64, week_used: f64) -> crate::usage::Usage {
    crate::usage::parse_usage(&format!(
        r#"{{"five_hour":{{"utilization":{five_used},"resets_at":"2099-10-09T17:29:00+00:00"}},"seven_day":{{"utilization":{week_used},"resets_at":"2099-10-16T13:00:00+00:00"}}}}"#
    ))
    .unwrap()
}

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

fn setup(tag: &str, opts: &[(&str, &str)]) -> T {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-ss-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, rx) = std::sync::mpsc::channel();
    let cfg = crate::config::Config::default();
    let mut app = App::new(cfg, tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
        app.accounts[a].usage = Some(usage(20.0, 20.0));
    }
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
    fn pump(&mut self, secs: u64, mut done: impl FnMut(&mut App) -> bool) -> bool {
        let t0 = Instant::now();
        loop {
            while let Ok(ev) = self.rx.try_recv() {
                if matches!(ev, AppEvent::Assistant(..)) {
                    self.app.handle(ev);
                }
            }
            self.app.remote_tick();
            self.app.self_switch_tick();
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
    fn sent(&self) -> String {
        std::fs::read_to_string(&self.rec).unwrap_or_default()
    }
    fn on(&self) -> Option<usize> {
        self.app.assistant.brain.as_ref().and_then(|b| b.account)
    }
    fn notes(&self, s: &str) -> usize {
        self.app
            .assistant
            .log
            .iter()
            .filter(|e| e.text.contains(s))
            .count()
    }
    /// A turn ends (as the stub never answers).
    fn done(&mut self, text: &str, error: bool) {
        self.app.on_brain(BrainEvent::Done {
            text: text.into(),
            cost: None,
            error,
        });
    }
    fn saved_account(&self) -> String {
        let t = std::fs::read_to_string(crate::config::Config::path()).unwrap();
        let v: toml::Value = toml::from_str(&t).unwrap();
        v["assistant"]["account"].as_str().unwrap_or("").to_string()
    }
}

fn transcript(app: &App, a: usize, id: &str, body: &str) -> PathBuf {
    let d = app.cfg.accounts[a]
        .config_dir()
        .join("projects")
        .join(crate::app_resume::project_slug(&crate::assistant::dir()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(format!("{id}.jsonl"));
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn warns_once_at_the_threshold() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("warn", &[]);
    t.app.accounts[0].usage = Some(usage(88.0, 20.0));
    t.app.accounts[2].usage = Some(usage(10.0, 10.0));
    for _ in 0..5 {
        t.app.self_switch_tick();
    }
    assert_eq!(
        t.notes("My account, Account 1, has 12% left in its 5 hour window"),
        1,
        "{:?}",
        t.app.assistant.log
    );
    assert_eq!(t.notes("I'll move to Account 3 (90% left) at 5%."), 1);
    // The weekly limit binds: said so.
    t.app.accounts[0].usage = Some(usage(20.0, 90.0));
    t.app.self_switch_tick();
    t.app.self_switch_tick();
    assert_eq!(t.notes("has 10% left this week"), 1);
    // Nothing moved for a warning.
    assert_eq!(t.app.cfg.assistant.account, "account1");
    assert_eq!(
        t.app.self_switch_json()["left_pct"],
        serde_json::json!(10.0)
    );
    assert_eq!(
        t.app.self_switch_json()["limited_by"],
        serde_json::json!("weekly")
    );
}

#[test]
fn moves_at_five_percent_on_either_limit_and_saves_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("auto", &[]);
    t.app.ensure_brain().unwrap();
    assert_eq!(t.on(), Some(0));
    // 5 hour: 96% used.
    t.app.accounts[0].usage = Some(usage(96.0, 20.0));
    t.app.accounts[1].usage = Some(usage(10.0, 10.0));
    t.app.self_switch_tick();
    assert_eq!(t.on(), Some(1), "{:?}", t.app.assistant.log);
    assert_eq!(t.app.cfg.assistant.account, "account2");
    assert_eq!(t.saved_account(), "account2");
    assert_eq!(crate::app_resume::load().unwrap().account, "account2");
    assert!(t.notes("so I moved myself to Account 2 (90% left)") >= 1);
    // Weekly: 96% used while the 5 hour window is fine.
    t.app.accounts[1].usage = Some(usage(10.0, 96.0));
    t.app.accounts[0].usage = Some(usage(10.0, 10.0));
    t.app.self_switch_tick();
    assert_eq!(t.on(), Some(0));
    assert!(t.notes("has 4% left this week") >= 1);
    assert_eq!(t.saved_account(), "account1");
    // Not in the middle of a turn.
    t.app.accounts[0].usage = Some(usage(99.0, 10.0));
    t.app.assistant.busy = true;
    t.app.self_switch_tick();
    assert_eq!(t.on(), Some(0));
    t.app.assistant.busy = false;
    t.app.self_switch_tick();
    assert_ne!(t.on(), Some(0));
}

#[test]
fn a_usage_limit_error_moves_and_sends_the_turn_again() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("limit", &[]);
    t.app.accounts[3].usage = Some(usage(5.0, 5.0));
    t.app.ask_assistant("what is the build doing");
    assert!(t.app.assistant.busy);
    assert_eq!(t.on(), Some(0));
    // The stub has read it.
    let rec = t.rec.clone();
    assert!(t.pump(5, |_| std::fs::read_to_string(&rec)
        .unwrap_or_default()
        .contains("what is the build doing")));
    t.done("Claude AI usage limit reached|1760000000", true);
    assert_eq!(t.on(), Some(3), "{:?}", t.app.assistant.log);
    assert!(t.app.assistant.busy, "the turn went out again");
    t.pump(2, |_| false);
    assert_eq!(
        t.sent()
            .lines()
            .filter(|l| l.contains("what is the build doing"))
            .count(),
        2
    );
    // Shown once, and the error is not the answer.
    assert_eq!(
        t.app
            .assistant
            .log
            .iter()
            .filter(|e| e.text == "what is the build doing")
            .count(),
        1
    );
    assert_eq!(t.notes("usage limit reached|"), 0);
    assert!(t.notes("hit its usage limit, so I moved myself to Account 4") >= 1);
    // A rate limit error counts too.
    t.app.accounts[0].usage = Some(usage(1.0, 1.0));
    t.done("API Error: 429 {\"type\":\"rate_limit_error\"}", true);
    assert_eq!(t.on(), Some(0));
    // An ordinary error does not move it.
    t.app.ask_assistant("again");
    t.done("something else broke", true);
    assert_eq!(t.on(), Some(0));
}

#[test]
fn the_best_account_skips_exhausted_logged_out_and_erroring_ones() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("pick", &[]);
    // 1 runs out; 2 is fine on 5 hours but out for the week; 3 is logged
    // out (the most left); 4 errors on its token.
    t.app.accounts[0].usage = Some(usage(97.0, 10.0));
    t.app.accounts[1].usage = Some(usage(0.0, 99.0));
    t.app.accounts[2].usage = Some(usage(0.0, 0.0));
    t.app.accounts[2].login.source = None;
    t.app.accounts[3].usage = Some(usage(1.0, 1.0));
    t.app.accounts[3].usage_err = Some(crate::usage::UsageError::Unauthorized);
    assert_eq!(t.app.self_switch_target(0), None);
    t.app.accounts[3].usage_err = Some(crate::usage::UsageError::RateLimited(None));
    assert_eq!(t.app.self_switch_target(0).map(|x| x.0), Some(3));
    t.app.accounts[3].usage_err = None;
    t.app.accounts[1].usage = Some(usage(0.0, 50.0));
    assert_eq!(t.app.self_switch_target(0).map(|x| x.0), Some(3));
    t.app.accounts[3].usage = Some(usage(60.0, 1.0));
    assert_eq!(t.app.self_switch_target(0).map(|x| x.0), Some(1));
}

#[test]
fn the_conversation_comes_along() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("keep", &[]);
    t.app.ensure_brain().unwrap();
    t.pump(1, |_| false);
    let id = crate::app_resume::load().unwrap().id;
    assert!(t.args().contains(&format!("--session-id {id}")));
    let body = "{\"type\":\"user\",\"sessionId\":\"x\",\"message\":{\"content\":\"the code word is plum\"}}\n";
    transcript(&t.app, 0, &id, body);
    let _ = std::fs::remove_file(&t.args);
    t.app.accounts[0].usage = Some(usage(97.0, 10.0));
    t.app.accounts[2].usage = Some(usage(1.0, 1.0));
    t.app.self_switch_tick();
    assert_eq!(t.on(), Some(2));
    t.pump(1, |_| false);
    // Resumed by the same id on the new account, its transcript copied.
    assert!(t.args().contains(&format!("--resume {id}")), "{}", t.args());
    let there = t.app.cfg.accounts[2]
        .config_dir()
        .join("projects")
        .join(crate::app_resume::project_slug(&crate::assistant::dir()))
        .join(format!("{id}.jsonl"));
    assert_eq!(std::fs::read_to_string(&there).unwrap(), body);
    let k = crate::app_resume::load().unwrap();
    assert_eq!(
        (k.id.as_str(), k.account.as_str()),
        (id.as_str(), "account3")
    );
    assert!(!t.app.assistant.brain_fresh, "no memory block on a resume");
    // "Move yourself to account two": the same path, Remote Control or not.
    t.app.assistant_turn += 1;
    t.app.assistant.last_user = "move yourself to account two".into();
    let v = t.app.control_call(
        "switch_assistant",
        &serde_json::json!({"_client": "brain", "account": 2}),
    );
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(t.on(), Some(1));
    assert_eq!(crate::app_resume::load().unwrap().id, id);
    assert_eq!(t.saved_account(), "account2");
}

#[test]
fn a_failed_resume_after_a_move_starts_fresh_with_a_fuller_summary() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("fallback", &[("resume_fail", "1")]);
    t.app.ask_assistant("remember the code word plum");
    t.done("Noted: plum.", false);
    let id = crate::app_resume::load().unwrap().id;
    transcript(&t.app, 0, &id, "{}\n");
    t.app.accounts[0].usage = Some(usage(97.0, 10.0));
    t.app.accounts[1].usage = Some(usage(1.0, 1.0));
    t.app.self_switch_tick();
    assert_eq!(t.app.cfg.assistant.account, "account2");
    // The resume fails: a fresh session with the summary.
    assert!(t.pump(10, |a| a.assistant.brain.is_none()));
    assert!(t.args().contains(&format!("--resume {id}")));
    assert!(t.app.assistant.fresh_session);
    let carry = t.app.assistant.carry.clone().unwrap_or_default();
    assert!(
        carry.contains("remember the code word plum") && carry.contains("Noted: plum."),
        "{carry}"
    );
    // The next request starts it fresh with that summary.
    t.app.ask_assistant("what was the word");
    t.pump(2, |_| false);
    assert!(t.sent().contains("Earlier in this conversation"));
    assert_eq!(t.on(), Some(1));
}

#[test]
fn remote_control_comes_back_on_the_new_account() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("remote", &[("rc_answers", "ok")]);
    t.app.enable_remote("GodTerm assistant").unwrap();
    assert!(t.pump(10, |a| a.assistant.remote.is_some()));
    let url0 = t.app.assistant.remote.as_ref().unwrap().url.clone();
    t.app.accounts[0].usage = Some(usage(97.0, 10.0));
    t.app.accounts[1].usage = Some(usage(1.0, 1.0));
    t.app.self_switch_tick();
    assert_eq!(t.on(), Some(1));
    assert!(t.notes("Remote Control is coming back there") >= 1);
    assert!(t.pump(10, |a| a.assistant.selfswitch.last_remote.is_some()));
    let r = t.app.assistant.remote.clone().unwrap();
    assert_eq!(r.account, Some(1));
    assert_ne!(r.url, url0);
    let said = t.app.assistant.selfswitch.last_remote.clone().unwrap();
    assert!(
        said.starts_with("Remote Control moved to Account 2: https://claude.ai/code/session_stub"),
        "{said}"
    );
    let st = t.app.state_json();
    assert_eq!(st["remote_control"]["account"], serde_json::json!(2));
    assert!(st["assistant_account"]["remote_control"]
        .as_str()
        .unwrap()
        .contains("moved to Account 2"));
}

#[test]
fn everyone_out_says_so_once_and_retries() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("allout", &[]);
    for a in 0..4 {
        t.app.accounts[a].usage = Some(usage(97.0, 10.0));
    }
    t.app.ensure_brain().unwrap();
    for _ in 0..3 {
        t.app.self_switch_tick();
    }
    assert_eq!(
        t.notes("no other account has room"),
        1,
        "{:?}",
        t.app.assistant.log
    );
    assert_eq!(
        t.notes("The soonest reset is Account 1's 5 hour limit at"),
        1
    );
    assert_eq!(t.notes("I keep running on Account 1 until it stops me."), 1);
    assert_eq!(t.on(), Some(0));
    // Hard blocked: the turn waits and is tried again now and then.
    t.app.assistant.selfswitch.retry_every = Some(Duration::from_millis(100));
    t.app.ask_assistant("deploy status please");
    t.done("You've hit your limit · resets 5pm", true);
    assert!(!t.app.assistant.busy);
    assert!(t.app.assistant.selfswitch.blocked);
    assert_eq!(
        t.notes("hit its usage limit, and no other account has room"),
        1
    );
    std::thread::sleep(Duration::from_millis(150));
    t.app.self_switch_tick();
    assert!(t.app.assistant.busy, "sent again");
    t.pump(1, |_| false);
    assert_eq!(
        t.sent()
            .lines()
            .filter(|l| l.contains("deploy status please"))
            .count(),
        2
    );
    // Room comes back on another account: it moves there with the turn.
    t.done("You've hit your limit · resets 5pm", true);
    t.app.accounts[2].usage = Some(usage(0.0, 10.0));
    t.app.self_switch_tick();
    assert_eq!(t.on(), Some(2));
    t.pump(1, |_| false);
    assert_eq!(
        t.sent()
            .lines()
            .filter(|l| l.contains("deploy status please"))
            .count(),
        3
    );
}

#[test]
fn ask_mode_has_yes_and_no_buttons() {
    use crate::confirm_click_tests::{click, draw, term};
    use crate::hits::UiAction;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for yes in [true, false] {
        let mut t = setup(if yes { "askyes" } else { "askno" }, &[]);
        t.app.cfg.assistant.auto_switch = "ask".into();
        t.app.assistant.show = true;
        t.app.ensure_brain().unwrap();
        t.app.accounts[0].usage = Some(usage(97.0, 10.0));
        t.app.accounts[1].usage = Some(usage(1.0, 1.0));
        t.app.self_switch_tick();
        t.app.self_switch_tick();
        assert_eq!(t.on(), Some(0), "asks first");
        assert_eq!(t.notes("Move me to Account 2 (99% left)?"), 1);
        let mut term = term();
        draw(&mut t.app, &mut term);
        click(&mut t.app, &UiAction::AssistantAnswer(yes));
        if yes {
            assert_eq!(t.on(), Some(1));
            assert_eq!(t.saved_account(), "account2");
        } else {
            assert_eq!(t.on(), Some(0));
            assert!(t.notes("Staying on Account 1.") >= 1);
            t.app.self_switch_tick();
            assert!(t.app.assistant.selfswitch.asked.is_none(), "asked once");
        }
        // Nothing went to the brain as a "yes" or "no".
        t.pump(1, |_| false);
        assert!(!t.sent().contains("\"yes") && !t.sent().contains("\"no"));
    }
    // A spoken yes answers it too.
    let mut t = setup("askvoice", &[]);
    t.app.cfg.assistant.auto_switch = "ask".into();
    t.app.accounts[0].usage = Some(usage(10.0, 97.0));
    t.app.accounts[3].usage = Some(usage(1.0, 1.0));
    t.app.self_switch_tick();
    t.app.ask_assistant_from("yes", Some("yes"));
    assert_eq!(t.app.cfg.assistant.account, "account4");
}

#[test]
fn off_does_nothing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("off", &[]);
    t.app.cfg.assistant.auto_switch = "off".into();
    t.app.ensure_brain().unwrap();
    t.app.accounts[0].usage = Some(usage(99.0, 99.0));
    let n = t.app.assistant.log.len();
    t.app.self_switch_tick();
    assert_eq!(t.on(), Some(0));
    assert_eq!(t.app.assistant.log.len(), n);
    t.app.ask_assistant("hello");
    t.done("Claude AI usage limit reached", true);
    assert_eq!(t.on(), Some(0));
    assert!(
        t.notes("usage limit reached") >= 1,
        "the error is the answer"
    );
    assert_eq!(t.saved_account(), "account1");
}

#[test]
fn the_fake_override_is_per_account() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let t = setup("fake", &[]);
    std::env::set_var(crate::app_selfswitch::FAKE_ENV, "account2:3:weekly");
    let k = t.app.self_binding(1).unwrap();
    let other = t.app.self_binding(0).unwrap();
    std::env::remove_var(crate::app_selfswitch::FAKE_ENV);
    assert_eq!((k.left_pct, k.long.as_str()), (3.0, "weekly"));
    assert_eq!(other.left_pct, 80.0);
    assert!(crate::app_selfswitch::limit_said(
        "Claude AI usage limit reached|1760000000"
    ));
    assert!(!crate::app_selfswitch::limit_said("No such file"));
}
