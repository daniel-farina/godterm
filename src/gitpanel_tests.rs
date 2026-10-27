use super::*;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::git_ops::tests::{fixture, g};

fn app_in(tag: &str, dir: &Path) -> (App, PathBuf) {
    let (mut app, home) = crate::confirm_click_tests::test_app(&format!("git-{tag}"));
    app.panes[0].tabs[0].cwd = dir.to_path_buf();
    app.panes[0].tabs[0].live_cwd = None;
    app.focus = 0;
    (app, home)
}

fn snapshot(fx: &crate::git_ops::tests::Fx) -> Vec<String> {
    vec![
        g(&fx.a, &["rev-parse", "HEAD"]),
        g(&fx.a, &["status", "--porcelain"]),
        g(&fx.a, &["for-each-ref"]),
        g(&fx.bare, &["for-each-ref"]),
        g(&fx.a, &["stash", "list"]),
        g(&fx.a, &["remote", "-v"]),
    ]
}

#[test]
fn assistant_tools_cannot_change_anything() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture("tools");
    let (mut app, home) = app_in("tools", &fx.a);
    let key = format!("sk-ant-{}", "K".repeat(30));
    std::fs::write(fx.a.join("w.txt"), format!("k = {key}\n")).unwrap();
    std::fs::write(fx.a.join(".env"), "SECRET=1\n").unwrap();
    let before = snapshot(&fx);
    let dir = fx.a.to_string_lossy().to_string();

    for t in NEVER_TOOLS {
        assert!(
            !crate::control::TOOLS.iter().any(|x| x.name == *t),
            "{t} exists"
        );
        for args in [
            json!({"project": dir, "message": "x", "all": true, "branch": "x", "owner": "alice", "repo": "x", "account": "alice"}),
            json!({"project": dir, "confirm_token": "abc123"}),
            json!({"project": dir, "confirm_token": "abc123", "_client": "web"}),
        ] {
            let v = app.control_call(t, &args);
            assert_eq!(v["ok"], false, "{t}: {v}");
        }
    }
    app.assistant_calls = 0;

    for t in crate::control::TOOLS
        .iter()
        .filter(|t| t.name.starts_with("git_"))
    {
        assert!(
            GIT_TOOLS.contains(&t.name) || t.name == "git_status",
            "{} is not a reading git tool",
            t.name
        );
    }
    for (t, args) in [
        ("git_log", json!({"project": dir})),
        ("git_diff", json!({"project": dir})),
        ("git_branches", json!({"project": dir})),
        ("git_bindings", json!({"project": dir})),
        (
            "git_draft_commit",
            json!({"project": dir, "message": "Add w"}),
        ),
        (
            "git_propose_binding",
            json!({"project": dir, "owner": "alice", "account": "alice"}),
        ),
        ("git_status", json!({"project": dir, "refresh": false})),
    ] {
        let v = app.control_call(t, &args);
        assert_eq!(v["ok"], true, "{t}: {v}");
        assert!(!v.to_string().contains(&key), "{t} showed a secret");
        assert!(!v.to_string().contains("SECRET=1"), "{t} showed .env");
    }
    assert_eq!(snapshot(&fx), before, "a tool changed the repo");
    assert!(
        !crate::git_map::store_path().exists(),
        "a tool saved a binding"
    );

    assert_eq!(git_panel::draft(&fx.a).as_deref(), Some("Add w"));
    assert_eq!(git_panel::proposal(&fx.a).unwrap().repo, "a");
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&home);
}

fn key(app: &mut App, c: KeyCode) {
    app.on_git_key(KeyEvent::from(c));
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

fn screen(app: &mut App) -> String {
    let mut t = crate::confirm_click_tests::term();
    crate::confirm_click_tests::draw(app, &mut t);
    let b = t.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            s.push_str(b[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn screen_html(app: &mut App) -> String {
    use ratatui::style::Color;
    let mut t = crate::confirm_click_tests::term();
    crate::confirm_click_tests::draw(app, &mut t);
    let b = t.backend().buffer().clone();
    let css = |c: Color, dflt: &str| match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Reset => dflt.to_string(),
        Color::Black => "#000".into(),
        Color::White | Color::Gray => "#ccc".into(),
        Color::DarkGray => "#666".into(),
        Color::Red | Color::LightRed => "#c66".into(),
        Color::Green | Color::LightGreen => "#8a8".into(),
        Color::Yellow | Color::LightYellow => "#cb8".into(),
        Color::Blue | Color::LightBlue => "#89b".into(),
        Color::Indexed(i) => format!("hsl({} 20% 60%)", i as u32 * 7),
        _ => dflt.to_string(),
    };
    let mut s = String::from("<html><body style=\"margin:0;background:#1c1e21\"><pre style=\"font:13px/1.25 Menlo,monospace;margin:8px\">");
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            let c = &b[(x, y)];
            let sym = c.symbol().replace('&', "&amp;").replace('<', "&lt;");
            s.push_str(&format!(
                "<span style=\"color:{};background:{}\">{}</span>",
                css(c.fg, "#d8d8d8"),
                css(c.bg, "#1c1e21"),
                sym
            ));
        }
        s.push('\n');
    }
    s.push_str("</pre></body></html>");
    s
}

fn dump(name: &str, text: &str) {
    if let Some(d) = std::env::var_os("GODTERM_GIT_SHOTS") {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(PathBuf::from(d).join(name), text);
    }
}

#[test]
fn tui_panel_commits_and_pushes_on_the_users_keys() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture("tui");
    let (mut app, home) = app_in("tui", &fx.a);
    std::fs::write(fx.a.join("notes.txt"), "n\n").unwrap();

    assert!(app
        .tab_actions(0, 0)
        .iter()
        .any(|r| r.act == crate::app_tabopts::TabAct::Git));
    app.tab_act_run(0, 0, crate::app_tabopts::TabAct::Git);
    assert!(matches!(app.modal, Modal::Git(_)));
    let s = screen(&mut app);
    dump("tui-main.txt", &s);
    let html = screen_html(&mut app);
    dump("tui-main.html", &html);
    assert!(s.contains("Not backed up: no GitHub repo"), "{s}");
    assert!(s.contains("Commit..."), "{s}");
    assert!(s.contains("No repo defined"), "{s}");
    let head0 = g(&fx.a, &["rev-parse", "HEAD"]);

    key(&mut app, KeyCode::Char('c'));
    let s = screen(&mut app);
    dump("tui-commit.txt", &s);
    assert!(s.contains("Message: Update notes.txt"), "{s}");
    key(&mut app, KeyCode::Enter);
    let s = screen(&mut app);
    dump("tui-confirm.txt", &s);
    assert!(s.contains("Yes, do it"), "{s}");
    assert_eq!(
        g(&fx.a, &["rev-parse", "HEAD"]),
        head0,
        "nothing before the yes"
    );

    key(&mut app, KeyCode::Esc);
    assert_eq!(g(&fx.a, &["rev-parse", "HEAD"]), head0);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    assert_ne!(
        g(&fx.a, &["rev-parse", "HEAD"]),
        head0,
        "committed on the yes"
    );
    let Modal::Git(d) = &app.modal else { panic!() };
    assert!(
        d.note.as_ref().unwrap().0.starts_with("Committed"),
        "{:?}",
        d.note
    );

    key(&mut app, KeyCode::Char('p'));
    let s = screen(&mut app);
    key(&mut app, KeyCode::Enter);
    let Modal::Git(d) = &app.modal else { panic!() };
    assert_eq!(
        g(&fx.bare, &["rev-parse", "refs/heads/main"]),
        g(&fx.a, &["rev-parse", "HEAD"]),
        "{:?}\n{s}",
        d.note
    );

    std::fs::write(fx.a.join("k.txt"), format!("x = AKIA{}\n", "Z".repeat(16))).unwrap();

    key(&mut app, KeyCode::Char('r'));
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    let pushed = g(&fx.bare, &["rev-parse", "refs/heads/main"]);
    key(&mut app, KeyCode::Char('p'));
    let s = screen(&mut app);
    dump("tui-secret.txt", &s);
    let html = screen_html(&mut app);
    dump("tui-secret.html", &html);
    assert!(s.contains("possible secret"), "{s}");
    assert!(
        !s.contains(&format!("AKIA{}", "Z".repeat(16))),
        "shown unredacted"
    );
    key(&mut app, KeyCode::Enter);
    typed(&mut app, "sure");
    key(&mut app, KeyCode::Enter);
    assert_eq!(g(&fx.bare, &["rev-parse", "refs/heads/main"]), pushed);
    key(&mut app, KeyCode::Char('p'));
    key(&mut app, KeyCode::Enter);
    typed(&mut app, "push anyway");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        g(&fx.bare, &["rev-parse", "refs/heads/main"]),
        g(&fx.a, &["rev-parse", "HEAD"])
    );

    app.git_tab_finished(&fx.a);
    crate::project_git::update(
        &app.project_index,
        Some(std::slice::from_ref(&fx.a)),
        &crate::project_git::Limits::tool(),
        true,
    );
    app.git.badges.borrow_mut().clear();
    let b = app.git_badge(&fx.a).unwrap_or_default();
    assert!(b.contains("Not backed up: no GitHub repo"), "{b}");
    app.modal = Modal::None;
    let s = screen(&mut app);
    dump("tui-tablist.txt", &s);
    assert!(s.contains('⚠'), "{s}");
    let st = app.control_call("get_state", &json!({}));
    assert!(st.to_string().contains("Not backed up"), "{st}");
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&home);
}
