use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::{App, AppEvent};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::LaunchKind;

const SID: &str = "d34c6258-af8c-417b-8e66-53936c24b49f";
const JOB_SID: &str = "516d52e6-15ea-46d1-8318-ffaf31bc91cd";

struct T {
    app: App,
    rx: Receiver<AppEvent>,
    home: PathBuf,
    args: PathBuf,
}

impl Drop for T {
    fn drop(&mut self) {
        for p in &mut self.app.panes {
            p.kill_all();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn setup(tag: &str, opts: &[(&str, &str)]) -> T {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-bgl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    let args = home.join("args.txt");
    let mut o: Vec<(&str, String)> = vec![
        ("args_to", args.display().to_string()),
        ("sleep_s", "30".into()),
    ];
    for (k, v) in opts {
        o.push((k, v.to_string()));
    }
    let stub = crate::test_stub::claude(&home.join("bin"), &o);
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    T {
        app,
        rx,
        home,
        args,
    }
}

impl T {
    fn pump(&mut self, secs: u64, mut done: impl FnMut(&mut App) -> bool) -> bool {
        let t0 = Instant::now();
        loop {
            while let Ok(ev) = self.rx.try_recv() {
                if matches!(ev, AppEvent::PaneExited(..) | AppEvent::PaneOutput) {
                    self.app.handle(ev);
                }
            }
            self.app.bglock_tick();
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

    fn launches(&mut self, n: usize) -> bool {
        let p = self.args.clone();
        self.pump(10, |_| {
            std::fs::read_to_string(&p)
                .unwrap_or_default()
                .lines()
                .count()
                >= n
        })
    }

    fn said(&mut self, s: &str) {
        self.app.assistant_turn += 1;
        self.app.assistant.last_user = s.into();
    }

    fn cwd(&self) -> PathBuf {
        let d = self.home.join("claudego");
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

fn transcript(dir: &Path, cwd: &Path, id: &str, prompt: &str) -> PathBuf {
    let p = dir
        .join("projects")
        .join(crate::state::encode_project_dir(cwd));
    std::fs::create_dir_all(&p).unwrap();
    let f = p.join(format!("{id}.jsonl"));
    let lines = [
        json!({"type": "user", "cwd": cwd, "sessionId": id, "message": {"role": "user", "content": prompt}}),
        json!({"type": "assistant", "sessionId": id, "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&f, text).unwrap();
    f
}

fn bg_job(dir: &Path, cwd: &Path, short: &str, sid: &str, from: &Path) {
    let j = dir.join("jobs").join(short);
    std::fs::create_dir_all(&j).unwrap();
    std::fs::write(
        j.join("state.json"),
        json!({"state": "working", "sessionId": sid, "resumeSessionId": sid, "daemonShort": short, "cwd": cwd}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("daemon")).unwrap();
    std::fs::write(
        dir.join("daemon").join("roster.json"),
        json!({"workers": {short: {"sessionId": sid, "dispatch": {"launch": {"mode": "resume", "sessionId": from, "fork": true}}}}}).to_string(),
    )
    .unwrap();

    let p = dir
        .join("projects")
        .join(crate::state::encode_project_dir(cwd));
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(
        p.join(format!("{sid}.jsonl")),
        format!("{}\n", json!({"type": "ai-title", "aiTitle": "Multi-account Claude Code manager", "sessionId": sid})),
    )
    .unwrap();
}

fn ids(v: &Value) -> Vec<String> {
    v["result"]["sessions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| r["id"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn sessions_match_full_prefix_truncated_and_job_ids() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("ids", &[]);
    let cwd = t.cwd();
    let dir = t.app.cfg.accounts[2].config_dir();
    let main = transcript(&dir, &cwd, SID, "make sure we released");
    transcript(
        &dir,
        &cwd,
        "aaaa1111-0000-4000-8000-000000000001",
        "something else",
    );
    for q in [
        SID,
        "d34c6258",
        "d34c6258-af8c-417b-8e66-53936c24b49",
        "D34C6258",
    ] {
        let v = t
            .app
            .control_call("sessions", &json!({"text": q, "state": "all"}));
        assert_eq!(ids(&v), vec![SID.to_string()], "{q}: {v}");
    }

    bg_job(&dir, &cwd, "516d52e6", JOB_SID, &main);
    let v = t
        .app
        .control_call("sessions", &json!({"text": "516d52e6", "state": "all"}));
    let got = ids(&v);
    assert!(got.contains(&SID.to_string()), "{v}");
    assert_eq!(v["result"]["background_runs"][0]["job"], "516d52e6", "{v}");
    assert_eq!(v["result"]["background_runs"][0]["continues"], SID, "{v}");

    let v = t.app.control_call(
        "open_session",
        &json!({"id": "d34c6258-af8c-417b-8e66-53936c24b49"}),
    );
    assert_eq!(v["result"]["opened"], SID, "{v}");
    assert_eq!(v["result"]["account"], "Account 3", "{v}");

    transcript(&dir, &cwd, "d34c6258-0000-4000-8000-000000000009", "twin");
    t.app.kick_index(true);
    let v = t
        .app
        .control_call("open_session", &json!({"id": "d34c6258"}));
    assert!(
        v["error"]
            .as_str()
            .unwrap_or("")
            .contains("starts 2 sessions"),
        "{v}"
    );
}

#[test]
fn open_session_resumes_with_the_config_dir_that_holds_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("acct", &[]);
    let cwd = t.cwd();
    let a3 = t.app.cfg.accounts[2].config_dir();
    transcript(&a3, &cwd, SID, "held by account 3");
    let v = t
        .app
        .control_call("open_session", &json!({"id": SID, "account": 1}));
    assert_eq!(v["result"]["account"], "Account 3", "{v}");
    assert!(
        v["result"]["say"]
            .as_str()
            .unwrap()
            .contains("It lives on Account 3"),
        "{v}"
    );
    let (s, ti) = (t.app.focus, t.app.panes[t.app.focus].active);
    assert_eq!(t.app.panes[s].account, Some(2));
    assert_eq!(t.app.panes[s].tabs[ti].session_id.as_deref(), Some(SID));
    let want = format!("--resume {SID}");
    let args = t.args.clone();
    assert!(
        t.pump(5, |_| std::fs::read_to_string(&args)
            .unwrap_or_default()
            .contains(&want)),
        "{}",
        t.args()
    );
    for p in &mut t.app.panes {
        p.kill_all();
    }

    let other = "bbbb2222-0000-4000-8000-000000000002";
    let main = t.home.join("main-claude");
    let _m = crate::config::testing::main_dirs(&main, None);
    let a1 = t.app.cfg.accounts[0].config_dir();
    let old = transcript(&a1, &cwd, other, "old copy");
    let past = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(past)
        .unwrap();
    transcript(&main, &cwd, other, "newest turns");
    t.app.kick_index(true);
    let v = t
        .app
        .control_call("open_session", &json!({"id": other, "account": 1}));
    assert_eq!(v["result"]["account"], "Account 1", "{v}");
    let copied = std::fs::read_to_string(&old).unwrap();
    assert!(copied.contains("newest turns"), "{copied}");
    let trash = crate::config::app_home().join("trash");
    assert!(
        std::fs::read_dir(&trash)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(other)),
        "the older copy is kept"
    );
}

#[test]
fn a_background_lock_offers_attach_stop_and_fork() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("lock", &[("resume_bg", "516d52e6")]);
    let cwd = t.cwd();
    let a3 = t.app.cfg.accounts[2].config_dir();
    let main = transcript(&a3, &cwd, SID, "the coordinator");
    bg_job(&a3, &cwd, "516d52e6", JOB_SID, &main);
    let s = t.app.pane_for_account(2);
    t.app.panes[s].cur_mut().cwd = cwd.clone();
    t.app.panes[s].cur_mut().session_id = Some(SID.into());
    t.app.launch_tab(s, 0, LaunchKind::Resume(SID.into()));
    let uid = t.app.panes[s].tabs[0].uid;
    assert!(
        t.pump(10, |a| a.panes[s].tabs[0].bg_lock.is_some()),
        "the lock is seen: {}",
        t.app.panes[s].tabs[0].screen_text()
    );
    let lock = t.app.panes[s].tabs[0].bg_lock.clone().unwrap();
    assert_eq!(lock.short, "516d52e6");
    assert_eq!(lock.session_id.as_deref(), Some(SID));

    let bar = crate::session_ids::lock_choices("516d52e6");
    assert!(bar.contains("attaches") && bar.contains("stops") && bar.contains("forks"));
    let tid = format!("t{uid}");
    let r = t
        .app
        .control_call("read_tab", &json!({"tab": tid, "what": "screen"}));
    assert_eq!(r["result"]["tab"], json!(tid));
    assert_eq!(r["result"]["background_lock"]["job"], "516d52e6", "{r}");

    let v = t.app.control_call(
        "open_tab",
        &json!({"account": 3, "dir": cwd.display().to_string()}),
    );
    assert!(
        v["error"].as_str().unwrap_or("").contains("fix_resume"),
        "{v}"
    );

    t.said("fix it");
    let v = t
        .app
        .control_call("fix_resume", &json!({"tab": tid, "choice": "stop"}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    assert!(v["question"]
        .as_str()
        .unwrap()
        .contains("may be doing work"));
    let tok = v["token"].as_str().unwrap().to_string();
    t.said("yes");
    let v = t
        .app
        .control_call("fix_resume", &json!({"confirm_token": tok}));
    assert_eq!(v["result"]["choice"], "stop", "{v}");
    assert_eq!(v["result"]["account"], "Account 3", "{v}");
    assert!(t.launches(3), "resumed after the stop: {}", t.args());
    let args = t.args();
    assert!(args.lines().any(|l| l == "stop 516d52e6"), "{args}");
    assert_eq!(
        args.lines()
            .filter(|l| l.contains(&format!("--resume {SID}")))
            .count(),
        2,
        "the same session, resumed again: {args}"
    );
    assert_eq!(t.app.panes[s].tabs[0].uid, uid, "the same tab");
}

#[test]
fn attach_and_fork_stay_in_the_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("attach", &[("resume_bg", "516d52e6")]);
    let cwd = t.cwd();
    let s = t.app.pane_for_account(2);
    t.app.panes[s].cur_mut().cwd = cwd.clone();
    t.app.panes[s].cur_mut().session_id = Some(SID.into());
    t.app.launch_tab(s, 0, LaunchKind::Resume(SID.into()));
    assert!(t.pump(10, |a| a.panes[s].tabs[0].bg_lock.is_some()));
    let tid = format!("t{}", t.app.panes[s].tabs[0].uid);
    let v = t.app.control_call("fix_resume", &json!({"tab": tid}));
    assert_eq!(v["result"]["choice"], "attach", "{v}");
    assert!(t.launches(2), "{}", t.args());
    assert!(
        t.args().lines().any(|l| l == "attach 516d52e6"),
        "{}",
        t.args()
    );
    assert_eq!(t.app.panes[s].tabs[0].session_id.as_deref(), Some(SID));

    t.app.panes[s].tabs[0].kill();
    t.app.launch_tab(s, 0, LaunchKind::Resume(SID.into()));
    assert!(t.pump(10, |a| a.panes[s].tabs[0].bg_lock.is_some()));
    t.app.focus = s;
    t.app.panes[s].active = 0;
    t.app.bg_lock_key(
        s,
        0,
        crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('f')),
    );
    assert!(t.launches(4), "{}", t.args());
    assert!(
        t.args()
            .lines()
            .any(|l| l.contains(&format!("--resume {SID}")) && l.ends_with("--fork-session")),
        "{}",
        t.args()
    );
    assert!(!t.args().contains("stop "), "nothing was stopped");
}

#[test]
fn a_background_fork_is_not_the_tabs_session() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let t = setup("detect", &[]);
    let cwd = t.cwd();
    let dir = t.app.cfg.accounts[2].config_dir();
    let since = std::time::SystemTime::now() - Duration::from_secs(60);
    let main = transcript(&dir, &cwd, SID, "the coordinator");
    std::thread::sleep(Duration::from_millis(20));
    bg_job(&dir, &cwd, "516d52e6", JOB_SID, &main);
    assert_eq!(
        crate::state::detect_session(&dir, &cwd, since, &[]).as_deref(),
        Some(SID)
    );
}

#[test]
fn read_tab_returns_the_asked_tab_after_churn() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("read", &[]);
    let a = t.app.pane_for_account(0);
    let b = t.app.pane_for_account(2);

    t.app.panes[a].add_tab(t.home.clone());
    let src = t.app.panes[a].tabs[0].uid;
    let other = t.app.panes[a].tabs[1].uid;
    t.app.panes[b].add_tab(t.home.clone());
    let dst = t.app.panes[b].tabs.last().unwrap().uid;
    t.app.moved_ids.insert(src, dst);
    t.app.pending_moves.push(crate::app_tabmove::PendingMove {
        source_uid: src,
        target_uid: dst,
        copy: false,
        since: Instant::now() - Duration::from_secs(2),
        name: "godterm".into(),
        target: 2,
    });

    assert_eq!(t.app.current_uid(src), dst);
    t.app.tabmove_tick();
    assert!(t.app.pending_moves.is_empty(), "the move failed");
    {
        let mut p = t.app.panes[a].tabs[0].parser.lock().unwrap();
        p.process(b"THE SOURCE TAB");
    }
    {
        let ti = t.app.panes[b].tabs.len() - 1;
        let mut p = t.app.panes[b].tabs[ti].parser.lock().unwrap();
        p.process(b"THE FAILED COPY");
    }
    for (asked, want) in [(src, "THE SOURCE TAB"), (dst, "THE FAILED COPY")] {
        let r = t.app.control_call(
            "read_tab",
            &json!({"tab": format!("t{asked}"), "what": "screen"}),
        );
        assert_eq!(r["result"]["tab"], json!(format!("t{asked}")), "{r}");
        assert!(
            r["result"]["screen"].as_str().unwrap().contains(want),
            "{r}"
        );
    }

    t.app.moved_ids.insert(src, dst);
    let r = t.app.control_call(
        "read_tab",
        &json!({"tab": format!("t{src}"), "what": "screen"}),
    );
    assert_eq!(r["result"]["tab"], json!(format!("t{src}")), "{r}");

    t.app.panes[a].close_tab(1, t.home.clone());
    let fresh = t.app.panes[a].add_tab(t.home.clone());
    let fuid = t.app.panes[a].tabs[fresh].uid;
    assert_ne!(fuid, other);
    let r = t.app.control_call(
        "read_tab",
        &json!({"tab": format!("t{other}"), "what": "screen"}),
    );
    assert!(r["error"].as_str().unwrap_or("").contains("no tab"), "{r}");
    let r = t.app.control_call(
        "read_tab",
        &json!({"tab": format!("t{fuid}"), "what": "screen"}),
    );
    assert_eq!(r["result"]["tab"], json!(format!("t{fuid}")), "{r}");

    t.app.panes[a].close_tab(0, t.home.clone());
    let r = t.app.control_call(
        "read_tab",
        &json!({"tab": format!("t{src}"), "what": "screen"}),
    );
    assert_eq!(r["result"]["tab"], json!(format!("t{dst}")), "{r}");
    assert_eq!(r["result"]["moved_from"], json!(format!("t{src}")), "{r}");
}
