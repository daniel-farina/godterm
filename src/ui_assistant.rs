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
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::app::App;
use crate::app_assistant::{ChatGeo, Who};
use crate::hits::UiAction;
use crate::input_edit;
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};

/// One logical row of the conversation: styled pieces, and what a click
/// on it does.
pub struct Row {
    pub parts: Vec<(String, Style)>,
    pub action: Option<UiAction>,
    pub indent: u16,
    /// Already laid out to the panel's width (a reply's markdown): drawn
    /// as it is, never wrapped again.
    pub pre: bool,
}

fn row(parts: Vec<(String, Style)>) -> Row {
    Row {
        parts,
        action: None,
        indent: 0,
        pre: false,
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
        "archive_tabs" | "archive_tab" => (
            format!(
                "▫ archived {}",
                if tabs.is_empty() { "tabs".into() } else { tabs }
            ),
            theme::CLAY,
        ),
        "restore_archived" => ("↻ restored archived tabs".to_string(), theme::SAGE),
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
        | "search_projects"
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
                // Its markdown laid out (tables as a grid), the spoken
                // part first, then "more ›" and the rest, in normal text.
                let lines = crate::md::reply_view(
                    &e.text,
                    app.cfg.assistant.spoken_sentences,
                    width.saturating_sub(2) as usize,
                );
                push_md(&mut out, &lines, label("◆ ", theme::MAUVE));
            }
            Who::Preamble => out.push(Row {
                parts: vec![(
                    e.text.clone(),
                    Style::default().fg(FAINT).add_modifier(Modifier::ITALIC),
                )],
                action: None,
                indent: 2,
                pre: false,
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
                    pre: false,
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
                    pre: false,
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
                        pre: false,
                    });
                }
            }
        }
        first = false;
    }
    // Requests to tabs: each one's receipt (tab, first words, state, time).
    let shown: Vec<&crate::control::Delivery> = app
        .deliveries
        .iter()
        .filter(|d| {
            !d.done()
                || (d.rc.notify
                    && (!d.rc.receipt.is_final()
                        || d.rc.resendable()
                        || d.rc.receipt_at.elapsed() < std::time::Duration::from_secs(600)))
        })
        .collect();
    for d in shown.iter().skip(shown.len().saturating_sub(6)) {
        let st = d.state_word();
        let (mark, color) = match st {
            "dropped" | "interrupted" => ("✗", theme::CLAY),
            "started" | "done" => ("✓", DIM),
            _ => ("⧗", theme::SAND),
        };
        let name = app
            .locate_uid(d.uid)
            .map(|(s, t)| app.panes[s].tabs[t].name())
            .unwrap_or_else(|| crate::control::tab_id(d.uid));
        let resend = if d.rc.resendable() {
            " · say resend"
        } else {
            ""
        };
        out.push(Row {
            parts: vec![(
                format!(
                    "{mark} {name} · \"{}\" · {st} · {}{resend}",
                    crate::receipts::first_words(&d.text),
                    d.rc.created_at.format("%H:%M")
                ),
                Style::default().fg(color),
            )],
            action: None,
            indent: 2,
            pre: false,
        });
    }
    // Brainstorm mode's card (first use, or the ⓘ): a click hides it.
    if app.brainstorm.card {
        if !out.is_empty() {
            out.push(row(vec![]));
        }
        let mut lines = vec![(
            "ⓘ Brainstorm mode".to_string(),
            Style::default()
                .fg(theme::SAND)
                .add_modifier(Modifier::BOLD),
        )];
        lines.push(("  (click to hide)".into(), Style::default().fg(FAINT)));
        out.push(Row {
            parts: lines,
            action: Some(UiAction::Menu(crate::menus::Cmd::BrainstormInfo)),
            indent: 0,
            pre: false,
        });
        let body = crate::app_brainstorm::EXPLAIN
            .trim_start_matches("Brainstorm mode: ")
            .to_string();
        let mut cs = body.chars();
        let body = cs
            .next()
            .map(|c| format!("{}{}", c.to_uppercase(), cs.as_str()))
            .unwrap_or_default();
        out.push(Row {
            parts: vec![(body, Style::default().fg(DIM))],
            action: Some(UiAction::Menu(crate::menus::Cmd::BrainstormInfo)),
            indent: 2,
            pre: false,
        });
        out.push(Row {
            parts: vec![(
                "Ctrl-a B turns it on or off; Esc leaves it and keeps the draft.".into(),
                Style::default().fg(FAINT),
            )],
            action: None,
            indent: 2,
            pre: false,
        });
    }
    // Update mode's card (first use, or a click on its chip).
    if app.updates.card {
        if !out.is_empty() {
            out.push(row(vec![]));
        }
        let info = UiAction::Menu(crate::menus::Cmd::UpdatesInfo);
        out.push(Row {
            parts: vec![
                (
                    "ⓘ Update mode".to_string(),
                    Style::default()
                        .fg(theme::SAGE)
                        .add_modifier(Modifier::BOLD),
                ),
                ("  (click to hide)".into(), Style::default().fg(FAINT)),
            ],
            action: Some(info.clone()),
            indent: 0,
            pre: false,
        });
        let body = crate::app_updates::EXPLAIN
            .trim_start_matches("Update mode: ")
            .to_string();
        let mut cs = body.chars();
        let body = cs
            .next()
            .map(|c| format!("{}{}", c.to_uppercase(), cs.as_str()))
            .unwrap_or_default();
        out.push(Row {
            parts: vec![(body, Style::default().fg(DIM))],
            action: Some(info),
            indent: 2,
            pre: false,
        });
        out.push(Row {
            parts: vec![(
                "Voice ▾ > Update mode turns it on or off; with the speaker off the updates show here.".into(),
                Style::default().fg(FAINT),
            )],
            action: None,
            indent: 2,
            pre: false,
        });
    }
    if app.assistant.busy {
        let partial = if app.assistant.current.is_empty() {
            "thinking…".to_string()
        } else {
            app.assistant.current.clone()
        };
        // Streaming: laid out as it comes (not cached, it changes).
        let blocks = crate::md::parse(&partial);
        let lines = crate::md::render(
            &blocks,
            width.saturating_sub(2) as usize,
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        );
        push_md(&mut out, &lines, label("◆ ", theme::MAUVE));
    }
    if out.is_empty() {
        out.push(row(vec![(
            "Ask anything about your sessions, by voice or typed below: \"what's everyone working on?\", \"approve the one in account two if it's just tests\", \"add Slack to account two\".".into(),
            Style::default().fg(DIM),
        )]));
    }
    out
}

/// A reply laid out by `md`: the first line after the ◆ label, the others
/// under it (two cells in), each drawn as it is.
fn push_md(out: &mut Vec<Row>, lines: &[crate::md::Line], mark: (String, Style)) {
    for (k, l) in lines.iter().enumerate() {
        let mut parts = vec![];
        if k == 0 {
            parts.push(mark.clone());
        }
        parts.extend(l.iter().cloned());
        out.push(Row {
            parts,
            action: None,
            indent: if k == 0 { 0 } else { 2 },
            pre: true,
        });
    }
}

/// Wrap a row to physical lines of at most `w` cells (on spaces when it
/// can), keeping each piece's style.
pub fn wrap(r: &Row, w: u16) -> Vec<Vec<(String, Style)>> {
    if r.pre {
        return vec![r.parts.clone()];
    }
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

pub fn cells(s: &str) -> u16 {
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

/// Remote Control and the kept session, at the top of the Admin view.
pub fn remote_rows(app: &App) -> Vec<(String, Option<UiAction>)> {
    let mut v = vec![];
    let keep = app.cfg.assistant.remote_control;
    let line = match &app.assistant.remote {
        Some(r) => format!(
            "Remote Control: on as '{}' · {} · {}",
            r.name,
            r.bridge,
            r.url.as_deref().unwrap_or("no link")
        ),
        None if app.assistant.remote_flow.is_some() => "Remote Control: turning on…".into(),
        None => "Remote Control: off".into(),
    };
    let link = app
        .assistant
        .remote
        .as_ref()
        .and_then(|r| r.url.as_ref())
        .is_some();
    v.push((line, link.then_some(UiAction::AssistantRemoteLink)));
    let kept = if !keep {
        "Keep Remote Control on: off (Settings > Assistant, or say keep remote control on)"
            .to_string()
    } else if app.remote_supported().is_err() {
        "Keep Remote Control on: set, resting (not supported on this provider; Claude only)".into()
    } else {
        "Keep Remote Control on: on (it turns on whenever the assistant starts)".into()
    };
    v.push((kept, None));
    if app.assistant.remote.is_none() {
        if let Some(u) = &app.assistant.remote_last_url {
            v.push((
                format!("Last link: {u}"),
                Some(UiAction::AssistantRemoteLink),
            ));
        }
    }
    v.push((
        format!(
            "Session: {}",
            if !app.cfg.assistant.resume {
                "fresh each start (resume off)".to_string()
            } else {
                match crate::app_resume::load().filter(|k| !k.id.is_empty()) {
                    Some(k) => format!("kept across restarts ({})", crate::paths::head(&k.id, 8)),
                    None => "kept across restarts".into(),
                }
            }
        ),
        None,
    ));
    v
}

/// The account the assistant spends: the running brain's when it is of
/// the chosen provider (a brain left from before a switch does not count),
/// else the one it would start on.
pub fn brain_account(app: &App) -> Option<usize> {
    let p = crate::providers::by_id(&app.cfg.assistant.provider);
    app.assistant
        .brain
        .as_ref()
        .filter(|b| b.provider == p.id)
        .and_then(|b| b.account)
        .or_else(|| app.assistant_account())
}

/// The status row under the toolbar, and whether it is a sign in prompt
/// (then a click starts the login). A provider with its own login (Grok)
/// never needs an account: it asks for its own login only when missing.
pub fn login_note(app: &App, acct: Option<usize>) -> (String, bool) {
    let p = crate::providers::by_id(&app.cfg.assistant.provider);
    match (p.own_home, acct) {
        (Some(home), _) if !crate::harness::grok::logged_in(&home()) => (
            format!(
                "{} needs its own login: click to sign in (or say log in the assistant's {})",
                p.name, p.name
            ),
            true,
        ),
        (None, None) => (format!("log in a {} account to use it", p.name), false),
        _ => (status_line(app), false),
    }
}

/// The title: "assistant · Claude · Account 2 · 5h 89% · wk 3% · haiku ▾",
/// as parts (tone 2: the binding bucket when it is low). Narrow, whole
/// pieces go: the other buckets, then "assistant", then the provider and
/// the account; the binding bucket and the model stay.
pub fn title_parts(app: &App, room: usize) -> Vec<(String, u8)> {
    let p = crate::providers::by_id(&app.cfg.assistant.provider);
    // The chosen model, not the running brain's: right after a switch
    // (even mid answer) the title already names what the next turn uses.
    let model = crate::providers::model_for(p, &app.cfg.assistant);
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
    let acct = brain_account(app);
    match (p.own_home, acct) {
        (Some(home), _) if !crate::harness::grok::logged_in(&home()) => {
            segs.push(("not signed in".into(), 2, 0))
        }
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
    if let Some(r) = app.remote_indicator() {
        segs.push((r, 1, 0));
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
    app.assistant.chat_geo.set(None);
    app.sched_ui.rect.set(Rect::default());
    if app.assistant.win == crate::app_panelwin::PanelWin::Min {
        crate::ui_panelwin::draw_strip(f, app, area);
        app.assistant.input_geo.set(None);
        *app.assistant_text.borrow_mut() = None;
        return;
    }
    let acct = brain_account(app);
    // The title row's right side: the width carets and window controls.
    let room =
        area.width
            .saturating_sub(6 + crate::ui_panelwin::title_tail(app, area.width)) as usize;
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
    let mut remote_at = None;
    for (t, tone) in &title {
        let st = match tone {
            2 => title_st.fg(theme::SAND),
            1 => title_st.fg(theme::SLATE).add_modifier(Modifier::BOLD),
            _ => title_st,
        };
        let x0 = tx;
        tx = crate::hits::text(buf, tx, area.y, area.x + 2 + tw, t, st);
        if t.starts_with("Remote") {
            remote_at = Some((x0, tx.saturating_sub(x0)));
        }
    }
    // Right: the width carets and the window controls; the edge drags.
    let ctl_x = crate::ui_panelwin::draw_title_controls(buf, &mut hits, app, area);
    crate::ui_panelwin::edge_region(&mut hits, app, area);
    // What it is doing, in the same calm accent.
    if let Some(chip) = state_chip(app) {
        tx = crate::hits::text(
            buf,
            tx + 1,
            area.y,
            ctl_x.saturating_sub(1),
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
    if let Some((x, w)) = remote_at.filter(|(_, w)| *w > 0) {
        hits.add(
            Rect::new(x, area.y, w, 1),
            UiAction::AssistantRemoteLink,
            "Remote Control: click to open its claude.ai link (also copied)",
        );
    }
    // Toolbar: what fits, then ⋯ (everything; close is on the title row).
    let y = inner.y;
    let tail = cells(" ⋯ ");
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
    crate::hits::button(
        buf,
        &mut hits,
        app.mouse_pos,
        mx,
        y,
        limit,
        "⋯",
        UiAction::MenuOpen(crate::menus::MenuId::AssistantMore),
        "More: conversation, rules, prompt, admin, timing, panel width, settings",
        theme::SLATE,
    );
    // Status line (direct mode says where the voice goes instead).
    if let Some(d) = &app.direct.on {
        let line = format!("● mic at {} · click to bring back", d.name);
        crate::hits::text(
            buf,
            inner.x,
            y + 1,
            limit,
            &crate::sessions::snippet(&line, inner.width as usize),
            Style::default().fg(theme::SAGE).bg(BAR_BG),
        );
        hits.add(
            Rect::new(inner.x, y + 1, inner.width, 1),
            UiAction::MicToAssistant,
            "Back to the assistant (Ctrl-a K, Esc here, or say \"back to the assistant\")",
        );
    }
    let (note, login) = login_note(app, acct);
    if app.direct.on.is_none() {
        if login {
            hits.add(
                Rect::new(inner.x, y + 1, inner.width, 1),
                UiAction::Menu(crate::menus::Cmd::AssistantLogin),
                "Sign the assistant's own Grok in (your Grok accounts are not touched)",
            );
        }
        crate::hits::text(
            buf,
            inner.x,
            y + 1,
            limit,
            &crate::sessions::snippet(&note, inner.width as usize),
            Style::default()
                .fg(if login { theme::SAND } else { FAINT })
                .bg(BAR_BG),
        );
    }
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
        app.assistant.input_geo.set(None);
        return;
    }
    let buf = f.buffer_mut();
    let mut hits = app.hits.borrow_mut();
    // Loops & schedules above the conversation: the composer (with its
    // frame) and four rows of conversation stay.
    let top = {
        let avail = (inner.y + inner.height).saturating_sub(top + 5 + 4);
        top + crate::ui_schedules::draw(
            buf,
            &mut hits,
            app,
            inner.x,
            top,
            inner.width,
            avail,
            inner.height,
        )
    };
    // The composer: a framed field at the bottom, 3 rows tall, growing
    // with what is in it up to assistant.input_rows (its end shown).
    // Inside the frame: one cell of padding, the "› " lead, the text.
    let bottom = inner.y + inner.height;
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
    // The speaker: a click (or Ctrl-a O) mutes what it says out loud.
    let spk_label = if app.cfg.voice.speaker_muted {
        "● silent"
    } else {
        "sound on"
    };
    let tx = inner.x + 4;
    let right = limit.saturating_sub(2);
    // The input wrapped, the caret's row and column in it, and the rows
    // shown (scrolled to keep the caret in view).
    let in_text = app.assistant.input.as_str();
    let in_w = right.saturating_sub(tx).max(4) as usize;
    let in_rows = input_edit::rows(in_text, in_w);
    let (caret, sel) = app.assistant.caret.view(in_text);
    let (c_row, c_col) = input_edit::caret_rc(in_text, &in_rows, caret, in_w);
    let in_total = in_rows.len().max(c_row + 1);
    let grow_to = app
        .cfg
        .assistant
        .input_rows
        .max(crate::app_panelwin::INPUT_MIN_ROWS);
    // Three rows of conversation stay above it (and its frame).
    let fit = bottom.saturating_sub(top + 3 + 2).max(1);
    let in_h = (in_total as u16)
        .max(crate::app_panelwin::INPUT_MIN_ROWS)
        .min(grow_to)
        .min(fit);
    let in_top = {
        let h = in_h as usize;
        let mut t = app
            .assistant
            .input_top
            .get()
            .min(in_total.saturating_sub(h));
        if c_row < t {
            t = c_row;
        } else if c_row >= t + h {
            t = c_row + 1 - h;
        }
        app.assistant.input_top.set(t);
        t
    };
    let frame_bot = bottom - 1;
    let iy0 = frame_bot - in_h;
    let frame_top = iy0 - 1;
    app.assistant
        .input_geo
        .set(Some(crate::app_input::InputGeo {
            x0: inner.x + 1,
            right: limit.saturating_sub(1),
            tx,
            y: iy0,
            h: in_h,
            w: in_w,
            top: in_top,
        }));
    // A question waiting for the user's yes: Yes / No buttons above the
    // input (a click answers, as typing "yes" would).
    let asking = (!app.pending_confirms.is_empty() || app.assistant.selfswitch.asked.is_some())
        && !app.assistant.show_admin;
    // The quick bar above the input (and the question): it goes first
    // when the panel is short.
    let free = frame_top.saturating_sub(top + u16::from(asking));
    let quick_h = crate::ui_panelwin::quick_rows(free);
    let quick_y = frame_top - quick_h;
    let ask_y = quick_y.saturating_sub(1);
    let body_h = (if asking { ask_y } else { quick_y }).saturating_sub(top);
    let rows: Vec<Row> = if app.assistant.show_admin {
        let mut v = vec![row(vec![(
            "Admin actions".into(),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        )])];
        for (line, action) in remote_rows(app) {
            v.push(Row {
                parts: vec![(line, Style::default().fg(DIM))],
                action,
                indent: 0,
                pre: false,
            });
        }
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
                pre: false,
            });
        }
        v
    } else {
        // On screen: what came in is seen.
        app.mark_replies_seen();
        rows(app, inner.width)
    };
    // The chat's scrollbar sits on the panel's right border, so the text
    // keeps its whole width.
    let chat_w = inner.width;
    let tlimit = limit;
    #[allow(clippy::type_complexity)]
    let mut physical: Vec<(u16, Vec<(String, Style)>, Option<UiAction>)> = vec![];
    for r in &rows {
        for l in wrap(r, chat_w) {
            physical.push((r.indent, l, r.action.clone()));
        }
    }
    // Scrolled back: the same lines stay in view as new ones arrive.
    let total = physical.len();
    let view = body_h as usize;
    let max_off = total.saturating_sub(view);
    let mut off = app.assistant.chat_off.get();
    if off > 0 {
        off += total.saturating_sub(app.assistant.chat_seen.get());
    }
    let off = off.min(max_off);
    app.assistant.chat_off.set(off);
    app.assistant.chat_seen.set(total);
    let skip = max_off - off;
    // What is drawn, for selecting it with the mouse.
    let mut shown: Vec<String> = vec![];
    for (k, (indent, l, action)) in physical.into_iter().skip(skip).take(view).enumerate() {
        let ly = top + k as u16;
        let mut cx = inner.x + indent;
        let mut text = " ".repeat(indent as usize);
        for (s, stl) in l {
            text.push_str(&s);
            cx = crate::hits::text(
                buf,
                cx,
                ly,
                tlimit,
                &s,
                if stl.bg.is_some() {
                    stl
                } else {
                    stl.bg(BAR_BG)
                },
            );
        }
        shown.push(text);
        if let Some(a) = action {
            let hint = if a == UiAction::AssistantRemoteLink {
                "Click to open the Remote Control link (also copied)"
            } else {
                "Click to show or hide what it ran"
            };
            hits.add(Rect::new(inner.x, ly, inner.width, 1), a, hint);
        }
    }
    // The chat's scrollbar: the right border is its track and a muted thumb
    // shows where the view is; dragging it or the wheel scrolls the chat
    // only.
    let geo = ChatGeo {
        area: Rect::new(inner.x, top, inner.width + 1, body_h),
        total,
        view,
    };
    if body_h > 0 && chat_w > 0 {
        app.assistant.chat_geo.set(Some(geo));
        if let Some((ts, tl)) = crate::scrollbar::thumb(body_h, total, view, geo.top(off)) {
            // The track is the border itself; the thumb a muted block on it.
            let bx = limit;
            for k in ts..(ts + tl).min(body_h) {
                if let Some(c) = buf.cell_mut((bx, top + k)) {
                    c.set_char('█');
                    c.set_style(Style::default().fg(theme::STONE).bg(BAR_BG));
                }
            }
            hits.add(
                geo.bar(),
                UiAction::ChatBar,
                "Drag to scroll the chat (the wheel works over it too)",
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
            .or_else(|| {
                app.assistant
                    .selfswitch
                    .asked
                    .as_ref()
                    .map(|q| q.question.clone())
            })
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
    let body = Rect::new(inner.x, top, chat_w, body_h);
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
    // The quick actions.
    if quick_h > 0 {
        crate::ui_panelwin::draw_quick(buf, &mut hits, app, inner.x, quick_y, limit, quick_h);
    }
    // The composer: its frame (calm slate with the keys, faint without),
    // the field's own background inside, the text (or the placeholder),
    // and the mic state, the speaker and Send on the frame's bottom edge.
    let frame_st = Style::default()
        .fg(if focused { theme::SLATE } else { FAINT })
        .bg(BAR_BG);
    let fx1 = limit - 1;
    for y in frame_top..=frame_bot {
        for cx in inner.x..limit {
            let edge_row = y == frame_top || y == frame_bot;
            let ch = match (cx == inner.x, cx == fx1, y == frame_top, y == frame_bot) {
                (true, _, true, _) => '╭',
                (_, true, true, _) => '╮',
                (true, _, _, true) => '╰',
                (_, true, _, true) => '╯',
                (true, _, _, _) | (_, true, _, _) => '│',
                _ if edge_row => '─',
                _ => ' ',
            };
            if let Some(c) = buf.cell_mut((cx, y)) {
                c.set_char(ch);
                if ch == ' ' {
                    c.set_style(Style::default().bg(SEL_BG));
                } else {
                    c.set_style(frame_st);
                }
            }
        }
    }
    // Brainstorm mode: its chip on the frame's top edge.
    let mut chip_x = inner.x + 2;
    if let Some(chip) = app.brainstorm_chip() {
        let s = format!(" {chip} ");
        let cx = inner.x + 2;
        let end = fx1.saturating_sub(1);
        let x2 = crate::hits::text(
            buf,
            cx,
            frame_top,
            end,
            &s,
            Style::default()
                .fg(theme::SAND)
                .bg(BAR_BG)
                .add_modifier(Modifier::BOLD),
        );
        hits.add(
            Rect::new(cx, frame_top, x2.saturating_sub(cx), 1),
            UiAction::Key('B'),
            "Brainstorm mode: nothing is sent until you say send it or press Enter. Click (or Esc) to leave it; the draft stays",
        );
        chip_x = x2 + 1;
    }
    // Update mode: its chip next to it.
    if let Some(chip) = app.updates_chip() {
        let s = format!(" {chip} ");
        let end = fx1.saturating_sub(1);
        if chip_x < end {
            let x2 = crate::hits::text(
                buf,
                chip_x,
                frame_top,
                end,
                &s,
                Style::default()
                    .fg(theme::SAGE)
                    .bg(BAR_BG)
                    .add_modifier(Modifier::BOLD),
            );
            hits.add(
                Rect::new(chip_x, frame_top, x2.saturating_sub(chip_x), 1),
                UiAction::Menu(crate::menus::Cmd::UpdatesInfo),
                "Update mode: progress of the active projects is said as it moves. Click for how it works; say \"stop updates\" to end it",
            );
        }
    }
    let field = Style::default().bg(SEL_BG);
    if app.assistant.input.is_empty() {
        crate::hits::text(
            buf,
            tx - 2,
            iy0,
            right,
            "› ",
            Style::default().fg(FAINT).bg(SEL_BG),
        );
        let lines = wrap(
            &row(vec![(
                if app.brainstorm_on() {
                    PLACEHOLDER_BRAINSTORM
                } else if focused {
                    PLACEHOLDER
                } else {
                    PLACEHOLDER_AWAY
                }
                .to_string(),
                Style::default().fg(FAINT).bg(SEL_BG),
            )]),
            in_w as u16,
        );
        for (k, l) in lines.into_iter().take(in_h as usize).enumerate() {
            let mut cx = tx;
            for (s, st) in l {
                cx = crate::hits::text(buf, cx, iy0 + k as u16, right, &s, st);
            }
        }
        if focused {
            app.want_cursor.set(Some((tx, iy0)));
        }
    } else {
        let style = field.fg(FG);
        for k in 0..in_h as usize {
            let r = in_top + k;
            let y = iy0 + k as u16;
            if r == 0 {
                crate::hits::text(buf, tx - 2, y, right, "› ", style);
            }
            let Some(row) = in_rows.get(r) else {
                continue;
            };
            // Grapheme by grapheme (wide ones take two cells), the
            // selection on a calm highlight.
            let mut x = tx;
            for (i, g) in in_text[row.start..row.end].grapheme_indices(true) {
                let at = row.start + i;
                let gw = input_edit::gw(g) as u16;
                if gw == 0 {
                    continue;
                }
                if x + gw > right {
                    break;
                }
                buf.set_stringn(x, y, g, gw as usize, style);
                if sel.is_some_and(|(a, b)| at >= a && at < b) {
                    for dx in 0..gw {
                        crate::ui::select_cell(buf, x + dx, y);
                    }
                }
                x += gw;
            }
            // A selected line break: one cell after the line.
            if !row.soft && x < right && sel.is_some_and(|(a, b)| row.end >= a && row.end < b) {
                crate::ui::select_cell(buf, x, y);
            }
        }
        // The caret is the terminal's own cursor, at its row and column.
        if focused {
            let y = iy0 + (c_row - in_top) as u16;
            let x = (tx + c_col as u16).min(right.saturating_sub(1));
            app.want_cursor.set(Some((x, y)));
        }
    }
    // The bottom edge: Send at the right; the speaker and the mic state
    // at the left while they fit.
    let send_x = fx1.saturating_sub(1 + send_w);
    let mut ex = inner.x + 2;
    let spk_w = cells(spk_label) + 2;
    if ex + spk_w + 1 < send_x {
        let spk_style = if app.cfg.voice.speaker_muted {
            Style::default().fg(theme::SAND).bg(BAR_BG)
        } else {
            Style::default().fg(DIM).bg(BAR_BG)
        };
        let rect = Rect::new(ex, frame_bot, spk_w, 1);
        let spk_hover = app
            .mouse_pos
            .is_some_and(|(mx, my)| crate::hits::contains(rect, mx, my));
        crate::hits::text(
            buf,
            ex,
            frame_bot,
            ex + spk_w,
            &format!(" {spk_label} "),
            if spk_hover {
                spk_style.add_modifier(Modifier::UNDERLINED)
            } else {
                spk_style
            },
        );
        hits.add(
            rect,
            UiAction::SpeakerToggle,
            if app.cfg.voice.speaker_muted {
                "The speaker is muted: answers show as text only. Click (or Ctrl-a O) to talk again"
            } else {
                "Mute what it says out loud (Ctrl-a O); the mic keeps listening"
            },
        );
        ex += spk_w + 1;
    }
    let mic_w = cells(mic_label) + 2;
    if ex + mic_w + 1 < send_x {
        crate::hits::text(
            buf,
            ex,
            frame_bot,
            ex + mic_w,
            &format!(" {mic_label} "),
            Style::default()
                .fg(if app.voice.muted {
                    theme::MUTED_RED
                } else {
                    DIM
                })
                .bg(BAR_BG),
        );
    }
    crate::hits::button(
        buf,
        &mut hits,
        app.mouse_pos,
        send_x,
        frame_bot,
        fx1,
        "Send",
        UiAction::AssistantSend,
        "Send what you typed (Enter; Shift+Enter is a new line)",
        theme::SAGE,
    );
}

/// The empty input's words.
pub const PLACEHOLDER: &str = "Type or speak…  (Enter sends, Shift+Enter new line)";
/// While brainstorming, before anything is said.
pub const PLACEHOLDER_BRAINSTORM: &str =
    "Brainstorming: talk as long as you like; say \"send it\" or press Enter to send";
/// The same without the keys: how to give them to it.
pub const PLACEHOLDER_AWAY: &str = "click or Ctrl-a . to type  (Enter sends, Shift+Enter new line)";
