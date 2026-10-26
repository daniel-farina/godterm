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

/// Loudest playback (dBFS) in the last `window` (None when nothing played):
/// this Mac's speakers, or the browser that is the voice (see WebClock).
pub fn played_db(window: std::time::Duration) -> Option<f32> {
    let now = std::time::Instant::now();
    let local = {
        let p = PLAYED.lock().unwrap_or_else(|e| e.into_inner());
        p.iter()
            .rev()
            .take_while(|(t, _)| now.duration_since(*t) <= window)
            .map(|(_, d)| *d)
            .reduce(f32::max)
    };
    let web = web_played_db(window);
    match (local, web) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// How long a clip that finds the browser idle takes to start playing
/// there (the network, then the page schedules it 30 ms ahead).
pub const WEB_LEAD: std::time::Duration = std::time::Duration::from_millis(80);
/// One level per this much of a clip.
const WEB_BLOCK_MS: u64 = 20;

/// The speech a browser plays, as GodTerm can know it: the page plays the
/// clips back to back in the order they come, the next one at the end of
/// the one before, or at once when it comes after a gap. So each clip's
/// levels go on a timeline from its expected start: the echo reference
/// for that browser's mic (as PLAYED is for this Mac's speakers), so the
/// echo gate learns how much of the reply comes back, and the expected
/// end of what it plays (a reply with gaps ends later than its length).
#[derive(Debug, Default)]
pub struct WebClock {
    /// (when it plays, dBFS), in time order.
    levels: VecDeque<(std::time::Instant, f32)>,
    end: Option<std::time::Instant>,
}

impl WebClock {
    /// A clip handed to the browser at `now`; returns when it ends there.
    pub fn clip(
        &mut self,
        samples: &[f32],
        rate: u32,
        now: std::time::Instant,
    ) -> std::time::Instant {
        let start = match self.end {
            Some(e) if e > now => e,
            _ => now + WEB_LEAD,
        };
        if samples.is_empty() || rate == 0 {
            return self.end.unwrap_or(now);
        }
        let block = (rate as u64 * WEB_BLOCK_MS / 1000).max(1) as usize;
        for (i, c) in samples.chunks(block).enumerate() {
            let rms = (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt();
            let db = if rms <= 1e-5 {
                -100.0
            } else {
                20.0 * rms.log10()
            };
            let at = start + std::time::Duration::from_millis(i as u64 * WEB_BLOCK_MS);
            self.levels.push_back((at, db));
        }
        let end = start + std::time::Duration::from_secs_f64(samples.len() as f64 / rate as f64);
        self.end = Some(end);
        // What played more than a few seconds ago is no reference any more.
        let old = now
            .checked_sub(std::time::Duration::from_secs(3))
            .unwrap_or(now);
        while self.levels.front().is_some_and(|(t, _)| *t < old) {
            self.levels.pop_front();
        }
        end
    }

    /// The reply was cut off: the page dropped what it had queued.
    pub fn stop(&mut self, now: std::time::Instant) {
        while self.levels.back().is_some_and(|(t, _)| *t > now) {
            self.levels.pop_back();
        }
        self.end = None;
    }

    /// Loudest level playing in the `window` up to `now`.
    pub fn played_db(&self, window: std::time::Duration, now: std::time::Instant) -> Option<f32> {
        self.levels
            .iter()
            .rev()
            .skip_while(|(t, _)| *t > now)
            .take_while(|(t, _)| now.saturating_duration_since(*t) <= window)
            .map(|(_, d)| *d)
            .reduce(f32::max)
    }

    /// When the browser is expected to finish what it was sent.
    pub fn end(&self) -> Option<std::time::Instant> {
        self.end
    }
}

static WEB_CLOCK: Mutex<WebClock> = Mutex::new(WebClock {
    levels: VecDeque::new(),
    end: None,
});

fn web_clock() -> std::sync::MutexGuard<'static, WebClock> {
    WEB_CLOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A clip went to the browser that is the voice.
pub fn web_clip(samples: &[f32], rate: u32) {
    web_clock().clip(samples, rate, std::time::Instant::now());
}

/// The browser was told to stop (or the voice left it).
pub fn web_stop() {
    web_clock().stop(std::time::Instant::now());
}

/// The browser's playback (dBFS) in the last `window`.
pub fn web_played_db(window: std::time::Duration) -> Option<f32> {
    web_clock().played_db(window, std::time::Instant::now())
}

/// When the browser should be done playing what it was sent.
pub fn web_end() -> Option<std::time::Instant> {
    web_clock().end()
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

/// Output volume factor on top of `push`'s volume (f32 bits): lowered
/// briefly while a barge-in is decided, then restored.
static DUCK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

/// Duck the output to `v` (0 to 1; 1 is full volume).
pub fn set_duck(v: f32) {
    DUCK.store(
        v.clamp(0.0, 1.0).to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
}

pub fn duck() -> f32 {
    f32::from_bits(DUCK.load(std::sync::atomic::Ordering::Relaxed))
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
    // A zero rate (a damaged WAV header) would divide by zero.
    if from == to || input.is_empty() || from == 0 || to == 0 {
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
    let gain = duck();
    for frame in out.chunks_mut(channels.max(1)) {
        let x = q.pop_front().unwrap_or(0.0) * gain;
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
    use std::time::{Duration, Instant};

    fn tone(secs: f32, amp: f32, rate: u32) -> Vec<f32> {
        (0..(secs * rate as f32) as usize)
            .map(|i| (i as f32 * 0.1).sin() * amp)
            .collect()
    }

    #[test]
    fn web_clock_plays_clips_back_to_back() {
        let mut c = WebClock::default();
        let t0 = Instant::now();
        // The first clip starts a moment after it is sent.
        let e1 = c.clip(&tone(1.0, 0.5, 24_000), 24_000, t0);
        assert_eq!(e1, t0 + WEB_LEAD + Duration::from_secs(1));
        // One sent while that plays starts at its end, not when it came.
        let e2 = c.clip(
            &tone(0.5, 0.5, 24_000),
            24_000,
            t0 + Duration::from_millis(100),
        );
        assert_eq!(e2, e1 + Duration::from_millis(500));
        // A clip after a gap starts when it comes: the reply ends later
        // than its length.
        let late = e2 + Duration::from_millis(700);
        let e3 = c.clip(&tone(0.5, 0.5, 24_000), 24_000, late);
        assert_eq!(e3, late + WEB_LEAD + Duration::from_millis(500));
        assert_eq!(c.end(), Some(e3));
    }

    #[test]
    fn web_clock_level_is_the_reference_while_it_plays() {
        let mut c = WebClock::default();
        let t0 = Instant::now();
        c.clip(&tone(1.0, 0.5, 16_000), 16_000, t0);
        let w = Duration::from_millis(400);
        // Nothing plays before its start.
        assert_eq!(c.played_db(w, t0), None);
        let mid = c
            .played_db(w, t0 + Duration::from_millis(500))
            .expect("a level while it plays");
        // A sine at 0.5 is about -9 dBFS RMS.
        assert!((mid - -9.0).abs() < 1.0, "{mid}");
        // A little after the end it still counts (the room's tail), then
        // not.
        assert!(c.played_db(w, t0 + Duration::from_millis(1300)).is_some());
        assert_eq!(c.played_db(w, t0 + Duration::from_millis(1600)), None);
    }

    #[test]
    fn web_clock_stop_drops_what_was_queued() {
        let mut c = WebClock::default();
        let t0 = Instant::now();
        c.clip(&tone(3.0, 0.5, 16_000), 16_000, t0);
        let cut = t0 + Duration::from_millis(500);
        c.stop(cut);
        assert_eq!(c.end(), None);
        let w = Duration::from_millis(400);
        assert!(c.played_db(w, cut).is_some());
        assert_eq!(c.played_db(w, cut + Duration::from_millis(1000)), None);
        // The next reply starts fresh, a moment after it is sent.
        let t1 = cut + Duration::from_millis(10);
        assert_eq!(
            c.clip(&tone(0.1, 0.5, 16_000), 16_000, t1),
            t1 + WEB_LEAD + Duration::from_millis(100)
        );
    }

    #[test]
    fn web_clock_keeps_only_recent_levels() {
        let mut c = WebClock::default();
        let t0 = Instant::now();
        for i in 0..20 {
            c.clip(
                &tone(1.0, 0.3, 8_000),
                8_000,
                t0 + Duration::from_secs(i * 2),
            );
        }
        // 50 levels a second, only the last few seconds kept.
        assert!(c.levels.len() <= 50 * 5, "{}", c.levels.len());
        // Empty clips and a zero rate add nothing.
        let n = c.levels.len();
        c.clip(&[], 16_000, t0 + Duration::from_secs(41));
        c.clip(&[0.1; 10], 0, t0 + Duration::from_secs(41));
        assert_eq!(c.levels.len(), n);
    }

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
