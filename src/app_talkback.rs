//! The speaker (talk back) mute, typed versus spoken replies, the volume
//! and the assistant panel's mode, as tools the assistant runs at once.
//!
//! "Be quiet" mutes the speaker (it stops talking; the mic keeps
//! listening); "hold on" is pause_listening (it stops hearing).

use crate::app::App;
use serde_json::{json, Value};

/// Settings that change at once from set_setting, with no yes: they are
/// how it talks and looks, nothing to undo.
pub const INSTANT_KEYS: &[&str] = &[
    "voice.speaker_muted",
    "voice.tts_volume",
    "assistant.speak_typed",
    "assistant.panel",
];

/// "50%", "50", "0.5", "half" -> 0.5.
pub fn parse_volume(v: &Value) -> Option<f32> {
    let n = match v {
        Value::Number(n) => n.as_f64()? as f32,
        Value::String(s) => {
            let t = s.trim().trim_end_matches('%').trim().to_lowercase();
            match t.as_str() {
                "half" => 0.5,
                "full" | "max" | "maximum" => 1.0,
                _ => t.parse::<f32>().ok()?,
            }
        }
        _ => return None,
    };
    if !(0.0..=100.0).contains(&n) {
        return None;
    }
    Some(if n > 1.0 { n / 100.0 } else { n })
}

fn truthy(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Some(true),
            "false" | "off" | "no" | "0" => Some(false),
            _ => None,
        },
        Value::Number(n) => Some(n.as_f64() != Some(0.0)),
        _ => None,
    }
}

impl App {
    /// The speaker and panel tools, and set_setting on INSTANT_KEYS.
    pub fn speaker_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        let r = match tool {
            "speaker" => self.tool_speaker(args),
            "assistant_panel" => {
                let m = args["mode"].as_str().unwrap_or("");
                self.set_panel_mode(m)
                    .map(|say| json!({"ok": true, "result": {"say": say}}))
            }
            "set_setting"
                if args.get("account").is_none_or(Value::is_null)
                    && INSTANT_KEYS.contains(&args["key"].as_str().unwrap_or("")) =>
            {
                let v = &args["value"];
                let a = match args["key"].as_str().unwrap_or("") {
                    "voice.speaker_muted" => json!({"muted": v}),
                    "voice.tts_volume" => json!({"volume": v}),
                    "assistant.speak_typed" => json!({"speak_typed": v}),
                    _ => {
                        let m = v.as_str().unwrap_or("").to_string();
                        return Some(
                            self.set_panel_mode(&m)
                                .map(|say| json!({"ok": true, "result": {"say": say}})),
                        );
                    }
                };
                self.tool_speaker(&a)
            }
            _ => return None,
        };
        Some(r)
    }

    fn tool_speaker(&mut self, args: &Value) -> Result<Value, String> {
        let mut said: Vec<String> = vec![];
        if let Some(v) = args.get("volume").filter(|v| !v.is_null()) {
            let vol = parse_volume(v).ok_or("volume: 0 to 100 (percent)")?;
            self.set_tts_volume(vol);
            said.push(format!("Volume {}%.", (vol * 100.0).round() as u32));
        }
        if let Some(v) = args.get("speak_typed").filter(|v| !v.is_null()) {
            let on = truthy(v).ok_or("speak_typed: true or false")?;
            self.set_speak_typed(on);
            said.push(if on {
                "I'll speak my replies to typed messages too.".into()
            } else {
                "Typed messages get a text answer only.".into()
            });
        }
        if let Some(v) = args.get("muted").filter(|v| !v.is_null()) {
            let on = truthy(v).ok_or("muted: true or false")?;
            // Unmuting first, so the confirmation is heard.
            self.set_speaker_muted(on);
            said.push(if on {
                "Muted; I'll answer in text.".into()
            } else {
                "Okay, I'm talking again.".into()
            });
        }
        if said.is_empty() {
            return Err("say what to change: muted, volume or speak_typed".into());
        }
        Ok(json!({"ok": true, "result": {"say": said.join(" "), "speaker": self.speaker_state()}}))
    }

    pub fn set_speak_typed(&mut self, on: bool) {
        let _ = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("assistant.speak_typed"),
            Some(toml_edit::value(on)),
        );
        self.config_mtime = crate::app::config_mtime();
        self.cfg.assistant.speak_typed = on;
    }

    fn speaker_state(&self) -> Value {
        json!({
            "muted": self.cfg.voice.speaker_muted,
            "volume_pct": (self.cfg.voice.tts_volume * 100.0).round() as u32,
            "typed_replies_spoken": self.cfg.assistant.speak_typed,
        })
    }

    /// The state block's line on talking back and the panel.
    pub fn speaker_state_line(&self) -> String {
        format!(
            "speaker: {} (volume {}%), typed replies spoken: {}, this message came {}; panel: {} (speaker mutes your voice, pause_listening stops hearing)\n",
            if self.cfg.voice.speaker_muted {
                "muted, your replies show as text only"
            } else {
                "on"
            },
            (self.cfg.voice.tts_volume * 100.0).round() as u32,
            if self.cfg.assistant.speak_typed { "yes" } else { "no" },
            if self.assistant.turn_spoken { "by voice" } else { "typed" },
            self.cfg.assistant.panel,
        )
    }
}

/// The panel's width in `main` (0: not shown).
pub fn panel_width(app: &App, main: ratatui::layout::Rect) -> u16 {
    if app.assistant.show && main.width >= 50 {
        (main.width / 2).clamp(44, 76)
    } else {
        0
    }
}

/// The floating panel's edge: a one column shadow left of it, so it reads
/// as a layer over the pane beneath.
pub fn draw_overlay_edge(
    f: &mut ratatui::Frame,
    r: ratatui::layout::Rect,
    main: ratatui::layout::Rect,
) {
    if r.x <= main.x {
        return;
    }
    let buf = f.buffer_mut();
    let x = r.x - 1;
    for y in r.y..r.y + r.height {
        if let Some(c) = buf.cell_mut((x, y)) {
            c.set_char(' ');
            c.set_style(ratatui::style::Style::default().bg(SHADOW));
        }
    }
}

/// A calm, dark shadow (no glow).
pub const SHADOW: ratatui::style::Color = ratatui::style::Color::Rgb(18, 19, 21);

pub const PANEL_MODES: &[&str] = &["docked", "overlay", "auto"];

/// Under this many columns a pane is too narrow to share the screen with
/// a docked panel (auto mode floats it then).
pub const AUTO_MIN_COLS: u16 = 80;

impl App {
    /// Whether the panel docks this frame (auto: while every visible
    /// pane keeps about 80 columns beside it).
    pub fn panel_docked(&self, main: ratatui::layout::Rect, w: u16) -> bool {
        match self.cfg.assistant.panel.as_str() {
            "overlay" => false,
            "auto" => {
                if self.view != crate::app::View::Grid {
                    // Other screens have no panes to keep wide.
                    return true;
                }
                let left = ratatui::layout::Rect::new(
                    main.x,
                    main.y,
                    main.width.saturating_sub(w),
                    main.height,
                );
                let (shown, _, _) = crate::ui::pane_rects(self, left);
                shown
                    .iter()
                    .filter(|r| r.width > 0)
                    .all(|r| r.width >= AUTO_MIN_COLS)
            }
            _ => true,
        }
    }

    /// docked / overlay / auto, kept in config.toml; at once.
    pub fn set_panel_mode(&mut self, mode: &str) -> Result<String, String> {
        let m = mode.trim().to_lowercase();
        let m = match m.as_str() {
            "float" | "floating" | "over" => "overlay",
            "dock" | "beside" => "docked",
            o => o,
        }
        .to_string();
        if !PANEL_MODES.contains(&m.as_str()) {
            return Err("panel mode: docked, overlay or auto".into());
        }
        let _ = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("assistant.panel"),
            Some(toml_edit::value(m.clone())),
        );
        self.config_mtime = crate::app::config_mtime();
        self.cfg.assistant.panel = m.clone();
        self.state_dirty = true;
        let say = match m.as_str() {
            "docked" => "The panel is docked; the panes make room for it.",
            "overlay" => "The panel floats over the panes now; they keep their size.",
            _ => "The panel docks while the panes stay wide enough, and floats otherwise.",
        };
        self.flash(say);
        Ok(say.to_string())
    }
}
