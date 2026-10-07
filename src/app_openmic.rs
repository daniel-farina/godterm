//! Open mic: always listening, every utterance a command (no wake word),
//! with guards against noise and whisper hallucinations, a visible
//! indicator, and an automatic return to wake word mode after a while.

use std::time::{Duration, Instant};

use crate::app::App;
use crate::voice::wake_profile::UttStats;

/// What whisper writes for silence, noise or background chatter, and
/// lone filler words: never a request.
const HALLUCINATIONS: &[&str] = &[
    "thank you",
    "thanks",
    "thank you very much",
    "thank you so much",
    "thanks for watching",
    "thank you for watching",
    "thanks for listening",
    "you",
    "bye",
    "bye bye",
    "byebye",
    "goodbye",
    "blank audio",
    "silence",
    "music",
    "applause",
    "laughter",
    "um",
    "uh",
    "uh huh",
    "hmm",
    "mm",
    "mhm",
    "okay",
    "ok",
    "oh",
    "ah",
    "so",
    "well",
    "right",
    "alright",
    "all right",
    "hello",
    "hi",
    "hey",
    "maybe",
    "yeah",
    "huh",
    "what",
    "subtitles by the amara org community",
    "please subscribe",
    "like and subscribe",
    "see you next time",
    "i'm sorry",
    "sorry",
    "wow",
    "cool",
    "nice",
];

/// Drop audio whisper itself thinks is not speech, or decoded with low
/// confidence (no_speech_prob over 0.6, mean log probability under -1).
pub fn confidence_gate(stats: UttStats) -> Option<&'static str> {
    if stats.no_speech.is_some_and(|n| n > 0.6) {
        return Some("no speech");
    }
    if stats.logprob.is_some_and(|l| l < -1.0) {
        return Some("unclear");
    }
    None
}

/// Empty, a stock transcription of silence ("Thank you."), or a sound tag.
pub fn is_noise(norm: &str) -> bool {
    hands_free_filter(norm, UttStats::default(), &[]) == Some("noise")
}

/// Why an utterance heard without a wake word (open mic, or the follow up
/// window) is ignored, or None to act on it.
pub fn hands_free_filter(
    norm: &str,
    stats: UttStats,
    instant: &[(String, crate::instant::Instant)],
) -> Option<&'static str> {
    let t = norm.trim();
    let bare: String = t
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '\'')
        .collect();
    let bare = bare.split_whitespace().collect::<Vec<_>>().join(" ");
    if bare.is_empty()
        || HALLUCINATIONS.contains(&bare.as_str())
        || HALLUCINATIONS.contains(&bare.replace('\'', "").as_str())
        || t.starts_with('[')
        || t.starts_with('(')
    {
        return Some("noise");
    }
    let exact = crate::instant::match_instant(t, instant).is_some();
    if !exact {
        if let Some(why) = confidence_gate(stats) {
            return Some(why);
        }
    }
    let words = bare.split(' ').count();
    // Too little speech energy, or one word, is not a request.
    let short = (stats.ms > 0 && stats.ms < 600)
        || (stats.speech_ms > 0 && stats.speech_ms < 350)
        || words < 2;
    if short && !exact {
        return Some("too short");
    }
    None
}

impl App {
    pub fn start_open_mic(&mut self) {
        self.stop_voice();
        self.voice.open_mic = true;
        self.voice.asleep = false;
        self.voice.last_heard = Instant::now();
        self.start_voice(true);
        self.flash("Open mic: everything you say is a command, no wake word (say \"sleep\" or click OPEN MIC to stop)");
        self.speak("open mic");
    }

    /// The Voice dropdown: 0 off, 1 push to talk, 2 wake word, 3 open mic.
    pub fn set_voice_mode(&mut self, m: u8) {
        // Muted means the mic is off: the mode waits for the unmute.
        if self.voice.muted && m > 0 {
            self.voice.muted_prev = m;
            crate::log::info(&format!(
                "voice: mode {m} chosen while muted; it starts on unmute"
            ));
            self.flash(
                "The mic is muted: unmute with Ctrl-a X (or click ● MUTED); that mode starts then",
            );
            return;
        }
        match m {
            0 => {
                self.voice.open_mic = false;
                self.stop_voice();
                self.voice.info = None;
                self.flash("Voice off");
            }
            1 | 2 => {
                self.voice.open_mic = false;
                self.stop_voice();
                self.start_voice(m == 2);
                self.flash(if m == 2 {
                    "Voice: wake word"
                } else {
                    "Voice: push to talk (Ctrl-a space)"
                });
            }
            _ => self.start_open_mic(),
        }
    }

    /// The current voice mode, as set_voice_mode numbers it.
    pub fn voice_mode(&self) -> u8 {
        match (
            &self.voice.engine,
            self.voice.always_on,
            self.voice.open_mic,
        ) {
            (_, _, true) => 3,
            // Demo mode: the wake word mode is shown, no mic is opened.
            (None, true, false) if crate::demo::active() => 2,
            (None, _, _) => 0,
            (Some(_), true, false) => 2,
            (Some(_), false, false) => 1,
        }
    }

    /// Back to the wake word (true) or voice off.
    pub fn leave_open_mic(&mut self, to_wake: bool) {
        self.voice.open_mic = false;
        self.stop_voice();
        if to_wake {
            self.start_voice(true);
        }
        let ww = self
            .cfg
            .voice
            .wake_words
            .first()
            .cloned()
            .unwrap_or_default();
        self.flash(if to_wake {
            format!("Open mic off: listening for \"{ww}\" again")
        } else {
            "Voice off".into()
        });
    }

    /// Auto sleep after a long silence.
    pub fn open_mic_tick(&mut self) {
        let min = self.cfg.voice.open_mic_sleep_min;
        if !self.voice.open_mic
            || min == 0
            || self.voice.last_heard.elapsed() < Duration::from_secs(min as u64 * 60)
        {
            return;
        }
        let ww = self
            .cfg
            .voice
            .wake_words
            .first()
            .cloned()
            .unwrap_or_else(|| "the wake word".into());
        self.speak(&format!("going to sleep, say {ww} to wake me"));
        self.leave_open_mic(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Vec<(String, crate::instant::Instant)> {
        crate::instant::table(
            &crate::instant::DEFAULTS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn guards() {
        let long = UttStats {
            ms: 1500,
            mean_db: -30.0,
            peak_db: -20.0,
            speech_ms: 1500,
            ..Default::default()
        };
        let blip = UttStats {
            ms: 300,
            mean_db: -40.0,
            peak_db: -30.0,
            speech_ms: 300,
            ..Default::default()
        };
        for n in [
            "thank you",
            "okay",
            "hello",
            "maybe",
            "bye-bye",
            "bye bye",
            "you",
            "thanks for watching",
            "um",
        ] {
            assert_eq!(
                hands_free_filter(&crate::voice::grammar::normalize(n), long, &ctx()),
                Some("noise"),
                "{n}"
            );
        }
        // whisper's own doubts.
        let doubt = UttStats {
            no_speech: Some(0.8),
            ..long
        };
        assert_eq!(
            hands_free_filter("it's santa in his house", doubt, &ctx()),
            Some("no speech")
        );
        let unclear = UttStats {
            logprob: Some(-1.4),
            ..long
        };
        assert_eq!(
            hands_free_filter("it's santa in his house", unclear, &ctx()),
            Some("unclear")
        );
        let sure = UttStats {
            no_speech: Some(0.02),
            logprob: Some(-0.2),
            ..long
        };
        assert_eq!(
            hands_free_filter("close all tabs in all accounts", sure, &ctx()),
            None
        );
        // Little speech energy in a long recording.
        let faint = UttStats {
            speech_ms: 200,
            ..long
        };
        assert_eq!(
            hands_free_filter("open a new tab", faint, &ctx()),
            Some("too short")
        );

        assert_eq!(
            hands_free_filter("thanks for watching", long, &ctx()),
            Some("noise")
        );
        assert_eq!(
            hands_free_filter("[blank_audio]", long, &ctx()),
            Some("noise")
        );
        assert_eq!(hands_free_filter("you", long, &ctx()), Some("noise"));
        // One word or a blip: only an exact command gets through.
        assert_eq!(hands_free_filter("approve", blip, &ctx()), None);
        assert_eq!(hands_free_filter("next tab", blip, &ctx()), None);
        assert_eq!(hands_free_filter("banana", long, &ctx()), Some("too short"));
        assert_eq!(
            hands_free_filter("tell me about the weather today", blip, &ctx()),
            Some("too short")
        );
        assert_eq!(
            hands_free_filter("what is everyone working on", long, &ctx()),
            None
        );
    }
}
