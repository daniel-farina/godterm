use std::path::{Path, PathBuf};

use crate::config::testing::LOCK as ENV_LOCK;
use crate::config::Config;

fn home(tag: &str) -> PathBuf {
    let h = std::env::temp_dir().join(format!("godterm-recover-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    crate::config::testing::set_home(&h);
    h
}

fn backups(dir: &Path, name: &str) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{name}.corrupt-"))
        })
        .count()
}

fn app(cfg: Config) -> crate::app::App {
    let (tx, _rx) = std::sync::mpsc::channel();
    crate::app::App::new(cfg, tx)
}

#[test]
fn damaged_files_are_moved_aside_and_the_start_is_clean() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("start");
    let mut cfg = Config::default();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.autostart = false;
    cfg.save().unwrap();
    std::fs::write(h.join("state.json"), "{\"panes\": [{\"tabs\": [").unwrap();
    std::fs::write(h.join("jobs.json"), "").unwrap();
    std::fs::write(h.join("archive.json"), "\u{0}\u{0}garbage").unwrap();
    crate::recover::take_notes();
    let cfg = Config::load_or_recover().unwrap();
    let mut a = app(cfg);
    a.autostart();
    assert!(a.jobs.jobs.is_empty());
    let notes = crate::recover::take_notes();
    for f in ["state.json", "jobs.json", "archive.json"] {
        assert!(!h.join(f).exists(), "{f} moved aside");
        assert_eq!(backups(&h, f), 1, "{f}");
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with(&format!("{f} was damaged"))),
            "{f}: {notes:?}"
        );
    }
    assert!(notes.iter().any(|n| n.contains("(empty)")), "{notes:?}");

    a.jobs.save().unwrap();
    assert!(crate::jobs::Store::load().jobs.is_empty());
    assert_eq!(backups(&h, "jobs.json"), 1, "a good file stays put");
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn a_config_that_does_not_parse_is_moved_aside_and_setup_comes_back() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("config");
    let mut cfg = Config::default();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.save().unwrap();
    let good = std::fs::read_to_string(h.join("config.toml")).unwrap();

    assert!(Config::load_or_recover().is_ok());
    assert_eq!(backups(&h, "config.toml"), 0);
    crate::recover::take_notes();
    for bad in [
        String::new(),
        "[[account]]\nname = \"a\"\n[[account]]\nname = \"a\"\n".to_string(),
        "claude_bin = \"/x\n".to_string(),
        "\u{0}\u{1}\u{2}".to_string(),
        "[voice\n".to_string(),
    ] {
        std::fs::write(h.join("config.toml"), &bad).unwrap();
        let c = Config::load_or_recover();
        if bad.is_empty() {
            assert!(c.is_ok());
            continue;
        }
        let c = c.unwrap_or_else(|e| panic!("{bad:?}: {e:#}"));
        assert_eq!(
            c.accounts.len(),
            Config::default().accounts.len(),
            "{bad:?}: a clean start"
        );
        assert!(!h.join("config.toml").exists(), "{bad:?}");
        let notes = crate::recover::take_notes();
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("config.toml was damaged")),
            "{bad:?}: {notes:?}"
        );
    }
    assert_eq!(backups(&h, "config.toml"), 4);

    let bytes = good.as_bytes();
    for cut in (0..bytes.len()).step_by(7) {
        std::fs::write(h.join("config.toml"), &bytes[..cut]).unwrap();
        assert!(Config::load_or_recover().is_ok(), "cut at {cut}");
    }
    crate::recover::take_notes();
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn saved_state_cut_at_any_byte_never_panics() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("cut");
    let mut cfg = Config::default();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.autostart = false;
    cfg.save().unwrap();
    let a = app(cfg);
    let st = crate::state::AppState {
        privacy: Some(true),
        ..Default::default()
    };
    st.save().unwrap();
    let full = std::fs::read(h.join("state.json")).unwrap();
    let mut store = crate::jobs::Store::default();
    store.jobs.push(crate::jobs_tests::job_for(
        &a,
        0,
        0,
        "nightly",
        "check the build",
        15,
    ));
    store.save().unwrap();
    let jobs = std::fs::read(h.join("jobs.json")).unwrap();
    for cut in 0..full.len() {
        std::fs::write(h.join("state.json"), &full[..cut]).unwrap();
        let _ = crate::state::AppState::load();
    }
    for cut in 0..jobs.len() {
        std::fs::write(h.join("jobs.json"), &jobs[..cut]).unwrap();
        let _ = crate::jobs::Store::load();
    }

    std::fs::write(h.join("state.json"), &full).unwrap();
    assert_eq!(crate::state::AppState::load().unwrap().privacy, Some(true));
    std::fs::write(h.join("jobs.json"), &jobs).unwrap();
    assert_eq!(crate::jobs::Store::load().jobs.len(), 1);
    crate::recover::take_notes();
    let _ = std::fs::remove_dir_all(&h);
}
