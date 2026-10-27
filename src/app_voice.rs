use std::time::{Duration, Instant};

use crate::app::{App, Modal, View};
use crate::keys;
use crate::pane::Activity;
use crate::prompt::Choice;
use crate::voice::audio::Source;
use crate::voice::grammar;
use crate::voice::tts::Kind;
use crate::voice::{Voice, VoiceEvent, VoiceStatus};

pub struct VoiceState {
    pub engine: Option<Voice>,
    pub status: VoiceStatus,
    pub always_on: bool,
    pub asleep: bool,

    pub file: Option<String>,
    pub heard: Option<String>,
    pub action: Option<String>,
    pub info: Option<String>,

    pub pending: Option<String>,
    pub updated: Instant,

    pub hold_supported: bool,

    pub holding: Option<Instant>,

    pub(crate) wake_until: Option<Instant>,

    pub listen_after_speech: bool,

    pub said_wake: Vec<String>,
    notified: crate::notify::Limiter<(usize, usize, bool)>,

    pub term_focused: Option<bool>,

    pub open_mic: bool,

    pub last_heard: Instant,

    pub cur_stats: crate::voice::wake_profile::UttStats,

    pub vocab_sent: Option<String>,

    pub partial: Option<String>,

    pub partial_ms: Option<u128>,

    first_partial: Option<Instant>,

    was_speaking: bool,

    pub training: Option<crate::app_wake::Training>,

    pub profile: Option<crate::voice::wake_profile::WakeProfile>,

    pub preview: Option<crate::voice::tts::Speaker>,

    pub log: std::collections::VecDeque<LogEntry>,

    pub show_log: bool,

    pub ignored: u32,

    pub muted: bool,
    pub muted_prev: u8,

    pub last_on_mode: u8,

    pub reopen: Option<(Instant, u8, u32)>,

    pub open_mic_after_speech: bool,

    pub started_at: Option<Instant>,

    pub open_mic_asleep: bool,

    pub wake_to_open_mic: bool,

    pub paused: Option<crate::app_pause::Pause>,

    pub away_summary: Option<String>,

    pub not_you: u32,
    pub speaker_last: Option<crate::voice::speaker::Check>,
    pub speaker_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub at: chrono::DateTime<chrono::Local>,
    pub heard: String,
    pub action: String,

    pub partial_ms: Option<u128>,
}

pub const LOG_LEN: usize = 5;

impl VoiceState {
    pub fn cur_stats_stt(&self) -> Option<u32> {
        (self.cur_stats.stt_ms > 0).then_some(self.cur_stats.stt_ms)
    }
}

impl Default for VoiceState {
    fn default() -> Self {
        VoiceState {
            engine: None,
            status: VoiceStatus::Off(None),
            always_on: false,
            asleep: false,
            file: None,
            heard: None,
            action: None,
            info: None,
            pending: None,
            updated: Instant::now(),
            hold_supported: false,
            holding: None,
            wake_until: None,
            listen_after_speech: false,
            said_wake: vec![],
            notified: crate::notify::Limiter::new(Duration::from_secs(60), Duration::from_secs(3)),
            term_focused: None,
            open_mic: false,
            last_heard: Instant::now(),
            cur_stats: Default::default(),
            vocab_sent: None,
            partial: None,
            partial_ms: None,
            first_partial: None,
            log: Default::default(),
            preview: None,
            training: None,
            profile: crate::voice::wake_profile::WakeProfile::load(),
            was_speaking: false,
            show_log: false,
            ignored: 0,
            muted: false,
            muted_prev: 0,
            last_on_mode: 0,
            reopen: None,
            open_mic_after_speech: false,
            started_at: None,
            open_mic_asleep: false,
            wake_to_open_mic: false,
            paused: None,
            away_summary: None,
            not_you: 0,
            speaker_last: None,
            speaker_profile: crate::voice::speaker::SpeakerProfile::load().map(|p| p.summary()),
        }
    }
}

fn spell(n: usize) -> String {
    const W: [&str; 11] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    W.get(n)
        .map(|s| s.to_string())
        .unwrap_or_else(|| n.to_string())
}

impl App {
    pub fn vocabulary(&self) -> String {
        let mut words: Vec<String> = vec!["GodTerm".into()];
        words.extend(self.cfg.voice.wake_words.iter().cloned());
        words.extend(
            [
                "approve",
                "deny",
                "always allow",
                "yes",
                "no",
                "next tab",
                "previous tab",
                "new tab",
                "close tab",
                "usage",
                "quota",
                "dashboard",
                "overview",
                "sessions",
                "settings",
                "loops",
                "stop all loops",
                "zoom",
                "layout",
                "open mic",
                "stop talking",
                "read last",
                "memory saver",
                "approvals",
                "back to the assistant",
                "talk to",
                "Claude",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        for (i, a) in self.cfg.accounts.iter().enumerate() {
            words.push(format!("account {}", spell(i + 1)));
            let l = a.display().to_string();
            if !l.to_lowercase().starts_with("account") {
                words.push(l);
            }
        }
        for slot in &self.panes {
            for t in slot.tabs.iter().take(6) {
                let n = t.name();
                if !words.contains(&n) {
                    words.push(n);
                }
            }
        }
        for r in self.recents.iter().take(8) {
            if let Some(f) = std::path::Path::new(&r.path).file_name() {
                let f = f.to_string_lossy().into_owned();
                if !words.contains(&f) {
                    words.push(f);
                }
            }
        }

        {
            let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
            let mut rows: Vec<&crate::session_index::Row> =
                ix.rows.iter().filter(|r| r.info.parent.is_none()).collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.info.modified));
            let mut folders = 0;
            for r in &rows {
                if folders >= 12 {
                    break;
                }
                let Some(f) = std::path::Path::new(&r.info.cwd)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                else {
                    continue;
                };
                if f.starts_with('.') || words.contains(&f) || crate::sess_sort::headless(&r.info) {
                    continue;
                }
                let spaced = f.replace(['-', '_'], " ");
                words.push(f);
                if !words.contains(&spaced) {
                    words.push(spaced);
                }
                folders += 1;
            }
            for r in rows.iter().filter(|r| r.info.title.is_some()).take(6) {
                let t = crate::sessions::snippet(r.info.title.as_deref().unwrap_or(""), 40);
                if !words.contains(&t) {
                    words.push(t);
                }
            }
        }

        let mut out = String::new();
        for w in words {
            if out.len() + w.len() > 800 {
                break;
            }
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(&w);
        }
        out
    }

    pub fn refresh_vocabulary(&mut self) {
        let v = self.vocabulary();
        if self.voice.vocab_sent.as_deref() != Some(v.as_str()) {
            if let Some(e) = &self.voice.engine {
                e.set_vocabulary(v.clone());
                self.voice.vocab_sent = Some(v);
            }
        }
    }

    pub fn start_voice_in(&mut self, m: u8) {
        match m {
            3 => {
                self.voice.open_mic = true;
                self.voice.asleep = false;
                self.voice.last_heard = Instant::now();
                self.start_voice(true);
            }
            1 => self.start_voice(false),
            _ => self.start_voice(true),
        }
    }

    pub fn voice_mode_tick(&mut self) {
        let m = if self.voice.open_mic_asleep {
            3
        } else {
            self.voice_mode()
        };
        if m > 0 && !self.voice.muted {
            self.voice.last_on_mode = m;
        }
        if self.voice.wake_to_open_mic && !self.tap_speaking() {
            self.voice.wake_to_open_mic = false;
            self.voice.open_mic_asleep = false;
            if !self.voice.muted && !self.voice.open_mic {
                crate::log::info("voice: the wake word woke open mic: open mic again");
                self.stop_voice();
                self.start_voice_in(3);
            }
        }
        if self.voice.open_mic_after_speech && !self.tap_speaking() {
            self.voice.open_mic_after_speech = false;
            if !self.voice.muted && !self.voice.always_on {
                crate::log::info("voice: the reply ended; starting open mic (headphone tap)");
                self.stop_voice();
                self.start_voice_in(3);
            }
        }
        if let Some((at, mode, tries)) = self.voice.reopen {
            if Instant::now() >= at && !self.voice.muted && !self.paused() {
                crate::log::info(&format!(
                    "voice: reopening the mic in {} mode",
                    mode_name(mode)
                ));
                self.stop_voice();
                self.start_voice_in(mode);

                self.voice.reopen = Some((Instant::now() + Duration::from_secs(3600), mode, tries));
            }
        }
    }

    pub fn voice_mode_now(&self) -> &'static str {
        if self.voice.engine.is_none() || self.voice.muted {
            "off"
        } else if self.voice.open_mic || self.voice.open_mic_asleep {
            "open"
        } else if self.voice.always_on {
            "wake"
        } else {
            "push"
        }
    }

    pub fn start_voice(&mut self, always_on: bool) {
        if self.voice.muted {
            return;
        }
        if self.voice.engine.is_some() {
            return;
        }
        if cfg!(test) {
            self.voice.always_on = always_on;
            return;
        }
        let source = match (&self.voice.file, self.web_mic()) {
            (Some(f), _) => Source::File {
                path: f.clone(),
                realtime: true,
            },

            (None, Some(m)) => Source::Web(m),
            (None, None) => Source::Mic(self.cfg.voice.device.clone()),
        };

        let always_on = always_on || self.voice.file.is_some();
        let mut vc = self.cfg.voice.clone();
        if self.voice.open_mic {
            vc.mode = "open".into();
        }

        if self.voice.open_mic || self.assistant_on() {
            vc.end_silence_ms = vc
                .end_silence_ms
                .min(self.cfg.assistant.endpoint_ms.max(250));
        }
        if self.assistant_on() {
            vc.tts_unload_after_s = 0;
        }
        let v = Voice::start(&vc, source, always_on, self.tx.clone());
        v.set_vocabulary(self.vocabulary());
        if self.brainstorm_on() {
            v.set_brainstorm(true);
        }
        self.voice.engine = Some(v);
        self.voice.started_at = Some(Instant::now());
        self.voice.always_on = always_on;
        self.voice.status = VoiceStatus::Starting;
        self.voice.updated = Instant::now();
    }

    pub fn stop_voice(&mut self) {
        if let Some(v) = self.voice.engine.take() {
            v.shutdown();
        }
        self.voice.status = VoiceStatus::Off(None);
    }

    pub fn set_muted(&mut self, on: bool) {
        if on == self.voice.muted {
            return;
        }
        if on {
            self.voice.muted_prev = self.voice_mode();
            self.voice.open_mic = false;

            if let Some(v) = &self.voice.engine {
                v.set_mic(false);
            }
            self.voice.wake_until = None;
            self.voice.listen_after_speech = false;
            self.voice.holding = None;
            self.voice.partial = None;
            self.voice.reopen = None;
            self.voice.muted = true;
            self.voice.action = Some("muted".into());
            crate::log::info("voice: muted");
            self.flash("Mic muted: nothing is heard or sent (click ● MUTED or Ctrl-a X to unmute)");
        } else if self.voice.engine.is_some() {
            self.resume_kept_mic();
            self.voice.action = Some("unmuted".into());
            self.flash("Mic on");
        } else {
            self.voice.muted = false;
            let prev = self.voice.muted_prev;
            crate::log::info("voice: unmuted");
            if prev > 0 {
                if prev == 3 {
                    self.start_open_mic();
                } else {
                    self.set_voice_mode(prev);
                }
            }
            self.voice.action = Some("unmuted".into());
            self.flash("Mic on");
        }
    }

    pub fn set_speaker_muted(&mut self, on: bool) {
        if on {
            if let Some(v) = &self.voice.engine {
                v.stop_speaking();
            }
            if let Some(p) = &self.voice.preview {
                p.stop();
            }
        }
        if self.cfg.voice.speaker_muted != on {
            let _ = crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global("voice.speaker_muted"),
                Some(toml_edit::value(on)),
            );
            self.config_mtime = crate::app::config_mtime();
            self.cfg.voice.speaker_muted = on;
        }
        crate::log::info(&format!("speaker muted: {on}"));
        self.flash(if on {
            "Speaker muted: answers show as text (Ctrl-a O talks again)"
        } else {
            "Speaker on"
        });
    }

    pub fn toggle_speaker(&mut self) {
        let on = !self.cfg.voice.speaker_muted;
        self.set_speaker_muted(on);
    }

    pub fn set_tts_volume(&mut self, v: f32) {
        let v = (v.clamp(0.0, 1.0) * 100.0).round() / 100.0;
        let _ = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("voice.tts_volume"),
            Some(toml_edit::value(v as f64)),
        );
        self.config_mtime = crate::app::config_mtime();
        self.cfg.voice.tts_volume = v;
        if let Some(e) = self.voice.engine.as_mut() {
            e.configure_tts(&self.cfg.voice);
        }
        self.voice.preview = None;
        self.flash(format!("Volume {}%", (v * 100.0).round() as u32));
    }

    pub fn nudge_volume(&mut self, by: f32) {
        let v = self.cfg.voice.tts_volume + by;
        self.set_tts_volume(v);
    }

    pub fn toggle_mute(&mut self) {
        let on = !self.voice.muted;
        self.set_muted(on);
    }

    pub fn push_to_talk(&mut self) {
        if self.voice.muted {
            self.flash("The mic is muted (Ctrl-a X unmutes)");
            return;
        }

        let speaking = self.voice.engine.as_ref().is_some_and(|v| v.speaking())
            || self.voice.preview.as_ref().is_some_and(|p| p.speaking());
        if speaking || self.assistant.busy {
            self.assistant_barge_in(0, 0);
        }
        if self.voice.engine.is_none() {
            let wake = self.cfg.voice.mode == "wake";
            self.start_voice(wake);
        }
        if let Some(v) = &self.voice.engine {
            if self.voice.status == VoiceStatus::Listening {
                v.stop_talk();
            } else {
                v.push_to_talk();
                self.voice.status = VoiceStatus::Listening;
                if self.voice.hold_supported {
                    self.voice.holding = Some(Instant::now());
                    self.quiet_user_spoke();
                }
            }
        }
        self.voice.updated = Instant::now();
    }

    pub fn bare_wake(&mut self, reply: &str, ptt: bool, cut_in: bool) {
        let secs = self.cfg.voice.wake_follow_up_s.clamp(3, 30);
        crate::log::info(&format!(
            "wake: bare{}, opening follow-up {secs} s",
            if ptt { " (push to talk)" } else { "" }
        ));
        if cut_in {
            if let Some(v) = &self.voice.engine {
                v.stop_speaking();
                v.chime();
            }
            if let Some(p) = &self.voice.preview {
                p.stop();
            }
        }
        self.voice.wake_until = Some(Instant::now() + Duration::from_secs(secs));
        self.voice.action = Some("listening…".into());
        self.voice.asleep = false;
        self.voice.listen_after_speech = ptt || !self.voice.always_on;
        self.voice.said_wake.push(reply.to_string());
        self.speak(reply);
    }

    pub fn listen_after_reply(&mut self, speaking: bool) {
        if !self.voice.listen_after_speech || speaking {
            return;
        }
        self.voice.listen_after_speech = false;
        if self.voice.muted || self.voice.wake_until.is_none_or(|t| Instant::now() >= t) {
            return;
        }
        if let Some(v) = &self.voice.engine {
            if self.voice.status != VoiceStatus::Listening {
                crate::log::info("wake: listening for the command (push to talk follow-up)");
                v.push_to_talk();
                self.voice.status = VoiceStatus::Listening;
            }
        }
    }

    pub fn hold_to_talk_key(&mut self, k: &crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyEventKind};
        let Some(since) = self.voice.holding else {
            return false;
        };
        if k.code != KeyCode::Char(' ') {
            if k.kind == KeyEventKind::Press {
                self.voice.holding = None;
            }
            return false;
        }
        match k.kind {
            KeyEventKind::Repeat | KeyEventKind::Press => true,
            KeyEventKind::Release => {
                self.voice.holding = None;
                if since.elapsed() >= Duration::from_millis(350) {
                    if let Some(v) = &self.voice.engine {
                        v.stop_talk();
                    }
                    self.voice.action = Some("released: transcribing".into());
                }
                true
            }
        }
    }

    pub fn toggle_wake_mode(&mut self) {
        let next = !self.voice.always_on || self.voice.engine.is_none();
        self.stop_voice();
        self.start_voice(next);
        let ww = self
            .cfg
            .voice
            .wake_words
            .first()
            .cloned()
            .unwrap_or_default();
        self.flash(if self.voice.always_on {
            format!("Voice: always listening for \"{ww}\"")
        } else {
            "Voice: push to talk (Ctrl-a space)".to_string()
        });
    }

    pub fn on_voice(&mut self, ev: VoiceEvent) {
        self.voice.updated = Instant::now();
        match ev {
            VoiceEvent::Status(s) => {
                if s == VoiceStatus::Listening {
                    self.prewarm_assistant();
                }
                if let VoiceStatus::Off(Some(reason)) = &s {
                    self.voice.info = Some(reason.clone());
                    crate::log::info(&format!("voice: {reason}"));

                    let settled = self
                        .voice
                        .started_at
                        .is_some_and(|t| t.elapsed() > Duration::from_secs(5));
                    if reason == "microphone stream ended"
                        && settled
                        && self.voice.engine.is_some()
                        && !self.voice.muted
                        && self.voice.last_on_mode > 0
                    {
                        let tries = self.voice.reopen.map_or(0, |r| r.2);
                        if tries < 5 {
                            let m = self.voice.last_on_mode;
                            self.voice.reopen =
                                Some((Instant::now() + Duration::from_secs(2), m, tries + 1));
                            crate::log::info(&format!(
                                "voice: reopening the mic in 2 s in {} mode (try {})",
                                mode_name(m),
                                tries + 1
                            ));
                        }
                    }
                }
                if s == VoiceStatus::Listening || s == VoiceStatus::Idle {
                    if self.voice.reopen.is_some_and(|r| Instant::now() > r.0) {
                        self.voice.reopen = None;
                    }
                }
                self.voice.status = s;
            }

            VoiceEvent::BargeIn { .. }
                if self.paused() || self.voice.muted || self.brainstorm_on() => {}
            VoiceEvent::BargeIn { speech_ms, stop_ms } => {
                self.quiet_user_spoke();
                self.assistant_barge_in(speech_ms, stop_ms)
            }
            VoiceEvent::Speaker(e) => self.on_speaker(e),
            VoiceEvent::Info(i) => {
                crate::log::info(&format!("voice: {i}"));
                self.voice.info = Some(i);
            }
            VoiceEvent::Partial(text, ms) => {
                if self.voice.status == VoiceStatus::Listening {
                    self.voice.partial = Some(text.trim().to_string());
                    self.voice.partial_ms = Some(ms);
                    self.quiet_user_spoke();
                    self.voice.first_partial.get_or_insert_with(Instant::now);
                }
            }
            VoiceEvent::Heard { text, stats, .. } if self.train_heard(&text, stats) => {
                self.voice.partial = None;
                self.voice.heard = Some(text);
            }
            VoiceEvent::Heard { text, ptt, stats } => {
                self.voice.partial = None;
                self.voice.first_partial = None;
                crate::log::info(&format!(
                    "voice: final: {} ms of speech, STT {} ms after the end of speech (last partial took {} ms)",
                    stats.ms,
                    stats.stt_ms,
                    self.voice.partial_ms.unwrap_or(0)
                ));
                self.voice.cur_stats = stats;
                self.assistant.heard_at = Some(Instant::now());
                self.on_heard(&text, ptt);
                let entry = LogEntry {
                    at: chrono::Local::now(),
                    heard: text.trim().to_string(),
                    action: self.voice.action.clone().unwrap_or_default(),
                    partial_ms: self.voice.partial_ms.take(),
                };
                if self.voice.log.len() == LOG_LEN {
                    self.voice.log.pop_front();
                }
                self.voice.log.push_back(entry);
            }
        }
        if !matches!(self.voice.status, VoiceStatus::Listening)
            && self.voice.status != VoiceStatus::Transcribing
        {
            self.voice.partial = None;
        }
    }

    pub fn strip_wake<'a>(&self, norm: &'a str) -> Option<&'a str> {
        crate::voice::wake_profile::strip_wake_with(
            self.voice.profile.as_ref(),
            norm,
            &self.cfg.voice.wake_words,
            self.cfg.voice.wake_sensitivity,
        )
    }

    pub fn preview_voice(&mut self) {
        let vc = &self.cfg.voice;
        let (engine, why) = crate::voice::tts::effective_engine(vc);
        let who = match engine {
            "kokoro" => crate::voice::catalog::kokoro_name(&vc.kokoro_voice),
            "chatterbox" | "ming" => format!(
                "{}, {}",
                crate::voice::local_tts::info(engine)
                    .map(|i| i.name)
                    .unwrap_or(engine),
                crate::voice::local_tts::voice_label(engine, vc)
            ),
            "grok" => format!("Grok {}", vc.grok.voice),
            _ => "the say voice".into(),
        };
        let text = format!(
            "Hi, this is {}. Approved, account two is next.",
            who.replace('_', " ")
        );
        self.told_record("voice preview", &text, crate::told::Channel::Spoken, true);
        match &self.voice.engine {
            Some(v) if v.tts => v.say(&text),
            _ => {
                if self.voice.preview.is_none() && !cfg!(test) {
                    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    self.voice.preview = Some(crate::voice::tts::Speaker::start(vc, flag));
                }
                if let Some(p) = &self.voice.preview {
                    p.say(&text);
                }
            }
        }
        self.flash(match why {
            Some(w) if vc.tts_engine == "kokoro" => {
                format!("Preview with {engine}: kokoro unavailable ({w})")
            }
            Some(w) => format!("Preview with {engine}: {w}"),
            None => format!("Preview: {who} ({engine})"),
        });
    }

    pub fn meter_live(&self) -> bool {
        self.voice_hud_visible()
            && self
                .voice
                .engine
                .as_ref()
                .is_some_and(|v| v.levels.open.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub fn voice_hud_rows(&self) -> u16 {
        if !self.voice_hud_visible() {
            0
        } else if self.voice.show_log {
            1 + self.voice.log.len().max(1) as u16
        } else {
            1
        }
    }

    fn on_heard(&mut self, text: &str, ptt: bool) {
        crate::log::info(&format!("voice heard: {text}"));
        self.voice.heard = Some(text.to_string());
        let norm = grammar::normalize(text);
        self.voice.last_heard = Instant::now();
        self.quiet_user_spoke();

        if self.voice.muted {
            crate::log::info(&format!("muted: ignored: {text}"));
            self.voice.action = Some("(muted, ignored: Ctrl-a X unmutes)".into());
            return;
        }

        if self.paused() {
            let Some(rest) = self.while_paused(&norm, ptt) else {
                return;
            };
            if rest.trim().is_empty() {
                crate::log::info("paused: wake word, resuming");
                return self.bare_wake("I'm back.", ptt, false);
            }
            crate::log::info("paused: wake word with a command, resuming and running it");
            return self.run_heard(rest);
        }

        if self.onboarding_heard(text, &norm) {
            return;
        }

        if self.brainstorm_on() {
            let woke = self.strip_wake(&norm).map(str::to_string);
            return self.brainstorm_heard(text, &norm, woke);
        }

        let table = crate::instant::table(&self.cfg.voice.instant);
        let follow_up = self.voice.wake_until.is_some_and(|t| Instant::now() < t);
        let stats = self.voice.cur_stats;
        let woke = self.strip_wake(&norm).map(str::to_string);

        let addressed = ptt
            || woke.is_some()
            || self.voice.open_mic
            || follow_up
            || !self.voice.always_on
            || self.direct.on.is_some();
        if addressed {
            let command = woke.clone().unwrap_or_else(|| norm.clone());
            if !command.trim().is_empty() && self.brainstorm_intercept(text, &command) {
                crate::log::info(&format!("voice: brainstorm phrase: {text}"));
                return;
            }

            if !command.trim().is_empty() && self.updates_intercept(&command) {
                return;
            }

            if !command.trim().is_empty() && self.voice_choice_intercept(&command) {
                return;
            }
        }

        if self.direct.on.is_some() {
            return self.direct_heard(text, &norm, woke, ptt, stats, &table);
        }

        if woke.as_deref().is_some_and(|c| c.trim().is_empty()) {
            return self.bare_wake("Yes?", ptt, true);
        }
        let why = if ptt {
            crate::app_openmic::is_noise(&norm).then_some("noise")
        } else if woke.is_none() && (self.voice.open_mic || follow_up) {
            crate::app_openmic::hands_free_filter(&norm, stats, &table)
        } else {
            crate::app_openmic::confidence_gate(stats)
        };

        let replying = follow_up || self.pending_confirms.iter().any(|p| p.answerable());
        let why = why.filter(|w| {
            !(replying
                && *w == "too short"
                && (crate::instant::affirmative(text)
                    || crate::instant::negative(text)
                    || norm.split_whitespace().count() >= 2))
        });
        if let Some(why) = why {
            self.voice.ignored += 1;
            crate::log::info(&format!("voice: ignored ({why}): {text}"));
            self.voice.action = Some(format!("({why}, ignored)"));
            return;
        }
        if self.voice.open_mic && !ptt {
            let command = woke.unwrap_or(norm.clone());
            return self.run_heard(command);
        }
        let had_wake = woke.is_some();
        let command = if ptt || follow_up || !self.voice.always_on {
            self.voice.wake_until = None;
            Some(woke.unwrap_or(norm.clone()))
        } else {
            woke
        };
        let Some(command) = command else {
            self.voice.ignored += 1;
            crate::log::info(&format!("wake: no wake word, ignored: {text}"));
            self.voice.action = Some("(no wake word, ignored)".into());
            return;
        };
        crate::log::info(&format!(
            "wake: {}: {command}",
            if ptt {
                "push to talk"
            } else if follow_up {
                "follow-up window"
            } else if had_wake {
                "wake word with a command"
            } else {
                "wake word not needed"
            }
        ));
        if !ptt && !follow_up {
            if let Some(v) = &self.voice.engine {
                v.chime();
            }
        }
        if had_wake && self.voice.open_mic_asleep {
            self.voice.wake_to_open_mic = true;
        }
        if command.trim().is_empty() {
            self.voice.wake_until = Some(Instant::now() + Duration::from_secs(6));
            self.voice.action = Some("listening for a command".into());
            return;
        }
        self.run_heard(command);
    }

    pub(crate) fn run_heard(&mut self, command: String) {
        let raw = self.voice.heard.clone().unwrap_or_default();
        let inst = if self.cfg.voice.instant_commands {
            crate::instant::match_instant(&command, &crate::instant::table(&self.cfg.voice.instant))
        } else {
            None
        };
        if self.voice.asleep && inst != Some(crate::instant::Instant::Wake) {
            self.voice.action = Some("asleep: say \"wake up\" or press Ctrl-a space".into());
            return;
        }
        if let Some(i) = inst {
            if let Some(msg) = self.run_instant(i) {
                crate::log::info(&format!("voice instant: {i:?}: {msg}"));
                self.voice.action = Some(msg);
                return;
            }
        }
        if !self.assistant_on() {
            self.voice.action = Some(
                "the assistant is off: only the instant commands work (Settings > Assistant)"
                    .into(),
            );
            return;
        }
        if let Some(v) = &self.voice.engine {
            v.stop_speaking();
        }

        self.brainstorm_suggest_check(&raw);
        self.ask_assistant_from(&command, Some(&raw));
    }

    pub fn run_instant(&mut self, i: crate::instant::Instant) -> Option<String> {
        use crate::instant::Instant as I;
        match i {
            I::Stop => {
                if let Some(v) = &self.voice.engine {
                    v.stop_speaking();
                }
                if let Some(p) = &self.voice.preview {
                    p.stop();
                }
                self.assistant.muted = true;
                Some("stopped talking".into())
            }
            I::Mute => {
                self.set_muted(true);
                Some("muted".into())
            }
            I::Pause => {
                let secs = self.pause_listening(None, None);
                let ww = self
                    .cfg
                    .voice
                    .wake_words
                    .first()
                    .cloned()
                    .unwrap_or_default();
                self.speak(&format!(
                    "Okay, I'll wait {}. Say {ww} if you need me sooner.",
                    crate::app_pause::spoken_duration(secs)
                ));
                Some(format!(
                    "paused for {}",
                    crate::app_pause::spoken_duration(secs)
                ))
            }
            I::Resume if self.paused() => {
                self.resume_listening("resume");
                Some("listening again".into())
            }

            I::Resume => None,
            I::Sleep if self.voice.open_mic => {
                self.leave_open_mic(true);
                Some("open mic off: listening for the wake word".into())
            }
            I::Sleep => {
                self.voice.asleep = true;
                self.speak("going quiet");
                Some("asleep until \"wake up\"".into())
            }
            I::Wake => {
                self.voice.asleep = false;
                self.speak("listening");
                Some("awake".into())
            }
            I::NextTab | I::PrevTab => {
                let s = &mut self.panes[self.focus];
                if i == I::NextTab {
                    s.next_tab()
                } else {
                    s.prev_tab()
                }
                self.view = View::Grid;
                let here = self.describe(self.focus, self.panes[self.focus].active);
                Some(format!("now on {here}"))
            }
            I::Approve | I::Deny => {
                let yes = i == I::Approve;

                let asked = self.pending_confirms.iter().any(|p| p.answerable())
                    || self
                        .assistant
                        .log
                        .iter()
                        .rev()
                        .find(|e| e.who == crate::app_assistant::Who::Reply)
                        .is_some_and(|e| {
                            e.text.trim_end().ends_with('?')
                                && self.assistant.asked_at.is_some_and(|t| {
                                    crate::clock::age(t) < Duration::from_secs(120)
                                })
                        });
                if asked && self.assistant_on() {
                    return None;
                }

                match self.modal {
                    Modal::ConfirmStopLoops => {
                        self.modal = Modal::None;
                        if yes {
                            self.stop_all_loops();
                        }
                        return Some(if yes {
                            "stopping every loop".into()
                        } else {
                            "kept the loops".into()
                        });
                    }
                    Modal::SessConfirm => {
                        if yes {
                            self.confirm_op()
                        } else {
                            self.cancel_op()
                        }
                        return Some(if yes {
                            "done".into()
                        } else {
                            "cancelled".into()
                        });
                    }
                    Modal::ConfirmBypass(pp, aa) => {
                        self.modal = Modal::None;
                        if yes {
                            self.set_permission_mode(pp, aa, "bypass", true);
                        }
                        return Some(if yes {
                            "bypass permissions on for new tabs".into()
                        } else {
                            "kept the permission mode".into()
                        });
                    }
                    Modal::OfferRestart(pp) => {
                        self.modal = Modal::None;
                        if yes {
                            let kind = self.panes[pp].cur().relaunch_kind();
                            self.launch(pp, kind);
                        }
                        return Some(if yes {
                            "restarted the tab".into()
                        } else {
                            "the new mode applies to new tabs".into()
                        });
                    }
                    Modal::ConfirmClose => {
                        self.modal = Modal::None;
                        if yes {
                            self.close_tab_now();
                        }
                        return Some(if yes {
                            "closed the tab".into()
                        } else {
                            "kept the tab".into()
                        });
                    }
                    _ => {}
                }

                let (fs, ft) = (self.focus, self.panes[self.focus].active);
                let target = if self.panes[fs].tabs[ft].activity == Activity::Permission {
                    Some((fs, ft))
                } else {
                    let w = self.waiting_tabs();
                    (w.len() == 1).then(|| w[0])
                };
                let (s, t) = target?;
                let msg = self.answer_tab(s, t, if yes { Choice::Approve } else { Choice::Deny });
                self.speak(if yes { "approved" } else { "denied" });
                Some(msg)
            }
        }
    }

    fn describe(&self, slot: usize, tab: usize) -> String {
        self.spoken_tab_name(self.panes[slot].tabs[tab].uid)
    }

    pub fn waiting_tabs(&self) -> Vec<(usize, usize)> {
        let mut v: Vec<(usize, usize, Instant)> = self
            .panes
            .iter()
            .enumerate()
            .flat_map(|(si, s)| s.tabs.iter().enumerate().map(move |(ti, t)| (si, ti, t)))
            .filter(|(_, _, t)| t.activity == Activity::Permission)
            .map(|(si, ti, t)| (si, ti, t.activity_since))
            .collect();
        v.sort_by_key(|x| x.2);
        v.into_iter().map(|(s, t, _)| (s, t)).collect()
    }

    pub fn request_of(&self, slot: usize, tab: usize) -> Option<crate::prompt::Request> {
        let t = &self.panes[slot].tabs[tab];
        let screen = t
            .parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .contents();
        let h = self
            .pane_harness(slot)
            .unwrap_or(crate::harness::Harness::Claude);
        crate::prompt_cli::describe_request_for(h, &screen)
    }

    pub fn tab_answerable(&self, slot: usize, tab: usize, choice: Choice) -> Result<(), String> {
        let who = self.describe(slot, tab);
        if self.panes[slot].tabs[tab].activity != Activity::Permission {
            return Err(format!("{who} is not waiting for approval"));
        }
        if let Some(h) = self.pane_harness(slot).filter(|h| !h.integrated()) {
            let screen = self.panes[slot].tabs[tab]
                .parser
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .screen()
                .contents();
            if crate::prompt_cli::parse_prompt_for(h, &screen)
                .and_then(|p| p.index_for(choice))
                .is_none()
            {
                return Err(format!(
                    "{who} runs {}: I cannot read its approval prompt, answer it with the keyboard",
                    h.label()
                ));
            }
        }
        Ok(())
    }

    pub fn answer_tab(&mut self, slot: usize, tab: usize, choice: Choice) -> String {
        let who = self.describe(slot, tab);
        if let Err(e) = self.tab_answerable(slot, tab, choice) {
            return e;
        }
        let (key, picked) = self.answer_keys(slot, tab, choice);
        self.send_keys(slot, tab, &key);
        self.attention.retain(|a| !(a.slot == slot && a.tab == tab));
        let verb = match choice {
            Choice::Approve => "approved",
            Choice::Always => "approved always",
            Choice::Deny => "denied",
        };
        crate::log::info(&format!("{verb} {who}"));
        match picked {
            Some(l) => format!("{verb} {who} ({l})"),
            None => format!("{verb} {who}"),
        }
    }

    pub fn answer_keys(
        &self,
        slot: usize,
        tab: usize,
        choice: Choice,
    ) -> (Vec<u8>, Option<String>) {
        let t = &self.panes[slot].tabs[tab];
        let screen = t
            .parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .contents();
        let h = self
            .pane_harness(slot)
            .unwrap_or(crate::harness::Harness::Claude);
        if let Some(p) = crate::prompt_cli::parse_prompt_for(h, &screen) {
            if let Some(i) = p.index_for(choice) {
                return (
                    p.keys_for(i, t.app_cursor()),
                    Some(p.options[i].label.clone()),
                );
            }
        }
        let fallback: &[u8] = match choice {
            Choice::Approve => b"1",
            Choice::Always => b"2",
            Choice::Deny => b"\x1b",
        };
        (fallback.to_vec(), None)
    }

    fn send_keys(&mut self, slot: usize, tab: usize, bytes: &[u8]) {
        let t = &mut self.panes[slot].tabs[tab];
        t.reset_scroll();
        t.write(bytes);
    }

    pub(crate) fn send_text(&mut self, slot: usize, tab: usize, text: &str) {
        self.type_text(slot, tab, text, true);
    }

    fn type_text(&mut self, slot: usize, tab: usize, text: &str, enter: bool) {
        if enter {
            self.paste_then_enter(slot, tab, text);
            return;
        }
        let t = &mut self.panes[slot].tabs[tab];
        t.reset_scroll();
        t.write(&keys::encode_paste(text, t.bracketed_paste()));
    }

    pub(crate) fn speak(&self, text: &str) {
        self.speak_as(Kind::Confirm, text);
    }

    pub fn speak_as(&self, kind: Kind, text: &str) {
        let vc = &self.cfg.voice;
        if vc.speaker_muted {
            return;
        }
        let on = match kind {
            Kind::Confirm => vc.speak_confirm,
            Kind::Announce => vc.announce,
        };
        if !on {
            return;
        }

        if kind == Kind::Announce && self.brainstorm_on() {
            return;
        }

        if self.speaker_on() {
            let what = match kind {
                Kind::Confirm => "confirmation",
                Kind::Announce => "announcement",
            };
            self.told_record(what, text, crate::told::Channel::Spoken, false);
        }
        if let Some(v) = &self.voice.engine {
            v.say(text);
        }
    }

    pub fn best_account(&self) -> Option<(usize, f64)> {
        self.accounts
            .iter()
            .enumerate()
            .filter(|(_, st)| st.login.logged_in())
            .filter_map(|(i, st)| st.effective_left().map(|l| (i, l)))
            .filter(|(_, l)| *l >= 2.0)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    pub fn low_quota_hint(&self) -> Option<String> {
        let a = self.panes[self.focus].account?;
        let left = self.accounts[a].effective_left()?;
        if left >= 10.0 {
            return None;
        }
        let (b, bl) = self.best_account()?;
        if b == a || bl <= left + 10.0 {
            return None;
        }
        Some(format!(
            "{} has {left:.0}% left this window; {} has {bl:.0}%: say \"switch to the best account\"",
            self.cfg.accounts[a].display(),
            self.cfg.accounts[b].display()
        ))
    }

    pub fn voice_tick(&mut self) {
        crate::clock::observe();

        let speaking = self.voice.engine.as_ref().is_some_and(|v| v.speaking());
        if self.voice.was_speaking
            && !speaking
            && self.cfg.voice.conversation
            && self.voice.always_on
        {
            self.voice.wake_until = Some(Instant::now() + Duration::from_secs(6));
            self.voice.action = Some("listening for your reply".into());
        }
        self.voice.was_speaking = speaking;
        self.pause_tick();
        self.brainstorm_tick();
        self.open_mic_tick();
        self.refresh_vocabulary();
        let any_speaking = speaking || self.voice.preview.as_ref().is_some_and(|p| p.speaking());
        self.assistant_follow_up(any_speaking);
        self.listen_after_reply(any_speaking);
        self.quiet_tick();

        let items = std::mem::take(&mut self.attention);
        for a in items {
            let Some((slot, tab)) = self.find_tab(a.uid) else {
                continue;
            };
            let now = self.panes[slot].tabs[tab].activity;
            let need = a.what == Activity::Permission;
            if need && now != Activity::Permission {
                continue;
            }
            let tid = crate::control::tab_id(a.uid);
            crate::log::info(&format!(
                "attention: {tid} {} to {} (now {}): an event for the assistant",
                a.from.label(),
                a.what.label(),
                now.label()
            ));

            if self.direct_with(a.uid) {
                continue;
            }
            if need {
                self.tab_approval_event(a.uid);

                let key = (slot, tab, true);
                if self.cfg.notifications
                    && self.voice.term_focused != Some(true)
                    && self.voice.notified.allow(key)
                {
                    let who = self.describe(slot, tab);
                    self.notify_user("godterm", &format!("{who} needs approval"));
                }
            } else {
                self.tab_finished_event(a.uid);
            }
        }
        self.updates_tick();
        self.events_tick();
    }

    pub fn voice_hud_visible(&self) -> bool {
        self.voice.engine.is_some()
            || self.voice.info.is_some()
            || self.voice.muted
            || crate::demo::active()
    }
}

#[cfg(test)]
mod voice_resume_tests {
    use crate::app::App;
    use crate::config::testing::LOCK as ENV_LOCK;

    fn app(tag: &str) -> (App, std::path::PathBuf) {
        let home =
            std::env::temp_dir().join(format!("godterm-voiceresume-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        crate::config::testing::set_home(&home);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut cfg = crate::config::Config::default();
        cfg.voice.enabled = true;
        cfg.voice.mode = "push".into();
        (App::new(cfg, tx), home)
    }

    #[test]
    fn an_open_mic_comes_back_after_a_restart() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = app("open");

        assert_eq!(app.startup_voice(true).unwrap(), "push");

        app.voice_at_quit = Some("open".into());
        app.save_state();
        let st = crate::state::AppState::load().unwrap();
        assert_eq!(st.voice.as_deref(), Some("open"));
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut cfg = crate::config::Config::default();
        cfg.voice.enabled = true;
        cfg.voice.mode = "push".into();
        let mut next = App::new(cfg, tx);

        next.restore(crate::state::AppState::load().unwrap());
        assert_eq!(next.startup_voice(true).unwrap(), "open");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn wake_comes_back_and_off_falls_back_to_the_setting() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = app("modes");
        app.rt_voice = Some("wake".into());
        assert_eq!(app.startup_voice(true).unwrap(), "wake");
        app.rt_voice = Some("off".into());
        assert_eq!(app.startup_voice(true).unwrap(), "push");
        app.rt_voice = Some("nonsense".into());
        assert_eq!(app.startup_voice(true).unwrap(), "push");

        assert_eq!(app.voice_mode_now(), "off");
        app.state_dirty = false;
        app.rt_voice = Some("open".into());
        app.tick();
        assert_eq!(app.rt_voice.as_deref(), Some("off"));
        assert!(app.state_dirty);
        let _ = std::fs::remove_dir_all(&home);
    }
}

fn mode_name(m: u8) -> &'static str {
    match m {
        3 => "open mic",
        2 => "wake word",
        1 => "push to talk",
        _ => "off",
    }
}
