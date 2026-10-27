use nnnoiseless::DenoiseState;

const UP: usize = 3;

const TAPS: usize = 48;

fn lowpass() -> Vec<f32> {
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

    up_hist: Vec<f32>,

    pending: Vec<f32>,

    ready: std::collections::VecDeque<f32>,

    down_hist: Vec<f32>,

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

    pub fn process(&mut self, frame: &mut [i16]) {
        let k = TAPS / UP;
        let mut out = vec![0f32; DenoiseState::FRAME_SIZE];
        for s in frame.iter() {
            self.up_hist.rotate_left(1);
            self.up_hist[k - 1] = *s as f32;

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

        let w = 2.0 * std::f32::consts::PI * 12_000.0 / 48_000.0;
        let (re, im) = h.iter().enumerate().fold((0.0, 0.0), |(r, i), (n, v)| {
            (r + v * (w * n as f32).cos(), i - v * (w * n as f32).sin())
        });
        assert!((re * re + im * im).sqrt() < 0.01);
    }

    #[test]
    fn keeps_a_voice_band_tone_and_cuts_steady_noise() {
        let mut d = Denoiser::new();

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

        let mut fr = vec![0i16; FRAME];
        d.process(&mut fr);
        assert_eq!(fr.len(), FRAME);
    }

    #[test]
    fn faster_than_real_time() {
        let mut d = Denoiser::new();
        let mut fr: Vec<i16> = (0..FRAME)
            .map(|i| ((i * 37) % 2000) as i16 - 1000)
            .collect();
        let t = std::time::Instant::now();
        let frames = 100;
        for _ in 0..frames {
            d.process(&mut fr);
        }
        let took = t.elapsed().as_secs_f32();

        assert!(took < 3.0, "{took:.2} s for 3 s of audio");
    }
}
