//! The new version indicator and the Updates window: a chip with a slow,
//! calm shimmer (sand to sage, no glow; still with reduced motion) in the
//! menu bar, or the status bar when the menu bar is hidden; a click (or
//! Ctrl-a D) opens what is new in every version since this one, newest
//! first, with Download / Restart to update / Later / Release page, or the
//! package manager's command for Homebrew and system installs.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui::Frame;

use crate::app::{App, Modal};
use crate::app_update::Phase;
use crate::hits::UiAction;
use crate::theme::{self, BAR_BG, DIM, FAINT, FG};
use crate::update::{Method, Version};

/// One shimmer cycle.
const CYCLE_MS: u128 = 2000;

/// Whether the chip moves: `splash_motion` (off, auto: macOS Reduce
/// Motion, on) and GODTERM_REDUCED_MOTION.
pub fn motion(app: &App) -> bool {
    if std::env::var_os("GODTERM_REDUCED_MOTION").is_some_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    match app.cfg.splash_motion.as_str() {
        "off" => false,
        "auto" => !reduce_motion(),
        _ => true,
    }
}

/// macOS Reduce Motion, asked once.
fn reduce_motion() -> bool {
    static R: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        if cfg!(test) || !cfg!(target_os = "macos") {
            return false;
        }
        std::process::Command::new("defaults")
            .args(["read", "com.apple.universalaccess", "reduceMotion"])
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
    })
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

const SAND: (u8, u8, u8) = (196, 176, 134);
const SAGE: (u8, u8, u8) = (138, 160, 128);
const LIGHT: (u8, u8, u8) = (226, 214, 184);

/// The chip's background at cell `i` of `n`: a sand to sage gradient, and
/// a soft lighter band sweeping across once per cycle (`phase` 0..1;
/// None: still).
pub fn chip_bg(i: usize, n: usize, phase: Option<f64>) -> (u8, u8, u8) {
    let p = if n <= 1 {
        0.0
    } else {
        i as f64 / (n - 1) as f64
    };
    let base = mix(SAND, SAGE, p);
    match phase {
        None => base,
        Some(ph) => {
            // The band runs a little past both ends, so it fades in and out.
            let at = ph * 1.6 - 0.3;
            let d = (p - at) / 0.18;
            mix(base, LIGHT, 0.55 * (-d * d).exp())
        }
    }
}

/// The shimmer's phase now (None: still).
pub fn phase(app: &App) -> Option<f64> {
    motion(app).then(|| {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        (ms % CYCLE_MS) as f64 / CYCLE_MS as f64
    })
}

/// Draw the chip at `x` (registers its click); returns its end.
pub fn draw_chip(buf: &mut Buffer, app: &App, x: u16, y: u16, label: &str) -> u16 {
    let text = format!(" {label} ");
    let cells: Vec<char> = text.chars().collect();
    let n = cells.len();
    let ph = phase(app);
    let mut cx = x;
    for (i, ch) in cells.iter().enumerate() {
        if let Some(c) = buf.cell_mut((cx, y)) {
            c.set_char(*ch);
            c.set_style(
                Style::default()
                    .fg(Color::Rgb(34, 34, 30))
                    .bg(theme::rgb(chip_bg(i, n, ph)))
                    .add_modifier(Modifier::BOLD),
            );
        }
        cx += unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(1) as u16;
    }
    app.hits.borrow_mut().add(
        Rect::new(x, y, cx - x, 1),
        UiAction::OpenUpdates,
        "A new GodTerm version: what's new, download, restart (Ctrl-a D)",
    );
    cx
}

pub fn chip_width(label: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(label) as u16 + 2
}

/// A line of the Updates window: text and how it looks.
#[derive(Debug, Clone, PartialEq)]
pub enum Tone {
    Title,
    Heading,
    Body,
    Dim,
    Warn,
}

/// Markdown to readable lines: headings, bullets (•), bold and code marks
/// and link targets dropped, wrapped to `w`.
pub fn render_markdown(md: &str, w: usize) -> Vec<(String, Tone)> {
    let mut out = vec![];
    for raw in md.lines() {
        let l = raw.trim_end();
        let t = l.trim_start();
        if t.is_empty() {
            if out
                .last()
                .is_some_and(|(s, _): &(String, Tone)| !s.is_empty())
            {
                out.push((String::new(), Tone::Body));
            }
            continue;
        }
        let (text, tone, indent) = if let Some(h) = t.strip_prefix('#') {
            (
                h.trim_start_matches('#').trim().to_string(),
                Tone::Heading,
                0,
            )
        } else if let Some(b) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            let lead = (l.len() - t.len()).min(8);
            (
                format!("{}• {}", " ".repeat(lead), b.trim()),
                Tone::Body,
                lead + 2,
            )
        } else {
            (t.to_string(), Tone::Body, 0)
        };
        let text = clean_inline(&text);
        for (k, piece) in wrap(&text, w.max(10), indent).into_iter().enumerate() {
            let _ = k;
            out.push((piece, tone.clone()));
        }
    }
    while out.last().is_some_and(|(s, _)| s.is_empty()) {
        out.pop();
    }
    out
}

/// "**b**", "`c`", "[text](url)" -> "b", "c", "text".
fn clean_inline(s: &str) -> String {
    let s = s.replace("**", "").replace('`', "");
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find('[') {
        let (before, after) = rest.split_at(i);
        out.push_str(before);
        match (after.find("]("), after.find(')')) {
            (Some(a), Some(b)) if a < b => {
                out.push_str(&after[1..a]);
                rest = &after[b + 1..];
            }
            _ => {
                out.push('[');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Word wrap; continuation lines are indented by `indent`.
fn wrap(s: &str, w: usize, indent: usize) -> Vec<String> {
    let mut lines = vec![];
    // Leading spaces (a nested bullet) stay.
    let body = s.trim_start_matches(' ');
    let mut cur = " ".repeat(s.len() - body.len());
    for word in body.split(' ') {
        let cw = unicode_width::UnicodeWidthStr::width(cur.as_str());
        let ww = unicode_width::UnicodeWidthStr::width(word);
        if cw > 0 && cw + 1 + ww > w {
            lines.push(std::mem::take(&mut cur));
            cur = " ".repeat(indent);
        } else if cw > 0 && !cur.trim().is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.trim().is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

impl App {
    /// The chip's label: "↑ v0.2.8" (out, or downloading), "↑ Update
    /// ready" (downloaded and verified). None: nothing new, a development
    /// build, or "Later" for this version.
    pub fn update_badge(&self) -> Option<String> {
        let s = self.update.snapshot();
        if matches!(s.method, Some(Method::Dev { .. })) {
            return None;
        }
        let v = s.latest.as_ref()?.version();
        if v <= Version::current() || self.update.later.as_deref() == Some(v.to_string().as_str()) {
            return None;
        }
        match s.phase {
            Phase::Ready => Some("↑ Update ready".into()),
            Phase::Available => Some(format!("↑ v{v}")),
            Phase::Downloading(p) => Some(format!("↑ v{v} {p}%")),
            _ => None,
        }
    }

    pub fn open_updates(&mut self) {
        self.modal = Modal::Updates(0);
    }

    /// "Download": now, through the usual checks (signature, SHA-256).
    pub fn updates_download(&mut self) {
        self.update
            .force_download
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.start_update_checker();
        if let Some(k) = &self.update.kick {
            let _ = k.send(());
        }
        self.flash("Downloading the update...");
    }

    /// "Later": no chip for this version until the next launch.
    pub fn updates_later(&mut self) {
        let s = self.update.snapshot();
        self.update.later = s.latest.map(|r| r.version().to_string());
        self.modal = Modal::None;
        self.flash("Later it is: GodTerm reminds you at the next launch or version");
    }

    pub fn updates_release_page(&mut self) {
        let s = self.update.snapshot();
        if let Some(r) = s.latest {
            crate::admin::open_url(&r.page);
        }
    }

    /// What survives a restart into the update, as the restore code does it.
    pub fn restart_survival(&self) -> Vec<String> {
        let mut v = vec![
            "Every tab comes back in its pane and folder; a tab with a conversation resumes it (claude --resume).".to_string(),
        ];
        if !self.cfg.autostart {
            v.push(
                "Autostart is off: tabs come back stopped; press Enter in each to resume.".into(),
            );
        } else if self.restore_mode() == "eager" {
            v.push("They all start again, a moment apart.".into());
        } else {
            v.push("The tabs on screen start again at once; the others when you open them.".into());
        }
        let working: Vec<String> = self.restart_blockers();
        v.push("Work in progress is interrupted: a running turn stops and does not continue by itself. Loops set up inside a session have to be set up again.".into());
        if !working.is_empty() {
            v.push(format!("Right now: {}.", working.join(", ")));
        }
        v
    }

    /// The Updates window's lines (`w` wide) and buttons.
    pub fn updates_view(
        &self,
        w: usize,
    ) -> (Vec<(String, Tone)>, Vec<(String, UiAction, &'static str)>) {
        let s = self.update.snapshot();
        let cur = Version::current();
        let mut lines: Vec<(String, Tone)> = vec![];
        let latest = s.latest.as_ref().map(|r| r.version());
        lines.push((
            match &latest {
                Some(l) if *l > cur => format!("You have v{cur}. The latest is v{l}."),
                _ => format!("You have v{cur}, the latest."),
            },
            Tone::Title,
        ));
        let hint = s.method.as_ref().and_then(Method::hint);
        match (&s.phase, &hint) {
            (_, Some(h)) => lines.push((format!("This install updates with: {h}"), Tone::Warn)),
            (Phase::Downloading(p), _) => {
                let n = (w.saturating_sub(12)).max(10);
                let done = n * (*p as usize) / 100;
                lines.push((
                    format!(
                        "Downloading {}{} {p}%",
                        "█".repeat(done),
                        "░".repeat(n - done)
                    ),
                    Tone::Body,
                ));
            }
            (Phase::Ready, _) => lines.push((
                "Downloaded and verified (signature and SHA-256): ready to restart.".into(),
                Tone::Body,
            )),
            (Phase::Failed(e), _) => lines.push((format!("Last try failed: {e}"), Tone::Warn)),
            _ => {}
        }
        lines.push((String::new(), Tone::Body));
        let mut rels: Vec<crate::update::Release> = s.between.clone();
        if rels.is_empty() {
            if let Some(r) = s.latest.clone().filter(|r| r.version() > cur) {
                rels.push(r);
            }
        }
        for r in &rels {
            let date = r
                .published
                .as_deref()
                .map(|d| format!(" · {}", &d[..d.len().min(10)]))
                .unwrap_or_default();
            lines.push((format!("v{}{date}", r.version()), Tone::Heading));
            let body = if r.body.trim().is_empty() {
                vec![("No notes for this version.".to_string(), Tone::Dim)]
            } else {
                render_markdown(&r.body, w)
            };
            lines.extend(body);
            lines.push((String::new(), Tone::Body));
        }
        let mut buttons: Vec<(String, UiAction, &'static str)> = vec![];
        match (&s.phase, &hint) {
            (_, Some(_)) => {}
            (Phase::Ready, _) => buttons.push((
                "Restart to update".into(),
                UiAction::UpdatesRestart,
                "Restart into the new version (asks first)",
            )),
            (Phase::Available | Phase::Failed(_), _) => buttons.push((
                "Download".into(),
                UiAction::UpdatesDownload,
                "Download and verify it now",
            )),
            _ => {}
        }
        if s.latest.as_ref().is_some_and(|r| !r.page.is_empty()) {
            buttons.push((
                "Release page".into(),
                UiAction::UpdatesPage,
                "Open the release on GitHub",
            ));
        }
        buttons.push((
            "Later".into(),
            UiAction::UpdatesLater,
            "Hide the chip until the next launch or version",
        ));
        (lines, buttons)
    }
}

/// The Updates window.
pub fn draw(f: &mut Frame, area: Rect, app: &App, scroll: u16) {
    let w = 96u16.min(area.width.saturating_sub(2)).max(30);
    let h = (area.height.saturating_sub(4)).clamp(8, 40);
    let r = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::SAND))
            .title(" updates ")
            .style(Style::default().bg(BAR_BG)),
        r,
    );
    let inner_w = r.width.saturating_sub(4) as usize;
    let (lines, buttons) = app.updates_view(inner_w);
    let body_h = r.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(body_h);
    let top = (scroll as usize).min(max_scroll);
    app.updates_max_scroll.set(max_scroll as u16);
    let buf = f.buffer_mut();
    for (k, (t, tone)) in lines.iter().skip(top).take(body_h).enumerate() {
        let st = match tone {
            Tone::Title => Style::default().fg(FG).add_modifier(Modifier::BOLD),
            Tone::Heading => Style::default()
                .fg(theme::SAND)
                .add_modifier(Modifier::BOLD),
            Tone::Body => Style::default().fg(FG),
            Tone::Dim => Style::default().fg(DIM),
            Tone::Warn => Style::default().fg(theme::CLAY),
        };
        crate::hits::text(
            buf,
            r.x + 2,
            r.y + 1 + k as u16,
            r.x + r.width - 2,
            t,
            st.bg(BAR_BG),
        );
    }
    if max_scroll > 0 {
        let more = if top < max_scroll { "↓ more" } else { "↑" };
        crate::hits::text(
            buf,
            r.x + r.width - 9,
            r.y,
            r.x + r.width - 2,
            more,
            Style::default().fg(FAINT).bg(BAR_BG),
        );
    }
    let b: Vec<(&str, UiAction, &str)> = buttons
        .iter()
        .map(|(l, a, h)| (l.as_str(), a.clone(), *h))
        .collect();
    crate::ui_chrome::modal_chrome(buf, app, r, &b);
}

/// The restart confirmation: what comes back and what is interrupted.
pub fn draw_restart_confirm(f: &mut Frame, area: Rect, app: &App) {
    let v = app.update.ready().unwrap_or_default();
    let mut body = vec![format!("Restart into v{v} now?")];
    body.extend(app.restart_survival());
    crate::ui_chrome::draw_message(
        f,
        area,
        app,
        "restart to update",
        &body,
        &[
            (
                "Restart",
                UiAction::UpdatesRestartGo,
                "Restart now (Enter, y)",
            ),
            ("Cancel", UiAction::ModalCancel, "Not now (Esc)"),
        ],
        theme::SAND,
    );
}
