//! Local voices: config defaults, the engine chain and its fallback,
//! install consent, Settings, and the voice phrases. Nothing here starts
//! a real model, downloads anything, or touches ~/.godterm or the real
//! Hugging Face cache (the dirs point at a temp folder).

use crate::config::testing::LOCK as ENV_LOCK;
use crate::config::{Config, VoiceCfg};
use crate::settings::{self, Key, Section};
use crate::voice::{engines, local_tts, tts};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    let home = std::env::temp_dir().join(format!("godterm-lv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    (app, home)
}

/// Pretend `id` is installed (a marker and an empty venv python).
fn fake_install(id: &str, vc: &VoiceCfg) {
    let py = local_tts::venv_python(&vc.local);
    std::fs::create_dir_all(py.parent().unwrap()).unwrap();
    std::fs::write(&py, "").unwrap();
    std::fs::write(local_tts::marker(id), "{}").unwrap();
}

fn screen(app: &mut crate::app::App, w: u16) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(w, 60)).unwrap();
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

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

#[test]
fn kokoro_stays_the_default_and_local_parses() {
    let d = VoiceCfg::default();
    assert_eq!(d.tts_engine, "kokoro");
    assert_eq!(d.kokoro_voice, "af_heart");
    assert_eq!(d.tts_fallback, vec!["kokoro".to_string(), "say".into()]);
    assert_eq!(d.local.ming_voice, "US_Emma");
    assert!(d.local.chatterbox_ref.is_empty() && d.local.venv.is_empty());
    // An old config without [voice.local] still loads, with the defaults.
    let c: Config = toml::from_str("[voice]\ntts_engine = \"grok\"\n").unwrap();
    assert_eq!(c.voice.tts_engine, "grok");
    assert_eq!(c.voice.local, d.local);
    let c: Config = toml::from_str(
        "[voice]\ntts_engine = \"chatterbox\"\n[voice.local]\nchatterbox_ref = \"~/me.wav\"\nming_voice = \"UK_George\"\n",
    )
    .unwrap();
    assert_eq!(c.voice.tts_engine, "chatterbox");
    assert_eq!(c.voice.local.chatterbox_ref, "~/me.wav");
    assert_eq!(c.voice.local.ming_voice, "UK_George");
    assert_eq!(c.voice.local.timeout_ms, 8000);
    // A voice or clip change is a talk back change only (the mic keeps running).
    let mut b = d.clone();
    b.local.chatterbox_ref = "x.wav".into();
    b.tts_engine = "ming".into();
    assert!(tts::only_tts_changed(&d, &b));
}

#[test]
fn chain_skips_a_local_voice_until_installed_and_falls_back() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("godterm-lv-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let mut c = VoiceCfg {
        tts_engine: "chatterbox".into(),
        kokoro_model: "/nonexistent/kokoro.onnx".into(),
        ..Default::default()
    };
    // Not installed: what the fallback list says (Kokoro is missing here too).
    assert_eq!(engines::chain(&c), vec!["say"]);
    let (e, why) = tts::effective_engine(&c);
    assert_eq!(e, "say");
    assert!(why.unwrap().contains("chatterbox"));
    std::fs::create_dir_all(local_tts::voices_dir()).unwrap();
    fake_install("chatterbox", &c);
    if local_tts::supported() {
        assert_eq!(engines::chain(&c), vec!["chatterbox", "say"]);
        assert_eq!(tts::effective_engine(&c), ("chatterbox", None));
        // Ming in the fallbacks, not installed: passed over.
        c.tts_fallback = vec!["ming".into(), "kokoro".into(), "say".into()];
        assert_eq!(engines::chain(&c), vec!["chatterbox", "say"]);
        fake_install("ming", &c);
        assert_eq!(engines::chain(&c), vec!["chatterbox", "ming", "say"]);
    } else {
        // Elsewhere they never speak.
        assert_eq!(engines::chain(&c), vec!["say"]);
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn install_waits_for_a_yes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("consent");
    if !local_tts::supported() {
        let rows = settings::settings_for(Section::Voice, &app.cfg);
        assert!(rows
            .iter()
            .all(|r| settings::local_install_row(&r.key).is_none()));
        let _ = std::fs::remove_dir_all(&home);
        return;
    }
    local_tts::STARTED.lock().unwrap().clear();
    app.view = crate::app::View::Settings;
    app.settings_section = settings::SECTIONS
        .iter()
        .position(|s| s.0 == Section::Voice)
        .unwrap();
    let rows = app.settings_rows();
    let row = rows
        .iter()
        .position(|r| r.key == Key::Global("voice.local_install.chatterbox"))
        .expect("the install row");
    app.settings_sel = row;
    let t = screen(&mut app, 200);
    assert!(t.contains("Chatterbox Turbo: install"), "{t}");
    assert!(t.contains("not installed") && t.contains("Install…"), "{t}");
    // Enter asks first, with the size, memory, speed and license.
    app.setting_edit(row);
    assert_eq!(
        app.modal,
        crate::app::Modal::ConfirmLocalVoice("chatterbox", false)
    );
    let t = screen(&mut app, 200);
    assert!(
        t.contains("2.9 GB") && t.contains("MIT") && t.contains("0.2 s"),
        "{t}"
    );
    assert!(
        local_tts::STARTED.lock().unwrap().is_empty(),
        "nothing before the yes"
    );
    // No: nothing happens.
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.modal, crate::app::Modal::None);
    assert!(local_tts::STARTED.lock().unwrap().is_empty());
    // Yes: it starts.
    app.setting_step(row, 1);
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(
        *local_tts::STARTED.lock().unwrap(),
        vec!["chatterbox".to_string()]
    );
    // Installed: Enter offers Remove (asks too), and removes on yes.
    fake_install("chatterbox", &app.cfg.voice);
    let t = screen(&mut app, 200);
    assert!(t.contains("installed") && t.contains("Remove…"), "{t}");
    app.setting_step(row, 1);
    assert_eq!(
        app.modal,
        crate::app::Modal::ConfirmLocalVoice("chatterbox", true)
    );
    app.on_key(key(KeyCode::Char('y')));
    assert!(!local_tts::installed("chatterbox", &app.cfg.voice.local));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn settings_show_engines_and_kokoro_voices() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("rows");
    *settings::AUDIO_LISTS.lock().unwrap() = None;
    let rows = settings::settings_for(Section::Voice, &app.cfg);
    let find = |rows: &[settings::Setting], k: &'static str| {
        rows.iter().find(|r| r.key == Key::Global(k)).cloned()
    };
    let eng = find(&rows, "voice.tts_engine").unwrap();
    assert_eq!(
        eng.kind,
        settings::Kind::Choice(crate::voice::catalog::engine_choices())
    );
    assert_eq!(
        settings::describe(&app.cfg, &eng).unwrap(),
        crate::voice::catalog::engine_line("kokoro").unwrap()
    );
    let v = find(&rows, "voice.kokoro_voice").unwrap();
    match &v.kind {
        settings::Kind::Pick(l) => assert_eq!(l.len(), 28),
        k => panic!("{k:?}"),
    }
    assert_eq!(
        settings::describe(&app.cfg, &v).unwrap(),
        "American female, grade A"
    );
    app.view = crate::app::View::Settings;
    app.settings_section = settings::SECTIONS
        .iter()
        .position(|s| s.0 == Section::Voice)
        .unwrap();
    app.settings_sel = app
        .settings_rows()
        .iter()
        .position(|r| r.key == Key::Global("voice.kokoro_voice"))
        .unwrap();
    let t = screen(&mut app, 220);
    assert!(
        t.contains("af_heart") && t.contains("American female, grade A"),
        "{t}"
    );
    assert!(t.contains("Preview"), "{t}");
    // Chatterbox and Ming panels only with that engine.
    let labels = |app: &crate::app::App| -> Vec<String> {
        settings::settings_for(Section::Voice, &app.cfg)
            .into_iter()
            .map(|r| r.label)
            .collect()
    };
    assert!(!labels(&app)
        .iter()
        .any(|l| l.starts_with("Chatterbox:") || l.starts_with("Ming:")));
    app.cfg.voice.tts_engine = "chatterbox".into();
    let l = labels(&app);
    assert!(l.iter().any(|x| x == "Chatterbox: voice clip"), "{l:?}");
    assert!(
        l.iter().any(|x| x == "Speed") && l.iter().any(|x| x == "Local voice: unload after (s)")
    );
    assert!(!l.iter().any(|x| x == "Kokoro: voice"));
    app.cfg.voice.tts_engine = "ming".into();
    assert!(labels(&app).iter().any(|x| x == "Ming: voice"));
    assert!(!labels(&app).iter().any(|x| x == "Ming: voice description"));
    app.cfg.voice.local.ming_voice = "design".into();
    assert!(labels(&app).iter().any(|x| x == "Ming: voice description"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn spoken_voice_choices_switch_and_explain() {
    use crate::app_voices::VoicePick;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("phrases");
    std::fs::write(Config::path(), "[voice]\ntts_engine = \"grok\"\n").unwrap();
    app.cfg.voice.tts_engine = "grok".into();
    // "use the Bella voice": Kokoro with Bella, saved.
    assert!(app.voice_choice_intercept("use the Bella voice"));
    assert_eq!(app.cfg.voice.tts_engine, "kokoro");
    assert_eq!(app.cfg.voice.kokoro_voice, "af_bella");
    let saved = std::fs::read_to_string(Config::path()).unwrap();
    assert!(
        saved.contains("af_bella") && saved.contains("kokoro"),
        "{saved}"
    );
    assert!(app.voice.action.as_deref().unwrap().contains("Bella"));
    // Grok again, then "use the local voice": back to Kokoro.
    assert!(app.voice_choice_intercept("use the grok voice"));
    assert_eq!(app.cfg.voice.tts_engine, "grok");
    assert!(app.voice_choice_intercept("use the local voice"));
    assert_eq!(app.cfg.voice.tts_engine, "kokoro");
    // Chatterbox, not installed: it says so and nothing changes.
    let say = app.apply_voice_pick(&VoicePick::Engine("chatterbox"));
    assert!(
        say.contains("not installed") || say.contains("Apple Silicon"),
        "{say}"
    );
    assert_eq!(app.cfg.voice.tts_engine, "kokoro");
    if local_tts::supported() {
        fake_install("chatterbox", &app.cfg.voice);
        assert!(app.voice_choice_intercept("use chatterbox"));
        assert_eq!(app.cfg.voice.tts_engine, "chatterbox");
    }
    // Other speech is not a voice choice.
    assert!(!app.voice_choice_intercept("use the tests in the project"));
    let _ = std::fs::remove_dir_all(&home);
}
