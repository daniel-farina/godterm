use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::app_catalog::*;
use crate::config::testing::LOCK as ENV_LOCK;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("godterm-appcat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sess(id: &str, cwd: &str, mins_ago: u64) -> Sess {
    Sess {
        id: id.into(),
        title: format!("talk {id}"),
        cwd: cwd.into(),
        modified: Some(SystemTime::now() - Duration::from_secs(mins_ago * 60)),
        harness: "claude".into(),
        source: "Account 1".into(),
        gone: false,
    }
}

fn app(name: &str, path: &str, project: &str) -> CatalogApp {
    CatalogApp {
        name: name.into(),
        kind: "app".into(),
        path: path.into(),
        project: project.into(),
        ..Default::default()
    }
}

const FULL: &str = r#"{"version":1,"generated_at":"2026-01-02T03:04:05Z","apps":[
 {"name":"Tide Clock","kind":"Utility","description":"Shows the next high tide.","path":"/work/harbor/tide-clock/","project":"/work/harbor","tech":"Swift, SwiftUI","run":"swift run","tags":["tides"," sea ",7,""],"stars":5},
 {"kind":"spaceship","path":"/work/kiln"},
 {"name":"Loose"},
 {"name":"Inner","path":"tools/inner","project":"/work/harbor"},
 "nonsense", 12, {}
],"extra":true}"#;

#[test]
fn a_full_catalog_is_read_and_unknown_fields_are_ignored() {
    let c = parse(FULL);
    assert_eq!(c.generated_at, "2026-01-02T03:04:05Z");
    assert_eq!(c.apps.len(), 4);
    let a = &c.apps[0];
    assert_eq!(a.name, "Tide Clock");
    assert_eq!(a.kind, "utility");
    assert_eq!(a.description, "Shows the next high tide.");
    assert_eq!(a.path, "/work/harbor/tide-clock");
    assert_eq!(a.project, "/work/harbor");
    assert_eq!(a.tech, "Swift, SwiftUI");
    assert_eq!(a.run, "swift run");
    assert_eq!(a.tags, vec!["tides", "sea"]);
}

#[test]
fn missing_fields_are_empty_and_get_sensible_fallbacks() {
    let c = parse(FULL);
    let kiln = &c.apps[1];
    assert_eq!(kiln.name, "kiln", "named after its folder");
    assert_eq!(kiln.kind, "other", "an unknown kind is other");
    assert_eq!(kiln.project, "/work/kiln", "the project is the path");
    assert!(kiln.description.is_empty() && kiln.tech.is_empty() && kiln.tags.is_empty());
    let loose = &c.apps[2];
    assert_eq!(loose.name, "Loose");
    assert!(loose.path.is_empty() && loose.project.is_empty());
    assert_eq!(c.apps[3].path, "/work/harbor/tools/inner");
}

#[test]
fn a_broken_or_missing_file_is_an_empty_catalog() {
    for bad in [
        "",
        "{",
        "[]",
        "null",
        "42",
        r#"{"apps":"none"}"#,
        r#"{"apps":{"a":1}}"#,
        r#"{"version":"x","apps":null}"#,
    ] {
        assert!(parse(bad).apps.is_empty(), "{bad}");
    }
    let d = scratch("broken");
    assert_eq!(load(&d.join("apps.json")), Catalog::default());
    std::fs::write(d.join("apps.json"), b"\xff\xfe not json").unwrap();
    assert_eq!(load(&d.join("apps.json")), Catalog::default());
    std::fs::write(d.join("apps.json"), FULL).unwrap();
    assert_eq!(load(&d.join("apps.json")).apps.len(), 4);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_catalog_file_lives_in_the_config_folder_of_the_test() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = scratch("home");
    crate::config::testing::set_home(&home);
    assert_eq!(catalog_path(), home.join("apps.json"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn sessions_in_the_app_or_its_project_are_attached_newest_first() {
    let cat = Catalog {
        generated_at: String::new(),
        apps: vec![
            app("Tide Clock", "/work/harbor/tide-clock", "/work/harbor"),
            app("Buoy Map", "/work/harbor/buoy-map", "/work/harbor"),
            app("Kiln Timer", "/work/kiln", "/work/kiln"),
        ],
    };
    let ss = [
        sess("old", "/work/harbor", 600),
        sess("new", "/work/harbor/tide-clock/src", 5),
        sess("mid", "/work/harbor/buoy-map", 60),
        sess("near", "/work/harbor-two", 1),
        sess("kiln", "/work/kiln", 30),
    ];
    let out = merge(&cat, &ss, &|_| true);
    let ids = |i: usize| -> Vec<&str> { out[i].sessions.iter().map(|s| s.id.as_str()).collect() };
    assert_eq!(ids(0), vec!["new", "mid", "old"]);
    assert_eq!(ids(1), vec!["new", "mid", "old"], "apps share a project");
    assert_eq!(ids(2), vec!["kiln"]);
    assert!(out[..3].iter().all(|e| e.described && !e.missing));
    assert_eq!(out[0].last(), ss[1].modified);
    assert_eq!(out.len(), 4);
    assert_eq!(
        out[3].app.path, "/work/harbor-two",
        "a sibling is not inside"
    );
}

#[test]
fn folders_with_sessions_and_no_entry_are_listed_undescribed() {
    let cat = Catalog {
        generated_at: String::new(),
        apps: vec![app("Kiln Timer", "/work/kiln", "/work/kiln")],
    };
    let ss = [
        sess("a", "/work/orchard", 90),
        sess("b", "/work/orchard", 10),
        sess("c", "/work/lantern/", 50),
        sess("d", "/work/kiln/ui", 1),
        sess("e", "", 1),
    ];
    let out = merge(&cat, &ss, &|_| true);
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].app.name, "Kiln Timer");
    let o = &out[1];
    assert_eq!(o.app.name, "orchard");
    assert_eq!(o.app.path, "/work/orchard");
    assert_eq!(o.app.project, "/work/orchard");
    assert!(!o.described && o.app.description.is_empty());
    assert_eq!(
        o.sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["b", "a"]
    );
    assert_eq!(out[2].app.name, "lantern", "ordered by last worked on");
    assert_eq!(out[2].app.path, "/work/lantern");
    let j = entry_json(o);
    assert_eq!(j["kind"], "undescribed");
    assert_eq!(j["described"], false);
    assert_eq!(j["conversations"], 2);
    assert_eq!(j["sessions"][0]["id"], "b");
    assert_eq!(j["sessions"][0]["title"], "talk b");
    assert_eq!(j["project_name"], "orchard");
}

#[test]
fn worktree_sessions_count_for_the_project_they_branch_from() {
    assert_eq!(
        project_root("/work/orchard/.claude/worktrees/agent-12"),
        "/work/orchard"
    );
    assert_eq!(project_root("/work/orchard/"), "/work/orchard");
    let cat = Catalog {
        generated_at: String::new(),
        apps: vec![app("Tide Clock", "/work/harbor/tide-clock", "/work/harbor")],
    };
    let ss = [
        sess("w1", "/work/orchard/.claude/worktrees/agent-12", 3),
        sess("w2", "/work/orchard/.claude/worktrees/agent-13/web", 2),
        sess("w3", "/work/harbor/.claude/worktrees/agent-1", 1),
    ];
    let out = merge(&cat, &ss, &|_| true);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].sessions.len(), 1);
    assert_eq!(out[1].app.path, "/work/orchard");
    assert_eq!(out[1].sessions.len(), 2);
}

#[test]
fn entries_whose_folder_is_gone_are_flagged_not_hidden() {
    let d = scratch("missing");
    let here = d.join("here");
    std::fs::create_dir_all(&here).unwrap();
    let gone = d.join("gone");
    let lost = d.join("lost");
    let cat = Catalog {
        generated_at: String::new(),
        apps: vec![
            app("Here", &here.to_string_lossy(), &here.to_string_lossy()),
            app("Gone", &gone.to_string_lossy(), &gone.to_string_lossy()),
            app("Nowhere", "", ""),
        ],
    };
    let ss = [sess("x", &lost.to_string_lossy(), 1)];
    let out = merge(&cat, &ss, &|p: &Path| p.exists());
    assert_eq!(out.len(), 4);
    assert!(!out[0].missing);
    assert!(out[1].missing);
    assert!(!out[2].missing, "no path to miss");
    assert!(out[3].missing && !out[3].described);
    assert!(out[3].project_missing);
    assert_eq!(out[3].sessions.len(), 1, "its conversations stay listed");
    assert!(out[3].sessions[0].gone);
    let j = entry_json(&out[3]);
    assert_eq!(j["missing"], true);
    assert_eq!(j["conversations"], 1);
    assert_eq!(j["sessions"][0]["gone"], true);
    assert_eq!(j["sessions"][0]["title"], "talk x");
    assert_eq!(j["sessions"][0]["cwd"], lost.to_string_lossy().as_ref());
    assert!(
        to_survey(&out).is_empty(),
        "nothing to read in a missing folder"
    );
    let live = merge(&cat, &ss, &|_| true);
    assert_eq!(to_survey(&live), vec![lost.to_string_lossy().into_owned()]);
    assert_eq!(entry_json(&out[1])["missing"], true);
    assert_eq!(entry_json(&out[1])["last"], serde_json::Value::Null);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_prompts_carry_the_rules_the_schema_and_the_folders() {
    let cat = Path::new("/cfg/apps.json");
    let d = describe_prompt(cat, "/work/orchard");
    let r = refresh_prompt(cat, &["/work/orchard".into(), "/work/lantern".into()]);
    for p in [&d, &r] {
        assert!(p.contains("/cfg/apps.json"));
        assert!(p.contains(SCHEMA));
        assert!(p.contains("README"));
        assert!(p.contains("/Volumes/Private"));
        assert!(p.contains("secrets"));
        assert!(p.contains("keep every existing entry"));
        assert!(!p.contains('\u{2014}'));
    }
    assert!(d.contains("/work/orchard"));
    assert!(r.contains("/work/orchard, /work/lantern"));
    let many: Vec<String> = (0..55).map(|i| format!("/work/p{i}")).collect();
    let big = refresh_prompt(cat, &many);
    assert!(big.contains("/work/p39") && !big.contains("/work/p40"));
    assert!(big.contains("15 more"));
    assert!(!refresh_prompt(cat, &[]).contains("not described yet"));
    assert!(serde_json::from_str::<serde_json::Value>(SCHEMA).is_ok());
}

fn row(id: &str, cwd: &Path, mins_ago: u64, prompt: &str) -> crate::session_index::Row {
    crate::session_index::Row {
        src: crate::app_sessions::Src::Main,
        harness: crate::harness::Harness::Claude,
        source: "Account 1".into(),
        info: crate::sessions::SessionInfo {
            id: id.into(),
            cwd: cwd.to_string_lossy().into_owned(),
            modified: Some(SystemTime::now() - Duration::from_secs(mins_ago * 60)),
            first_prompt: Some(prompt.into()),
            ..Default::default()
        },
    }
}

fn app_with_rows(tag: &str) -> (crate::app::App, PathBuf, PathBuf) {
    let home = scratch(tag);
    crate::config::testing::set_home(&home);
    let work = PathBuf::from(format!("/nowhere-{tag}-{}", std::process::id()));
    let (tx, _rx) = std::sync::mpsc::channel();
    let app = crate::app::App::new(crate::config::Config::default(), tx);
    {
        let mut ix = app.session_index.lock().unwrap();
        ix.built = Some(Instant::now());
        ix.rows = vec![
            row("s1", &work.join("harbor"), 40, "add a tide table"),
            row("s2", &work.join("orchard"), 5, "plan the orchard"),
            row("s3", &work.join("vanished"), 9, "old idea"),
        ];
        let mut sub = row("s4", &work.join("orchard"), 1, "helper");
        sub.info.parent = Some("s2".into());
        ix.rows.push(sub);
        let mut scripted = row("s5", &work.join("orchard"), 1, "scripted");
        scripted.info.headless = true;
        ix.rows.push(scripted);
    }
    (app, home, work)
}

#[test]
fn the_listing_merges_the_catalog_file_with_the_session_index() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, work) = app_with_rows("listing");
    let cat = serde_json::json!({"version": 1, "generated_at": "2026-01-02T03:04:05Z", "apps": [
        {"name": "Tide Clock", "kind": "utility", "description": "Shows the next high tide.",
         "path": work.join("harbor/tide-clock"), "project": work.join("harbor"), "tech": "Swift"}
    ]});
    std::fs::write(home.join("apps.json"), cat.to_string()).unwrap();
    let v = app.app_listing();
    assert_eq!(v["generated_at"], "2026-01-02T03:04:05Z");
    assert_eq!(v["indexing"], false);
    let apps = v["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 3, "{v}");
    assert_eq!(apps[0]["name"], "Tide Clock");
    assert_eq!(apps[0]["kind"], "utility");
    assert_eq!(apps[0]["conversations"], 1);
    assert_eq!(apps[0]["sessions"][0]["id"], "s1");
    assert_eq!(apps[0]["sessions"][0]["title"], "add a tide table");
    assert_eq!(apps[0]["sessions"][0]["harness"], "claude");
    assert_eq!(apps[0]["sessions"][0]["source"], "Account 1");
    assert_eq!(apps[0]["missing"], true, "that folder is not on disk");
    assert_eq!(apps[1]["name"], "orchard");
    assert_eq!(apps[1]["described"], false);
    assert_eq!(apps[1]["conversations"], 1, "no subagent or scripted runs");
    assert_eq!(apps[2]["name"], "vanished");
    assert_eq!(apps[2]["missing"], true);

    std::fs::write(home.join("apps.json"), "{ broken").unwrap();
    let v = app.app_listing();
    assert_eq!(v["apps"].as_array().unwrap().len(), 3);
    assert!(v["apps"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["described"] == false));
    let cat = serde_json::json!({"apps": [{"name": "Hidden", "path": "/Volumes/Private/hidden"}]});
    std::fs::write(home.join("apps.json"), cat.to_string()).unwrap();
    app.session_index.lock().unwrap().rows.push(row(
        "s9",
        Path::new("/Volumes/Private/hidden/src"),
        1,
        "never listed",
    ));
    let v = app.app_listing();
    assert_eq!(
        v["apps"].as_array().unwrap().len(),
        3,
        "the vault stays out: {v}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_prompt_waits_for_its_tab_and_is_never_sent_to_a_tab_that_is_gone() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, work) = app_with_rows("prefill");
    let uid = app.panes[0].tabs[0].uid;
    let tab = crate::control::tab_id(uid);
    assert!(app
        .app_prefill(&serde_json::json!({"tab": tab, "what": "describe"}))
        .is_err());
    assert!(app
        .app_prefill(&serde_json::json!({"tab": tab, "what": "sing"}))
        .is_err());
    assert!(app
        .app_prefill(&serde_json::json!({"tab": "t999999", "what": "refresh"}))
        .is_err());
    assert!(app.app_prefills.is_empty());

    let r = app
        .app_prefill(
            &serde_json::json!({"tab": tab, "what": "describe", "path": work.join("orchard")}),
        )
        .unwrap();
    assert_eq!(r["tab"], tab);
    assert_eq!(app.app_prefills.len(), 1, "the tab is not ready yet");
    assert!(app.app_prefills[0]
        .text
        .contains(&*work.join("orchard").to_string_lossy()));
    assert!(app.app_prefills[0]
        .text
        .contains(&*home.join("apps.json").to_string_lossy()));

    app.app_prefill(&serde_json::json!({"tab": tab, "what": "refresh"}))
        .unwrap();
    assert_eq!(app.app_prefills.len(), 1, "one prompt per tab");
    let text = &app.app_prefills[0].text;
    assert!(text.starts_with("Refresh my personal app catalog"));
    assert!(
        !text.contains(&*work.join("vanished").to_string_lossy()),
        "missing folders are not surveyed"
    );

    app.app_prefills[0].at -= Duration::from_secs(600);
    app.app_prefill_tick();
    assert!(app.app_prefills.is_empty(), "given up after the wait");

    app.app_prefills.push(Prefill {
        uid: 987_654,
        text: "x".into(),
        at: Instant::now(),
    });
    app.app_prefill_tick();
    assert!(app.app_prefills.is_empty(), "a closed tab drops it");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_app_folder_that_is_gone_inside_a_live_project_flags_only_the_app() {
    let d = scratch("partly");
    let project = d.join("harbor");
    std::fs::create_dir_all(project.join("buoy-map")).unwrap();
    let p = |x: &Path| x.to_string_lossy().into_owned();
    let cat = Catalog {
        generated_at: String::new(),
        apps: vec![
            app("Tide Clock", &p(&project.join("tide-clock")), &p(&project)),
            app("Buoy Map", &p(&project.join("buoy-map")), &p(&project)),
        ],
    };
    let ss = [sess("x", &p(&project), 1)];
    let out = merge(&cat, &ss, &|x: &Path| x.exists());
    assert_eq!(out.len(), 2);
    assert!(out[0].missing && !out[0].project_missing);
    assert!(!out[1].missing && !out[1].project_missing);
    assert!(
        !out[0].sessions[0].gone,
        "the conversation still has its folder"
    );
    let j = entry_json(&out[0]);
    assert_eq!(j["missing"], true);
    assert_eq!(j["project_missing"], false);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_rebuild_prompt_looks_for_a_remote_first_and_owns_up_to_gaps() {
    let p = rebuild_prompt("/work/paper-kite", None);
    assert!(p.contains("/work/paper-kite"));
    assert!(p.contains("git remote"));
    assert!(p.contains("do not clone before I say yes"));
    assert!(p.contains("could not be recovered"));
    assert!(p.contains("/Volumes/Private"));
    assert!(p.contains("this conversation's history"));
    assert!(!p.contains("transcript file"));
    assert!(!p.contains('\u{2014}'));
    let q = rebuild_prompt(
        "/work/paper-kite",
        Some(Path::new("/cfg/projects/x/abc.jsonl")),
    );
    assert!(q.contains("transcript file /cfg/projects/x/abc.jsonl"));
    assert!(!q.contains("this conversation's history"));
}

const SID: &str = "7b1e4c2a-33d0-4f6e-9a51-0c8d2e6f1a90";

struct Stubbed {
    app: crate::app::App,
    home: PathBuf,
    args: PathBuf,
}

impl Drop for Stubbed {
    fn drop(&mut self) {
        for p in &mut self.app.panes {
            p.kill_all();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn stubbed(tag: &str) -> Stubbed {
    let home = scratch(tag);
    let home = crate::fspath::canonical(&home).unwrap();
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(crate::creds::CredSource::Keychain);
    }
    let args = home.join("args.txt");
    let stub = crate::test_stub::claude(
        &home.join("bin"),
        &[
            ("sleep_s", "30".to_string()),
            ("args_to", args.display().to_string()),
        ],
    );
    app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
    app.resume_check = true;
    Stubbed { app, home, args }
}

fn transcript(t: &Stubbed, cwd: &Path) -> PathBuf {
    let dir = t.app.cfg.accounts[0]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(cwd));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join(format!("{SID}.jsonl"));
    let lines = [
        serde_json::json!({"type": "user", "cwd": cwd, "sessionId": SID, "message": {"role": "user", "content": "fold the paper kite"}}),
        serde_json::json!({"type": "assistant", "sessionId": SID, "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&f, text).unwrap();
    f
}

fn opened_tab(app: &crate::app::App) -> &crate::pane::Pane {
    app.panes[app.focus].cur()
}

#[test]
fn a_conversation_of_a_vanished_folder_resumes_once_the_folder_is_confirmed_back() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = stubbed("resume");
    let work = crate::fspath::canonical(&scratch("resume-work")).unwrap();
    let gone = work.join("old").join("paper-kite");
    let file = transcript(&t, &gone);
    t.app.kick_index(true);
    assert!(t.app.app_session_row(SID).is_some());
    assert!(!gone.exists());

    let v = t
        .app
        .control_call("open_session", &serde_json::json!({ "id": SID }));
    assert_eq!(v["result"]["opened"], SID, "{v}");
    assert!(
        opened_tab(&t.app).lost.is_some(),
        "the plain resume cannot find the conversation from another folder"
    );
    assert!(std::fs::read_to_string(&t.args)
        .unwrap_or_default()
        .is_empty());
    assert!(!gone.exists(), "resuming alone never makes folders");

    assert_eq!(
        t.app.app_restore_folder(SID, false),
        Ok(Restore::Ask(gone.clone()))
    );
    assert!(!gone.exists(), "nothing is created before the yes");
    assert!(!work.join("old").exists());
    assert!(t.app.app_restore_folder("no-such-id", true).is_err());

    assert_eq!(
        t.app.app_restore_folder(SID, true),
        Ok(Restore::Created(gone.clone()))
    );
    assert!(gone.is_dir());
    assert_eq!(std::fs::read_dir(&gone).unwrap().count(), 0, "empty");
    assert_eq!(
        t.app.app_restore_folder(SID, false),
        Ok(Restore::Present(gone.clone()))
    );

    let v = t
        .app
        .control_call("open_session", &serde_json::json!({ "id": SID }));
    assert_eq!(v["result"]["opened"], SID, "{v}");
    let tab = opened_tab(&t.app);
    assert!(tab.lost.is_none(), "{:?}", tab.lost);
    assert_eq!(tab.cwd, gone);
    assert_eq!(tab.session_id.as_deref(), Some(SID));
    let args = t.args.clone();
    let t0 = Instant::now();
    while !std::fs::read_to_string(&args)
        .unwrap_or_default()
        .contains(&format!("--resume {SID}"))
    {
        assert!(t0.elapsed() < Duration::from_secs(10), "never resumed");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(file.is_file());

    let r = t
        .app
        .app_prefill(&serde_json::json!({
            "tab": crate::control::tab_id(opened_tab(&t.app).uid),
            "what": "rebuild", "path": gone, "session": SID, "resumed": false
        }))
        .unwrap();
    assert_eq!(r["tab"], crate::control::tab_id(opened_tab(&t.app).uid));
    let text = &t.app.app_prefills.last().unwrap().text;
    assert!(text.contains(&*file.to_string_lossy()), "{text}");
    assert!(text.contains(&*gone.to_string_lossy()));
    drop(t);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn folders_are_never_recreated_in_the_vault_or_outside_the_home_folder() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home, _work) = app_with_rows("refuse");
    {
        let mut ix = app.session_index.lock().unwrap();
        ix.rows = vec![
            row("v1", Path::new("/Volumes/Private/secret-project"), 1, "a"),
            row("v2", Path::new("/nowhere-at-all/kite"), 1, "b"),
            row("v3", Path::new("relative/kite"), 1, "c"),
        ];
    }
    for id in ["v1", "v2", "v3"] {
        let r = app.app_restore_folder(id, true);
        assert!(r.is_err(), "{id}: {r:?}");
    }
    let r = app.app_restore_folder("v1", false);
    assert!(
        r.is_err(),
        "the vault is refused before any question: {r:?}"
    );
    assert!(!Path::new("/nowhere-at-all").exists());
    let _ = std::fs::remove_dir_all(&home);
}
