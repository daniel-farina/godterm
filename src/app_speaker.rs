//! Speaker lock in the app: what the voice thread's guard reported
//! (rejections counted for the strip and listed in the voice log),
//! enrollment progress for the training dialog, and the mic mode picker.

use crate::app::App;
use crate::app_voice::{LogEntry, LOG_LEN};
use crate::voice::speaker::{Check, Cmd, Event};

/// Read during "Train my voice", after the wake word training clips.
pub const SPEAKER_SENTENCES: &[&str] = &[
    "My voice is the one GodTerm should listen to.",
    "Open a new tab and show me what changed today.",
    "Seven bright orange foxes ran along the quiet river.",
];

/// The voice strip's shield and count: "⛨ 3 ignored: not you".
pub fn strip_text(lock_on: bool, profile: bool, not_you: u32) -> Option<String> {
    if !lock_on || !profile {
        return None;
    }
    Some(if not_you == 0 {
        "⛨".to_string()
    } else {
        format!("⛨ {not_you} ignored: not you")
    })
}

/// One line for Settings: the last score, or what is missing.
pub fn status_line(last: Option<&Check>, profile: Option<&str>, model: bool) -> String {
    if !model {
        return "Speaker lock: model not installed (godterm voice install-speaker)".into();
    }
    let Some(p) = profile else {
        return "Speaker lock: no voice profile yet (Train my voice)".into();
    };
    let last = match last.and_then(|c| c.verdict.map(|v| (v, c))) {
        Some((v, c)) => format!(
            "   last: score {:.2} vs {:.2} ({}{})",
            v.score,
            v.threshold,
            if v.accepted { "you" } else { "not you" },
            match (&c.far, c.applies) {
                (Some(f), _) => format!(", {f}"),
                (None, false) => ", not enforced".into(),
                _ => String::new(),
            }
        ),
        None => "   last: none yet (talk, then look here)".into(),
    };
    format!("Speaker lock: {p}{last}")
}

impl App {
    pub fn on_speaker(&mut self, e: Event) {
        match e {
            Event::Checked(c) => {
                if let Some(why) = c.reason() {
                    self.voice.not_you += 1;
                    self.voice.action = Some(format!("({why}, ignored)"));
                    let entry = LogEntry {
                        at: chrono::Local::now(),
                        heard: "(not you)".into(),
                        action: format!("{why}, ignored"),
                        partial_ms: None,
                    };
                    if self.voice.log.len() == LOG_LEN {
                        self.voice.log.pop_front();
                    }
                    self.voice.log.push_back(entry);
                }
                self.voice.speaker_last = Some(c);
            }
            Event::Clips(n) => self.train_speaker_note(format!("Voice clips: {n}"), false),
            Event::Calibrated(Ok(s)) => self.train_speaker_note(format!("Your voice: {s}"), true),
            Event::Calibrated(Err(e)) => {
                self.train_speaker_note(format!("Voice not learned: {e}"), false)
            }
            Event::Saved(Ok(s)) => {
                self.voice.speaker_profile = Some(s.clone());
                self.flash(format!("Voice profile saved: {s}"));
            }
            Event::Saved(Err(e)) => self.flash(format!("Could not save the voice profile: {e}")),
            Event::Error(e) => {
                crate::log::info(&format!("voice: speaker lock: {e}"));
                self.train_speaker_note(format!("Speaker lock: {e}"), false);
                if self.voice.training.is_none() {
                    self.voice.info = Some(format!("speaker lock: {e}"));
                }
            }
        }
    }

    fn train_speaker_note(&mut self, s: String, ready: bool) {
        if let Some(t) = self.voice.training.as_mut() {
            t.speaker = Some(s);
            t.speaker_ready = ready;
        }
    }

    /// Send a command to the voice thread's speaker guard. False when the
    /// engine is not running.
    pub fn speaker_cmd(&mut self, c: Cmd) -> bool {
        match &self.voice.engine {
            Some(v) => {
                v.speaker(c);
                true
            }
            None => false,
        }
    }

    /// Settings > Voice > Train my voice: the wake training dialog, with
    /// three sentences for the voice only (added to earlier clips).
    pub fn start_voice_training(&mut self) {
        let ww = self
            .cfg
            .voice
            .wake_words
            .first()
            .cloned()
            .unwrap_or_else(|| "hey go".into());
        let mut t = crate::app_wake::Training::new(&ww);
        t.voice_only = true;
        self.voice.training = Some(t);
        self.modal = crate::app::Modal::WakeTrain;
    }

    /// Settings > Voice > Open macOS mic modes: the system picker where
    /// Voice Isolation is chosen (while GodTerm's mic is open).
    pub fn open_mic_modes(&mut self) {
        match crate::voice::capture::show_mic_modes() {
            Ok(()) => self.flash(
                "Mic modes: pick Voice Isolation (Control Center lists it while the mic is open)",
            ),
            Err(e) => self.flash(format!("Mic modes unavailable: {e}")),
        }
    }

    /// The Settings status line for the speaker lock.
    pub fn speaker_status(&self) -> String {
        let model = crate::voice::speaker::model_path(&self.cfg.voice.speaker_model).is_file();
        status_line(
            self.voice.speaker_last.as_ref(),
            self.voice.speaker_profile.as_deref(),
            model,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::speaker::Verdict;

    fn check(score: f32, applies: bool, far: Option<&str>) -> Check {
        Check {
            verdict: Some(Verdict {
                score,
                threshold: 0.42,
                accepted: score >= 0.42,
                ms: 40,
                windows: 0,
            }),
            far: far.map(str::to_string),
            applies,
        }
    }

    #[test]
    fn strip_shows_the_shield_and_count() {
        assert_eq!(strip_text(false, true, 3), None);
        assert_eq!(strip_text(true, false, 3), None);
        assert_eq!(strip_text(true, true, 0).as_deref(), Some("⛨"));
        assert_eq!(
            strip_text(true, true, 3).as_deref(),
            Some("⛨ 3 ignored: not you")
        );
    }

    #[test]
    fn rejections_are_counted_and_logged() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(crate::config::Config::default(), tx);
        app.on_speaker(Event::Checked(check(0.41, true, None)));
        assert_eq!(app.voice.not_you, 1);
        let last = app.voice.log.back().unwrap();
        assert_eq!(last.action, "speaker mismatch (score 0.41), ignored");
        // Not enforced (push to talk): scored, not counted.
        app.on_speaker(Event::Checked(check(0.2, false, None)));
        assert_eq!(app.voice.not_you, 1);
        // Far away: counted with the reason.
        app.on_speaker(Event::Checked(check(
            0.8,
            true,
            Some("far away: 18 dB under your voice"),
        )));
        assert_eq!(app.voice.not_you, 2);
        assert!(app.voice.action.as_deref().unwrap().contains("far away"));
        // Accepted: nothing counted.
        app.on_speaker(Event::Checked(check(0.7, true, None)));
        assert_eq!(app.voice.not_you, 2);
    }

    #[test]
    fn settings_status_line() {
        assert!(status_line(None, None, false).contains("install-speaker"));
        assert!(status_line(None, None, true).contains("Train my voice"));
        let c = check(0.41, true, None);
        let s = status_line(Some(&c), Some("8 clips"), true);
        assert!(s.contains("0.41 vs 0.42 (not you)"), "{s}");
        let c = check(0.7, false, None);
        let s = status_line(Some(&c), Some("8 clips"), true);
        assert!(s.contains("(you, not enforced)"), "{s}");
    }
}
