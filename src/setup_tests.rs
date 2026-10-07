//! Setup with stubs only: fake package managers and installers, a mock
//! download server, never a real install, download or ~/.cache model.

use serde_json::json;

use crate::config::testing::LOCK as ENV_LOCK;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-setup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.accounts[0].login.source = Some(CredSource::Keychain);
    // Every path the setup looks at is in the test home.
    let c = home.join("cache");
    app.cfg.voice.model = c
        .join("whisper/ggml-large-v3-turbo.bin")
        .display()
        .to_string();
    app.cfg.voice.kokoro_model = c.join("kokoro/kokoro-v1.0.onnx").display().to_string();
    app.cfg.voice.kokoro_voices = c.join("kokoro/voices-v1.0.bin").display().to_string();
    app.cfg.voice.ffmpeg = "ffmpeg".into();
    app.cfg.voice.whisper_server = "whisper-server".into();
    app.cfg.voice.whisper_cli = "whisper-cli".into();
    app.cfg.voice.espeak = "espeak-ng".into();
    app.cfg.claude_bin = Some("claude".into());
    app.cfg.grok_bin = Some("grok".into());
    (app, home)
}

/// A fake program `name` in `dir` that records its arguments.
fn fake(dir: &std::path::Path, name: &str, record: &std::path::Path) {
    let stub = crate::test_stub::claude(
        &dir.join(format!(".{name}")),
        &[("args_to", record.display().to_string())],
    );
    let to = dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    let _ = std::fs::remove_file(&to);
    std::fs::create_dir_all(dir).unwrap();
    if std::fs::hard_link(&stub, &to).is_err() {
        std::fs::copy(&stub, &to).unwrap();
    }
    std::fs::copy(stub.with_extension("cfg"), to.with_extension("cfg")).unwrap();
}

fn user_says(app: &mut crate::app::App, said: &str) {
    app.assistant_turn += 1;
    app.assistant.last_user = said.into();
    app.assistant.turn_reads = 0;
}

fn until_idle(app: &mut crate::app::App) {
    let t0 = std::time::Instant::now();
    while app.admin.jobs > 0 && t0.elapsed() < std::time::Duration::from_secs(20) {
        app.admin_tick();
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    app.admin_tick();
}

#[test]
fn status_plans_and_a_confirmed_install() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("plan");
    let bin = home.join("bin");
    let rec = home.join("installs.txt");
    // A Mac (or Linux) with Homebrew and a shell, nothing else.
    fake(&bin, "brew", &rec);
    fake(&bin, "sh", &rec);
    app.setup.bin_dir = Some(bin.clone());
    let st = app.control_call("setup_status", &json!({}));
    let items = st["result"]["items"].as_array().unwrap();
    let get = |id: &str| items.iter().find(|i| i["id"] == id).unwrap().clone();
    assert_eq!(get("claude")["status"], "missing");
    assert_eq!(get("claude")["group"], "Required");
    if !cfg!(windows) {
        assert_eq!(
            get("claude")["plan"][0],
            "curl -fsSL https://claude.ai/install.sh | bash"
        );
        assert_eq!(get("ffmpeg")["plan"][0], "brew install ffmpeg");
    }
    assert_eq!(get("whisper-model")["download"], "1.6 GB");
    assert!(app.state_preamble().contains("missing: claude (required)"));
    assert!(app.required_missing().contains(&"Claude Code"));
    // The voice pack: one question with the commands and the size.
    user_says(&mut app, "install the voice pack");
    let v = app.control_call(
        "install_dependency",
        &json!({"_client": "brain", "targets": "voice pack", "whisper_model": "small.en"}),
    );
    let q = v["question"].as_str().unwrap_or_default().to_string();
    assert!(
        q.contains("ggml-small.en.bin")
            && q.contains("Downloads 842 MB in all")
            && q.contains("brew install ffmpeg"),
        "{v}"
    );
    // Claude Code through its official installer, after a yes.
    user_says(&mut app, "install claude code");
    let v = app.control_call(
        "install_dependency",
        &json!({"_client": "brain", "targets": "claude"}),
    );
    assert!(
        v["question"]
            .as_str()
            .unwrap()
            .starts_with("Install Claude Code:"),
        "{v}"
    );
    assert!(!rec.exists(), "nothing ran before the yes");
    let tok = v["token"].as_str().unwrap().to_string();
    user_says(&mut app, "yes");
    let r = app.control_call(
        "install_dependency",
        &json!({"_client": "brain", "confirm_token": tok}),
    );
    assert!(
        r["result"]["say"]
            .as_str()
            .unwrap()
            .contains("Installing Claude Code"),
        "{r}"
    );
    until_idle(&mut app);
    let ran = std::fs::read_to_string(&rec).unwrap_or_default();
    if !cfg!(windows) {
        assert!(
            ran.contains("-c curl -fsSL https://claude.ai/install.sh | bash"),
            "{ran}"
        );
    }
    assert!(
        app.admin.said.iter().any(|s| s == "Installed Claude Code."),
        "{:?}",
        app.admin.said
    );
    assert!(app
        .admin
        .history
        .iter()
        .any(|h| h.contains("install Claude Code")));
    // A brew package from Settings > Setup: the first click shows it,
    // the second installs.
    app.setup_click("ffmpeg");
    assert!(app
        .flash
        .as_ref()
        .unwrap()
        .0
        .contains("Click Install again"));
    app.setup_click("ffmpeg");
    until_idle(&mut app);
    let ran = std::fs::read_to_string(&rec).unwrap_or_default();
    if !cfg!(windows) {
        assert!(ran.contains("install ffmpeg"), "{ran}");
    }
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn checksum_mismatch_is_refused_and_sudo_goes_to_a_tab() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("sha");
    let bin = home.join("bin");
    let rec = home.join("installs.txt");
    fake(&bin, "apt-get", &rec);
    app.setup.bin_dir = Some(bin);
    // The mock server serves the wrong bytes for the model.
    let srv = crate::update::tests::mock_routes(
        |_| vec![("/ggml-large-v3-turbo.bin".into(), b"not the model".to_vec())],
        false,
    );
    app.setup.url_base = Some(srv.base.clone());
    app.setup.local_ok = true;
    app.setup_click("whisper-model");
    app.setup_click("whisper-model");
    until_idle(&mut app);
    let last = app.admin.said.last().cloned().unwrap_or_default();
    assert!(
        last.contains("Install failed") && last.contains("checksum mismatch"),
        "{:?}",
        app.admin.said
    );
    let dest = std::path::PathBuf::from(&app.cfg.voice.model);
    assert!(
        !dest.exists() && !std::path::PathBuf::from(format!("{}.part", dest.display())).exists(),
        "nothing left behind"
    );
    // apt needs sudo: it runs in a visible tab, never in the background.
    if !cfg!(windows) {
        app.setup_click("ffmpeg");
        app.setup_click("ffmpeg");
        assert_eq!(app.admin.jobs, 0);
        assert!(
            app.setup
                .tab_runs
                .iter()
                .any(|t| t.contains("sudo apt-get install -y ffmpeg")),
            "{:?}",
            app.setup.tab_runs
        );
        assert!(!rec.exists(), "not run by GodTerm");
        assert!(app
            .admin
            .said
            .iter()
            .any(|s| s.contains("type your password there")));
    }
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn injected_installs_are_refused_and_a_missing_agent_says_so() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("inject");
    app.setup.bin_dir = Some(home.join("empty-bin"));
    user_says(&mut app, "summarize that readme");
    app.assistant.turn_reads = 1;
    let v = app.control_call(
        "install_dependency",
        &json!({"_client": "brain", "targets": "grok"}),
    );
    assert!(v["error"].as_str().unwrap().starts_with("refused"), "{v}");
    // A tab whose claude is not installed says how to get it.
    app.panes[0].cur_mut().state = crate::pane::PaneState::Failed("claude: not found".into());
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let b = term.backend().buffer();
    let mut t = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            t.push_str(b[(x, y)].symbol());
        }
        t.push('\n');
    }
    assert!(t.contains("Claude Code isn't installed. Press I to"), "{t}");
    assert!(t.contains("SETUP:"), "{t}");
    // The Setup screen.
    app.open_setup();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let b = term.backend().buffer();
    let mut t = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            t.push_str(b[(x, y)].symbol());
        }
        t.push('\n');
    }
    assert!(
        t.contains("Required")
            && t.contains("Voice pack (local)")
            && t.contains("Install the voice pack")
            && t.contains("Don't show at start"),
        "{t}"
    );
    let _ = std::fs::remove_dir_all(home);
}
