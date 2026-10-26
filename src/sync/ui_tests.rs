//! The sync tools the web's Settings > Sync and the assistant share
//! (app_sync.rs): the dry run with its checks, enable (init) and join
//! (clone) behind one question each, pause and resume, and the richer
//! status. Fixture homes under target/tmp and local bare repos stand in
//! for GitHub: nothing here reaches GitHub, gh's login state or a real
//! home.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::tests::{fixture_a, root, w};
use super::*;
use crate::app::App;
use crate::config::testing::LOCK as ENV_LOCK;

/// An app whose GodTerm home is `home/.godterm`, with `[sync]` pointing
/// at `remotes` (a folder of bare repos) and sync off.
fn app_at(home: &Path, remotes: &Path) -> App {
    let app_home = home.join(".godterm");
    std::fs::create_dir_all(&app_home).unwrap();
    crate::config::testing::set_home(&app_home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let cfg = crate::config::Config {
        claude_bin: Some(crate::test_stub::true_bin()),
        sync: SyncCfg {
            owner: "tester".into(),
            account: "tester".into(),
            remote: format!("file://{}", remotes.display()),
            ..Default::default()
        },
        ..Default::default()
    };
    cfg.save().unwrap();
    let mut app = App::new(cfg, tx);
    app.sync_ui.test_home = Some((home.to_path_buf(), app_home));
    app
}

/// The user's next message (a new assistant turn).
fn user_says(app: &mut App, said: &str) {
    app.assistant_turn += 1;
    app.assistant.last_user = said.into();
    app.assistant.turn_reads = 0;
}

/// A tool whose reply is finished off the UI thread (sync_plan).
fn call_wait(app: &mut App, tool: &str, args: Value) -> Value {
    let v = app.control_call(tool, &args);
    if v.get("_wait").is_none() {
        return v;
    }
    let rx = app.pending_job.take().expect("a job");
    let r = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the job answers");
    json!({"ok": true, "result": r})
}

/// Wait for the background init or clone to finish.
fn op_done(app: &App) -> crate::app_sync::SyncOp {
    let t = Instant::now();
    loop {
        if let Some(o) = app.sync_op().filter(|o| o.ok.is_some()) {
            return o;
        }
        assert!(
            t.elapsed() < Duration::from_secs(90),
            "the setup never finished"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn cfg_text(home: &Path) -> String {
    std::fs::read_to_string(home.join(".godterm/config.toml")).unwrap()
}

/// `[sync]` as config.toml has it now.
fn sync_cfg(home: &Path) -> SyncCfg {
    crate::config::Config::parse(&cfg_text(home)).unwrap().sync
}

#[test]
fn enable_sync_runs_the_plan_then_asks_once_then_inits_in_the_background() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = root("ui-init");
    let home = r.join("m1");
    let remotes = r.join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    w(&home.join("code/site/README.md"), "site\n");
    let mut app = app_at(&home, &remotes);

    // "enable sync": init on its own is refused until a plan was made.
    user_says(&mut app, "enable sync");
    let v = app.control_call("sync_init", &json!({"_client": "brain"}));
    assert_eq!(v["ok"], false, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("run sync_plan first"),
        "{v}"
    );
    assert!(!remotes.join("tester").exists(), "nothing created");

    // The dry run: repo, account, warnings, and what to do next.
    let p = call_wait(&mut app, "sync_plan", json!({"_client": "brain"}));
    let pr = &p["result"];
    assert_eq!(pr["preflight"]["state_repo"], "tester/godterm-state", "{p}");
    assert_eq!(pr["preflight"]["account"], "tester");
    assert_eq!(pr["preflight"]["can_init"], true, "{p}");
    assert!(
        pr["say"]
            .as_str()
            .unwrap()
            .starts_with("Dry run, nothing written"),
        "{p}"
    );
    assert!(
        pr["next"].as_str().unwrap().contains("call sync_init"),
        "{p}"
    );
    assert!(pr["text"].as_str().unwrap().contains("Sync plan (dry run"));
    assert!(!remotes.join("tester").exists(), "a plan writes nothing");

    // A plan for another owner does not cover this one.
    let v = app.control_call(
        "sync_init",
        &json!({"_client": "brain", "owner": "someone"}),
    );
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("run sync_plan with owner someone"),
        "{v}"
    );

    // init asks once, naming the private repo; nothing before the yes.
    let v = app.control_call("sync_init", &json!({"_client": "brain"}));
    assert_eq!(v["needs_confirmation"], true, "{v}");
    let q = v["question"].as_str().unwrap();
    assert!(q.contains("private state repo tester/godterm-state"), "{q}");
    assert!(q.contains("Project repos are never touched"), "{q}");
    let tok = v["token"].as_str().unwrap().to_string();
    // The same turn's token is refused: the user has not heard it.
    let same = app.control_call(
        "sync_init",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(same["ok"], false, "{same}");
    // An unclear answer runs nothing.
    user_says(&mut app, "what would that cost?");
    let un = app.control_call(
        "sync_init",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(un["needs_confirmation"], true, "{un}");
    assert!(app.sync_op().is_none());
    assert!(!remotes.join("tester/godterm-state.git").exists());

    // A clear yes in a later turn: started in the background.
    user_says(&mut app, "yes");
    let ok = app.control_call(
        "sync_init",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["result"]["started"], true);
    let op = op_done(&app);
    assert_eq!(op.ok, Some(true), "{op:?}");
    assert_eq!(op.kind, "init");
    assert!(remotes.join("tester/godterm-state.git/HEAD").is_file());
    assert!(sync_cfg(&home).enabled);
    // Status while it is on: the op, the repo, the next sync.
    app.reload_config_now();
    assert!(app.cfg.sync.enabled);
    let st = app.control_call("sync_status", &json!({}));
    let s = &st["result"];
    assert_eq!(s["enabled"], true, "{st}");
    assert_eq!(s["op"]["kind"], "init");
    assert_eq!(s["repos"][0]["name"], "tester/godterm-state", "{st}");
    assert_eq!(s["repos"][0]["role"], "state");
    assert!(s["next"]["words"].is_string(), "{st}");
    assert!(s["never"].as_str().unwrap().contains("never"));
    // A second init is refused now.
    let again = app.control_call("sync_init", &json!({"_client": "brain"}));
    assert!(
        again["error"].as_str().unwrap().contains("already on"),
        "{again}"
    );
    // The visibility marker shows in the status (private here).
    w(
        &remotes.join("tester/godterm-state.git/visibility"),
        "PRIVATE\n",
    );
    let st = app.control_call("sync_status", &json!({}));
    assert_eq!(st["result"]["repos"][0]["visibility"], "PRIVATE", "{st}");
    assert!(
        st["result"]["warnings"].as_array().unwrap().is_empty()
            || !st["result"]["warnings"].to_string().contains("not private")
    );
    w(
        &remotes.join("tester/godterm-state.git/visibility"),
        "PUBLIC\n",
    );
    let st = app.control_call("sync_status", &json!({}));
    assert!(
        st["result"]["warnings"].to_string().contains("not private"),
        "{st}"
    );

    // Pause and resume: [sync] paused in config.toml, status says so.
    let v = app.control_call("sync_pause", &json!({"pause": true}));
    assert_eq!(v["result"]["paused"], true, "{v}");
    assert!(sync_cfg(&home).paused);
    assert!(app.cfg.sync.paused);
    let st = app.control_call("sync_status", &json!({}));
    assert_eq!(st["result"]["paused"], true);
    assert_eq!(st["result"]["next"]["paused"], true);
    // sync_now still runs once while paused.
    let now = app.control_call("sync_now", &json!({}));
    assert!(
        now["result"]["say"]
            .as_str()
            .unwrap()
            .contains("stays paused"),
        "{now}"
    );
    let v = app.control_call("sync_pause", &json!({"pause": false}));
    assert_eq!(v["result"]["paused"], false);
    assert!(!sync_cfg(&home).paused && sync_cfg(&home).enabled);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn join_checks_the_repo_asks_once_then_clones() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = root("ui-clone");
    let remotes = r.join("remotes");
    // The first machine (the sync's own fixture) made the state repo.
    let (a, _) = fixture_a(&r);
    restore::init(&a, a.cfg.sync.clone(), &|_| true).unwrap();
    let home = r.join("m2");
    let mut app = app_at(&home, &remotes);

    // A repo that is not there: the check says so, clone is refused.
    let p = call_wait(
        &mut app,
        "sync_plan",
        json!({"repo": "tester/nothing-here"}),
    );
    assert_eq!(p["result"]["preflight"]["exists"], false, "{p}");
    assert!(p["result"]["plan"].is_null(), "a join check makes no plan");
    let v = app.control_call("sync_clone", &json!({"repo": "tester/nothing-here"}));
    assert!(v["error"].as_str().unwrap().contains("cannot join"), "{v}");
    // Bad names are refused at once.
    let v = app.control_call("sync_clone", &json!({"repo": "no-slash"}));
    assert_eq!(v["ok"], false, "{v}");

    // The real one: checked, then asked once, run on the yes.
    let p = call_wait(
        &mut app,
        "sync_plan",
        json!({"repo": "tester/godterm-state"}),
    );
    assert_eq!(p["result"]["preflight"]["can_clone"], true, "{p}");
    user_says(&mut app, "join my other machine's sync");
    let v = app.control_call(
        "sync_clone",
        &json!({"_client": "brain", "repo": "tester/godterm-state", "label": "Laptop"}),
    );
    assert_eq!(v["needs_confirmation"], true, "{v}");
    let q = v["question"].as_str().unwrap();
    assert!(
        q.contains("Join tester/godterm-state") && q.contains("\"Laptop\""),
        "{q}"
    );
    assert!(q.contains("never committed to or pushed"), "{q}");
    assert!(!sync_cfg(&home).enabled);
    let tok = v["token"].as_str().unwrap().to_string();
    // A no drops it.
    user_says(&mut app, "no");
    let no = app.control_call(
        "sync_clone",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(no["result"]["cancelled"], true, "{no}");
    assert!(app.sync_op().is_none());
    // Asked again, and a yes.
    let v = app.control_call(
        "sync_clone",
        &json!({"_client": "brain", "repo": "tester/godterm-state", "label": "Laptop"}),
    );
    let tok = v["token"].as_str().unwrap().to_string();
    user_says(&mut app, "yes, do it");
    let ok = app.control_call(
        "sync_clone",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert_eq!(ok["result"]["started"], true, "{ok}");
    let op = op_done(&app);
    assert_eq!(op.ok, Some(true), "{op:?}");
    assert_eq!(op.kind, "clone");
    assert!(op.summary["placed"].as_u64().unwrap() >= 1, "{op:?}");
    let sc = sync_cfg(&home);
    assert!(sc.enabled && sc.state_repo == "godterm-state", "{sc:?}");
    let m = Machine::load_or_new(&home.join(".godterm"));
    assert_eq!(m.label, "Laptop");
    assert_ne!(m.id, a.machine.id, "a machine id is never copied");
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn the_web_confirm_dialog_is_the_users_yes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = root("ui-web");
    let remotes = r.join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    let home = r.join("m3");
    let mut app = app_at(&home, &remotes);
    // Preview, then Enable: the reply is the dialog's question...
    let p = call_wait(&mut app, "sync_plan", json!({"_client": "web"}));
    assert_eq!(p["result"]["preflight"]["can_init"], true, "{p}");
    let v = app.control_call("sync_init", &json!({"_client": "web"}));
    assert_eq!(v["needs_confirmation"], true, "{v}");
    let tok = v["token"].as_str().unwrap().to_string();
    // ...and its Yes comes as the user's own turn (ws.rs user_says).
    app.handle(crate::app::AppEvent::UserTurn("yes".into()));
    let ok = app.control_call(
        "sync_init",
        &json!({"_client": "web", "confirm_token": tok}),
    );
    assert_eq!(ok["result"]["started"], true, "{ok}");
    assert_eq!(op_done(&app).ok, Some(true));
    // Pausing needs sync on; sync_now with sync off says how to turn it on.
    let mut off = app_at(&r.join("m4"), &remotes);
    let v = off.control_call("sync_pause", &json!({}));
    assert!(v["error"].as_str().unwrap().contains("off"), "{v}");
    let v = off.control_call("sync_now", &json!({}));
    assert!(v["error"].as_str().unwrap().contains("sync_plan"), "{v}");
    let st = off.control_call("sync_status", &json!({}));
    assert_eq!(st["result"]["enabled"], false);
    assert!(
        st["result"]["lines"]
            .to_string()
            .contains("Settings > Sync"),
        "{st}"
    );
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn tools_refuse_without_a_fixture_home_in_tests() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = root("ui-guard");
    let mut app = app_at(&r.join("m"), &r.join("remotes"));
    app.sync_ui.test_home = None;
    let v = app.control_call("sync_plan", &json!({}));
    assert!(v["error"].as_str().unwrap().contains("fixture home"), "{v}");
    let _: PathBuf = r;
}
