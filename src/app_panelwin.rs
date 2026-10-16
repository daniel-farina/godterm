//! The assistant panel as a window: its width (the < > carets in its
//! title, Ctrl-a { and }, a drag on its left edge, set_setting
//! assistant.panel_width, "make the assistant wider"), minimize to a thin
//! strip at the right edge (voice keeps working), maximize over the grid,
//! close; and the quick actions above its input.

use std::time::Instant;

use serde_json::Value;

use crate::app::App;

/// The narrowest panel.
pub const PANEL_MIN: u16 = 36;
/// The panes keep at least this many columns beside it.
pub const GRID_MIN: u16 = 40;
/// One click of a caret (or Ctrl-a { / }).
pub const STEP: u16 = 8;
/// The minimized strip.
pub const STRIP_W: u16 = 3;
/// The input's rows before it grows.
pub const INPUT_MIN_ROWS: u16 = 3;

/// The canned request of the "Summarize tabs" button.
pub const SUMMARIZE_PROMPT: &str = "What's everyone working on?";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelWin {
    #[default]
    Normal,
    /// A thin strip at the right edge.
    Min,
    /// Over the whole grid area.
    Max,
}

/// The panel's window controls and quick actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelCtl {
    /// Minimize (minimized: restore).
    Minimize,
    /// Maximize (maximized: restore).
    Maximize,
    Close,
    /// The strip: back to its normal size.
    Restore,
    Wider,
    Narrower,
    /// A press on its left edge starts a drag.
    Edge,
    /// Ask "what's everyone working on?".
    Summarize,
    /// The clipboard's image into the input.
    AttachImage,
}

/// Automatic: half the main area, 44 to 76 columns.
pub fn auto_width(main_w: u16) -> u16 {
    (main_w / 2).clamp(44, 76)
}

/// The widest it goes: the panes keep GRID_MIN columns.
pub fn max_width(main_w: u16) -> u16 {
    main_w.saturating_sub(GRID_MIN).max(PANEL_MIN)
}

/// Its width at normal size, from the setting (0: automatic).
pub fn normal_width(set: u16, main_w: u16) -> u16 {
    let w = if set == 0 {
        auto_width(main_w)
    } else {
        set.clamp(PANEL_MIN, max_width(main_w))
    };
    w.min(main_w)
}

/// The panel's width in `main` this frame (0: not shown).
pub fn panel_width(app: &App, main: ratatui::layout::Rect) -> u16 {
    if !app.assistant.show {
        return 0;
    }
    match app.assistant.win {
        PanelWin::Min if main.width >= 20 => STRIP_W,
        PanelWin::Max if main.width >= 20 => main.width,
        _ if main.width >= 50 => normal_width(app.cfg.assistant.panel_width, main.width),
        _ => 0,
    }
}

impl App {
    /// A window control or quick action of the panel.
    pub fn panel_ctl(&mut self, c: PanelCtl) {
        match c {
            PanelCtl::Minimize if self.assistant.win == PanelWin::Min => {
                self.panel_ctl(PanelCtl::Restore)
            }
            PanelCtl::Minimize => {
                self.assistant.win = PanelWin::Min;
                self.assistant.focused = false;
                self.assistant.edge_drag = false;
                self.flash("Assistant minimized: voice still works. Click the strip or Ctrl-a . to restore");
            }
            PanelCtl::Maximize => {
                self.assistant.win = if self.assistant.win == PanelWin::Max {
                    PanelWin::Normal
                } else {
                    PanelWin::Max
                };
                self.assistant.show = true;
                self.assistant.focused = true;
            }
            PanelCtl::Close => {
                self.assistant.show = false;
                self.assistant.focused = false;
                self.assistant.win = PanelWin::Normal;
            }
            PanelCtl::Restore => {
                self.assistant.win = PanelWin::Normal;
                self.assistant.show = true;
                self.assistant.focused = true;
            }
            PanelCtl::Wider => {
                self.nudge_panel_width(STEP as i32);
            }
            PanelCtl::Narrower => {
                self.nudge_panel_width(-(STEP as i32));
            }
            PanelCtl::Edge => {
                if self.assistant.win == PanelWin::Normal {
                    self.assistant.edge_drag = true;
                }
            }
            PanelCtl::Summarize => {
                self.back_to_conversation();
                self.assistant.focused = true;
                self.ask_assistant(SUMMARIZE_PROMPT);
            }
            PanelCtl::AttachImage => {
                self.assistant.focused = true;
                self.paste_clipboard_image(false);
            }
        }
    }

    /// The main area's width the panel was drawn in (none yet: the
    /// terminal's).
    fn panel_main_w(&self) -> u16 {
        match self.assistant.main_geo.get() {
            (_, 0) => crossterm::terminal::size().map(|s| s.0).unwrap_or(160),
            (_, w) => w,
        }
    }

    /// The panel's width now at normal size.
    pub fn panel_width_now(&self) -> u16 {
        normal_width(self.cfg.assistant.panel_width, self.panel_main_w())
    }

    /// Ctrl-a { / }, a caret, "make the assistant wider": by `d` columns,
    /// kept between PANEL_MIN and what leaves the panes GRID_MIN. Back to
    /// normal size first when minimized or maximized. Returns the width.
    pub fn nudge_panel_width(&mut self, d: i32) -> u16 {
        self.assistant.show = true;
        self.assistant.win = PanelWin::Normal;
        let main_w = self.panel_main_w();
        let cur = self.panel_width_now();
        let w = (cur as i32 + d).clamp(PANEL_MIN as i32, max_width(main_w) as i32) as u16;
        self.set_panel_width(w);
        if w == cur {
            let edge = if d > 0 {
                format!("the widest that leaves the panes {GRID_MIN}")
            } else {
                "the narrowest".to_string()
            };
            self.flash(format!("Assistant panel: {w} columns ({edge})"));
        }
        w
    }

    /// assistant.panel_width, kept in config.toml (0: automatic).
    pub fn set_panel_width(&mut self, w: u16) {
        self.write_panel_width(w);
        self.flash(if w == 0 {
            format!(
                "Assistant panel: automatic width ({} columns)",
                self.panel_width_now()
            )
        } else {
            format!("Assistant panel: {w} columns (Ctrl-a {{ / }} resize)")
        });
    }

    fn write_panel_width(&mut self, w: u16) {
        let _ = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("assistant.panel_width"),
            Some(toml_edit::value(w as i64)),
        );
        self.config_mtime = crate::app::config_mtime();
        self.cfg.assistant.panel_width = w;
        self.assistant.width_shown = Some(Instant::now());
    }

    /// Dragging the panel's left edge to column `col`.
    pub fn drag_panel_edge(&mut self, col: u16) {
        let (x, main_w) = self.assistant.main_geo.get();
        if main_w == 0 {
            return;
        }
        let right = x + main_w;
        let w = right
            .saturating_sub(col)
            .clamp(PANEL_MIN, max_width(main_w));
        self.cfg.assistant.panel_width = w;
        self.assistant.width_shown = Some(Instant::now());
    }

    /// The drag ended: the width is kept.
    pub fn end_panel_drag(&mut self) {
        self.assistant.edge_drag = false;
        let w = self.cfg.assistant.panel_width;
        self.write_panel_width(w);
        self.flash(format!("Assistant panel: {w} columns"));
    }

    /// assistant.input_rows, kept in config.toml.
    pub fn set_input_rows(&mut self, n: u16) -> String {
        let n = n.clamp(INPUT_MIN_ROWS, 20);
        let _ = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("assistant.input_rows"),
            Some(toml_edit::value(n as i64)),
        );
        self.config_mtime = crate::app::config_mtime();
        self.cfg.assistant.input_rows = n;
        format!("The input grows to {n} rows.")
    }

    /// The assistant_panel tool's width ("wider", "narrower", "auto", a
    /// number, "+16") and window ("minimize", "maximize", "restore",
    /// "close").
    pub fn panel_tool_size(&mut self, args: &Value) -> Result<Option<String>, String> {
        let mut said = vec![];
        if let Some(v) = args.get("width").filter(|v| !v.is_null()) {
            let w = match v {
                Value::Number(n) => {
                    let n = n.as_f64().ok_or("width: a number of columns")?.round();
                    self.panel_width_set_to(n.max(0.0) as u16)
                }
                Value::String(s) => {
                    let t = s.trim().to_lowercase();
                    match t.as_str() {
                        "wider" | "bigger" | "more" | "larger" | "grow" => {
                            self.nudge_panel_width(STEP as i32)
                        }
                        "narrower" | "smaller" | "less" | "thinner" | "shrink" => {
                            self.nudge_panel_width(-(STEP as i32))
                        }
                        "auto" | "automatic" | "default" | "normal" | "reset" => {
                            self.panel_width_set_to(0)
                        }
                        o => {
                            let rel = o.starts_with('+') || o.starts_with('-');
                            let n: i32 = o
                                .trim_start_matches('+')
                                .trim_end_matches("columns")
                                .trim_end_matches("cols")
                                .trim()
                                .parse()
                                .map_err(|_| {
                                    "width: wider, narrower, auto, a number of columns, or +8 / -8"
                                        .to_string()
                                })?;
                            if rel {
                                self.nudge_panel_width(n)
                            } else {
                                self.panel_width_set_to(n.max(0) as u16)
                            }
                        }
                    }
                }
                _ => return Err("width: wider, narrower, auto or a number".into()),
            };
            said.push(if self.cfg.assistant.panel_width == 0 {
                format!("The panel is back to its automatic width, {w} columns.")
            } else {
                format!("The panel is {w} columns wide.")
            });
        }
        if let Some(v) = args.get("window").and_then(Value::as_str) {
            let t = v.trim().to_lowercase();
            let (c, say) = match t.as_str() {
                "minimize" | "minimise" | "min" | "hide" | "collapse" => (
                    PanelCtl::Minimize,
                    "Minimized to a strip at the right; I'm still listening.",
                ),
                "maximize" | "maximise" | "max" | "full" | "fullscreen" => {
                    (PanelCtl::Maximize, "The panel takes the whole grid now.")
                }
                "restore" | "normal" | "open" | "show" => {
                    (PanelCtl::Restore, "The panel is back to its normal size.")
                }
                "close" => (
                    PanelCtl::Close,
                    "Closed the panel; Ctrl-a . opens it again.",
                ),
                _ => return Err("window: minimize, maximize, restore or close".into()),
            };
            match c {
                // Maximize toggles; by voice it means maximized.
                PanelCtl::Maximize if self.assistant.win == PanelWin::Max => {}
                PanelCtl::Minimize if self.assistant.win == PanelWin::Min => {}
                c => self.panel_ctl(c),
            }
            said.push(say.to_string());
        }
        Ok((!said.is_empty()).then(|| said.join(" ")))
    }

    /// "60 columns wide (automatic), minimized", for the state block.
    pub fn panel_state_words(&self) -> String {
        let w = self.panel_width_now();
        let auto = if self.cfg.assistant.panel_width == 0 {
            " (automatic)"
        } else {
            ""
        };
        let win = match self.assistant.win {
            PanelWin::Normal => "",
            PanelWin::Min => ", minimized to a strip",
            PanelWin::Max => ", maximized over the grid",
        };
        format!("{w} columns wide{auto}{win}")
    }

    /// An absolute width (0: automatic), clamped; returns the width now.
    fn panel_width_set_to(&mut self, w: u16) -> u16 {
        self.assistant.show = true;
        self.assistant.win = PanelWin::Normal;
        let main_w = self.panel_main_w();
        let w = if w == 0 {
            0
        } else {
            w.clamp(PANEL_MIN, max_width(main_w))
        };
        self.set_panel_width(w);
        self.panel_width_now()
    }
}
