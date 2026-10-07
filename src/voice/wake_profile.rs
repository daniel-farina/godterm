//! Wake word training, transcript based: you say the wake word a few
//! times and a few normal sentences, we keep what whisper actually heard
//! for the wake word (aliases, with how often) and the normal sentences as
//! negatives. Stored in `~/.godterm/voice/wake_profile.json`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::grammar::{edit_distance, normalize, strip_wake};

/// What one utterance sounded like.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UttStats {
    pub ms: u32,
    pub mean_db: f32,
    pub peak_db: f32,
    /// Time whisper took on it (0 when unknown).
    #[serde(default)]
    pub stt_ms: u32,
    /// Time with speech energy in it (frames within 20 dB of the peak).
    #[serde(default)]
    pub speech_ms: u32,
    /// whisper's mean word probability, no-speech probability and mean
    /// log probability (None when unknown).
    #[serde(default)]
    pub conf: Option<f32>,
    #[serde(default)]
    pub no_speech: Option<f32>,
    #[serde(default)]
    pub logprob: Option<f32>,
}

impl UttStats {
    pub fn of(samples: &[i16]) -> UttStats {
        use super::audio::{db, rms, FRAME, RATE};
        let ms = (samples.len() as u64 * 1000 / RATE as u64) as u32;
        let frames: Vec<f32> = samples.chunks(FRAME).map(|f| db(rms(f))).collect();
        let peak_db = frames.iter().cloned().fold(-90.0, f32::max);
        // Mean over the frames that carry speech (within 30 dB of the peak).
        let loud: Vec<f32> = frames
            .iter()
            .cloned()
            .filter(|d| *d > peak_db - 30.0)
            .collect();
        let mean_db = if loud.is_empty() {
            -90.0
        } else {
            loud.iter().sum::<f32>() / loud.len() as f32
        };
        let speech_ms = frames
            .iter()
            .filter(|d| **d > peak_db - 20.0 && **d > -60.0)
            .count() as u32
            * super::audio::FRAME_MS;
        UttStats {
            ms,
            mean_db,
            peak_db,
            speech_ms,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alias {
    pub text: String,
    pub count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Accuracy {
    pub detected: usize,
    pub attempts: usize,
    pub false_triggers: usize,
    pub negatives: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WakeProfile {
    pub wake_word: String,
    /// What whisper heard when you said the wake word, most frequent first.
    pub aliases: Vec<Alias>,
    /// Normal sentences that must not wake it.
    pub negatives: Vec<String>,
    /// Your wake word takes: average length and loudness.
    pub wake_stats: UttStats,
    pub normal_stats: UttStats,
    pub accuracy: Accuracy,
    pub trained_at: String,
}

pub fn path() -> PathBuf {
    crate::config::app_home()
        .join("voice")
        .join("wake_profile.json")
}

/// Similarity 0..1 from edit distance.
pub fn similarity(a: &str, b: &str) -> f32 {
    let n = a.chars().count().max(b.chars().count());
    if n == 0 {
        return 1.0;
    }
    1.0 - edit_distance(a, b) as f32 / n as f32
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| *c != ' ').collect()
}

fn mean(v: &[UttStats]) -> UttStats {
    if v.is_empty() {
        return UttStats::default();
    }
    let n = v.len() as f32;
    UttStats {
        ms: (v.iter().map(|s| s.ms as f32).sum::<f32>() / n) as u32,
        mean_db: v.iter().map(|s| s.mean_db).sum::<f32>() / n,
        peak_db: v.iter().map(|s| s.peak_db).sum::<f32>() / n,
        ..Default::default()
    }
}

impl WakeProfile {
    pub fn load() -> Option<WakeProfile> {
        Self::load_from(&path())
    }

    pub fn load_from(p: &std::path::Path) -> Option<WakeProfile> {
        let text = std::fs::read_to_string(p).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&path())
    }

    pub fn save_to(&self, p: &std::path::Path) -> std::io::Result<()> {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, p)
    }

    pub fn reset() -> std::io::Result<()> {
        Self::reset_at(&path())
    }

    pub fn reset_at(p: &std::path::Path) -> std::io::Result<()> {
        match std::fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Learn from the wake word takes and the normal sentences, then
    /// score the result on the same takes.
    pub fn learn(
        wake_word: &str,
        takes: &[(String, UttStats)],
        normals: &[(String, UttStats)],
        wake_words: &[String],
        sensitivity: f32,
    ) -> WakeProfile {
        let ww = normalize(wake_word);
        let n = ww.split(' ').filter(|w| !w.is_empty()).count().max(1);
        let negatives: Vec<String> = normals
            .iter()
            .map(|(t, _)| normalize(t))
            .filter(|t| !t.is_empty())
            .collect();
        let mut aliases: Vec<Alias> = vec![];
        for (t, _) in takes {
            let norm = normalize(t);
            let words: Vec<&str> = norm.split(' ').filter(|w| !w.is_empty()).collect();
            if words.is_empty() {
                continue;
            }
            // The wake word is what was said, so the first n words (or all
            // of them, when whisper merged it into fewer) are its sound.
            let cand = words[..n.min(words.len())].join(" ");
            if cand == ww {
                continue;
            }
            // Never learn something a normal sentence starts with.
            let clash = negatives.iter().any(|neg| {
                let head: Vec<&str> = neg.split(' ').take(n).collect();
                similarity(&squash(&head.join(" ")), &squash(&cand)) >= 0.9
            });
            if clash {
                continue;
            }
            match aliases.iter_mut().find(|a| a.text == cand) {
                Some(a) => a.count += 1,
                None => aliases.push(Alias {
                    text: cand,
                    count: 1,
                }),
            }
        }
        aliases.sort_by(|a, b| b.count.cmp(&a.count).then(a.text.cmp(&b.text)));
        let mut p = WakeProfile {
            wake_word: ww,
            aliases,
            negatives,
            wake_stats: mean(&takes.iter().map(|(_, s)| *s).collect::<Vec<_>>()),
            normal_stats: mean(&normals.iter().map(|(_, s)| *s).collect::<Vec<_>>()),
            accuracy: Accuracy::default(),
            trained_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        };
        let detected = takes
            .iter()
            .filter(|(t, _)| p.matches(&normalize(t), wake_words, sensitivity).is_some())
            .count();
        let false_triggers = normals
            .iter()
            .filter(|(t, _)| p.matches(&normalize(t), wake_words, sensitivity).is_some())
            .count();
        p.accuracy = Accuracy {
            detected,
            attempts: takes.len(),
            false_triggers,
            negatives: normals.len(),
        };
        p
    }

    /// The command after the wake word, using the built in matching first
    /// and then the learned aliases. A known normal sentence never wakes.
    pub fn matches<'a>(
        &self,
        norm: &'a str,
        wake_words: &[String],
        sensitivity: f32,
    ) -> Option<&'a str> {
        if self
            .negatives
            .iter()
            .any(|neg| similarity(neg, norm) >= 0.9)
        {
            return None;
        }
        if let Some(rest) = strip_wake(norm, wake_words) {
            return Some(rest);
        }
        let words: Vec<&str> = norm.split(' ').filter(|w| !w.is_empty()).collect();
        for a in &self.aliases {
            let n = a.text.split(' ').count();
            // Compare squashed, over the same number of words and one more
            // or less, since whisper splits and joins words freely.
            for k in [n, n + 1, n.saturating_sub(1)] {
                if k == 0 || k > words.len() {
                    continue;
                }
                let head = words[..k].join(" ");
                if similarity(&squash(&head), &squash(&a.text)) >= sensitivity {
                    let skip: usize = words[..k].iter().map(|w| w.len() + 1).sum();
                    return Some(norm.get(skip.min(norm.len())..).unwrap_or("").trim());
                }
            }
        }
        None
    }
}

/// Wake matching with an optional profile.
pub fn strip_wake_with<'a>(
    profile: Option<&WakeProfile>,
    norm: &'a str,
    wake_words: &[String],
    sensitivity: f32,
) -> Option<&'a str> {
    match profile {
        Some(p) => p.matches(norm, wake_words, sensitivity),
        None => strip_wake(norm, wake_words),
    }
}

/// Normal sentences read during training. One starts with "hey" on
/// purpose, a near miss for "hey go".
pub const NORMAL_SENTENCES: &[&str] = &[
    "I think we should get lunch soon.",
    "The build finished a few minutes ago.",
    "Can you send me the report later?",
    "Let's go over the plan tomorrow morning.",
    "Hey, how was your weekend?",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> UttStats {
        UttStats {
            ms: 800,
            mean_db: -30.0,
            peak_db: -20.0,
            speech_ms: 800,
            ..Default::default()
        }
    }

    #[test]
    fn learns_aliases_and_scores() {
        let ww = vec!["jarvis".to_string()];
        let takes: Vec<(String, UttStats)> = ["Jarvis.", "Travis.", "Travis", "Jervis!", "Jarvis"]
            .iter()
            .map(|t| (t.to_string(), s()))
            .collect();
        let normals: Vec<(String, UttStats)> = NORMAL_SENTENCES
            .iter()
            .map(|t| (t.to_string(), s()))
            .collect();
        let p = WakeProfile::learn("Jarvis", &takes, &normals, &ww, 0.75);
        assert_eq!(p.wake_word, "jarvis");
        assert_eq!(
            p.aliases[0],
            Alias {
                text: "travis".into(),
                count: 2
            }
        );
        assert!(p.aliases.iter().any(|a| a.text == "jervis"));
        assert_eq!(p.accuracy.detected, 5);
        assert_eq!(p.accuracy.attempts, 5);
        assert_eq!(p.accuracy.false_triggers, 0);
        assert_eq!(p.wake_stats.ms, 800);
        // A learned alias now wakes it, with the command after it.
        assert_eq!(
            p.matches("travis show the dashboard", &ww, 0.75),
            Some("show the dashboard")
        );
        // Close variants within the sensitivity also do; far ones do not.
        assert_eq!(p.matches("traviss approve", &ww, 0.75), Some("approve"));
        assert_eq!(p.matches("david approve", &ww, 0.75), None);
        // Without the profile only the built in matching applies.
        assert_eq!(strip_wake_with(None, "travis approve", &ww, 0.75), None);
        // A normal sentence never wakes it.
        assert_eq!(
            p.matches("the build finished a few minutes ago", &ww, 0.5),
            None
        );
    }

    #[test]
    fn never_learns_a_negative() {
        let ww = vec!["hey go".to_string()];
        // Whisper heard "hey how" for the wake word once, but "hey, how
        // was your weekend" is a normal sentence: not learned.
        let takes = vec![
            ("hey how".to_string(), s()),
            ("a go".to_string(), s()),
            ("hey go".to_string(), s()),
        ];
        let normals = vec![("Hey, how was your weekend?".to_string(), s())];
        let p = WakeProfile::learn("hey go", &takes, &normals, &ww, 0.75);
        assert!(p.aliases.iter().all(|a| a.text != "hey how"));
        assert!(p.aliases.iter().any(|a| a.text == "a go"));
        assert_eq!(p.accuracy.false_triggers, 0);
        assert_eq!(p.matches("hey how was your weekend", &ww, 0.75), None);
    }

    #[test]
    fn stats_of_audio() {
        let mut v = vec![0i16; 16_000];
        for (i, x) in v.iter_mut().enumerate().take(8_000) {
            *x = if i % 2 == 0 { 3277 } else { -3277 };
        }
        let st = UttStats::of(&v);
        assert_eq!(st.ms, 1000);
        assert!((st.peak_db + 20.0).abs() < 0.5, "{st:?}");
        assert!((st.mean_db + 20.0).abs() < 0.5, "{st:?}");
    }

    #[test]
    fn saves_and_resets() {
        let home = std::env::temp_dir().join(format!("cg-wake-{}", std::process::id()));
        let f = home.join("voice").join("wake_profile.json");
        let p = WakeProfile {
            wake_word: "jarvis".into(),
            ..Default::default()
        };
        p.save_to(&f).unwrap();
        assert_eq!(WakeProfile::load_from(&f).unwrap().wake_word, "jarvis");
        WakeProfile::reset_at(&f).unwrap();
        assert!(WakeProfile::load_from(&f).is_none());
        WakeProfile::reset_at(&f).unwrap();
        let _ = std::fs::remove_dir_all(home);
    }
}
