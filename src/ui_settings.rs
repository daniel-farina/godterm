//! The Settings screen: sections on the left, clickable controls on the right.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::hits::{button, contains, text, List, UiAction};
use crate::settings::{self, Kind, Section, SECTIONS};
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};

fn kb(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} GB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.0} MB", n as f64 / 1024.0)
    } else {
        format!("{n} KB")
    }
}

pub fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FAINT))
        .title(Span::styled(
            " Settings ",
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    if inner.width < 30 || inner.height < 6 {
        return;
    }
    let left_w = 16u16;
    let right = Rect::new(
        inner.x + left_w + 1,
        inner.y,
        inner.width - left_w - 1,
        inner.height,
    );
    let buf = f.buffer_mut();
    // Section list.
    for (i, (_, name)) in SECTIONS.iter().enumerate() {
        let y = inner.y + 1 + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let sel = i == app.settings_section;
        let r = Rect::new(inner.x, y, left_w, 1);
        let hovered = app.mouse_pos.is_some_and(|(c, rr)| contains(r, c, rr));
        let st = if sel {
            Style::default()
                .fg(FG)
                .bg(SEL_BG)
                .add_modifier(Modifier::BOLD)
        } else if hovered {
            Style::default().fg(theme::SAND)
        } else {
            Style::default().fg(DIM)
        };
        for x in r.x..r.x + r.width {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char(' ');
                c.set_style(st);
            }
        }
        text(buf, r.x, y, r.x + r.width, &format!(" {name}"), st);
        app.hits.borrow_mut().add(
            r,
            UiAction::Row(List::SettingsSection, i),
            format!("{name} settings"),
        );
    }
    for y in inner.y..inner.y + inner.height {
        if let Some(c) = buf.cell_mut((inner.x + left_w, y)) {
            c.set_char('│');
            c.set_style(Style::default().fg(FAINT));
        }
    }
    // Bottom bar of the left column.
    let by = inner.y + inner.height - 1;
    let mut hits = app.hits.borrow_mut();
    button(
        buf,
        &mut hits,
        app.mouse_pos,
        inner.x,
        by,
        inner.x + left_w,
        "Edit file",
        UiAction::OpenConfig,
        "Open config.toml in $EDITOR (e); changes apply when saved",
        theme::SLATE,
    );
    drop(hits);
    match app.settings_section_kind() {
        Section::Keys => draw_keys(f, right),
        Section::About => draw_about(f, app, right),
        sec => draw_rows(f, app, right, sec),
    }
}

fn draw_rows(f: &mut Frame, app: &App, area: Rect, sec: Section) {
    let (rows, lines) = app.settings_view();
    let buf = f.buffer_mut();
    // Labels are never cut: the column is as wide as the longest one (up
    // to 60% of the width), then the values.
    let longest = rows
        .iter()
        .map(|s| s.label.chars().count() as u16 + 3)
        .max()
        .unwrap_or(20);
    let label_w = longest.clamp(20, (area.width * 3 / 5).max(20));
    let help_h = 3u16;
    let list_h = area.height.saturating_sub(
        help_h
            + 1
            + u16::from(sec == Section::Memory || sec == Section::Accounts) * 5
            + u16::from(sec == Section::Voice) * 4,
    );
    let sel = app.settings_sel.min(rows.len().saturating_sub(1));
    let limit = area.x + area.width;
    // The search box, when searching.
    let mut top = area.y + 1;
    if let Some(q) = &app.settings_query {
        text(
            buf,
            area.x + 1,
            area.y,
            limit,
            &format!(
                "/ {q}▏   {} across all sections · Esc ends the search",
                crate::control::plural(rows.len(), "match")
            ),
            Style::default().fg(theme::SAND),
        );
        top = area.y + 1;
    } else if !rows.is_empty() {
        text(
            buf,
            area.x + 1,
            area.y,
            limit,
            "/ search every setting",
            Style::default().fg(FAINT),
        );
    }
    // Lines: headers, rows, and a dim hint under the selected row.
    enum L<'a> {
        Head(&'a crate::app_settings::SettingsLine),
        Row(usize),
        Hint(usize),
    }
    let mut ls: Vec<L> = vec![];
    for l in &lines {
        match l {
            crate::app_settings::SettingsLine::Row(i) => {
                ls.push(L::Row(*i));
                if *i == sel {
                    ls.push(L::Hint(*i));
                }
            }
            h => ls.push(L::Head(h)),
        }
    }
    let cap = list_h as usize;
    let at = ls.iter().position(|l| matches!(l, L::Hint(_))).unwrap_or(0);
    let first = (at + 1).saturating_sub(cap);
    let mut placed: Vec<(u16, usize)> = vec![];
    for (k, l) in ls.iter().skip(first).take(cap).enumerate() {
        let y = top + k as u16;
        match l {
            L::Row(i) => placed.push((y, *i)),
            L::Hint(i) => {
                let s = &rows[*i];
                let mut h = if s.key == settings::Key::Global("voice.grok.source") {
                    app.grok_sources_hint()
                } else {
                    s.help.to_string()
                };
                if let Some(w) = &s.hidden {
                    h = format!("hidden now: {w} · {h}");
                }
                let room = (limit.saturating_sub(area.x + 3)) as usize;
                text(
                    buf,
                    area.x + 3,
                    y,
                    limit,
                    &crate::sessions::snippet(&h, room.max(10)),
                    Style::default().fg(FAINT),
                );
            }
            L::Head(crate::app_settings::SettingsLine::Header {
                title,
                group,
                collapsed,
            }) => {
                let mark = if *collapsed { "▸" } else { "▾" };
                let t = format!("{mark} {title}");
                let x = text(
                    buf,
                    area.x + 1,
                    y,
                    limit,
                    &t,
                    Style::default()
                        .fg(theme::SAND)
                        .add_modifier(Modifier::BOLD),
                );
                for xx in x + 1..limit {
                    if let Some(c) = buf.cell_mut((xx, y)) {
                        c.set_char('─');
                        c.set_style(Style::default().fg(FAINT));
                    }
                }
                if !group.is_empty() {
                    app.hits.borrow_mut().add(
                        Rect::new(area.x, y, area.width, 1),
                        UiAction::SettingsGroup(group),
                        if *collapsed {
                            "Show these settings"
                        } else {
                            "Fold these settings away"
                        },
                    );
                }
            }
            L::Head(_) => {}
        }
    }
    for (y, i) in placed {
        let s = &rows[i];
        let selected = i == sel;
        let row = Rect::new(area.x, y, area.width, 1);
        let bg = if selected { SEL_BG } else { Color::Reset };
        for x in area.x..limit {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(bg));
            }
        }
        let lst = if s.soon {
            Style::default().fg(FAINT).bg(bg)
        } else {
            Style::default().fg(FG).bg(bg)
        };
        let lst = if s.hidden.is_some() {
            Style::default().fg(DIM).bg(bg)
        } else {
            lst
        };
        let mut x = text(buf, area.x + 2, y, area.x + label_w, &s.label, lst);
        if s.hidden.is_some() {
            text(
                buf,
                x + 1,
                y,
                area.x + label_w + 8,
                "hidden",
                Style::default().fg(theme::STONE).bg(bg),
            );
        }
        if s.soon {
            text(
                buf,
                x + 1,
                y,
                area.x + label_w,
                "soon",
                Style::default().fg(theme::SAND).bg(bg),
            );
        }
        app.hits
            .borrow_mut()
            .add(row, UiAction::Row(List::Settings, i), s.help);
        x = area.x + label_w + 1;
        // The Grok login: who, and what it has left for voice.
        let shown = if s.key == settings::Key::Global("voice.grok.source") {
            app.grok_source_line(&settings::display(&app.cfg, s))
        } else {
            settings::display(&app.cfg, s)
        };
        let mut hits = app.hits.borrow_mut();
        // The Grok login: who and what is left, and Change… for the picker.
        let kind = if s.key == settings::Key::Global("voice.grok.source") {
            x = text(
                buf,
                x,
                y,
                limit.saturating_sub(20),
                &shown,
                Style::default().fg(FG).bg(bg),
            );
            &Kind::Toggle
        } else {
            &s.kind
        };
        let grok_row = s.key == settings::Key::Global("voice.grok.source");
        match kind {
            _ if grok_row => {}
            Kind::Toggle => {
                let on = shown == "on";
                let label = if s.inherit && shown == "inherit" {
                    "inherit"
                } else if on {
                    "● on "
                } else {
                    "○ off"
                };
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x,
                    y,
                    limit,
                    label,
                    UiAction::SettingStep(i, 1),
                    "Toggle (Enter)",
                    if on { theme::SAGE } else { theme::STONE },
                );
            }
            Kind::Choice(_) | Kind::Pick(_) => {
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x,
                    y,
                    limit,
                    "‹",
                    UiAction::SettingStep(i, -1),
                    "Previous value (Left)",
                    theme::SLATE,
                );
                let v = if shown.is_empty() {
                    "default".to_string()
                } else {
                    shown.clone()
                };
                let color = if v == "bypass" {
                    Color::Rgb(160, 92, 78)
                } else {
                    FG
                };
                x = text(
                    buf,
                    x,
                    y,
                    limit,
                    &format!(" {v:<12} "),
                    Style::default()
                        .fg(color)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                );
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x,
                    y,
                    limit,
                    "›",
                    UiAction::SettingStep(i, 1),
                    "Next value (Right)",
                    theme::SLATE,
                );
            }
            Kind::Number { .. } | Kind::Float { .. } => {
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x,
                    y,
                    limit,
                    "-",
                    UiAction::SettingStep(i, -1),
                    "Less (Left)",
                    theme::SLATE,
                );
                x = text(
                    buf,
                    x,
                    y,
                    limit,
                    &format!(" {shown:>8} "),
                    Style::default().fg(FG).bg(bg).add_modifier(Modifier::BOLD),
                );
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x,
                    y,
                    limit,
                    "+",
                    UiAction::SettingStep(i, 1),
                    "More (Right)",
                    theme::SLATE,
                );
            }
            Kind::Secret => {
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x,
                    y,
                    limit,
                    &shown,
                    UiAction::SettingEdit(i),
                    "Type or paste the key (masked; saved to the Keychain)",
                    theme::SLATE,
                );
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x + 1,
                    y,
                    limit.saturating_sub(9),
                    "Test",
                    UiAction::GrokTest,
                    "Check the key with xAI (lists the voices; no audio, no cost)",
                    theme::MAUVE,
                );
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x + 1,
                    y,
                    limit.saturating_sub(9),
                    "Remove key",
                    UiAction::GrokRemoveKey,
                    "Delete the key from the Keychain",
                    theme::CLAY,
                );
            }
            Kind::Text | Kind::List => {
                let w = (limit.saturating_sub(x + 12)) as usize;
                let v = if shown.is_empty() {
                    "(empty)".to_string()
                } else {
                    crate::sessions::snippet(&shown, w.max(4))
                };
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x,
                    y,
                    limit,
                    &v,
                    UiAction::SettingEdit(i),
                    "Edit (Enter)",
                    theme::SLATE,
                );
            }
        }
        if s.key == settings::Key::Global("assistant.memory_days") {
            x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                x + 1,
                y,
                limit.saturating_sub(9),
                "Learned rules…",
                UiAction::OpenLearned,
                "What the assistant learned from your corrections: toggle, edit, history, revert",
                theme::MAUVE,
            );
        }
        if s.key == settings::Key::Global("assistant.style") {
            x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                x + 1,
                y,
                limit.saturating_sub(9),
                "System prompt…",
                UiAction::OpenPrompt,
                "The assistant's instructions: sections (safety ones locked), edit, reset, history and undo",
                theme::MAUVE,
            );
        }
        if s.key == settings::Key::Global("voice.grok_stt.fallback") {
            x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                x + 1,
                y,
                limit.saturating_sub(9),
                "Test recognition",
                UiAction::SttTest,
                "Record 3 s, send it to Grok, and show the transcript and how long it took",
                theme::MAUVE,
            );
        }
        if s.key == settings::Key::Global("voice.grok.source") {
            x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                x + 1,
                y,
                limit.saturating_sub(9),
                "Change…",
                UiAction::OpenGrokLogins,
                "Pick the Grok login: each one's email, usage and status (Enter)",
                theme::MAUVE,
            );
        }
        if s.key == settings::Key::Global("voice.kokoro_voice")
            || s.key == settings::Key::Global("voice.tts_voice")
            || s.key == settings::Key::Global("voice.grok.voice")
        {
            x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                x + 1,
                y,
                limit.saturating_sub(9),
                "Preview",
                UiAction::VoicePreview,
                "Play a sample in this voice",
                theme::MAUVE,
            );
        }
        if limit > x + 8 {
            button(
                buf,
                &mut hits,
                app.mouse_pos,
                limit - 8,
                y,
                limit,
                "reset",
                UiAction::SettingReset(i),
                "Back to the default (Backspace)",
                theme::STONE,
            );
        }
        let _ = selected;
    }
    // Section extras.
    let mut y = area.y + 1 + list_h;
    if sec == Section::Accounts && y < area.y + area.height {
        let mut hits = app.hits.borrow_mut();
        let mut x = button(
            buf,
            &mut hits,
            app.mouse_pos,
            area.x + 1,
            y,
            limit,
            "+ Add account",
            UiAction::Key('A'),
            "Add an account and log it in",
            theme::SAGE,
        );
        for (a, acc) in app.cfg.accounts.iter().enumerate() {
            x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                x,
                y,
                limit,
                &format!("Log in {}", acc.display()),
                UiAction::LoginAccount(a),
                "Run the login for this account",
                theme::SLATE,
            );
        }
        y += 1;
        let mut x = area.x + 1;
        for (a, acc) in app.cfg.accounts.iter().enumerate() {
            x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                x,
                y,
                limit,
                &format!("Remove {}", acc.display()),
                UiAction::RemoveAccount(a),
                "Remove from config.toml (asks first; its folder is kept)",
                theme::CLAY,
            );
        }
        y += 2;
    }
    if sec == Section::Memory && y < area.y + area.height {
        let m = &app.mem;
        let line = format!(
            " Now: godterm {}  ·  {} running tab(s) {}  ·  total {}",
            kb(m.own_kb),
            m.tabs.len(),
            kb(m.tabs.iter().map(|t| t.3).sum()),
            kb(m.total_kb())
        );
        text(
            buf,
            area.x,
            y,
            limit,
            &line,
            Style::default().fg(theme::SLATE),
        );
        y += 1;
        let mut parts = String::from(" ");
        for (_, _, label, k) in m.tabs.iter().take(8) {
            parts.push_str(&format!("{label} {}   ", kb(*k)));
        }
        let paused = app.suspended_count();
        if paused > 0 {
            parts.push_str(&format!("  {paused} tab(s) paused (zz)"));
        }
        text(buf, area.x, y, limit, &parts, Style::default().fg(FAINT));
        y += 1;
        let mut hits = app.hits.borrow_mut();
        let on = app.memory_saver_on();
        let x = button(
            buf,
            &mut hits,
            app.mouse_pos,
            area.x + 1,
            y,
            limit,
            if on { "● Memory saver on" } else { "○ Memory saver off" },
            UiAction::Key('Z'),
            "Low memory profile: short history, idle background tabs paused, caches dropped (Ctrl-a Z)",
            if on { theme::SAGE } else { theme::STONE },
        );
        button(
            buf,
            &mut hits,
            app.mouse_pos,
            x + 1,
            y,
            limit,
            "Free now",
            UiAction::FreeNow,
            "Pause every idle background tab now, drop caches and background history",
            theme::SAND,
        );
        drop(hits);
        y += 1;
    }
    if sec == Section::Voice && y < area.y + area.height {
        let mut hits = app.hits.borrow_mut();
        let mut x = button(
            buf,
            &mut hits,
            app.mouse_pos,
            area.x + 1,
            y,
            limit,
            "Test mic",
            UiAction::Mic,
            "Talk now: the level meter moves and the voice strip shows what was heard",
            theme::MAUVE,
        );
        x = button(
            buf,
            &mut hits,
            app.mouse_pos,
            x + 1,
            y,
            limit,
            "Train wake word",
            UiAction::TrainWake,
            "Say the wake word and a few normal sentences, so it is recognized in your voice",
            theme::MAUVE,
        );
        // The device lists come from a background check.
        match settings::list_state() {
            settings::ListState::Detecting | settings::ListState::NotStarted => {
                x = text(
                    buf,
                    x + 2,
                    y,
                    limit,
                    "detecting devices…",
                    Style::default().fg(FAINT),
                );
            }
            settings::ListState::Failed(why) => {
                x = text(
                    buf,
                    x + 2,
                    y,
                    limit,
                    &format!("couldn't list devices ({why})"),
                    Style::default().fg(theme::CLAY),
                );
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x + 1,
                    y,
                    limit,
                    "Retry",
                    UiAction::RetryDevices,
                    "List the microphones, speakers and voices again",
                    theme::MAUVE,
                );
            }
            settings::ListState::Done => {}
        }
        drop(hits);
        // The live meter while the mic is open.
        let spans = crate::ui::meter_spans(app);
        let mut mx = x + 2;
        for sp in spans {
            mx = text(buf, mx, y, limit, &sp.content, sp.style);
        }
        if let Some(h) = app.voice.partial.as_ref().or(app.voice.heard.as_ref()) {
            text(
                buf,
                mx + 1,
                y,
                limit,
                &format!("\"{}\"", crate::sessions::snippet(h, 40)),
                Style::default().fg(DIM),
            );
        }
        // Which talk back engine is in use, and why when it fell back.
        let engine = match (&app.voice.engine, &app.voice.preview) {
            (Some(v), _) => v.tts_engine(),
            (None, Some(p)) => p.engine(),
            (None, None) => match crate::voice::tts::effective_engine(&app.cfg.voice) {
                (e, Some(why)) => format!("{e} (kokoro unavailable: {why})"),
                (e, None) => e.to_string(),
            },
        };
        let wake = match &app.voice.profile {
            Some(p) => format!(
                "   wake profile: {} aliases, detected {}/{}",
                p.aliases.len(),
                p.accuracy.detected,
                p.accuracy.attempts
            ),
            None => "   wake profile: none".into(),
        };
        if y + 1 < area.y + area.height {
            text(
                buf,
                area.x + 1,
                y + 1,
                limit,
                &format!("Talk back: {engine}{}{wake}", grok_note(app)),
                Style::default().fg(FAINT),
            );
        }
        // Background voices: train, the mic mode picker, the live score.
        if y + 3 < area.y + area.height {
            let mut hits = app.hits.borrow_mut();
            let mut x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                area.x + 1,
                y + 2,
                limit,
                "Train my voice",
                UiAction::TrainVoice,
                "Read three sentences so the speaker lock knows your voice (adds to the wake word clips)",
                theme::MAUVE,
            );
            if cfg!(target_os = "macos") {
                x = button(
                    buf,
                    &mut hits,
                    app.mouse_pos,
                    x + 1,
                    y + 2,
                    limit,
                    "Open macOS mic modes…",
                    UiAction::MicModes,
                    "The system picker: choose Voice Isolation for GodTerm (Apple capture)",
                    theme::MAUVE,
                );
            }
            drop(hits);
            text(
                buf,
                x + 2,
                y + 2,
                limit,
                &app.speaker_status(),
                Style::default().fg(DIM),
            );
            text(
                buf,
                area.x + 1,
                y + 3,
                limit,
                &format!(
                    "Capture: {}",
                    crate::voice::capture::describe(&app.cfg.voice)
                ),
                Style::default().fg(FAINT),
            );
        }
    }
    // Help for the selected row.
    if let Some(s) = rows.get(sel) {
        let hy = area.y + area.height - help_h;
        let r = Rect::new(area.x + 1, hy, area.width.saturating_sub(2), help_h);
        let mut t = s.help.to_string();
        if s.soon {
            t.push_str("  Coming soon: saved now, used once the feature lands.");
        }
        if s.inherit {
            t.push_str("  reset = inherit the global value.");
        }
        f.render_widget(
            Paragraph::new(t)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(DIM)),
            r,
        );
    }
}

fn draw_keys(f: &mut Frame, area: Rect) {
    let lines: Vec<Line> = crate::ui::HELP
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("  {k:<26}"), Style::default().fg(theme::SAND)),
                Span::styled(v.to_string(), Style::default().fg(FG)),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines),
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        ),
    );
}

fn draw_about(f: &mut Frame, app: &App, area: Rect) {
    let home = crate::config::app_home();
    let mut lines = vec![
        Line::styled(
            format!("  godterm {}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(
            format!("  config     {}", crate::config::Config::path().display()),
            Style::default().fg(DIM),
        ),
        Line::styled(
            format!("  tabs       {}", crate::state::AppState::path().display()),
            Style::default().fg(DIM),
        ),
        Line::styled(
            format!("  log        {}", crate::log::path().display()),
            Style::default().fg(DIM),
        ),
        Line::styled(
            format!("  accounts   {}", home.join("accounts").display()),
            Style::default().fg(DIM),
        ),
        Line::styled(
            format!("  program    {}", app.cfg.claude_bin()),
            Style::default().fg(DIM),
        ),
        Line::raw(""),
    ];
    for l in crate::install::Status::read().lines() {
        lines.push(Line::styled(format!("  {l}"), Style::default().fg(DIM)));
    }
    lines.push(Line::raw(""));
    let ready = app.update.ready().is_some();
    for (i, l) in app.update_lines().into_iter().enumerate() {
        let st = if i == 0 && ready {
            Style::default()
                .fg(theme::SAGE)
                .add_modifier(Modifier::BOLD)
        } else if i == 0 {
            Style::default().fg(FG)
        } else {
            Style::default().fg(DIM)
        };
        lines.push(Line::styled(format!("  {l}"), st));
    }
    lines.push(Line::raw(""));
    let update_y = area.y + 1 + lines.len() as u16;
    lines.push(Line::raw(""));
    lines.push(Line::raw(""));
    if let Some(e) = &app.config_error {
        lines.push(Line::styled(
            format!("  {e}"),
            Style::default().fg(theme::CLAY),
        ));
    }
    let n_lines = lines.len() as u16;
    f.render_widget(
        Paragraph::new(lines),
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        ),
    );
    let mut hits = app.hits.borrow_mut();
    if update_y < area.y + area.height {
        let mut x = button(
            f.buffer_mut(),
            &mut hits,
            app.mouse_pos,
            area.x + 2,
            update_y,
            area.x + area.width,
            "Check now",
            UiAction::UpdateCheck,
            "Look for a new GodTerm version on GitHub now",
            theme::SLATE,
        );
        if ready {
            x = button(
                f.buffer_mut(),
                &mut hits,
                app.mouse_pos,
                x + 1,
                update_y,
                area.x + area.width,
                "Restart to update",
                UiAction::Key('N'),
                "Install the verified download and restart into it (Ctrl-a N); every tab resumes",
                theme::SAGE,
            );
        }
        if app.update_chip().is_some() {
            button(
                f.buffer_mut(),
                &mut hits,
                app.mouse_pos,
                x + 1,
                update_y,
                area.x + area.width,
                "Skip this version",
                UiAction::UpdateSkip,
                "Do not offer this version again (a newer one still shows)",
                theme::STONE,
            );
        }
    }
    let y = (area.y + 1 + n_lines).max(area.y + 14);
    if y < area.y + area.height {
        let x = button(
            f.buffer_mut(),
            &mut hits,
            app.mouse_pos,
            area.x + 2,
            y + 1,
            area.x + area.width,
            "Install app / CLI",
            UiAction::Install(false),
            "GodTerm.app in ~/Applications and the godterm command in ~/.local/bin (godterm install)",
            theme::SAGE,
        );
        button(
            f.buffer_mut(),
            &mut hits,
            app.mouse_pos,
            x + 1,
            y + 1,
            area.x + area.width,
            "Add to Dock",
            UiAction::Install(true),
            "Install, then add GodTerm to the Dock (asks first; restarts the Dock)",
            theme::SLATE,
        );
        button(
            f.buffer_mut(),
            &mut hits,
            app.mouse_pos,
            area.x + 2,
            y,
            area.x + area.width,
            "Run doctor",
            UiAction::RunDoctor,
            "Check every dependency in a new tab",
            theme::SLATE,
        );
    }
}

/// The text editor dialog for a setting.
/// With Grok in the chain: where the text goes, the Voice usage when the
/// billing reply has it, and the last Test.
fn grok_note(app: &App) -> String {
    let mut s = String::new();
    if app.cfg.voice.engine == "grok" {
        s.push_str(
            " · Grok recognition sends your microphone audio (after the speaker lock) to xAI",
        );
        if let Some(c) = crate::voice::grok_stt::LAST_CHECK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            s.push_str(&format!(" · Test recognition: {c}"));
        }
    }
    if !crate::voice::engines::chain(&app.cfg.voice).contains(&"grok") {
        return s;
    }
    s.push_str(" · Grok sends the spoken text (assistant replies) to xAI");
    let voice_use = app.accounts.iter().find_map(|a| {
        a.usage
            .as_ref()?
            .windows
            .iter()
            .find(|w| w.label.to_lowercase().contains("voice"))
            .map(|w| w.utilization)
    });
    if let Some(u) = voice_use {
        s.push_str(&format!(" · Voice {u:.0}% used"));
    }
    if let Some(c) = crate::voice::grok_tts::LAST_CHECK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        s.push_str(&format!(" · Test: {c}"));
    }
    s
}

pub fn draw_edit(f: &mut Frame, area: Rect, app: &App, row: usize, buf_text: &str) {
    let rows = app.settings_rows();
    let label = rows.get(row).map(|s| s.label.clone()).unwrap_or_default();
    let w = 72u16.min(area.width);
    let h = 5u16.min(area.height);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    // A secret is masked as it is typed.
    let secret = rows
        .get(row)
        .is_some_and(|s| s.kind == settings::Kind::Secret);
    let shown_text = if secret {
        "•".repeat(buf_text.chars().count())
    } else {
        buf_text.to_string()
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("> ", Style::default().fg(theme::SAND)),
            Span::styled(shown_text, Style::default().fg(FG)),
            Span::styled("▏", Style::default().fg(theme::SAND)),
        ]),
        Line::styled(
            "Lists are comma separated. Enter saves, Esc cancels.",
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
                    .title(Span::styled(format!(" {label} "), Style::default().fg(FG))),
            )
            .style(Style::default().bg(BAR_BG)),
        r,
    );
    crate::ui_chrome::modal_chrome(
        f.buffer_mut(),
        app,
        r,
        &[
            ("Save", UiAction::ModalOk, "Save (Enter)"),
            ("Cancel", UiAction::ModalCancel, "Close (Esc)"),
        ],
    );
}
