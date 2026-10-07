//! Near field gating: the user talks close to the mic; a phone call on
//! speaker or a TV across the room is much quieter and duller (distance
//! and narrowband phone audio take the highs away, reverb smears them).
//! An utterance far under the user's usual level that also has much less
//! high frequency energy than the user's voice is dropped.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use super::speaker_fbank::{Fbank, NUM_BINS};

/// How an utterance sounds: speech level and brightness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Acoustic {
    /// Mean level of the speech frames (dBFS).
    pub level_db: f32,
    /// Energy over 3.4 kHz relative to 300 Hz to 3.4 kHz, over the speech
    /// frames (dB; lower is duller).
    pub hf_db: f32,
}

fn bin_hz(b: usize) -> f32 {
    // Bin centers of the 80 bin mel bank (20 Hz to 7.6 kHz).
    let mel = |f: f32| 1127.0 * (1.0 + f / 700.0).ln();
    let (lo, hi) = (mel(20.0), mel(7_600.0));
    let m = lo + (b + 1) as f32 * (hi - lo) / (NUM_BINS + 1) as f32;
    700.0 * ((m / 1127.0).exp() - 1.0)
}

impl Acoustic {
    pub fn of(samples: &[i16], fbank: &Fbank) -> Acoustic {
        let st = super::wake_profile::UttStats::of(samples);
        let x: Vec<f32> = samples.iter().map(|s| *s as f32).collect();
        let feats = fbank.compute(&x);
        let speech = super::speaker::speech_frames(samples);
        let (mut hi, mut mid) = (0f64, 0f64);
        for (row, sp) in feats.chunks(NUM_BINS).zip(&speech) {
            if !sp {
                continue;
            }
            for (b, v) in row.iter().enumerate() {
                let hz = bin_hz(b);
                let e = (*v as f64).exp();
                if hz >= 3_400.0 {
                    hi += e;
                } else if hz >= 300.0 {
                    mid += e;
                }
            }
        }
        let hf_db = if mid > 0.0 && hi > 0.0 {
            (10.0 * (hi / mid).log10()) as f32
        } else {
            -60.0
        };
        Acoustic {
            level_db: st.mean_db,
            hf_db,
        }
    }
}

/// The user's reference level and brightness: enrollment clips plus the
/// most recent accepted utterances.
#[derive(Debug, Clone, Default)]
pub struct NearField {
    enrolled: Vec<Acoustic>,
    recent: VecDeque<Acoustic>,
}

/// How much duller than the user counts as distant (dB of HF ratio).
pub const DULL_DB: f32 = 6.0;
/// This much under the quiet bar is rejected even when bright.
pub const VERY_QUIET_EXTRA_DB: f32 = 10.0;
/// References needed before the gate does anything.
pub const MIN_REFS: usize = 3;

fn median(mut v: Vec<f32>) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

impl NearField {
    pub fn new(enrolled: Vec<Acoustic>) -> NearField {
        NearField {
            enrolled,
            recent: Default::default(),
        }
    }

    /// Remember an accepted utterance (the last 20).
    pub fn accept(&mut self, a: Acoustic) {
        self.recent.push_back(a);
        while self.recent.len() > 20 {
            self.recent.pop_front();
        }
    }

    fn refs(&self) -> Vec<Acoustic> {
        self.enrolled
            .iter()
            .chain(self.recent.iter())
            .copied()
            .collect()
    }

    /// (median level, median brightness) of the user, once known.
    pub fn reference(&self) -> Option<(f32, f32)> {
        let r = self.refs();
        if r.len() < MIN_REFS {
            return None;
        }
        Some((
            median(r.iter().map(|a| a.level_db).collect())?,
            median(r.iter().map(|a| a.hf_db).collect())?,
        ))
    }

    /// Why this utterance sounds like someone far away, if it does:
    /// more than `quiet_db` under the user's median level and duller than
    /// the user, or quieter still regardless of brightness.
    pub fn check(&self, a: Acoustic, quiet_db: f32) -> Option<String> {
        let (level, hf) = self.reference()?;
        let under = level - a.level_db;
        let duller = hf - a.hf_db;
        if under > quiet_db + VERY_QUIET_EXTRA_DB {
            return Some(format!("far away: {under:.0} dB under your voice"));
        }
        if under > quiet_db && duller > DULL_DB {
            return Some(format!(
                "far away: {under:.0} dB under your voice and {duller:.0} dB duller"
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(level_db: f32, hf_db: f32) -> Acoustic {
        Acoustic { level_db, hf_db }
    }

    #[test]
    fn level_gate() {
        // Nothing known yet: never rejects.
        let g = NearField::new(vec![a(-22.0, -12.0)]);
        assert_eq!(g.check(a(-70.0, -40.0), 12.0), None);
        let mut g = NearField::new(vec![a(-22.0, -12.0), a(-24.0, -13.0), a(-21.0, -11.0)]);
        assert_eq!(g.reference(), Some((-22.0, -12.0)));
        // The user, a bit quieter: fine.
        assert_eq!(g.check(a(-28.0, -14.0), 12.0), None);
        // Much quieter but just as bright (the user leaning back): fine.
        assert_eq!(g.check(a(-37.0, -12.0), 12.0), None);
        // Much quieter and duller: a speakerphone across the room.
        let why = g.check(a(-38.0, -25.0), 12.0).expect("rejected");
        assert!(
            why.contains("16 dB under") && why.contains("duller"),
            "{why}"
        );
        // Very quiet: rejected even when bright.
        assert!(g.check(a(-46.0, -12.0), 12.0).is_some());
        // A looser setting lets it through.
        assert_eq!(g.check(a(-38.0, -25.0), 20.0), None);
        // Accepted utterances move the reference.
        for _ in 0..10 {
            g.accept(a(-36.0, -24.0));
        }
        assert_eq!(g.check(a(-38.0, -25.0), 12.0), None);
    }

    #[test]
    fn acoustics_of_bright_and_dull_audio() {
        let fb = Fbank::new();
        // White-ish noise (bright) vs the same noise low passed (dull).
        let mut seed = 12345u32;
        let noise: Vec<i16> = (0..32_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((seed >> 16) as i16 as f32 * 0.3) as i16
            })
            .collect();
        // Three one pole low passes around 500 Hz (18 dB per octave).
        let mut x: Vec<f32> = noise.iter().map(|s| *s as f32).collect();
        for _ in 0..3 {
            let mut y = 0f32;
            for v in x.iter_mut() {
                y += 0.18 * (*v - y);
                *v = y;
            }
        }
        let dull: Vec<i16> = x.iter().map(|v| *v as i16).collect();
        let b = Acoustic::of(&noise, &fb);
        let d = Acoustic::of(&dull, &fb);
        assert!(b.hf_db > d.hf_db + 10.0, "{b:?} {d:?}");
        assert!(b.level_db > d.level_db, "{b:?} {d:?}");
    }
}
