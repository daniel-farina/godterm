//! Audio output through CoreAudio (cpal): a queue of samples that the
//! output callback drains. Pushing more while it plays gives gapless
//! sentence streaming; `clear` stops at once.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// What was just played: (when, RMS of that output block, dBFS). The mic
/// side compares against it so our own voice never counts as the user
/// talking (a reference for echo gating).
pub static PLAYED: Mutex<VecDeque<(std::time::Instant, f32)>> = Mutex::new(VecDeque::new());

/// Loudest playback (dBFS) in the last `window` (None when nothing played).
pub fn played_db(window: std::time::Duration) -> Option<f32> {
    let now = std::time::Instant::now();
    let p = PLAYED.lock().unwrap_or_else(|e| e.into_inner());
    p.iter()
        .rev()
        .take_while(|(t, _)| now.duration_since(*t) <= window)
        .map(|(_, d)| *d)
        .reduce(f32::max)
}

/// Record one output block's level (also used by tests to simulate playback).
pub fn note_played(rms: f32) {
    let db = if rms <= 1e-5 {
        -100.0
    } else {
        20.0 * rms.log10()
    };
    let mut p = PLAYED.lock().unwrap_or_else(|e| e.into_inner());
    p.push_back((std::time::Instant::now(), db));
    while p.len() > 400 {
        p.pop_front();
    }
}

/// When the last flush emptied the queue (barge-in timing).
pub static FLUSHED_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);

pub struct Player {
    queue: Arc<Mutex<VecDeque<f32>>>,
    _stream: cpal::Stream,
    rate: u32,
    pub device: String,
}

/// Linear resampling, enough for speech going from 24 kHz to 44.1 or 48.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let n = (input.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    (0..n)
        .map(|i| {
            let x = i as f64 * step;
            let j = x as usize;
            let f = (x - j as f64) as f32;
            let a = input[j.min(input.len() - 1)];
            let b = input[(j + 1).min(input.len() - 1)];
            a + (b - a) * f
        })
        .collect()
}

/// No audio devices at all: unit tests (many run at once, and WASAPI on a
/// runner without a sound card crashes when enumerated concurrently) and
/// GODTERM_NO_MIC / GODTERM_NO_AUDIO.
pub fn devices_off() -> bool {
    cfg!(test)
        || crate::config::env_var("NO_MIC").is_some()
        || crate::config::env_var("NO_AUDIO").is_some()
}

/// Output device names (for Settings).
pub fn output_devices() -> Vec<String> {
    if devices_off() {
        return vec![];
    }
    let host = cpal::default_host();
    host.output_devices()
        .map(|d| d.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

impl Player {
    /// Open `device` (a name, or "default").
    /// `flush`: set it and the output callback drops what is queued at
    /// once (barge-in, "stop").
    pub fn open(device: &str, flush: Arc<std::sync::atomic::AtomicBool>) -> Result<Player> {
        if devices_off() {
            return Err(anyhow!("audio devices are off (tests, GODTERM_NO_AUDIO)"));
        }
        let host = cpal::default_host();
        let dev = if device.is_empty() || device == "default" {
            host.default_output_device()
        } else {
            host.output_devices()
                .ok()
                .and_then(|mut it| it.find(|d| d.name().is_ok_and(|n| n == device)))
                .or_else(|| host.default_output_device())
        }
        .ok_or_else(|| anyhow!("no audio output device"))?;
        let name = dev.name().unwrap_or_default();
        let cfg = dev.default_output_config().map_err(|e| anyhow!("{e}"))?;
        let rate = cfg.sample_rate().0;
        let channels = cfg.channels() as usize;
        let queue: Arc<Mutex<VecDeque<f32>>> = Default::default();
        let q = Arc::clone(&queue);
        let (f1, f2) = (Arc::clone(&flush), flush);
        let err = |e| crate::log::info(&format!("audio output: {e}"));
        let sc: cpal::StreamConfig = cfg.clone().into();
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => dev.build_output_stream(
                &sc,
                move |out: &mut [f32], _| fill(out, channels, &q, &f1, |s| s),
                err,
                None,
            ),
            cpal::SampleFormat::I16 => dev.build_output_stream(
                &sc,
                move |out: &mut [i16], _| fill(out, channels, &q, &f2, |s| (s * 32767.0) as i16),
                err,
                None,
            ),
            f => return Err(anyhow!("unsupported output format {f:?}")),
        }
        .map_err(|e| anyhow!("{e}"))?;
        stream.play().map_err(|e| anyhow!("{e}"))?;
        Ok(Player {
            queue,
            _stream: stream,
            rate,
            device: name,
        })
    }

    /// Queue 24 kHz mono samples at `volume` (0 to 1).
    pub fn push(&self, samples: &[f32], from_rate: u32, volume: f32) {
        let s = resample(samples, from_rate, self.rate);
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.extend(s.into_iter().map(|v| v * volume));
    }

    /// Seconds of audio still queued.
    pub fn pending_s(&self) -> f32 {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).len() as f32 / self.rate as f32
    }

    pub fn clear(&self) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

fn fill<T: Copy>(
    out: &mut [T],
    channels: usize,
    q: &Mutex<VecDeque<f32>>,
    flush: &std::sync::atomic::AtomicBool,
    conv: impl Fn(f32) -> T,
) {
    let mut q = q.lock().unwrap_or_else(|e| e.into_inner());
    if flush.load(std::sync::atomic::Ordering::SeqCst) && !q.is_empty() {
        q.clear();
        *FLUSHED_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    }
    let mut sq = 0.0f32;
    let mut n = 0usize;
    for frame in out.chunks_mut(channels.max(1)) {
        let x = q.pop_front().unwrap_or(0.0);
        sq += x * x;
        n += 1;
        let v = conv(x);
        for s in frame {
            *s = v;
        }
    }
    if n > 0 {
        note_played((sq / n as f32).sqrt());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling() {
        let x: Vec<f32> = (0..240).map(|i| i as f32).collect();
        let y = resample(&x, 24_000, 48_000);
        assert_eq!(y.len(), 480);
        assert_eq!(y[0], 0.0);
        assert!((y[1] - 0.5).abs() < 1e-6);
        assert!((y[200] - 100.0).abs() < 1e-4);
        assert_eq!(resample(&x, 24_000, 24_000), x);
        assert_eq!(resample(&x, 24_000, 44_100).len(), 441);
        assert!(resample(&[], 24_000, 48_000).is_empty());
    }
}
