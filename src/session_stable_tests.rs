use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;

use crate::app::{App, AppEvent};
use crate::app_loops::{durable_rows, DurableTab, LoopRow};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::LaunchKind;
use crate::state;

const LONG: &str = "aaaa1111-0000-4000-8000-000000000001";
const OTHER: &str = "bbbb2222-0000-4000-8000-000000000002";
const FRESH: &str = "cccc3333-0000-4000-8000-000000000003";
const NEXT: &str = "dddd4444-0000-4000-8000-000000000004";

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

fn setup(tag: &str) -> T {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-sid-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut app = App::new(crate::config::Config::default(), tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    let args = home.join("args.txt");
    let o: Vec<(&str, String)> = vec![
        ("args_to", args.display().to_string()),
        ("sleep_s", "30".into()),
    ];
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
    fn args(&self) -> String {
        std::fs::read_to_string(&self.args).unwrap_or_default()
    }

    fn launches(&mut self, n: usize) -> bool {
        let t0 = Instant::now();
        loop {
            while let Ok(ev) = self.rx.try_recv() {
                if matches!(ev, AppEvent::PaneExited(..) | AppEvent::PaneOutput) {
                    self.app.handle(ev);
                }
            }
            if self.args().lines().count() >= n {
                return true;
            }
            if t0.elapsed() > Duration::from_secs(10) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn folder(&self) -> PathBuf {
        let d = self.home.join("work");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn dir(&self) -> PathBuf {
        let a = self.app.panes[0].account.unwrap();
        self.app.cfg.accounts[a].config_dir()
    }

    fn sid(&self, tab: usize) -> Option<String> {
        self.app.panes[0].tabs[tab].session_id.clone()
    }

    fn resume(&mut self, tab: usize, cwd: &Path, id: &str) {
        while self.app.panes[0].tabs.len() <= tab {
            self.app.panes[0].add_tab(cwd.to_path_buf());
        }
        self.app.panes[0].tabs[tab].cwd = cwd.to_path_buf();
        self.app.panes[0].tabs[tab].session_id = Some(id.into());
        self.app.launch_tab(0, tab, LaunchKind::Resume(id.into()));
    }

    fn fresh(&mut self, tab: usize, cwd: &Path) {
        while self.app.panes[0].tabs.len() <= tab {
            self.app.panes[0].add_tab(cwd.to_path_buf());
        }
        self.app.panes[0].tabs[tab].cwd = cwd.to_path_buf();
        self.app.launch_tab(0, tab, LaunchKind::Normal);
    }

    fn register(&self, tab: usize, id: &str) {
        let pid = self.app.panes[0].tabs[tab].pid().unwrap();
        let d = self.dir().join("sessions");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(format!("{pid}.json")),
            json!({"pid": pid, "sessionId": id, "cwd": self.folder(), "kind": "interactive"})
                .to_string(),
        )
        .unwrap();
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("godterm.log")).unwrap_or_default()
    }
}

fn transcript(dir: &Path, cwd: &Path, id: &str, parent: Option<&str>) -> PathBuf {
    let p = dir.join("projects").join(state::encode_project_dir(cwd));
    std::fs::create_dir_all(&p).unwrap();
    let f = p.join(format!("{id}.jsonl"));
    let mut first = json!({"type": "user", "cwd": cwd, "sessionId": id, "message": {"role": "user", "content": "hello"}});
    if let Some(parent) = parent {
        first["forkedFrom"] = json!({"sessionId": parent, "messageUuid": "m-1"});
    }
    let lines = [
        first,
        json!({"type": "assistant", "sessionId": id, "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&f, text).unwrap();
    f
}

fn slashed(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}/", p.display()))
}

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("godterm-sidu-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_trailing_slash_encodes_like_the_plain_folder() {
    let plain = state::encode_project_dir(Path::new("/home/sam"));
    assert_eq!(plain, "-home-sam");
    assert_eq!(state::encode_project_dir(Path::new("/home/sam/")), plain);
    assert_eq!(state::encode_project_dir(Path::new("/home/sam//")), plain);
    assert_eq!(state::encode_project_dir(Path::new("/home/./sam/.")), plain);
    assert_eq!(state::encode_project_dir(Path::new("//home//sam")), plain);
    assert_eq!(
        state::encode_project_dir(Path::new("/home/sam/my.work_dir/")),
        "-home-sam-my-work-dir"
    );
    assert_eq!(state::encode_project_dir(Path::new("/")), "-");
    assert_eq!(
        state::normalize_cwd(Path::new("/home/sam/")),
        PathBuf::from("/home/sam")
    );
}

#[test]
fn only_a_transcript_created_after_the_start_is_a_candidate() {
    let root = temp("born");
    let cwd = Path::new("/home/sam/proj/");
    let old = transcript(&root, cwd, LONG, None);
    let since = SystemTime::now() + Duration::from_secs(30);
    let f = std::fs::File::options().write(true).open(&old).unwrap();
    f.set_modified(since + Duration::from_secs(30)).unwrap();
    drop(f);
    assert_eq!(state::detect_session(&root, cwd, since, &[]), None);
    let before = SystemTime::now() - Duration::from_secs(30);
    assert_eq!(
        state::detect_session(&root, Path::new("/home/sam/proj"), before, &[]).as_deref(),
        Some(LONG)
    );
    assert_eq!(
        state::detect_session(&root, cwd, before, &[LONG.to_string()]),
        None
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_link_to_the_previous_session_is_read_from_the_transcript() {
    let root = temp("link");
    let cwd = Path::new("/home/sam/proj");
    let before = SystemTime::now() - Duration::from_secs(30);
    let plain = transcript(&root, cwd, OTHER, None);
    let linked = transcript(&root, cwd, NEXT, Some(LONG));
    assert_eq!(state::transcript_parent(&plain), None);
    assert_eq!(state::transcript_parent(&linked).as_deref(), Some(LONG));
    assert_eq!(
        state::linked_session(&root, cwd, before, &[], LONG).as_deref(),
        Some(NEXT)
    );
    assert_eq!(state::linked_session(&root, cwd, before, &[], OTHER), None);
    assert_eq!(
        state::linked_session(&root, cwd, before, &[NEXT.to_string()], LONG),
        None
    );
    let later = SystemTime::now() + Duration::from_secs(30);
    assert_eq!(state::linked_session(&root, cwd, later, &[], LONG), None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_registry_names_the_session_of_one_process_only() {
    let root = temp("reg");
    let d = root.join("sessions");
    std::fs::create_dir_all(&d).unwrap();
    let now = SystemTime::now();
    let write = |pid: u32, v: serde_json::Value| {
        std::fs::write(d.join(format!("{pid}.json")), v.to_string()).unwrap()
    };
    write(
        4101,
        json!({"pid": 4101, "sessionId": LONG, "kind": "interactive"}),
    );
    write(4102, json!({"pid": 9, "sessionId": OTHER}));
    write(4103, json!({"pid": 4103, "sessionId": OTHER, "kind": "bg"}));
    write(4104, json!({"pid": 4104, "sessionId": "../escape"}));
    assert_eq!(
        state::registered_session(&root, 4101, now).as_deref(),
        Some(LONG)
    );
    assert_eq!(state::registered_session(&root, 4102, now), None);
    assert_eq!(state::registered_session(&root, 4103, now), None);
    assert_eq!(state::registered_session(&root, 4104, now), None);
    assert_eq!(state::registered_session(&root, 4105, now), None);
    assert_eq!(
        state::registered_session(&root, 4101, now + Duration::from_secs(60)),
        None,
        "a record older than the process belongs to an earlier process"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_new_claude_tab_is_launched_with_its_own_session_id() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("new");
    let cwd = t.folder();
    t.fresh(0, &slashed(&cwd));
    assert!(t.launches(1), "{}", t.args());
    let id = t.app.panes[0].tabs[0].launch_id.clone().expect("an id");
    assert!(
        t.args().contains(&format!("--session-id {id}")),
        "{}",
        t.args()
    );
    assert!(!t.args().contains("--resume"));
    assert_eq!(t.app.panes[0].tabs[0].cwd, cwd, "the folder is normalised");
    assert_eq!(t.sid(0), None, "no conversation yet");

    transcript(&t.dir(), &cwd, OTHER, None);
    t.app.detect_sessions();
    assert_eq!(t.sid(0), None, "another transcript is not this tab's");

    transcript(&t.dir(), Path::new("/somewhere/else"), &id, None);
    t.app.detect_sessions();
    assert_eq!(t.sid(0).as_deref(), Some(id.as_str()));
    let log = t.log();
    let uid = t.app.panes[0].tabs[0].uid;
    assert!(
        log.contains(&format!(
            "session: tab {uid} changes from none to {id} (launched with this id)"
        )),
        "{log}"
    );
}

#[test]
fn a_resume_keeps_its_id_and_a_fork_gets_a_new_one() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("resume");
    let cwd = t.folder();
    transcript(&t.dir(), &cwd, LONG, None);
    t.resume(0, &cwd, LONG);
    assert!(t.launches(1), "{}", t.args());
    assert!(
        t.args().contains(&format!("--resume {LONG}")),
        "{}",
        t.args()
    );
    assert!(!t.args().contains("--session-id"), "{}", t.args());
    assert_eq!(t.app.panes[0].tabs[0].launch_id, None);
    assert_eq!(t.sid(0).as_deref(), Some(LONG));

    t.app.panes[0].tabs[0].next_launch = Some(crate::session_ids::NextLaunch::Fork);
    t.app.launch_tab(0, 0, LaunchKind::Resume(LONG.into()));
    assert!(t.launches(2), "{}", t.args());
    let id = t.app.panes[0].tabs[0].launch_id.clone().expect("a fork id");
    let line = t.args().lines().last().unwrap().to_string();
    assert!(
        line.contains(&format!("--resume {LONG} --fork-session --session-id {id}")),
        "{line}"
    );
    assert_eq!(t.sid(0).as_deref(), Some(LONG), "until the fork is written");
    transcript(&t.dir(), &cwd, &id, Some(LONG));
    t.app.detect_sessions();
    assert_eq!(t.sid(0).as_deref(), Some(id.as_str()));
}

#[test]
fn account_arguments_that_pick_a_session_get_no_second_id() {
    let pick = |a: &[&str]| {
        crate::harness::picks_session(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    };
    assert!(pick(&["--continue"]));
    assert!(pick(&["--model", "x", "-c"]));
    assert!(pick(&["--session-id=abc"]));
    assert!(pick(&["--resume", "abc"]));
    assert!(!pick(&["--model", "opus", "--verbose"]));
}

#[test]
fn three_tabs_in_one_folder_keep_their_own_sessions() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("incident");
    let cwd = t.folder();
    let dir = t.dir();
    transcript(&dir, &cwd, LONG, None);
    transcript(&dir, &cwd, OTHER, None);
    t.resume(0, &cwd, LONG);
    t.resume(1, &cwd, OTHER);
    t.fresh(2, &slashed(&cwd));
    assert!(t.launches(3), "{}", t.args());
    let id = t.app.panes[0].tabs[2].launch_id.clone().expect("an id");
    t.app.panes[0].tabs[2].cwd = slashed(&cwd);

    t.app.detect_sessions();
    assert_eq!(
        (t.sid(0), t.sid(1), t.sid(2)),
        (Some(LONG.into()), Some(OTHER.into()), None)
    );

    std::thread::sleep(Duration::from_millis(30));
    transcript(&dir, &cwd, &id, None);
    for _ in 0..3 {
        t.app.detect_sessions();
        assert_eq!(
            t.sid(0).as_deref(),
            Some(LONG),
            "the long conversation stays"
        );
        assert_eq!(t.sid(1).as_deref(), Some(OTHER), "the neighbour stays");
        assert_eq!(
            t.sid(2).as_deref(),
            Some(id.as_str()),
            "the new tab owns it"
        );
    }

    let st = t.app.snapshot_state();
    let saved: Vec<Option<String>> = st.slots[0]
        .tabs
        .iter()
        .map(|x| x.session_id.clone())
        .collect();
    assert_eq!(
        saved,
        vec![Some(LONG.to_string()), Some(OTHER.to_string()), Some(id)],
        "a restart resumes each tab in its own session"
    );
    let log = t.log();
    let moves = log
        .lines()
        .filter(|l| l.contains("session: tab ") && !l.contains("launched"))
        .count();
    assert_eq!(moves, 0, "{log}");
}

#[test]
fn a_tab_without_an_explicit_id_claims_only_a_new_unheld_transcript() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("legacy");
    let cwd = t.folder();
    let dir = t.dir();
    transcript(&dir, &cwd, LONG, None);
    transcript(&dir, &cwd, OTHER, None);
    t.resume(0, &cwd, LONG);
    t.resume(1, &cwd, OTHER);
    t.fresh(2, &slashed(&cwd));
    assert!(t.launches(3), "{}", t.args());
    t.app.panes[0].tabs[2].launch_id = None;
    t.app.panes[0].tabs[2].cwd = slashed(&cwd);

    t.app.detect_sessions();
    assert_eq!(t.sid(2), None, "held transcripts are not up for grabs");

    std::thread::sleep(Duration::from_millis(30));
    transcript(&dir, &cwd, FRESH, None);
    t.app.detect_sessions();
    assert_eq!(
        (t.sid(0), t.sid(1), t.sid(2)),
        (Some(LONG.into()), Some(OTHER.into()), Some(FRESH.into())),
        "the trailing slash does not hide the new transcript from its tab"
    );

    std::thread::sleep(Duration::from_millis(30));
    transcript(&dir, &cwd, NEXT, None);
    t.app.detect_sessions();
    assert_eq!(
        (t.sid(0), t.sid(1), t.sid(2)),
        (Some(LONG.into()), Some(OTHER.into()), Some(FRESH.into())),
        "a newer file moves nobody who already has a session"
    );
}

#[test]
fn a_clear_is_followed_through_the_process_registry() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("clear");
    let cwd = t.folder();
    let dir = t.dir();
    transcript(&dir, &cwd, LONG, None);
    transcript(&dir, &cwd, OTHER, None);
    t.resume(0, &cwd, LONG);
    t.resume(1, &cwd, OTHER);
    assert!(t.launches(2), "{}", t.args());

    t.register(0, LONG);
    t.app.detect_sessions();
    assert_eq!(t.sid(0).as_deref(), Some(LONG));

    t.register(0, FRESH);
    t.app.detect_sessions();
    assert_eq!(t.sid(0).as_deref(), Some(LONG), "nothing written yet");

    transcript(&dir, &cwd, FRESH, None);
    t.app.detect_sessions();
    assert_eq!(t.sid(0).as_deref(), Some(FRESH));
    assert_eq!(t.sid(1).as_deref(), Some(OTHER));
    let uid = t.app.panes[0].tabs[0].uid;
    let log = t.log();
    assert!(
        log.contains(&format!(
            "session: tab {uid} changes from {LONG} to {FRESH} (the harness reports it for this process)"
        )),
        "{log}"
    );

    t.register(0, OTHER);
    t.app.detect_sessions();
    assert_eq!(
        t.sid(0).as_deref(),
        Some(FRESH),
        "a session another tab holds is never taken"
    );
    assert_eq!(t.sid(1).as_deref(), Some(OTHER));

    transcript(&dir, &cwd, NEXT, Some(FRESH));
    t.register(0, FRESH);
    t.app.detect_sessions();
    assert_eq!(
        t.sid(0).as_deref(),
        Some(FRESH),
        "the registry confirms the id, so a linked file does not move the tab"
    );
}

#[test]
fn without_a_registry_only_a_linked_never_held_transcript_moves_a_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("linked");
    let cwd = t.folder();
    let dir = t.dir();
    transcript(&dir, &cwd, LONG, None);
    t.fresh(1, &cwd);
    t.app.panes[0].tabs[1].launch_id = None;
    t.resume(0, &cwd, LONG);
    assert!(t.launches(2), "{}", t.args());

    std::thread::sleep(Duration::from_millis(30));
    transcript(&dir, &cwd, FRESH, Some(LONG));
    let other = t.app.panes[0].tabs[1].uid;
    t.app.session_held.insert(FRESH.into(), other + 1000);
    t.app.detect_sessions();
    assert_eq!(
        t.sid(0).as_deref(),
        Some(LONG),
        "a transcript another tab once held is not taken"
    );
    assert_eq!(t.sid(1), None, "nor by a tab that has no session yet");

    t.app.session_held.remove(FRESH);
    t.app.panes[0].tabs[1].launch_id = Some(NEXT.into());
    t.app.detect_sessions();
    assert_eq!(t.sid(0).as_deref(), Some(FRESH), "the link is followed");
    assert_eq!(t.sid(1), None);

    t.app.panes[0].tabs[1].launch_id = None;
    t.app.detect_sessions();
    assert_eq!(
        t.sid(1),
        None,
        "the session the first tab left was held before and stays unclaimed"
    );
}

fn durable_file(folder: &Path, ids: &[&str]) {
    let d = folder.join(".claude");
    std::fs::create_dir_all(&d).unwrap();
    let tasks: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| json!({"id": id, "cron": "0 9 * * *", "prompt": "check the build", "recurring": true}))
        .collect();
    std::fs::write(
        d.join("scheduled_tasks.json"),
        json!({"tasks": tasks}).to_string(),
    )
    .unwrap();
}

fn tab(uid: u64, cwd: &Path) -> DurableTab {
    DurableTab {
        uid,
        account: Some(0),
        tab_name: format!("tab {uid}"),
        cwd: cwd.to_path_buf(),
    }
}

#[test]
fn a_durable_job_belongs_to_one_tab_of_a_shared_folder() {
    let folder = temp("durable");
    durable_file(&folder, &["job00001", "job00002"]);
    let tabs = [tab(7, &folder), tab(5, &slashed(&folder)), tab(9, &folder)];
    let rows = durable_rows(vec![], &tabs);
    let got: Vec<(u64, &str)> = rows.iter().map(|r| (r.uid, r.lp.id.as_str())).collect();
    assert_eq!(got, vec![(5, "job00001"), (5, "job00002")]);

    let mut made = crate::loops::durable(&folder).remove(1);
    made.fires = 3;
    let from_transcript = |uid: u64, lp: &crate::loops::Loop| LoopRow {
        uid,
        account: Some(0),
        tab_name: format!("tab {uid}"),
        lp: lp.clone(),
    };
    let rows = durable_rows(vec![from_transcript(9, &made)], &tabs);
    let got: Vec<(u64, &str)> = rows.iter().map(|r| (r.uid, r.lp.id.as_str())).collect();
    assert_eq!(
        got,
        vec![(9, "job00002"), (5, "job00001")],
        "the tab whose transcript created the job owns it"
    );

    let rows = durable_rows(
        vec![from_transcript(9, &made), from_transcript(7, &made)],
        &tabs,
    );
    let owners: Vec<u64> = rows
        .iter()
        .filter(|r| r.lp.id == "job00002")
        .map(|r| r.uid)
        .collect();
    assert_eq!(
        owners,
        vec![7],
        "two transcripts naming it still give one row"
    );
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn stopping_a_shared_durable_job_asks_one_tab_and_counts_it_once() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = setup("stop");
    let cwd = t.folder();
    durable_file(&cwd, &["job00001"]);
    for i in 0..3 {
        t.fresh(i, &cwd);
    }
    assert!(t.launches(3), "{}", t.args());
    let tabs: Vec<DurableTab> = t.app.panes[0]
        .tabs
        .iter()
        .map(|p| tab(p.uid, &p.cwd))
        .collect();
    let first = tabs.iter().map(|x| x.uid).min().unwrap();
    t.app.on_loops(durable_rows(vec![], &tabs));
    assert_eq!(t.app.loops.len(), 1);
    t.app.stop_all_loops();
    assert_eq!(t.app.stop_reqs.len(), 1);
    assert_eq!(t.app.stop_reqs[0].uid, first);
    let said = t
        .app
        .flash
        .as_ref()
        .map(|f| f.0.clone())
        .unwrap_or_default();
    assert!(said.starts_with("Stopping 1 loop:"), "{said}");
}
