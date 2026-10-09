//! The way back to the conversation from the panel's other views: a
//! "‹ Conversation" button first in every one (whole on narrow panels),
//! Esc, the current conversation pinned in History, and a dot for replies
//! that came while away.

use crate::app_assistant::{Entry, Who};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;
use crate::panel_views::PanelView;

fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    use crate::creds::CredSource;
    let home = std::env::temp_dir().join(format!("godterm-pview-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut app = crate::app::App::new(crate::config::Config::default(), tx);
    app.cfg.claude_bin = Some(crate::test_stub::true_bin());
    for a in 0..app.accounts.len() {
        app.accounts[a].login.source = Some(CredSource::Keychain);
    }
    app.assistant.show = true;
    app.assistant.focused = true;
    app.assistant.log.push(Entry {
        who: Who::User,
        text: "which tabs are busy".into(),
    });
    app.assistant.log.push(Entry {
        who: Who::Reply,
        text: "Two tabs are working on Bravo.".into(),
    });
    (app, home)
}

fn draw(
    app: &mut crate::app::App,
    t: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) -> String {
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
    let b = t.backend().buffer();
    let mut s = String::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            s.push_str(b[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn term(w: u16) -> ratatui::Terminal<ratatui::backend::TestBackend> {
    ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 40)).unwrap()
}

fn click(app: &mut crate::app::App, a: &UiAction) {
    let r = app
        .last_hits
        .regions
        .iter()
        .rev()
        .find(|r| &r.action == a)
        .unwrap_or_else(|| panic!("no {a:?} on screen"))
        .rect;
    app.on_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: r.x + r.width / 2,
        row: r.y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
}

/// The first toolbar button (row under the panel's title).
fn first_button(app: &crate::app::App) -> Option<(UiAction, ratatui::layout::Rect)> {
    let panel = app.last_hits.find(&UiAction::AssistantFocus)?.rect;
    app.last_hits
        .regions
        .iter()
        .filter(|r| {
            r.rect.y == panel.y + 1
                && r.rect.x > panel.x
                && r.rect.x < panel.x + panel.width
                && r.action != UiAction::AssistantFocus
        })
        .min_by_key(|r| r.rect.x)
        .map(|r| (r.action.clone(), r.rect))
}

fn open_view(app: &mut crate::app::App, v: PanelView) {
    use crate::menus::Cmd;
    match v {
        PanelView::History => app.toggle_assistant_history(),
        PanelView::Admin => app.menu_cmd(Cmd::AssistantAdmin),
        PanelView::Rules => app.menu_cmd(Cmd::AssistantRules),
        PanelView::Prompt => app.menu_cmd(Cmd::AssistantPrompt),
        PanelView::Conversation => {}
    }
    assert_eq!(app.panel_view(), v);
}

#[test]
fn every_view_has_the_way_back_first_and_it_returns_intact() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("back");
    let mut t = term(160);
    let s = draw(&mut app, &mut t);
    assert!(s.contains("Two tabs are working on Bravo."));
    assert!(
        !s.contains("‹ Conversation"),
        "not in the conversation itself"
    );
    for v in [
        PanelView::History,
        PanelView::Admin,
        PanelView::Rules,
        PanelView::Prompt,
    ] {
        open_view(&mut app, v);
        let s = draw(&mut app, &mut t);
        let (a, _) = first_button(&app).unwrap();
        assert_eq!(a, UiAction::AssistantConversation, "{v:?}");
        assert!(s.contains("‹ Conversation"), "{v:?}:\n{s}");
        assert!(s.contains(" New "), "New stays: {v:?}");
        // The title says where you are.
        let title: String = crate::ui_assistant::title_parts(&app, 200)
            .iter()
            .map(|(t, _)| t.as_str())
            .collect();
        assert!(title.contains(&format!("· {} ·", v.name())), "{title}");
        click(&mut app, &UiAction::AssistantConversation);
        assert_eq!(app.panel_view(), PanelView::Conversation, "{v:?}");
        let s = draw(&mut app, &mut t);
        assert!(
            s.contains("Two tabs are working on Bravo.") && s.contains("which tabs are busy"),
            "{v:?}: messages intact"
        );
        assert_eq!(app.assistant.log.len(), 2);
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn esc_goes_back_to_the_conversation() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("esc");
    let esc = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    );
    for v in [
        PanelView::History,
        PanelView::Admin,
        PanelView::Rules,
        PanelView::Prompt,
    ] {
        open_view(&mut app, v);
        app.assistant.focused = true;
        app.on_key(esc);
        assert_eq!(app.panel_view(), PanelView::Conversation, "{v:?}");
        assert!(app.assistant.focused, "{v:?}: still typing to it");
    }
    // In the conversation, Esc gives the keys back (as before).
    app.on_key(esc);
    assert!(!app.assistant.focused && app.assistant.show);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn history_pins_the_current_conversation_and_opens_it_live() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("pin");
    // A saved conversation, and the one going on (saved too).
    let old = crate::assistant_history::ConvLog::new();
    old.write("user", serde_json::json!({"text": "an older question"}));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let cur = crate::assistant_history::ConvLog::new();
    cur.write("user", serde_json::json!({"text": "which tabs are busy"}));
    app.assistant.conv = Some(cur.clone());
    app.toggle_assistant_history();
    let h = app.assistant.history.as_ref().unwrap();
    assert!(h.items.iter().all(|s| s.id != cur.id), "not listed twice");
    assert!(h.items.iter().any(|s| s.id == old.id));
    let mut t = term(160);
    let s = draw(&mut app, &mut t);
    let pin = s
        .lines()
        .position(|l| l.contains("● current"))
        .expect("pinned");
    let older = s
        .lines()
        .position(|l| l.contains("an older question"))
        .unwrap();
    assert!(pin < older, "on top");
    assert!(s.lines().nth(pin).unwrap().contains("which tabs are busy"));
    // A click opens the live conversation, not a read only replay.
    click(&mut app, &UiAction::AssistantConversation);
    assert_eq!(app.panel_view(), PanelView::Conversation);
    assert!(app.assistant.conv.as_ref().is_some_and(|c| c.id == cur.id));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn replies_while_away_show_a_dot() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("dot");
    let mut t = term(160);
    draw(&mut app, &mut t);
    assert_eq!(app.unread_replies(), 0);
    open_view(&mut app, PanelView::Admin);
    app.assistant.log.push(Entry {
        who: Who::Reply,
        text: "Done.".into(),
    });
    let s = draw(&mut app, &mut t);
    assert_eq!(app.unread_replies(), 1);
    assert!(s.contains("‹ Conversation ●"), "{s}");
    app.assistant.log.push(Entry {
        who: Who::Reply,
        text: "Also done.".into(),
    });
    let s = draw(&mut app, &mut t);
    assert!(s.contains("‹ Conversation ● 2"), "{s}");
    click(&mut app, &UiAction::AssistantConversation);
    let s = draw(&mut app, &mut t);
    assert_eq!(app.unread_replies(), 0);
    assert!(s.contains("Also done."));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn narrow_panels_keep_the_button_whole() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = test_app("narrow");
    draw(&mut app, &mut term(160));
    open_view(&mut app, PanelView::History);
    for w in [200u16, 100, 90] {
        let mut t = term(w);
        let s = draw(&mut app, &mut t);
        let (a, r) = first_button(&app).expect("a button");
        assert_eq!(a, UiAction::AssistantConversation, "width {w}");
        let row: String = s
            .lines()
            .nth(r.y as usize)
            .unwrap()
            .chars()
            .skip(r.x as usize)
            .take(r.width as usize)
            .collect();
        let row = row.trim();
        assert!(
            ["‹ Conversation", "‹ Chat", "‹"].contains(&row),
            "width {w}: {row:?}"
        );
        // Narrow: the views go before it does.
        if w == 90 {
            assert!(!s.contains(" Admin "), "Admin overflows into ⋯ first");
        }
        // ⋯ and × still fit after it.
        assert!(app
            .last_hits
            .find(&UiAction::MenuOpen(crate::menus::MenuId::AssistantMore))
            .is_some());
    }
    // The narrowest label when nothing else fits.
    assert_eq!(crate::ui_assistant::back_label(&app, 1), "‹");
    let _ = std::fs::remove_dir_all(&home);
}
