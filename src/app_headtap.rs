//! The headphone tap (voice.headphone_tap): play/pause on headphones or
//! the media key turns the mic on or off. See media_keys for how a press
//! gets here and which presses count.
//!
//! A tap is about the mic only:
//!
//! - Off: the mic closes (● MUTED), but the voice engine stays, so a reply
//!   being said goes on to its end and announcements are still spoken.
//!   Nothing is cut off, there is no barge-in.
//! - On: from muted, paused, asleep or voice off, the mic opens and the
//!   next thing said needs no wake word (as after the wake word alone).
//!   While a reply is being said it keeps going; in push to talk the
//!   capture starts once it ends. Barge-in rules are as usual.
//!
//! Feedback is a flash, the mic chip, and the chime when on (only when
//! nothing is being said).

use std::time::{Duration, Instant};

use crate::app::App;
use crate::media_keys::{Press, Verdict};
use crate::voice::VoiceStatus;

impl App {
    /// The mic is on and listening (what a tap turns off).
    pub fn tap_listening(&self) -> bool {
        if self.voice.engine.is_none() || self.voice.muted || self.paused() || self.voice.asleep {
            return false;
        }
        if self.voice.always_on || self.voice.open_mic {
            return true;
        }
        // Push to talk: only while capturing, or about to (after a reply).
        self.voice.status == VoiceStatus::Listening || self.voice.listen_after_speech
    }

    /// Talking back right now (the engine or the preview).
    fn tap_speaking(&self) -> bool {
        self.voice.engine.as_ref().is_some_and(|v| v.speaking())
            || self.voice.preview.as_ref().is_some_and(|p| p.speaking())
    }

    /// Every UI loop, on the main thread.
    pub fn media_pump(&mut self) {
        if !self.media.claimed() {
            return;
        }
        let (listening, speaking) = (self.tap_listening(), self.tap_speaking());
        self.media.pump(listening, speaking);
    }

    /// Apply the setting (it may change in Settings or config.toml).
    pub fn headphone_tap_tick(&mut self) {
        let want = self.cfg.voice.headphone_tap && !crate::demo::active();
        let tx = self.tx.clone();
        self.media.set(want, &tx);
    }

    /// Turn the setting on or off, kept in config.toml.
    pub fn set_headphone_tap(&mut self, on: bool) {
        if self.cfg.voice.headphone_tap != on {
            let _ = crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global("voice.headphone_tap"),
                Some(toml_edit::value(on)),
            );
            self.config_mtime = crate::app::config_mtime();
            self.cfg.voice.headphone_tap = on;
        }
        self.headphone_tap_tick();
        self.flash(if on {
            "Headphone tap on: play/pause on your headphones (or the media key) turns the mic on or off"
        } else {
            "Headphone tap off: play/pause goes to your music app again"
        });
    }

    pub fn toggle_headphone_tap(&mut self) {
        let on = !self.cfg.voice.headphone_tap;
        self.set_headphone_tap(on);
    }

    /// A press from the headphones or the media key.
    pub fn headphone_tap(&mut self, p: Press) {
        if !self.media.on() {
            return;
        }
        let listening = self.tap_listening();
        match self.media.decide(p, listening) {
            Verdict::Off => self.tap_mic_off(),
            Verdict::On => self.tap_mic_on(),
            Verdict::Ignored(_) => {}
        }
    }

    /// Close the mic; what is being said goes on.
    fn tap_mic_off(&mut self) {
        crate::log::info("voice: headphone tap: mic off");
        if !self.voice.muted {
            self.voice.muted_prev = self.voice_mode();
        }
        self.voice.muted = true;
        self.voice.open_mic = false;
        self.voice.wake_until = None;
        self.voice.listen_after_speech = false;
        self.voice.holding = None;
        self.voice.partial = None;
        if let Some(v) = &self.voice.engine {
            v.set_mic(false);
        }
        self.voice.action = Some("mic off (headphone tap)".into());
        self.flash("Mic off (headphone tap; tap again to listen)");
    }

    /// Open the mic and listen for a command without the wake word.
    fn tap_mic_on(&mut self) {
        crate::log::info("voice: headphone tap: listening");
        let speaking = self.tap_speaking();
        if self.voice.muted {
            if self.voice.engine.is_some() {
                self.resume_kept_mic();
            } else {
                self.set_muted(false);
            }
        }
        if self.paused() {
            self.resume_listening("headphone tap");
        }
        if self.voice.engine.is_none() {
            match self.cfg.voice.mode.as_str() {
                "open" => {
                    self.voice.open_mic = true;
                    self.voice.last_heard = Instant::now();
                    self.start_voice(true);
                }
                "push" => self.start_voice(false),
                _ => self.start_voice(true),
            }
        }
        self.voice.asleep = false;
        self.voice.last_heard = Instant::now();
        // As after the wake word alone: the next utterance is the command.
        let secs = self.cfg.voice.wake_follow_up_s.clamp(3, 30);
        self.voice.wake_until = Some(Instant::now() + Duration::from_secs(secs));
        if !self.voice.always_on && !self.voice.open_mic {
            if speaking {
                // Push to talk opens after the reply (never cuts it off).
                self.voice.listen_after_speech = true;
            } else if let Some(v) = &self.voice.engine {
                v.push_to_talk();
                self.voice.status = VoiceStatus::Listening;
            }
        }
        if !speaking {
            if let Some(v) = &self.voice.engine {
                v.chime();
            }
        }
        self.voice.updated = Instant::now();
        self.voice.action = Some("listening (headphone tap)".into());
        self.flash("Listening (headphone tap)");
    }

    /// Unmute when the engine was kept (a tap turned the mic off): open
    /// the mic again without a restart, so nothing being said stops.
    pub(crate) fn resume_kept_mic(&mut self) {
        self.voice.muted = false;
        if self.voice.muted_prev == 3 {
            self.voice.open_mic = true;
            self.voice.last_heard = Instant::now();
        }
        if let Some(v) = &self.voice.engine {
            v.set_mic(true);
        }
        crate::log::info("voice: unmuted (engine kept)");
    }
}
