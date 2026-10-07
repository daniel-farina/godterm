//! "Hold on a minute": stop listening for a while. While paused nothing
//! heard is acted on except the wake word (or "resume"), which resumes at
//! once and runs the rest of the utterance. Background announcements wait
//! and are summarized on resume. When the time is up the previous mode
//! comes back by itself. The assistant pauses with the `pause_listening`
//! tool (it reads the duration from what was said); "hold on" and "pause"
//! alone are instant.

use std::time::{Duration, Instant};

use crate::app::App;

#[derive(Debug, Clone)]
pub struct Pause {
    pub until: Instant,
    /// Open mic was on: it comes back on resume.
    pub was_open_mic: bool,
    /// Announcements held back while paused.
    pub queued: Vec<String>,
    pub reason: Option<String>,
    pub started: Instant,
}

impl Pause {
    /// Now on the pause's clock: time the Mac slept counts, so "five
    /// minutes" is over after a night with the lid closed.
    fn now(&self) -> Instant {
        Instant::now() + crate::clock::slept_since(self.started)
    }
}

/// "2 minutes", "30 seconds", "1 minute 30 seconds".
pub fn spoken_duration(secs: u64) -> String {
    let (m, s) = (secs / 60, secs % 60);
    let unit = |n: u64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    match (m, s) {
        (0, s) => unit(s, "second"),
        (m, 0) => unit(m, "minute"),
        (m, s) => format!("{} {}", unit(m, "minute"), unit(s, "second")),
    }
}

impl App {
    pub fn paused(&self) -> bool {
        self.voice.paused.is_some()
    }

    /// Seconds left, rounded up.
    pub fn pause_left(&self) -> Option<u64> {
        self.voice.paused.as_ref().map(|p| {
            p.until
                .saturating_duration_since(p.now())
                .as_secs_f64()
                .ceil() as u64
        })
    }

    /// Stop acting on speech for `secs` (default and cap from Settings).
    /// Returns the seconds it will wait.
    pub fn pause_listening(&mut self, secs: Option<u64>, reason: Option<String>) -> u64 {
        let max = self.cfg.voice.pause_max_s.max(10) as u64;
        let secs = secs
            .filter(|s| *s > 0)
            .unwrap_or(self.cfg.voice.pause_default_s as u64)
            .clamp(1, max);
        let was_open_mic = self
            .voice
            .paused
            .as_ref()
            .map(|p| p.was_open_mic)
            .unwrap_or(self.voice.open_mic);
        let queued = self
            .voice
            .paused
            .take()
            .map(|p| p.queued)
            .unwrap_or_default();
        // Open mic is suspended: only the wake word is screened for.
        if self.voice.open_mic {
            self.voice.open_mic = false;
            self.stop_voice();
            self.start_voice(true);
        }
        self.voice.wake_until = None;
        self.voice.paused = Some(Pause {
            until: Instant::now() + Duration::from_secs(secs),
            was_open_mic,
            queued,
            reason: reason.clone(),
            started: Instant::now(),
        });
        if let Some(v) = &self.voice.engine {
            v.chime();
        }
        let ww = self
            .cfg
            .voice
            .wake_words
            .first()
            .cloned()
            .unwrap_or_default();
        crate::log::info(&format!(
            "voice: paused for {secs} s{}",
            reason.map(|r| format!(" ({r})")).unwrap_or_default()
        ));
        self.voice.action = Some(format!("paused for {}", spoken_duration(secs)));
        self.flash(format!(
            "Paused for {}: say \"{ww}\" or click PAUSED to resume",
            spoken_duration(secs)
        ));
        secs
    }

    /// Back to listening: the mode from before, and what was held back.
    pub fn resume_listening(&mut self, why: &str) {
        let Some(p) = self.voice.paused.take() else {
            return;
        };
        crate::log::info(&format!("voice: resumed ({why})"));
        let mins = p.started.elapsed().as_secs().div_ceil(60).max(1);
        self.assistant.resume_note = Some(format!(
            "(Listening resumed at {} after a {} pause ({why}). It is active now: answer normally.)",
            chrono::Local::now().format("%H:%M"),
            if mins == 1 { "1 minute".to_string() } else { format!("{mins} minute") }
        ));
        if p.was_open_mic && !self.voice.muted {
            // Quietly: no "open mic" announcement.
            self.stop_voice();
            self.voice.open_mic = true;
            self.voice.asleep = false;
            self.voice.last_heard = Instant::now();
            self.start_voice(true);
        }
        if let Some(v) = &self.voice.engine {
            v.chime();
        }
        self.voice.action = Some(format!("listening again ({why})"));
        if !p.queued.is_empty() {
            let mut seen: Vec<String> = vec![];
            for q in p.queued {
                if !seen.contains(&q) {
                    seen.push(q);
                }
            }
            let msg = format!("While you were away: {}.", seen.join("; "));
            self.flash(msg.clone());
            self.speak_as(crate::voice::tts::Kind::Announce, &msg);
            self.voice.away_summary = Some(msg);
        } else {
            self.flash("Listening again");
        }
    }

    /// Hold an announcement while paused. True when it was held.
    pub fn hold_announcement(&mut self, msg: &str) -> bool {
        match self.voice.paused.as_mut() {
            Some(p) => {
                p.queued.push(msg.to_string());
                true
            }
            None => false,
        }
    }

    /// The pause ran out: resume on our own.
    pub fn pause_tick(&mut self) {
        if self
            .voice
            .paused
            .as_ref()
            .is_some_and(|p| p.now() >= p.until)
        {
            self.resume_listening("time is up");
        }
    }

    /// While paused, an utterance: the wake word (or "resume") resumes and
    /// gives back what follows it; anything else is dropped (None).
    pub fn while_paused(&mut self, norm: &str, ptt: bool) -> Option<String> {
        let woke = self.strip_wake(norm).map(str::to_string);
        let resume_word = matches!(
            norm.trim(),
            "resume" | "resume listening" | "i'm back" | "im back" | "continue listening"
        );
        if ptt || woke.is_some() || resume_word {
            self.resume_listening(if ptt { "push to talk" } else { "wake word" });
            return Some(if resume_word {
                String::new()
            } else {
                woke.unwrap_or_else(|| norm.to_string())
            });
        }
        crate::log::info(&format!("voice: paused, ignored: {norm}"));
        self.voice.ignored += 1;
        self.voice.action = Some("(paused, ignored)".into());
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_out() {
        assert_eq!(spoken_duration(10), "10 seconds");
        assert_eq!(spoken_duration(60), "1 minute");
        assert_eq!(spoken_duration(120), "2 minutes");
        assert_eq!(spoken_duration(90), "1 minute 30 seconds");
    }
}
