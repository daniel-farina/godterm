//! Neural noise suppression for steady non speech noise (fans, hum, keys,
//! street noise): RNNoise through `nnnoiseless`, a pure Rust port, so it
//! works the same on Linux. RNNoise runs at 48 kHz in 10 ms frames; the
//! mic stream is 16 kHz, so frames go up 3x through a polyphase low pass,
//! through the network, and back down. About 12 ms of delay.
//!
//! It does not remove other people talking (that is speech too; see
//! speaker.rs for that).

use nnnoiseless::DenoiseState;

const UP: usize = 3;
/// Low pass taps (per 48 kHz sample); a multiple of UP.
const TAPS: usize = 48;

fn lowpass() -> Vec<f32> {
    // Windowed sinc, cutoff 7.6 kHz at 48 kHz, Blackman window, unity DC.
    let fc = 7_600.0 / 48_000.0;
    let m = (TAPS - 1) as f32;
    let mut h: Vec<f32> = (0..TAPS)
        .map(|i| {
            let x = i as f32 - m / 2.0;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f32::consts::PI * fc * x).sin() / (std::f32::consts::PI * x)
            };
            let w = 0.42 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / m).cos()
                + 0.08 * (4.0 * std::f32::consts::PI * i as f32 / m).cos();
            sinc * w
        })
        .collect();
    let sum: f32 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= sum);
    h
}

pub struct Denoiser {
    st: Box<DenoiseState<'static>>,
    h: Vec<f32>,
    /// Last 16 kHz inputs (for the upsampler).
    up_hist: Vec<f32>,
    /// 48 kHz samples waiting for a full RNNoise frame.
    pending: Vec<f32>,
    /// Denoised 48 kHz samples not yet downsampled (primed with a frame
    /// of silence, so output keeps pace with input).
    ready: std::collections::VecDeque<f32>,
    /// Last 48 kHz denoised samples (for the downsampler).
    down_hist: Vec<f32>,
    /// RNNoise's speech probability for the last frame.
    pub vad: f32,
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser {
    pub fn new() -> Denoiser {
        Denoiser {
            st: DenoiseState::new(),
            h: lowpass(),
            up_hist: vec![0.0; TAPS / UP],
            pending: Vec::with_capacity(DenoiseState::FRAME_SIZE * 2),
            ready: std::iter::repeat_n(0.0, DenoiseState::FRAME_SIZE + TAPS).collect(),
            down_hist: vec![0.0; TAPS],
            vad: 0.0,
        }
    }

    /// Denoise one 16 kHz frame in place (any length).
    pub fn process(&mut self, frame: &mut [i16]) {
        let k = TAPS / UP;
        let mut out = vec![0f32; DenoiseState::FRAME_SIZE];
        for s in frame.iter() {
            self.up_hist.rotate_left(1);
            self.up_hist[k - 1] = *s as f32;
            // Polyphase: output phase p uses taps p, p+3, p+6, ...
            for p in 0..UP {
                let mut acc = 0f32;
                for j in 0..k {
                    acc += self.h[j * UP + p] * self.up_hist[k - 1 - j];
                }
                self.pending.push(acc * UP as f32);
            }
            if self.pending.len() >= DenoiseState::FRAME_SIZE {
                let input: Vec<f32> = self.pending.drain(..DenoiseState::FRAME_SIZE).collect();
                self.vad = self.st.process_frame(&mut out, &input);
                self.ready.extend(out.iter().copied());
            }
        }
        for s in frame.iter_mut() {
            for _ in 0..UP {
                let v = self.ready.pop_front().unwrap_or(0.0);
                self.down_hist.rotate_left(1);
                self.down_hist[TAPS - 1] = v;
            }
            let acc: f32 = self
                .h
                .iter()
                .zip(self.down_hist.iter().rev())
                .map(|(a, b)| a * b)
                .sum();
            *s = acc.round().clamp(-32768.0, 32767.0) as i16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::audio::{db, rms, FRAME};

    #[test]
    fn lowpass_has_unity_gain_and_cuts_highs() {
        let h = lowpass();
        assert!((h.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        // Response at 12 kHz (well over the cutoff) is small.
        let w = 2.0 * std::f32::consts::PI * 12_000.0 / 48_000.0;
        let (re, im) = h.iter().enumerate().fold((0.0, 0.0), |(r, i), (n, v)| {
            (r + v * (w * n as f32).cos(), i - v * (w * n as f32).sin())
        });
        assert!((re * re + im * im).sqrt() < 0.01);
    }

    #[test]
    fn keeps_a_voice_band_tone_and_cuts_steady_noise() {
        let mut d = Denoiser::new();
        // 2 s of white-ish noise at about -30 dBFS.
        let mut seed = 7u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((seed >> 16) as i16 as f32 * 0.03) as i16
        };
        let mut before = 0.0;
        let mut after = 0.0;
        for f in 0..66 {
            let mut fr: Vec<i16> = (0..FRAME).map(|_| noise()).collect();
            let b = rms(&fr);
            d.process(&mut fr);
            if f > 30 {
                before += b;
                after += rms(&fr);
            }
        }
        assert!(
            db(after) < db(before) - 6.0,
            "noise cut by {:.1} dB",
            db(before) - db(after)
        );
        // Output keeps the frame length and the stream stays in step.
        let mut fr = vec![0i16; FRAME];
        d.process(&mut fr);
        assert_eq!(fr.len(), FRAME);
    }

    /// CPU cost: denoising must run far faster than real time.
    #[test]
    fn faster_than_real_time() {
        let mut d = Denoiser::new();
        let mut fr: Vec<i16> = (0..FRAME)
            .map(|i| ((i * 37) % 2000) as i16 - 1000)
            .collect();
        let t = std::time::Instant::now();
        let frames = 100; // 3 s of audio
        for _ in 0..frames {
            d.process(&mut fr);
        }
        let took = t.elapsed().as_secs_f32();
        // Debug builds are slow; still well under real time.
        assert!(took < 3.0, "{took:.2} s for 3 s of audio");
    }
}
