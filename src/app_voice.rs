//! Voice commands: wake word handling, targeting and execution.

use std::time::{Duration, Instant};

use crate::app::{App, Modal, View};
use crate::keys;
use crate::pane::Activity;
use crate::prompt::{parse_prompt, Choice};
use crate::voice::audio::Source;
use crate::voice::grammar;
use crate::voice::tts::Kind;
use crate::voice::{Voice, VoiceEvent, VoiceStatus};

/// Voice related app state (HUD contents and timers).
pub struct VoiceState {
    pub engine: Option<Voice>,
    pub status: VoiceStatus,
    pub always_on: bool,
    pub asleep: bool,
    /// Debug: read audio from this file instead of the mic.
    pub file: Option<String>,
    pub heard: Option<String>,
    pub action: Option<String>,
    pub info: Option<String>,
    /// Dictation held until "send".
    pub pending: Option<String>,
    pub updated: Instant,
    /// The terminal reports key release (kitty keyboard protocol), so
    /// Ctrl-a space can be held.
    pub hold_supported: bool,
    /// Space is being held for push to talk, since this instant.
    pub holding: Option<Instant>,
    /// After a bare wake word, the next utterance needs no wake word.
    pub(crate) wake_until: Option<Instant>,
    /// Push to talk after a bare wake word: listen again once the reply
    /// has been spoken.
    pub listen_after_speech: bool,
    /// What a bare wake word answered (tests and the HUD).
    pub said_wake: Vec<String>,
    spoken: crate::notify::Limiter<(usize, usize, bool)>,
    notified: crate::notify::Limiter<(usize, usize, bool)>,
    /// Terminal focus as reported by focus events (None: never reported).
    pub term_focused: Option<bool>,
    /// Open mic: every utterance is a command, no wake word.
    pub open_mic: bool,
    /// Last utterance (open mic auto sleep).
    pub last_heard: Instant,
    /// Stats of the utterance being handled (open mic guards).
    pub cur_stats: crate::voice::wake_profile::UttStats,
    /// The vocabulary last given to the engine.
    pub vocab_sent: Option<String>,
    /// Partial transcript of speech still going on.
    pub partial: Option<String>,
    /// Latency of the last partial request (ms).
    pub partial_ms: Option<u128>,
    /// When the current utterance's first partial arrived.
    first_partial: Option<Instant>,
    /// We were talking back at the last tick (conversation mode).
    was_speaking: bool,
    /// Wake word training in progress.
    pub training: Option<crate::app_wake::Training>,
    /// Learned wake aliases and negatives.
    pub profile: Option<crate::voice::wake_profile::WakeProfile>,
    /// Talk back for Settings > Preview while the voice engine is off.
    pub preview: Option<crate::voice::tts::Speaker>,
    /// The last few utterances, newest last.
    pub log: std::collections::VecDeque<LogEntry>,
    /// Show the voice log above the strip.
    pub show_log: bool,
    /// Utterances dropped as noise, doubtful or not addressed to us.
    pub ignored: u32,
    /// The mic is muted (capture stopped); the voice mode to go back to.
    pub muted: bool,
    pub muted_prev: u8,
    /// "Hold on a minute": listening paused until then.
    pub paused: Option<crate::app_pause::Pause>,
    /// What was said on the last resume about held announcements.
    pub away_summary: Option<String>,
    /// Speaker lock: utterances dropped as not the user's voice (or far
    /// away), the last score, and the saved profile's summary.
    pub not_you: u32,
    pub speaker_last: Option<crate::voice::speaker::Check>,
    pub speaker_profile: Option<String>,
}

/// One utterance in the voice log.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub at: chrono::DateTime<chrono::Local>,
    pub heard: String,
    pub action: String,
    /// Partial latency in ms when partials were shown.
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
            spoken: crate::notify::Limiter::new(Duration::from_secs(60), Duration::from_secs(5)),
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
    /// Words whisper should expect (its prompt): commands, account names,
    /// tab names and recent folders. Biasing toward these is the biggest
    /// accuracy win for short commands.
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
        // Recent project folders and session titles, so a name like
        // "botmesh" is transcribed as said; "trading-room" also as
        // "trading room", the way it is spoken.
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
        // whisper's prompt is short: keep it under about 220 tokens.
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

    /// Keep the engine's vocabulary current as tabs change.
    pub fn refresh_vocabulary(&mut self) {
        let v = self.vocabulary();
        if self.voice.vocab_sent.as_deref() != Some(v.as_str()) {
            if let Some(e) = &self.voice.engine {
                e.set_vocabulary(v.clone());
                self.voice.vocab_sent = Some(v);
            }
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
            // Never open the microphone from unit tests.
            self.voice.always_on = always_on;
            return;
        }
        let source = match &self.voice.file {
            Some(f) => Source::File {
                path: f.clone(),
                realtime: true,
            },
            None => Source::Mic(self.cfg.voice.device.clone()),
        };
        // A file is a recording of hands free use, so listen for the wake word.
        let always_on = always_on || self.voice.file.is_some();
        let mut vc = self.cfg.voice.clone();
        if self.voice.open_mic {
            vc.mode = "open".into(); // no wake word screening in the engine
        }
        // Answer sooner: a shorter end of speech with the assistant or open
        // mic, and Kokoro stays loaded while the assistant is on.
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
        self.voice.engine = Some(v);
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

    /// Ctrl-a space: start the engine if needed and capture one utterance;
    /// pressed again while listening, it ends the capture.
    /// Mute: capture stops entirely (ffmpeg ends, the mic indicator goes
    /// off) and nothing is heard or sent; talk back still works. Unmute
    /// goes back to the voice mode from before.
    pub fn set_muted(&mut self, on: bool) {
        if on == self.voice.muted {
            return;
        }
        if on {
            self.voice.muted_prev = self.voice_mode();
            self.voice.open_mic = false;
            self.stop_voice();
            self.voice.muted = true;
            self.voice.action = Some("muted".into());
            crate::log::info("voice: muted");
            self.flash("Mic muted: nothing is heard or sent (click ● MUTED or Ctrl-a X to unmute)");
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

    pub fn toggle_mute(&mut self) {
        let on = !self.voice.muted;
        self.set_muted(on);
    }

    pub fn push_to_talk(&mut self) {
        if self.voice.muted {
            self.flash("The mic is muted (Ctrl-a X unmutes)");
            return;
        }
        // Talking over a reply cuts it off.
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
                }
            }
        }
        self.voice.updated = Instant::now();
    }

    /// The wake word alone ("hey god"): always an answer. A chime and a
    /// short "Yes?" (or `reply`), "listening" on the HUD, and the next
    /// utterance within the follow up window is the command. In push to
    /// talk the mic opens again once the reply is spoken.
    pub fn bare_wake(&mut self, reply: &str, ptt: bool, cut_in: bool) {
        let secs = self.cfg.voice.wake_follow_up_s.clamp(3, 30);
        crate::log::info(&format!(
            "wake: bare{}, opening follow-up {secs} s",
            if ptt { " (push to talk)" } else { "" }
        ));
        if cut_in {
            if let Some(v) = &self.voice.engine {
                // It cuts in over whatever was being said.
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

    /// After a bare wake word in push to talk: once "Yes?" is spoken,
    /// listen for the command without the key.
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

    /// Hold to talk: while space is held after Ctrl-a space, swallow its
    /// repeats; on release after a real hold, end the capture. A quick tap
    /// keeps listening until the pause detector ends it. Returns true when
    /// the key was consumed.
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

    /// Ctrl-a v: switch between push to talk and always listening.
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
                // Speech started: warm the assistant up while they talk.
                if s == VoiceStatus::Listening {
                    self.prewarm_assistant();
                }
                if let VoiceStatus::Off(Some(reason)) = &s {
                    self.voice.info = Some(reason.clone());
                    crate::log::info(&format!("voice: {reason}"));
                }
                self.voice.status = s;
            }
            // No barge-in while paused.
            VoiceEvent::BargeIn { .. } if self.paused() => {}
            VoiceEvent::BargeIn { speech_ms, stop_ms } => {
                self.assistant_barge_in(speech_ms, stop_ms)
            }
            VoiceEvent::Speaker(e) => self.on_speaker(e),
            VoiceEvent::Info(i) => {
                crate::log::info(&format!("voice: {i}"));
                self.voice.info = Some(i);
            }
            VoiceEvent::Partial(text, ms) => {
                // A partial that lands after the final is stale.
                if self.voice.status == VoiceStatus::Listening {
                    self.voice.partial = Some(text.trim().to_string());
                    self.voice.partial_ms = Some(ms);
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

    /// The command after the wake word (learned aliases included).
    pub fn strip_wake<'a>(&self, norm: &'a str) -> Option<&'a str> {
        crate::voice::wake_profile::strip_wake_with(
            self.voice.profile.as_ref(),
            norm,
            &self.cfg.voice.wake_words,
            self.cfg.voice.wake_sensitivity,
        )
    }

    /// Settings > Voice and Audio > Preview: a sample in the chosen voice.
    pub fn preview_voice(&mut self) {
        let vc = &self.cfg.voice;
        let (engine, why) = crate::voice::tts::effective_engine(vc);
        let who = if engine == "kokoro" {
            vc.kokoro_voice.clone()
        } else {
            "the say voice".into()
        };
        let text = format!(
            "Hi, this is {}. Approved, account two is next.",
            who.replace('_', " ")
        );
        match &self.voice.engine {
            Some(v) if v.tts => v.say(&text),
            _ => {
                if self.voice.preview.is_none() {
                    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    self.voice.preview = Some(crate::voice::tts::Speaker::start(vc, flag));
                }
                if let Some(p) = &self.voice.preview {
                    p.say(&text);
                }
            }
        }
        self.flash(match why {
            Some(w) => format!("Preview with say: kokoro unavailable ({w})"),
            None => format!("Preview: {who} ({engine})"),
        });
    }

    /// The level meter needs frames at ~25 fps.
    pub fn meter_live(&self) -> bool {
        self.voice_hud_visible()
            && self
                .voice
                .engine
                .as_ref()
                .is_some_and(|v| v.levels.open.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Rows the voice strip takes: one, plus the log when shown.
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
        // Muted: the mic is off; a transcript still in flight is dropped.
        if self.voice.muted {
            crate::log::info(&format!("muted: ignored: {text}"));
            self.voice.action = Some("(muted, ignored: Ctrl-a X unmutes)".into());
            return;
        }
        // Paused: only the wake word (or "resume") counts; it resumes and
        // what follows it runs as usual.
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
        // Without a wake word (open mic, or the follow up window) short
        // sounds, whisper's stock hallucinations and audio whisper doubts
        // are not requests.
        let table = crate::instant::table(&self.cfg.voice.instant);
        let follow_up = self.voice.wake_until.is_some_and(|t| Instant::now() < t);
        let stats = self.voice.cur_stats;
        let woke = self.strip_wake(&norm).map(str::to_string);
        // The wake word alone always gets an answer, in every mode (no
        // length or noise filter applies to it).
        if woke.as_deref().is_some_and(|c| c.trim().is_empty()) {
            return self.bare_wake("Yes?", ptt, true);
        }
        let why = if ptt {
            // Push to talk is deliberate; only silence's stock text is dropped.
            crate::app_openmic::is_noise(&norm).then_some("noise")
        } else if woke.is_none() && (self.voice.open_mic || follow_up) {
            crate::app_openmic::hands_free_filter(&norm, stats, &table)
        } else {
            crate::app_openmic::confidence_gate(stats)
        };
        // A short reply to what the assistant just said or asked ("Go
        // ahead.", "Close it.", "Yes.") is an answer, not noise.
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
        if command.trim().is_empty() {
            self.voice.wake_until = Some(Instant::now() + Duration::from_secs(6));
            self.voice.action = Some("listening for a command".into());
            return;
        }
        self.run_heard(command);
    }

    /// Route an utterance: an instant one word command runs at once;
    /// everything else goes to the assistant, which interprets it.
    fn run_heard(&mut self, command: String) {
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
        self.ask_assistant_from(&command, Some(&raw));
    }

    /// Run an instant command. None hands the utterance to the assistant
    /// instead (a yes the assistant is waiting for, several tabs waiting).
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
            // Not paused: "resume" is a request for the assistant.
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
                // The assistant asked something: the answer is for it.
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
                // An open dialog takes yes / no.
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
                // The focused tab if it asks, else the only waiting tab;
                // several waiting is the assistant's to sort out.
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
        let name = self.panes[slot]
            .account
            .map(|a| self.cfg.accounts[a].display().to_string())
            .unwrap_or_else(|| format!("pane {}", slot + 1));
        if self.panes[slot].tabs.len() > 1 {
            format!("{name} tab {}", spell(tab + 1))
        } else {
            name
        }
    }

    /// Every tab waiting for approval, longest waiting first.
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

    /// What tab (slot, tab) is asking for, read from its screen.
    pub fn request_of(&self, slot: usize, tab: usize) -> Option<crate::prompt::Request> {
        let t = &self.panes[slot].tabs[tab];
        let screen = t
            .parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .contents();
        crate::prompt::describe_request(&screen)
    }

    /// Answer one waiting tab. Only ever called on an explicit request.
    pub fn answer_tab(&mut self, slot: usize, tab: usize, choice: Choice) -> String {
        let who = self.describe(slot, tab);
        if self.panes[slot].tabs[tab].activity != Activity::Permission {
            return format!("{who} is not waiting for approval");
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

    /// Keys that pick `choice` on the prompt shown in a tab, read from the
    /// screen. Falls back to 1 / 2 / Esc when the prompt cannot be parsed.
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
        if let Some(p) = parse_prompt(&screen) {
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

    /// Type a prompt into a tab and submit it.
    pub(crate) fn send_text(&mut self, slot: usize, tab: usize, text: &str) {
        self.type_text(slot, tab, text, true);
    }

    fn type_text(&mut self, slot: usize, tab: usize, text: &str, enter: bool) {
        let t = &mut self.panes[slot].tabs[tab];
        t.reset_scroll();
        if enter {
            // Paste, then Enter as its own write a moment later: in the
            // same burst claude reads the CR as a newline in the paste.
            t.write(&keys::encode_paste(text, true));
            t.write_later(b"\r", std::time::Duration::from_millis(80));
        } else {
            t.write(&keys::encode_paste(text, t.bracketed_paste()));
        }
    }

    /// Speak a short confirmation.
    pub(crate) fn speak(&self, text: &str) {
        self.speak_as(Kind::Confirm, text);
    }

    pub fn speak_as(&self, kind: Kind, text: &str) {
        let vc = &self.cfg.voice;
        let on = match kind {
            Kind::Confirm => vc.speak_confirm,
            Kind::Announce => vc.announce,
        };
        if !on {
            return;
        }
        if let Some(v) = &self.voice.engine {
            v.say(text);
        }
    }

    /// Run a parsed command. Returns the HUD text describing what happened.
    /// Logged in account with the most 5 hour quota left, and that percent.
    pub fn best_account(&self) -> Option<(usize, f64)> {
        self.accounts
            .iter()
            .enumerate()
            .filter(|(_, st)| st.login.logged_in())
            .filter_map(|(i, st)| st.effective_left().map(|l| (i, l)))
            .filter(|(_, l)| *l >= 2.0)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// When the focused account is nearly out of 5 hour quota, suggest the
    /// account with the most left.
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

    /// Announce background tabs that need approval or finished.
    pub fn voice_tick(&mut self) {
        // Notice a system sleep right after the wake (see clock).
        crate::clock::observe();
        // Conversation mode: when we finish talking back, the reply needs
        // no wake word.
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
        self.open_mic_tick();
        self.refresh_vocabulary();
        let any_speaking = speaking || self.voice.preview.as_ref().is_some_and(|p| p.speaking());
        self.assistant_follow_up(any_speaking);
        self.listen_after_reply(any_speaking);
        if self.attention.is_empty() {
            return;
        }
        let items = std::mem::take(&mut self.attention);
        let every = Duration::from_secs(self.cfg.voice.announce_every_s.max(3) as u64);
        self.voice.spoken.set_per_key(every * 3);
        for a in items {
            let need = a.what == Activity::Permission;
            let who = self.describe(a.slot, a.tab);
            let msg = if need {
                format!("{who} needs approval")
            } else {
                format!("{who} finished")
            };
            self.flash(msg.clone());
            crate::log::info(&format!("attention: {msg}"));
            let key = (a.slot, a.tab, need);
            // Desktop notification when the terminal is not in front (or
            // when it never reports focus at all).
            if self.cfg.notifications
                && self.voice.term_focused != Some(true)
                && self.voice.notified.allow(key)
            {
                crate::notify::post("godterm", &msg);
            }
            // Paused: held, and summarized on resume.
            if self.cfg.voice.announce && self.hold_announcement(&msg) {
                continue;
            }
            if !self.cfg.voice.announce || self.voice.engine.is_none() || self.voice.asleep {
                continue;
            }
            if self.voice.spoken.allow(key) {
                self.speak_as(Kind::Announce, &msg);
            }
        }
    }

    pub fn voice_hud_visible(&self) -> bool {
        self.voice.engine.is_some()
            || self.voice.info.is_some()
            || self.voice.muted
            || crate::demo::active()
    }
}
