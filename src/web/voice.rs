use super::tap::Outbox;
use crate::app::App;
use crate::voice::audio::WebMic;
use crate::voice::tts::{set_web_out, WebAudio};
use serde_json::json;
use std::sync::Arc;

pub struct WebVoice {
    pub client: u64,

    pub mode: u8,
    pub mic: Arc<WebMic>,

    prev: u8,

    pub muted: bool,
}

pub const WEB_SETTINGS: &[&str] = &[
    "voice.tts",
    "voice.tts_engine",
    "voice.kokoro_voice",
    "voice.grok.voice",
    "voice.tts_voice",
    "voice.tts_speed",
    "voice.tts_volume",
    "voice.wake_words",
    "voice.wake_sensitivity",
    "voice.wake_follow_up_s",
    "voice.open_mic_sleep_min",
    "voice.barge_in",
    "voice.barge_margin_db",
    "voice.barge_in_min_words",
    "voice.barge_in_duck",
    "voice.direct_reply",
    "voice.instant_commands",
    "voice.end_silence_ms",
    "voice.engine",
    "voice.language",
    "voice.speaker_lock",
];

fn mode_num(mode: &str) -> u8 {
    match mode {
        "open" => 3,
        "wake" => 2,
        _ => 1,
    }
}

fn mode_name(m: u8) -> &'static str {
    match m {
        3 => "open",
        2 => "wake",
        _ => "push",
    }
}

pub fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    v
}

fn route_to(out: Arc<Outbox>) {
    set_web_out(Some(Box::new(move |a| match a {
        WebAudio::Pcm(s, rate) => out.audio(wav(&s, rate)),
        WebAudio::Stop => {
            out.text(json!({"t": "audio_stop"}).to_string());
        }
        WebAudio::Said(t) => {
            out.text(json!({"t": "audio_said", "text": t}).to_string());
        }
        WebAudio::Duck(v) => {
            out.text(
                json!({"t": "audio_duck", "level": (f64::from(v) * 100.0).round() / 100.0})
                    .to_string(),
            );
        }
    })));
}

impl App {
    pub fn web_mic(&self) -> Option<Arc<WebMic>> {
        let v = self.web.as_ref()?.voice.as_ref()?;
        Some(Arc::clone(&v.mic))
    }

    pub fn web_voice_client(&self) -> Option<u64> {
        Some(self.web.as_ref()?.voice.as_ref()?.client)
    }

    fn web_voice_reply(&self, client: u64, v: serde_json::Value) {
        if let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) {
            let mut v = v;
            v["t"] = json!("voice");
            c.out.text(v.to_string());
        }
    }

    fn web_voice_refuse(&self, client: u64, code: &str, text: &str) {
        crate::log::info(&format!("web: client {client} voice refused: {text}"));
        self.web_voice_reply(client, json!({"ok": false, "code": code, "error": text}));
    }

    pub fn web_voice(&mut self, client: u64, on: Option<bool>, mode: &str, talk: Option<bool>) {
        let mine = self.web_voice_client() == Some(client);

        let on = match (on, talk) {
            (None, Some(true)) if !mine => Some(true),
            _ => on,
        };
        match on {
            Some(true) => {
                let m = mode_num(mode);
                let same = mine
                    && self
                        .web
                        .as_ref()
                        .and_then(|w| w.voice.as_ref())
                        .map(|v| v.mode)
                        == Some(m);
                if !same && !self.web_voice_on(client, m) {
                    return;
                }
                if !same {
                    self.web_voice_reply(
                        client,
                        json!({"ok": true, "on": true, "mode": mode_name(m)}),
                    );
                }
            }
            Some(false) if mine => {
                self.web_voice_off("the browser turned it off");
                self.web_voice_reply(client, json!({"ok": true, "on": false}));
            }
            _ => {}
        }
        let mine = self.web_voice_client() == Some(client);
        if !mine {
            return;
        }
        let listening = self.voice.status == crate::voice::VoiceStatus::Listening;
        match talk {
            Some(true) => {
                if let Some(m) = self.web_mic() {
                    m.take_stats();
                }
                crate::log::info(&format!("web: client {client} pressed push to talk"));
                if !listening {
                    self.push_to_talk()
                }
            }
            Some(false) => {
                let s = self.web_mic().map(|m| m.take_stats()).unwrap_or_default();
                crate::log::info(&format!(
                    "web: client {client} released push to talk: {}{}",
                    s.line(),
                    if s.frames == 0 {
                        " (no audio came from the browser)"
                    } else if s.nonzero == 0 {
                        " (only silence: the browser's mic gave zeros)"
                    } else {
                        ""
                    }
                ));

                match &self.voice.engine {
                    Some(v) if !listening => v.stop_talk(),
                    _ if listening => self.push_to_talk(),
                    _ => {}
                }
            }
            None => {}
        }
    }

    fn web_voice_setting(
        &mut self,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<String, String> {
        if !WEB_SETTINGS.contains(&key) {
            return Err(format!("{key} is not a voice setting the web app changes"));
        }
        let text = match value {
            serde_json::Value::String(t) => t.clone(),
            serde_json::Value::Null => return Err("no value".into()),
            o => o.to_string(),
        };
        let st = self.find_setting(key, None)?;
        let (item, shown) = self.setting_item(&st, &text)?;
        crate::settings::write(&crate::config::Config::path(), &st.key, Some(item))
            .map_err(|e| format!("not saved: {e:#}"))?;
        self.state_dirty = true;
        self.reload_config_now();
        Ok(shown)
    }

    fn web_voice_on(&mut self, client: u64, mode: u8) -> bool {
        if self.voice.muted {
            self.web_voice_refuse(
                client,
                "muted",
                "The mic is muted in GodTerm (Ctrl-a X unmutes)",
            );
            return false;
        }
        let Some(w) = self.web.as_mut() else {
            return false;
        };
        let Some(c) = w.hub.client(client) else {
            return false;
        };

        let prev = match w.voice.take() {
            Some(old) => {
                if let Some(oc) = w.hub.client(old.client) {
                    *oc.mic.lock().unwrap_or_else(|e| e.into_inner()) = None;

                    if old.client != client {
                        oc.out.text(
                            json!({"t": "voice", "ok": true, "on": false, "moved": true})
                                .to_string(),
                        );
                    }
                }
                old.prev
            }

            None if self.voice.engine.is_none() && !self.voice.open_mic_asleep => 0,
            None => self.voice_mode(),
        };
        let Some(w) = self.web.as_mut() else {
            return false;
        };
        let mic = WebMic::new();
        *c.mic.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&mic));
        w.voice = Some(WebVoice {
            client,
            mode,
            mic,
            prev,
            muted: false,
        });
        w.changed = true;
        route_to(Arc::clone(&c.out));
        {
            let held = c.tts.lock().unwrap_or_else(|e| e.into_inner());
            crate::voice::web_tts::set_route(Some((client, held.clone())));
        }
        crate::log::info(&format!(
            "web: client {client} is the voice now ({}); the Mac's mode was {prev}",
            match mode {
                3 => "open mic",
                2 => "wake word",
                _ => "push to talk",
            }
        ));
        crate::voice::webdec::set_ffmpeg(&self.cfg.voice.ffmpeg);

        self.voice.reopen = None;
        match mode {
            3 => self.start_open_mic(),
            m => {
                self.voice.open_mic = false;
                self.voice.open_mic_asleep = false;
                self.stop_voice();
                self.start_voice(m == 2);
            }
        }
        true
    }

    pub fn web_voice_ctl(&mut self, client: u64, v: &serde_json::Value) {
        if v["unmute_mac"].as_bool() == Some(true) && self.voice.muted {
            crate::log::info(&format!("web: client {client} unmuted GodTerm's mic"));
            self.toggle_mute();
        }
        if let Some(m) = v["muted"].as_bool() {
            self.web_mic_muted(client, m);
        }
        if let Some(a) = v["away"].as_bool() {
            crate::log::info(&format!(
                "web: client {client} {}",
                if a {
                    "went to the background (the phone stops its mic)"
                } else {
                    "is back in the foreground"
                }
            ));
        }
        if v["hush"].as_bool() == Some(true) && self.web_voice_client() == Some(client) {
            crate::log::info(&format!("web: client {client} stopped the reply"));
            self.assistant_barge_in(0, 0);
            if let Some(e) = &self.voice.engine {
                e.stop_speaking();
            }
        }
        match &v["direct"] {
            serde_json::Value::Bool(false) => {
                if self.direct.on.is_some() {
                    self.end_direct("from the web app", true);
                }
            }
            d => {
                let uid = d.as_u64().or_else(|| {
                    d.as_str()
                        .and_then(|s| s.trim_start_matches('t').parse().ok())
                });
                if let Some(uid) = uid {
                    if !self.direct_with(uid) {
                        self.mic_move_to(uid);
                    }
                }
            }
        }
        if let Some(set) = v.get("set").filter(|s| s.is_object()) {
            let key = set["key"].as_str().unwrap_or("").to_string();
            let r = self.web_voice_setting(&key, &set["value"]);
            crate::log::info(&format!(
                "web: client {client} voice setting {key}: {}",
                match &r {
                    Ok(v) => format!("now {v}"),
                    Err(e) => e.clone(),
                }
            ));
            self.web_voice_reply(
                client,
                match r {
                    Ok(v) => json!({"set": key, "ok": true, "value": v}),
                    Err(e) => json!({"set": key, "ok": false, "error": e}),
                },
            );
        }
        if v["web_tts_list"].as_bool() == Some(true) {
            self.web_tts_list(client);
        }
        if let Some(b) = v["brainstorm"].as_bool() {
            if b != self.brainstorm_on() {
                self.toggle_brainstorm();
            }
        }
        if let Some(w) = self.web.as_mut() {
            w.changed = true;
        }
    }

    fn web_mic_muted(&mut self, client: u64, on: bool) {
        let Some(wv) = self
            .web
            .as_mut()
            .and_then(|w| w.voice.as_mut())
            .filter(|wv| wv.client == client)
        else {
            return;
        };
        if wv.muted == on {
            return;
        }
        wv.muted = on;
        crate::log::info(&format!(
            "web: client {client} {} its mic",
            if on { "muted" } else { "unmuted" }
        ));
        if !on {
            self.voice.last_heard = std::time::Instant::now();
        }
        self.voice.action = Some(if on { "web mic muted" } else { "web mic on" }.into());
    }

    pub fn web_mic_is_muted(&self) -> bool {
        self.web
            .as_ref()
            .and_then(|w| w.voice.as_ref())
            .is_some_and(|wv| wv.muted)
    }

    fn web_tts_list(&self, client: u64) {
        let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) else {
            return;
        };
        let mac = self.cfg.voice.clone();
        let _ = std::thread::Builder::new()
            .name("web-tts-list".into())
            .spawn(move || {
                let lists = crate::voice::web_tts::Lists::now(&mac);
                let choice = c.tts.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let v = crate::voice::web_tts::list(&mac, &lists, choice.as_ref());
                c.out.text(v.to_string());
            });
    }

    pub fn web_voice_off(&mut self, why: &str) {
        let Some(w) = self.web.as_mut() else {
            return;
        };
        let Some(v) = w.voice.take() else {
            return;
        };
        w.changed = true;
        if let Some(c) = w.hub.client(v.client) {
            *c.mic.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        set_web_out(None);
        crate::voice::web_tts::set_route(None);
        crate::log::info(&format!(
            "web: client {} is no longer the voice ({why}); back to mode {}",
            v.client, v.prev
        ));
        self.voice.open_mic = false;
        self.voice.open_mic_asleep = false;
        self.stop_voice();
        match v.prev {
            0 => {}
            3 => self.start_open_mic(),
            m => self.start_voice_in(m),
        }
    }
}
