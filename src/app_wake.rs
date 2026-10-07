//! Wake word training: say the wake word five times and five normal
//! sentences; what whisper heard becomes the wake profile.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Modal};
use crate::voice::wake_profile::{UttStats, WakeProfile, NORMAL_SENTENCES};

pub const TAKES: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Intro,
    /// Saying the wake word, take n (0 based).
    Wake(usize),
    /// Reading normal sentence n.
    Normal(usize),
    /// Learned, not saved yet.
    Done(WakeProfile),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Training {
    pub phase: Phase,
    pub wake_word: String,
    pub takes: Vec<(String, UttStats)>,
    pub normals: Vec<(String, UttStats)>,
    /// What was heard last, shown under the prompt.
    pub last: Option<String>,
    /// The profile on disk when training started.
    pub existing: Option<WakeProfile>,
    /// "Train my voice": only the speaker lock's sentences, no wake word.
    pub voice_only: bool,
    /// Speaker lock enrollment progress or result.
    pub speaker: Option<String>,
    /// The voice was calibrated and can be saved.
    pub speaker_ready: bool,
}

impl Training {
    pub fn new(wake_word: &str) -> Training {
        Training {
            phase: Phase::Intro,
            wake_word: wake_word.to_string(),
            takes: vec![],
            normals: vec![],
            last: None,
            existing: WakeProfile::load(),
            voice_only: false,
            speaker: None,
            speaker_ready: false,
        }
    }

    /// The sentences read in this training.
    pub fn sentences(&self) -> &'static [&'static str] {
        if self.voice_only {
            crate::app_speaker::SPEAKER_SENTENCES
        } else {
            NORMAL_SENTENCES
        }
    }

    /// Voice only training is over: Done carries the wake profile as it
    /// was (it is not changed).
    fn voice_done(&mut self) {
        self.phase = Phase::Done(self.existing.clone().unwrap_or_default());
    }

    /// What to say now.
    pub fn prompt(&self) -> Option<String> {
        match &self.phase {
            Phase::Wake(_) => Some(format!("Say \"{}\"", self.wake_word)),
            Phase::Normal(i) => self.sentences().get(*i).map(|s| format!("Say \"{s}\"")),
            _ => None,
        }
    }

    /// Record one utterance and move on. True when a new capture should
    /// start.
    pub fn heard(
        &mut self,
        text: &str,
        stats: UttStats,
        wake_words: &[String],
        sensitivity: f32,
    ) -> bool {
        self.last = Some(text.trim().to_string());
        match self.phase {
            Phase::Wake(i) => {
                self.takes.push((text.to_string(), stats));
                self.phase = if i + 1 < TAKES {
                    Phase::Wake(i + 1)
                } else {
                    Phase::Normal(0)
                };
                true
            }
            Phase::Normal(i) => {
                self.normals.push((text.to_string(), stats));
                if i + 1 < self.sentences().len() {
                    self.phase = Phase::Normal(i + 1);
                    true
                } else if self.voice_only {
                    self.voice_done();
                    false
                } else {
                    let p = WakeProfile::learn(
                        &self.wake_word,
                        &self.takes,
                        &self.normals,
                        wake_words,
                        sensitivity,
                    );
                    self.phase = Phase::Done(p);
                    false
                }
            }
            _ => false,
        }
    }

    /// Skip the current prompt (nothing heard, or a bad take).
    pub fn skip(&mut self, wake_words: &[String], sensitivity: f32) -> bool {
        match self.phase {
            Phase::Wake(i) => {
                self.phase = if i + 1 < TAKES {
                    Phase::Wake(i + 1)
                } else {
                    Phase::Normal(0)
                };
                true
            }
            Phase::Normal(i) if i + 1 < self.sentences().len() => {
                self.phase = Phase::Normal(i + 1);
                true
            }
            Phase::Normal(_) if self.voice_only => {
                self.voice_done();
                false
            }
            Phase::Normal(_) => {
                let p = WakeProfile::learn(
                    &self.wake_word,
                    &self.takes,
                    &self.normals,
                    wake_words,
                    sensitivity,
                );
                self.phase = Phase::Done(p);
                false
            }
            _ => false,
        }
    }

    /// The step counter, e.g. "wake word 2/5".
    pub fn step(&self) -> String {
        match &self.phase {
            Phase::Intro => "ready".into(),
            Phase::Wake(i) => format!("wake word {}/{TAKES}", i + 1),
            Phase::Normal(i) if self.voice_only => {
                format!("sentence {}/{}", i + 1, self.sentences().len())
            }
            Phase::Normal(i) => format!("normal sentence {}/{}", i + 1, NORMAL_SENTENCES.len()),
            Phase::Done(_) => "done".into(),
        }
    }

    /// Body lines of the dialog.
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![];
        if self.voice_only {
            return self.voice_lines();
        }
        match &self.phase {
            Phase::Intro => {
                v.push(format!("Wake word: \"{}\"", self.wake_word));
                v.push(format!(
                    "You will say it {TAKES} times, then read {} normal sentences.",
                    NORMAL_SENTENCES.len()
                ));
                v.push("Each step listens on its own; speak after the prompt, then pause.".into());
                v.push("Only transcripts are kept (~/.godterm/voice/wake_profile.json).".into());
                if let Some(p) = &self.existing {
                    v.push(String::new());
                    v.push(format!(
                        "Current profile ({}): {} aliases, detected {}/{}, false triggers {}/{}",
                        p.trained_at,
                        p.aliases.len(),
                        p.accuracy.detected,
                        p.accuracy.attempts,
                        p.accuracy.false_triggers,
                        p.accuracy.negatives
                    ));
                }
            }
            Phase::Wake(_) | Phase::Normal(_) => {
                v.push(format!("Step: {}", self.step()));
                v.push(String::new());
                v.push(self.prompt().unwrap_or_default());
                v.push(String::new());
                v.push(match &self.last {
                    Some(h) => format!("Last heard: \"{h}\""),
                    None => "Listening...".into(),
                });
            }
            Phase::Done(p) => {
                v.push(format!(
                    "Detected {}/{}, false triggers {}/{}",
                    p.accuracy.detected,
                    p.accuracy.attempts,
                    p.accuracy.false_triggers,
                    p.accuracy.negatives
                ));
                if p.aliases.is_empty() {
                    v.push("Learned aliases: none needed (heard the wake word as is)".into());
                } else {
                    let a: Vec<String> = p
                        .aliases
                        .iter()
                        .map(|a| format!("\"{}\" x{}", a.text, a.count))
                        .collect();
                    v.push(format!("Learned aliases: {}", a.join(", ")));
                }
                v.push(format!(
                    "Wake word takes: {} ms, {:.0} dBFS average, {:.0} dBFS peak",
                    p.wake_stats.ms, p.wake_stats.mean_db, p.wake_stats.peak_db
                ));
                v.push(format!(
                    "Normal sentences: {} ms, {:.0} dBFS average",
                    p.normal_stats.ms, p.normal_stats.mean_db
                ));
                v.push(String::new());
                v.push(
                    "Save keeps it; Retrain starts over; Reset removes any saved profile.".into(),
                );
            }
        }
        if let Some(sp) = &self.speaker {
            v.push(sp.clone());
        }
        v
    }

    /// The dialog for "Train my voice".
    fn voice_lines(&self) -> Vec<String> {
        let mut v = vec![];
        match &self.phase {
            Phase::Intro => {
                v.push("Speaker lock: GodTerm learns your voice, so people talking nearby".into());
                v.push("(a phone call, a TV) are ignored in open mic.".into());
                v.push(format!(
                    "Read {} sentences; they add to the clips from wake word training.",
                    self.sentences().len()
                ));
                v.push("Speak normally, at your usual distance from the mic.".into());
                v.push(
                    "Only voice embeddings are kept (~/.godterm/voice/speaker_profile.json)."
                        .into(),
                );
            }
            Phase::Wake(_) | Phase::Normal(_) => {
                v.push(format!("Step: {}", self.step()));
                v.push(String::new());
                v.push(self.prompt().unwrap_or_default());
                v.push(String::new());
                v.push(match &self.last {
                    Some(h) => format!("Last heard: \"{h}\""),
                    None => "Listening...".into(),
                });
            }
            Phase::Done(_) => {
                if self.speaker.is_none() {
                    v.push("Learning your voice...".into());
                }
                v.push(String::new());
                v.push(
                    "Save keeps it; Retrain starts over; Reset removes the voice profile.".into(),
                );
            }
        }
        if let Some(sp) = &self.speaker {
            v.insert(0, sp.clone());
        }
        v
    }
}

impl App {
    pub fn start_wake_training(&mut self) {
        let ww = self
            .cfg
            .voice
            .wake_words
            .first()
            .cloned()
            .unwrap_or_else(|| "hey go".into());
        self.voice.training = Some(Training::new(&ww));
        self.modal = Modal::WakeTrain;
    }

    /// Start capturing the next take (push to talk style, so no wake word
    /// is needed and the mic closes after each one in push mode).
    fn train_listen(&mut self) {
        if cfg!(test) {
            return; // never open the microphone from tests
        }
        if self.voice.engine.is_none() {
            self.start_voice(false);
        }
        if let Some(v) = &self.voice.engine {
            v.push_to_talk();
        }
    }

    pub fn train_begin(&mut self) {
        let mut voice_only = false;
        if let Some(t) = self.voice.training.as_mut() {
            voice_only = t.voice_only;
            t.phase = if voice_only {
                Phase::Normal(0)
            } else {
                Phase::Wake(0)
            };
            t.takes.clear();
            t.normals.clear();
            t.last = None;
            t.speaker = None;
            t.speaker_ready = false;
        }
        // The same takes enroll the voice for the speaker lock: from
        // scratch with the wake word, added to earlier clips otherwise.
        if !cfg!(test) && self.voice.engine.is_none() {
            self.start_voice(false);
        }
        self.speaker_cmd(crate::voice::speaker::Cmd::EnrollStart { append: voice_only });
        self.train_listen();
    }

    /// Training reached Done: calibrate the voice on the clips.
    fn train_calibrate(&mut self) {
        if self
            .voice
            .training
            .as_ref()
            .is_some_and(|t| matches!(t.phase, Phase::Done(_)))
        {
            self.speaker_cmd(crate::voice::speaker::Cmd::EnrollCalibrate);
        }
    }

    /// Route a transcript to training. True when it was consumed.
    pub fn train_heard(&mut self, text: &str, stats: UttStats) -> bool {
        if self.modal != Modal::WakeTrain {
            return false;
        }
        let (ww, sens) = (
            self.cfg.voice.wake_words.clone(),
            self.cfg.voice.wake_sensitivity,
        );
        let Some(t) = self.voice.training.as_mut() else {
            return false;
        };
        if !matches!(t.phase, Phase::Wake(_) | Phase::Normal(_)) {
            return true;
        }
        if t.heard(text, stats, &ww, sens) {
            self.train_listen();
        } else {
            self.train_calibrate();
        }
        true
    }

    pub fn train_skip(&mut self) {
        let (ww, sens) = (
            self.cfg.voice.wake_words.clone(),
            self.cfg.voice.wake_sensitivity,
        );
        let again = self
            .voice
            .training
            .as_mut()
            .is_some_and(|t| t.skip(&ww, sens));
        if again {
            self.train_listen();
        } else {
            self.train_calibrate();
        }
    }

    pub fn train_save(&mut self) {
        let Some(Training {
            phase: Phase::Done(p),
            voice_only,
            speaker_ready,
            ..
        }) = self.voice.training.clone()
        else {
            return;
        };
        if speaker_ready {
            self.speaker_cmd(crate::voice::speaker::Cmd::EnrollSave);
        }
        if voice_only {
            if !speaker_ready {
                self.flash("Your voice was not learned yet; nothing saved");
            }
            self.voice.training = None;
            self.modal = Modal::None;
            return;
        }
        match p.save() {
            Ok(()) => {
                self.voice.profile = Some(p.clone());
                if let Some(v) = &self.voice.engine {
                    v.set_profile(Some(p.clone()));
                }
                self.flash(format!(
                    "Wake profile saved: detected {}/{}, false triggers {}/{}",
                    p.accuracy.detected,
                    p.accuracy.attempts,
                    p.accuracy.false_triggers,
                    p.accuracy.negatives
                ));
            }
            Err(e) => self.flash(format!("Could not save the wake profile: {e}")),
        }
        self.voice.training = None;
        self.modal = Modal::None;
    }

    pub fn train_reset(&mut self) {
        if self.voice.training.as_ref().is_some_and(|t| t.voice_only) {
            if !self.speaker_cmd(crate::voice::speaker::Cmd::Reset) {
                let _ = crate::voice::speaker::SpeakerProfile::reset();
            }
            self.voice.speaker_profile = None;
            return self
                .flash("Voice profile removed: the speaker lock is off until you train again");
        }
        let _ = WakeProfile::reset();
        self.voice.profile = None;
        if let Some(v) = &self.voice.engine {
            v.set_profile(None);
        }
        if let Some(t) = self.voice.training.as_mut() {
            t.existing = None;
        }
        self.flash("Wake profile removed: built in matching only");
    }

    pub fn train_close(&mut self) {
        if let Some(v) = &self.voice.engine {
            v.stop_talk();
            v.speaker(crate::voice::speaker::Cmd::EnrollCancel);
        }
        self.voice.training = None;
        self.modal = Modal::None;
    }

    pub fn on_train_key(&mut self, k: KeyEvent) {
        let phase = self.voice.training.as_ref().map(|t| t.phase.clone());
        match (k.code, phase) {
            (KeyCode::Esc, _) => self.train_close(),
            (KeyCode::Enter, Some(Phase::Intro)) => self.train_begin(),
            (KeyCode::Enter, Some(Phase::Done(_))) => self.train_save(),
            (KeyCode::Char('s'), Some(Phase::Wake(_) | Phase::Normal(_))) => self.train_skip(),
            (KeyCode::Char('r'), _) => self.train_begin(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_through_the_steps() {
        let ww = vec!["jarvis".to_string()];
        let st = UttStats {
            ms: 700,
            mean_db: -28.0,
            peak_db: -18.0,
            speech_ms: 700,
            ..Default::default()
        };
        let mut t = Training::new("jarvis");
        t.existing = None;
        assert_eq!(t.step(), "ready");
        t.phase = Phase::Wake(0);
        for h in ["Jarvis.", "Travis", "Travis.", "Jarvis", "Service"] {
            assert!(t.prompt().unwrap().contains("jarvis"));
            assert!(t.heard(h, st, &ww, 0.75));
        }
        assert_eq!(t.phase, Phase::Normal(0));
        for s in NORMAL_SENTENCES.iter().take(NORMAL_SENTENCES.len() - 1) {
            assert!(t.heard(s, st, &ww, 0.75));
        }
        assert!(!t.heard(NORMAL_SENTENCES[4], st, &ww, 0.75));
        let Phase::Done(p) = &t.phase else {
            panic!("not done")
        };
        assert_eq!(p.accuracy.attempts, 5);
        assert_eq!(p.accuracy.detected, 5);
        assert_eq!(p.accuracy.false_triggers, 0);
        assert!(t.lines()[0].starts_with("Detected 5/5, false triggers 0/5"));
        assert!(t.lines()[1].contains("\"travis\" x2"));
    }

    #[test]
    fn skipping_still_finishes() {
        let ww = vec!["hey go".to_string()];
        let mut t = Training::new("hey go");
        t.phase = Phase::Wake(0);
        let mut n = 0;
        while t.skip(&ww, 0.75) {
            n += 1;
        }
        assert_eq!(n, TAKES + NORMAL_SENTENCES.len() - 1);
        let Phase::Done(p) = &t.phase else { panic!() };
        assert_eq!(p.accuracy.attempts, 0);
    }

    #[test]
    fn voice_only_training_reads_three_sentences() {
        let ww = vec!["hey go".to_string()];
        let mut t = Training::new("hey go");
        t.voice_only = true;
        t.existing = Some(WakeProfile {
            wake_word: "hey go".into(),
            ..Default::default()
        });
        assert!(t.lines()[0].contains("Speaker lock"));
        t.phase = Phase::Normal(0);
        let n = crate::app_speaker::SPEAKER_SENTENCES.len();
        assert_eq!(n, 3);
        for (i, s) in crate::app_speaker::SPEAKER_SENTENCES.iter().enumerate() {
            assert_eq!(t.step(), format!("sentence {}/3", i + 1));
            assert!(t.prompt().unwrap().contains(s));
            assert_eq!(t.heard(s, UttStats::default(), &ww, 0.75), i + 1 < n);
        }
        // Done keeps the wake profile as it was.
        let Phase::Done(p) = &t.phase else { panic!() };
        assert_eq!(p.wake_word, "hey go");
        assert!(t.lines().iter().any(|l| l.contains("Learning your voice")));
        t.speaker = Some("Your voice: 13 clips, threshold 0.42".into());
        assert_eq!(t.lines()[0], "Your voice: 13 clips, threshold 0.42");
        // Skipping also finishes.
        t.phase = Phase::Normal(0);
        let mut k = 0;
        while t.skip(&ww, 0.75) {
            k += 1;
        }
        assert_eq!(k, n - 1);
    }
}
