//! The assistant panel's window chrome: the minimized strip, the width
//! carets and window controls on its title row, its draggable left edge,
//! and the quick action bar above the input.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::app::App;
use crate::app_panelwin::{PanelCtl, PanelWin};
use crate::hits::{contains, Hits, UiAction};
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};

/// The window controls (three 3 cell buttons), as on a pane header.
pub const CONTROLS_W: u16 = 9;
/// The width carets " < > ".
pub const CARETS_W: u16 = 5;
/// The title keeps at least this much room, or the carets go.
const TITLE_MIN: u16 = 14;

/// The minimized panel: a thin strip at the right edge with "◂", the word
/// assistant down it, and the mic and state dot. A click restores it.
pub fn draw_strip(f: &mut Frame, app: &App, area: Rect) {
    let buf = f.buffer_mut();
    let hovered = app.mouse_pos.is_some_and(|(c, r)| contains(area, c, r));
    let bg = if hovered { SEL_BG } else { BAR_BG };
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(bg));
            }
        }
        if let Some(c) = buf.cell_mut((area.x, y)) {
            c.set_char('│');
            c.set_style(Style::default().fg(FAINT).bg(bg));
        }
    }
    let x = area.x + 1;
    let mut y = area.y;
    let end = area.y + area.height;
    let put = |buf: &mut Buffer, y: u16, s: &str, st: Style| {
        if y < end {
            crate::hits::text(buf, x, y, area.x + area.width, s, st.bg(bg));
        }
    };
    put(buf, y, "◂", Style::default().fg(theme::SLATE));
    y += 2;
    // The state dot first (it must show even on a short strip).
    let (dot, color) = strip_dot(app);
    let word_room = end.saturating_sub(y + 2) as usize;
    for ch in "assistant".chars().take(word_room) {
        put(buf, y, &ch.to_string(), Style::default().fg(DIM));
        y += 1;
    }
    put(
        buf,
        y + 1,
        dot,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    );
    app.hits.borrow_mut().add(
        area,
        UiAction::AssistantPanel(PanelCtl::Restore),
        "The assistant, minimized (voice still works): click to restore (Ctrl-a .)",
    );
}

/// The strip's dot: the mic muted, or what it is doing.
fn strip_dot(app: &App) -> (&'static str, Color) {
    if app.voice.muted {
        return ("●", theme::MUTED_RED);
    }
    match crate::ui_assistant::state_chip(app) {
        Some(c) if c.contains("speaking") => ("●", theme::SAGE),
        Some(c) if c.contains("thinking") => ("●", theme::SAND),
        Some(_) => ("●", theme::SLATE),
        None if app.voice_mode() == 0 => ("○", FAINT),
        None => ("●", DIM),
    }
}

/// Whether the width carets show in a panel this wide (they go before the
/// title gets too short).
pub fn carets_fit(app: &App, width: u16) -> bool {
    app.assistant.win == PanelWin::Normal
        && width.saturating_sub(6 + CONTROLS_W + 1 + CARETS_W + 1) >= TITLE_MIN
}

/// Cells the title row's right side takes (controls, carets, gaps).
pub fn title_tail(app: &App, width: u16) -> u16 {
    CONTROLS_W
        + 1
        + if carets_fit(app, width) {
            CARETS_W + 1
        } else {
            0
        }
}

/// The title row's right side: [< >] (wider, narrower) and _ □ ×, ending
/// before the top right corner. Returns where it starts.
pub fn draw_title_controls(buf: &mut Buffer, hits: &mut Hits, app: &App, area: Rect) -> u16 {
    let y = area.y;
    let right = area.x + area.width - 1;
    let start = right.saturating_sub(CONTROLS_W);
    let max = app.assistant.win == PanelWin::Max;
    let (max_glyph, max_hint) = if max {
        ("▣", "Restore the panel to its normal size")
    } else {
        ("□", "Maximize: the panel takes the whole grid area")
    };
    let mut x = start;
    for (glyph, c, hint, hover_fg) in [
        (
            "_",
            PanelCtl::Minimize,
            "Minimize to a thin strip at the right edge; voice keeps working (Ctrl-a . restores)",
            FG,
        ),
        (max_glyph, PanelCtl::Maximize, max_hint, FG),
        (
            "×",
            PanelCtl::Close,
            "Close the panel (Ctrl-a .)",
            theme::CLAY,
        ),
    ] {
        ctl_button(buf, hits, app, x, y, glyph, c, hint, hover_fg);
        x += 3;
    }
    let mut left = start;
    if carets_fit(app, area.width) {
        let cx = start - 1 - CARETS_W;
        let w = app.panel_width_now();
        let (lt, gt) = (
            format!(
                "Wider: {} columns (Ctrl-a {{)",
                w + crate::app_panelwin::STEP
            ),
            format!(
                "Narrower: {} columns (Ctrl-a }})",
                w.saturating_sub(crate::app_panelwin::STEP)
                    .max(crate::app_panelwin::PANEL_MIN)
            ),
        );
        // " < > ": "<" takes the first three cells, ">" the last two.
        caret(
            buf,
            hits,
            app,
            Rect::new(cx, y, 3, 1),
            " < ",
            PanelCtl::Wider,
            &lt,
        );
        caret(
            buf,
            hits,
            app,
            Rect::new(cx + 3, y, 2, 1),
            "> ",
            PanelCtl::Narrower,
            &gt,
        );
        left = cx;
        // The width a moment after it changed.
        if app
            .assistant
            .width_shown
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(1800))
        {
            let s = format!(" {w} cols ");
            let sw = crate::ui_assistant::cells(&s);
            let sx = cx.saturating_sub(sw + 1).max(area.x + 2);
            crate::hits::text(
                buf,
                sx,
                y,
                cx,
                &s,
                Style::default().fg(BAR_BG).bg(theme::SLATE),
            );
            left = sx;
        }
    }
    left
}

#[allow(clippy::too_many_arguments)]
fn ctl_button(
    buf: &mut Buffer,
    hits: &mut Hits,
    app: &App,
    x: u16,
    y: u16,
    glyph: &str,
    c: PanelCtl,
    hint: &str,
    hover_fg: Color,
) {
    let rect = Rect::new(x, y, 3, 1);
    let hovered = app.mouse_pos.is_some_and(|(cx, cy)| contains(rect, cx, cy));
    let st = if hovered {
        Style::default()
            .fg(hover_fg)
            .bg(SEL_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::SLATE).bg(BAR_BG)
    };
    crate::hits::text(buf, x, y, x + 3, &format!(" {glyph} "), st);
    hits.add(rect, UiAction::AssistantPanel(c), hint);
}

fn caret(buf: &mut Buffer, hits: &mut Hits, app: &App, r: Rect, s: &str, c: PanelCtl, hint: &str) {
    let hovered = app.mouse_pos.is_some_and(|(cx, cy)| contains(r, cx, cy));
    let st = if hovered {
        Style::default()
            .fg(FG)
            .bg(SEL_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::SLATE).bg(BAR_BG)
    };
    crate::hits::text(buf, r.x, r.y, r.x + r.width, s, st);
    hits.add(r, UiAction::AssistantPanel(c), hint);
}

/// The left edge: drag it to resize (at normal size).
pub fn edge_region(hits: &mut Hits, app: &App, area: Rect) {
    if app.assistant.win == PanelWin::Normal && area.height > 2 {
        hits.add(
            Rect::new(area.x, area.y + 1, 1, area.height - 2),
            UiAction::AssistantPanel(PanelCtl::Edge),
            "Drag to resize the assistant panel (Ctrl-a { and } too)",
        );
    }
}

/// One quick action: its words, its key, what a click does.
pub struct Quick {
    pub label: String,
    pub key: &'static str,
    pub action: UiAction,
    pub hint: String,
    /// A state worth a color (muted, waiting, on).
    pub tone: Option<Color>,
    /// A small ⓘ at its right end with its own action and hint.
    pub info: Option<(UiAction, String)>,
}

fn q(
    label: impl Into<String>,
    key: &'static str,
    action: UiAction,
    hint: impl Into<String>,
) -> Quick {
    Quick {
        label: label.into(),
        key,
        action,
        hint: hint.into(),
        tone: None,
        info: None,
    }
}

/// The quick actions, most needed first (what does not fit is left out
/// from the end).
pub fn quick_items(app: &App) -> Vec<Quick> {
    let mut v = vec![];
    v.push(if app.voice.muted {
        Quick {
            tone: Some(theme::MUTED_RED),
            ..q(
                "Mic off",
                "C-a X",
                UiAction::MuteToggle,
                "The mic is muted: click to listen again (Ctrl-a X)",
            )
        }
    } else {
        q(
            "Mic",
            "C-a X",
            UiAction::MuteToggle,
            "Mute the mic (Ctrl-a X)",
        )
    });
    v.push(if app.cfg.voice.speaker_muted {
        Quick {
            tone: Some(theme::SAND),
            ..q(
                "Speaker off",
                "C-a O",
                UiAction::SpeakerToggle,
                "The speaker is muted: answers show as text only. Click to talk again (Ctrl-a O)",
            )
        }
    } else {
        q(
            "Speaker",
            "C-a O",
            UiAction::SpeakerToggle,
            "Mute what it says out loud; the mic keeps listening (Ctrl-a O)",
        )
    });
    let mode = match app.voice_mode() {
        0 => "Voice off",
        1 => "Push to talk",
        2 => "Wake word",
        _ => "Open mic",
    };
    v.push(q(
        mode,
        "C-a v",
        UiAction::Key('v'),
        "Switch between push to talk and always listening for the wake word (Ctrl-a v)",
    ));
    v.extend(brainstorm_items(app));
    v.push(q(
        "Summarize tabs",
        "",
        UiAction::AssistantPanel(PanelCtl::Summarize),
        format!("Ask: \"{}\"", crate::app_panelwin::SUMMARIZE_PROMPT),
    ));
    let n = app.waiting_tabs().len();
    v.push(Quick {
        tone: (n > 0).then_some(theme::SAND),
        ..q(
            format!("Approvals {n}"),
            "C-a y",
            UiAction::Key('y'),
            "Every tab waiting for your yes (Ctrl-a y)",
        )
    });
    v.push(if app.direct.on.is_some() {
        Quick {
            tone: Some(theme::SAGE),
            ..q(
                "Direct on",
                "C-a K",
                UiAction::Key('K'),
                "Back to the assistant (Ctrl-a K)",
            )
        }
    } else {
        q(
            "Direct",
            "C-a K",
            UiAction::Key('K'),
            "Direct mode: talk straight to the focused tab (Ctrl-a K)",
        )
    });
    if app.remote_supported().is_ok() {
        let on = app.assistant.remote.is_some();
        v.push(Quick {
            tone: on.then_some(theme::SLATE),
            ..q(
                if on { "Remote on" } else { "Remote" },
                "",
                UiAction::Menu(crate::menus::Cmd::AssistantRemote),
                if on {
                    "Remote Control is on: click to turn it off"
                } else {
                    "Remote Control: talk to the assistant from claude.ai (asks once)"
                },
            )
        });
    }
    v.push(q(
        "Image",
        "C-v",
        UiAction::AssistantPanel(PanelCtl::AttachImage),
        "Attach the clipboard's image (Ctrl-V in the input)",
    ));
    v
}

/// Brainstorm (on: its state), its ⓘ, and Undo after a discard.
pub fn brainstorm_items(app: &App) -> Vec<Quick> {
    use crate::menus::Cmd;
    let mut v = vec![];
    v.push(if app.brainstorm_on() {
        Quick {
            tone: Some(theme::SAND),
            ..q(
                "Brainstorm on",
                "C-a B",
                UiAction::Key('B'),
                "Brainstorm mode: nothing is sent until you say send it (or press Enter). Click to leave it; the draft stays (Ctrl-a B, Esc)",
            )
        }
    } else {
        q(
            "Brainstorm",
            "C-a B",
            UiAction::Key('B'),
            "Brainstorm mode: talk as long as you like, nothing is sent until you say send it (Ctrl-a B, or say \"brainstorm mode\")",
        )
    });
    if let Some(b) = v.last_mut() {
        b.info = Some((
            UiAction::Menu(Cmd::BrainstormInfo),
            "How brainstorm mode works".into(),
        ));
    }
    if app.brainstorm_undo_ready() && !app.brainstorm_on() {
        v.push(Quick {
            tone: Some(theme::SAND),
            ..q(
                "Undo discard",
                "",
                UiAction::Menu(Cmd::BrainstormUndo),
                "Bring the discarded brainstorm back (or say \"undo\")",
            )
        });
    }
    v
}

/// Cells a quick button takes: " label key ".
fn quick_w(it: &Quick) -> u16 {
    let key = if it.key.is_empty() {
        0
    } else {
        crate::ui_assistant::cells(it.key) + 1
    };
    let info = if it.info.is_some() { 2 } else { 0 };
    crate::ui_assistant::cells(&it.label) + key + info + 2
}

/// The quick bar: `rows` rows of buttons from (x, y) to `limit`, in
/// order; what does not fit is left out. Returns the actions drawn.
pub fn draw_quick(
    buf: &mut Buffer,
    hits: &mut Hits,
    app: &App,
    x0: u16,
    y: u16,
    limit: u16,
    rows: u16,
) -> Vec<UiAction> {
    let mut shown = vec![];
    let (mut x, mut row) = (x0, 0u16);
    for it in quick_items(app) {
        let w = quick_w(&it);
        if x + w > limit {
            if x == x0 || row + 1 >= rows {
                // Too wide for a row of its own, or no rows left.
                continue;
            }
            row += 1;
            x = x0;
            if x + w > limit {
                continue;
            }
        }
        let r = Rect::new(x, y + row, w, 1);
        let hovered = app.mouse_pos.is_some_and(|(c, cy)| contains(r, c, cy));
        let (fg, kfg, bg) = if hovered {
            (Color::Rgb(28, 28, 28), Color::Rgb(28, 28, 28), theme::SLATE)
        } else {
            (it.tone.unwrap_or(FG), FAINT, SEL_BG)
        };
        let mut cx = crate::hits::text(buf, x, r.y, x + w, " ", Style::default().bg(bg));
        let mut lst = Style::default().fg(fg).bg(bg);
        if hovered || it.tone.is_some() {
            lst = lst.add_modifier(Modifier::BOLD);
        }
        cx = crate::hits::text(buf, cx, r.y, x + w, &it.label, lst);
        if !it.key.is_empty() {
            cx = crate::hits::text(buf, cx, r.y, x + w, " ", Style::default().bg(bg));
            cx = crate::hits::text(buf, cx, r.y, x + w, it.key, Style::default().fg(kfg).bg(bg));
        }
        let mut main = r;
        if let Some((a, hint)) = &it.info {
            let ir = Rect::new(cx + 1, r.y, 1, 1);
            let ih = app.mouse_pos.is_some_and(|(c, cy)| contains(ir, c, cy));
            cx = crate::hits::text(buf, cx, r.y, x + w, " ", Style::default().bg(bg));
            let ist = if ih {
                Style::default()
                    .fg(FG)
                    .bg(theme::SLATE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(kfg).bg(bg)
            };
            cx = crate::hits::text(buf, cx, r.y, x + w, "ⓘ", ist);
            main.width = ir.x.saturating_sub(r.x).saturating_sub(1);
            hits.add(Rect::new(ir.x, r.y, 2, 1), a.clone(), hint.clone());
        }
        crate::hits::text(buf, cx, r.y, x + w, " ", Style::default().bg(bg));
        hits.add(main, it.action.clone(), it.hint);
        shown.push(it.action);
        x += w + 1;
    }
    shown
}

/// Rows for the quick bar, given the rows left above the input: two on a
/// tall panel, one, or none (it goes first on a short screen).
pub fn quick_rows(free: u16) -> u16 {
    if free >= 18 {
        2
    } else if free >= 8 {
        1
    } else {
        0
    }
}
