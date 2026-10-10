//! Native CLI launches use stand-ins only, never the user's real agents.

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
    assert_eq!(
        Harness::OpenCode.permission_args("plan"),
        ["--agent", "plan"]
    );
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
            ("env_to", env.display().to_string()),
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
            let expected = match &kind {
                LaunchKind::Login if h.login_args().is_some() => h.login_args().unwrap(),
                LaunchKind::Resume(id) => [h.permission_args("plan"), h.resume_args(id)].concat(),
                _ => h.permission_args("plan"),
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
    // A CLI-managed login is enough to start an idle slot automatically.
    let a = account(Harness::Codex);
    app.cfg.accounts[0] = a;
    app.accounts[0].login = crate::app::snapshot_of(&app.cfg.accounts[0], false).login;
    app.panes[0].cur_mut().state = crate::pane::PaneState::Idle;
    app.autostart();
    assert!(app.panes[0].cur().is_running());
    app.shutdown();
    let _ = std::fs::remove_dir_all(home);
}
