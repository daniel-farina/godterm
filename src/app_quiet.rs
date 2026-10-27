use std::time::{Duration, Instant};

use crate::app::App;
use crate::app_assistant::{Entry, Who};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Voice {
    Announce,

    Reply,

    Progress,
}

#[derive(Debug, Clone)]
pub struct Update {
    pub text: String,
    pub voice: Voice,

    pub urgent: bool,

    pub src: Option<(String, crate::told::Source)>,
}

#[derive(Debug, Default)]
pub struct QuietGate {
    pub queue: Vec<Update>,

    pub spoke_at: Option<Instant>,
    pub typed_at: Option<Instant>,

    pub said: Vec<String>,

    #[cfg(test)]
    pub fake_tts: bool,
}

impl App {
    pub fn quiet_user_spoke(&mut self) {
        self.assistant.quiet.spoke_at = Some(Instant::now());
    }

    pub fn quiet_user_typed(&mut self) {
        self.assistant.quiet.typed_at = Some(Instant::now());
    }

    pub fn tts_playing(&self) -> bool {
        #[cfg(test)]
        if self.assistant.quiet.fake_tts {
            return true;
        }
        self.voice.engine.as_ref().is_some_and(|v| v.speaking())
            || self.voice.preview.as_ref().is_some_and(|p| p.speaking())
    }

    pub fn sentence_over(&self) -> bool {
        self.voice.partial.is_none() && self.voice.holding.is_none()
    }

    pub fn quiet_moment(&self) -> bool {
        self.not_quiet_because().is_none()
    }

    pub fn not_quiet_because(&self) -> Option<&'static str> {
        let n = Duration::from_secs(self.cfg.assistant.update_quiet_s);
        let q = &self.assistant.quiet;
        let calm = |t: Option<Instant>| t.is_none_or(|t| t.elapsed() >= n);
        if !self.sentence_over() {
            Some("the user is mid sentence")
        } else if !calm(q.spoke_at) {
            Some("the user just spoke")
        } else if !calm(q.typed_at) {
            Some("the user just typed")
        } else if self.assistant.busy {
            Some("the assistant is mid turn")
        } else if self.assistant.stale != 0 {
            Some("a cut off turn is still ending")
        } else if self.tts_playing() {
            Some("talk back is playing")
        } else if !self.pending_confirms.is_empty() {
            Some("a confirmation waits for an answer")
        } else if self.paused() {
            Some("listening is paused")
        } else {
            None
        }
    }

    pub fn can_report_late(&self) -> bool {
        self.sentence_over() && !self.assistant.busy && self.assistant.stale == 0 && !self.paused()
    }

    pub fn say_update(&mut self, voice: Voice, text: &str) {
        self.queue_update(voice, text, false);
    }

    pub fn say_urgent(&mut self, voice: Voice, text: &str) {
        self.queue_update(voice, text, true);
    }

    fn queue_update(&mut self, voice: Voice, text: &str, urgent: bool) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        if self.hold_announcement(text) {
            return;
        }
        if self.assistant.quiet.queue.iter().any(|u| u.text == text) {
            return;
        }
        crate::log::info(&format!(
            "quiet: queued{}: {text}",
            if urgent { " (urgent)" } else { "" }
        ));
        let src = self.told.borrow().ctx.clone();
        self.assistant.quiet.queue.push(Update {
            text: text.to_string(),
            voice,
            urgent,
            src,
        });
        self.quiet_tick();
    }

    pub fn quiet_tick(&mut self) {
        if self.assistant.quiet.queue.is_empty() {
            return;
        }

        if self.brainstorm_on() {
            return;
        }
        if self.paused() {
            for u in std::mem::take(&mut self.assistant.quiet.queue) {
                self.hold_announcement(&u.text);
            }
            return;
        }
        let now: Vec<Update> = if self.quiet_moment() {
            std::mem::take(&mut self.assistant.quiet.queue)
        } else if self.sentence_over() && self.assistant.quiet.queue.iter().any(|u| u.urgent) {
            let (u, rest) = std::mem::take(&mut self.assistant.quiet.queue)
                .into_iter()
                .partition(|u| u.urgent);
            self.assistant.quiet.queue = rest;
            u
        } else {
            return;
        };
        self.deliver_updates(now);
    }

    pub fn take_updates(&mut self) -> Vec<String> {
        let q = std::mem::take(&mut self.assistant.quiet.queue);
        let texts: Vec<String> = q.into_iter().map(|u| u.text).collect();
        self.assistant.quiet.said.extend(texts.iter().cloned());
        self.trim_said();
        texts
    }

    fn trim_said(&mut self) {
        let n = self.assistant.quiet.said.len();
        if n > 20 {
            self.assistant.quiet.said.drain(..n - 20);
        }
    }

    fn deliver_updates(&mut self, us: Vec<Update>) {
        for voice in [Voice::Reply, Voice::Progress, Voice::Announce] {
            let texts: Vec<String> = us
                .iter()
                .filter(|u| u.voice == voice)
                .map(|u| u.text.clone())
                .collect();
            if texts.is_empty() {
                continue;
            }
            let text = texts.join(" ");
            crate::log::info(&format!("quiet: saying: {text}"));
            self.assistant.quiet.said.push(text.clone());

            let record = |app: &App, how: &str, ch: crate::told::Channel| {
                for u in us.iter().filter(|u| u.voice == voice) {
                    let mut log = app.told.borrow_mut();
                    let prev = std::mem::replace(&mut log.ctx, u.src.clone());
                    log.record(how, &u.text, ch, false);
                    log.ctx = prev;
                }
            };
            match voice {
                Voice::Announce => {
                    let vc = &self.cfg.voice;
                    if !vc.speaker_muted && vc.announce && self.speaker_on() {
                        record(self, "announcement", crate::told::Channel::Spoken);
                    }
                    if let Some(v) = &self.voice.engine {
                        if !vc.speaker_muted && vc.announce {
                            v.say(&text);
                        }
                    }
                }
                Voice::Reply => {
                    record(self, "update", crate::told::Channel::Panel);
                    self.assistant
                        .log
                        .push(Entry::new(Who::Reply, text.clone()));
                    self.assistant.spoken.push(text.clone());
                    if self.tts_on() && !self.cfg.voice.speaker_muted {
                        if self.will_speak_assistant() {
                            record(self, "update", crate::told::Channel::Spoken);
                        }
                        self.speak_assistant(&text);
                    }
                }
                Voice::Progress => {
                    if !self.assistant.muted && self.tts_on() && self.reply_spoken() {
                        if self.will_speak_assistant() {
                            record(self, "progress", crate::told::Channel::Spoken);
                        }
                        self.speak_assistant(&text);
                    }
                }
            }
        }
        self.trim_said();
    }
}
