//! Kaldi compatible log mel filterbank features (80 bins, 25 ms frames
//! every 10 ms, povey window, pre-emphasis 0.97, no dither, frames
//! centered like `snip_edges = false`), the input the WeSpeaker and
//! 3D-Speaker embedding models were trained on. Matches kaldi-native-fbank
//! as used by sherpa-onnx, so the published ONNX exports work unchanged.

pub const NUM_BINS: usize = 80;
const RATE: f32 = 16_000.0;
const FRAME_LEN: usize = 400; // 25 ms
const FRAME_SHIFT: usize = 160; // 10 ms
const FFT: usize = 512;
const LOW_HZ: f32 = 20.0;
const HIGH_HZ: f32 = 7_600.0; // nyquist - 400

fn mel(f: f32) -> f32 {
    1127.0 * (1.0 + f / 700.0).ln()
}

/// Precomputed window, mel banks and FFT twiddles.
pub struct Fbank {
    window: Vec<f32>,
    /// Per bin: first FFT bin index and its weights.
    banks: Vec<(usize, Vec<f32>)>,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Default for Fbank {
    fn default() -> Self {
        Self::new()
    }
}

impl Fbank {
    pub fn new() -> Fbank {
        let window = (0..FRAME_LEN)
            .map(|i| {
                let a = 2.0 * std::f64::consts::PI * i as f64 / (FRAME_LEN - 1) as f64;
                (0.5 - 0.5 * a.cos()).powf(0.85) as f32
            })
            .collect();
        let n_fft_bins = FFT / 2;
        let bin_w = RATE / FFT as f32;
        let (lo, hi) = (mel(LOW_HZ), mel(HIGH_HZ));
        let delta = (hi - lo) / (NUM_BINS + 1) as f32;
        let banks = (0..NUM_BINS)
            .map(|b| {
                let left = lo + b as f32 * delta;
                let center = left + delta;
                let right = center + delta;
                let mut first = None;
                let mut w = vec![];
                for i in 0..n_fft_bins {
                    let m = mel(bin_w * i as f32);
                    if m > left && m < right {
                        first.get_or_insert(i);
                        w.push(if m <= center {
                            (m - left) / (center - left)
                        } else {
                            (right - m) / (right - center)
                        });
                    } else if first.is_some() {
                        break;
                    }
                }
                (first.unwrap_or(0), w)
            })
            .collect();
        let (cos, sin) = (0..FFT / 2)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / FFT as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .unzip();
        Fbank {
            window,
            banks,
            cos,
            sin,
        }
    }

    /// Number of frames for `n` samples (kaldi, snip_edges = false).
    pub fn num_frames(n: usize) -> usize {
        (n + FRAME_SHIFT / 2) / FRAME_SHIFT
    }

    /// Features for 16 kHz samples already scaled as the model expects
    /// (int16 range for WeSpeaker, -1..1 for others). Row major, frames x 80.
    pub fn compute(&self, samples: &[f32]) -> Vec<f32> {
        let n = samples.len();
        let frames = Self::num_frames(n);
        let mut out = Vec::with_capacity(frames * NUM_BINS);
        if n == 0 {
            return out;
        }
        let mut re = vec![0f32; FFT];
        let mut im = vec![0f32; FFT];
        let mut w = vec![0f32; FRAME_LEN];
        for f in 0..frames {
            let start = (f * FRAME_SHIFT + FRAME_SHIFT / 2) as i64 - (FRAME_LEN / 2) as i64;
            for (i, slot) in w.iter_mut().enumerate() {
                let mut s = start + i as i64;
                // Reflect at the edges.
                while s < 0 || s >= n as i64 {
                    s = if s < 0 { -s - 1 } else { 2 * n as i64 - 1 - s };
                }
                *slot = samples[s as usize];
            }
            let mean = w.iter().sum::<f32>() / FRAME_LEN as f32;
            w.iter_mut().for_each(|x| *x -= mean);
            for i in (1..FRAME_LEN).rev() {
                w[i] -= 0.97 * w[i - 1];
            }
            w[0] -= 0.97 * w[0];
            re.iter_mut().for_each(|x| *x = 0.0);
            im.iter_mut().for_each(|x| *x = 0.0);
            for i in 0..FRAME_LEN {
                re[i] = w[i] * self.window[i];
            }
            self.fft(&mut re, &mut im);
            for (first, weights) in &self.banks {
                let mut e = 0f32;
                for (j, wt) in weights.iter().enumerate() {
                    let k = first + j;
                    e += wt * (re[k] * re[k] + im[k] * im[k]);
                }
                out.push(e.max(f32::EPSILON).ln());
            }
        }
        out
    }

    /// In place iterative radix-2 FFT of size FFT.
    fn fft(&self, re: &mut [f32], im: &mut [f32]) {
        let n = FFT;
        let mut j = 0;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for s in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (c, si) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (s + k, s + k + len / 2);
                    let tr = re[b] * c - im[b] * si;
                    let ti = re[b] * si + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

/// Subtract the per bin mean over frames (WeSpeaker's CMN).
pub fn subtract_mean(feats: &mut [f32], dim: usize) {
    let frames = feats.len() / dim.max(1);
    if frames == 0 {
        return;
    }
    let mut mean = vec![0f32; dim];
    for r in feats.chunks(dim) {
        for (m, v) in mean.iter_mut().zip(r) {
            *m += v;
        }
    }
    mean.iter_mut().for_each(|m| *m /= frames as f32);
    for r in feats.chunks_mut(dim) {
        for (v, m) in r.iter_mut().zip(&mean) {
            *v -= m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_matches_kaldi() {
        assert_eq!(Fbank::num_frames(16_000), 100);
        assert_eq!(Fbank::num_frames(160), 1);
        assert_eq!(Fbank::num_frames(79), 0);
        assert_eq!(Fbank::num_frames(80), 1);
    }

    #[test]
    fn fft_of_a_tone_peaks_at_its_bin() {
        let fb = Fbank::new();
        let mut re: Vec<f32> = (0..FFT)
            .map(|i| (2.0 * std::f32::consts::PI * 32.0 * i as f32 / FFT as f32).cos())
            .collect();
        let mut im = vec![0f32; FFT];
        fb.fft(&mut re, &mut im);
        let mags: Vec<f32> = (0..FFT / 2).map(|k| re[k].hypot(im[k])).collect();
        let peak = mags
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(peak, 32);
        assert!((mags[32] - FFT as f32 / 2.0).abs() < 0.5);
    }

    #[test]
    fn tone_energy_lands_in_the_right_mel_bin() {
        let fb = Fbank::new();
        // 1 kHz tone: mel(1000) = 1000 roughly; find the bin centers.
        let x: Vec<f32> = (0..16_000)
            .map(|i| 8000.0 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / RATE).sin())
            .collect();
        let feats = fb.compute(&x);
        assert_eq!(feats.len(), 100 * NUM_BINS);
        let row = &feats[50 * NUM_BINS..51 * NUM_BINS];
        let best = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        let (lo, hi) = (mel(LOW_HZ), mel(HIGH_HZ));
        let delta = (hi - lo) / (NUM_BINS + 1) as f32;
        let center = lo + (best + 1) as f32 * delta;
        assert!((center - mel(1000.0)).abs() < delta * 1.5, "bin {best}");
        // Silence floors at log(epsilon), no NaN.
        let z = fb.compute(&vec![0.0; 1600]);
        assert!(z.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn mean_subtraction_zeroes_each_bin() {
        let mut f = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        subtract_mean(&mut f, 3);
        assert_eq!(f, vec![-1.5, -1.5, -1.5, 1.5, 1.5, 1.5]);
    }
}
