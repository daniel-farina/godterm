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
fn toolbar(app: &App) -> Vec<(&'static str, UiAction, &'static str)> {
    use crate::menus::Cmd;
    vec![
        (
            "New",
            UiAction::AssistantNew,
            "New conversation (\"forget that\")",
        ),
        (
            if app.assistant.history.is_some() {
                "Chat"
            } else {
                "History"
            },
            UiAction::AssistantHistory,
            "Saved conversations: read, search, resume, delete",
        ),
        (
            "Rules",
            UiAction::Menu(Cmd::AssistantRules),
            "Rules it learned from your corrections",
        ),
        (
            "Prompt",
            UiAction::Menu(Cmd::AssistantPrompt),
            "Its system prompt (view and edit)",
        ),
        (
            "Admin",
            UiAction::Menu(Cmd::AssistantAdmin),
            "Admin actions: installs, settings and accounts it changed",
        ),
    ]
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

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Clear, area);
    let acct = app
        .assistant
        .brain
        .as_ref()
        .and_then(|b| b.account)
        .or_else(|| app.assistant_account());
    let model = app
        .assistant
        .brain
        .as_ref()
        .map(|b| b.model.clone())
        .unwrap_or_else(|| app.cfg.assistant.model.clone());
    let short_model = model
        .trim_start_matches("claude-")
        .split('-')
        .next()
        .unwrap_or(&model)
        .to_string();
    let room = area.width.saturating_sub(6) as usize;
    let title = match acct {
        Some(a) => {
            let name = app.account_cfg(a).display().to_string();
            let left = app.accounts[a]
                .effective_left()
                .map(|l| format!(" · {l:.0}% left"))
                .unwrap_or_default();
            let full = format!(" assistant · {name}{left} · {short_model} ▾ ");
            // Narrow: the usage goes first, then the word "assistant".
            if cells(&full) as usize <= room {
                full
            } else if cells(&format!(" assistant · {name} · {short_model} ▾ ")) as usize <= room
            {
                format!(" assistant · {name} · {short_model} ▾ ")
            } else {
                format!(" {name} · {short_model} ▾ ")
            }
        }
        None => {
            let p = crate::providers::by_id(&app.cfg.assistant.provider);
            match p.own_home {
                Some(_) => format!(
                    " assistant · {} · {} ▾ ",
                    p.name,
                    crate::providers::model_for(p, &app.cfg.assistant)
                ),
                None => " assistant · no logged in account ▾ ".into(),
            }
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::MAUVE))
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
    let tw = cells(&title).min(inner.width.saturating_sub(2));
    crate::hits::text(
        buf,
        area.x + 2,
        area.y,
        area.x + 2 + tw,
        &title,
        Style::default().fg(FG).bg(BAR_BG),
    );
    hits.add(
        Rect::new(area.x + 2, area.y, tw, 1),
        UiAction::MenuOpen(crate::menus::MenuId::AssistantAccount),
        "The account it spends and its model: click to switch",
    );
    // Toolbar: what fits, then ⋯ (everything) and × (close).
    let y = inner.y;
    let tail = cells(" ⋯ ") + 1 + cells(" × ");
    let mut x = inner.x;
    for (l, a, hint) in toolbar(app) {
        let w = cells(&format!(" {l} "));
        if x + w + 1 + tail > limit {
            break;
        }
        x = crate::hits::button(
            buf,
            &mut hits,
            app.mouse_pos,
            x,
            y,
            limit,
            l,
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
    let body_h = iy.saturating_sub(top + 1);
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
        rows(app, inner.width)
    };
    let mut physical: Vec<(u16, Vec<(String, Style)>, Option<UiAction>)> = vec![];
    for r in &rows {
        for l in wrap(r, inner.width) {
            physical.push((r.indent, l, r.action.clone()));
        }
    }
    let skip = physical.len().saturating_sub(body_h as usize);
    for (k, (indent, l, action)) in physical.into_iter().skip(skip).enumerate() {
        let ly = top + k as u16;
        let mut cx = inner.x + indent;
        for (s, stl) in l {
            cx = crate::hits::text(buf, cx, ly, limit, &s, stl.bg(BAR_BG));
        }
        if let Some(a) = action {
            hits.add(
                Rect::new(inner.x, ly, inner.width, 1),
                a,
                "Click to show or hide what it ran",
            );
        }
    }
    // Input: the text (or the hint), the mic state, Send.
    for cx in inner.x..limit {
        if let Some(c) = buf.cell_mut((cx, iy)) {
            c.set_char(' ');
            c.set_style(Style::default().bg(SEL_BG));
        }
    }
    let mic = if app.voice.muted {
        "● muted"
    } else {
        match app.voice_mode() {
            0 => "mic off",
            1 => "push to talk",
            2 => "wake word",
            _ => "● open mic",
        }
    };
    let send_w = cells(" Send ");
    let mic_w = cells(mic) + 1;
    let right = limit.saturating_sub(send_w + mic_w);
    let shown = if app.assistant.input.is_empty() {
        "Type or speak…".to_string()
    } else {
        // The end of a long input stays visible.
        let room = right.saturating_sub(inner.x + 3) as usize;
        let n = app.assistant.input.chars().count();
        let tail: String = app
            .assistant
            .input
            .chars()
            .skip(n.saturating_sub(room.saturating_sub(1)))
            .collect();
        format!("{tail}▏")
    };
    let st = if app.assistant.input.is_empty() {
        Style::default().fg(FAINT).bg(SEL_BG)
    } else {
        Style::default().fg(FG).bg(SEL_BG)
    };
    crate::hits::text(buf, inner.x, iy, right, &format!("› {shown}"), st);
    crate::hits::text(
        buf,
        right,
        iy,
        right + mic_w,
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
