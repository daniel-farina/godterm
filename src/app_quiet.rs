//! One gate for everything the assistant says without being asked: a
//! tab's answer coming back, a tab that finished or needs approval, admin
//! and Remote Control progress, its own account moving, a failover. While
//! the user is talking, has just spoken or typed, the assistant is mid
//! turn or still speaking, or a confirmation waits for an answer, the
//! update is queued; at the next quiet moment the queue is said in one go
//! (or folded into the turn that reports tab answers). Flashes and panel
//! lines still show at once; only speech and turns wait.

use std::time::{Duration, Instant};

use crate::app::App;
use crate::app_assistant::{Entry, Who};

/// How an update is said.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Voice {
    /// The announcer (voice.announce): "hyper finished".
    Announce,
    /// The assistant's own voice, shown as its reply when said.
    Reply,
    /// The assistant's voice for a progress line already shown in the
    /// panel (spoken only when replies are spoken).
    Progress,
}

#[derive(Debug, Clone)]
pub struct Update {
    pub text: String,
    pub voice: Voice,
    /// Said as soon as the user's sentence ends, quiet or not.
    pub urgent: bool,
    /// Where it came from (told.rs), recorded when it is said.
    pub src: Option<(String, crate::told::Source)>,
}

#[derive(Debug, Default)]
pub struct QuietGate {
    pub queue: Vec<Update>,
    /// The user's last utterance (or partial), and last typed message.
    pub spoke_at: Option<Instant>,
    pub typed_at: Option<Instant>,
    /// What the gate let through, newest last (the panel and tests).
    pub said: Vec<String>,
    /// Tests: talk back is playing.
    #[cfg(test)]
    pub fake_tts: bool,
}

impl App {
    /// The user said something (or is saying it): the quiet wait starts
    /// over.
    pub fn quiet_user_spoke(&mut self) {
        self.assistant.quiet.spoke_at = Some(Instant::now());
    }

    pub fn quiet_user_typed(&mut self) {
        self.assistant.quiet.typed_at = Some(Instant::now());
    }

    /// Talk back is playing (the voice engine or the preview speaker).
    pub fn tts_playing(&self) -> bool {
        #[cfg(test)]
        if self.assistant.quiet.fake_tts {
            return true;
        }
        self.voice.engine.as_ref().is_some_and(|v| v.speaking())
            || self.voice.preview.as_ref().is_some_and(|p| p.speaking())
    }

    /// The user is not in the middle of a sentence.
    pub fn sentence_over(&self) -> bool {
        self.voice.partial.is_none() && self.voice.holding.is_none()
    }

    /// Whether an unprompted update may be said now.
    pub fn quiet_moment(&self) -> bool {
        let n = Duration::from_secs(self.cfg.assistant.update_quiet_s);
        let q = &self.assistant.quiet;
        let calm = |t: Option<Instant>| t.is_none_or(|t| t.elapsed() >= n);
        self.sentence_over()
            && calm(q.spoke_at)
            && calm(q.typed_at)
            && !self.assistant.busy
            && self.assistant.stale == 0
            && !self.tts_playing()
            && self.pending_confirms.is_empty()
            && !self.paused()
    }

    /// Say an update now if it is quiet, else at the next quiet moment.
    pub fn say_update(&mut self, voice: Voice, text: &str) {
        self.queue_update(voice, text, false);
    }

    /// An update that cannot wait (the account limit is hit and the
    /// assistant moves now, a hard failure): said once the user's current
    /// sentence ends.
    pub fn say_urgent(&mut self, voice: Voice, text: &str) {
        self.queue_update(voice, text, true);
    }

    fn queue_update(&mut self, voice: Voice, text: &str, urgent: bool) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        // Paused: held for the "While you were away" summary.
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

    /// Every tick: say what waited, once it is quiet (urgent ones once the
    /// sentence is over).
    pub fn quiet_tick(&mut self) {
        if self.assistant.quiet.queue.is_empty() {
            return;
        }
        // Brainstorming: updates wait until the draft is sent.
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

    /// Take what waits, for a turn that tells it along with tab answers.
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

    /// Say a batch: one line per voice, merged.
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
            // Recorded one by one, each with its own source (told.rs).
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
                    self.assistant.log.push(Entry {
                        who: Who::Reply,
                        text: text.clone(),
                    });
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
