use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::app_onboarding::{self as ob, Mark, Phase};
use crate::hits::{button, text, UiAction};
use crate::onboarding::{self, StepId, STEPS};
use crate::theme::{self, BAR_BG, DIM, FAINT, FG};
use crate::voice::VoiceStatus;

const GROUND: Color = Color::Rgb(22, 25, 30);

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    match app.onb_phase() {
        Some(p) if p.full_screen() => draw_full(f, app, p, area),
        Some(Phase::Lesson) => draw_lesson(f, app),
        Some(Phase::Recap) => draw_phrases(f, app, area, true),
        Some(_) => {}
        None if app.onb.card => draw_phrases(f, app, area, false),
        None => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn btn(
    buf: &mut Buffer,
    app: &App,
    x: u16,
    y: u16,
    limit: u16,
    label: &str,
    n: u8,
    hint: &str,
) -> u16 {
    let mut hits = app.hits.borrow_mut();
    button(
        buf,
        &mut hits,
        app.mouse_pos,
        x,
        y,
        limit,
        label,
        UiAction::Onboarding(n),
        hint,
        theme::SAND,
    )
}

fn para(f: &mut Frame, r: Rect, lines: Vec<Line<'static>>) {
    if r.width == 0 || r.height == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(GROUND)),
        r,
    );
}

fn s(t: impl Into<String>, c: Color) -> Span<'static> {
    Span::styled(t.into(), Style::default().fg(c))
}

fn bold(t: impl Into<String>, c: Color) -> Span<'static> {
    Span::styled(
        t.into(),
        Style::default().fg(c).add_modifier(Modifier::BOLD),
    )
}

fn draw_full(f: &mut Frame, app: &mut App, p: Phase, area: Rect) {
    app.hits.borrow_mut().clear();
    app.want_cursor.set(None);
    f.render_widget(Clear, area);
    f.render_widget(Block::default().style(Style::default().bg(GROUND)), area);
    let w = area.width.saturating_sub(4).min(86);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let h = 22u16.min(area.height);
    let top = area.y + area.height.saturating_sub(h) / 2;
    let limit = x + w;

    let step = match p {
        Phase::Welcome if app.cfg.accounts.is_empty() => crate::first_run::step_label(1),
        Phase::Welcome => String::new(),
        Phase::Install => "voice setup 1 of 3: pieces".into(),
        Phase::Mic => "voice setup 2 of 3: mic".into(),
        _ => "voice setup 3 of 3: speaker".into(),
    };
    para(
        f,
        Rect::new(x, top, w, 1),
        vec![Line::from(vec![
            bold("God", FG),
            bold("Term", theme::SLATE),
            s(
                if step.is_empty() {
                    String::new()
                } else {
                    format!("   {step}")
                },
                FAINT,
            ),
        ])],
    );
    let body = Rect::new(x, top + 2, w, h.saturating_sub(2));
    match p {
        Phase::Welcome => welcome(f, app, body, limit),
        Phase::Install => install(f, app, body, limit),
        Phase::Mic => mic(f, app, body, limit),
        _ => speaker(f, app, body, limit),
    }

    let foot = Rect::new(x, (area.y + area.height).saturating_sub(2), w, 1);
    para(
        f,
        foot,
        vec![Line::from(s(
            "Nothing changes without your yes. Replay any time: palette > voice onboarding.",
            FAINT,
        ))],
    );
}

fn meter_line(app: &App) -> Option<Line<'static>> {
    let v = app.voice.engine.as_ref()?;
    let (db, _, _, _) = v.levels.read();
    let open = v.levels.open.load(std::sync::atomic::Ordering::Relaxed);
    let speech = v.levels.speech.load(std::sync::atomic::Ordering::Relaxed);
    let state = match &app.voice.status {
        VoiceStatus::Off(Some(why)) => format!("the mic could not start: {why}"),
        VoiceStatus::Starting => "starting the mic…".into(),
        _ if speech => "hearing you".into(),
        VoiceStatus::Transcribing => "understanding…".into(),
        _ if open => "listening".into(),
        _ => "mic not open yet".into(),
    };
    let db = if db.is_finite() && open { db } else { -90.0 };
    Some(Line::from(vec![
        s("● ", if speech { theme::SAGE } else { DIM }),
        s(format!("{state:<14} "), DIM),
        s(
            onboarding::meter(db, 28),
            if speech { theme::SAGE } else { theme::SLATE },
        ),
        s(format!(" {:>4.0} dB", db.max(-99.0)), FAINT),
    ]))
}

fn welcome(f: &mut Frame, app: &mut App, r: Rect, limit: u16) {
    para(
        f,
        Rect::new(r.x, r.y, r.width, 2),
        vec![Line::from(bold(onboarding::HOOK_TITLE, FG))],
    );
    para(
        f,
        Rect::new(r.x, r.y + 2, r.width, 3),
        vec![Line::from(s(onboarding::HOOK_BODY, DIM))],
    );
    let y = r.y + 6;
    let x = btn(
        f.buffer_mut(),
        app,
        r.x,
        y,
        limit,
        "Enter  Turn voice on",
        ob::BTN_VOICE_ON,
        "Set up voice: a mic check, a speaker check, five short steps",
    );
    btn(
        f.buffer_mut(),
        app,
        x + 2,
        y,
        limit,
        "Esc  Skip voice",
        ob::BTN_SKIP_ALL,
        "Go on without voice (turn it on later: palette > voice onboarding)",
    );
    let listening = app.voice.engine.is_some();
    let hint = if listening {
        "or just say \"yes\". About a minute: a mic check, a speaker check, five short steps."
    } else {
        "About a minute: the voice pieces, a mic check, a speaker check, five short steps."
    };
    para(
        f,
        Rect::new(r.x, y + 2, r.width, 2),
        vec![Line::from(s(hint, DIM))],
    );
    if let Some(m) = meter_line(app) {
        para(f, Rect::new(r.x, y + 4, r.width, 1), vec![m]);
    }

    if app.onb.on.as_ref().is_some_and(|o| o.login_walk) || app.cfg.accounts.is_empty() {
        para(
            f,
            Rect::new(r.x, y + 10, r.width, 1),
            vec![Line::from(vec![
                s("Getting started   ", FAINT),
                s(crate::first_run::roadmap(), DIM),
            ])],
        );
    }
    if let Some(h) = app.onb.on.as_ref().and_then(|o| o.heard.clone()) {
        para(
            f,
            Rect::new(r.x, y + 5, r.width, 1),
            vec![Line::from(vec![
                s("heard: ", FAINT),
                s(format!("“{h}”"), DIM),
            ])],
        );
    }
    if app.onb.on.as_ref().is_some_and(|o| o.reminded) {
        para(
            f,
            Rect::new(r.x, y + 7, r.width, 2),
            vec![Line::from(s(onboarding::REMINDER, theme::SAND))],
        );
    }
}

fn install(f: &mut Frame, app: &mut App, r: Rect, limit: u16) {
    let needs = app.onb_needs();
    let deps = app.deps();
    let mut lines = vec![
        Line::from(bold("Voice needs a few pieces on this machine.", FG)),
        Line::raw(""),
    ];
    for d in deps
        .iter()
        .filter(|d| d.group == crate::deps::Group::Voice && d.id != "speaker")
    {
        let manual = needs.manual.iter().find(|m| m.0 == d.name);
        let (mark, c) = if d.status.ok() {
            ("✓", theme::SAGE)
        } else if manual.is_some() {
            ("·", theme::CLAY)
        } else {
            ("○", theme::SAND)
        };
        let detail = if d.status.ok() {
            d.why.to_string()
        } else if let Some(m) = manual {
            format!("by hand: {}", m.1)
        } else if d.download_bytes() > 0 {
            format!(
                "{}, {} download",
                d.why,
                crate::deps::size(d.download_bytes())
            )
        } else if needs.install.iter().any(|i| i.0 == d.id) {
            d.why.to_string()
        } else {
            format!("{} (not needed here)", d.why)
        };
        lines.push(Line::from(vec![
            s(format!(" {mark} "), c),
            s(format!("{:<20}", d.name), FG),
            s(detail, DIM),
        ]));
    }
    let n = lines.len() as u16;
    para(f, Rect::new(r.x, r.y, r.width, n + 1), lines);
    let y = r.y + n + 1;
    let o = app.onb.on.as_ref();
    let installing = o.is_some_and(|o| o.installing);
    let mut x = r.x;
    if !needs.install.is_empty() && !installing {
        let size = if needs.bytes > 0 {
            format!(" ({})", crate::deps::size(needs.bytes))
        } else {
            String::new()
        };
        x = btn(
            f.buffer_mut(),
            app,
            x,
            y,
            limit,
            &format!("Enter  Install{size}"),
            ob::BTN_INSTALL,
            "Install the missing pieces now (the same as Settings > Setup)",
        );
        x += 1;
    }
    x = btn(
        f.buffer_mut(),
        app,
        x,
        y,
        limit,
        "c  Continue without voice",
        ob::BTN_NO_VOICE,
        "The steps by typing to the assistant instead",
    );
    btn(
        f.buffer_mut(),
        app,
        x + 1,
        y,
        limit,
        "Esc  Skip onboarding",
        ob::BTN_SKIP_ALL,
        "Straight to GodTerm",
    );
    let mut more: Vec<Line> = vec![];
    if installing {
        let last = app
            .admin
            .said
            .last()
            .cloned()
            .unwrap_or_else(|| "Starting…".into());
        more.push(Line::from(vec![
            s("Installing…  ", theme::SAND),
            s(last, DIM),
        ]));
    } else if let Some(e) = o.and_then(|o| o.install_error.clone()) {
        more.push(Line::from(vec![s("Not done: ", theme::CLAY), s(e, DIM)]));
        more.push(Line::from(s(
            "r tries again; c goes on without voice (type to the assistant).",
            FAINT,
        )));
    } else if needs.install.is_empty() && !needs.ready() {
        more.push(Line::from(s(
            "GodTerm cannot install these here. Install them as shown, then replay the onboarding; or go on without voice.",
            DIM,
        )));
    } else if !needs.install.is_empty() {
        more.push(Line::from(s(
            "Or say \"yes\". Downloads are pinned and checked; nothing runs with sudo.",
            DIM,
        )));
    }
    para(f, Rect::new(r.x, y + 2, r.width, 4), more);
}

fn mic(f: &mut Frame, app: &mut App, r: Rect, limit: u16) {
    let o = app.onb.on.as_ref();
    let heard = o.and_then(|o| o.mic_heard.clone());
    let since = o.map(|o| o.phase_at.elapsed().as_secs()).unwrap_or(0);
    let mut lines = vec![
        Line::from(bold("Mic check: say anything.", FG)),
        Line::from(s(
            "For example \"hello GodTerm\". The meter moves when it hears you.",
            DIM,
        )),
        Line::raw(""),
    ];
    if let Some(m) = meter_line(app) {
        lines.push(m);
    }
    lines.push(Line::raw(""));
    match heard {
        Some(h) => lines.push(Line::from(vec![
            s("✓ heard: ", theme::SAGE),
            s(format!("“{h}”"), FG),
        ])),
        None if since >= 12 => {
            lines.push(Line::from(s("Nothing heard yet.", theme::SAND)));
            lines.push(Line::from(s(
                "macOS: System Settings > Privacy & Security > Microphone, allow your terminal. Another mic: Settings > Voice > device.",
                DIM,
            )));
        }
        None => {}
    }
    let n = lines.len() as u16;
    para(f, Rect::new(r.x, r.y, r.width, n + 1), lines);
    let y = r.y + n + 2;
    let mut x = btn(
        f.buffer_mut(),
        app,
        r.x,
        y,
        limit,
        "Enter  Next",
        ob::BTN_NEXT,
        "On to the speaker check",
    );
    x = btn(
        f.buffer_mut(),
        app,
        x + 1,
        y,
        limit,
        "r  Try again",
        ob::BTN_AGAIN,
        "Open the mic again",
    );
    x = btn(
        f.buffer_mut(),
        app,
        x + 1,
        y,
        limit,
        "c  Continue without voice",
        ob::BTN_NO_VOICE,
        "The steps by typing instead",
    );
    btn(
        f.buffer_mut(),
        app,
        x + 1,
        y,
        limit,
        "Esc  Stop",
        ob::BTN_SKIP_ALL,
        "End the onboarding",
    );
}

fn speaker(f: &mut Frame, app: &mut App, r: Rect, limit: u16) {
    let name = onboarding::voice_name(&app.cfg.voice.kokoro_voice);
    let o = app.onb.on.as_ref();
    let not_heard = o.and_then(|o| o.speaker) == Some(false);
    let mut lines = vec![
        Line::from(bold("Speaker check: did you hear me?", FG)),
        Line::from(s(format!("\"This is {name}, the voice of GodTerm.\""), DIM)),
        Line::from(vec![s("voice: ", FAINT), s(app.onb_voice_label(), DIM)]),
    ];
    if app.cfg.voice.speaker_muted {
        lines.push(Line::from(s(
            "The speaker is muted (Ctrl-a O unmutes).",
            theme::SAND,
        )));
    }
    if not_heard {
        lines.push(Line::raw(""));
        lines.push(Line::from(s(
            "No sound? Turn the volume up, or pick the output: Settings > Voice > output device.",
            theme::SAND,
        )));
        lines.push(Line::from(s(
            "Enter goes on; the answers always show as text too.",
            DIM,
        )));
    }
    let n = lines.len() as u16;
    para(f, Rect::new(r.x, r.y, r.width, n + 1), lines);
    let y = r.y + n + 2;
    let mut x = btn(
        f.buffer_mut(),
        app,
        r.x,
        y,
        limit,
        "Enter  Yes, I heard it",
        ob::BTN_NEXT,
        "On to the first step",
    );
    x = btn(
        f.buffer_mut(),
        app,
        x + 1,
        y,
        limit,
        "r  Play again",
        ob::BTN_AGAIN,
        "Say the sample again",
    );
    x = btn(
        f.buffer_mut(),
        app,
        x + 1,
        y,
        limit,
        "n  I heard nothing",
        ob::BTN_NOT_HEARD,
        "Tips for the speaker",
    );
    btn(
        f.buffer_mut(),
        app,
        x + 1,
        y,
        limit,
        "Esc  Stop",
        ob::BTN_SKIP_ALL,
        "End the onboarding",
    );
    if let Some(m) = meter_line(app) {
        para(f, Rect::new(r.x, y + 2, r.width, 1), vec![m]);
    }
}

fn draw_lesson(f: &mut Frame, app: &mut App) {
    if app.modal != crate::app::Modal::None {
        return;
    }
    let Some(o) = app.onb.on.as_ref() else { return };
    let main = app.main_area;
    let w = main.width.saturating_sub(2).min(66);
    let h = 14u16;
    if w < 30 || main.height < h + 2 {
        return;
    }
    let r = Rect::new(main.x + 1, main.y + main.height - h - 1, w, h);
    let cur = o.steps.current();
    let typed = !o.voice;
    let mut lines: Vec<Line> = vec![];
    for (i, st) in STEPS.iter().enumerate() {
        let (mark, c) = match o.steps.marks[i] {
            Mark::Done => ("✓", theme::SAGE),
            Mark::Skipped => ("–", FAINT),
            Mark::Todo if Some(*st) == cur => ("▸", theme::SAND),
            Mark::Todo => ("○", DIM),
        };
        let style = if Some(*st) == cur { FG } else { DIM };
        lines.push(Line::from(vec![
            s(format!(" {mark} "), c),
            s(st.title(), style),
        ]));
    }
    lines.push(Line::raw(""));
    if let Some(st) = cur {
        let verb = if typed { "Type" } else { "Say" };
        lines.push(Line::from(vec![
            s(format!(" {verb}: "), DIM),
            bold(format!("“{}”", example(app, st)), theme::SAND),
        ]));
        lines.push(Line::from(s(format!(" {}", st.note()), DIM)));
    }
    if let Some(h) = &o.heard {
        lines.push(Line::from(vec![
            s(" heard: ", FAINT),
            s(format!("“{h}”"), DIM),
        ]));
    }
    let title = format!(
        " voice tour · step {} of {} ",
        (o.steps.cur + 1).min(STEPS.len()),
        STEPS.len()
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme::SLATE))
                    .title(Span::styled(
                        title,
                        Style::default().fg(FG).add_modifier(Modifier::BOLD),
                    )),
            )
            .style(Style::default().bg(BAR_BG)),
        r,
    );
    let y = r.y + r.height - 2;
    let limit = r.x + r.width - 1;
    let mut x = btn(
        f.buffer_mut(),
        app,
        r.x + 2,
        y,
        limit,
        "Skip step",
        ob::BTN_SKIP_STEP,
        "Skip this step (or say skip, Ctrl-a J)",
    );
    x = btn(
        f.buffer_mut(),
        app,
        x,
        y,
        limit,
        "Stop tour",
        ob::BTN_STOP,
        "End the onboarding (or say stop onboarding)",
    );
    let hint = if typed {
        "type skip / stop onboarding"
    } else {
        "say skip · next · stop onboarding"
    };
    text(
        f.buffer_mut(),
        x + 1,
        y,
        limit,
        hint,
        Style::default().fg(FAINT).bg(BAR_BG),
    );
}

fn example(app: &App, st: StepId) -> String {
    use crate::harness::Harness;
    let out = |h: Harness| {
        app.cfg
            .accounts
            .iter()
            .position(|a| a.harness() == h)
            .filter(|&a| app.accounts.get(a).is_some_and(|s| !s.login.can_start()))
    };
    match st {
        StepId::ClaudeLogin => match out(Harness::Claude) {
            Some(a) => format!("log in account {}", a + 1),
            None => st.example().into(),
        },
        StepId::GrokLogin => match out(Harness::Grok) {
            Some(a) => format!("log in account {}", a + 1),
            None => st.example().into(),
        },
        _ => st.example().into(),
    }
}

fn draw_phrases(f: &mut Frame, app: &mut App, area: Rect, recap: bool) {
    let w = area.width.saturating_sub(4).min(78);
    let h = (onboarding::RECAP.len() as u16) + 9;
    if w < 30 || area.height < h {
        return;
    }
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let mut lines: Vec<Line> = vec![Line::from(s(
        if recap {
            " You're set. The phrases worth remembering:"
        } else {
            " The phrases worth remembering:"
        },
        DIM,
    ))];
    lines.push(Line::raw(""));
    for (say, what) in onboarding::RECAP {
        lines.push(Line::from(vec![
            bold(format!("  “{say}”"), theme::SAND),
            s(format!("  {what}"), DIM),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(s(
        " Reopen this card: Ctrl-a J, or the palette > voice phrases.",
        FAINT,
    )));
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme::SAND))
                    .title(Span::styled(
                        if recap {
                            " voice tour: done "
                        } else {
                            " voice phrases "
                        },
                        Style::default().fg(FG).add_modifier(Modifier::BOLD),
                    )),
            )
            .style(Style::default().bg(BAR_BG)),
        r,
    );
    let y = r.y + r.height - 2;
    let limit = r.x + r.width - 1;
    if recap {
        btn(
            f.buffer_mut(),
            app,
            r.x + 2,
            y,
            limit,
            "Enter  Done",
            ob::BTN_FINISH,
            "Into GodTerm",
        );
    } else {
        btn(
            f.buffer_mut(),
            app,
            r.x + 2,
            y,
            limit,
            "Esc  Close",
            ob::BTN_CLOSE_CARD,
            "Close the card",
        );
    }
}
