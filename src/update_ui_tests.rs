//! The new version chip and the Updates window, against a fake release
//! feed on 127.0.0.1 (no real network): the chip, its still form with
//! reduced motion, every version's notes newest first, Download then
//! Restart (which asks), Homebrew's command, Later, a narrow terminal.

use crate::app_update::Phase;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;
use crate::update::{self, tests as fake};

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-updui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    (app, home)
}

/// The feed: the latest (9.9.9, the usual fake release) and the list with
/// 9.9.8 between, plus one older than this build.
fn feed(base: &str, empty_998: bool) -> Vec<(String, Vec<u8>)> {
    let mut r = fake::routes(base, "v9.9.9", true, fake::SUMS);
    let latest: serde_json::Value =
        serde_json::from_slice(&fake::release_json(base, "v9.9.9", true)).unwrap();
    let mid = serde_json::json!({
        "tag_name": "v9.9.8", "html_url": format!("{base}/rel/v9.9.8"), "prerelease": false,
        "draft": false, "published_at": "2099-01-02T10:00:00Z",
        "body": if empty_998 { "" } else { "## Changes\n- A **calmer** panel\n  - with [links](https://x.y) gone" },
        "assets": [{"name": "CHANGELOG.md", "browser_download_url": format!("{base}/dl/CHANGELOG.md"), "size": 10}],
    });
    let old = serde_json::json!({"tag_name": "v0.0.1", "html_url": "", "draft": false, "body": "ancient", "assets": []});
    r.push((
        format!("/repos/{}/releases?per_page=30", update::REPO),
        serde_json::to_vec(&serde_json::json!([latest, mid, old])).unwrap(),
    ));
    r.push((
        "/dl/CHANGELOG.md".into(),
        b"# Changelog\n\n## [9.9.9] - 2099-01-03\n- latest\n\n## [9.9.8] - 2099-01-02\n- From the changelog file\n\n## [9.9.7]\n- older\n".to_vec(),
    ));
    r
}

/// A check against the feed, as the background checker runs it.
fn check(app: &mut crate::app::App, method: update::Method, force: bool, empty_998: bool) {
    let srv = fake::mock_routes(|b| feed(b, empty_998), false);
    let src = fake::test_source(&srv.base);
    let mut cfg = app.cfg.update.clone();
    cfg.auto_download = false;
    let l = update::Layout::default();
    crate::app_update::run_once_with(&app.update.shared, &cfg, &src, &l, method, force);
}

fn standalone(home: &std::path::Path) -> update::Method {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let target = bin.join("godterm");
    std::fs::write(&target, "old build").unwrap();
    update::Method::Standalone { path: target }
}

fn screen(
    app: &mut crate::app::App,
    w: u16,
) -> (String, ratatui::Terminal<ratatui::backend::TestBackend>) {
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 45)).unwrap();
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
    let b = t.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            s.push_str(b[(x, y)].symbol());
        }
        s.push('\n');
    }
    (s, t)
}

#[test]
fn a_newer_release_shows_the_chip_and_opens_the_window() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("chip");
    assert!(app.update_badge().is_none());
    let m = standalone(&home);
    check(&mut app, m, false, false);
    assert_eq!(app.update.snapshot().phase, Phase::Available);
    assert_eq!(app.update_badge().as_deref(), Some("↑ v9.9.9"));
    let (s, _) = screen(&mut app, 200);
    let top = s.lines().next().unwrap();
    assert!(top.contains("↑ v9.9.9"), "{top}");
    assert!(app.state_preamble().contains("update_available: v9.9.9"));
    // A click opens the Updates window.
    let r = app.last_hits.find(&UiAction::OpenUpdates).unwrap().rect;
    app.on_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: r.x + 1,
        row: r.y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert_eq!(app.modal, crate::app::Modal::Updates(0));
    // And Ctrl-a D.
    app.modal = crate::app::Modal::None;
    app.on_command(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('D'),
        crossterm::event::KeyModifiers::SHIFT,
    ));
    assert_eq!(app.modal, crate::app::Modal::Updates(0));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_chip_shimmers_calmly_or_stands_still() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use crate::ui_updates::chip_bg;
    let (mut app, home) = test_app("motion");
    let m = standalone(&home);
    check(&mut app, m, false, false);
    // Moving: the band sweeps (one cycle per 2 s, redrawn 10 times a second).
    app.cfg.splash_motion = "on".into();
    assert!(crate::ui_updates::motion(&app));
    assert_eq!(
        app.anim_interval(),
        Some(std::time::Duration::from_millis(100))
    );
    assert_ne!(chip_bg(3, 12, Some(0.2)), chip_bg(3, 12, Some(0.8)));
    // Warm and muted: every tone stays between sand, sage and a pale sand.
    for i in 0..12 {
        for ph in [0.0, 0.25, 0.5, 0.75] {
            let (r, g, b) = chip_bg(i, 12, Some(ph));
            assert!(
                r >= 130 && g >= 150 && b >= 120 && r <= 230 && b <= 190,
                "({r},{g},{b})"
            );
        }
    }
    // Reduced motion: a still gradient, no redraws for it.
    app.cfg.splash_motion = "off".into();
    assert!(!crate::ui_updates::motion(&app));
    assert_eq!(crate::ui_updates::phase(&app), None);
    assert_eq!(app.anim_interval(), None);
    let (_, t1) = screen(&mut app, 200);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (_, t2) = screen(&mut app, 200);
    let r = app.last_hits.find(&UiAction::OpenUpdates).unwrap().rect;
    for x in r.x..r.x + r.width {
        assert_eq!(
            t1.backend().buffer()[(x, r.y)].bg,
            t2.backend().buffer()[(x, r.y)].bg
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_window_lists_every_newer_version_newest_first() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("notes");
    let m = standalone(&home);
    check(&mut app, m, false, false);
    let (lines, buttons) = app.updates_view(80);
    let text: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
    let at = |needle: &str| {
        text.iter()
            .position(|t| t.contains(needle))
            .unwrap_or_else(|| panic!("{needle} not in {text:#?}"))
    };
    assert!(text[0].contains("The latest is v9.9.9"));
    assert!(at("v9.9.9") < at("v9.9.8"), "newest first");
    assert!(at("• Faster restarts") > at("v9.9.9"));
    assert!(at("• A calmer panel") > at("v9.9.8"));
    assert!(
        text.iter().any(|t| t.contains("• with links gone")),
        "{text:#?}"
    );
    assert!(
        !text.iter().any(|t| t.contains("ancient")),
        "older than this build"
    );
    assert!(!text.iter().any(|t| t.contains("**") || t.contains("](")));
    let labels: Vec<&str> = buttons.iter().map(|(l, _, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["Download", "Release page", "Later"]);
    // Drawn and scrollable.
    app.open_updates();
    let (s, _) = screen(&mut app, 120);
    assert!(s.contains("v9.9.8") && s.contains("Download"), "{s}");
    // An empty body: that version's CHANGELOG.md section.
    let (mut app2, home2) = test_app("notes2");
    let m = standalone(&home2);
    check(&mut app2, m, false, true);
    let (lines, _) = app2.updates_view(80);
    assert!(lines
        .iter()
        .any(|(t, _)| t.contains("From the changelog file")));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&home2);
}

#[test]
fn download_then_restart_which_asks_first() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("download");
    let m = standalone(&home);
    check(&mut app, m.clone(), false, false);
    assert_eq!(app.update.snapshot().phase, Phase::Available);
    // "Download": the checker downloads through the usual checks.
    check(&mut app, m, true, false);
    assert_eq!(app.update.ready().as_deref(), Some("9.9.9"));
    assert_eq!(app.update_badge().as_deref(), Some("↑ Update ready"));
    let (lines, buttons) = app.updates_view(80);
    assert!(lines
        .iter()
        .any(|(t, _)| t.contains("Downloaded and verified")));
    assert_eq!(buttons[0].1, UiAction::UpdatesRestart);
    // Restart asks first and says what survives.
    app.open_updates();
    screen(&mut app, 140);
    let r = app.last_hits.find(&UiAction::UpdatesRestart).unwrap().rect;
    app.on_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: r.x + 1,
        row: r.y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert_eq!(app.modal, crate::app::Modal::UpdateRestart);
    assert!(!app.quit);
    let (s, _) = screen(&mut app, 140);
    assert!(s.contains("Restart into v9.9.9 now?"), "{s}");
    assert!(s.contains("resumes it") && s.contains("interrupted"), "{s}");
    // No: back to the window, nothing restarted.
    app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.modal, crate::app::Modal::Updates(0));
    assert!(!app.quit);
    // The assistant: one yes.
    let v = app.control_call("restart_to_update", &serde_json::json!({}));
    assert!(
        v["needs_confirmation"]
            .as_str()
            .unwrap()
            .contains("Restart into version 9.9.9"),
        "{v}"
    );
    let v = app.control_call("update_info", &serde_json::json!({}));
    assert_eq!(v["result"]["latest"], serde_json::json!("9.9.9"), "{v}");
    assert_eq!(v["result"]["versions"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn homebrew_gets_its_command_not_download() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("brew");
    check(&mut app, update::Method::Homebrew, true, false);
    assert_eq!(
        app.update.snapshot().phase,
        Phase::Available,
        "never downloads"
    );
    let (lines, buttons) = app.updates_view(80);
    assert!(lines
        .iter()
        .any(|(t, _)| t.contains("brew upgrade godterm")));
    assert!(buttons
        .iter()
        .all(|(_, a, _)| !matches!(a, UiAction::UpdatesDownload | UiAction::UpdatesRestart)));
    let v = app.control_call("restart_to_update", &serde_json::json!({"confirm": true}));
    assert!(v.to_string().contains("brew upgrade godterm"), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn later_hides_it_and_a_narrow_terminal_keeps_it_whole() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("later");
    let m = standalone(&home);
    check(&mut app, m, false, false);
    for w in [60u16, 80, 100] {
        let (s, _) = screen(&mut app, w);
        assert!(s.lines().next().unwrap().contains("↑ v9.9.9"), "width {w}");
    }
    app.open_updates();
    app.updates_later();
    assert!(app.update_badge().is_none());
    let (s, _) = screen(&mut app, 200);
    assert!(!s.contains("↑ v9.9.9"));
    assert_eq!(app.modal, crate::app::Modal::None);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn markdown_and_changelog_sections() {
    let md = "# Title\n\nSome **bold** and `code`.\n- one\n  - nested [x](http://y)\n\n\n";
    let r = crate::ui_updates::render_markdown(md, 40);
    let t: Vec<&str> = r.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        t,
        vec!["Title", "", "Some bold and code.", "• one", "  • nested x"]
    );
    let v = update::Version::parse("1.2.0").unwrap();
    assert_eq!(
        update::changelog_section("## [1.2.0] - x\n- a\n## [1.1.0]\n- b\n", &v).as_deref(),
        Some("- a")
    );
    assert_eq!(
        update::changelog_section("## v1.2.0\nfixed\n", &v).as_deref(),
        Some("fixed")
    );
    assert!(update::changelog_section("## 1.1.0\n- b\n", &v).is_none());
}
