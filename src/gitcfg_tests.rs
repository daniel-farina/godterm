//! Settings > GitHub accounts and repos in the app: the assistant can
//! read the owners and rules but no tool saves them; the web section's
//! frames check, confirm and save; the terminal editor saves only on the
//! user's keys, after the diff.

use super::*;
use crate::config::testing::LOCK as ENV_LOCK;

const START: &str = "# my config (keep this comment)\n";

/// A `github-suggestions.toml`, as documented in docs/SYNC.md.
const SUGGESTIONS: &str = r#"placeholders = ["acme-org"]

[[owners]]
owner = "alice"
account = "alice"

[[owners]]
owner = "alice-labs"
account = "alice"

[[owners]]
owner = "bob"
account = "bob"

[[owners]]
owner = "acme-org"
account = "alice_acme"

[[rules]]
match = "~/labs/**"
owner = "alice-labs"

[[rules]]
match = "~/acme/**"
owner = "choose"
choices = ["acme-org", "alice"]
state = "exclude"

[[rules]]
match = "~/**"
owner = "alice"
"#;

fn app(tag: &str) -> (App, PathBuf) {
    let (mut app, home) = crate::confirm_click_tests::test_app(&format!("gitcfg-{tag}"));
    let text = std::fs::read_to_string(Config::path()).unwrap();
    std::fs::write(Config::path(), format!("{START}{text}")).unwrap();
    app.reload_config_now();
    let proj = std::env::temp_dir().join(format!("gitcfg-proj-hub-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&proj).unwrap();
    app.panes[0].tabs[0].cwd = proj;
    // The user's own suggestions file stands in for the shipped examples.
    std::fs::write(git_cfg::suggestions_path(), SUGGESTIONS).unwrap();
    app.panes[0].tabs[0].live_cwd = None;
    app.git.cfg_ui.accounts = Some(vec![
        Acct {
            name: "alice".into(),
            logged_in: true,
            active: true,
        },
        Acct {
            name: "bob".into(),
            logged_in: true,
            active: false,
        },
    ]);
    (app, home)
}

fn config() -> String {
    std::fs::read_to_string(Config::path()).unwrap()
}

#[test]
fn the_assistant_can_read_but_never_save_them() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = app("tools");
    // Saved by the user first (the starter set), so there is something to read.
    let r = app
        .gitcfg_frame(&json!({"op": "cfg_check", "draft": git_cfg::sample("acme-org"), "id": 1}));
    let id = r["plan_id"].as_str().unwrap().to_string();
    let r = app.gitcfg_frame(&json!({"op": "cfg_save", "plan_id": id}));
    assert_eq!(r["ok"], true, "{r}");
    let before = config();
    let dir = app.tab_dir(0, 0).to_string_lossy().to_string();
    // It reads them.
    let v = app.control_call("git_bindings", &json!({"project": dir}));
    assert_eq!(v["ok"], true, "{v}");
    let s = v.to_string();
    assert!(s.contains("~/labs/**") && s.contains("alice_acme"), "{s}");
    assert!(s.contains("acme-org"), "{s}");
    // No tool by any saving name exists or works, with or without a token.
    for t in [
        "git_set_rules",
        "git_save_rules",
        "git_set_owners",
        "git_set_owner",
        "git_cfg_save",
        "git_settings_save",
        "save_git_settings",
        "git_use_placeholders",
        "git_unbind",
        "git_bind",
        "git_rules",
    ] {
        assert!(
            !crate::control::TOOLS.iter().any(|x| x.name == t),
            "{t} exists"
        );
        for args in [
            json!({"owners": {"x": "y"}, "rules": [{"match": "~/**", "owner": "x"}]}),
            json!({"draft": git_cfg::sample("acme-org"), "plan_id": "c1", "confirm_token": "abc"}),
        ] {
            assert_eq!(app.control_call(t, &args)["ok"], false, "{t}");
        }
    }
    // set_setting does not reach [git], as the assistant or from the web.
    for (k, val) in [
        ("git.owners", "{\"x\" = \"y\"}"),
        ("git.rules", "[]"),
        ("git.owners.alice", "someone"),
        ("git.undefined", "guess"),
    ] {
        for args in [
            json!({"key": k, "value": val}),
            json!({"key": k, "value": val, "_client": "web", "user_says": "set it"}),
        ] {
            let v = app.control_call("set_setting", &args);
            assert_eq!(v["ok"], false, "{k}: {v}");
        }
    }
    assert_eq!(config(), before, "a tool changed config.toml");
    assert!(
        !crate::git_map::store_path().exists(),
        "a tool saved a binding"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn web_frames_check_confirm_and_save() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = app("web");
    let v = app.gitcfg_frame(&json!({"op": "cfg_view", "id": "a"}));
    assert_eq!(v["t"], "git");
    assert_eq!(v["id"], "a");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["view"]["empty"], true);
    assert_eq!(v["view"]["placeholders"]["owners"][3]["owner"], "acme-org");
    assert!(v["view"]["placeholder_owners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o == "acme-org"));
    assert_eq!(v["view"]["accounts"][0]["name"], "alice");
    // The project folder (the tab's) is known.
    assert!(
        v["view"]["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().contains("proj-hub")),
        "{v}"
    );
    // Bad drafts: no plan, the reasons.
    let mut bad = git_cfg::sample("acme-org");
    bad.rules[0].pattern = "/Volumes/Private/**".into();
    bad.rules[1].pattern = "~/[x/**".into();
    let r = app.gitcfg_frame(&json!({"op": "cfg_check", "draft": bad, "id": "b"}));
    assert_eq!(r["ok"], true);
    assert!(r["plan_id"].is_null(), "{r}");
    let errs = r["check"]["errors"].to_string();
    assert!(
        errs.contains("vault") && errs.contains("not a valid glob"),
        "{errs}"
    );
    // "Use these": checked, the diff shown, saved only with its id.
    let r = app
        .gitcfg_frame(&json!({"op": "cfg_check", "draft": git_cfg::sample("acme-org"), "id": "c"}));
    let diff = r["diff"].to_string();
    assert!(
        diff.contains("+ owner acme-org -> alice_acme (placeholder)"),
        "{diff}"
    );
    assert!(
        r["check"]["warnings"]
            .to_string()
            .contains("alice_acme (for acme-org) is not logged in"),
        "{r}"
    );
    let id = r["plan_id"].as_str().unwrap().to_string();
    assert_eq!(
        config(),
        format!("{START}{}", config().trim_start_matches(START))
    );
    assert!(!config().contains("alice-labs"), "saved before the yes");
    let r = app.gitcfg_frame(&json!({"op": "cfg_save", "plan_id": "c0000", "id": "d"}));
    assert_eq!(r["ok"], false);
    let r = app.gitcfg_frame(&json!({"op": "cfg_save", "plan_id": id, "id": "e"}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["said"]
        .as_str()
        .unwrap()
        .starts_with("Saved 4 owners and 3 folder rules"));
    // Live: the app has them now, and the file kept its comments.
    assert_eq!(app.cfg.git.owners["alice-labs"], "alice");
    assert_eq!(app.cfg.git.rules.len(), 3);
    assert!(config().starts_with(START), "{}", config());
    assert!(config().contains("# placeholder: set the real name"));
    assert_eq!(r["view"]["empty"], false);
    // The same id cannot save twice.
    let r = app.gitcfg_frame(&json!({"op": "cfg_save", "plan_id": id}));
    assert_eq!(r["ok"], false);
    // A binding edit and an unbind go through the store.
    let tab = app.tab_dir(0, 0);
    let key = crate::git_map::key(&tab);
    let mut d: Draft = serde_json::from_value(app.gitcfg_view()["draft"].clone()).unwrap();
    d.binds = vec![BindEdit {
        key: key.clone(),
        binding: Some(Binding {
            owner: "alice-labs".into(),
            repo: "hub".into(),
            account: "alice".into(),
            ..Default::default()
        }),
    }];
    let r = app.gitcfg_frame(&json!({"op": "cfg_check", "draft": d}));
    assert_eq!(
        r["diff"][0],
        format!("+ bind {key} -> alice-labs/hub as alice")
    );
    let id = r["plan_id"].as_str().unwrap().to_string();
    assert_eq!(
        app.gitcfg_frame(&json!({"op": "cfg_save", "plan_id": id}))["ok"],
        true
    );
    assert_eq!(Store::load().get(&tab).unwrap().repo, "hub");
    d.binds[0].binding = None;
    let r = app.gitcfg_frame(&json!({"op": "cfg_check", "draft": d}));
    let id = r["plan_id"].as_str().unwrap().to_string();
    assert_eq!(
        app.gitcfg_frame(&json!({"op": "cfg_save", "plan_id": id}))["ok"],
        true
    );
    assert!(Store::load().get(&tab).is_none());
    let _ = std::fs::remove_dir_all(&home);
}

// The web Git screen's account picker: the saved draft plus one owner
// row, checked then saved; the other owners stay. Its "Add account..."
// ops answer without reaching a real gh.
#[test]
fn the_git_screen_picker_adds_one_owner() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = app("picker");
    let v = app.gitcfg_frame(&json!({"op": "cfg_view"}));
    let mut d: Draft = serde_json::from_value(v["view"]["draft"].clone()).unwrap();
    let before = d.owners.len();
    d.owners
        .retain(|o| !o.owner.eq_ignore_ascii_case("widget-co"));
    d.owners.push(OwnerRow {
        owner: "widget-co".into(),
        account: "bob".into(),
    });
    let r = app.gitcfg_frame(&json!({"op": "cfg_check", "draft": d}));
    assert_eq!(r["check"]["errors"], json!([]), "{r}");
    let id = r["plan_id"].as_str().unwrap().to_string();
    let r = app.gitcfg_frame(&json!({"op": "cfg_save", "plan_id": id}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(app.cfg.git.owners["widget-co"], "bob");
    assert_eq!(app.cfg.git.owners.len(), before + 1);
    for op in ["cfg_gh_login", "cfg_gh_login_poll", "cfg_gh_login_cancel"] {
        let r = app.gitcfg_frame(&json!({"op": op, "id": "x"}));
        assert_eq!(r["ok"], true, "{op}: {r}");
        assert_eq!(r["t"], "git");
        assert_eq!(r["login"]["state"], "failed", "{op}: {r}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

fn key(app: &mut App, c: KeyCode) {
    app.on_gitcfg_key(KeyEvent::from(c));
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

fn dump(name: &str, text: &str) {
    if let Some(d) = std::env::var_os("GODTERM_GIT_SHOTS") {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(PathBuf::from(d).join(name), text);
    }
}

fn dlg(app: &App) -> GitCfgDlg {
    match &app.modal {
        Modal::GitCfg(d) => d.clone(),
        _ => panic!("the editor is not open"),
    }
}

#[test]
fn tui_editor_saves_on_the_users_keys() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = app("tui");
    // Settings > GitHub, Enter: the editor, with the starter set.
    app.open_settings();
    app.settings_section = crate::settings::SECTIONS
        .iter()
        .position(|s| s.0 == crate::settings::Section::Git)
        .unwrap();
    let s = screen(&mut app);
    dump("tui-gitcfg-section.txt", &s);
    assert!(s.contains("Edit owners, rules and bindings"), "{s}");
    app.on_settings_key(KeyEvent::from(KeyCode::Enter));
    let d = dlg(&app);
    assert!(d.starter);
    let s = screen(&mut app);
    dump("tui-gitcfg-starter.txt", &s);
    assert!(s.contains("placeholders, not saved yet"), "{s}");
    assert!(s.contains("acme-org -> alice_acme  placeholder"), "{s}");
    assert!(s.contains("Use these"), "{s}");
    // u: the diff first, nothing written.
    key(&mut app, KeyCode::Char('u'));
    let s = screen(&mut app);
    dump("tui-gitcfg-confirm.txt", &s);
    assert!(s.contains("+ owner alice-labs -> alice"), "{s}");
    assert!(!config().contains("alice-labs"));
    key(&mut app, KeyCode::Enter);
    assert!(config().contains("[[git.rules]]"), "{}", config());
    assert!(!dlg(&app).starter);
    assert_eq!(app.cfg.git.rules[0].pattern, "~/labs/**");
    // Add a rule for the tab's folder (a known project) by typing.
    let tab = app.tab_dir(0, 0);
    let tk = crate::git_map::key(&tab);
    let items = app.gitcfg_items(&dlg(&app));
    let add = items.iter().position(|i| *i == Item::AddRule).unwrap();
    let mut d = dlg(&app);
    d.sel = add;
    app.modal = Modal::GitCfg(d);
    key(&mut app, KeyCode::Enter);
    typed(&mut app, &format!("{tk}/**"));
    key(&mut app, KeyCode::Tab);
    typed(&mut app, "bob");
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Right);
    let s = screen(&mut app);
    dump("tui-gitcfg-form.txt", &s);
    key(&mut app, KeyCode::Enter);
    let d = dlg(&app);
    assert_eq!(d.draft.rules[3].owner, "bob");
    assert_eq!(d.draft.rules[3].state.as_deref(), Some("exclude"));
    let s = screen(&mut app);
    assert!(s.contains("-> bob") && s.contains("(bob), kept out"), "{s}");
    // Move it to the top (Shift-Up three times): still saved only on s + Enter.
    for _ in 0..3 {
        app.on_gitcfg_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    }
    assert_eq!(dlg(&app).draft.rules[0].owner, "bob");
    assert_eq!(app.cfg.git.rules.len(), 3, "saved before the yes");
    key(&mut app, KeyCode::Char('s'));
    let s = screen(&mut app);
    dump("tui-gitcfg-save.txt", &s);
    assert!(s.contains("Rules in order"), "{s}");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.cfg.git.rules.len(), 4);
    assert_eq!(app.cfg.git.rules[0].owner, "bob");
    assert!(config().starts_with(START));
    let s = screen(&mut app);
    dump("tui-gitcfg-saved.txt", &s);
    assert!(s.contains("Saved 4 owners and 4 folder rules"), "{s}");
    // A bad glob is refused at Save.
    let mut d = dlg(&app);
    d.draft.rules[0].pattern = "/Volumes/Private/**".into();
    app.modal = Modal::GitCfg(d);
    key(&mut app, KeyCode::Char('s'));
    assert!(matches!(dlg(&app).step, CfgStep::Main));
    assert!(dlg(&app).note.unwrap().0.contains("vault"));
    assert_eq!(app.cfg.git.rules[0].owner, "bob");
    let _ = std::fs::remove_dir_all(&home);
}
