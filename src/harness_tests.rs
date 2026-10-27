use crate::config::{AccountCfg, Config};
use crate::harness::Harness;
use crate::pane::LaunchKind;

fn account(h: Harness) -> AccountCfg {
    let mut a = Config::default().accounts.remove(0);
    a.name = h.name().into();
    a.harness = h.name().into();
    a.permission_mode = Some("plan".into());
    a
}

#[test]
fn native_flags_and_registry() {
    for &h in crate::harness::ALL {
        assert_eq!(Harness::of(h.name()), h);
    }
    assert_eq!(Harness::of("cursor-agent"), Harness::Cursor);
    assert_eq!(Harness::of("agy"), Harness::Antigravity);
    assert_eq!(Harness::Codex.resume_args("id"), ["resume", "id"]);
    assert_eq!(Harness::Cursor.resume_args("id"), ["--resume", "id"]);
    assert_eq!(
        Harness::Antigravity.resume_args("id"),
        ["--conversation", "id"]
    );
    assert_eq!(Harness::OpenCode.resume_args("id"), ["--session", "id"]);
    assert_eq!(
        Harness::Codex.permission_args("plan"),
        ["--sandbox", "read-only"]
    );
    assert_eq!(Harness::Cursor.permission_args("plan"), ["--mode", "plan"]);
    assert_eq!(
        Harness::Antigravity.permission_args("accept-edits"),
        ["--mode", "accept-edits"]
    );

    assert!(Harness::OpenCode.permission_args("plan").is_empty());
    assert_eq!(Harness::OpenCode.permission_args("bypass"), ["--auto"]);
    assert_eq!(Harness::OpenCode.login_args().unwrap(), ["auth", "login"]);
    assert_eq!(Harness::Cursor.login_args().unwrap(), ["login"]);
    assert_eq!(Harness::Codex.login_args().unwrap(), ["login"]);
    assert!(Harness::Antigravity.login_args().is_none());
    for h in [Harness::Cursor, Harness::Antigravity, Harness::OpenCode] {
        assert!(h.home_env().is_empty(), "no unsupported isolation variable");
        assert!(
            h.permission_args("dont-ask").is_empty(),
            "unsupported modes use native defaults"
        );
    }
}

#[test]
fn native_login_is_startable_without_claiming_authentication() {
    for h in [
        Harness::Codex,
        Harness::Cursor,
        Harness::Antigravity,
        Harness::OpenCode,
    ] {
        let snap = crate::app::snapshot_of(&account(h), true);
        assert!(snap.login.cli_managed && snap.login.can_start());
        assert!(!snap.login.logged_in());
        assert!(snap.usage.is_none());
        assert!(crate::providers::for_harness(h).is_none());
        let source = crate::admin::Source::Command(vec!["server".into()]);
        assert!(crate::admin::install_steps(h, "example", &source, false, false, &[]).is_err());
        assert!(crate::admin::remove_steps(h, "example").is_empty());
        assert!(crate::admin::plugin_steps(h, "install", "example", None).is_empty());
        assert!(crate::admin::list_args(h).is_empty());
    }
}

#[test]
fn native_parent_markers_are_scrubbed_and_pass_env_is_respected() {
    for key in [
        "CODEX_HOME",
        "CODEX_THREAD_ID",
        "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
    ] {
        assert!(crate::pane::should_scrub(key, &[]));
        assert!(!crate::pane::should_scrub(key, &[key.into()]));
    }
    assert!(!crate::pane::should_scrub("OPENAI_API_KEY", &[]));
    assert!(!crate::pane::should_scrub("CURSOR_API_KEY", &[]));
}

#[test]
fn native_pty_launch_resume_login_and_autostart() {
    let _g = crate::config::testing::LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("godterm-native-launch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    crate::config::testing::set_home(&home);
    let args = home.join("args.txt");
    let env = home.join("env.txt");
    let stub = crate::test_stub::claude(
        &home.join("bin"),
        &[
            ("args_to", args.display().to_string()),
            ("home_env_to", env.display().to_string()),
        ],
    )
    .display()
    .to_string();
    let cfg = Config {
        accounts: vec![account(Harness::Codex)],
        codex_bin: Some(stub.clone()),
        cursor_bin: Some(stub.clone()),
        antigravity_bin: Some(stub.clone()),
        opencode_bin: Some(stub),
        ..Config::default()
    };
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(cfg, tx);

    app.cfg.tabs.jobs_tools = false;
    for h in [
        Harness::Codex,
        Harness::Cursor,
        Harness::Antigravity,
        Harness::OpenCode,
    ] {
        app.cfg.accounts[0] = account(h);
        app.accounts[0].login = crate::app::snapshot_of(&app.cfg.accounts[0], false).login;
        assert!(app
            .grid_targets(&serde_json::json!(h.name()))
            .unwrap()
            .contains(&0));
        for kind in [
            LaunchKind::Normal,
            LaunchKind::Resume("test-session".into()),
            LaunchKind::Login,
        ] {
            let _ = std::fs::remove_file(&args);
            let _ = std::fs::remove_file(&env);
            app.launch(0, kind.clone());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while (!args.is_file() || !env.is_file()) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let actual = std::fs::read_to_string(&args).expect("stand-in received args");

            let own: Vec<String> = if h == Harness::OpenCode {
                vec!["--standalone".into()]
            } else {
                vec![]
            };
            let expected = match &kind {
                LaunchKind::Login if h.login_args().is_some() => h.login_args().unwrap(),
                LaunchKind::Login => h.permission_args("plan"),
                LaunchKind::Resume(id) => {
                    [own, h.permission_args("plan"), h.resume_args(id)].concat()
                }
                _ => [own, h.permission_args("plan")].concat(),
            };
            assert_eq!(actual.trim(), expected.join(" "), "{} {kind:?}", h.name());
            let actual_env = std::fs::read_to_string(&env).unwrap();
            assert!(!actual_env.contains("CLAUDE_CONFIG_DIR="));
            assert!(!actual_env.contains("GROK_HOME="));
            if h == Harness::Codex {
                assert!(actual_env.contains(&format!(
                    "CODEX_HOME={}",
                    app.cfg.accounts[0].config_dir().display()
                )));
            } else {
                assert!(!actual_env.contains("CODEX_HOME="));
            }
            app.panes[0].kill_all();
        }
    }

    let a = account(Harness::Codex);
    app.cfg.accounts[0] = a;
    app.accounts[0].login = crate::app::snapshot_of(&app.cfg.accounts[0], false).login;
    app.panes[0].cur_mut().state = crate::pane::PaneState::Idle;
    app.autostart();
    assert!(app.panes[0].cur().is_running());
    app.shutdown();
    let _ = std::fs::remove_dir_all(home);
}

use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::{Activity, PaneState};
use serde_json::json;

fn native_app(tag: &str, h: Harness) -> (crate::app::App, usize, std::path::PathBuf) {
    let (mut app, home) = crate::confirm_click_tests::test_app(tag);
    app.cfg.accounts[0].harness = h.name().into();
    app.cfg.accounts[0].permission_mode = Some("plan".into());
    app.accounts[0].login = crate::app::snapshot_of(&app.cfg.accounts[0], false).login;
    let t = crate::jobs_tests::running(&mut app, 0, "native", "sess-1");
    (app, t, home)
}

const NEW: [Harness; 4] = [
    Harness::Codex,
    Harness::Cursor,
    Harness::Antigravity,
    Harness::OpenCode,
];

#[test]
fn resume_ids_cannot_become_options() {
    for &h in crate::harness::ALL {
        assert_eq!(
            h.resume_args("0b1c2d3e-4f50-6789-abcd-ef0123456789").len(),
            2
        );
        for bad in [
            "--dangerously-bypass-approvals-and-sandbox",
            "-x",
            "a b",
            "a\nb",
            "id;rm",
            "",
            "$(x)",
        ] {
            assert!(
                h.resume_args(bad).is_empty(),
                "{} accepted {bad:?}",
                h.name()
            );
        }
    }
}

#[test]
fn tab_tools_use_each_clis_per_launch_mcp_and_never_a_global_config() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(Harness::Claude.has_tab_tools() && Harness::Grok.has_tab_tools());
    for h in [Harness::Codex, Harness::Cursor, Harness::OpenCode] {
        assert!(h.has_tab_tools(), "{}", h.name());
        assert!(h.no_tab_tools_reason().is_none());
    }
    let exe = std::path::Path::new("/opt/godterm/godterm");
    let shared = "shared-secret";
    let token = crate::tab_jobs::tab_token(shared, 7);

    let (_app, _t, home) = native_app("tt-codex", Harness::Codex);
    let slot = home.join("slot-home");
    std::fs::create_dir_all(&slot).unwrap();
    std::fs::write(
        slot.join("config.toml"),
        "developer_instructions = \"Be brief.\"\n",
    )
    .unwrap();
    let mut args = vec!["--sandbox".to_string(), "read-only".to_string()];
    let x =
        crate::tab_jobs::extend_launch_for(Harness::Codex, exe, shared, 7, &mut args, &slot, false)
            .unwrap();
    assert_eq!(args[..2], ["--sandbox", "read-only"]);
    let overrides: Vec<&str> = args[2..]
        .chunks(2)
        .map(|c| {
            assert_eq!(c[0], "-c");
            c[1].as_str()
        })
        .collect();
    assert!(overrides.contains(&"mcp_servers.godterm.command=\"/opt/godterm/godterm\""));
    assert!(overrides.contains(&"mcp_servers.godterm.args=[\"mcp\"]"));
    assert!(overrides.contains(&"mcp_servers.godterm.env_vars=[\"GODTERM_TAB\",\"GODTERM_TOKEN\"]"));
    assert!(overrides.contains(&"mcp_servers.godterm.default_tools_approval_mode=\"approve\""));
    let env_line = overrides
        .iter()
        .find(|o| o.starts_with("mcp_servers.godterm.env={"))
        .unwrap();
    assert!(env_line.contains("GODTERM_CLIENT=\"tab\"") && env_line.contains("GODTERM_HOME="));
    let dev = overrides
        .iter()
        .find(|o| o.starts_with("developer_instructions="))
        .unwrap();
    assert!(
        dev.contains("Be brief.") && dev.contains("create_job"),
        "{dev}"
    );

    assert!(!args.iter().any(|a| a.contains(&token)));
    assert_eq!(
        x.env,
        vec![
            ("GODTERM_TAB".to_string(), "7".to_string()),
            ("GODTERM_TOKEN".to_string(), token.clone())
        ]
    );

    assert_eq!(std::fs::read_dir(&slot).unwrap().count(), 1);

    let mut args = vec![];
    let x = crate::tab_jobs::extend_launch_for(
        Harness::Cursor,
        exe,
        shared,
        7,
        &mut args,
        &slot,
        false,
    )
    .unwrap();
    assert!(x.env.is_empty());
    assert_eq!(args[0], "--plugin-dir");
    let dir = std::path::PathBuf::from(&args[1]);
    assert!(dir.starts_with(crate::config::app_home()));
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".cursor-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["name"], json!("godterm-tab-7"));
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("mcp.json")).unwrap()).unwrap();
    let s = &mcp["mcpServers"]["godterm"];
    assert_eq!(s["command"], json!("/opt/godterm/godterm"));
    assert_eq!(s["args"], json!(["mcp"]));
    assert_eq!(s["env"]["GODTERM_CLIENT"], json!("tab"));
    assert_eq!(s["env"]["GODTERM_TAB"], json!("7"));
    assert_eq!(s["env"]["GODTERM_TOKEN"], json!(token));

    std::env::remove_var("OPENCODE_CONFIG");
    let mut args = vec!["--auto".to_string()];
    let x = crate::tab_jobs::extend_launch_for(
        Harness::OpenCode,
        exe,
        shared,
        7,
        &mut args,
        &slot,
        true,
    )
    .unwrap();
    assert_eq!(args, ["--standalone", "--auto"]);
    assert_eq!(x.env.len(), 1);
    assert_eq!(x.env[0].0, "OPENCODE_CONFIG");
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&x.env[0].1).unwrap()).unwrap();
    assert_eq!(cfg["default_agent"], json!("plan"));
    let srv = &cfg["mcp"]["servers"]["godterm"];
    assert_eq!(srv["type"], json!("local"));
    assert_eq!(srv["command"], json!(["/opt/godterm/godterm", "mcp"]));
    assert_eq!(srv["environment"]["GODTERM_TAB"], json!("7"));

    let mut a2 = vec![];
    let x = crate::tab_jobs::extend_launch_for(
        Harness::OpenCode,
        exe,
        shared,
        7,
        &mut a2,
        &slot,
        false,
    )
    .unwrap();
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&x.env[0].1).unwrap()).unwrap();
    assert!(cfg.get("default_agent").is_none());

    std::env::set_var("OPENCODE_CONFIG", "/somewhere/mine.json");
    let mut a3 = vec![];
    let x =
        crate::tab_jobs::extend_launch_for(Harness::OpenCode, exe, shared, 7, &mut a3, &slot, true)
            .unwrap();
    std::env::remove_var("OPENCODE_CONFIG");
    assert!(a3.is_empty() && x.env.is_empty());
    let _ = std::fs::remove_dir_all(home);

    let h = Harness::Antigravity;
    assert!(!h.has_tab_tools());
    let reason = h.no_tab_tools_reason().unwrap();
    assert!(reason.contains(h.label()) && reason.contains("global mcp_config.json"));
    let mut args = vec!["--keep".to_string()];
    let x = crate::tab_jobs::extend_launch(h, exe, "shared", 7, &mut args).unwrap();
    assert!(x.env.is_empty());
    assert_eq!(args, ["--keep"]);
    let (mut app, t, home) = native_app("tt-agy", h);
    let dir = home.join("slot-home");
    std::fs::create_dir_all(&dir).unwrap();
    let mut a2 = vec![];
    assert!(app.tab_jobs_launch(h, 7, &dir, &mut a2, false).is_empty());
    assert!(a2.is_empty());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    let uid = app.panes[0].tabs[t].uid;
    for tool in ["list_jobs", "create_job", "list_browsers", "browser_status"] {
        let e = app.tab_scoped(uid, tool, &json!({})).unwrap_err();
        assert!(
            e.contains(h.label()) && e.contains("no GodTerm job or browser tools"),
            "{e}"
        );
    }

    app.open_browser_dialog(0);
    assert!(matches!(app.modal, crate::app::Modal::None));
    assert!(app
        .flash
        .as_ref()
        .unwrap()
        .0
        .contains("no GodTerm job or browser tools"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_launch_carries_the_tab_tools_of_codex_cursor_and_opencode() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("OPENCODE_CONFIG");
    for h in [Harness::Codex, Harness::Cursor, Harness::OpenCode] {
        let (app, t, home) = native_app(&format!("launch-{}", h.name()), h);
        let dir = home.join("slot-home");
        std::fs::create_dir_all(&dir).unwrap();
        let uid = app.panes[0].tabs[t].uid;
        let mut args = vec![];
        let env = app.tab_jobs_launch(h, uid, &dir, &mut args, false);
        assert!(!args.is_empty(), "{} got no flags", h.name());
        match h {
            Harness::Codex => {
                assert_eq!(env.len(), 2);
                assert!(env.iter().any(|(k, _)| k == "GODTERM_TOKEN"));
            }
            Harness::OpenCode => assert_eq!(args[0], "--standalone"),
            _ => assert_eq!(args[0], "--plugin-dir"),
        }

        let mut app = app;
        app.cfg.tabs.jobs_tools = false;
        let mut none = vec![];
        assert!(app
            .tab_jobs_launch(h, uid, &dir, &mut none, false)
            .is_empty());
        assert!(none.is_empty());

        let mut plan = vec![];
        let env = app.tab_jobs_launch(h, uid, &dir, &mut plan, true);
        if h == Harness::OpenCode {
            assert_eq!(plan, ["--standalone"]);
            let cfg: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&env[0].1).unwrap()).unwrap();
            assert_eq!(cfg["default_agent"], json!("plan"));
            assert!(cfg.get("mcp").is_none());
        } else {
            assert!(plan.is_empty() && env.is_empty());
        }
        let _ = std::fs::remove_dir_all(home);
    }
}

#[test]
fn the_bar_of_a_cli_without_a_model_picker_keeps_the_loops_and_browser_buttons() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for h in NEW {
        assert!(!h.has_model_picker());
        let (mut app, t, home) = native_app(&format!("bar-{}", h.name()), h);
        assert!(app.bar_models(0).is_empty());
        assert!(app.model_bar_on(0, 30), "{} has the bar", h.name());

        assert_eq!(crate::app_tabmodel::screen_model(h, "gpt-5 · high"), None);
        assert_eq!(crate::app_tabmodel::screen_effort(h, "gpt-5 (high)"), None);

        let mut term = crate::confirm_click_tests::term();
        crate::confirm_click_tests::draw(&mut app, &mut term);
        let regions = &app.last_hits.regions;
        assert!(regions
            .iter()
            .any(|r| r.action == crate::hits::UiAction::TabBrowser(0)));
        assert!(regions
            .iter()
            .any(|r| r.action == crate::hits::UiAction::LoopsDrawer(0)));
        assert!(!regions
            .iter()
            .any(|r| matches!(r.action, crate::hits::UiAction::TabModel(0, _))));

        if let Some(why) = h.no_tab_tools_reason() {
            let r = regions
                .iter()
                .find(|r| r.action == crate::hits::UiAction::TabBrowser(0))
                .unwrap();
            assert!(r.hint.contains(&why), "{}", r.hint);
        }

        let uid = app.panes[0].tabs[t].uid;
        let v = app.control_call(
            "set_tab_model",
            &json!({"tab": crate::control::tab_id(uid), "model": "x"}),
        );
        let e = v.to_string();
        assert_eq!(v["ok"], json!(false), "{v}");
        assert!(e.contains(h.label()) && e.contains("inside the CLI"), "{e}");
        app.focus = 0;
        app.start_model_entry();
        assert!(app.flash.as_ref().unwrap().0.contains("inside the CLI"));
        let _ = std::fs::remove_dir_all(home);
    }

    let (app, _t, home) = native_app("bar-claude", Harness::Claude);
    assert!(Harness::Claude.has_model_picker() && !app.bar_models(0).is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn native_tabs_are_not_moved_copied_or_archived_blind() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for h in NEW {
        let (mut app, t, home) = native_app(&format!("move-{}", h.name()), h);

        app.cfg.accounts[1].harness = h.name().into();
        app.accounts[1].login = crate::app::snapshot_of(&app.cfg.accounts[1], false).login;
        let targets = app.move_targets(0);
        assert!(!targets.is_empty());
        assert!(targets.iter().all(|m| m
            .disabled
            .as_deref()
            .is_some_and(|w| w.contains("cannot be moved"))));
        let before: usize = app.panes.iter().map(|p| p.tabs.len()).sum();
        app.open_move_picker(0, t, false);
        assert!(matches!(app.modal, crate::app::Modal::None));
        app.move_tab_now(0, t, 1, false, true);
        assert!(app.flash.as_ref().unwrap().0.contains("cannot be moved"));
        let after: usize = app.panes.iter().map(|p| p.tabs.len()).sum();
        assert_eq!(before, after, "no tab opened or closed");

        app.panes[0].tabs[t].session_id = None;
        app.panes[0].tabs[t].state = PaneState::Running;
        let blocks = app.archive_blockers(0, t);
        assert!(
            blocks
                .iter()
                .any(|b| b.hard && b.why.contains("session id")),
            "{blocks:?}"
        );
        assert!(app.archive_tab(0, t, "test", true).is_err());
        assert_eq!(
            before,
            app.panes.iter().map(|p| p.tabs.len()).sum::<usize>()
        );

        app.panes[0].tabs[t].session_id = Some("sess-1".into());
        assert!(app.archive_blockers(0, t).iter().all(|b| !b.hard));
        let _ = std::fs::remove_dir_all(home);
    }
}

#[test]
fn native_approvals_are_only_answered_with_keys_read_from_their_prompt() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for h in NEW {
        let (mut app, t, home) = native_app(&format!("ans-{}", h.name()), h);
        app.panes[0].tabs[t].activity = Activity::Permission;

        let say = app.answer_tab(0, t, crate::prompt::Choice::Approve);
        assert!(say.contains(h.label()) && say.contains("keyboard"), "{say}");
        let _ = std::fs::remove_dir_all(home);
    }

    let (mut app, t, home) = native_app("ans-claude", Harness::Claude);
    app.panes[0].tabs[t].activity = Activity::Permission;
    let say = app.answer_tab(0, t, crate::prompt::Choice::Approve);
    assert!(!say.contains("keyboard"), "{say}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn web_approvals_offer_only_the_answers_a_prompt_can_take() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = |n: &str| {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/screens/{n}.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
        .replace('\n', "\r\n")
    };
    let entry = |app: &crate::app::App, uid: u64| {
        app.web_approvals()
            .into_iter()
            .find(|v| v["id"] == json!(format!("tab:{uid}")))
            .unwrap()
    };
    for h in NEW {
        let (mut app, t, home) = native_app(&format!("webans-{}", h.name()), h);
        app.panes[0].tabs[t].activity = Activity::Permission;
        let uid = app.panes[0].tabs[t].uid;

        let v = entry(&app, uid);
        assert_eq!(v["choices"], json!([]), "{v}");
        let e = app.web_answer(&format!("tab:{uid}"), "yes").unwrap_err();
        assert!(e.contains("keyboard"), "{e}");
        let _ = std::fs::remove_dir_all(home);
    }

    let (mut app, t, home) = native_app("webans-agy", Harness::Antigravity);
    app.panes[0].tabs[t].activity = Activity::Permission;
    let uid = app.panes[0].tabs[t].uid;
    app.panes[0].tabs[t]
        .parser
        .lock()
        .unwrap()
        .process(fixture("antigravity_trust_narrow").as_bytes());
    assert_eq!(entry(&app, uid)["choices"], json!(["yes", "no"]));
    assert!(app.web_answer(&format!("tab:{uid}"), "always").is_err());
    let _ = std::fs::remove_dir_all(home);

    let (mut app, t, home) = native_app("webans-codex", Harness::Codex);
    app.panes[0].tabs[t].activity = Activity::Permission;
    let uid = app.panes[0].tabs[t].uid;
    app.panes[0].tabs[t]
        .parser
        .lock()
        .unwrap()
        .process(fixture("codex_approval").as_bytes());
    assert_eq!(entry(&app, uid)["choices"], json!(["yes", "always", "no"]));
    let _ = std::fs::remove_dir_all(home);

    let (mut app, t, home) = native_app("webans-claude", Harness::Claude);
    app.panes[0].tabs[t].activity = Activity::Permission;
    let uid = app.panes[0].tabs[t].uid;
    assert_eq!(entry(&app, uid)["choices"], json!(["yes", "always", "no"]));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_web_state_lists_each_accounts_tab_models() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let (app, _t, home) = native_app("webmodels-claude", Harness::Claude);
    let st = app.web_state();
    let labels: Vec<&str> = st["accounts"][0]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Haiku", "Sonnet", "Opus", "Fable"]);
    let _ = std::fs::remove_dir_all(home);

    for h in NEW {
        let (app, _t, home) = native_app(&format!("webmodels-{}", h.name()), h);
        assert_eq!(app.web_state()["accounts"][0]["models"], json!([]));
        let _ = std::fs::remove_dir_all(home);
    }
}

#[test]
fn the_web_state_gives_a_tabs_repo_in_numbers_for_the_status_bar() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, t, home) = native_app("webgit", Harness::Claude);
    let uid = app.panes[0].tabs[t].uid;
    let tab = |st: &serde_json::Value| {
        st["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["uid"] == json!(uid))
            .cloned()
            .unwrap()
    };
    let st = app.web_state();

    assert!(tab(&st).get("git_facts").is_none(), "{}", tab(&st));
    let repo = std::env::temp_dir().join(format!("godterm-webgit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    app.panes[0].tabs[t].cwd = repo.clone();
    let top = crate::project_git::repo_top(&repo).unwrap();
    app.project_index.lock().unwrap().git.insert(
        top.clone(),
        crate::project_git::Facts {
            top,
            branch: Some("main".into()),
            modified: 2,
            untracked: 1,
            ahead: Some(1),
            unpushed: Some(3),
            behind: Some(4),
            ..Default::default()
        },
    );
    let g = tab(&app.web_state())["git_facts"].clone();
    assert_eq!(
        g,
        json!({"branch": "main", "dirty": 3, "unpushed": 3, "behind": 4, "error": false})
    );
    let _ = std::fs::remove_dir_all(repo);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_move_tool_refuses_another_harness_and_the_native_clis() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let (mut app, t, home) = native_app("move-tool-claude", Harness::Claude);
    app.cfg.accounts[1].harness = "grok".into();
    let uid = app.panes[0].tabs[t].uid;
    let before: usize = app.panes.iter().map(|p| p.tabs.len()).sum();
    let v = app.control_call("move_tab", &json!({"tab": format!("t{uid}"), "to": 2}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(v.to_string().contains("moves only to another"), "{v}");
    assert!(app.queued_moves.is_empty());
    assert_eq!(
        before,
        app.panes.iter().map(|p| p.tabs.len()).sum::<usize>()
    );
    let _ = std::fs::remove_dir_all(home);

    for h in NEW {
        let (mut app, t, home) = native_app(&format!("move-tool-{}", h.name()), h);
        app.cfg.accounts[1].harness = h.name().into();
        app.panes[0].tabs[t].activity = Activity::Working;
        let uid = app.panes[0].tabs[t].uid;
        let v = app.control_call("move_tab", &json!({"tab": format!("t{uid}"), "to": 2}));
        assert_eq!(v["ok"], json!(false), "{v}");
        assert!(v.to_string().contains("cannot be moved"), "{v}");
        assert!(app.queued_moves.is_empty(), "{}", h.name());
        let _ = std::fs::remove_dir_all(home);
    }
}

#[test]
fn native_clis_do_not_inherit_the_global_bypass() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut cfg = Config::default();
    assert_eq!(cfg.permission_mode, "bypass");
    for h in NEW {
        cfg.accounts[0].harness = h.name().into();
        cfg.accounts[0].permission_mode = None;
        assert_eq!(cfg.mode_for(0), "default");
        assert!(h.permission_args(&cfg.mode_for(0)).is_empty());
        cfg.accounts[0].permission_mode = Some("bypass".into());
        assert_eq!(cfg.mode_for(0), "bypass");
    }
}

#[test]
fn a_quit_tab_keeps_the_session_its_farewell_names() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let farewell = |h: Harness| {
        let n = match h {
            Harness::Antigravity => "antigravity",
            h => h.name(),
        };
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/screens/{n}_exit.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    };
    for h in NEW {
        let (mut app, t, home) = native_app(&format!("exit-{}", h.name()), h);
        let tab = &mut app.panes[0].tabs[t];
        tab.session_id = None;
        tab.harness = h;

        tab.parser
            .lock()
            .unwrap()
            .process(farewell(h).replace('\n', "\r\n").as_bytes());
        app.learn_exit_session(0, t);
        let id = app.panes[0].tabs[t].session_id.clone().expect(h.name());
        assert!(Harness::safe_session_id(&id));
        assert!(!h.resume_args(&id).is_empty(), "{}", h.name());
        assert!(app.state_dirty);
        let resume = h.resume_args(&id);
        assert!(resume.iter().any(|a| a == &id));
        let _ = std::fs::remove_dir_all(home);
    }

    let (mut app, t, home) = native_app("exit-none", Harness::Codex);
    app.panes[0].tabs[t].session_id = None;
    app.panes[0].tabs[t].harness = Harness::Codex;
    app.learn_exit_session(0, t);
    assert_eq!(app.panes[0].tabs[t].session_id, None);
    let _ = std::fs::remove_dir_all(home);
}
