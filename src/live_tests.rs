//! Tests from the live take-over conversation (21:10 to 21:16): finding
//! sessions by their spoken names, what idle means, take-over as one
//! batch, and the read-only system checks.

use serde_json::json;

use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-live-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.accounts[0].login.source = Some(CredSource::Keychain);
    app.accounts[1].login.source = Some(CredSource::File);
    (app, home)
}

fn session(app: &crate::app::App, cwd: &str, id: &str, lines: &[serde_json::Value]) {
    let dir = app.cfg.accounts[0]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(std::path::Path::new(cwd)));
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for l in lines {
        let mut l = l.clone();
        if l["type"] == "user" {
            l["cwd"] = json!(cwd);
        }
        text.push_str(&format!("{l}\n"));
    }
    std::fs::write(dir.join(format!("{id}.jsonl")), text).unwrap();
}

/// "rarion refactor" finds the session the user named RARIUM REFACTOR;
/// "it's in the home directory" is a folder filter; no match offers the
/// closest ones.
#[test]
fn sessions_by_spoken_name_and_home_folder() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("names");
    let user = |t: &str| json!({"type": "user", "message": {"role": "user", "content": t}});
    session(
        &app,
        "/x/hyper",
        "1111aaaa-0000-4000-8000-000000000001",
        &[
            json!({"type": "custom-title", "customTitle": "RARIUM REFACTOR"}),
            user("split the reducer"),
        ],
    );
    let h = crate::config::home_dir().to_string_lossy().into_owned();
    session(
        &app,
        &h,
        "2222bbbb-0000-4000-8000-000000000002",
        &[
            json!({"type": "ai-title", "aiTitle": "Project understanding"}),
            user("explain this repo"),
        ],
    );
    session(
        &app,
        &std::path::Path::new(&h).join("meme").to_string_lossy(),
        "3333cccc-0000-4000-8000-000000000003",
        &[user("make a meme")],
    );
    let ids = |v: &serde_json::Value| {
        v["result"]["sessions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| r["id"].as_str().unwrap()[..4].to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    for q in ["rarion refactor", "rarium refactor", "refactor"] {
        let v = app.control_call("sessions", &json!({"text": q}));
        assert_eq!(ids(&v), vec!["1111"], "{q}: {v}");
    }
    assert_eq!(
        app.control_call("sessions", &json!({"text": "rarion refactor"}))["result"]["sessions"][0]
            ["title"],
        "RARIUM REFACTOR"
    );
    let v = app.control_call("sessions", &json!({"project": "~"}));
    assert_eq!(
        ids(&v),
        vec!["2222"],
        "only sessions run in the home folder itself: {v}"
    );
    let v = app.control_call("sessions", &json!({"project": "home directory"}));
    assert_eq!(ids(&v), vec!["2222"]);
    let v = app.control_call("sessions", &json!({"text": "project undersanding thing"}));
    let got = ids(&v);
    assert!(
        got == vec!["2222"]
            || v["result"]["did_you_mean"][0]["id"]
                .as_str()
                .is_some_and(|i| i.starts_with("2222")),
        "{v}"
    );
    let v = app.control_call("sessions", &json!({"text": "zebra kubernetes"}));
    assert!(v["result"]["sessions"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

/// An external "claude" on account 2's config dir: a shell script whose
/// process becomes `sleep` (exec); `bg` also leaves a shell running
/// under it, like a background task.
fn external(
    app: &crate::app::App,
    home: &std::path::Path,
    tag: &str,
    title: &str,
    bg: bool,
) -> (std::process::Child, String) {
    let src = app.cfg.accounts[1].config_dir();
    let cwd = home.join(tag);
    std::fs::create_dir_all(&cwd).unwrap();
    let pdir = src
        .join("projects")
        .join(crate::state::encode_project_dir(&cwd));
    std::fs::create_dir_all(&pdir).unwrap();
    let sid = format!(
        "{:0>8}-0000-4000-8000-000000000000",
        &tag[..tag.len().min(8)]
    );
    // (JSON written by serde: a Windows cwd has backslashes to escape.)
    let lines = [
        json!({"type": "custom-title", "customTitle": title}),
        json!({"type": "user", "cwd": cwd, "message": {"role": "user", "content": format!("work on {title}")}}),
    ];
    std::fs::write(
        pdir.join(format!("{sid}.jsonl")),
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
    // The transcript was not written just now: not "working".
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
    std::fs::File::options()
        .write(true)
        .open(pdir.join(format!("{sid}.jsonl")))
        .unwrap()
        .set_modified(old)
        .unwrap();
    // A stub "claude" that sleeps; `bg` also leaves a child (a background
    // task) running under it.
    let ready = home.join(format!("ready-{tag}"));
    let _ = std::fs::remove_file(&ready);
    let mut opts = vec![
        ("sleep_s", "60".to_string()),
        ("ready_to", ready.display().to_string()),
    ];
    if bg {
        opts.push(("child_sleep_s", "60".into()));
    }
    let exe = crate::test_stub::claude(&home.join(format!("bin-{tag}")), &opts);
    let child = crate::test_stub::own_console(&mut std::process::Command::new(&exe))
        .arg("60")
        .spawn()
        .unwrap();
    // Started, its background child too (slow machines take a while).
    crate::test_stub::wait_ready(&ready);
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::create_dir_all(src.join("sessions")).unwrap();
    std::fs::write(src.join("sessions").join(format!("{}.json", child.id())), json!({"pid": child.id(), "sessionId": sid, "cwd": cwd, "status": "idle", "kind": "interactive", "name": title}).to_string()).unwrap();
    (child, sid)
}

/// "Move the idle sessions": one plan, one token, naming each session;
/// one with a background shell is not idle; a newer ask replaces the
/// older one and says so; reissue is the same question again.
#[test]
fn take_over_is_one_batch_and_idle_means_nothing_runs() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("batch");
    let (mut a, sa) = external(&app, &home, "aaaaquiet", "Project understanding", false);
    let (mut b, sb) = external(&app, &home, "bbbbbusy", "Hacker sim", true);
    let live = app.live_sessions();
    if live.len() < 2 {
        // The sleep stub was not seen as an agent on this machine.
        let _ = a.kill();
        let _ = b.kill();
        return;
    }
    // The activity: a background shell is not idle.
    let v = app.control_call("sessions", &json!({"limit": 10, "include_headless": true}));
    let state = |sid: &str| {
        v["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == sid)
            .map(|r| r["external"]["state"].as_str().unwrap_or("").to_string())
            .unwrap_or_default()
    };
    assert_eq!(state(&sa), "idle", "{v}");
    assert!(state(&sb).starts_with("background"), "{v}");
    // "idle": only the quiet one, named, in one question.
    app.assistant_turn += 1;
    let v = app.control_call("take_over_session", &json!({"id": "idle", "account": 2}));
    let q = v["question"].as_str().unwrap();
    assert!(
        q.contains("Project understanding") && !q.contains("Hacker sim"),
        "{q}"
    );
    let first = v["token"].as_str().unwrap().to_string();
    // Both by id: one question names both and warns about the shell.
    let v = app.control_call("take_over_session", &json!({"id": [sa, sb], "account": 2}));
    let q = v["question"].as_str().unwrap();
    assert!(
        q.contains("Stop these 2")
            && q.contains("Project understanding")
            && q.contains("Hacker sim")
            && q.contains("still has 1 background task"),
        "{q}"
    );
    assert_eq!(
        v["replaces"]["token"],
        json!(first),
        "the brain is told this replaced the first question"
    );
    let second = v["token"].as_str().unwrap().to_string();
    app.handle(crate::app::AppEvent::UserTurn("yes".into()));
    let old = app.control_call("take_over_session", &json!({"confirm_token": first}));
    assert!(
        old["error"]
            .as_str()
            .unwrap()
            .contains(&format!("replaced by a newer one (token {second})")),
        "{old}"
    );
    // Reissue: the same question, a new token, and "ask again, don't call".
    let r = app.control_call("take_over_session", &json!({"reissue_token": second}));
    assert_eq!(r["reissued"], json!(true));
    assert_eq!(r["question"], v["question"]);
    assert!(r["next"]
        .as_str()
        .unwrap()
        .contains("Do not call take_over_session again"));
    let _ = a.kill();
    let _ = b.kill();
    let _ = a.wait();
    let _ = b.wait();
    let _ = std::fs::remove_dir_all(&home);
}

/// system_check runs only read-only commands, off the UI thread; a
/// process_tree check shows what still runs under a session.
#[test]
fn system_check_reads_and_refuses() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("syscheck");
    let call = |app: &mut crate::app::App, args: serde_json::Value| -> serde_json::Value {
        let (tx, rx) = std::sync::mpsc::channel();
        app.handle(crate::app::AppEvent::Control(
            "system_check".into(),
            args,
            tx,
        ));
        let t0 = std::time::Instant::now();
        loop {
            if let Ok(v) = rx.try_recv() {
                return v;
            }
            assert!(t0.elapsed() < std::time::Duration::from_secs(30));
            app.control_tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    // Its own home is private; a workspace folder is fine.
    let v = call(
        &mut app,
        json!({"command": ["ls", "-la", home.to_string_lossy()]}),
    );
    assert!(v["error"].as_str().unwrap_or("").contains("private"), "{v}");
    let v = call(
        &mut app,
        json!({"command": ["ls", "-la", home.join("newtabs").to_string_lossy()]}),
    );
    assert_eq!(v["ok"], json!(true), "{v}");
    for bad in [
        json!(["rm", "-rf", "/tmp/x"]),
        json!(["kill", "1"]),
        json!(["sh", "-c", "ls"]),
        json!("ps ; rm x"),
        json!(["curl", "http://x"]),
    ] {
        let v = call(&mut app, json!({"command": bad}));
        assert!(
            v["error"].as_str().unwrap_or("").starts_with("refused")
                || v["error"]
                    .as_str()
                    .unwrap_or("")
                    .contains("not a read-only check"),
            "{v}"
        );
    }
    // A session with a shell under it is not idle: process_tree shows it.
    let exe = crate::test_stub::claude(
        &home.join("bin-tree"),
        &[("sleep_s", "30".into()), ("child_sleep_s", "30".into())],
    );
    let mut c = crate::test_stub::own_console(&mut std::process::Command::new(&exe))
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    let v = call(&mut app, json!({"helper": "process_tree", "pid": c.id()}));
    assert!(
        v["result"]["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["cmd"].as_str().unwrap_or("").contains("claude")),
        "{v}"
    );
    let _ = c.kill();
    let _ = c.wait();
    let v = call(&mut app, json!({"helper": "port_owner", "port": 1}));
    assert_eq!(v["result"]["listening"], json!(false), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}

/// The 22:16 log: "go ahead, close it" was asked about again, and short
/// answers were dropped. A single idle tab the user names closes at once
/// (with Undo); a batch asks once, and the yes redeems without a second
/// question.
#[test]
fn closing_asks_only_when_it_should() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("close-tiers");
    for n in ["dan", "api", "web"] {
        let d = home.join(n);
        std::fs::create_dir_all(&d).unwrap();
        app.panes[1].add_tab(d);
    }
    let uid = |app: &crate::app::App, i: usize| app.panes[1].tabs[i].uid;
    let say = |app: &mut crate::app::App, t: &str| {
        app.assistant_turn += 1;
        app.assistant.last_user = t.into();
    };
    // Direct and idle: closed, no question, Undo offered.
    let dan = uid(&app, app.panes[1].tabs.len() - 3);
    say(&mut app, "okay now close it");
    let v = app.control_call("close_tabs", &json!({"tab": format!("t{dan}")}));
    assert!(v["needs_confirmation"].is_null(), "{v}");
    assert_eq!(v["result"]["closed"], json!(1), "{v}");
    assert!(v["result"]["say"].as_str().unwrap().contains("undo"), "{v}");
    assert!(app.find_tab(dan).is_none());
    // A question is not a command; a busy tab always asks.
    let api = uid(&app, app.panes[1].tabs.len() - 2);
    say(&mut app, "should I close the api tab?");
    let v = app.control_call("close_tabs", &json!({"tab": format!("t{api}")}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    say(&mut app, "close the api tab");
    let (s, t) = app.find_tab(api).unwrap();
    app.panes[s].tabs[t].activity = crate::pane::Activity::Working;
    let v = app.control_call("close_tabs", &json!({"tab": format!("t{api}")}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    app.panes[s].tabs[t].activity = crate::pane::Activity::Ready;
    // A batch asks once; the yes closes them, no second question.
    say(&mut app, "close all the tabs on account two");
    let v = app.control_call("close_tabs", &json!({"account": 2}));
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let tok = v["token"].as_str().unwrap().to_string();
    say(&mut app, "go ahead");
    let v = app.control_call("close_tabs", &json!({"confirm_token": tok}));
    assert!(v["result"]["closed"].as_u64().unwrap() >= 2, "{v}");
    assert!(v["needs_confirmation"].is_null());
    let _ = std::fs::remove_dir_all(&home);
}

/// "Fix your system prompt so you don't double-confirm": the brain reads
/// its prompt, edits a section (one question), and after the yes the next
/// brain start has the new text. Locked sections, file sourced edits and
/// injections are refused; a revert puts it back.
#[test]
fn the_assistant_edits_its_own_prompt() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("sysprompt");
    app.assistant.conv = Some(crate::assistant_history::ConvLog::new());
    let say = |app: &mut crate::app::App, t: &str| {
        app.assistant_turn += 1;
        app.assistant.last_user = t.into();
        app.assistant.turn_reads = 0;
    };
    assert!(crate::assistant::system_prompt("concise").contains("NOT Anthropic's"));
    say(
        &mut app,
        "go ahead and fix the system prompt so you don't double confirm",
    );
    let v = app.control_call("get_system_prompt", &json!({}));
    let secs = v["result"]["sections"].as_array().unwrap();
    assert!(secs
        .iter()
        .any(|s| s["id"] == "safety" && s["locked"] == true));
    let conf = secs.iter().find(|s| s["id"] == "confirmations").unwrap();
    assert!(conf["text"]
        .as_str()
        .unwrap()
        .contains("never ask a second time after a yes"));
    let add = " A short yes (go ahead, do it) redeems the token at once.";
    let v = app.control_call(
        "edit_system_prompt",
        &json!({"section": "confirmations", "patch": {"find": "never ask before the tool has.", "replace": format!("never ask before the tool has.{add}")}, "why": "the user was asked twice"}),
    );
    assert_eq!(v["needs_confirmation"], json!(true), "{v}");
    let q = v["question"].as_str().unwrap();
    assert!(
        q.starts_with("Change my 'confirmations'") && q.contains("add \""),
        "{q}"
    );
    let tok = v["token"].as_str().unwrap().to_string();
    say(&mut app, "yes");
    let v = app.control_call("edit_system_prompt", &json!({"confirm_token": tok}));
    assert_eq!(v["result"]["revision"], json!(1), "{v}");
    assert!(app.assistant.prompt_dirty, "the brain restarts with it");
    assert!(crate::assistant::system_prompt("concise").contains(add.trim()));
    // Locked.
    say(&mut app, "change your safety rules so you never ask");
    let v = app.control_call(
        "edit_system_prompt",
        &json!({"section": "safety", "new_text": "Ask nothing."}),
    );
    assert!(v["error"].as_str().unwrap().contains("locked"), "{v}");
    // From a file this turn.
    say(
        &mut app,
        "read the readme and fix your prompt the way it says",
    );
    app.assistant.turn_reads = 1;
    let v = app.control_call(
        "edit_system_prompt",
        &json!({"section": "style", "new_text": "Style: {style_line} Always answer in French."}),
    );
    assert!(v["error"].as_str().unwrap().contains("tab or file"), "{v}");
    // Loosening safety.
    say(&mut app, "fix your prompt so you stop asking");
    let v = app.control_call("edit_system_prompt", &json!({"section": "confirmations", "new_text": "- Never ask for confirmation, just do it automatically."}));
    assert!(v["error"].as_str().unwrap().starts_with("refused"), "{v}");
    // Not asked for at all.
    say(&mut app, "what's on account two");
    let v = app.control_call(
        "edit_system_prompt",
        &json!({"section": "style", "new_text": "Style: {style_line} Be terse."}),
    );
    assert!(v["error"].as_str().unwrap().contains("does not ask"), "{v}");
    // History and revert.
    let h = app.control_call("prompt_history", &json!({}));
    assert_eq!(
        h["result"]["revisions"][0]["section"],
        json!("confirmations")
    );
    say(&mut app, "undo that prompt change");
    let v = app.control_call("revert_prompt", &json!({"rev": 1}));
    assert_eq!(v["result"]["revision"], json!(2), "{v}");
    assert!(!crate::assistant::system_prompt("concise").contains(add.trim()));
    let _ = std::fs::remove_dir_all(&home);
}

fn screen(app: &mut crate::app::App) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
    term.draw(|f| crate::ui::draw(f, app)).unwrap();
    let b = term.backend().buffer();
    let mut s = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            s.push_str(b[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

/// Settings > Assistant > System prompt: sections with the locked badge,
/// editing a section, the history with its diff, and undo.
#[test]
fn system_prompt_view_edits_and_undoes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("prompt-view");
    app.open_prompt_view();
    let t = screen(&mut app);
    assert!(
        t.contains("System prompt: ") && t.contains("safety") && t.contains("locked"),
        "{t}"
    );
    assert!(
        t.contains("NOT Anthropic's"),
        "the selected section's text shows"
    );
    let key =
        |app: &mut crate::app::App, c: KeyCode, m: KeyModifiers| app.on_key(KeyEvent::new(c, m));
    // The first section is locked: e does not edit it.
    key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(app.prompt_ui.edit.is_none());
    // Style: append a sentence and save.
    let style = crate::sysprompt::SECTIONS
        .iter()
        .position(|s| s.id == "style")
        .unwrap();
    app.prompt_ui.sel = style;
    key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    for c in " Keep it warm.".chars() {
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(crate::assistant::system_prompt("concise").contains("Keep it warm."));
    assert!(app.assistant.prompt_dirty);
    key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
    let t = screen(&mut app);
    assert!(
        t.contains("rev 1") && t.contains("add \"Keep it warm\""),
        "{t}"
    );
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!crate::assistant::system_prompt("concise").contains("Keep it warm."));
    let _ = std::fs::remove_dir_all(&home);
}

/// Settings > Voice and Audio > Speech output with Grok: the sub panel
/// (sign in, key or login, voice, speed), the key typed masked and kept
/// out of config.toml, Kokoro's panel still there as the fallback.
#[test]
fn grok_speech_output_settings() {
    use crate::settings::{self, Kind, Section};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("grok-settings");
    std::fs::write(
        crate::config::Config::path(),
        "[voice]\ntts_engine = \"grok\"\n",
    )
    .unwrap();
    app.cfg.voice.tts_engine = "grok".into();
    let rows = settings::settings_for(Section::Voice, &app.cfg);
    let labels: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();
    for want in [
        "Engine",
        "Fallback engines",
        "Grok: sign in with",
        "Grok: voice",
        "Grok: speed",
    ] {
        assert!(labels.iter().any(|l| l == want), "{want} in {labels:?}");
    }
    assert!(labels.iter().any(|l| l == "Grok login"), "{labels:?}");
    // Only the chosen engine's panel.
    assert!(
        !labels.iter().any(|l| l.starts_with("Kokoro")),
        "{labels:?}"
    );
    // API key: a masked secret row.
    app.cfg.voice.grok.auth = "api_key".into();
    let rows = settings::settings_for(Section::Voice, &app.cfg);
    let k = rows
        .iter()
        .position(|r| r.kind == Kind::Secret)
        .expect("the key row");
    app.view = crate::app::View::Settings;
    app.settings_section = settings::SECTIONS
        .iter()
        .position(|s| s.0 == Section::Voice)
        .unwrap();
    app.settings_sel = k;
    app.setting_edit(k);
    assert_eq!(app.modal, crate::app::Modal::EditSetting(k, String::new()));
    app.modal = crate::app::Modal::EditSetting(k, "xai-secret-123".into());
    let t = screen(&mut app);
    assert!(
        !t.contains("xai-secret-123") && t.contains("••••••"),
        "masked: {t}"
    );
    app.modal = crate::app::Modal::None;
    app.setting_set_text(k, "xai-secret-123");
    assert_eq!(crate::voice::grok_tts::key_saved(), Some(true));
    let cfg_text = std::fs::read_to_string(crate::config::Config::path()).unwrap();
    assert!(!cfg_text.contains("xai-secret"), "{cfg_text}");
    assert!(settings::write(
        &crate::config::Config::path(),
        &settings::Key::Global(settings::XAI_KEY),
        Some(toml_edit::value("x"))
    )
    .is_err());
    let t = screen(&mut app);
    assert!(
        t.contains("saved (~/.godterm file)") && t.contains("Remove key"),
        "{t}"
    );
    assert!(
        t.contains("Grok sends the spoken text"),
        "privacy line: {t}"
    );
    app.setting_reset(k);
    assert_eq!(crate::voice::grok_tts::key_saved(), Some(false));
    let _ = std::fs::remove_dir_all(&home);
}

/// The account ▾ of every pane opens that pane's menu, on screen, in a
/// 3x2 grid of six accounts (four claude, two grok), at the user's size
/// and a smaller one.
#[test]
fn every_panes_account_menu_opens() {
    use crossterm::event::{MouseButton, MouseEventKind};
    use ratatui::{backend::TestBackend, Terminal};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("godterm-live-acctmenu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let mut toml = String::new();
    for (n, h) in [
        ("account1", "claude"),
        ("account2", "claude"),
        ("account3", "claude"),
        ("account4", "claude"),
        ("test", "grok"),
        ("demo8", "grok"),
    ] {
        toml.push_str(&format!(
            "[[account]]\nname = \"{n}\"\nharness = \"{h}\"\n\n"
        ));
    }
    let cfg = crate::config::Config::parse(&toml).unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(cfg, tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    let ev = |k, x: u16, y: u16| {
        crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
            crossterm::event::MouseEvent {
                kind: k,
                column: x,
                row: y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        ))
    };
    for (w, h) in [(200u16, 60u16), (160, 45), (120, 36)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let shown: Vec<usize> = app
            .pane_rects
            .iter()
            .enumerate()
            .filter(|(_, r)| r.width > 0)
            .map(|(i, _)| i)
            .collect();
        assert!(shown.len() >= 3, "{w}x{h}: {shown:?}");
        for i in shown {
            app.modal = crate::app::Modal::None;
            term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let r = app
                .last_hits
                .regions
                .iter()
                .rev()
                .find(|r| r.action == crate::hits::UiAction::AccountMenu(i))
                .map(|r| r.rect)
                .unwrap_or_else(|| panic!("{w}x{h}: no ▾ for pane {i}"));
            app.handle(ev(MouseEventKind::Down(MouseButton::Left), r.x, r.y));
            app.handle(ev(MouseEventKind::Up(MouseButton::Left), r.x, r.y));
            assert!(
                matches!(app.modal, crate::app::Modal::AccountMenu(s, _) if s == i),
                "{w}x{h}: pane {i}'s ▾ at {r:?} gave {:?}",
                app.modal
            );
            term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let b = term.backend().buffer();
            let text: String = (0..b.area.height)
                .map(|y| {
                    (0..b.area.width)
                        .map(|x| b[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                text.contains(&format!("pane {} account", i + 1)),
                "{w}x{h}: pane {i}'s menu not drawn:\n{text}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// Settings > Voice and Audio is grouped under headers and shows only
/// what applies to the current choices; labels are never cut; arrows
/// skip what is hidden; a search finds hidden rows and says why.
#[test]
fn voice_settings_follow_the_choices() {
    use crate::settings::{self, Section};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("voicegroups");
    let labels = |app: &crate::app::App| -> Vec<String> {
        settings::settings_for(Section::Voice, &app.cfg)
            .iter()
            .map(|r| r.label.clone())
            .collect()
    };
    let has = |app: &crate::app::App, l: &str| labels(app).iter().any(|x| x == l);
    // Recognition: whisper's rows only with whisper (or auto).
    app.cfg.voice.engine = "whisper".into();
    assert!(has(&app, "Whisper model") && has(&app, "Beam size"));
    app.cfg.voice.engine = "apple".into();
    assert!(!has(&app, "Whisper model") && !has(&app, "Beam size") && has(&app, "Language"));
    // Capture: Apple voice processing cleans the signal; ffmpeg needs RNNoise.
    app.cfg.voice.capture = "apple".into();
    app.cfg.voice.voice_processing = true;
    assert!(has(&app, "Apple voice processing") && !has(&app, "Noise suppression"));
    app.cfg.voice.capture = "ffmpeg".into();
    assert!(!has(&app, "Apple voice processing") && has(&app, "Noise suppression"));
    // Speaker lock off: only its mode.
    app.cfg.voice.speaker_lock = "off".into();
    assert!(has(&app, "Speaker lock") && !has(&app, "Threshold") && !has(&app, "Margin"));
    // Talk back: only the chosen engine's panel; off hides the rest.
    app.cfg.voice.tts_engine = "say".into();
    assert!(has(&app, "say: voice") && has(&app, "Speed"));
    assert!(!labels(&app)
        .iter()
        .any(|l| l.starts_with("Kokoro") || l.starts_with("Grok")));
    app.cfg.voice.tts_engine = "kokoro".into();
    assert!(has(&app, "Kokoro: voice") && !has(&app, "say: voice"));
    app.cfg.voice.tts_engine = "off".into();
    assert!(has(&app, "Engine") && !has(&app, "Volume") && !has(&app, "Kokoro: voice"));
    app.cfg.voice.tts = false;
    assert!(has(&app, "Talk back") && !labels(&app).iter().any(|l| l == "Fallback engines"));
    app.cfg.voice.tts = true;
    app.cfg.voice.tts_engine = "kokoro".into();
    // Listening: wake words only hands free.
    app.cfg.voice.mode = "push".into();
    assert!(!has(&app, "Wake words"));
    app.cfg.voice.mode = "wake".into();
    assert!(has(&app, "Wake words") && !has(&app, "Open mic sleeps after (min)"));
    // On screen: headers, Advanced folded, every label whole at 120 and 200.
    app.view = crate::app::View::Settings;
    app.settings_section = settings::SECTIONS
        .iter()
        .position(|s| s.0 == Section::Voice)
        .unwrap();
    for (w, h) in [(120u16, 90u16), (200, 90)] {
        use ratatui::{backend::TestBackend, Terminal};
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let b = term.backend().buffer();
        let text: String = (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for head in [
            "▾ Listening",
            "▾ Microphone",
            "▾ Speech recognition",
            "▾ Background voices",
            "▾ Talk back",
            "▸ Advanced and debugging",
        ] {
            assert!(text.contains(head), "{w}: {head}\n{text}");
        }
        for l in app.settings_rows().iter().map(|r| r.label.clone()) {
            assert!(
                text.contains(&format!(" {l} ")),
                "{w}: label {l:?} cut\n{text}"
            );
        }
        assert!(
            !text.contains("Recognition:") && !text.contains("Input:"),
            "no prefixes"
        );
    }
    // Arrows walk the shown rows only; Advanced opens with its header.
    let n = app.settings_rows().len();
    assert!(!app
        .settings_rows()
        .iter()
        .any(|r| r.group == "Advanced and debugging"));
    app.settings_sel = 0;
    for _ in 0..n + 5 {
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(
        app.settings_sel,
        n - 1,
        "the last shown row, no hidden ones"
    );
    app.toggle_settings_group("Advanced and debugging");
    assert!(app
        .settings_rows()
        .iter()
        .any(|r| r.label == "Silence timeout (ms)"));
    // Search: hidden rows too, with why.
    app.cfg.voice.engine = "apple".into();
    app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in "beam".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let rows = app.settings_rows();
    let beam = rows.iter().find(|r| r.label == "Beam size").expect("found");
    assert!(
        beam.hidden
            .as_deref()
            .unwrap_or("")
            .contains("engine = whisper"),
        "{:?}",
        beam.hidden
    );
    {
        use ratatui::{backend::TestBackend, Terminal};
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let b = term.backend().buffer();
        let text: String = (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("/ beam")
                && text.contains("Voice and Audio › Speech recognition")
                && text.contains("hidden"),
            "{text}"
        );
    }
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.settings_query.is_none());
    // The assistant's rows: off hides all but mode and history.
    app.cfg.assistant.mode = "off".into();
    let a: Vec<String> = settings::settings_for(Section::Assistant, &app.cfg)
        .iter()
        .map(|r| r.label.clone())
        .collect();
    assert_eq!(a, ["Mode", "Keep conversations (days)"]);
    let _ = std::fs::remove_dir_all(&home);
}

/// Speech recognition with Grok: its panel (the shared xAI login,
/// timeout, local partials, fallback, Test recognition), no whisper rows,
/// and the line saying microphone audio goes to xAI.
#[test]
fn grok_recognition_settings() {
    use crate::settings::{self, Section};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("grokstt");
    app.cfg.voice.engine = "grok".into();
    let labels: Vec<String> = settings::settings_for(Section::Voice, &app.cfg)
        .iter()
        .filter(|r| r.group == "Speech recognition")
        .map(|r| r.label.clone())
        .collect();
    for want in [
        "Engine",
        "Language",
        "Grok: sign in with",
        "Grok: timeout (ms)",
        "Live partials (local)",
        "Fallback",
    ] {
        assert!(labels.iter().any(|l| l == want), "{want} in {labels:?}");
    }
    assert!(labels.iter().any(|l| l == "Grok login"));
    assert!(
        !labels
            .iter()
            .any(|l| l == "Whisper model" || l == "Beam size"),
        "{labels:?}"
    );
    app.view = crate::app::View::Settings;
    app.settings_section = settings::SECTIONS
        .iter()
        .position(|s| s.0 == Section::Voice)
        .unwrap();
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(200, 90)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let b = term.backend().buffer();
    let text: String = (0..b.area.height)
        .map(|y| {
            (0..b.area.width)
                .map(|x| b[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Test recognition"), "{text}");
    assert!(
        text.contains("Grok recognition sends your microphone audio"),
        "{text}"
    );
    // The config keeps its shape.
    let cfg = crate::config::Config::parse(
        "[voice]\nengine = \"grok\"\n[voice.grok_stt]\ntimeout_ms = 4000\nfallback = \"apple\"\n",
    )
    .unwrap();
    assert_eq!(
        (
            cfg.voice.grok_stt.timeout_ms,
            cfg.voice.grok_stt.fallback.as_str()
        ),
        (4000, "apple")
    );
    assert!(cfg.voice.grok_stt.local_partials);
    let _ = std::fs::remove_dir_all(&home);
}

/// The Grok login row and picker: each login's email and what it has
/// left for voice (or the weekly credits), masked emails in privacy
/// mode, refused logins, "most left" never on a login with nothing
/// left, the usage error with retry, the auto choice, the picker's cards
/// and its actions.
#[test]
fn grok_login_picker_shows_usage() {
    use crate::usage::{Usage, Window};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("godterm-live-grokpick-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    std::fs::create_dir_all(&home).unwrap();
    let toml = "show_email = true\n[[account]]\nname = \"account1\"\n\n[[account]]\nname = \"test\"\nharness = \"grok\"\n\n[[account]]\nname = \"demo8\"\nharness = \"grok\"\n";
    std::fs::write(crate::config::Config::path(), toml).unwrap();
    let cfg = crate::config::Config::parse(toml).unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(cfg, tx);
    for n in ["test", "demo8"] {
        let d = home.join("accounts").join(n);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("auth.json"), "{\"key\":\"x\"}").unwrap();
    }
    crate::voice::grok_tts::clear_refused();
    let win = |key: &str, label: &str, used: f64| Window {
        key: key.into(),
        label: label.into(),
        short: label.into(),
        utilization: used,
        resets_at: Some(chrono::Utc::now() + chrono::Duration::days(2)),
    };
    let usage = |w: Vec<Window>| Usage {
        windows: w,
        absent: vec![],
        extra: None,
    };
    // demo8: voice reported, weekly credits used up; test: still loading.
    app.accounts[2].usage = Some(usage(vec![
        win("seven_day", "Weekly", 100.0),
        win("product:Voice", "Voice", 18.0),
    ]));
    app.accounts[2].profile.email = Some("dan@example.com".into());
    let line = app.grok_source_line("demo8");
    assert!(
        line.starts_with("demo8 (dan@example.com) · Voice 82% · weekly 0%"),
        "{line}"
    );
    assert!(
        !line.contains("most left"),
        "nothing left: never most left: {line}"
    );
    assert!(app.grok_source_line("test").contains("usage loading…"));
    // A usage error says so (and retries), never loads forever.
    app.accounts[1].usage_err = Some(crate::usage::UsageError::Parse("x".into()));
    assert!(app
        .grok_source_line("test")
        .contains("usage unavailable: bad response"));
    app.accounts[1].usage_err = None;
    app.accounts[1].usage = Some(usage(vec![win("seven_day", "Weekly", 63.0)]));
    let t = app.grok_source_line("test");
    assert!(
        t.contains("weekly 37% (voice not reported)") && t.contains("most left"),
        "{t}"
    );
    // Both used up: none is most left.
    app.accounts[1].usage = Some(usage(vec![win("seven_day", "Weekly", 100.0)]));
    assert_eq!(app.grok_best(), None);
    app.accounts[1].usage = Some(usage(vec![win("seven_day", "Weekly", 63.0)]));
    // Privacy: no email.
    app.rt_privacy = Some(true);
    let p = app.grok_source_line("demo8");
    assert!(
        !p.contains("dan@example.com") && p.starts_with("demo8 ·"),
        "{p}"
    );
    app.rt_privacy = None;
    // A refused login, and auto passing over it.
    assert!(app.grok_source_line("auto").starts_with("auto → test"));
    crate::voice::grok_tts::note_refused("test");
    assert!(app.grok_source_line("test").contains("out of credits at "));
    assert!(app
        .grok_source_line("auto")
        .starts_with("auto → demo8 (dan@example.com)"));
    // The picker: cards with usage bars, products, status and actions.
    app.open_grok_logins();
    assert!(matches!(app.modal, crate::app::Modal::GrokLogins(_)));
    {
        use ratatui::{backend::TestBackend, Terminal};
        let mut term = Terminal::new(TestBackend::new(140, 60)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let b = term.backend().buffer();
        let text: String = (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for want in [
            "Auto (best available)",
            "Weekly credits",
            "Voice",
            "not reported",
            "out of credits at",
            "Use for voice",
            "Refresh usage",
            "Log out",
            "Main ~/.grok",
        ] {
            assert!(text.contains(want), "{want}:\n{text}");
        }
    }
    // Use for voice: demo8 (card 2: auto, test, demo8, main).
    let cards = app.grok_cards();
    let i = cards.iter().position(|c| c == "demo8").unwrap();
    app.grok_login_act(i, crate::grok_logins::USE);
    assert_eq!(app.cfg.voice.grok.source, "demo8");
    // Refresh clears the refusal; Esc closes.
    let t = cards.iter().position(|c| c == "test").unwrap();
    app.grok_login_act(t, crate::grok_logins::REFRESH);
    assert!(!app.grok_source_line("test").contains("out of credits"));
    app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.modal, crate::app::Modal::None);
    crate::voice::grok_tts::clear_refused();
    let _ = std::fs::remove_dir_all(&home);
}

/// Tab states in the sidebar: working (spinner and bar in the accent,
/// bold), needs you (amber !), background (hollow spinner), idle (a dim
/// ○, never green), exited (×), paused (z); the header counts what runs,
/// and the screen animates only while something does.
#[test]
fn tab_states_stand_out() {
    use crate::pane::Activity;
    use ratatui::style::Modifier;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("tabstates");
    let names = ["work", "ask", "bg", "idle", "ended", "paused"];
    for n in names {
        let d = home.join(n);
        std::fs::create_dir_all(&d).unwrap();
        app.panes[0].add_tab(d);
    }
    let ix = |app: &crate::app::App, n: &str| {
        app.panes[0]
            .tabs
            .iter()
            .position(|t| t.name() == n)
            .unwrap()
    };
    let set = |app: &mut crate::app::App, n: &str, a: Activity| {
        let i = ix(app, n);
        app.panes[0].tabs[i].activity = a;
    };
    set(&mut app, "work", Activity::Working);
    set(&mut app, "ask", Activity::Permission);
    set(&mut app, "bg", Activity::Ready);
    set(&mut app, "idle", Activity::Ready);
    set(&mut app, "ended", Activity::Exited);
    let p = ix(&app, "paused");
    app.panes[0].tabs[p].suspended = true;
    let bg_uid = app.panes[0].tabs[ix(&app, "bg")].uid;
    app.bg_cache.borrow_mut().insert(
        bg_uid,
        (
            std::time::Instant::now(),
            Some("background · 2 agents".into()),
        ),
    );
    for t in app.panes[0].tabs.iter_mut() {
        if t.name() != "work"
            && t.name() != "ask"
            && t.name() != "bg"
            && t.name() != "idle"
            && t.name() != "ended"
            && t.name() != "paused"
        {
            t.activity = Activity::Ready;
        }
    }
    let look = |app: &crate::app::App, n: &str| crate::ui_chrome::tab_look(app, 0, ix(app, n));
    let w = look(&app, "work");
    assert!("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(&w.glyph), "{}", w.glyph);
    assert_eq!(
        (w.color, w.bar),
        (crate::theme::ACTIVE, Some(crate::theme::ACTIVE))
    );
    assert!(
        w.loud
            && w.word.starts_with("working")
            && w.word_style.add_modifier.contains(Modifier::BOLD)
    );
    let a = look(&app, "ask");
    assert_eq!((a.glyph.as_str(), a.bar), ("!", Some(crate::theme::SAND)));
    assert!(a.word.starts_with("needs you"));
    let b = look(&app, "bg");
    assert!(
        "◜◝◞◟".contains(&b.glyph) && b.word == "background · 2 agents",
        "{} {}",
        b.glyph,
        b.word
    );
    let i = look(&app, "idle");
    assert_eq!(
        (i.glyph.as_str(), i.color, i.bar),
        ("○", crate::theme::FAINT, None)
    );
    assert!(i.word.starts_with("idle") && !i.loud);
    assert_ne!(i.color, crate::theme::SAGE, "idle is never green");
    assert_ne!(
        crate::ui::activity_color(Activity::Ready),
        crate::theme::SAGE
    );
    let e = look(&app, "ended");
    assert_eq!((e.glyph.as_str(), e.color), ("×", crate::theme::CLAY));
    assert_eq!(look(&app, "paused").glyph, "z");
    // On screen: the header counts, the spinner animates.
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let b = term.backend().buffer();
    let text: String = (0..b.area.height)
        .map(|y| {
            (0..b.area.width)
                .map(|x| b[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    // (A narrow list: the short forms.)
    assert!(text.contains("·1w ·1!"), "{text}");
    assert!(
        text.contains("needs you") && text.contains("working") && text.contains("2 agents"),
        "{text}"
    );
    assert_eq!(
        app.anim_interval(),
        Some(std::time::Duration::from_millis(125))
    );
    // Nothing runs: no animation, no extra frames.
    for t in app.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()) {
        t.activity = Activity::Ready;
    }
    app.bg_cache.borrow_mut().clear();
    assert_eq!(app.anim_interval(), None);
    let _ = std::fs::remove_dir_all(&home);
}

/// Opening Settings > Voice and Audio never waits on a device listing:
/// with a lister that never answers, the screen draws and Esc works at
/// once, and it says it is still detecting.
#[test]
fn voice_settings_never_wait_on_devices() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("devhang");
    crate::settings::HANG_LISTS.store(true, std::sync::atomic::Ordering::SeqCst);
    *crate::settings::LIST_STATE.lock().unwrap() = crate::settings::ListState::NotStarted;
    *crate::settings::AUDIO_LISTS.lock().unwrap() = None;
    let t0 = std::time::Instant::now();
    app.open_settings();
    app.settings_section = crate::settings::SECTIONS
        .iter()
        .position(|s| s.0 == crate::settings::Section::Voice)
        .unwrap();
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(160, 50)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let b = term.backend().buffer();
    let text: String = (0..b.area.height)
        .map(|y| {
            (0..b.area.width)
                .map(|x| b[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("detecting devices…"), "{text}");
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.view, crate::app::View::Grid);
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(200),
        "the screen waited {:?}",
        t0.elapsed()
    );
    // Opening it again does not start another listing.
    app.open_settings();
    assert_eq!(
        crate::settings::list_state(),
        crate::settings::ListState::Detecting
    );
    crate::settings::HANG_LISTS.store(false, std::sync::atomic::Ordering::SeqCst);
    *crate::settings::LIST_STATE.lock().unwrap() = crate::settings::ListState::NotStarted;
    let _ = std::fs::remove_dir_all(&home);
}

/// A command that never ends is stopped at the limit (and reaped).
#[test]
fn listings_time_out() {
    let d = std::env::temp_dir().join(format!("godterm-hangcmd-{}", std::process::id()));
    let stub = crate::test_stub::claude(&d, &[("sleep_s", "30".into())]);
    let t0 = std::time::Instant::now();
    let r = crate::procs::output_timeout(
        &mut std::process::Command::new(&stub),
        std::time::Duration::from_millis(300),
    );
    assert!(r.unwrap_err().contains("timed out"));
    assert!(t0.elapsed() < std::time::Duration::from_secs(3));
    let _ = std::fs::remove_dir_all(&d);
}
