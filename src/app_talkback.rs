use crate::app::App;
use serde_json::{json, Value};

pub const INSTANT_KEYS: &[&str] = &[
    "voice.speaker_muted",
    "voice.tts_volume",
    "assistant.speak_typed",
    "assistant.panel",
    "assistant.panel_width",
    "assistant.input_rows",
    "voice.headphone_tap",
];

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
    pub fn speaker_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        let r = match tool {
            "speaker" => self.tool_speaker(args),
            "assistant_panel" => {
                let mut said = vec![];
                if let Some(m) = args["mode"].as_str().filter(|m| !m.trim().is_empty()) {
                    match self.set_panel_mode(m) {
                        Ok(s) => said.push(s),
                        Err(e) => return Some(Err(e)),
                    }
                }
                match self.panel_tool_size(args) {
                    Ok(Some(s)) => said.push(s),
                    Ok(None) => {}
                    Err(e) => return Some(Err(e)),
                }
                if said.is_empty() {
                    Err("say what to change: mode, width or window".into())
                } else {
                    Ok(json!({"ok": true, "result": {"say": said.join(" ")}}))
                }
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
                    "voice.headphone_tap" => {
                        let Some(on) = truthy(v) else {
                            return Some(Err("voice.headphone_tap: true or false".into()));
                        };
                        self.set_headphone_tap(on);
                        let say = if on {
                            "Headphone tap is on: tap your headphones to turn the mic on or off."
                        } else {
                            "Headphone tap is off: play and pause go to your music again."
                        };
                        return Some(Ok(json!({"ok": true, "result": {"say": say}})));
                    }
                    "assistant.panel_width" => {
                        return Some(
                            self.panel_tool_size(&json!({"width": v}))
                                .and_then(|s| {
                                    s.ok_or_else(|| {
                                        "assistant.panel_width: a number of columns (0: automatic)"
                                            .to_string()
                                    })
                                })
                                .map(|say| json!({"ok": true, "result": {"say": say}})),
                        );
                    }
                    "assistant.input_rows" => {
                        let Some(n) = v
                            .as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                        else {
                            return Some(Err("assistant.input_rows: 3 to 20".into()));
                        };
                        let say = self.set_input_rows(n.min(20) as u16);
                        return Some(Ok(json!({"ok": true, "result": {"say": say}})));
                    }
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

    pub fn speaker_state_line(&self) -> String {
        format!(
            "speaker: {} (volume {}%), typed replies spoken: {}, this message came {}; panel: {}, {} (speaker mutes your voice, pause_listening stops hearing)\n",
            if self.cfg.voice.speaker_muted {
                "muted, your replies show as text only"
            } else {
                "on"
            },
            (self.cfg.voice.tts_volume * 100.0).round() as u32,
            if self.cfg.assistant.speak_typed { "yes" } else { "no" },
            if self.assistant.turn_spoken { "by voice" } else { "typed" },
            self.cfg.assistant.panel,
            self.panel_state_words(),
        )
    }
}

pub fn panel_width(app: &App, main: ratatui::layout::Rect) -> u16 {
    crate::app_panelwin::panel_width(app, main)
}

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

pub const SHADOW: ratatui::style::Color = ratatui::style::Color::Rgb(18, 19, 21);

pub const PANEL_MODES: &[&str] = &["docked", "overlay", "auto"];

pub const AUTO_MIN_COLS: u16 = 80;

impl App {
    pub fn panel_docked(&self, main: ratatui::layout::Rect, w: u16) -> bool {
        match self.cfg.assistant.panel.as_str() {
            "overlay" => false,
            "auto" => {
                if self.view != crate::app::View::Grid {
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
