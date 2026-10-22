//! The model bar (app_tabmodel, ui_modelbar): what it shows, clicks and
//! keys, a busy tab queued until its turn ends, the set_tab_model tool,
//! and the current model read from a transcript, a session and a screen.
//! Tabs run the test stub only (never claude or grok), in a test home.

use crate::app_tabmodel::*;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::harness::Harness;
use crate::hits::UiAction;
#[cfg(unix)]
use crate::pane::{Activity, LaunchKind};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::json;
#[cfg(unix)]
use std::time::{Duration, Instant};

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-tabmodel-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    let base = home.join("newtabs");
    std::fs::create_dir_all(&base).unwrap();
    app.cfg.new_tab_base = base.to_string_lossy().into_owned();
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    app.cfg.grok_bin = Some(crate::test_stub::true_bin());
    for a in &mut app.accounts {
        a.login.source = Some(CredSource::Keychain);
    }
    // Account 2 runs Grok Build.
    app.cfg.accounts[1].harness = "grok".into();
    (app, home)
}

fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    let a = buf.area;
    let mut out = String::new();
    for y in a.y..a.y + a.height {
        for x in a.x..a.x + a.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Where `label` is drawn as button `k` of pane `i`'s bar.
fn button(app: &crate::app::App, i: usize, k: usize) -> ratatui::layout::Rect {
    app.last_hits
        .regions
        .iter()
        .rev()
        .find(|r| r.action == UiAction::TabModel(i, k))
        .unwrap_or_else(|| panic!("no model button {k} in pane {i}"))
        .rect
}

#[cfg(unix)]
fn click(app: &mut crate::app::App, r: ratatui::layout::Rect) {
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.handle(crate::app::AppEvent::Input(Event::Mouse(MouseEvent {
            kind,
            column: r.x + r.width / 2,
            row: r.y,
            modifiers: KeyModifiers::NONE,
        })));
    }
    app.last_click = None;
}

/// Tab (s, 0) runs a stub that records its input; returns that file.
#[cfg(unix)]
fn run_stub(app: &mut crate::app::App, home: &std::path::Path, s: usize) -> std::path::PathBuf {
    let input = home.join(format!("stdin-{s}.txt"));
    let stub = crate::test_stub::claude(
        &home.join(format!("stub-{s}")),
        &[
            ("stdin_to", input.display().to_string()),
            ("sleep_s", "60".into()),
        ],
    );
    let bin = Some(stub.to_string_lossy().into_owned());
    app.cfg.claude_bin = bin.clone();
    app.cfg.grok_bin = bin;
    let work = home.join(format!("work-{s}"));
    std::fs::create_dir_all(&work).unwrap();
    app.panes[s].tabs[0].cwd = work;
    app.launch_tab(s, 0, LaunchKind::Normal);
    let t0 = Instant::now();
    while !app.panes[s].tabs[0].is_running() {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "the stub never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    input
}

/// The Enter after the paste.
#[cfg(unix)]
const ENTER: &str = "\x1b[201~\r";

/// Press the pending Enters (as if the paste showed), then wait for the
/// stub to have read `want`.
#[cfg(unix)]
fn typed(app: &mut crate::app::App, input: &std::path::Path, want: &str) -> String {
    for p in &mut app.pending_enters {
        p.at -= Duration::from_secs(10);
    }
    app.control_tick();
    let t0 = Instant::now();
    loop {
        let got = std::fs::read_to_string(input).unwrap_or_default();
        // The tty turns the Enter (CR) into a newline.
        if got.contains(want) || (want == ENTER && got.contains("\x1b[201~\n")) {
            return got;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "never typed {want:?}: {got:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn registry_models_per_harness_and_overrides() {
    let mut cfg = crate::config::Config::default();
    let labels = |h, cfg: &crate::config::Config| -> Vec<String> {
        models_for(h, cfg).into_iter().map(|m| m.label).collect()
    };
    assert_eq!(labels(Harness::Claude, &cfg), ["Haiku", "Sonnet", "Opus"]);
    assert_eq!(labels(Harness::Grok, &cfg), ["4.7 fast", "4.7", "4.6"]);
    assert_eq!(models_for(Harness::Claude, &cfg)[0].arg, "haiku");
    // A list of its own: known ones keep their labels, new ones get one.
    cfg.tab_models.claude = vec![
        "haiku".into(),
        "opus".into(),
        "fable=Fable".into(),
        "sonnet".into(),
        "extra".into(),
    ];
    assert_eq!(
        labels(Harness::Claude, &cfg),
        ["Haiku", "Opus", "Fable", "Sonnet"],
        "at most 4"
    );
    let m = models_for(Harness::Claude, &cfg);
    assert_eq!((m[2].arg.as_str(), m[2].label.as_str()), ("fable", "Fable"));
    // Model ids and display names find their button; the longest wins.
    let g = models_for(Harness::Grok, &crate::config::Config::default());
    assert_eq!(index_of(&g, "grok-4.7-build-fast"), Some(0));
    assert_eq!(index_of(&g, "grok-4.7"), Some(1));
    assert_eq!(index_of(&g, "Grok 4.7"), Some(1));
    assert_eq!(index_of(&g, "grok-4.6"), Some(2));
    let c = models_for(Harness::Claude, &crate::config::Config::default());
    assert_eq!(index_of(&c, "claude-opus-5-5"), Some(2));
    assert_eq!(index_of(&c, "Sonnet 5.5"), Some(1));
    assert_eq!(index_of(&c, "claude-haiku-4-5-20251001"), Some(0));
    assert_eq!(index_of(&c, "gpt-5"), None);
}

#[test]
fn bar_shows_each_harness_models_with_the_current_one_highlighted() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("render");
    assert_eq!(app.panes[1].account, Some(1));
    app.panes[0].tabs[0].model = Some("claude-sonnet-5-5".into());
    app.panes[0].tabs[0].effort = Some("medium".into());
    app.panes[1].tabs[0].model = Some("grok-4.7-build-fast".into());
    let mut term = Terminal::new(TestBackend::new(240, 50)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let text = buffer_text(term.backend().buffer());
    let claude_row = text
        .lines()
        .find(|l| l.contains("1 Haiku") && l.contains("2 Sonnet") && l.contains("3 Opus"))
        .unwrap_or_else(|| panic!("the Claude bar:\n{text}"));
    assert!(claude_row.contains("model"), "{claude_row}");
    assert!(claude_row.contains("· medium"), "the effort: {claude_row}");
    assert!(
        text.lines()
            .any(|l| l.contains("1 4.7 fast") && l.contains("2 4.7") && l.contains("3 4.6")),
        "the Grok bar:\n{text}"
    );
    let buf = term.backend().buffer().clone();
    let bg = |r: ratatui::layout::Rect| buf[(r.x + 2, r.y)].bg;
    // The current one on the calm slate fill, the others plain.
    assert_eq!(bg(button(&app, 0, 1)), crate::theme::SELECT_BG);
    assert_ne!(bg(button(&app, 0, 0)), crate::theme::SELECT_BG);
    assert_ne!(bg(button(&app, 0, 2)), crate::theme::SELECT_BG);
    assert_eq!(bg(button(&app, 1, 0)), crate::theme::SELECT_BG);
    assert_ne!(bg(button(&app, 1, 1)), crate::theme::SELECT_BG);
    // The bar sits right above the usage footer, under the terminal.
    let b = button(&app, 0, 0);
    let term_rect = app.term_rects[0];
    assert_eq!(b.y, term_rect.y + term_rect.height, "{b:?} {term_rect:?}");
    // The user switched inside the tab: the bar follows.
    app.panes[0].tabs[0].model = Some("Opus 5.5".into());
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer().clone();
    assert_eq!(
        buf[(button(&app, 0, 2).x + 2, b.y)].bg,
        crate::theme::SELECT_BG
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn hidden_when_short_collapsed_or_off() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("hidden");
    let has_bar = |app: &crate::app::App, i: usize| {
        app.last_hits
            .regions
            .iter()
            .any(|r| matches!(r.action, UiAction::TabModel(p, _) if p == i))
    };
    let mut term = Terminal::new(TestBackend::new(240, 50)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(has_bar(&app, 0));
    let tall = app.panes[0].tabs[0].pty_size().0;
    // Short panes (under 12 rows inside) have none, and the PTY keeps
    // the row.
    let mut short = Terminal::new(TestBackend::new(240, 24)).unwrap();
    short.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(!has_bar(&app, 0), "no bar on a short pane");
    assert!(!model_bar_on_any(&app));
    assert!(tall > 2);
    // Collapsed: no bar on its strip.
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(has_bar(&app, 0));
    app.toggle_fold_pane(0);
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(!has_bar(&app, 0), "collapsed");
    assert!(has_bar(&app, 1), "the others keep theirs");
    app.toggle_fold_pane(0);
    // Off in Settings: none anywhere, the terminal gets the row back.
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let with = app.term_rects[0].height;
    app.cfg.tab_model_bar = false;
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(!has_bar(&app, 0) && !has_bar(&app, 1));
    assert_eq!(app.term_rects[0].height, with + 1);
    let _ = std::fs::remove_dir_all(&home);
}

fn model_bar_on_any(app: &crate::app::App) -> bool {
    app.last_hits
        .regions
        .iter()
        .any(|r| matches!(r.action, UiAction::TabModel(..)))
}

#[test]
#[cfg(unix)]
fn a_click_on_an_idle_tab_types_model_haiku() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("click");
    let input = run_stub(&mut app, &home, 0);
    app.panes[0].tabs[0].activity = Activity::Ready;
    app.panes[0].tabs[0].model = Some("claude-opus-5-5".into());
    let mut term = Terminal::new(TestBackend::new(240, 50)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let b = button(&app, 0, 0);
    click(&mut app, b);
    assert!(
        app.flash
            .as_ref()
            .is_some_and(|f| f.0.contains("to Haiku") && f.0.contains("default")),
        "{:?}",
        app.flash
    );
    // The paste path: the paste now, Enter once it shows.
    assert_eq!(app.pending_enters.len(), 1);
    assert_eq!(app.pending_enters[0].text, "/model haiku");
    let got = typed(&mut app, &input, ENTER);
    assert!(
        got.contains("\x1b[200~/model haiku\x1b[201~"),
        "bracketed paste: {got:?}"
    );
    assert!(app.pending_enters.is_empty());
    // Shown as current at once.
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer().clone();
    let b = button(&app, 0, 0);
    assert_eq!(buf[(b.x + 2, b.y)].bg, crate::theme::SELECT_BG);
    // Its own model again: nothing typed.
    app.panes[0].tabs[0].model = Some("claude-haiku-4-5".into());
    app.panes[0].tabs[0].model_set = None;
    app.click_tab_model(0, 0);
    assert!(app.pending_enters.is_empty());
    assert!(app.flash.as_ref().unwrap().0.contains("already"));
    // Keys: Ctrl-a # then 3 is Opus; digits otherwise go to the tab.
    app.start_model_entry();
    assert!(app
        .flash
        .as_ref()
        .unwrap()
        .0
        .contains("1 Haiku, 2 Sonnet, 3 Opus"));
    let key = |c| {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        )
    };
    assert!(app.model_entry_key(&key('3')));
    assert_eq!(app.pending_enters.last().unwrap().text, "/model opus");
    assert!(!app.model_entry_key(&key('3')), "once only");
    app.panes[0].tabs[0].kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
#[cfg(unix)]
fn a_busy_tab_is_queued_and_switched_when_its_turn_ends() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("busy");
    let input = run_stub(&mut app, &home, 0);
    app.panes[0].tabs[0].activity = Activity::Working;
    app.panes[0].tabs[0].model = Some("claude-opus-5-5".into());
    let r = app.switch_tab_model(0, 0, 0).unwrap();
    assert_eq!(r, Switched::Queued("after this turn"));
    assert!(app.switch_say(0, 0, 0, &r).contains("after this turn"));
    assert!(
        app.pending_enters.is_empty(),
        "never typed into a working tab"
    );
    let mut term = Terminal::new(TestBackend::new(240, 50)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(buffer_text(term.backend().buffer()).contains("switching to Haiku after this turn"), "{}", buffer_text(term.backend().buffer()));
    // The assistant's state says so.
    assert!(
        app.state_preamble().contains("Opus, Haiku queued"),
        "{}",
        app.state_preamble()
    );
    for _ in 0..3 {
        app.control_tick();
    }
    assert!(app.pending_enters.is_empty() && app.model_switches.len() == 1);
    // A draft in the input box: still waits.
    app.panes[0].tabs[0].activity = Activity::Ready;
    app.panes[0].tabs[0].parser.lock().unwrap().process(
        b"\x1b[2J\x1b[H\r\n\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\r\n\xe2\x9d\xaf half typed\r\n",
    );
    app.control_tick();
    assert!(app.pending_enters.is_empty(), "not over the user's draft");
    // The turn ended, the box is empty: switched now.
    app.panes[0].tabs[0]
        .parser
        .lock()
        .unwrap()
        .process(b"\x1b[2J\x1b[H\r\n\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\r\n\xe2\x9d\xaf \r\n");
    app.control_tick();
    assert!(app.model_switches.is_empty());
    assert_eq!(app.pending_enters.len(), 1);
    let got = typed(&mut app, &input, ENTER);
    assert!(got.contains("/model haiku"), "{got:?}");
    app.panes[0].tabs[0].kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_tool_resolves_model_names() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let c = models_for(Harness::Claude, &crate::config::Config::default());
    for (said, want) in [
        ("haiku", Some(0)),
        ("Haiku", Some(0)),
        ("use opus", Some(2)),
        ("claude sonnet 5.5", Some(1)),
        ("the quick one", Some(0)),
        ("the fastest", Some(0)),
        ("smartest", Some(2)),
        ("2", Some(1)),
        ("claude-opus-5-5", Some(2)),
        ("gpt", None),
        ("", None),
    ] {
        assert_eq!(resolve(&c, said), want, "{said}");
    }
    let g = models_for(Harness::Grok, &crate::config::Config::default());
    for (said, want) in [
        ("grok 4.7 fast", Some(0)),
        ("grok-4.7-build-fast", Some(0)),
        ("the fast one", Some(0)),
        ("grok-4.7", Some(1)),
        ("4.7", Some(1)),
        ("previous", Some(2)),
        ("4.6", Some(2)),
        ("grok", None),
    ] {
        assert_eq!(resolve(&g, said), want, "{said}");
    }
    // The tool: at once, no confirmation; a tab not started yet gets it
    // once it starts.
    let (mut app, home) = test_app("tool");
    let id = crate::control::tab_id(app.panes[0].tabs[0].uid);
    let v = app.control_call("set_tab_model", &json!({"tab": id, "model": "haiku"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(v.get("needs_confirmation").is_none());
    assert_eq!(v["result"]["status"], json!("queued"), "{v}");
    assert_eq!(v["result"]["model"], json!("Haiku"));
    assert!(v["result"]["say"]
        .as_str()
        .unwrap()
        .contains("once it starts"));
    let st = app.control_call("get_state", &json!({}));
    assert_eq!(
        st["result"]["tabs"][0]["model_queued"],
        json!("Haiku"),
        "{st}"
    );
    // A Grok tab resolves Grok's names; a wrong one lists the choices.
    let gid = crate::control::tab_id(app.panes[1].tabs[0].uid);
    let v = app.control_call(
        "set_tab_model",
        &json!({"tab": gid, "model": "the fast one"}),
    );
    assert_eq!(v["result"]["arg"], json!("grok-4.7-build-fast"), "{v}");
    let v = app.control_call("set_tab_model", &json!({"tab": gid, "model": "opus"}));
    let e = v["error"].as_str().unwrap_or_default();
    assert!(e.contains("4.7 fast (grok-4.7-build-fast)"), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_current_model_comes_from_the_transcript() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("transcript");
    let fixture = include_str!("../tests/fixtures/transcript_models.jsonl");
    let file = home.join("t.jsonl");
    std::fs::write(&file, fixture).unwrap();
    // The newest assistant reply's model, not a subagent's or a synthetic.
    assert_eq!(
        claude_transcript_model(&file).as_deref(),
        Some("claude-opus-5-5")
    );
    // The tab's own transcript, in the account's home (a test home).
    let sid = "5e55a0de-0000-4000-8000-000000000001";
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let proj = app.cfg.accounts[0]
        .config_dir()
        .join("projects")
        .join(crate::state::encode_project_dir(&work));
    std::fs::create_dir_all(&proj).unwrap();
    let path = proj.join(format!("{sid}.jsonl"));
    std::fs::write(&path, fixture).unwrap();
    app.panes[0].tabs[0].cwd = work;
    app.panes[0].tabs[0].session_id = Some(sid.into());
    app.refresh_tab_model(0, 0, true);
    assert_eq!(app.tab_model_label(0, 0).as_deref(), Some("Opus"));
    // The user runs /model sonnet in the tab: Claude Code logs it.
    let mut t = fixture.to_string();
    t.push_str(&json!({"type": "user", "isSidechain": false, "message": {"role": "user", "content": "<command-name>/model</command-name>\n<command-args>sonnet</command-args>"}}).to_string());
    t.push('\n');
    t.push_str(&json!({"type": "user", "isSidechain": false, "message": {"role": "user", "content": "<local-command-stdout>Set model to `Sonnet 5.5` and saved as your default for new sessions</local-command-stdout>"}}).to_string());
    t.push('\n');
    std::fs::write(&path, &t).unwrap();
    app.refresh_tab_model(0, 0, true);
    assert_eq!(app.panes[0].tabs[0].model.as_deref(), Some("Sonnet 5.5"));
    assert_eq!(app.tab_model_label(0, 0).as_deref(), Some("Sonnet"));
    assert!(app.state_preamble().contains("Sonnet"));
    // ANSI bold instead of backticks reads the same.
    assert_eq!(
        set_model_text("Set model to \u{1b}[1mHaiku 4.5\u{1b}[22m (claude-haiku-4-5)").as_deref(),
        Some("Haiku 4.5")
    );
    // No transcript yet: the screen's banner, and its effort.
    let screen = include_str!("../tests/fixtures/screens/input_ready.txt");
    assert_eq!(
        screen_model(Harness::Claude, screen).as_deref(),
        Some("Opus 5.5")
    );
    assert_eq!(
        screen_effort(Harness::Claude, screen).as_deref(),
        Some("medium")
    );
    let mut p = vt100::Parser::new(40, 120, 0);
    p.process(screen.replace('\n', "\r\n").as_bytes());
    assert_eq!(input_empty(Harness::Claude, p.screen()), Some(true));
    let _ = std::fs::remove_dir_all(&home);
}

/// Grok Build has /model too (`/model <name> [window] [effort]`, model
/// id or display name): the same paste, with Grok's ids; its model is
/// read from its status line, header or session.
#[test]
fn grok_tabs_switch_with_its_own_model_command() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let ready = include_str!("../tests/fixtures/screens/grok_ready.txt");
    assert_eq!(
        screen_model(Harness::Grok, ready).as_deref(),
        Some("grok-4.7-build-fast")
    );
    let status = " Weekly limit left: 40% · Grok 4.7 (high) · always-approve";
    assert_eq!(
        screen_model(Harness::Grok, status).as_deref(),
        Some("Grok 4.7")
    );
    assert_eq!(
        screen_effort(Harness::Grok, status).as_deref(),
        Some("high")
    );
    let mut p = vt100::Parser::new(10, 80, 0);
    p.process(ready.replace('\n', "\r\n").as_bytes());
    assert_eq!(input_empty(Harness::Grok, p.screen()), Some(true));
    let mut p = vt100::Parser::new(10, 80, 0);
    p.process(" ╭──────────╮\r\n │ draft    │\r\n ╰──────────╯\r\n".as_bytes());
    assert_eq!(input_empty(Harness::Grok, p.screen()), Some(false));
    let (mut app, home) = test_app("grok");
    // Its session's summary.json.
    let dir = home.join("sess");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("summary.json"),
        json!({"current_model_id": "grok-4.6"}).to_string(),
    )
    .unwrap();
    assert_eq!(grok_session_model(&dir).as_deref(), Some("grok-4.6"));
    #[cfg(unix)]
    {
        let input = run_stub(&mut app, &home, 1);
        app.panes[1].tabs[0].activity = Activity::Ready;
        let v = app.control_call(
            "set_tab_model",
            &json!({"tab": crate::control::tab_id(app.panes[1].tabs[0].uid), "model": "grok 4.7"}),
        );
        assert_eq!(v["result"]["status"], json!("applied"), "{v}");
        assert_eq!(v["result"]["command"], json!("/model grok-4.7"));
        assert!(!v["result"]["say"].as_str().unwrap().contains("default"));
        let got = typed(&mut app, &input, ENTER);
        assert!(got.contains("\x1b[200~/model grok-4.7\x1b[201~"), "{got:?}");
        app.panes[1].tabs[0].kill();
    }
    let _ = std::fs::remove_dir_all(&home);
}
