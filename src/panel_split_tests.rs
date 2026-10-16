//! The assistant panel's two areas: the Loops & schedules section (its own
//! title row, lines and divider) and the chat (its own scrollbar). They do
//! not overlap at any height, and the wheel and the bar act on the one
//! under the pointer. Nothing real runs: tabs are stubs, homes are test
//! homes.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::App;
use crate::app_assistant::{Entry, Who};
use crate::config::testing::LOCK as ENV_LOCK;
use crate::hits::UiAction;
use crate::jobs_tests::{loop_row, running, session_loop, setup};

type Term = ratatui::Terminal<ratatui::backend::TestBackend>;

fn sized(w: u16, h: u16) -> Term {
    ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap()
}

fn draw(app: &mut App, t: &mut Term) {
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
}

/// A panel with `loops` session loops and `lines` chat lines.
fn panel(tag: &str, loops: usize, lines: usize) -> (App, std::path::PathBuf) {
    let (mut app, home) = setup(tag);
    app.assistant.show = true;
    app.cfg.assistant.panel = "docked".into();
    for k in 0..loops {
        let t = running(&mut app, 0, &format!("tab {k}"), &format!("sess-{k}"));
        let lp = session_loop(&format!("cc{k:06}"), "*/15 * * * *", &format!("loop {k}"));
        let r = loop_row(&app, 0, t, lp);
        app.loops.push(r);
    }
    for k in 0..lines {
        app.assistant.log.push(Entry {
            who: if k % 2 == 0 { Who::User } else { Who::Reply },
            text: format!("message number {k}"),
        });
    }
    (app, home)
}

fn wheel(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    app.on_mouse_event(MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    });
}

fn press(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    wheel(app, kind, col, row);
}

fn overlap(a: Rect, b: Rect) -> bool {
    a.width > 0
        && a.height > 0
        && b.width > 0
        && b.height > 0
        && a.x < b.x + b.width
        && b.x < a.x + a.width
        && a.y < b.y + b.height
        && b.y < a.y + a.height
}

#[test]
fn the_section_and_the_chat_never_overlap_at_any_height() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for h in [70u16, 50, 40, 30, 24, 20, 16, 14, 12, 10, 8, 6] {
        let (mut app, home) = panel("split-heights", 14, 60);
        let mut t = sized(160, h);
        draw(&mut app, &mut t);
        let Some(p) = app
            .last_hits
            .find(&UiAction::AssistantFocus)
            .map(|r| r.rect)
        else {
            continue;
        };
        let sec = app.sched_ui.rect.get();
        let chat = app.assistant.chat_geo.get().map(|g| g.area);
        if let Some(c) = chat {
            assert!(
                !overlap(sec, c),
                "h={h}: section {sec:?} overlaps chat {c:?}"
            );
            assert!(c.y + c.height <= p.y + p.height, "h={h}: chat inside panel");
            if let Some(g) = app.assistant.input_geo.get() {
                assert!(c.y + c.height <= g.y, "h={h}: chat ends above the composer");
            }
            if sec.height > 0 {
                assert!(
                    sec.y + sec.height <= c.y,
                    "h={h}: section sits above the chat"
                );
            }
        }
        if sec.height > 0 {
            assert!(
                sec.y >= p.y && sec.y + sec.height <= p.y + p.height,
                "h={h}"
            );
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn a_divider_row_closes_the_section_before_the_chat() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = panel("split-divider", 6, 10);
    let mut t = sized(160, 40);
    draw(&mut app, &mut t);
    let sec = app.sched_ui.rect.get();
    let chat = app.assistant.chat_geo.get().unwrap().area;
    assert!(sec.height >= 3, "{sec:?}");
    let buf = t.backend().buffer();
    let div = sec.y + sec.height - 1;
    for x in sec.x..sec.x + sec.width {
        assert_eq!(buf[(x, div)].symbol(), "─", "divider at x={x}");
    }
    assert_eq!(chat.y, div + 1, "the chat starts right under the divider");
    // The title row is a band of its own with the title in it.
    let title: String = (sec.x..sec.x + sec.width)
        .map(|x| buf[(x, sec.y)].symbol().to_string())
        .collect();
    assert!(title.contains("Loops & schedules"), "{title}");
    assert_ne!(buf[(sec.x, sec.y)].bg, buf[(sec.x, sec.y + 1)].bg);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_chat_gets_a_scrollbar_only_when_it_overflows() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = panel("split-bar-fit", 0, 2);
    let mut t = sized(160, 40);
    draw(&mut app, &mut t);
    assert!(
        app.last_hits.find(&UiAction::ChatBar).is_none(),
        "fits: no bar"
    );
    let (mut app, home2) = panel("split-bar-over", 0, 80);
    draw(&mut app, &mut t);
    let g = app.assistant.chat_geo.get().unwrap();
    let panel = app.last_hits.find(&UiAction::AssistantFocus).unwrap().rect;
    assert!(g.total > g.view);
    let bar = app.last_hits.find(&UiAction::ChatBar).unwrap().rect;
    assert_eq!(bar, g.bar());
    assert_eq!(bar.x, g.area.x + g.area.width - 1);
    let buf = t.backend().buffer();
    let glyphs: Vec<&str> = (bar.y..bar.y + bar.height)
        .map(|y| buf[(bar.x, y)].symbol())
        .collect();
    assert!(glyphs.contains(&"█"), "a thumb: {glyphs:?}");
    assert!(glyphs.iter().any(|s| *s != "█"), "a track: {glyphs:?}");
    // At the newest line the thumb is at the bottom.
    assert_eq!(*glyphs.last().unwrap(), "█");
    // The track is the panel's right border, in the panel's frame.
    assert_eq!(bar.x, panel.x + panel.width - 1);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&home2);
}

#[test]
fn the_wheel_goes_to_the_area_under_the_pointer() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = panel("split-wheel", 14, 80);
    let mut t = sized(160, 30);
    draw(&mut app, &mut t);
    let sec = app.sched_ui.rect.get();
    let g = app.assistant.chat_geo.get().unwrap();
    // Over the chat: the chat scrolls back, the section stays.
    let (cx, cy) = (g.area.x + 3, g.area.y + 2);
    wheel(&mut app, MouseEventKind::ScrollUp, cx, cy);
    assert!(app.assistant.chat_off.get() > 0, "the chat scrolled");
    assert_eq!(app.sched_ui.scroll, 0, "the section did not");
    // Over the section (a row and its divider): it scrolls, the chat stays.
    let off = app.assistant.chat_off.get();
    draw(&mut app, &mut t);
    wheel(&mut app, MouseEventKind::ScrollDown, sec.x + 4, sec.y + 2);
    assert!(app.sched_ui.scroll > 0, "the section scrolled");
    assert_eq!(app.assistant.chat_off.get(), off, "the chat did not");
    let before = app.sched_ui.scroll;
    draw(&mut app, &mut t);
    wheel(
        &mut app,
        MouseEventKind::ScrollDown,
        sec.x + 4,
        sec.y + sec.height - 1,
    );
    assert!(app.sched_ui.scroll >= before);
    assert_eq!(app.assistant.chat_off.get(), off);
    // The chat's wheel stops at both ends.
    for _ in 0..200 {
        wheel(&mut app, MouseEventKind::ScrollUp, cx, cy);
    }
    assert_eq!(app.assistant.chat_off.get(), g.total - g.view);
    for _ in 0..200 {
        wheel(&mut app, MouseEventKind::ScrollDown, cx, cy);
    }
    assert_eq!(app.assistant.chat_off.get(), 0);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_wheel_over_the_grid_still_scrolls_the_pane_not_the_panel() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = panel("split-grid", 3, 80);
    let mut t = sized(160, 40);
    draw(&mut app, &mut t);
    let r = app.term_rects[0];
    wheel(&mut app, MouseEventKind::ScrollUp, r.x + 2, r.y + 2);
    assert_eq!(app.assistant.chat_off.get(), 0);
    assert_eq!(app.sched_ui.scroll, 0);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn dragging_the_thumb_scrolls_the_chat_and_a_click_on_the_track_jumps() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = panel("split-drag", 2, 120);
    let mut t = sized(160, 40);
    draw(&mut app, &mut t);
    let g = app.assistant.chat_geo.get().unwrap();
    let bar = g.bar();
    let max = g.total - g.view;
    // Thumb at the bottom; grab it and drag to the top.
    let (ts, tl) = crate::scrollbar::thumb(bar.height, g.total, g.view, g.top(0)).unwrap();
    press(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.x,
        bar.y + ts + tl - 1,
    );
    assert!(app.assistant.bar_drag.is_some());
    press(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        bar.x,
        bar.y,
    );
    assert_eq!(app.assistant.chat_off.get(), max, "dragged to the oldest");
    // Dragging off the bar sideways still follows the row.
    press(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        bar.x - 20,
        bar.y + bar.height + 5,
    );
    assert_eq!(app.assistant.chat_off.get(), 0, "past the bottom: newest");
    press(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        bar.x,
        bar.y,
    );
    assert!(app.assistant.bar_drag.is_none());
    // The drag never moved the section or selected text.
    assert_eq!(app.sched_ui.scroll, 0);
    assert!(app.selection.is_none());
    // A click on the empty track jumps there (near the top).
    draw(&mut app, &mut t);
    press(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.x,
        bar.y,
    );
    press(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        bar.x,
        bar.y,
    );
    assert!(
        app.assistant.chat_off.get() > max / 2,
        "{}",
        app.assistant.chat_off.get()
    );
    // The picture follows: the thumb moved up.
    draw(&mut app, &mut t);
    let buf = t.backend().buffer();
    assert_eq!(buf[(bar.x, bar.y)].symbol(), "█");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_scrolled_chat_keeps_its_lines_as_new_ones_arrive() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = panel("split-anchor", 0, 80);
    let mut t = sized(160, 40);
    draw(&mut app, &mut t);
    let g = app.assistant.chat_geo.get().unwrap();
    for _ in 0..5 {
        wheel(
            &mut app,
            MouseEventKind::ScrollUp,
            g.area.x + 2,
            g.area.y + 2,
        );
    }
    draw(&mut app, &mut t);
    let top_before = app
        .assistant
        .chat_geo
        .get()
        .unwrap()
        .top(app.assistant.chat_off.get());
    for k in 0..4 {
        app.assistant.log.push(Entry {
            who: Who::Reply,
            text: format!("late {k}"),
        });
    }
    draw(&mut app, &mut t);
    let g2 = app.assistant.chat_geo.get().unwrap();
    assert!(g2.total > g.total);
    assert_eq!(
        g2.top(app.assistant.chat_off.get()),
        top_before,
        "same lines in view"
    );
    // At the newest line it keeps following.
    app.assistant.chat_off.set(0);
    draw(&mut app, &mut t);
    app.assistant.log.push(Entry {
        who: Who::Reply,
        text: "newest".into(),
    });
    draw(&mut app, &mut t);
    assert_eq!(app.assistant.chat_off.get(), 0);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn no_wheel_routing_while_a_dialog_is_open_or_the_panel_is_closed() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = panel("split-modal", 4, 80);
    let mut t = sized(160, 40);
    draw(&mut app, &mut t);
    let g = app.assistant.chat_geo.get().unwrap();
    assert_eq!(
        app.panel_region_at(g.area.x + 1, g.area.y + 1),
        Some(crate::app_mouse::PanelArea::Chat)
    );
    let sec = app.sched_ui.rect.get();
    assert_eq!(
        app.panel_region_at(sec.x + 1, sec.y),
        Some(crate::app_mouse::PanelArea::Schedules)
    );
    app.modal = crate::app::Modal::Help;
    assert_eq!(app.panel_region_at(g.area.x + 1, g.area.y + 1), None);
    app.modal = crate::app::Modal::None;
    app.assistant.show = false;
    draw(&mut app, &mut t);
    assert_eq!(app.panel_region_at(g.area.x + 1, g.area.y + 1), None);
    let _ = std::fs::remove_dir_all(&home);
}
