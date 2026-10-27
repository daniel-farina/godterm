use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::hits::{button, contains, text, List, UiAction};
use crate::pane::{Activity, Pane};
use crate::prompt::Choice;
use crate::sessions::snippet;
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};

#[allow(clippy::too_many_arguments)]
fn link(
    buf: &mut Buffer,
    app: &App,
    x: u16,
    y: u16,
    limit: u16,
    s: &str,
    style: Style,
    action: UiAction,
    hint: &str,
) -> u16 {
    if x >= limit {
        return x;
    }
    let w = (s.chars().count() as u16).min(limit - x);
    let rect = Rect::new(x, y, w, 1);
    let hovered = app.mouse_pos.is_some_and(|(c, r)| contains(rect, c, r));
    let st = if hovered {
        style
            .bg(SEL_BG)
            .fg(theme::SAND)
            .add_modifier(Modifier::BOLD)
    } else {
        style
    };
    let end = text(buf, x, y, limit, s, st);
    app.hits
        .borrow_mut()
        .add(Rect::new(x, y, end - x, 1), action, hint);
    end
}

#[allow(clippy::too_many_arguments)]
fn btn(
    buf: &mut Buffer,
    app: &App,
    x: u16,
    y: u16,
    limit: u16,
    label: &str,
    action: UiAction,
    hint: &str,
    accent: Color,
) -> u16 {
    button(
        buf,
        &mut app.hits.borrow_mut(),
        app.mouse_pos,
        x,
        y,
        limit,
        label,
        action,
        hint,
        accent,
    )
}

const MOUSE_HINT_W: u16 = 32;

pub struct MenuItem {
    pub label: String,
    pub action: UiAction,
    pub hint: String,
    pub accent: Color,

    pub on: bool,

    pub lit: bool,

    pub dim: bool,

    pub prio: u8,
}

pub fn badge(n: usize) -> String {
    if n > 9 {
        "9+".into()
    } else {
        format!("{n:>2}")
    }
}

pub fn menu_items(app: &App) -> Vec<MenuItem> {
    let waiting = app.waiting_tabs().len();
    let loops = app.loops.len();
    let hidden = app.hidden_count();
    let check = |b: bool| if b { "[x]" } else { "[ ]" };
    let mk = |label: String, action: UiAction, hint: &str, accent: Color, prio: u8| MenuItem {
        lit: match &action {
            UiAction::Home => {
                app.view == crate::app::View::Grid
                    && app.modal == crate::app::Modal::None
                    && !app.zoom
            }
            UiAction::Key(c) => app.active_menu_key() == Some(*c),
            UiAction::MenuOpen(id) => {
                matches!(&app.modal, crate::app::Modal::Menu(o) if o.id == *id)
            }
            _ => false,
        },
        label,
        action,
        hint: hint.to_string(),
        accent,
        on: false,
        dim: false,
        prio,
    };
    use crate::menus::MenuId;
    let mut v = vec![
        mk("⌂ Grid".into(), UiAction::Home, "Home: the grid of all your sessions (Esc, Ctrl-a g)", theme::SAGE, 80),
        mk("Tabs ▾".into(), UiAction::MenuOpen(MenuId::Tabs), "New, close, rename, move and duplicate tabs, broadcast, hidden panes", theme::SLATE, 50),
        mk("View ▾".into(), UiAction::MenuOpen(MenuId::View), "Overview, dashboard, sessions, loops, approvals, layout, zoom, pages", theme::SLATE, 60),
        mk(format!("Approvals {}", badge(waiting)), UiAction::Key('y'), "Tabs waiting for approval, approve or deny each (Ctrl-a y)", if waiting > 0 { theme::SAND } else { theme::SLATE }, 90),
        mk("Voice ▾".into(), UiAction::MenuOpen(MenuId::Voice), "Voice mode, the assistant, wake word training, voice log", theme::MAUVE, 70),
        mk(if app.voice.muted { "● MUTED".into() } else { "  Mic  ".into() }, UiAction::MuteToggle, if app.voice.muted { "Muted: nothing is heard or sent. Click (or Ctrl-a X) to unmute" } else { "Mute the microphone (Ctrl-a X); Ctrl-a space talks" }, theme::MAUVE, 100),
        mk("Settings ▾".into(), UiAction::MenuOpen(MenuId::Settings), "Settings, memory saver, privacy, emails, folders, permissions, doctor, help, tour, quit", theme::SLATE, 100),
    ];
    let _ = (loops, hidden, check);
    for it in &mut v {
        match &it.action {
            UiAction::Key('Z') => it.on = app.memory_saver_on(),
            UiAction::Key('E') => it.on = app.privacy(),
            UiAction::Key('y') => it.dim = waiting == 0,
            UiAction::Key('@') => it.dim = loops == 0,
            UiAction::ShowHidden => it.dim = hidden == 0,
            _ => {}
        }
    }
    v
}

pub fn draw_menu_bar(f: &mut Frame, app: &App, area: Rect) {
    let buf = f.buffer_mut();
    for x in area.x..area.x + area.width {
        if let Some(c) = buf.cell_mut((x, area.y)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(BAR_BG));
        }
    }
    let limit = area.x + area.width;
    let y = area.y;
    let mut x = text(
        buf,
        area.x,
        y,
        limit,
        " ◆ ",
        Style::default().fg(theme::SAGE).bg(BAR_BG),
    );
    x = text(
        buf,
        x,
        y,
        limit,
        "God",
        Style::default()
            .fg(FG)
            .bg(BAR_BG)
            .add_modifier(Modifier::BOLD),
    );
    x = text(
        buf,
        x,
        y,
        limit,
        "Term ",
        Style::default()
            .fg(theme::SAGE)
            .bg(BAR_BG)
            .add_modifier(Modifier::BOLD),
    );

    app.hits.borrow_mut().add(
        Rect::new(area.x, y, x.saturating_sub(area.x), 1),
        UiAction::Home,
        "Home: back to all sessions (Esc, Ctrl-a g)",
    );
    x += 1;

    let badge = app.update_badge();
    let full_limit = limit;
    let limit = match &badge {
        Some(b) => limit.saturating_sub(crate::ui_updates::chip_width(b) + 1),
        None => limit,
    };
    let items = menu_items(app);

    let w = |l: &str| unicode_width::UnicodeWidthStr::width(l) as u16 + 3;
    let mut keep: Vec<bool> = vec![true; items.len()];
    let total = |keep: &[bool]| -> u16 {
        items
            .iter()
            .zip(keep)
            .filter(|(_, k)| **k)
            .map(|(i, _)| w(&i.label))
            .sum::<u16>()
            + if keep.iter().all(|k| *k) { 0 } else { 4 }
    };

    let show_hint = total(&keep) + MOUSE_HINT_W <= limit.saturating_sub(x);
    let room = limit
        .saturating_sub(x)
        .saturating_sub(if show_hint { MOUSE_HINT_W } else { 0 });
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| (items[i].prio, std::cmp::Reverse(i)));
    for i in order {
        if total(&keep) <= room {
            break;
        }
        if items[i].prio < 100 {
            keep[i] = false;
        }
    }
    let mut overflow = vec![];
    for (i, it) in items.iter().enumerate() {
        if !keep[i] {
            overflow.push((it.label.trim().to_string(), it.action.clone()));
            continue;
        }
        let before = x;
        let accent = if it.on { FG } else { it.accent };
        let start = x;
        x = btn(
            buf,
            app,
            x,
            y,
            limit,
            &it.label,
            it.action.clone(),
            &it.hint,
            accent,
        );

        if it.action == UiAction::MuteToggle && app.voice.muted {
            for cx in start..x.saturating_sub(1) {
                if let Some(c) = buf.cell_mut((cx, y)) {
                    c.set_style(
                        Style::default()
                            .fg(Color::Rgb(255, 245, 240))
                            .bg(theme::MUTED_RED)
                            .add_modifier(Modifier::BOLD),
                    );
                }
            }
        }

        if it.action == UiAction::Key('y') {
            if let Some(st) = crate::attention::approvals_style(app) {
                for cx in start..x.saturating_sub(1) {
                    if let Some(c) = buf.cell_mut((cx, y)) {
                        c.set_style(st);
                    }
                }
            }
        }
        let span = before..x.saturating_sub(1);
        for cx in span {
            if let Some(c) = buf.cell_mut((cx, y)) {
                if it.lit {
                    c.set_style(
                        c.style()
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    );
                }
                if it.on {
                    c.set_bg(Color::Rgb(58, 62, 58));
                }
                if it.dim {
                    c.set_fg(FAINT);
                }
            }
        }
    }
    if !overflow.is_empty() {
        x = btn(
            buf,
            app,
            x,
            y,
            limit,
            "»",
            UiAction::MenuOverflow,
            "More buttons",
            theme::SLATE,
        );
    }
    *app.menu_overflow.borrow_mut() = overflow;

    let m = if app.mouse_capture {
        "mouse on, C-a M to select text "
    } else {
        "mouse off, C-a M to click again "
    };
    let mw = m.chars().count() as u16;
    if show_hint && x + mw <= limit {
        text(
            buf,
            limit - mw,
            y,
            limit,
            m,
            Style::default().fg(FAINT).bg(BAR_BG),
        );
    }
    if let Some(b) = badge {
        let cw = crate::ui_updates::chip_width(&b);
        if full_limit >= area.x + cw {
            crate::ui_updates::draw_chip(buf, app, full_limit - cw, y, &b);
        }
    }
}

fn tab_text(ti: usize, t: &Pane) -> String {
    let mark = match t.activity {
        Activity::Permission => "!",
        Activity::Working => "*",
        _ => "",
    };
    format!(" {}:{}{mark} ", ti + 1, snippet(&t.name(), 16))
}

pub const CONTROLS_W: u16 = 9;

pub fn window_controls(
    buf: &mut Buffer,
    app: &App,
    i: usize,
    y: u16,
    right: u16,
    strip: bool,
) -> u16 {
    let start = right.saturating_sub(CONTROLS_W);
    let (min_glyph, min_hint) = if strip {
        ("▭", "Restore this pane to full size (Ctrl-a _)")
    } else {
        (
            "_",
            "Minimize: collapse this pane to a one row strip; the panes beside it take the space (Ctrl-a _)",
        )
    };
    let (max_glyph, max_hint) = if app.zoom {
        ("▣", "Restore: show every pane again (Ctrl-a z)")
    } else {
        ("□", "Maximize: zoom this pane to full size (Ctrl-a z)")
    };
    let close_hint = "Close this window: all its tabs (asks first; Undo brings them back). The account stays logged in";
    let mut x = start;
    for (glyph, action, hint, hover_fg) in [
        (min_glyph, UiAction::FoldAccount(i), min_hint, FG),
        (max_glyph, UiAction::Zoom(i), max_hint, FG),
        ("×", UiAction::CloseWindow(i), close_hint, theme::CLAY),
    ] {
        let rect = Rect::new(x, y, 3, 1);
        let hovered = app.mouse_pos.is_some_and(|(c, r)| contains(rect, c, r));
        let st = if hovered {
            Style::default()
                .fg(hover_fg)
                .bg(SEL_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::SLATE)
        };
        text(buf, x, y, x + 3, &format!(" {glyph} "), st);
        app.hits.borrow_mut().add(rect, action, hint);
        x += 3;
    }
    start
}

pub const CARETS_W: u16 = 6;

fn width_carets(
    buf: &mut Buffer,
    app: &App,
    i: usize,
    y: u16,
    width: u16,
    right: u16,
    label_end: u16,
) -> u16 {
    if width < 24 || app.pane_col(i).is_none() || right < CARETS_W + 1 {
        return right;
    }

    let resized = app
        .last_arranged
        .as_ref()
        .is_some_and(|a| app.ratios.contains_key(&a.shape));
    if !resized && right - CARETS_W - 1 < label_end {
        return right;
    }
    let cx = right - CARETS_W;
    let hint = |grow: bool| -> String {
        let name = if grow { "Wider" } else { "Narrower" };
        let now = app.last_arranged.as_ref().and_then(|a| {
            let pos = app.visible_panes().iter().position(|&p| p == i)?;
            a.placed
                .iter()
                .find(|p| p.index == pos)
                .map(|p| p.rect.width)
        });
        match (app.pane_width_after(i, grow), now) {
            (Some(w), Some(n)) if w != n => format!(
                "{name}: {w} columns, the pane beside it gives the space (Shift-click or double click: equal widths)"
            ),
            _ => format!(
                "Already as {} as it goes (Shift-click or double click: equal widths)",
                if grow { "wide" } else { "narrow" }
            ),
        }
    };

    for (r, s, grow) in [
        (Rect::new(cx, y, 3, 1), " < ", false),
        (Rect::new(cx + 3, y, 3, 1), " > ", true),
    ] {
        let hovered = app.mouse_pos.is_some_and(|(c, row)| contains(r, c, row));
        let st = if hovered {
            Style::default()
                .fg(FG)
                .bg(SEL_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::SLATE)
        };
        text(buf, r.x, r.y, r.x + r.width, s, st);
        app.hits
            .borrow_mut()
            .add(r, UiAction::PaneWidth(i, grow), hint(grow));
    }
    cx.saturating_sub(1)
}

#[allow(clippy::too_many_arguments)]

pub fn short_state(state: &str) -> String {
    match state {
        "needs approval" => "approval".into(),
        s => s.split_whitespace().next().unwrap_or("").to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn pane_header(
    buf: &mut Buffer,
    app: &App,
    i: usize,
    area: Rect,
    focused: bool,
    color: Color,
    state: (String, Color),
    scroll: usize,
    show_tabs: bool,
    list_folded: bool,
) {
    let y = area.y;
    let slot = &app.panes[i];
    let left = area.x + 1;
    let right = area.x + area.width.saturating_sub(1);
    if right <= left + 4 {
        return;
    }

    app.hits.borrow_mut().add(
        Rect::new(left, y, right - left, 1),
        UiAction::PaneHeader(i),
        "Drag onto another pane to swap them",
    );

    let (state_text, state_color) = state;

    type Item<'a> = (String, Option<UiAction>, &'a str, Color, u8, Option<String>);
    let mut items: Vec<Item> = vec![];
    if scroll > 0 {
        items.push((
            format!(" scroll {scroll} "),
            None,
            "",
            theme::SAND,
            5,
            Some(format!(" ↑{scroll} ")),
        ));
    }
    if let Some(a) = slot.account {
        let badge = crate::config::permission_badge(&app.cfg.mode_for(a));
        let color = if badge == "bypass" {
            Color::Rgb(160, 92, 78)
        } else {
            FAINT
        };
        items.push((
            badge.to_string(),
            Some(UiAction::AccountMenu(i)),
            "Permission mode of this account; click to change",
            color,
            6,
            None,
        ));
    }

    if let Some((a, l)) = app.move_suggestion(i) {
        let name = app.account_cfg(a).display().to_string();
        items.push((
            format!("Move to {name} ({l:.0}% left)"),
            Some(UiAction::SuggestMove(i, a)),
            "This account is nearly out: continue this conversation there",
            theme::SAND,
            4,
            Some(format!("→ {} ({l:.0}%)", crate::paths::middle(&name, 10))),
        ));
    }

    if let Some(d) = app
        .direct
        .on
        .as_ref()
        .filter(|d| slot.tabs.iter().any(|t| t.uid == d.uid))
    {
        let live = crate::app_direct::mic_state(app);
        let here = slot.cur().uid == d.uid;
        let mut label = String::from("● mic");
        if !here {
            label.push_str(&format!(": {}", crate::paths::middle(&d.name, 14)));
        }

        let roomy = area.width >= 70;
        if !roomy {
            label = "● mic".into();
        }
        let full = match live.filter(|_| roomy) {
            Some(l) => format!("{label} · {l}"),
            None => label,
        };
        items.push((
            full,
            Some(UiAction::MicMenu(i)),
            "The mic is here (direct mode): click to move it to another tab or back to the assistant",
            if live == Some("muted") {
                theme::MUTED_RED
            } else {
                theme::SAGE
            },
            9,
            None,
        ));
    }
    items.push((
        "restart".into(),
        Some(UiAction::Restart(i)),
        "Restart claude in this tab, resuming its session (Ctrl-a r)",
        theme::SLATE,
        1,
        None,
    ));

    let counts = if app.tab_pos(i) == crate::slot::TabPos::Top && !show_tabs {
        vec![]
    } else {
        app.hidden_label_counts(i, show_tabs || list_folded)
    };
    if let Some((first, _)) = counts.first() {
        items.push((
            format!(" {} ", crate::tab_labels::summary(&counts, false)),
            None,
            "",
            first.color(),
            3,
            Some(format!(" {} ", crate::tab_labels::summary(&counts, true))),
        ));
    }
    let short_state = short_state(&state_text);
    items.push((
        format!(" {state_text} "),
        None,
        "",
        state_color,
        8,
        Some(format!(" {short_state} ")),
    ));

    let right = window_controls(buf, app, i, y, right, false).saturating_sub(1);
    if right <= left + 4 {
        return;
    }

    let label_w = slot
        .account
        .map(|a| app.account_cfg(a).display().chars().count() + 2)
        .unwrap_or(12) as u16;
    let folder_w: u16 = if slot.account.is_some() && app.cfg.show_path {
        13
    } else {
        0
    };

    let email_w: u16 = slot
        .account
        .and_then(|a| app.email_label(a, 200))
        .map(|e| e.chars().count().min(18) as u16 + 3)
        .unwrap_or(0);
    let left_need = 6 + label_w + folder_w + email_w + 2 + if show_tabs { 12 } else { 10 };

    let right = width_carets(buf, app, i, y, area.width, right, left + left_need);
    let avail = (right - left).saturating_sub(left_need);
    let iw = |it: &Item| it.0.chars().count() as u16 + if it.1.is_some() { 3 } else { 0 };
    loop {
        let width: u16 = items.iter().map(iw).sum();
        if width <= avail {
            break;
        }

        let mut order: Vec<usize> = (0..items.len()).filter(|&k| items[k].4 < 9).collect();
        order.sort_by_key(|&k| items[k].4);
        let Some(&k) = order.first() else { break };
        match items[k].5.take() {
            Some(short) => items[k].0 = short,
            None => {
                items.remove(k);
            }
        }
    }
    let right_items: Vec<(String, Option<UiAction>, &str, Color)> = items
        .into_iter()
        .map(|(a, b, c, d, _, _)| (a, b, c, d))
        .collect();
    let width: u16 = right_items
        .iter()
        .map(|(t, a, _, _)| t.chars().count() as u16 + if a.is_some() { 3 } else { 0 })
        .sum();
    let rstart = right.saturating_sub(width).max(left);
    let mut x = rstart;
    for (t, a, hint, c) in &right_items {
        x = match a {
            Some(UiAction::AccountMenu(_))
            | Some(UiAction::MicMenu(_))
            | Some(UiAction::CloseWindow(_)) => {
                let end = link(
                    buf,
                    app,
                    x,
                    y,
                    right,
                    &format!(" {t} "),
                    Style::default().fg(*c),
                    a.clone().unwrap(),
                    hint,
                );
                end + 1
            }
            Some(a) => btn(buf, app, x, y, right, t, a.clone(), hint, *c),
            None => text(buf, x, y, right, t, Style::default().fg(*c)),
        };
    }
    let limit = rstart.saturating_sub(1);

    let left = if slot.account.is_some() {
        link(
            buf,
            app,
            left,
            y,
            limit,
            "◂",
            Style::default().fg(theme::SLATE),
            UiAction::FoldSideways(i),
            "Collapse sideways to a narrow column; the panes beside it take the width (Ctrl-a \\)",
        )
    } else {
        left
    };
    let mut x = link(
        buf,
        app,
        left,
        y,
        limit,
        &format!(" {} ", i + 1),
        Style::default().fg(if focused { color } else { DIM }),
        UiAction::FocusPane(i),
        "Focus this pane (Ctrl-a 1..4)",
    );
    let label = slot
        .account
        .map(|a| format!("{} ▾", app.account_cfg(a).display()))
        .unwrap_or_else(|| "no account ▾".into());
    x = link(
        buf,
        app,
        x,
        y,
        limit,
        &label,
        Style::default()
            .fg(if focused { FG } else { DIM })
            .add_modifier(if focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        UiAction::AccountMenu(i),
        "Account menu: switch account, log in or out, tab position",
    );

    if let Some(b) = slot
        .account
        .and_then(|a| app.account_cfg(a).harness().badge())
    {
        x = text(
            buf,
            x,
            y,
            limit,
            &format!(" {b}"),
            Style::default()
                .fg(theme::STONE)
                .add_modifier(Modifier::BOLD),
        );
    }

    if let Some(by) = slot
        .tabs
        .get(slot.active)
        .and_then(|t| t.web_sizer.as_deref())
    {
        x = text(
            buf,
            x,
            y,
            limit,
            &format!(" · sized by {by}"),
            Style::default().fg(theme::SAND),
        );
    }

    if let Some(a) = slot.account {
        let room = limit
            .saturating_sub(x)
            .saturating_sub(if show_tabs { 30 } else { 2 }) as usize;
        let path = app
            .cfg
            .show_path
            .then(|| crate::paths::tilde(&app.tab_dir(i, slot.active)));
        let email = app.email_label(a, 200);
        let (p, e) = crate::paths::header_fit(path.as_deref(), email.as_deref(), room);
        if let Some(p) = p {
            x = text(buf, x, y, limit, " · ", Style::default().fg(FAINT));
            x = link(
                buf,
                app,
                x,
                y,
                limit,
                &p,
                Style::default().fg(DIM),
                UiAction::PathClick(i),
                "Click: copy the folder · double click: open in Finder · right click: more",
            );
        }
        if let Some(e) = e {
            x = text(
                buf,
                x,
                y,
                limit,
                &format!(" · {e}"),
                Style::default().fg(FAINT),
            );
        }
    }
    x = text(buf, x, y, limit, " │", Style::default().fg(FAINT));
    let x = if show_tabs {
        for (ti, t) in slot.tabs.iter().enumerate() {
            let active = ti == slot.active;
            let mut st = Style::default().fg(if active { FG } else { DIM });
            if active {
                st = st.bg(SEL_BG).add_modifier(Modifier::BOLD);
            }
            if t.activity == Activity::Permission {
                st = st.fg(theme::SAND);
            }
            x = link(
                buf,
                app,
                x,
                y,
                limit,
                &tab_text(ti, t),
                st,
                UiAction::SelectTab(i, ti),
                "Switch to this tab · ✎ rename · double click: move and options · right click: options",
            );
            if active {
                x = link(
                    buf,
                    app,
                    x,
                    y,
                    limit,
                    "✎",
                    Style::default().fg(DIM),
                    UiAction::RenameTab(i, ti),
                    "Rename this tab (Ctrl-a R)",
                );
            }
            x = link(
                buf,
                app,
                x,
                y,
                limit,
                "×",
                Style::default().fg(FAINT),
                UiAction::CloseTab(i, ti),
                "Close this tab (Ctrl-a w)",
            );
            x = text(buf, x, y, limit, " ", Style::default());
        }

        link(
            buf,
            app,
            x,
            y,
            limit,
            " + ",
            Style::default().fg(theme::SAGE),
            UiAction::NewTab(i),
            "New tab: pick a folder or resume a session (Ctrl-a t)",
        )
    } else {
        let t = slot.cur();
        text(
            buf,
            x,
            y,
            limit,
            &format!(" {} ", snippet(&t.name(), 24)),
            Style::default().fg(DIM),
        )
    };
    let _ = x;
}

pub fn pane_regions(f: &mut Frame, app: &App, i: usize, area: Rect, term: Rect, foot: Rect) {
    let mut hits = app.hits.borrow_mut();
    hits.add(
        term,
        UiAction::PaneBody(i),
        "Click to focus; drag to select and copy text (wheel scrolls history)",
    );
    let slot = &app.panes[i];
    match slot.account {
        Some(a) if app.accounts[a].login.can_start() => {
            hits.add(
                foot,
                UiAction::UsageOf(a),
                "Open the dashboard for this account",
            );
        }
        Some(_) => hits.add(
            foot,
            UiAction::LoginPane(i),
            "Log this account in (Ctrl-a I)",
        ),
        None => hits.add(
            foot,
            UiAction::AccountMenu(i),
            "Pick an account for this pane",
        ),
    }
    drop(hits);
    let _ = area;

    let t = slot.cur();
    if t.activity != Activity::Permission || foot.height == 0 {
        return;
    }
    let screen = t
        .parser
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .screen()
        .contents();
    let prompt = crate::prompt_cli::parse_prompt_for(t.harness, &screen);
    let y = foot.y + foot.height - 1;
    let buf = f.buffer_mut();
    for x in foot.x..foot.x + foot.width {
        if let Some(c) = buf.cell_mut((x, y)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(BAR_BG));
        }
    }
    let limit = foot.x + foot.width;
    let mut x = text(
        buf,
        foot.x,
        y,
        limit,
        " waiting: ",
        Style::default().fg(theme::SAND).bg(BAR_BG),
    );
    let tab = slot.active;
    let label_for = |c: Choice, fallback: &str| -> Option<String> {
        match &prompt {
            Some(p) => p.index_for(c).map(|idx| snippet(&p.options[idx].label, 22)),
            None => Some(fallback.to_string()),
        }
    };
    for (c, name, accent) in [
        (Choice::Approve, "Approve", theme::SAGE),
        (Choice::Always, "Always", theme::SLATE),
        (Choice::Deny, "Deny", theme::CLAY),
    ] {
        if let Some(lbl) = label_for(c, name) {
            let shown = if lbl.to_lowercase().starts_with(&name.to_lowercase()) {
                lbl.clone()
            } else {
                format!("{name}: {lbl}")
            };
            let hint = format!("{name} this request: picks \"{lbl}\"");
            x = btn(
                buf,
                app,
                x,
                y,
                limit,
                &snippet(&shown, 26),
                UiAction::Answer(i, tab, c),
                &hint,
                accent,
            );
        }
    }
}

pub fn modal_chrome(buf: &mut Buffer, app: &App, r: Rect, buttons: &[(&str, UiAction, &str)]) {
    if r.width < 6 || r.height < 2 {
        return;
    }
    let right = r.x + r.width - 1;
    btn(
        buf,
        app,
        right.saturating_sub(4),
        r.y,
        right,
        "x",
        UiAction::ModalCancel,
        "Close (Esc)",
        theme::CLAY,
    );
    let total: u16 = buttons
        .iter()
        .map(|(l, _, _)| l.chars().count() as u16 + 3)
        .sum();
    let y = r.y + r.height - 1;

    if !buttons.is_empty() {
        let border_style = buf.cell((r.x, y)).map(|c| c.style()).unwrap_or_default();
        for cx in r.x + 1..right {
            if let Some(c) = buf.cell_mut((cx, y)) {
                c.set_char('─');
                c.set_style(border_style);
            }
        }
    }
    let mut x = right.saturating_sub(total + 1).max(r.x + 1);
    for (k, (label, action, hint)) in buttons.iter().enumerate() {
        let accent = match action {
            UiAction::ModalCancel | UiAction::TourSkip => theme::STONE,
            _ => theme::SAGE,
        };
        let start = x;
        x = btn(buf, app, x, y, right, label, action.clone(), hint, accent);

        if k == 0 && !matches!(action, UiAction::ModalCancel) {
            for cx in start + 1..x.saturating_sub(1) {
                if let Some(c) = buf.cell_mut((cx, y)) {
                    c.set_style(
                        c.style()
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn list_rows(
    app: &App,
    list: List,
    x: u16,
    y0: u16,
    w: u16,
    first: usize,
    count: usize,
    height_each: u16,
    max_y: u16,
) {
    let mut hits = app.hits.borrow_mut();
    for k in 0..count {
        let y = y0 + k as u16 * height_each;
        if y >= max_y {
            break;
        }
        hits.add(
            Rect::new(x, y, w, height_each.min(max_y - y)),
            UiAction::Row(list, first + k),
            "Click to select, double click to open (wheel scrolls)",
        );
    }
}

pub fn draw_account_menu(f: &mut Frame, area: Rect, app: &App, slot: usize, sel: usize) {
    let entries = app.account_menu_entries(slot);
    let h = (entries.len() as u16 + 4).min(area.height);
    let w = 54u16.min(area.width);

    let pr = app.pane_rects.get(slot).copied().unwrap_or(area);
    let x =
        pr.x.saturating_add(2)
            .min(area.x + area.width.saturating_sub(w));
    let y = (pr.y + 1).min(area.y + area.height.saturating_sub(h));
    let r = Rect::new(x, y, w, h);
    f.render_widget(Clear, r);
    let sel = sel.min(entries.len().saturating_sub(1));
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            let st = if i == sel {
                Style::default()
                    .fg(FG)
                    .bg(SEL_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(FG)
            };
            Line::styled(format!(" {label}"), st)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(DIM))
                    .title(Span::styled(
                        format!(" pane {} account ", slot + 1),
                        Style::default().fg(FG),
                    )),
            )
            .style(Style::default().bg(BAR_BG)),
        r,
    );
    list_rows(
        app,
        List::AccountMenu,
        r.x + 1,
        r.y + 1,
        r.width.saturating_sub(2),
        0,
        entries.len(),
        1,
        r.y + r.height - 1,
    );
    modal_chrome(
        f.buffer_mut(),
        app,
        r,
        &[("Cancel", UiAction::ModalCancel, "Close the menu (Esc)")],
    );
}

pub fn draw_tour(f: &mut Frame, area: Rect, app: &App, step: usize, menu: Rect) {
    let steps = crate::app_mouse::TOUR;
    let step = step.min(steps.len() - 1);
    let focus_pane = app.pane_rects.get(app.focus).copied().unwrap_or(area);
    let find = |a: &UiAction| app.hits.borrow().find(a).map(|r| r.rect);
    let target = match step {
        0 => menu,
        1 => Rect::new(
            area.x,
            menu.y + menu.height,
            area.width,
            area.height.saturating_sub(menu.height + 1),
        ),
        2 => {
            let inner = Block::default().borders(Borders::ALL).inner(focus_pane);
            let bar = crate::ui::pane_layout(inner, app.tab_pos(app.focus), app.side(app.focus)).0;
            if bar.width > 0 {
                bar
            } else {
                Rect::new(focus_pane.x, focus_pane.y, focus_pane.width, 1)
            }
        }
        3 => {
            let inner = Block::default().borders(Borders::ALL).inner(focus_pane);
            crate::ui::pane_layout(inner, app.tab_pos(app.focus), app.side(app.focus)).2
        }
        4 => find(&UiAction::Key('y')).unwrap_or(menu),
        _ => find(&UiAction::VoiceCycle).unwrap_or(menu),
    };

    {
        let buf = f.buffer_mut();
        let tint = Color::Rgb(66, 60, 46);
        for yy in target.y
            ..target
                .y
                .saturating_add(target.height)
                .min(area.y + area.height)
        {
            for xx in target.x
                ..target
                    .x
                    .saturating_add(target.width)
                    .min(area.x + area.width)
            {
                if let Some(c) = buf.cell_mut((xx, yy)) {
                    c.set_bg(tint);
                }
            }
        }
    }
    let outline = target;
    let (title, body) = steps[step];
    let w = 64u16.min(area.width);
    let h = 8u16.min(area.height);
    let below = outline.y + outline.height;
    let y = if below + h <= area.y + area.height {
        below
    } else {
        outline.y.saturating_sub(h).max(area.y)
    };
    let x = (outline.x + 2).min(area.x + area.width.saturating_sub(w));
    let r = Rect::new(x, y, w, h);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(vec![Line::styled(
            body.to_string(),
            Style::default().fg(FG),
        )])
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::SAND))
                .title(Span::styled(
                    format!(" tour {}/{}: {title} ", step + 1, steps.len()),
                    Style::default().fg(FG).add_modifier(Modifier::BOLD),
                )),
        )
        .style(Style::default().bg(BAR_BG)),
        r,
    );
    let last = step + 1 == steps.len();
    modal_chrome(
        f.buffer_mut(),
        app,
        r,
        &[
            (
                "Skip",
                UiAction::TourSkip,
                "End the tour (Esc); Help has it again",
            ),
            (
                if last { "Done" } else { "Next" },
                UiAction::TourNext,
                "Next step (Enter)",
            ),
        ],
    );
}

pub fn help_action(keys: &str) -> Option<UiAction> {
    let k = keys.trim();
    let c = match k {
        _ if k.starts_with("Ctrl-a t") => 't',
        _ if k.starts_with("Ctrl-a w") => 'w',
        _ if k.starts_with("Ctrl-a f") => 'f',
        _ if k.starts_with("Ctrl-a n") => 'n',
        _ if k.starts_with("Ctrl-a o") => 'o',
        _ if k.starts_with("Ctrl-a z") => 'z',
        _ if k.starts_with("Ctrl-a r") => 'r',
        _ if k.starts_with("Ctrl-a I") => 'I',
        _ if k.starts_with("Ctrl-a a") => 'a',
        _ if k.starts_with("Ctrl-a A") => 'A',
        _ if k.starts_with("Ctrl-a d") => 'd',
        _ if k.starts_with("Ctrl-a s") => 's',
        _ if k.starts_with("Ctrl-a S") => 'S',
        _ if k.starts_with("Ctrl-a H") => 'H',
        _ if k.starts_with("Ctrl-a ,") => ',',
        _ if k.starts_with("Ctrl-a R") => 'R',
        _ if k.starts_with("Ctrl-a u") => 'u',
        _ if k.starts_with("Ctrl-a :") => ':',
        _ if k.starts_with("Ctrl-a b") => 'b',
        _ if k.starts_with("Ctrl-a y") => 'y',
        _ if k.starts_with("Ctrl-a c") => 'c',
        _ if k.starts_with("Ctrl-a v") => 'v',
        _ if k.starts_with("Ctrl-a M") => 'M',
        _ if k.starts_with("Ctrl-a #") => '#',
        _ if k.starts_with("Ctrl-a ;") => ';',
        _ if k.starts_with("Ctrl-a B") => 'B',
        _ if k.starts_with("Ctrl-a m") => 'm',
        _ if k.starts_with("Ctrl-a q") => 'q',
        _ if k.starts_with("Ctrl-a space") => return Some(UiAction::Mic),
        _ => return None,
    };
    Some(UiAction::Key(c))
}

fn tail_chars(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

pub fn anim_frame() -> usize {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        / 125) as usize
}

pub fn tab_marker(t: &Pane) -> (String, Color) {
    if t.suspended && !t.is_running() {
        return ("z".into(), FAINT);
    }
    const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    match t.activity {
        Activity::Working => (SPIN[anim_frame() % 10].to_string(), theme::ACTIVE),
        Activity::Permission => {
            let on = (anim_frame() / 4).is_multiple_of(2);
            (
                "!".into(),
                if on {
                    theme::ATTN_GLYPH
                } else {
                    theme::ATTN_GLYPH_DIM
                },
            )
        }
        Activity::Ready => ("○".into(), FAINT),
        Activity::Starting => ("◌".into(), DIM),
        Activity::Exited => ("×".into(), theme::CLAY),
        Activity::Idle => ("○".into(), FAINT),
    }
}

pub fn background_of(app: &App, s: usize, t: usize) -> Option<String> {
    background_work(app, s, t).map(|w| format!("background · {w}"))
}

pub fn background_work(app: &App, s: usize, t: usize) -> Option<String> {
    let tab = app.panes.get(s)?.tabs.get(t)?;
    if !matches!(tab.activity, Activity::Ready | Activity::Idle) {
        return None;
    }

    if app.tab_at_limit(s, t) {
        return None;
    }
    let mut cache = app.bg_cache.borrow_mut();
    let fresh = cache
        .get(&tab.uid)
        .filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(2))
        .map(|(_, v)| v.clone());

    match fresh {
        Some(v) => v,
        None if !tab.is_running() => None,
        None => {
            let a = app.tab_activity(s, t);
            let n = a.background();
            let v = (n > 0).then(|| {
                let mut parts = vec![];
                if a.subagents_running > 0 {
                    parts.push(crate::control::plural(a.subagents_running, "subagent"));
                }
                if a.background_shells > 0 {
                    parts.push(crate::control::plural(a.background_shells, "shell"));
                }
                if a.background_tasks > 0 {
                    parts.push(crate::control::plural(a.background_tasks, "task"));
                }
                parts.join(", ")
            });
            cache.insert(tab.uid, (std::time::Instant::now(), v.clone()));
            v
        }
    }
}

pub struct TabLook {
    pub glyph: String,
    pub color: Color,
    pub word: String,
    pub word_style: Style,
    pub bar: Option<Color>,
    pub loud: bool,
}

pub fn tab_look(app: &App, s: usize, t: usize) -> TabLook {
    let tab = &app.panes[s].tabs[t];
    let (glyph, color) = tab_marker(tab);
    let word = since(tab);
    let base = TabLook {
        glyph,
        color,
        word,
        word_style: Style::default().fg(FAINT),
        bar: None,
        loud: false,
    };
    if tab.suspended && !tab.is_running() {
        return base;
    }
    match tab.activity {
        Activity::Working => TabLook {
            word_style: Style::default()
                .fg(theme::ACTIVE)
                .add_modifier(Modifier::BOLD),
            bar: Some(theme::ACTIVE),
            loud: true,
            ..base
        },
        Activity::Permission => TabLook {
            word_style: Style::default()
                .fg(theme::SAND)
                .add_modifier(Modifier::BOLD),
            bar: Some(theme::SAND),
            loud: true,
            ..base
        },
        Activity::Exited => TabLook {
            word_style: Style::default().fg(theme::CLAY),
            ..base
        },
        _ => match background_of(app, s, t) {
            Some(w) => {
                const HOLLOW: [&str; 4] = ["◜", "◝", "◞", "◟"];
                TabLook {
                    glyph: HOLLOW[anim_frame() % 4].into(),
                    color: theme::SLATE,
                    word: w,
                    word_style: Style::default().fg(theme::SLATE),
                    bar: Some(theme::SLATE),
                    loud: false,
                }
            }
            None => base,
        },
    }
}

fn since(t: &Pane) -> String {
    if t.suspended && !t.is_running() {
        return "zz saved".into();
    }
    let word = match t.activity {
        Activity::Working => "working",
        Activity::Permission => "needs you",
        Activity::Ready => "idle",
        Activity::Starting => "starting",
        Activity::Exited => "exited",
        Activity::Idle => "idle",
    };
    if !t.is_running() {
        return word.to_string();
    }
    let s = t.activity_since.elapsed().as_secs();
    let age = if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    };
    format!("{word} {age}")
}

fn since_short(t: &Pane) -> String {
    let long = since(t);
    if t.suspended && !t.is_running() {
        return "zz".into();
    }

    let last = long.rsplit(' ').next().unwrap_or("");
    if last.starts_with(|c: char| c.is_ascii_digit()) {
        last.to_string()
    } else {
        long
    }
}

#[allow(clippy::too_many_arguments)]
fn group_header(
    buf: &mut ratatui::buffer::Buffer,
    app: &App,
    i: usize,
    g: usize,
    x0: u16,
    y: u16,
    limit: u16,
    narrow: bool,
) {
    let slot = &app.panes[i];
    let grp = &slot.groups[g];
    let color = grp.color.as_deref().map(theme::parse_color).unwrap_or(DIM);
    let arrow = if grp.collapsed { "▸" } else { "▾" };
    let n = slot.members(g).len();
    let st = slot.group_state(g);
    let dot = crate::ui::activity_color(st);
    let hovered = app
        .mouse_pos
        .is_some_and(|(c, r)| contains(Rect::new(x0, y, limit - x0, 1), c, r));
    let bg = if hovered {
        Color::Rgb(44, 47, 51)
    } else {
        Color::Reset
    };
    for xx in x0..limit {
        if let Some(c) = buf.cell_mut((xx, y)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(bg));
        }
    }
    let mut cx = text(buf, x0, y, limit, arrow, Style::default().fg(color).bg(bg));
    if !narrow {
        cx = text(
            buf,
            cx + 1,
            y,
            limit,
            &format!("{} ({n})", grp.name),
            Style::default()
                .fg(color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        );
        text(buf, cx + 1, y, limit, "●", Style::default().fg(dot).bg(bg));
    }
    app.hits.borrow_mut().add(
        Rect::new(x0, y, limit - x0, 1),
        UiAction::GroupHeader(i, g),
        format!(
            "Group {}: click folds or unfolds · right click: rename, color, close, move, ungroup · drop a tab here to add it",
            grp.name
        ),
    );
}

fn archive_header(
    buf: &mut ratatui::buffer::Buffer,
    app: &App,
    i: usize,
    x0: u16,
    y: u16,
    limit: u16,
    n: usize,
) {
    let open = !app.archive_collapsed || !app.panes[i].filter.trim().is_empty();
    let hovered = app
        .mouse_pos
        .is_some_and(|(c, r)| contains(Rect::new(x0, y, limit - x0, 1), c, r));
    let bg = if hovered {
        Color::Rgb(44, 47, 51)
    } else {
        Color::Reset
    };
    for xx in x0..limit {
        if let Some(c) = buf.cell_mut((xx, y)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(bg));
        }
    }
    let arrow = if open { "▾" } else { "▸" };
    let cx = text(buf, x0, y, limit, arrow, Style::default().fg(DIM).bg(bg));
    text(
        buf,
        cx + 1,
        y,
        limit,
        &format!("Archived ({n})"),
        Style::default().fg(DIM).bg(bg).add_modifier(Modifier::BOLD),
    );
    app.hits.borrow_mut().add(
        Rect::new(x0, y, limit - x0, 1),
        UiAction::ArchivedHeader(i),
        "Archived conversations: stopped sessions that cost no memory · click folds or unfolds · double click one to resume it · Ctrl-a f archives the current tab",
    );
}

fn archived_row(
    buf: &mut ratatui::buffer::Buffer,
    app: &App,
    i: usize,
    e: &crate::archive::Archived,
    x0: u16,
    y: u16,
    limit: u16,
) {
    let row = Rect::new(x0, y, limit - x0, 1);
    let hovered = app.mouse_pos.is_some_and(|(c, r)| contains(row, c, r));
    let selected = app.archive_sel.as_deref() == Some(e.id.as_str());
    let bg = if selected {
        SEL_BG
    } else if hovered {
        Color::Rgb(44, 47, 51)
    } else {
        Color::Reset
    };
    for xx in x0..limit {
        if let Some(c) = buf.cell_mut((xx, y)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(bg));
        }
    }
    let ago = crate::ui::ago_secs(app.archive_now() - e.last_activity);
    let ago = ago.replace(" ago", "").replace("just now", "now");
    let room = (limit.saturating_sub(x0) as usize).saturating_sub(ago.chars().count() + 6);
    let cx = text(
        buf,
        x0 + 2,
        y,
        limit,
        "▫ ",
        Style::default().fg(FAINT).bg(bg),
    );
    text(
        buf,
        cx,
        y,
        limit,
        &crate::paths::middle(&e.label, room.max(1)),
        Style::default().fg(DIM).bg(bg),
    );
    let ax = limit.saturating_sub(ago.chars().count() as u16 + 1);
    text(buf, ax, y, limit, &ago, Style::default().fg(FAINT).bg(bg));
    app.hits.borrow_mut().add(
        row,
        UiAction::ArchivedRow(i, e.id.clone()),
        format!(
            "Archived: {} · {} · {} · double click resumes it · right click: restore, delete from archive",
            e.label,
            crate::paths::tilde(std::path::Path::new(&e.cwd)),
            crate::ui::ago_secs(app.archive_now() - e.archived_at)
        ),
    );
}

pub fn window_start(scroll: usize, active: usize, n: usize, cap: usize) -> usize {
    if cap == 0 || n <= cap {
        return 0;
    }
    if scroll != crate::slot::FOLLOW {
        return scroll.min(n - cap);
    }
    active.saturating_sub(cap - 1).min(n - cap)
}

pub fn draw_tab_list(f: &mut Frame, app: &App, i: usize, bar: Rect, pos: crate::slot::TabPos) {
    if bar.width == 0 || bar.height == 0 {
        return;
    }

    let start = {
        let mut h = app.hits.borrow_mut();
        let start = h.regions.len();
        h.add(
            bar,
            UiAction::FocusPane(i),
            "Click to focus this pane (wheel scrolls the tab list)",
        );
        start
    };
    draw_tab_list_body(f, app, i, bar, pos);
    let mut h = app.hits.borrow_mut();
    let end = h.regions.len();
    h.tab_lists.push((i, start, end));
}

pub fn list_max_first(list: &[crate::tab_groups::Row], rows: usize, per: usize) -> usize {
    let mut used = 0;
    let mut k = list.len();
    while k > 0 {
        let h = match list[k - 1] {
            crate::tab_groups::Row::Group(_)
            | crate::tab_groups::Row::ArchHead
            | crate::tab_groups::Row::Arch(_) => 1,
            crate::tab_groups::Row::Tab(_) => per,
        };
        if used + h > rows {
            break;
        }
        used += h;
        k -= 1;
    }
    k
}

fn draw_tab_list_body(f: &mut Frame, app: &App, i: usize, bar: Rect, pos: crate::slot::TabPos) {
    use crate::slot::TabPos;
    if pos == TabPos::Top {
        return draw_tab_strip(f, app, i, bar);
    }
    let slot = &app.panes[i];
    let buf = f.buffer_mut();

    let sep_x = if pos == TabPos::Left {
        bar.x + bar.width - 1
    } else {
        bar.x
    };
    for yy in bar.y..bar.y + bar.height {
        if let Some(c) = buf.cell_mut((sep_x, yy)) {
            c.set_char('│');
            c.set_style(Style::default().fg(FAINT));
        }
    }
    let x0 = if pos == TabPos::Left {
        bar.x
    } else {
        bar.x + 1
    };
    let w = bar.width - 1;
    let limit = x0 + w;
    let collapsed = w <= 3;

    let (handle, hint) = match (pos, collapsed) {
        (TabPos::Left, false) | (TabPos::Right, true) => (
            "«",
            if collapsed {
                "Expand the tab list (Ctrl-a s)"
            } else {
                "Collapse the tab list (Ctrl-a s)"
            },
        ),
        _ => (
            "»",
            if collapsed {
                "Expand the tab list (Ctrl-a s)"
            } else {
                "Collapse the tab list (Ctrl-a s)"
            },
        ),
    };
    if !collapsed {
        let tx = if pos == TabPos::Right { x0 + 2 } else { x0 };
        let ex = text(
            buf,
            tx,
            bar.y,
            limit,
            &format!(" tabs {}", slot.tabs.len()),
            Style::default().fg(FAINT),
        );

        let working = slot
            .tabs
            .iter()
            .filter(|t| t.activity == Activity::Working)
            .count();
        let waiting = slot
            .tabs
            .iter()
            .filter(|t| t.activity == Activity::Permission)
            .count();

        let room = limit.saturating_sub(ex + 3) as usize;
        let full = (if working > 0 { 12 } else { 0 }) + (if waiting > 0 { 12 } else { 0 });
        let mid = (if working > 0 { 7 } else { 0 }) + (if waiting > 0 { 7 } else { 0 });
        let (short, tiny) = (full > room, mid > room);
        let ex = if working > 0 {
            let t = if tiny {
                format!(" ·{working}w")
            } else if short {
                format!(" ·{working} work")
            } else {
                format!(" · {working} working")
            };
            text(
                buf,
                ex,
                bar.y,
                limit,
                &t,
                Style::default()
                    .fg(theme::ACTIVE)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ex
        };
        let ex = if waiting > 0 {
            let t = if tiny {
                format!(" ·{waiting}!")
            } else if short {
                format!(" ·{waiting} wait")
            } else {
                format!(" · {waiting} waiting")
            };
            text(
                buf,
                ex,
                bar.y,
                limit,
                &t,
                Style::default()
                    .fg(theme::SAND)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ex
        };

        let ex = if slot.sort != crate::tab_groups::TabSort::Manual && bar.height < 6 {
            text(
                buf,
                ex,
                bar.y,
                limit,
                &format!(" ↓{}", slot.sort.name()),
                Style::default().fg(theme::STONE),
            )
        } else {
            ex
        };
        let ex = match slot
            .account
            .and_then(|a| app.account_cfg(a).harness().badge())
        {
            Some(b) => text(
                buf,
                ex,
                bar.y,
                limit,
                &format!(" {b}"),
                Style::default().fg(theme::STONE),
            ),
            None => ex,
        };

        if let Some(left) = slot.account.and_then(|a| app.accounts[a].effective_left()) {
            text(buf, ex + 1, bar.y, limit, "●", theme::remaining_style(left));
        }
    }
    let hx = if collapsed {
        x0
    } else if pos == TabPos::Left {
        limit.saturating_sub(2)
    } else {
        x0
    };
    link(
        buf,
        app,
        hx,
        bar.y,
        limit,
        &format!("{handle} "),
        Style::default().fg(DIM),
        UiAction::SidebarToggle(i),
        hint,
    );
    if bar.height < 3 {
        return;
    }

    let last = bar.y + bar.height - 1;
    let plus = if collapsed { " +" } else { " + New tab" };
    link(
        buf,
        app,
        x0,
        last,
        limit,
        plus,
        Style::default().fg(theme::SAGE),
        UiAction::NewTab(i),
        "New tab: pick a folder or resume a session (Ctrl-a t)",
    );

    let search_row = !collapsed && bar.height >= 6;
    let only = app.tab_filter(i);
    if search_row {
        draw_search_row(buf, app, i, x0, bar.y + 1, limit, only.as_deref());
    }
    let top = bar.y + 1 + u16::from(search_row);

    let rows = (bar.height - 2 - u16::from(search_row)) as usize;
    let two_line = !collapsed && rows >= 2;

    let wide = two_line && bar.width >= crate::ui::SIDEBAR_WIDE_AT;
    let per = if two_line { 2 } else { 1 };
    let cap = rows / per;
    let labels = crate::slot::tab_labels(slot);

    let hold = app.mouse_pos.is_some_and(|(c, r)| contains(bar, c, r));
    let mut list = app.list_rows(i, hold);

    let archived = if collapsed {
        vec![]
    } else {
        app.archive_for_pane(i)
    };
    if !collapsed {
        list.extend(app.archive_rows(i));
    }
    if list.is_empty() && only.is_some() && top < last {
        text(
            buf,
            x0 + 1,
            top,
            limit,
            "No tab matches (Esc clears)",
            Style::default().fg(FAINT),
        );
    }
    let at = list
        .iter()
        .position(|r| *r == crate::tab_groups::Row::Tab(slot.active))
        .unwrap_or(0);
    let max_first = list_max_first(&list, rows, per);
    let first = window_start(slot.sidebar_scroll, at, list.len(), cap).min(max_first);
    slot.list_window.set((first, max_first));
    let mut y = top;
    for row in list.iter().skip(first) {
        let ti = match *row {
            crate::tab_groups::Row::Group(g) => {
                if y >= last {
                    break;
                }
                group_header(buf, app, i, g, x0, y, limit, collapsed);
                y += 1;
                continue;
            }
            crate::tab_groups::Row::ArchHead => {
                if y >= last {
                    break;
                }
                archive_header(buf, app, i, x0, y, limit, archived.len());
                y += 1;
                continue;
            }
            crate::tab_groups::Row::Arch(k) => {
                if y >= last {
                    break;
                }
                if let Some(e) = archived.get(k) {
                    archived_row(buf, app, i, e, x0, y, limit);
                }
                y += 1;
                continue;
            }
            crate::tab_groups::Row::Tab(ti) => ti,
        };
        if y + per as u16 > last {
            break;
        }
        let t = &slot.tabs[ti];
        let y0 = y;
        y += per as u16;
        let y = y0;
        let active = ti == slot.active;
        let row = Rect::new(x0, y, w, per as u16);
        let hovered = app.mouse_pos.is_some_and(|(c, r)| contains(row, c, r));
        let bg = if active {
            SEL_BG
        } else if hovered {
            Color::Rgb(44, 47, 51)
        } else {
            Color::Reset
        };

        let label = app.tab_label(i, ti);
        let bg = match label {
            Some(l) => crate::tab_labels::tint(bg, l.color(), 10),
            None => bg,
        };
        for yy in y..y + per as u16 {
            for xx in x0..limit {
                if let Some(c) = buf.cell_mut((xx, yy)) {
                    c.set_char(' ');
                    c.set_style(Style::default().bg(bg));
                }
            }
        }
        let look = tab_look(app, i, ti);
        let (mark, mc) = (look.glyph.clone(), look.color);

        let name_style = if look.loud {
            Style::default()
                .fg(look.color)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else if active {
            Style::default().fg(FG).bg(bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM).bg(bg)
        };

        let num = if collapsed {
            if slot.tabs.len() > 9 {
                format!("{:>2}", ti + 1)
            } else {
                format!("{}", ti + 1)
            }
        } else {
            format!(" {:>2} ", ti + 1)
        };
        let num_style = if collapsed && look.loud {
            Style::default()
                .fg(look.color)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else if let Some(l) = label.filter(|_| collapsed) {
            Style::default().fg(l.color()).bg(bg)
        } else {
            Style::default().fg(FAINT).bg(bg)
        };
        let mut cx = text(buf, x0, y, limit, &num, num_style);

        let accent = slot.accent_of(ti);
        let bar = look
            .bar
            .or_else(|| accent.as_deref().map(theme::parse_color));
        if let Some(c) = bar.filter(|_| !collapsed) {
            for yy in y..y + per as u16 {
                text(buf, x0, yy, limit, "▎", Style::default().fg(c).bg(bg));
            }
        }
        if t.pinned {
            text(
                buf,
                cx.saturating_sub(1),
                y,
                limit,
                crate::app_tabgroups::PIN,
                Style::default().fg(theme::SAND).bg(bg),
            );
        }
        cx = text(buf, cx, y, limit, &mark, Style::default().fg(mc).bg(bg));
        let dir = crate::paths::tilde(&app.tab_dir(i, ti));

        app.hits.borrow_mut().add(
            row,
            UiAction::SelectTab(i, ti),
            format!("Switch to this tab: {} · {dir} · ✎ rename · double click: move and options · right click: options", labels[ti]),
        );
        if collapsed {
            continue;
        }
        cx = text(buf, cx, y, limit, " ", Style::default().bg(bg));

        let nl = app.loops_in(t.uid);
        if nl > 0 {
            let bx = cx;
            cx = text(
                buf,
                cx,
                y,
                limit,
                &format!("⟳{nl} "),
                Style::default().fg(theme::MAUVE).bg(bg),
            );
            app.hits.borrow_mut().add(
                Rect::new(bx, y, cx.saturating_sub(bx), 1),
                UiAction::SelectTab(i, ti),
                format!(
                    "{} scheduled in this tab (the Loops view lists them)",
                    crate::control::plural(nl, "loop")
                ),
            );
        }

        if let Some(w) = app.git_badge(&app.tab_dir(i, ti)) {
            let bx = cx;
            cx = text(
                buf,
                cx,
                y,
                limit,
                "⚠ ",
                Style::default().fg(theme::SAND).bg(bg),
            );
            app.hits.borrow_mut().add(
                Rect::new(bx, y, cx.saturating_sub(bx), 1),
                UiAction::GitOpen(i, ti),
                format!("Git: {w} (click for the Git section)"),
            );
        }

        if app.queued_for(t.uid) > 0 && label != Some(crate::tab_labels::TabLabel::Queued) {
            cx = text(
                buf,
                cx,
                y,
                limit,
                "⧗ ",
                Style::default().fg(theme::SAND).bg(bg),
            );
        }

        if app.move_pending(t.uid).is_some() {
            cx = text(
                buf,
                cx,
                y,
                limit,
                "→ ",
                Style::default().fg(theme::SAND).bg(bg),
            );
        }

        let tools = active || hovered;
        let room = (limit.saturating_sub(cx) as usize).saturating_sub(if tools { 5 } else { 1 });

        let pill = label.map(|l| {
            let fit = crate::tab_labels::fit(l, room, 6);
            (l, l.pill(fit), fit)
        });
        let room = match &pill {
            Some((_, p, _)) => room.saturating_sub(p.chars().count() + 1),
            None => room,
        };

        if let crate::app::Modal::Rename(rs, rt, b) = &app.modal {
            if *rs == i && *rt == ti {
                let room = limit.saturating_sub(cx) as usize;
                let shown = format!("{}▏", tail_chars(b, room.saturating_sub(1).max(1)));
                for x in cx..limit {
                    if let Some(c) = buf.cell_mut((x, y)) {
                        c.set_char(' ');
                        c.set_style(Style::default().bg(SEL_BG));
                    }
                }
                text(
                    buf,
                    cx,
                    y,
                    limit,
                    &shown,
                    Style::default()
                        .fg(FG)
                        .bg(SEL_BG)
                        .add_modifier(Modifier::BOLD),
                );
                app.inline_rename.set(true);
                continue;
            }
        }
        text(
            buf,
            cx,
            y,
            limit,
            &crate::paths::middle(&labels[ti], room.max(1)),
            name_style,
        );
        if let Some((l, p, fit)) = &pill {
            let pw = p.chars().count() as u16;
            let end = limit.saturating_sub(if tools { 5 } else { 1 });
            let px = end.saturating_sub(pw).max(cx);
            let st = if *fit == crate::tab_labels::Fit::Dot {
                Style::default().fg(l.color()).bg(bg)
            } else {
                Style::default()
                    .fg(l.color())
                    .bg(crate::tab_labels::tint(bg, l.color(), 22))
            };
            text(buf, px, y, end, p, st);
            app.hits.borrow_mut().add(
                Rect::new(px, y, pw.min(end.saturating_sub(px)), 1),
                UiAction::SelectTab(i, ti),
                l.hint(),
            );
        }
        if tools && limit >= x0 + 8 {
            link(
                buf,
                app,
                limit - 4,
                y,
                limit,
                "✎",
                Style::default().fg(DIM).bg(bg),
                UiAction::RenameTab(i, ti),
                "Rename this tab (Ctrl-a R)",
            );
            link(
                buf,
                app,
                limit - 2,
                y,
                limit,
                "×",
                Style::default().fg(DIM).bg(bg),
                UiAction::CloseTab(i, ti),
                "Close this tab (Ctrl-a w)",
            );
        }
        if two_line {
            let lx = x0 + 5;
            let room = limit.saturating_sub(lx + 1) as usize;
            let full = dir.chars().count();

            let show_dir = app.cfg.show_path;
            let want = full.min(10);
            let long = look.word.clone();
            let st = if !show_dir || room >= want + 1 + long.chars().count() {
                long
            } else if let Some(what) = long.strip_prefix("background · ") {
                what.to_string()
            } else {
                since_short(t)
            };
            let stn = st.chars().count();

            let urgent = look.bar.is_some();
            let (pw, show_st) = if !show_dir {
                (0, room >= stn)
            } else if room >= want + 1 + stn || (urgent && room > stn) {
                ((room.saturating_sub(stn + 1)).min(full), true)
            } else {
                (room, false)
            };
            let fst = Style::default().fg(FAINT).bg(bg);

            let title = wide
                .then(|| app.native_title(i, ti))
                .flatten()
                .filter(|n| !n.eq_ignore_ascii_case(&labels[ti]));
            let (lx, pw) = match &title {
                Some(n) => {
                    let st_room = if show_st { stn + 1 } else { 0 };
                    let avail = room.saturating_sub(st_room);

                    let tw = n.chars().count().min(avail);
                    let end = text(
                        buf,
                        lx,
                        y + 1,
                        limit,
                        &crate::paths::middle(n, tw.max(1)),
                        Style::default().fg(DIM).bg(bg),
                    );
                    let left = avail.saturating_sub(tw + 3);
                    if show_dir && left >= 6 {
                        let e = text(buf, end, y + 1, limit, " · ", fst);
                        (e, left)
                    } else {
                        (end, 0)
                    }
                }
                None => (lx, pw),
            };
            if show_dir && pw > 0 {
                text(
                    buf,
                    lx,
                    y + 1,
                    limit,
                    &crate::paths::shorten_path(&dir, pw),
                    fst,
                );
            }
            let lx = x0 + 5;
            if show_st {
                text(
                    buf,
                    (lx + room as u16).saturating_sub(stn as u16),
                    y + 1,
                    limit,
                    &st,
                    look.word_style.bg(bg),
                );
            }
        }
    }

    if !collapsed {
        app.hits.borrow_mut().add(
            Rect::new(sep_x, bar.y + 1, 1, bar.height.saturating_sub(2)),
            UiAction::SidebarEdge(i),
            "Drag to resize the tab list · double click: back to auto",
        );
    }
}

fn draw_search_row(
    buf: &mut ratatui::buffer::Buffer,
    app: &App,
    i: usize,
    x0: u16,
    y: u16,
    limit: u16,
    only: Option<&[bool]>,
) {
    let slot = &app.panes[i];
    let focused = app.tab_search == Some(i);
    let sort = format!("↓{} ▾", slot.sort.name());
    let sw = sort.chars().count() as u16;
    let sx = limit.saturating_sub(sw + 1);
    let field_end = sx.saturating_sub(1).max(x0);
    let bg = if focused { SEL_BG } else { Color::Reset };
    for x in x0..field_end {
        if let Some(c) = buf.cell_mut((x, y)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(bg));
        }
    }
    let mut x = text(
        buf,
        x0,
        y,
        field_end,
        " ⌕ ",
        Style::default().fg(DIM).bg(bg),
    );

    let count = only.map(|m| {
        format!(
            "{} of {}",
            m.iter().filter(|b| **b).count(),
            slot.tabs.len()
        )
    });
    let tail = count
        .as_ref()
        .map(|c| c.chars().count() as u16 + 3)
        .unwrap_or(0);
    let qroom = field_end.saturating_sub(x).saturating_sub(tail) as usize;
    if slot.filter.is_empty() && !focused {
        text(
            buf,
            x,
            y,
            field_end,
            "search",
            Style::default().fg(FAINT).bg(bg),
        );
    } else {
        let shown = if focused {
            format!(
                "{}▏",
                tail_chars(&slot.filter, qroom.saturating_sub(1).max(1))
            )
        } else {
            tail_chars(&slot.filter, qroom.max(1))
        };
        x = text(buf, x, y, field_end, &shown, Style::default().fg(FG).bg(bg));
    }
    let _ = x;
    app.hits.borrow_mut().add(
        Rect::new(x0, y, field_end.saturating_sub(x0), 1),
        UiAction::TabSearch(i),
        "Search this account's tabs: names, session titles, folders, status (Ctrl-a /; Esc clears)",
    );
    if let Some(c) = count {
        let cx = field_end.saturating_sub(tail);
        let end = text(buf, cx, y, field_end, &c, Style::default().fg(DIM).bg(bg));
        link(
            buf,
            app,
            end + 1,
            y,
            field_end,
            "×",
            Style::default().fg(DIM).bg(bg),
            UiAction::TabSearchClear(i),
            "Clear the search: every tab shows again",
        );
    }
    link(
        buf,
        app,
        sx,
        y,
        limit,
        &sort,
        Style::default().fg(theme::STONE),
        UiAction::TabSortMenu(i),
        "Sort this tab list: manual, most active, most recent, name, status, created, folder",
    );
}

fn draw_tab_strip(f: &mut Frame, app: &App, i: usize, bar: Rect) {
    let slot = &app.panes[i];
    let buf = f.buffer_mut();
    let y = bar.y;
    for xx in bar.x..bar.x + bar.width {
        if let Some(c) = buf.cell_mut((xx, y)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(BAR_BG));
        }
    }
    let limit = bar.x + bar.width;
    let names = crate::slot::tab_labels(slot);
    let label = |ti: usize, t: &Pane| {
        format!(
            " {}{} {} {} ",
            if t.pinned {
                crate::app_tabgroups::PIN
            } else {
                ""
            },
            ti + 1,
            tab_marker(t).0,
            crate::paths::middle(&names[ti], 18)
        )
    };

    let hold = app.mouse_pos.is_some_and(|(c, r)| contains(bar, c, r));
    let order = slot.tab_order_by(app.ungrouped_top(), hold, &|t| app.status_rank(i, t));
    let tags: Vec<Option<crate::tab_labels::TabLabel>> = (0..slot.tabs.len())
        .map(|ti| app.tab_label(i, ti))
        .collect();
    let widths: Vec<u16> = order
        .iter()
        .map(|&ti| label(ti, &slot.tabs[ti]).chars().count() as u16 + 2 + tags[ti].map_or(0, |_| 2))
        .collect();
    let pa = order.iter().position(|&t| t == slot.active).unwrap_or(0);
    let room = bar.width.saturating_sub(4 + 5);

    let max_first = (0..order.len())
        .find(|&k| bar.x + if k > 0 { 2 } else { 0 } + widths[k..].iter().sum::<u16>() + 5 <= limit)
        .unwrap_or(order.len().saturating_sub(1));
    let mut first = if slot.sidebar_scroll == crate::slot::FOLLOW {
        0
    } else {
        slot.sidebar_scroll.min(max_first)
    };
    if slot.sidebar_scroll == crate::slot::FOLLOW {
        while first < pa && widths[first..=pa].iter().sum::<u16>() > room {
            first += 1;
        }
    }
    slot.list_window.set((first, max_first.max(first)));
    let mut x = bar.x;
    if first > 0 {
        x = link(
            buf,
            app,
            x,
            y,
            limit,
            "‹ ",
            Style::default().fg(DIM).bg(BAR_BG),
            UiAction::TabsScroll(i, -1),
            "Earlier tabs",
        );
    }
    let mut overflow = false;
    for (k, &ti) in order.iter().enumerate().skip(first) {
        let t = &slot.tabs[ti];
        let lw = widths[k];
        if x + lw + 5 > limit {
            overflow = true;
            break;
        }
        let active = ti == slot.active;
        let (_, mc) = tab_marker(t);
        let st = if active {
            Style::default()
                .fg(FG)
                .bg(SEL_BG)
                .add_modifier(Modifier::BOLD)
        } else if t.activity == Activity::Permission {
            Style::default().fg(theme::SAND).bg(BAR_BG)
        } else if t.activity == Activity::Working {
            Style::default()
                .fg(theme::ACTIVE)
                .bg(BAR_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM).bg(BAR_BG)
        };
        let st = if t.activity == Activity::Permission {
            st.fg(theme::SAND)
        } else {
            st
        };
        let _ = mc;

        if let Some(a) = &slot.accent_of(ti) {
            x = text(
                buf,
                x,
                y,
                limit,
                "▎",
                Style::default().fg(theme::parse_color(a)).bg(BAR_BG),
            );
        }
        x = link(
            buf,
            app,
            x,
            y,
            limit,
            &label(ti, t),
            st,
            UiAction::SelectTab(i, ti),
            &format!(
                "Switch to this tab: {} · {} · ✎ rename · double click: move and options · right click: options",
                names[ti],
                crate::paths::tilde(&app.tab_dir(i, ti))
            ),
        );

        if let Some(l) = tags[ti] {
            x = link(
                buf,
                app,
                x,
                y,
                limit,
                "● ",
                Style::default().fg(l.color()).bg(BAR_BG),
                UiAction::SelectTab(i, ti),
                l.hint(),
            );
        }
        if active {
            x = link(
                buf,
                app,
                x,
                y,
                limit,
                "✎",
                Style::default().fg(DIM).bg(BAR_BG),
                UiAction::RenameTab(i, ti),
                "Rename this tab (Ctrl-a R)",
            );
        }
        x = link(
            buf,
            app,
            x,
            y,
            limit,
            "×",
            Style::default().fg(FAINT).bg(BAR_BG),
            UiAction::CloseTab(i, ti),
            "Close this tab (Ctrl-a w)",
        );
        x = text(buf, x, y, limit, " ", Style::default().bg(BAR_BG));
    }
    if overflow {
        x = link(
            buf,
            app,
            x,
            y,
            limit,
            "› ",
            Style::default().fg(DIM).bg(BAR_BG),
            UiAction::TabsScroll(i, 1),
            "More tabs",
        );
    }
    link(
        buf,
        app,
        x,
        y,
        limit,
        " + ",
        Style::default().fg(theme::SAGE).bg(BAR_BG),
        UiAction::NewTab(i),
        "New tab (Ctrl-a t)",
    );
}

pub fn draw_rename(f: &mut Frame, area: Rect, app: &App, p: usize, t: usize, buf_text: &str) {
    let w = 56u16.min(area.width);
    let h = 5u16.min(area.height);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    let lines = vec![
        Line::from(vec![
            Span::styled("> ", Style::default().fg(theme::SAND)),
            Span::styled(buf_text.to_string(), Style::default().fg(FG)),
            Span::styled("▏", Style::default().fg(theme::SAND)),
        ]),
        Line::styled(
            "Empty goes back to the folder name.",
            Style::default().fg(FAINT),
        ),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(DIM))
                    .title(Span::styled(
                        format!(" rename tab {} of pane {} ", t + 1, p + 1),
                        Style::default().fg(FG),
                    )),
            )
            .style(Style::default().bg(BAR_BG)),
        r,
    );
    modal_chrome(
        f.buffer_mut(),
        app,
        r,
        &[
            ("Rename", UiAction::ModalOk, "Save the name (Enter)"),
            ("Cancel", UiAction::ModalCancel, "Close (Esc)"),
        ],
    );
}

pub fn draw_overview_tree(f: &mut Frame, app: &App, r: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FAINT))
        .title(Span::styled(" tabs ", Style::default().fg(DIM)));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let buf = f.buffer_mut();
    let limit = inner.x + inner.width;
    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    for (si, slot) in app.panes.iter().enumerate() {
        if y >= bottom {
            break;
        }
        let label = slot
            .account
            .map(|a| app.account_cfg(a).display().to_string())
            .unwrap_or_else(|| "no account".into());
        let color = app.account_color(slot.account);
        let x = text(
            buf,
            inner.x,
            y,
            limit,
            &format!(" {} ", si + 1),
            Style::default().fg(FAINT),
        );
        link(
            buf,
            app,
            x,
            y,
            limit,
            &label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
            UiAction::FocusPane(si),
            "Go to this pane",
        );
        y += 1;
        for (ti, t) in slot.tabs.iter().enumerate() {
            if y >= bottom {
                break;
            }
            let (mark, mc) = tab_marker(t);
            let active = si == app.focus && ti == slot.active;
            let st = if active {
                Style::default()
                    .fg(FG)
                    .bg(SEL_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            };
            let mut x = text(
                buf,
                inner.x,
                y,
                limit,
                &format!("   {:>2} ", ti + 1),
                Style::default().fg(FAINT),
            );
            x = text(buf, x, y, limit, &mark, Style::default().fg(mc));
            link(
                buf,
                app,
                x,
                y,
                limit,
                &format!(
                    " {}",
                    crate::paths::middle(
                        &crate::slot::tab_labels(slot)[ti],
                        (inner.width as usize).saturating_sub(9).max(1)
                    )
                ),
                st,
                UiAction::SelectTab(si, ti),
                "Jump to this tab",
            );
            y += 1;
        }
    }
}

pub fn draw_message(
    f: &mut Frame,
    area: Rect,
    app: &App,
    title: &str,
    body: &[String],
    buttons: &[(&str, UiAction, &str)],
    accent: Color,
) {
    let w = (body.iter().map(|l| l.chars().count()).max().unwrap_or(20) as u16 + 6)
        .clamp(40, 90)
        .min(area.width);
    let h = (body.len() as u16 + 3).min(area.height);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    let lines: Vec<Line> = body
        .iter()
        .map(|l| Line::styled(l.clone(), Style::default().fg(FG)))
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(accent))
                    .title(Span::styled(format!(" {title} "), Style::default().fg(FG))),
            )
            .style(Style::default().bg(BAR_BG)),
        r,
    );
    modal_chrome(f.buffer_mut(), app, r, buttons);
}
