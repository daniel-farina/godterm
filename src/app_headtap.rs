use std::time::{Duration, Instant};

use crate::app::App;
use crate::media_keys::{Press, Verdict};
use crate::voice::VoiceStatus;

impl App {
    pub fn tap_listening(&self) -> bool {
        if self.voice.engine.is_none() || self.voice.muted || self.paused() || self.voice.asleep {
            return false;
        }
        if self.voice.always_on || self.voice.open_mic {
            return true;
        }

        self.voice.status == VoiceStatus::Listening || self.voice.listen_after_speech
    }

    pub(crate) fn tap_speaking(&self) -> bool {
        self.voice.engine.as_ref().is_some_and(|v| v.speaking())
            || self.voice.preview.as_ref().is_some_and(|p| p.speaking())
    }

    pub fn media_pump(&mut self) {
        if !self.media.claimed() {
            return;
        }
        let (listening, speaking) = (self.tap_listening(), self.tap_speaking());
        self.media.pump(listening, speaking);
    }

    pub fn headphone_tap_tick(&mut self) {
        let want = self.cfg.voice.headphone_tap && !crate::demo::active();
        let tx = self.tx.clone();
        self.media.set(want, &tx);
    }

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

    fn tap_mic_off(&mut self) {
        crate::log::info("voice: headphone tap: mic off");
        if !self.voice.muted {
            self.voice.muted_prev = 3;
        }
        self.voice.muted = true;

        self.voice.reopen = None;
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

    fn tap_mic_on(&mut self) {
        crate::log::info("voice: headphone tap: listening (open mic)");
        let speaking = self.tap_speaking();
        self.voice.muted_prev = 3;
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

        if self.voice.engine.is_some() && !self.voice.always_on {
            if speaking {
                self.voice.open_mic_after_speech = true;
            } else {
                self.stop_voice();
            }
        }
        if self.voice.engine.is_none() {
            self.voice.reopen = None;
            self.start_voice_in(3);
        }
        if !self.voice.open_mic {
            self.voice.open_mic = true;
            self.voice.last_heard = Instant::now();
        }
        self.voice.asleep = false;
        self.voice.last_heard = Instant::now();

        let secs = self.cfg.voice.wake_follow_up_s.clamp(3, 30);
        self.voice.wake_until = Some(Instant::now() + Duration::from_secs(secs));
        if !self.voice.always_on && !self.voice.open_mic {
            if speaking {
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
