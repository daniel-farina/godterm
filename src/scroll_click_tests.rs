use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::config::testing::LOCK as ENV_LOCK;
use crate::confirm_click_tests::{click, draw, term, test_app};
use crate::hits::UiAction;

fn mouse(app: &mut crate::app::App, kind: MouseEventKind, x: u16, y: u16) {
    app.handle(crate::app::AppEvent::Input(crossterm::event::Event::Mouse(
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    )));
}

fn with_history(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    let (mut app, home) = test_app(tag);
    let t = app.panes[0].cur_mut();
    t.state = crate::pane::PaneState::Running;
    let mut p = t.parser.lock().unwrap();
    for i in 1..=400 {
        p.process(format!("line {i}\r\n").as_bytes());
    }
    drop(p);
    (app, home)
}

fn body_point(app: &crate::app::App, p: usize) -> (u16, u16) {
    let r = app.term_rects[p];
    (r.x + r.width / 2, r.y + r.height / 2)
}

fn wheel_up_scrolls(
    app: &mut crate::app::App,
    t: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    what: &str,
) {
    draw(app, t);
    let (x, y) = body_point(app, 0);
    let before = app.panes[0].cur().scroll;
    mouse(app, MouseEventKind::ScrollUp, x, y);
    assert!(
        app.panes[0].cur().scroll > before,
        "{what}: the wheel scrolls the pane ({} -> {})",
        before,
        app.panes[0].cur().scroll
    );

    for _ in 0..40 {
        mouse(app, MouseEventKind::ScrollDown, x, y);
    }
    assert_eq!(app.panes[0].cur().scroll, 0, "{what}");
}

#[test]
fn the_wheel_scrolls_after_a_tab_click_and_a_pane_click() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = with_history("wheel");
    let cwd = app.panes[0].cur().cwd.clone();
    app.panes[0].add_tab(cwd);
    app.panes[0].active = 0;
    let mut t = term();

    wheel_up_scrolls(&mut app, &mut t, "before a click");

    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 1));
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 0));
    assert_eq!(app.panes[0].active, 0);
    wheel_up_scrolls(&mut app, &mut t, "after a tab click");

    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 1));
    app.mem_ticked = std::time::Instant::now() - std::time::Duration::from_secs(5);
    app.memory_tick();
    draw(&mut app, &mut t);
    click(&mut app, &UiAction::SelectTab(0, 0));
    app.mem_ticked = std::time::Instant::now() - std::time::Duration::from_secs(5);
    app.memory_tick();
    wheel_up_scrolls(&mut app, &mut t, "after a tab click and the memory tick");

    draw(&mut app, &mut t);
    let (x, y) = body_point(&app, 0);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
    wheel_up_scrolls(&mut app, &mut t, "after a pane click");

    draw(&mut app, &mut t);
    let (x1, y1) = body_point(&app, 1);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x1, y1);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x1, y1);
    wheel_up_scrolls(&mut app, &mut t, "after a click in another pane");

    for _ in 0..30 {
        let cwd = app.panes[0].cur().cwd.clone();
        app.panes[0].add_tab(cwd);
    }
    app.panes[0].active = 0;
    app.panes[0].sidebar_scroll = crate::slot::FOLLOW;
    draw(&mut app, &mut t);
    let r = app.last_hits.find(&UiAction::SelectTab(0, 1)).unwrap().rect;
    mouse(&mut app, MouseEventKind::ScrollDown, r.x + 2, r.y);
    assert_ne!(app.panes[0].sidebar_scroll, crate::slot::FOLLOW);
    assert!(app.panes[0].sidebar_scroll > 0, "the list scrolled");
    let _ = std::fs::remove_dir_all(&home);
}

fn busy_list(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    let (mut app, home) = with_history(tag);
    let cwd = app.panes[0].cur().cwd.clone();
    for _ in 1..40 {
        app.panes[0].add_tab(cwd.clone());
    }
    let slot = &mut app.panes[0];
    for name in ["g1", "g2"] {
        slot.groups.push(crate::tab_groups::TabGroup {
            name: name.into(),
            color: None,
            collapsed: false,
        });
    }
    for (k, t) in slot.tabs.iter_mut().enumerate() {
        let word = if k % 7 == 3 { "drop" } else { "keep" };
        t.custom_name = Some(format!("{word} {:02}", 99 - k));
        t.group = match k % 5 {
            1 => Some("g1".into()),
            2 => Some("g2".into()),
            _ => None,
        };
        t.pinned = k == 10 || k == 30;
    }
    slot.sort = crate::tab_groups::TabSort::Name;
    slot.filter = "keep".into();
    slot.active = 0;
    slot.sidebar_scroll = crate::slot::FOLLOW;
    (app, home)
}

fn list_rect(app: &crate::app::App, p: usize) -> ratatui::layout::Rect {
    let &(_, start, _) = app
        .last_hits
        .tab_lists
        .iter()
        .find(|l| l.0 == p)
        .expect("the list is drawn");
    app.last_hits.regions[start].rect
}

fn top_row(app: &crate::app::App, p: usize) -> crate::tab_groups::Row {
    use crate::tab_groups::Row;
    app.last_hits
        .regions
        .iter()
        .filter_map(|r| match r.action {
            UiAction::SelectTab(s, t) if s == p => Some((r.rect.y, Row::Tab(t))),
            UiAction::GroupHeader(s, g) if s == p => Some((r.rect.y, Row::Group(g))),
            _ => None,
        })
        .min_by_key(|(y, _)| *y)
        .map(|(_, r)| r)
        .unwrap()
}

fn first_shown(app: &crate::app::App) -> usize {
    let rows = app.list_rows(0, true);
    let top = top_row(app, 0);
    rows.iter().position(|r| *r == top).unwrap()
}

#[test]
fn the_list_wheel_moves_the_window_shown_by_one_row() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = busy_list("listwheel");
    let mut t = term();

    draw(&mut app, &mut t);
    let rows = app.list_rows(0, true);
    let crate::tab_groups::Row::Tab(deep) = rows[rows.len() - 4] else {
        panic!("a tab")
    };
    app.panes[0].active = deep;
    draw(&mut app, &mut t);
    let bar = list_rect(&app, 0);
    let (follow_first, max) = app.panes[0].list_window.get();
    assert!(follow_first > 0 && max > 0, "the list overflows");
    assert_eq!(first_shown(&app), follow_first);

    let (x, y) = (bar.x + 4, bar.y + bar.height / 2);
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    draw(&mut app, &mut t);
    assert_eq!(first_shown(&app), follow_first - 1, "one row up");

    for _ in 0..200 {
        mouse(&mut app, MouseEventKind::ScrollDown, x, y);
    }
    draw(&mut app, &mut t);
    assert_eq!(app.panes[0].sidebar_scroll, max);
    assert_eq!(first_shown(&app), max);
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    draw(&mut app, &mut t);
    assert_eq!(first_shown(&app), max - 1);

    let header = app
        .last_hits
        .regions
        .iter()
        .find(|r| matches!(r.action, UiAction::GroupHeader(0, _)))
        .map(|r| (r.rect.x + 1, r.rect.y));
    let second = app
        .last_hits
        .regions
        .iter()
        .find(|r| matches!(r.action, UiAction::SelectTab(0, _)) && r.rect.height == 2)
        .map(|r| (r.rect.x + r.rect.width / 2, r.rect.y + 1))
        .expect("two line rows");
    let mut points = vec![
        (bar.x + 2, bar.y + 1),
        (bar.x + bar.width - 8, bar.y + 1),
        second,
        (bar.x + bar.width - 1, bar.y + bar.height / 2),
        (bar.x + bar.width - 3, second.1 - 1),
    ];
    points.extend(header);
    for (px, py) in points {
        draw(&mut app, &mut t);
        let before = first_shown(&app);
        let pane_before = app.panes[0].cur().scroll;
        mouse(&mut app, MouseEventKind::ScrollUp, px, py);
        draw(&mut app, &mut t);
        assert_eq!(first_shown(&app), before - 1, "wheel at ({px},{py})");
        assert_eq!(
            app.panes[0].cur().scroll,
            pane_before,
            "the pane stays at ({px},{py})"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_wheel_at_the_list_edge_goes_to_the_side_it_is_on() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = busy_list("listedge");
    let mut t = term();

    draw(&mut app, &mut t);
    let rows = app.list_rows(0, true);
    let mid = rows[rows.len() / 2..]
        .iter()
        .find_map(|r| match *r {
            crate::tab_groups::Row::Tab(t) => Some(t),
            _ => None,
        })
        .unwrap();
    app.panes[0].active = mid;
    {
        let tab = app.panes[0].cur_mut();
        tab.state = crate::pane::PaneState::Running;
        let mut p = tab.parser.lock().unwrap();
        for i in 1..=200 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
    }
    draw(&mut app, &mut t);
    let bar = list_rect(&app, 0);
    let term_r = app.term_rects[0];
    let y = term_r.y + term_r.height / 2;
    assert_eq!(
        bar.x + bar.width,
        term_r.x,
        "the body starts right after the list"
    );

    let (first, _) = app.panes[0].list_window.get();
    mouse(&mut app, MouseEventKind::ScrollUp, term_r.x, y);
    draw(&mut app, &mut t);
    assert!(app.panes[0].cur().scroll > 0, "the pane scrolled");
    assert_eq!(
        app.panes[0].sidebar_scroll,
        crate::slot::FOLLOW,
        "the list did not"
    );
    assert_eq!(app.panes[0].list_window.get().0, first);

    let scroll = app.panes[0].cur().scroll;
    mouse(
        &mut app,
        MouseEventKind::ScrollDown,
        bar.x + bar.width - 1,
        y,
    );
    draw(&mut app, &mut t);
    assert_eq!(app.panes[0].cur().scroll, scroll, "the pane did not");
    assert_eq!(app.panes[0].list_window.get().0, first + 1, "the list did");

    let bar1 = list_rect(&app, 1);
    let s1 = app.panes[1].cur().scroll;
    mouse(
        &mut app,
        MouseEventKind::ScrollUp,
        bar1.x + 3,
        bar1.y + bar1.height - 3,
    );
    assert_eq!(app.panes[1].sidebar_scroll, crate::slot::FOLLOW);
    assert_eq!(app.panes[1].cur().scroll, s1);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn wheel_bursts_are_taken_as_one() {
    use crossterm::event::{Event, KeyModifiers};
    let ev = |kind, column| MouseEvent {
        kind,
        column,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let first = ev(MouseEventKind::ScrollUp, 3);

    let mut q: std::collections::VecDeque<Event> = (0..4)
        .map(|_| Event::Mouse(ev(MouseEventKind::ScrollUp, 3)))
        .chain([Event::Mouse(ev(MouseEventKind::ScrollDown, 3))])
        .collect();
    let (n, next) = crate::app_mouse::coalesce_wheel(&first, &mut || q.pop_front());
    assert_eq!(n, 5);
    assert_eq!(next, Some(Event::Mouse(ev(MouseEventKind::ScrollDown, 3))));

    let (n, next) = crate::app_mouse::coalesce_wheel(&first, &mut || None);
    assert_eq!((n, next), (1, None));

    let mut q: std::collections::VecDeque<Event> =
        [Event::Mouse(ev(MouseEventKind::ScrollUp, 9))].into();
    let (n, next) = crate::app_mouse::coalesce_wheel(&first, &mut || q.pop_front());
    assert_eq!(n, 1);
    assert!(next.is_some());

    let (n, _) = crate::app_mouse::coalesce_wheel(&first, &mut || {
        Some(Event::Mouse(ev(MouseEventKind::ScrollUp, 3)))
    });
    assert_eq!(n, crate::app_mouse::WHEEL_BURST);

    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = busy_list("burst");
    let mut t = term();
    draw(&mut app, &mut t);
    let (x, y) = body_point(&app, 0);
    let m = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle(crate::app::AppEvent::Wheel(m, 5));
    assert_eq!(app.panes[0].cur().scroll, 5);
    assert_eq!(app.wheel_n, 1, "back to one after the burst");
    app.wheel_last = None;
    app.handle(crate::app::AppEvent::Wheel(m, 1));
    assert_eq!(app.panes[0].cur().scroll, 8, "a lone notch: three lines");

    let bar = list_rect(&app, 0);
    app.panes[0].sidebar_scroll = 0;
    let max = app.panes[0].list_window.get().1;
    app.panes[0].list_window.set((0, max));
    let down = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: bar.x + 3,
        row: bar.y + 6,
        modifiers: KeyModifiers::NONE,
    };
    app.handle(crate::app::AppEvent::Wheel(down, 4));
    assert_eq!(app.panes[0].sidebar_scroll, 4);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_history_limit_waits_for_the_tab_being_scrolled_or_shown() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = with_history("histhold");
    let cwd = app.panes[0].cur().cwd.clone();
    app.panes[0].add_tab(cwd);
    app.panes[0].active = 0;
    let mut t = term();
    draw(&mut app, &mut t);
    let tick = |app: &mut crate::app::App| {
        app.mem_ticked = std::time::Instant::now() - std::time::Duration::from_secs(5);
        app.memory_tick();
    };
    tick(&mut app);
    let had = app.panes[0].tabs[0].history_lines();

    let (x, y) = body_point(&app, 0);
    for _ in 0..5 {
        mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    }
    let scroll = app.panes[0].cur().scroll;
    assert!(scroll > 0);
    app.cfg.scrollback_lines = had + 500;
    tick(&mut app);
    assert_eq!(
        app.panes[0].tabs[0].history_lines(),
        had,
        "not while scrolled"
    );
    assert_eq!(app.panes[0].cur().scroll, scroll, "the view did not move");

    app.panes[0].cur_mut().reset_scroll();
    tick(&mut app);
    assert_eq!(app.panes[0].tabs[0].history_lines(), had + 500);

    app.cfg.scrollback_lines = had;
    tick(&mut app);
    assert_eq!(
        app.panes[0].tabs[0].history_lines(),
        had + 500,
        "not on screen"
    );
    app.panes[0].active = 1;
    tick(&mut app);
    assert!(
        app.panes[0].tabs[0].history_lines() <= had,
        "applied off screen"
    );
    let _ = std::fs::remove_dir_all(&home);
}
