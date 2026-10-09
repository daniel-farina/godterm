//! The assistant panel: a title with the account and model (a menu to
//! switch), a toolbar that never clips (what does not fit goes to ⋯), one
//! compact status line, the conversation as labelled turns with tool calls
//! as readable action chips (raw arguments one click away), and the input
//! box with the mic state and Send.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui::Frame;
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::app::App;
use crate::app_assistant::Who;
use crate::hits::UiAction;
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};

/// One logical row of the conversation: styled pieces, and what a click
/// on it does.
pub struct Row {
    pub parts: Vec<(String, Style)>,
    pub action: Option<UiAction>,
    pub indent: u16,
}

fn row(parts: Vec<(String, Style)>) -> Row {
    Row {
        parts,
        action: None,
        indent: 0,
    }
}

/// A tool call as an action chip: icon, words, accent.
pub fn chip(app: &App, tool: &str, args: &Value, status: &str) -> (String, Color) {
    let s = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let tab_name = |t: &str| -> String {
        let id = t.trim();
        let uid = id.strip_prefix('t').and_then(|n| n.parse::<u64>().ok());
        match uid.and_then(|u| app.find_tab(u)) {
            Some((sl, ti)) => format!("{id} ({})", app.panes[sl].tabs[ti].name()),
            None if id.is_empty() => "a tab".into(),
            None => id.to_string(),
        }
    };
    let tabs = match &args["tab"] {
        Value::String(t) => tab_name(t),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(tab_name)
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    };
    let accounts = match &args["account"] {
        Value::Null => String::new(),
        Value::Array(a) => a
            .iter()
            .map(|x| x.to_string().trim_matches('"').to_string())
            .collect::<Vec<_>>()
            .join(", "),
        v => v.to_string().trim_matches('"').to_string(),
    };
    let on_acct = |a: &str| {
        if a.is_empty() {
            String::new()
        } else {
            format!(" on account {a}")
        }
    };
    let quote = |t: &str| format!("\"{}\"", crate::sessions::snippet(t, 70));
    let (text, color) = match tool {
        "send_prompt" => (
            format!(
                "→ asked {}: {}",
                if tabs.is_empty() {
                    "a tab".into()
                } else {
                    tabs
                },
                quote(&s("text"))
            ),
            theme::SLATE,
        ),
        "answer_prompt" => (
            format!("✓ answered {} with {}", tabs, s("choice")),
            theme::SAGE,
        ),
        "open_tab" => (
            format!(
                "+ opened a tab{}{}",
                if s("name").is_empty() {
                    String::new()
                } else {
                    format!(" \"{}\"", s("name"))
                },
                on_acct(&accounts)
            ),
            theme::SAGE,
        ),
        "open_session" | "reopen_tab" | "reopen_closed" | "take_over_session" => {
            ("↻ reopened a session".to_string(), theme::SAGE)
        }
        "close_tabs" => (
            format!(
                "× closed {}",
                if tabs.is_empty() { "tabs".into() } else { tabs }
            ),
            theme::CLAY,
        ),
        "stop_loops" => ("■ stopped loops".to_string(), theme::CLAY),
        "read_tab" | "recent_turns" => (
            format!(
                "◦ read {}",
                if tabs.is_empty() {
                    "a tab".into()
                } else {
                    tabs
                }
            ),
            FAINT,
        ),
        "list_dir" | "read_file" => (
            format!(
                "◦ looked at {}{}",
                if s("path").is_empty() {
                    "the folder".to_string()
                } else {
                    s("path")
                },
                if tabs.is_empty() {
                    String::new()
                } else {
                    format!(" in {tabs}")
                }
            ),
            FAINT,
        ),
        "sessions"
        | "session_detail"
        | "find"
        | "history"
        | "tab_history"
        | "get_state"
        | "account_capabilities"
        | "mcp_catalog"
        | "list_settings"
        | "get_setting"
        | "list_learnings"
        | "get_system_prompt" => (format!("◦ checked {}", tool.replace('_', " ")), FAINT),
        "install_mcp" => (
            format!(
                "⚙ install {}{}",
                if s("source").is_empty() {
                    s("name")
                } else {
                    s("source")
                },
                on_acct(&accounts)
            ),
            theme::MAUVE,
        ),
        "remove_mcp" => (
            format!("⚙ remove {}{}", s("name"), on_acct(&accounts)),
            theme::MAUVE,
        ),
        "connect_mcp" => (
            format!("⚙ sign in to {}{}", s("name"), on_acct(&accounts)),
            theme::MAUVE,
        ),
        "install_plugin" | "remove_plugin" | "enable_plugin" | "disable_plugin" => (
            format!(
                "⚙ {} {}{}",
                tool.trim_end_matches("_plugin").replace('_', " "),
                s("plugin"),
                on_acct(&accounts)
            ),
            theme::MAUVE,
        ),
        "set_setting" => (
            format!(
                "⚙ set {} to {}",
                s("key"),
                args["value"].to_string().trim_matches('"')
            ),
            theme::MAUVE,
        ),
        "add_account" => (format!("⚙ add account {}", s("label")), theme::MAUVE),
        "relogin_account" | "logout_account" => (
            format!("⚙ {} account {accounts}", tool.trim_end_matches("_account")),
            theme::MAUVE,
        ),
        "learn" | "update_learning" | "forget_learning" | "revert_learning" => {
            ("✎ learned rule change".to_string(), theme::SAND)
        }
        "edit_system_prompt" | "revert_prompt" | "reset_section" => {
            ("✎ system prompt change".to_string(), theme::SAND)
        }
        "show" => ("▣ changed the view".to_string(), FAINT),
        "pause_listening" => ("❚❚ paused listening".to_string(), theme::SAND),
        "speak" => ("♪ said something".to_string(), FAINT),
        "speaker" => ("♪ speaker".to_string(), FAINT),
        "assistant_panel" => ("▣ panel".to_string(), FAINT),
        t => (format!("· {}", t.replace('_', " ")), FAINT),
    };
    let st = match status {
        "ok" | "" => String::new(),
        "needs your yes" => " · asks you".into(),
        "opening" => " · opening".into(),
        e => format!(" · ✗ {}", crate::sessions::snippet(e, 60)),
    };
    let color = if st.starts_with(" · ✗") {
        theme::CLAY
    } else {
        color
    };
    (format!("{text}{st}"), color)
}

/// The conversation as rows (logical; wrapped when drawn).
pub fn rows(app: &App, width: u16) -> Vec<Row> {
    let mut out: Vec<Row> = vec![];
    // Index of each send_prompt chip by tab id, for its delivery result.
    let mut by_tab: Vec<(String, usize)> = vec![];
    let label = |t: &str, c: Color| {
        (
            t.to_string(),
            Style::default().fg(c).add_modifier(Modifier::BOLD),
        )
    };
    let mut first = true;
    for (i, e) in app.assistant.log.iter().enumerate() {
        match e.who {
            Who::User => {
                if !first {
                    out.push(row(vec![]));
                }
                out.push(row(vec![
                    label("you ", theme::SAND),
                    (e.text.clone(), Style::default().fg(theme::SAND)),
                ]));
            }
            Who::Reply => {
                let (say, _, rest) =
                    crate::assistant::spoken_part(&e.text, app.cfg.assistant.spoken_sentences);
                out.push(row(vec![
                    label("◆ ", theme::MAUVE),
                    (say, Style::default().fg(FG)),
                ]));
                if let Some(r) = rest {
                    out.push(Row {
                        parts: vec![
                            ("more › ".into(), Style::default().fg(FAINT)),
                            (r, Style::default().fg(DIM)),
                        ],
                        action: None,
                        indent: 2,
                    });
                }
            }
            Who::Preamble => out.push(Row {
                parts: vec![(
                    e.text.clone(),
                    Style::default().fg(FAINT).add_modifier(Modifier::ITALIC),
                )],
                action: None,
                indent: 2,
            }),
            Who::Note => {
                // A delivery result joins the chip it belongs to.
                if let Some(rest) = e.text.strip_prefix("Prompt to ") {
                    if let Some((tab, what)) = rest.split_once(": ") {
                        if let Some(&(_, at)) = by_tab.iter().rev().find(|(t, _)| t == tab) {
                            let c = if what.starts_with("failed") {
                                theme::CLAY
                            } else {
                                theme::SAGE
                            };
                            out[at]
                                .parts
                                .push((format!(" · {what}"), Style::default().fg(c)));
                            continue;
                        }
                    }
                }
                out.push(Row {
                    parts: vec![(e.text.clone(), Style::default().fg(DIM))],
                    action: None,
                    indent: 2,
                });
            }
            Who::Tool => {
                let v: Value = serde_json::from_str(&e.text).unwrap_or(Value::Null);
                let (tool, args, status) = match v.get("tool").and_then(Value::as_str) {
                    Some(t) => (
                        t.to_string(),
                        v["args"].clone(),
                        v["status"].as_str().unwrap_or("").to_string(),
                    ),
                    // An entry from before the chips: its words as they are.
                    None => (
                        e.text.split(' ').next().unwrap_or("").to_string(),
                        Value::Null,
                        String::new(),
                    ),
                };
                let (text, color) = chip(app, &tool, &args, &status);
                let mut parts = vec![(text, Style::default().fg(color))];
                // A question out to a tab: waiting, then answered, in place.
                if tool == "send_prompt" {
                    let q = args["text"].as_str().unwrap_or("");
                    if let Some(f) = app
                        .assistant
                        .follow_ups
                        .iter()
                        .rev()
                        .find(|f| f.question == q)
                    {
                        use crate::app_followup::FuState;
                        let (t, c) = match &f.state {
                            FuState::Waiting => {
                                (format!(" · ⧗ waiting for {}…", f.tab), theme::SAND)
                            }
                            FuState::Answered { at, .. } | FuState::Reported { at, .. } => {
                                (format!(" · ✓ answered {}", at.format("%H:%M")), theme::SAGE)
                            }
                            FuState::TimedOut => (" · no answer in time".into(), FAINT),
                            FuState::Cancelled => (" · stopped waiting".into(), FAINT),
                            FuState::Gone => (" · tab closed".into(), FAINT),
                        };
                        parts.push((t, Style::default().fg(c)));
                    }
                    if let Some(t) = args["tab"].as_str() {
                        by_tab.push((t.to_string(), out.len()));
                    }
                }
                out.push(Row {
                    parts,
                    action: Some(UiAction::AssistantChip(i)),
                    indent: 2,
                });
                if app.assistant.expanded.contains(&i) {
                    let raw = if args.is_null() {
                        e.text.clone()
                    } else {
                        format!("{tool} {args}")
                    };
                    out.push(Row {
                        parts: vec![(raw, Style::default().fg(FAINT))],
                        action: Some(UiAction::AssistantChip(i)),
                        indent: 4,
                    });
                }
            }
        }
        first = false;
    }
    for d in app.deliveries.iter().filter(|d| !d.done()) {
        let (st, _) = d.status();
        out.push(Row {
            parts: vec![(
                format!(
                    "⧗ {st} to {}: {}",
                    crate::control::tab_id(d.uid),
                    crate::sessions::snippet(&d.text, 60)
                ),
                Style::default().fg(theme::SAND),
            )],
            action: None,
            indent: 2,
        });
    }
    if app.assistant.busy {
        let partial = if app.assistant.current.is_empty() {
            "thinking…".to_string()
        } else {
            app.assistant.current.clone()
        };
        out.push(row(vec![
            label("◆ ", theme::MAUVE),
            (
                partial,
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
            ),
        ]));
    }
    if out.is_empty() {
        out.push(row(vec![(
            "Ask anything about your sessions, by voice or typed below: \"what's everyone working on?\", \"approve the one in account two if it's just tests\", \"add Slack to account two\".".into(),
            Style::default().fg(DIM),
        )]));
    }
    let _ = width;
    out
}

/// Wrap a row to physical lines of at most `w` cells (on spaces when it
/// can), keeping each piece's style.
pub fn wrap(r: &Row, w: u16) -> Vec<Vec<(String, Style)>> {
    let w = w.saturating_sub(r.indent).max(4) as usize;
    let mut lines: Vec<Vec<(String, Style)>> = vec![vec![]];
    let mut col = 0usize;
    for (text, st) in &r.parts {
        for word in text.split_inclusive(' ') {
            let ww: usize = word.chars().map(|c| c.width().unwrap_or(0)).sum();
            if col + ww.min(w) > w && col > 0 {
                lines.push(vec![]);
                col = 0;
            }
            // A word longer than the line is cut across lines.
            let mut piece = String::new();
            for ch in word.chars() {
                let cw = ch.width().unwrap_or(0);
                if col + cw > w {
                    lines
                        .last_mut()
                        .expect("one line")
                        .push((std::mem::take(&mut piece), *st));
                    lines.push(vec![]);
                    col = 0;
                }
                piece.push(ch);
                col += cw;
            }
            if !piece.is_empty() {
                lines.last_mut().expect("one line").push((piece, *st));
            }
        }
    }
    lines
}

/// The toolbar's items, in order of importance.
/// The toolbar, in order: in a view other than the conversation, the way
/// back comes first (and is never dropped), then New, then the views.
fn toolbar(app: &App) -> Vec<(String, UiAction, &'static str)> {
    use crate::menus::Cmd;
    use crate::panel_views::PanelView;
    let view = app.panel_view();
    let mut v: Vec<(String, UiAction, &'static str)> = vec![];
    if view != PanelView::Conversation {
        v.push((
            back_label(app, 3),
            UiAction::AssistantConversation,
            "Back to the conversation going on (Esc)",
        ));
    }
    v.push((
        "New".into(),
        UiAction::AssistantNew,
        "New conversation (\"forget that\")",
    ));
    for (pv, label, action, hint) in [
        (
            PanelView::History,
            "History",
            UiAction::AssistantHistory,
            "Saved conversations: read, search, resume, delete",
        ),
        (
            PanelView::Rules,
            "Rules",
            UiAction::Menu(Cmd::AssistantRules),
            "Rules it learned from your corrections",
        ),
        (
            PanelView::Prompt,
            "Prompt",
            UiAction::Menu(Cmd::AssistantPrompt),
            "Its system prompt (view and edit)",
        ),
        (
            PanelView::Admin,
            "Admin",
            UiAction::Menu(Cmd::AssistantAdmin),
            "Admin actions: installs, settings and accounts it changed",
        ),
    ] {
        // The view you are in is not a button to itself.
        if pv != view {
            v.push((label.into(), action, hint));
        }
    }
    v
}

/// "‹ Conversation", with "● 2" when replies came while away; `size` 3
/// is the full label, 2 "‹ Chat", 1 "‹" (narrow panels keep it whole).
pub fn back_label(app: &App, size: u8) -> String {
    let unread = app.unread_replies();
    let dot = match unread {
        0 => String::new(),
        1 => " ●".into(),
        n => format!(" ● {n}"),
    };
    let word = match size {
        3 => "‹ Conversation",
        2 => "‹ Chat",
        _ => "‹",
    };
    format!("{word}{dot}")
}

fn cells(s: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(s) as u16
}

/// The compact status line.
pub fn status_line(app: &App) -> String {
    let mut parts = vec![];
    let days = app.cfg.assistant.memory_days;
    if let Some((_, n)) = app.assistant.memory_count.filter(|_| days > 0) {
        parts.push(format!(
            "memory {n} · {days} day{}",
            if days == 1 { "" } else { "s" }
        ));
    }
    let waiting = app
        .assistant
        .follow_ups
        .iter()
        .filter(|f| f.state == crate::app_followup::FuState::Waiting)
        .count();
    if waiting > 0 {
        parts.push(format!(
            "⧗ {waiting} waiting answer{}",
            if waiting == 1 { "" } else { "s" }
        ));
    }
    if app.admin.jobs > 0 {
        parts.push(format!(
            "⚙ {} admin job{}",
            app.admin.jobs,
            if app.admin.jobs == 1 { "" } else { "s" }
        ));
    }
    if let Some(t) = &app.assistant.last_timing {
        let num = |key: &str| -> Option<f64> {
            let i = t.find(key)? + key.len();
            let n: String = t[i..]
                .chars()
                .skip_while(|c| *c == ' ' || *c == '+')
                .take_while(|c| c.is_ascii_digit())
                .collect();
            n.parse::<f64>().ok().map(|ms| ms / 1000.0)
        };
        match (num("first token"), num("first audio")) {
            (Some(a), Some(b)) => parts.push(format!("last {a:.1} s → {b:.1} s")),
            (Some(a), None) => parts.push(format!("last {a:.1} s")),
            _ => {}
        }
    }
    parts.join(" · ")
}

/// What the assistant is doing, for the title: listening, thinking or
/// speaking.
pub fn state_chip(app: &App) -> Option<&'static str> {
    let speaking = app.voice.engine.as_ref().is_some_and(|v| v.speaking())
        || app.voice.preview.as_ref().is_some_and(|p| p.speaking());
    if speaking {
        Some("▶ speaking")
    } else if app.assistant.busy {
        Some("◌ thinking")
    } else if app.voice.status == crate::voice::VoiceStatus::Listening {
        Some("● listening")
    } else {
        None
    }
}

/// The input as rows of at most `w` cells: its own lines, wrapped.
pub fn input_lines(text: &str, w: usize) -> Vec<String> {
    let mut out = vec![];
    for line in text.split('\n') {
        let mut cur = String::new();
        let mut n = 0;
        for ch in line.chars() {
            let cw = ch.width().unwrap_or(0);
            if n + cw > w && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                n = 0;
            }
            cur.push(ch);
            n += cw;
        }
        out.push(cur);
    }
    out
}

/// The title: "assistant · Claude · Account 2 · 5h 89% · wk 3% · haiku ▾",
/// as parts (tone 2: the binding bucket when it is low). Narrow, whole
/// pieces go: the other buckets, then "assistant", then the provider and
/// the account; the binding bucket and the model stay.
pub fn title_parts(app: &App, room: usize) -> Vec<(String, u8)> {
    let p = crate::providers::by_id(&app.cfg.assistant.provider);
    let model = app
        .assistant
        .brain
        .as_ref()
        .map(|b| b.model.clone())
        .unwrap_or_else(|| crate::providers::model_for(p, &app.cfg.assistant));
    let short_model = if p.id == "claude" {
        model
            .trim_start_matches("claude-")
            .split('-')
            .next()
            .unwrap_or(&model)
            .to_string()
    } else {
        model.clone()
    };
    // (text, tone, drop order: 0 never, lower first).
    let mut segs: Vec<(String, u8, u8)> = vec![("assistant".into(), 0, 2)];
    // Not in the conversation: say where you are (kept on narrow panels).
    let view = app.panel_view();
    if view != crate::panel_views::PanelView::Conversation {
        segs.push((view.name().into(), 1, 0));
    }
    segs.push((p.name.into(), 0, 3));
    let acct = app
        .assistant
        .brain
        .as_ref()
        .and_then(|b| b.account)
        .or_else(|| app.assistant_account());
    match (p.own_home, acct) {
        (Some(_), _) => {}
        (None, Some(a)) => segs.push((app.account_cfg(a).display().to_string(), 0, 4)),
        (None, None) => segs.push(("no logged in account".into(), 0, 0)),
    }
    if let Some(b) = app.assistant_buckets() {
        for k in &b {
            let low = k.binding && k.left_pct < crate::providers::LOW_PCT;
            segs.push((
                crate::providers::bucket_text(k),
                if low { 2 } else { 0 },
                if k.binding { 0 } else { 1 },
            ));
        }
    }
    segs.push((format!("{short_model} ▾"), 0, 0));
    if app.assistant.remote.is_some() {
        segs.push(("RC".into(), 0, 0));
    }
    let width = |segs: &[(String, u8, u8)]| -> usize {
        segs.iter().map(|s| cells(&s.0) as usize).sum::<usize>()
            + 3 * segs.len().saturating_sub(1)
            + 2
    };
    for rank in 1..=4u8 {
        // The rightmost of a rank goes first.
        while width(&segs) > room {
            match segs.iter().rposition(|s| s.2 == rank) {
                Some(i) => {
                    segs.remove(i);
                }
                None => break,
            }
        }
    }
    let mut out: Vec<(String, u8)> = vec![(" ".into(), 0)];
    for (i, (t, tone, _)) in segs.into_iter().enumerate() {
        if i > 0 {
            out.push((" · ".into(), 0));
        }
        out.push((t, tone));
    }
    out.push((" ".into(), 0));
    out
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Clear, area);
    let acct = app
        .assistant
        .brain
        .as_ref()
        .and_then(|b| b.account)
        .or_else(|| app.assistant_account());
    let room = area.width.saturating_sub(6) as usize;
    let title = title_parts(app, room);
    // With the keys, a calm blue border; without, a dim one.
    let focused = app.assistant_has_focus();
    let border = if focused { theme::SLATE } else { FAINT };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(BAR_BG));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 5 || inner.width < 20 {
        return;
    }
    let limit = inner.x + inner.width;
    let buf = f.buffer_mut();
    let mut hits = app.hits.borrow_mut();
    // The title, on the border: a click opens account and model.
    // Anywhere in the panel: it takes the keys (its own buttons win).
    hits.add(
        area,
        UiAction::AssistantFocus,
        "Type to the assistant here (Esc gives the keys back)",
    );
    let tw = (title.iter().map(|(t, _)| cells(t)).sum::<u16>()).min(inner.width.saturating_sub(2));
    let title_st = if focused {
        Style::default()
            .fg(FG)
            .bg(BAR_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM).bg(BAR_BG)
    };
    let mut tx = area.x + 2;
    for (t, tone) in &title {
        let st = match tone {
            2 => title_st.fg(theme::SAND),
            1 => title_st.fg(theme::SLATE).add_modifier(Modifier::BOLD),
            _ => title_st,
        };
        tx = crate::hits::text(buf, tx, area.y, area.x + 2 + tw, t, st);
    }
    // What it is doing, in the same calm accent.
    if let Some(chip) = state_chip(app) {
        tx = crate::hits::text(
            buf,
            tx + 1,
            area.y,
            area.x + area.width.saturating_sub(2),
            &format!(" {chip} "),
            Style::default().fg(BAR_BG).bg(theme::SLATE),
        );
    }
    let _ = tx;
    hits.add(
        Rect::new(area.x + 2, area.y, tw, 1),
        UiAction::MenuOpen(crate::menus::MenuId::AssistantAccount),
        "The account it spends and its model: click to switch",
    );
    // Toolbar: what fits, then ⋯ (everything) and × (close).
    let y = inner.y;
    let tail = cells(" ⋯ ") + 1 + cells(" × ");
    let mut x = inner.x;
    for (k, (mut l, a, hint)) in toolbar(app).into_iter().enumerate() {
        // The way back always fits: shorter, never pushed out.
        if k == 0 && a == UiAction::AssistantConversation {
            for size in [3, 2, 1] {
                l = back_label(app, size);
                if x + cells(&format!(" {l} ")) + 1 + tail <= limit {
                    break;
                }
            }
        }
        let w = cells(&format!(" {l} "));
        if x + w + 1 + tail > limit && !(k == 0 && a == UiAction::AssistantConversation) {
            break;
        }
        x = crate::hits::button(
            buf,
            &mut hits,
            app.mouse_pos,
            x,
            y,
            limit,
            &l,
            a,
            hint,
            theme::SLATE,
        ) + 1;
    }
    let mx = limit - tail;
    let x2 = crate::hits::button(
        buf,
        &mut hits,
        app.mouse_pos,
        mx,
        y,
        limit,
        "⋯",
        UiAction::MenuOpen(crate::menus::MenuId::AssistantMore),
        "More: conversation, rules, prompt, admin, timing, settings",
        theme::SLATE,
    );
    crate::hits::button(
        buf,
        &mut hits,
        app.mouse_pos,
        x2 + 1,
        y,
        limit,
        "×",
        UiAction::Key('.'),
        "Close (Esc, Ctrl-a .)",
        theme::CLAY,
    );
    // Status line.
    let st = status_line(app);
    let note = match acct {
        None => "log in an account to use it".to_string(),
        Some(_) if st.is_empty() => String::new(),
        Some(_) => st,
    };
    crate::hits::text(
        buf,
        inner.x,
        y + 1,
        limit,
        &crate::sessions::snippet(&note, inner.width as usize),
        Style::default().fg(FAINT).bg(BAR_BG),
    );
    let mut top = y + 2;
    if app.assistant.show_details {
        if let Some(t) = &app.assistant.last_timing {
            for l in wrap(
                &row(vec![(t.clone(), Style::default().fg(FAINT))]),
                inner.width,
            ) {
                if top >= inner.y + inner.height - 3 {
                    break;
                }
                let mut cx = inner.x;
                for (s, stl) in l {
                    cx = crate::hits::text(buf, cx, top, limit, &s, stl.bg(BAR_BG));
                }
                top += 1;
            }
        }
    }
    drop(hits);
    if let Some(h) = &app.assistant.history {
        crate::ui::draw_history(
            f,
            app,
            h,
            Rect::new(inner.x, top, inner.width, inner.y + inner.height - top),
        );
        return;
    }
    let buf = f.buffer_mut();
    let mut hits = app.hits.borrow_mut();
    // The input box takes the last row.
    let iy = inner.y + inner.height - 1;
    // The input box grows with what is in it (up to 6 rows, its end shown).
    let send_w = cells(" Send ");
    let mic_label = if app.voice.muted {
        "● muted"
    } else {
        match app.voice_mode() {
            0 => "mic off",
            1 => "push to talk",
            2 => "wake word",
            _ => "● open mic",
        }
    };
    let mic_w = cells(mic_label) + 1;
    // The speaker: a click (or Ctrl-a O) mutes what it says out loud.
    let spk_label = if app.cfg.voice.speaker_muted {
        "● silent"
    } else {
        "sound on"
    };
    let spk_w = cells(spk_label) + 2;
    let right = limit.saturating_sub(send_w + mic_w + spk_w);
    let in_lines = input_lines(
        &app.assistant.input,
        right.saturating_sub(inner.x + 2).max(4) as usize,
    );
    let in_h = (in_lines.len() as u16)
        .clamp(1, 6)
        .min(inner.height.saturating_sub(4).max(1));
    let iy0 = iy + 1 - in_h;
    // A question waiting for the user's yes: Yes / No buttons above the
    // input (a click answers, as typing "yes" would).
    let asking = !app.pending_confirms.is_empty() && !app.assistant.show_admin;
    let ask_y = iy0.saturating_sub(1);
    let body_h = (if asking { ask_y } else { iy0 }).saturating_sub(top + 1);
    let rows: Vec<Row> = if app.assistant.show_admin {
        let mut v = vec![row(vec![(
            "Admin actions".into(),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        )])];
        if app.admin.history.is_empty() {
            v.push(row(vec![(
                "None yet: installs, settings and account changes the assistant makes show here."
                    .into(),
                Style::default().fg(DIM),
            )]));
        }
        for h in &app.admin.history {
            v.push(Row {
                parts: vec![(h.clone(), Style::default().fg(DIM))],
                action: None,
                indent: 2,
            });
        }
        v
    } else {
        // On screen: what came in is seen.
        app.mark_replies_seen();
        rows(app, inner.width)
    };
    let mut physical: Vec<(u16, Vec<(String, Style)>, Option<UiAction>)> = vec![];
    for r in &rows {
        for l in wrap(r, inner.width) {
            physical.push((r.indent, l, r.action.clone()));
        }
    }
    let skip = physical.len().saturating_sub(body_h as usize);
    // What is drawn, for selecting it with the mouse.
    let mut shown: Vec<String> = vec![];
    for (k, (indent, l, action)) in physical.into_iter().skip(skip).enumerate() {
        let ly = top + k as u16;
        let mut cx = inner.x + indent;
        let mut text = " ".repeat(indent as usize);
        for (s, stl) in l {
            text.push_str(&s);
            cx = crate::hits::text(buf, cx, ly, limit, &s, stl.bg(BAR_BG));
        }
        shown.push(text);
        if let Some(a) = action {
            hits.add(
                Rect::new(inner.x, ly, inner.width, 1),
                a,
                "Click to show or hide what it ran",
            );
        }
    }
    if asking {
        for cx in inner.x..limit {
            if let Some(c) = buf.cell_mut((cx, ask_y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(BAR_BG));
            }
        }
        let yes_w = cells(" Yes ") + 1 + cells(" No ") + 1;
        let q = app
            .pending_confirms
            .last()
            .map(|p| p.summary.clone())
            .unwrap_or_default();
        let q = format!("? {q}");
        let qx = crate::hits::text(
            buf,
            inner.x,
            ask_y,
            limit.saturating_sub(yes_w),
            &crate::sessions::snippet(&q, limit.saturating_sub(yes_w + inner.x + 1) as usize),
            Style::default().fg(theme::SAND).bg(BAR_BG),
        );
        let bx = qx.max(inner.x).min(limit.saturating_sub(yes_w)) + 1;
        let nx = crate::hits::button(
            buf,
            &mut hits,
            app.mouse_pos,
            bx,
            ask_y,
            limit,
            "Yes",
            UiAction::AssistantAnswer(true),
            "Yes, go ahead (or type yes)",
            theme::SAGE,
        );
        crate::hits::button(
            buf,
            &mut hits,
            app.mouse_pos,
            nx + 1,
            ask_y,
            limit,
            "No",
            UiAction::AssistantAnswer(false),
            "No, leave it (or type no)",
            theme::STONE,
        );
    }
    let body = Rect::new(inner.x, top, inner.width, body_h);
    if let Some(sel) = app
        .selection
        .as_ref()
        .filter(|s| s.visible() && s.target == crate::select::Target::Assistant)
    {
        let span = crate::select::span(&mut crate::select::TextLines(&shown), sel);
        for r in 0..body.height {
            for c in 0..body.width {
                let at = crate::select::Pos {
                    line: r as usize,
                    col: c,
                };
                if crate::select::contains(span, at) {
                    crate::ui::select_cell(buf, body.x + c, body.y + r);
                }
            }
        }
    }
    *app.assistant_text.borrow_mut() = Some((body, shown));
    // Input: the text (or the hint), the mic state, Send.
    for y in iy0..=iy {
        for cx in inner.x..limit {
            if let Some(c) = buf.cell_mut((cx, y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(SEL_BG));
            }
        }
    }
    let mic = mic_label;
    if app.assistant.input.is_empty() {
        let hint = if focused {
            "› Type or speak…"
        } else {
            "› click or Ctrl-a . to type"
        };
        crate::hits::text(
            buf,
            inner.x,
            iy,
            right,
            hint,
            Style::default().fg(FAINT).bg(SEL_BG),
        );
        if focused {
            app.want_cursor.set(Some((inner.x + 2, iy)));
        }
    } else {
        let shown = &in_lines[in_lines.len() - in_h as usize..];
        for (k, l) in shown.iter().enumerate() {
            let y = iy0 + k as u16;
            let lead = if k == 0 && in_lines.len() <= in_h as usize {
                "› "
            } else {
                "  "
            };
            // The caret is the terminal's own cursor, at the end.
            let end = crate::hits::text(
                buf,
                inner.x,
                y,
                right,
                &format!("{lead}{l}"),
                Style::default().fg(FG).bg(SEL_BG),
            );
            if k + 1 == shown.len() && focused {
                app.want_cursor
                    .set(Some((end.min(right.saturating_sub(1)), y)));
            }
        }
    }
    let spk_style = if app.cfg.voice.speaker_muted {
        Style::default().fg(theme::SAND).bg(SEL_BG)
    } else {
        Style::default().fg(DIM).bg(SEL_BG)
    };
    let spk_hover = app
        .mouse_pos
        .is_some_and(|(mx, my)| my == iy && mx >= right && mx < right + spk_w - 1);
    crate::hits::text(
        buf,
        right,
        iy,
        right + spk_w,
        spk_label,
        if spk_hover {
            spk_style.add_modifier(Modifier::UNDERLINED)
        } else {
            spk_style
        },
    );
    hits.add(
        Rect::new(right, iy, spk_w - 1, 1),
        UiAction::SpeakerToggle,
        if app.cfg.voice.speaker_muted {
            "The speaker is muted: answers show as text only. Click (or Ctrl-a O) to talk again"
        } else {
            "Mute what it says out loud (Ctrl-a O); the mic keeps listening"
        },
    );
    crate::hits::text(
        buf,
        right + spk_w,
        iy,
        right + spk_w + mic_w,
        mic,
        Style::default().fg(DIM).bg(SEL_BG),
    );
    crate::hits::button(
        buf,
        &mut hits,
        app.mouse_pos,
        limit - send_w,
        iy,
        limit,
        "Send",
        UiAction::AssistantSend,
        "Send what you typed (Enter)",
        theme::SAGE,
    );
}
