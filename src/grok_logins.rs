//! The Grok login picker: one card per login (each grok account, the
//! main ~/.grok) with its email, status, what uses it and its usage
//! (weekly credits and each product, with bars), plus an Auto card.
//! Talk back and recognition share the login.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui::Frame;

use crate::app::{App, Modal};
use crate::hits::UiAction;
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};

/// Card actions.
pub const SELECT: u8 = 0;
pub const USE: u8 = 1;
pub const LOGIN: u8 = 2;
pub const LOGOUT: u8 = 3;
pub const REFRESH: u8 = 4;
pub const TEST: u8 = 5;

/// The products a billing reply may report, in this order.
const PRODUCTS: &[&str] = &[
    "Voice",
    "GrokBuild",
    "GrokImagine",
    "GrokAppBuilder",
    "Chat",
];

impl App {
    /// The cards: "auto", then every login's id.
    pub fn grok_cards(&self) -> Vec<String> {
        let mut v = vec!["auto".to_string()];
        v.extend(
            crate::voice::grok_tts::sources(&self.cfg)
                .into_iter()
                .map(|s| s.id),
        );
        v
    }

    pub fn open_grok_logins(&mut self) {
        self.want_grok_usage();
        let cur = self.cfg.voice.grok.source.clone();
        let sel = self
            .grok_cards()
            .iter()
            .position(|c| *c == cur)
            .unwrap_or(0);
        self.modal = Modal::GrokLogins(sel);
    }

    /// Run a card's action.
    pub fn grok_login_act(&mut self, card: usize, act: u8) {
        let cards = self.grok_cards();
        let Some(id) = cards.get(card).cloned() else {
            return;
        };
        let acct = self.cfg.accounts.iter().position(|c| c.name == id);
        match act {
            SELECT => self.modal = Modal::GrokLogins(card),
            USE => {
                let path = crate::config::Config::path();
                let r = crate::settings::write(
                    &path,
                    &crate::settings::Key::Global("voice.grok.auth"),
                    Some(toml_edit::value("oauth")),
                )
                .and_then(|_| {
                    crate::settings::write(
                        &path,
                        &crate::settings::Key::Global("voice.grok.source"),
                        Some(toml_edit::value(id.as_str())),
                    )
                });
                match r {
                    Ok(()) => {
                        self.reload_config_now();
                        self.flash(format!("Grok voice signs in with {}", self.grok_source_line(&id)));
                    }
                    Err(e) => self.flash(format!("Not saved: {e:#}")),
                }
                self.modal = Modal::GrokLogins(card);
            }
            LOGIN | LOGOUT => match acct {
                Some(a) => {
                    self.modal = Modal::None;
                    crate::voice::grok_tts::clear_refusal(&id);
                    if act == LOGIN {
                        self.login_for_account(a);
                    } else {
                        self.logout_account(a);
                    }
                }
                None => self.flash(if id == "main" {
                    "The main ~/.grok is yours: log in or out with grok login / grok logout in a terminal".to_string()
                } else {
                    "Auto has no login of its own".to_string()
                }),
            },
            REFRESH => {
                crate::voice::grok_tts::clear_refusal(&id);
                match acct {
                    Some(a) => self.request_usage(a),
                    None if id == "main" => self.fetch_main_grok_usage(true),
                    None => self.want_grok_usage(),
                }
                self.flash("Refreshing usage");
            }
            _ => {
                let mut g = self.cfg.voice.grok.clone();
                g.auth = "oauth".into();
                g.source = id;
                crate::voice::grok_tts::check_in_background(g);
                self.flash("Testing that login with xAI (no audio, no cost)");
            }
        }
    }

    pub fn on_grok_logins_key(&mut self, k: KeyEvent) {
        let Modal::GrokLogins(sel) = self.modal else {
            return;
        };
        let n = self.grok_cards().len();
        match k.code {
            KeyCode::Esc => self.modal = Modal::None,
            KeyCode::Up => self.modal = Modal::GrokLogins(sel.saturating_sub(1)),
            KeyCode::Down => self.modal = Modal::GrokLogins((sel + 1).min(n.saturating_sub(1))),
            KeyCode::Enter | KeyCode::Char('u') => self.grok_login_act(sel, USE),
            KeyCode::Char('l') => self.grok_login_act(sel, LOGIN),
            KeyCode::Char('o') => self.grok_login_act(sel, LOGOUT),
            KeyCode::Char('r') => self.grok_login_act(sel, REFRESH),
            KeyCode::Char('t') => self.grok_login_act(sel, TEST),
            _ => {}
        }
    }

    /// The lines of one card: (text, style) runs per line, and its
    /// buttons (label, action) on the first line.
    fn grok_card(&self, id: &str) -> (Vec<Vec<(String, Style)>>, Vec<(&'static str, u8)>) {
        use crate::voice::grok_tts;
        let dim = Style::default().fg(DIM);
        let mut lines: Vec<Vec<(String, Style)>> = vec![];
        let all = grok_tts::sources(&self.cfg);
        let refused = grok_tts::refused_ids();
        let chosen = self.cfg.voice.grok.source.as_str();
        let chosen = if chosen.is_empty() { "auto" } else { chosen };
        if id == "auto" {
            let pick = grok_tts::auto_choice(&all, &refused).map(|s| s.label.clone());
            lines.push(vec![(
                "Auto (best available)".into(),
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            )]);
            lines.push(vec![(
                "Uses the logged in login with the most voice left, passing over ones xAI refused."
                    .into(),
                dim,
            )]);
            lines.push(vec![(
                format!(
                    "Now: {}",
                    match (self.grok_best(), pick) {
                        (Some(b), _) => self.grok_source_line(&b),
                        // Usage known and nothing left anywhere.
                        (None, _) if all.iter().any(|x| self.grok_left(&x.id).is_some()) => {
                            "none available (every login is out or refused)".into()
                        }
                        (None, Some(p)) => format!("{p} (no usage known yet)"),
                        (None, None) => "none available".into(),
                    }
                ),
                Style::default().fg(theme::SAGE),
            )]);
            if chosen == "auto" {
                lines.push(vec![("Used for Talk back and Recognition".into(), dim)]);
            }
            return (lines, vec![("Use for voice", USE), ("Test", TEST)]);
        }
        let Some(s) = all.iter().find(|s| s.id == id) else {
            return (lines, vec![]);
        };
        let a = self.cfg.accounts.iter().position(|c| c.name == s.id);
        let mut head = vec![(
            s.label.clone(),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        )];
        if let Some(e) = a.and_then(|a| self.email_label(a, 40)) {
            head.push((format!("  {e}"), dim));
        }
        let (status, sc) = if !s.logged_in {
            ("not logged in".to_string(), theme::CLAY)
        } else if let Some(at) = grok_tts::refused_at(&s.id) {
            (
                format!("out of credits at {}", at.format("%H:%M")),
                theme::CLAY,
            )
        } else if a.is_some_and(|a| {
            matches!(
                self.accounts.get(a).and_then(|x| x.usage_err.as_ref()),
                Some(crate::usage::UsageError::Unauthorized)
            )
        }) {
            ("expired".to_string(), theme::SAND)
        } else {
            ("logged in".to_string(), theme::SAGE)
        };
        head.push((format!("  · {status}"), Style::default().fg(sc)));
        if self.grok_best().as_deref() == Some(s.id.as_str()) {
            head.push(("  · most left".into(), Style::default().fg(theme::SAGE)));
        }
        lines.push(head);
        // What uses it now.
        let resolved = if chosen == "auto" {
            grok_tts::auto_choice(&all, &refused).map(|x| x.id.clone())
        } else {
            Some(chosen.to_string())
        };
        if resolved.as_deref() == Some(s.id.as_str()) {
            let mut used = vec![];
            if self.cfg.voice.tts_engine == "grok"
                || crate::voice::engines::chain(&self.cfg.voice).contains(&"grok")
            {
                used.push("Talk back");
            }
            if self.cfg.voice.engine == "grok" {
                used.push("Recognition");
            }
            let what = if used.is_empty() {
                "the Grok voice login (no Grok engine is on)".to_string()
            } else {
                used.join(", ")
            };
            lines.push(vec![(format!("Used for: {what}"), dim)]);
        }
        // Usage: the weekly credits, then each product.
        match self.grok_usage(&s.id) {
            Some(u) => {
                let bar = |left: f64| {
                    let n = (left.clamp(0.0, 100.0) / 10.0).round() as usize;
                    format!("{}{}", "█".repeat(n), "░".repeat(10 - n))
                };
                let color = |left: f64| theme::remaining_style(left).fg.unwrap_or(FG);
                if let Some(w) = u.windows.iter().find(|w| w.key == "seven_day") {
                    let mut l = vec![
                        ("Weekly credits  ".into(), dim),
                        (bar(w.left()), Style::default().fg(color(w.left()))),
                        (format!(" {:.0}% left", w.left()), Style::default().fg(FG)),
                    ];
                    if let Some(r) = w.resets_at {
                        l.push((
                            format!(
                                " · resets {}",
                                r.with_timezone(&chrono::Local).format("%a %H:%M")
                            ),
                            dim,
                        ));
                    }
                    lines.push(l);
                }
                for p in PRODUCTS {
                    let w = u.windows.iter().find(|w| w.label.eq_ignore_ascii_case(p));
                    lines.push(match w {
                        Some(w) => vec![
                            (format!("{p:<16}"), dim),
                            (bar(w.left()), Style::default().fg(color(w.left()))),
                            (format!(" {:.0}% left", w.left()), Style::default().fg(FG)),
                        ],
                        None => vec![
                            (format!("{p:<16}"), dim),
                            ("not reported".into(), Style::default().fg(FAINT)),
                        ],
                    });
                }
            }
            None => match self.grok_usage_error(&s.id) {
                Some(e) => lines.push(vec![(
                    format!("usage unavailable: {e} · r retries"),
                    Style::default().fg(theme::CLAY),
                )]),
                None if s.logged_in => lines.push(vec![("usage loading…".into(), dim)]),
                None => {}
            },
        }
        let mut buttons = vec![("Use for voice", USE)];
        if a.is_some() {
            buttons.push(if s.logged_in {
                ("Log out", LOGOUT)
            } else {
                ("Log in", LOGIN)
            });
        }
        buttons.push(("Refresh usage", REFRESH));
        buttons.push(("Test", TEST));
        (lines, buttons)
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App, sel: usize) {
    let w = 104u16.min(area.width.saturating_sub(4));
    let h = area.height.saturating_sub(4);
    if w < 40 || h < 8 {
        return;
    }
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + 2, w, h);
    f.render_widget(Clear, r);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(DIM))
            .title(" Grok login for voice (talk back and recognition) ")
            .style(Style::default().bg(BAR_BG)),
        r,
    );
    let buf = f.buffer_mut();
    let mut hits = app.hits.borrow_mut();
    let inner = Rect::new(r.x + 2, r.y + 1, r.width - 4, r.height - 3);
    let limit = inner.x + inner.width;
    // Cards with their heights; scroll so the selected one shows.
    let cards = app.grok_cards();
    let built: Vec<_> = cards.iter().map(|c| app.grok_card(c)).collect();
    let heights: Vec<u16> = built.iter().map(|(l, _)| l.len() as u16 + 1).collect();
    let mut first = 0;
    while first < sel && heights[first..=sel].iter().sum::<u16>() > inner.height {
        first += 1;
    }
    let mut y = inner.y;
    for (i, (lines, buttons)) in built.iter().enumerate().skip(first) {
        if y >= inner.y + inner.height {
            break;
        }
        let on = i == sel;
        let bg = if on { SEL_BG } else { BAR_BG };
        let top = y;
        for (k, runs) in lines.iter().enumerate() {
            if y >= inner.y + inner.height {
                break;
            }
            for xx in inner.x..limit {
                if let Some(c) = buf.cell_mut((xx, y)) {
                    c.set_char(' ');
                    c.set_style(Style::default().bg(bg));
                }
            }
            let mut x = crate::hits::text(
                buf,
                inner.x,
                y,
                limit,
                if k == 0 && on { "› " } else { "  " },
                Style::default().fg(theme::SAND).bg(bg),
            );
            for (t, st) in runs {
                x = crate::hits::text(buf, x, y, limit, t, st.bg(bg));
            }
            if k == 0 {
                // The actions, right aligned on the first line.
                let total: u16 = buttons
                    .iter()
                    .map(|(l, _)| l.chars().count() as u16 + 3)
                    .sum();
                let mut bx = limit.saturating_sub(total).max(x + 1);
                for (label, act) in buttons {
                    bx = crate::hits::button(
                        buf,
                        &mut hits,
                        app.mouse_pos,
                        bx,
                        y,
                        limit,
                        label,
                        UiAction::GrokLoginAct(i, *act),
                        match *act {
                            USE => "Talk back and recognition sign in with this (Enter)",
                            LOGIN => "Run grok login for this account in its pane (l)",
                            LOGOUT => "Log this account out of grok (o)",
                            REFRESH => "Fetch its usage again (r)",
                            _ => "Check it with xAI: the free voices list (t)",
                        },
                        theme::MAUVE,
                    ) + 1;
                }
            }
            y += 1;
        }
        hits.add(
            Rect::new(inner.x, top, inner.width.saturating_sub(1), y - top),
            UiAction::GrokLoginAct(i, SELECT),
            "Select (Enter uses it)",
        );
        y += 1;
    }
    // The last Test, and the keys.
    let by = r.y + r.height - 2;
    let test = crate::voice::grok_tts::LAST_CHECK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .map(|t| format!("Test: {t}   "))
        .unwrap_or_default();
    crate::hits::text(
        buf,
        inner.x,
        by,
        limit,
        &format!(
            "{test}↑↓ choose · Enter use · l log in · o log out · r refresh · t test · Esc close"
        ),
        Style::default().fg(FAINT).bg(BAR_BG),
    );
    let _ = Color::Reset;
}
