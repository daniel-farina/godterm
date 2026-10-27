use std::io::Read;
use std::process::{Child, Command, Stdio};

pub const RATE: u32 = 16_000;

pub const FRAME: usize = 480;

pub const FRAME_MS: u32 = 30;

#[derive(Debug, Clone)]
pub struct VadCfg {
    pub threshold: f32,
    pub min_rms: f32,
    pub end_silence_ms: u32,
    pub min_speech_ms: u32,
    pub max_utterance_s: u32,

    pub preroll_frames: usize,

    pub fixed_floor: Option<f32>,
}

impl Default for VadCfg {
    fn default() -> Self {
        VadCfg {
            threshold: 3.0,
            min_rms: 300.0,
            end_silence_ms: 700,
            min_speech_ms: 250,
            max_utterance_s: 15,
            preroll_frames: 8,
            fixed_floor: None,
        }
    }
}

pub fn gain_factor(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn apply_gain(frame: &mut [i16], db: f32) {
    if db == 0.0 {
        return;
    }
    let g = gain_factor(db);
    for s in frame {
        *s = (*s as f32 * g).clamp(-32768.0, 32767.0) as i16;
    }
}

pub fn rms_of_db(db: f32) -> f32 {
    32768.0 * 10f32.powf(db / 20.0)
}

pub fn db(rms: f32) -> f32 {
    if rms <= 0.0 {
        return -90.0;
    }
    (20.0 * (rms / 32768.0).log10()).max(-90.0)
}

pub fn rms(frame: &[i16]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f64 = frame.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / frame.len() as f64).sqrt() as f32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    Start,

    None,
}

pub struct Endpointer {
    cfg: VadCfg,
    floor: f32,
    seen: usize,
    in_speech: bool,
    speech_frames: usize,
    silence_frames: usize,
    buf: Vec<i16>,
    preroll: std::collections::VecDeque<Vec<i16>>,

    calib: Vec<f32>,
    calib_frames: Vec<Vec<i16>>,

    pub forced: bool,

    pub hold_ms: u32,
}

const CALIB_FRAMES: usize = 16;

impl Endpointer {
    pub fn new(cfg: VadCfg) -> Endpointer {
        Endpointer {
            floor: cfg.fixed_floor.unwrap_or(0.0),
            cfg,
            seen: 0,
            in_speech: false,
            speech_frames: 0,
            silence_frames: 0,
            buf: vec![],
            preroll: Default::default(),
            calib: vec![],
            calib_frames: vec![],
            forced: false,
            hold_ms: 0,
        }
    }

    pub fn reset(&mut self) {
        self.in_speech = false;
        self.speech_frames = 0;
        self.silence_frames = 0;
        self.buf.clear();
        self.preroll.clear();
        self.hold_ms = 0;
    }

    fn ms(frames: usize) -> u32 {
        (frames * FRAME * 1000 / RATE as usize) as u32
    }

    pub fn floor(&self) -> f32 {
        self.floor
    }

    pub fn calibrated(&self) -> bool {
        self.cfg.fixed_floor.is_some() || self.calib.len() >= CALIB_FRAMES
    }

    pub fn trailing_silence_ms(&self) -> u32 {
        Self::ms(self.silence_frames)
    }

    pub fn snapshot(&self) -> Option<&[i16]> {
        self.in_speech.then_some(self.buf.as_slice())
    }

    pub fn threshold(&self) -> f32 {
        (self.floor * self.cfg.threshold).max(self.cfg.min_rms)
    }

    pub fn push(&mut self, frame: &[i16]) -> (VadEvent, Option<Vec<i16>>) {
        let e = rms(frame);
        self.seen += 1;
        if !self.calibrated() && !self.forced && !self.in_speech {
            self.calib.push(e);
            self.calib_frames.push(frame.to_vec());
            if self.calib.len() < CALIB_FRAMES {
                return (VadEvent::None, None);
            }
            let mut v = self.calib.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            self.floor = v[v.len() / 5];
            let thr = self.threshold();
            let frames = std::mem::take(&mut self.calib_frames);
            let loud_n = self.calib.iter().filter(|r| **r > thr).count();
            if loud_n >= 2 {
                self.in_speech = true;
                self.speech_frames = loud_n;
                self.silence_frames = self.calib.iter().rev().take_while(|r| **r <= thr).count();
                self.buf = frames.into_iter().flatten().collect();
                return (VadEvent::Start, None);
            }
            for f in frames.into_iter().rev().take(self.cfg.preroll_frames).rev() {
                self.preroll.push_back(f);
            }
            return (VadEvent::None, None);
        }
        let loud = e > self.threshold();
        if !self.in_speech {
            if !loud && self.cfg.fixed_floor.is_none() {
                self.floor = self.floor * 0.97 + e * 0.03;
            }
            self.preroll.push_back(frame.to_vec());
            if self.preroll.len() > self.cfg.preroll_frames.max(1) {
                self.preroll.pop_front();
            }
            if loud || self.forced {
                self.in_speech = true;
                self.speech_frames = usize::from(loud);
                self.silence_frames = 0;
                self.buf = self.preroll.drain(..).flatten().collect();
                return (VadEvent::Start, None);
            }
            return (VadEvent::None, None);
        }
        self.buf.extend_from_slice(frame);
        if loud {
            self.speech_frames += 1;
            self.silence_frames = 0;
        } else {
            self.silence_frames += 1;
        }
        let too_long = self.buf.len() >= (self.cfg.max_utterance_s * RATE) as usize;

        let wait = if self.forced && self.speech_frames == 0 {
            5000
        } else {
            self.cfg.end_silence_ms + self.hold_ms
        };
        let ended = Self::ms(self.silence_frames) >= wait;
        if ended || too_long {
            let enough = Self::ms(self.speech_frames) >= self.cfg.min_speech_ms;
            let mut out = std::mem::take(&mut self.buf);

            if ended {
                let silence = self.silence_frames * FRAME;
                let keep = (RATE as usize * 3 / 10).min(silence);
                out.truncate(out.len().saturating_sub(silence - keep));
            }
            self.in_speech = false;
            self.forced = false;
            self.speech_frames = 0;
            self.silence_frames = 0;
            self.hold_ms = 0;
            return (VadEvent::None, enough.then_some(out));
        }
        (VadEvent::None, None)
    }

    pub fn flush(&mut self) -> Option<Vec<i16>> {
        let enough = Self::ms(self.speech_frames) >= self.cfg.min_speech_ms;
        let out = std::mem::take(&mut self.buf);
        self.reset();
        (enough && !out.is_empty()).then_some(out)
    }
}

pub fn normalize(samples: &mut [i16], target_db: f32, max_gain_db: f32) -> f32 {
    let frames: Vec<f32> = samples.chunks(FRAME).map(rms).collect();
    let loudest = frames.iter().cloned().fold(0.0f32, f32::max);
    if loudest <= 1.0 {
        return 0.0;
    }
    let speech: Vec<f32> = frames
        .iter()
        .cloned()
        .filter(|r| db(*r) > db(loudest) - 25.0)
        .collect();
    let mean_sq = speech.iter().map(|r| r * r).sum::<f32>() / speech.len().max(1) as f32;
    let level = db(mean_sq.sqrt());
    let peak = samples
        .iter()
        .map(|s| (*s as f32).abs())
        .fold(0.0f32, f32::max)
        .max(1.0);
    let headroom = db(32767.0 * 0.891) - db(peak);
    let gain = (target_db - level)
        .clamp(-12.0, max_gain_db)
        .min(headroom.max(0.0));
    if gain.abs() < 0.5 {
        return 0.0;
    }
    let g = gain_factor(gain);
    for s in samples.iter_mut() {
        *s = (*s as f32 * g).round().clamp(-32768.0, 32767.0) as i16;
    }
    gain
}

pub fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[derive(Debug, Clone)]
pub enum Source {
    Mic(String),

    File { path: String, realtime: bool },

    Web(std::sync::Arc<WebMic>),
}

#[derive(Debug, Default)]
pub struct WebMic {
    q: std::sync::Mutex<WebQueue>,
    cv: std::sync::Condvar,

    playing: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Default)]
struct WebQueue {
    pcm: std::collections::VecDeque<i16>,

    open: bool,

    early: Option<std::time::Instant>,

    stats: MicStats,
    second: MicStats,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct MicStats {
    pub frames: u64,
    pub samples: u64,

    pub nonzero: u64,
    sumsq: f64,
    pub peak: i32,

    pub dropped: u64,
}

impl MicStats {
    fn add(&mut self, samples: impl Iterator<Item = i16>) {
        self.frames += 1;
        for s in samples {
            self.samples += 1;
            if s != 0 {
                self.nonzero += 1;
            }
            self.sumsq += f64::from(s) * f64::from(s);
            self.peak = self.peak.max(i32::from(s).abs());
        }
    }

    pub fn db(&self) -> f32 {
        if self.samples == 0 {
            return -90.0;
        }
        db((self.sumsq / self.samples as f64).sqrt() as f32)
    }

    pub fn peak_db(&self) -> f32 {
        db(self.peak as f32)
    }

    pub fn ms(&self) -> u64 {
        self.samples * 1000 / RATE as u64
    }

    pub fn line(&self) -> String {
        format!(
            "{} frames, {} ms of audio, level {:.0} dBFS (peak {:.0}), {}% non zero{}",
            self.frames,
            self.ms(),
            self.db(),
            self.peak_db(),
            (self.nonzero * 100).checked_div(self.samples).unwrap_or(0),
            if self.dropped > 0 {
                format!(
                    ", {} ms dropped (nothing listening)",
                    self.dropped * 1000 / RATE as u64
                )
            } else {
                String::new()
            }
        )
    }
}

pub static VOICE_DEBUG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn voice_debug() -> bool {
    VOICE_DEBUG.load(std::sync::atomic::Ordering::Relaxed)
        || crate::config::env_var("VOICE_DEBUG").is_some_and(|v| v != "0")
}

const WEB_MAX: usize = RATE as usize * 10;

const WEB_EARLY: std::time::Duration = std::time::Duration::from_secs(1);
const WEB_EARLY_MAX: usize = RATE as usize * 3;

impl WebMic {
    pub fn new() -> std::sync::Arc<WebMic> {
        std::sync::Arc::new(WebMic::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WebQueue> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, bytes: &[u8]) {
        let mut q = self.lock();
        let samples = bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]));
        q.stats.add(samples.clone());
        q.second.add(samples.clone());
        q.pcm.extend(samples);
        let max = if q.open {
            WEB_MAX
        } else {
            q.early = Some(std::time::Instant::now());
            WEB_EARLY_MAX
        };
        let over = q.pcm.len().saturating_sub(max);
        q.pcm.drain(..over);
        if !q.open {
            q.stats.dropped += over as u64;
            q.second.dropped += over as u64;
        }
        self.cv.notify_one();
    }

    pub fn set_playing(&self, on: bool) {
        self.playing.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn playing(&self) -> bool {
        self.playing.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn take_stats(&self) -> MicStats {
        std::mem::take(&mut self.lock().stats)
    }

    pub fn take_second(&self) -> (MicStats, bool) {
        let mut q = self.lock();
        (std::mem::take(&mut q.second), q.open)
    }

    fn set_open(&self, on: bool) {
        let mut q = self.lock();
        let fresh = q.early.is_some_and(|t| t.elapsed() < WEB_EARLY);
        if !(on && fresh) {
            let n = q.pcm.len() as u64;
            if !q.open {
                q.stats.dropped += n;
                q.second.dropped += n;
            }
            q.pcm.clear();
        }
        q.open = on;
        q.early = None;
    }

    fn queued(&self) -> usize {
        self.lock().pcm.len()
    }

    fn frame(&self, out: &mut Vec<i16>) -> Option<()> {
        let wait = std::time::Duration::from_millis(250);
        let q = self.lock();
        let (mut q, _) = self
            .cv
            .wait_timeout_while(q, wait, |q| q.pcm.len() < FRAME)
            .unwrap_or_else(|e| e.into_inner());
        out.clear();
        let n = q.pcm.len().min(FRAME);
        out.extend(q.pcm.drain(..n));
        out.resize(FRAME, 0);
        Some(())
    }
}

pub struct Capture {
    pub(super) child: Option<Child>,
    pub(super) web: Option<std::sync::Arc<WebMic>>,

    pub(super) err: Option<std::sync::Arc<std::sync::Mutex<String>>>,

    pub vp: bool,
}

pub fn mic_input_args(dev: &str) -> Vec<String> {
    if cfg!(target_os = "macos") {
        return vec![
            "-f".into(),
            "avfoundation".into(),
            "-i".into(),
            format!(":{dev}"),
        ];
    }
    match dev.strip_prefix("alsa:") {
        Some(d) => vec!["-f".into(), "alsa".into(), "-i".into(), d.to_string()],
        None => vec!["-f".into(), "pulse".into(), "-i".into(), dev.to_string()],
    }
}

impl Capture {
    pub fn start(ffmpeg: &str, src: &Source) -> std::io::Result<Capture> {
        if matches!(src, Source::Mic(_))
            && (cfg!(test) || crate::config::env_var("NO_MIC").is_some())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the microphone is disabled (GODTERM_NO_MIC)",
            ));
        }
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        match src {
            Source::Mic(dev) => {
                let dev = if dev.is_empty() {
                    "default"
                } else {
                    dev.as_str()
                };
                cmd.args(mic_input_args(dev));
            }
            Source::File { path, realtime } => {
                if *realtime {
                    cmd.arg("-re");
                }
                cmd.args(["-i", path]);
            }
            Source::Web(mic) => return Ok(Capture::web(mic)),
        }

        cmd.args([
            "-af",
            "aresample=resampler=swr:filter_size=64:phase_shift=10:cutoff=0.97:linear_interp=1",
        ]);
        cmd.args(["-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"]);
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        super::track(child.id());
        Ok(Capture {
            child: Some(child),
            web: None,
            err: None,
            vp: false,
        })
    }

    pub fn web(mic: &std::sync::Arc<WebMic>) -> Capture {
        mic.set_open(true);
        Capture {
            child: None,
            web: Some(std::sync::Arc::clone(mic)),
            err: None,
            vp: true,
        }
    }

    pub fn web_playing(&self) -> bool {
        self.web.as_ref().is_some_and(|w| w.playing())
    }

    pub fn queued(&self) -> usize {
        self.web.as_ref().map_or(0, |w| w.queued())
    }

    pub fn frame(&mut self, out: &mut Vec<i16>) -> Option<()> {
        if let Some(w) = &self.web {
            return w.frame(out);
        }
        let stdout = self.child.as_mut()?.stdout.as_mut()?;
        let mut bytes = [0u8; FRAME * 2];
        let mut got = 0;
        while got < bytes.len() {
            match stdout.read(&mut bytes[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        if got < 2 {
            return None;
        }
        out.clear();
        out.extend(
            bytes[..got - got % 2]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]])),
        );
        Some(())
    }

    pub fn error_text(&mut self) -> String {
        if self.web.is_some() {
            return "the browser stopped sending audio".into();
        }
        if let Some(c) = self.child.as_mut() {
            let _ = c.wait();
        }
        if let Some(e) = &self.err {
            return e.lock().unwrap_or_else(|e| e.into_inner()).clone();
        }
        let mut s = String::new();
        if let Some(e) = self.child.as_mut().and_then(|c| c.stderr.as_mut()) {
            let _ = e.read_to_string(&mut s);
        }
        s.lines().last().unwrap_or("").trim().to_string()
    }

    pub fn stop(&mut self) {
        if let Some(w) = &self.web {
            w.set_open(false);
        }
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
            super::untrack(c.id());
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gate {
    Feed,

    Mute,

    BargeIn,
}

#[derive(Debug, Clone)]
pub struct EchoGate {
    pub barge_in: bool,

    pub margin_db: f32,

    pub need_ms: u32,
    recent: std::collections::VecDeque<bool>,

    pub coupling_db: f32,

    pub aec: bool,

    pub noise_bar_db: f32,

    loud_c: Option<f32>,
    trigger_c: Option<f32>,
}

pub const REF_MARGIN_DB: f32 = 9.0;

pub const REF_MARGIN_AEC_DB: f32 = 5.0;

impl EchoGate {
    pub fn new(barge_in: bool, margin_db: f32) -> EchoGate {
        EchoGate {
            barge_in,
            margin_db,
            need_ms: 210,
            recent: Default::default(),
            coupling_db: -12.0,
            aec: false,
            noise_bar_db: -100.0,
            loud_c: None,
            trigger_c: None,
        }
    }

    pub fn set_aec(&mut self, on: bool) {
        if on != self.aec {
            self.coupling_db = if on { -25.0 } else { -12.0 };
        }
        self.aec = on;
    }

    pub fn echo_heard(&mut self) {
        if let Some(c) = self.trigger_c.take() {
            self.coupling_db = self.coupling_db.max(c);
        }
    }

    pub fn push(
        &mut self,
        level_db: f32,
        threshold_db: f32,
        speaking: bool,
        played_db: Option<f32>,
    ) -> Gate {
        if !speaking {
            self.recent.clear();
            self.loud_c = None;
            return Gate::Feed;
        }
        let (ref_margin, margin) = if self.aec {
            (REF_MARGIN_AEC_DB, (self.margin_db - 6.0).max(6.0))
        } else {
            (REF_MARGIN_DB, self.margin_db)
        };
        let bar = match played_db.filter(|p| *p > -80.0) {
            Some(p) => (p + self.coupling_db + ref_margin).max(threshold_db + 3.0),
            None => threshold_db + margin,
        }
        .max(self.noise_bar_db);
        let loud = self.barge_in && level_db > bar;

        self.recent.push_back(loud);
        while self.recent.len() > (300 / FRAME_MS) as usize {
            self.recent.pop_front();
        }
        if loud {
            if let Some(p) = played_db.filter(|p| *p > -60.0) {
                let c = (level_db - p).clamp(-50.0, 6.0);
                self.loud_c = Some(self.loud_c.map_or(c, |m| m.max(c)));
            }
        }
        if loud && self.recent.iter().filter(|l| **l).count() as u32 * FRAME_MS >= self.need_ms {
            self.recent.clear();
            self.trigger_c = self.loud_c.take();
            return Gate::BargeIn;
        }
        if !loud {
            if let Some(p) = played_db.filter(|p| *p > -60.0) {
                let c = (level_db - p).clamp(-50.0, 6.0);
                let rate = if c > self.coupling_db { 0.15 } else { 0.02 };
                self.coupling_db += (c - self.coupling_db) * rate;
            }
        }
        Gate::Mute
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_echo_heard_as_our_own_words_is_learned() {
        let mut g = EchoGate::new(true, 15.0);
        g.set_aec(true);
        let trig = (0..20).position(|_| g.push(-30.0, -45.0, true, Some(-20.0)) == Gate::BargeIn);
        assert!(trig.is_some(), "the strong echo triggers at first");

        g.echo_heard();
        assert!((g.coupling_db - -10.0).abs() < 0.01, "{}", g.coupling_db);
        assert!(
            (0..40).all(|_| g.push(-30.0, -45.0, true, Some(-20.0)) == Gate::Mute),
            "the same echo no longer triggers"
        );

        g.echo_heard();
        assert!((g.coupling_db - -10.0).abs() < 1.0, "{}", g.coupling_db);

        assert!((0..20).any(|_| g.push(-20.0, -45.0, true, Some(-20.0)) == Gate::BargeIn));
    }

    #[test]
    fn mic_input_per_platform() {
        let a = mic_input_args("default");
        if cfg!(target_os = "macos") {
            assert_eq!(a, ["-f", "avfoundation", "-i", ":default"]);
        } else {
            assert_eq!(a, ["-f", "pulse", "-i", "default"]);
            assert_eq!(mic_input_args("alsa:hw:1"), ["-f", "alsa", "-i", "hw:1"]);
        }
    }

    #[test]
    fn tests_never_open_the_mic() {
        let e = Capture::start("/bin/echo", &Source::Mic(String::new()))
            .err()
            .expect("refused");
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
    }

    use super::*;

    #[test]
    fn normalizes_toward_minus_20() {
        let mut v: Vec<i16> = (0..16000)
            .map(|i| ((i as f32 * 0.05).sin() * 460.0) as i16)
            .collect();
        let g = normalize(&mut v, -20.0, 24.0);
        let level =
            db((v.iter().map(|s| (*s as f32).powi(2)).sum::<f32>() / v.len() as f32).sqrt());
        assert!(
            (g - 20.0).abs() < 1.5 && (level + 20.0).abs() < 1.5,
            "gain {g} level {level}"
        );

        let mut loud: Vec<i16> = (0..16000)
            .map(|i| ((i as f32 * 0.05).sin() * 30000.0) as i16)
            .collect();
        normalize(&mut loud, -20.0, 24.0);
        assert!(loud.iter().all(|s| s.unsigned_abs() <= 32767));

        let mut z = vec![0i16; 1600];
        assert_eq!(normalize(&mut z, -20.0, 24.0), 0.0);
    }

    fn tone(frames: usize, amp: f32) -> Vec<Vec<i16>> {
        (0..frames)
            .map(|f| {
                (0..FRAME)
                    .map(|i| {
                        let t = (f * FRAME + i) as f32 / RATE as f32;
                        (amp * (t * 440.0 * std::f32::consts::TAU).sin()) as i16
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn cuts_one_utterance() {
        let mut ep = Endpointer::new(VadCfg::default());
        let mut frames = tone(20, 20.0);
        frames.extend(tone(30, 8000.0));
        frames.extend(tone(40, 20.0));
        let mut started = 0;
        let mut utts = vec![];
        for f in &frames {
            let (ev, u) = ep.push(f);
            if ev == VadEvent::Start {
                started += 1;
            }
            if let Some(u) = u {
                utts.push(u);
            }
        }
        assert_eq!(started, 1);
        assert_eq!(utts.len(), 1);

        assert!(utts[0].len() >= 30 * FRAME);
    }

    #[test]
    fn a_held_end_waits_longer() {
        let end_after = |hold: u32| -> Option<usize> {
            let mut ep = Endpointer::new(VadCfg {
                end_silence_ms: 600,
                ..VadCfg::default()
            });
            for f in tone(20, 20.0) {
                ep.push(&f);
            }
            for f in tone(30, 8000.0) {
                ep.push(&f);
            }
            ep.hold_ms = hold;
            for (i, f) in tone(120, 20.0).iter().enumerate() {
                if ep.push(f).1.is_some() {
                    return Some(i + 1);
                }
            }
            None
        };
        let plain = end_after(0).unwrap();
        assert_eq!(plain, 20, "600 ms of 30 ms frames");
        let held = end_after(1500).unwrap();
        assert_eq!(held, 70, "600 + 1500 ms");

        assert!(plain * 30 < 1000 && held * 30 > 1000);
    }

    #[test]
    fn ignores_clicks_and_flushes() {
        let mut ep = Endpointer::new(VadCfg::default());
        let mut frames = tone(15, 10.0);
        frames.extend(tone(2, 9000.0));
        frames.extend(tone(40, 10.0));
        let n: usize = frames.iter().filter_map(|f| ep.push(f).1).count();
        assert_eq!(n, 0);

        for f in tone(20, 9000.0) {
            ep.push(&f);
        }
        assert!(ep.flush().is_some());
    }

    #[test]
    fn calibrates_noise_floor_from_startup() {
        let mut ep = Endpointer::new(VadCfg {
            min_rms: 50.0,
            ..VadCfg::default()
        });

        let mut frames = tone(7, 400.0);
        frames.extend(tone(1, 20000.0));
        frames.extend(tone(8, 400.0));
        for f in &frames {
            assert_eq!(ep.push(f), (VadEvent::None, None));
        }
        assert!(ep.calibrated());

        assert!((200.0..400.0).contains(&ep.floor()), "{}", ep.floor());

        for f in tone(20, 400.0) {
            assert_eq!(ep.push(&f).0, VadEvent::None);
        }

        let started = tone(5, 6000.0)
            .iter()
            .any(|f| ep.push(f).0 == VadEvent::Start);
        assert!(started);
    }

    #[test]
    fn speech_during_calibration_is_kept() {
        let mut ep = Endpointer::new(VadCfg::default());
        let mut frames = tone(4, 10.0);
        frames.extend(tone(30, 8000.0));
        frames.extend(tone(40, 10.0));
        let mut utts = vec![];
        for f in &frames {
            if let (_, Some(u)) = ep.push(f) {
                utts.push(u);
            }
        }
        assert_eq!(utts.len(), 1);

        assert!(utts[0].len() >= 34 * FRAME);
    }

    #[test]
    fn forced_capture_for_push_to_talk() {
        let mut ep = Endpointer::new(VadCfg::default());
        ep.forced = true;
        let (ev, _) = ep.push(&tone(1, 0.0)[0]);
        assert_eq!(ev, VadEvent::Start);
        assert!(ep.in_speech);
    }

    #[test]
    fn decibels() {
        assert_eq!(db(0.0), -90.0);
        assert!((db(32768.0) - 0.0).abs() < 0.001);
        assert!((db(16384.0) + 6.02).abs() < 0.01);
        assert!((db(327.68) + 40.0).abs() < 0.01);
        assert_eq!(db(0.0001), -90.0);
    }

    #[test]
    fn wav_header() {
        let w = wav_bytes(&[0, 1, -1]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), RATE);
    }

    #[test]
    fn tts_echo_never_triggers() {
        let pcm: Vec<i16> = (0..RATE as usize * 3)
            .map(|i| {
                let t = i as f32 / RATE as f32;

                let syl = (t % 0.57) < 0.45;
                let env = if syl {
                    0.75 + 0.25 * (std::f32::consts::TAU * 4.0 * t).sin()
                } else {
                    0.02
                };
                let voiced = (std::f32::consts::TAU * 180.0 * t).sin()
                    + 0.5 * (std::f32::consts::TAU * 360.0 * t).sin();
                (env * voiced * 8000.0) as i16
            })
            .collect();
        let peak_db = |x: &[i16], gain: f32| -> f32 {
            x.chunks(FRAME)
                .map(|f| db(rms(f) * gain))
                .fold(-90.0, f32::max)
        };
        let threshold = -45.0;

        let mut g = EchoGate::new(false, 15.0);
        for f in pcm.chunks(FRAME) {
            assert_eq!(g.push(db(rms(f)), threshold, true, None), Gate::Mute);
        }

        let gain = 10f32.powf((threshold + 10.0 - peak_db(&pcm, 1.0)) / 20.0);
        let mut g = EchoGate::new(true, 15.0);
        for f in pcm.chunks(FRAME) {
            assert_eq!(g.push(db(rms(f) * gain), threshold, true, None), Gate::Mute);
        }

        let gain = 10f32.powf((threshold + 25.0 - peak_db(&pcm, 1.0)) / 20.0);
        let mut g = EchoGate::new(true, 15.0);
        let hit = pcm
            .chunks(FRAME)
            .position(|f| g.push(db(rms(f) * gain), threshold, true, None) == Gate::BargeIn);
        assert!(hit.is_some());

        assert_eq!(g.push(-20.0, threshold, false, None), Gate::Feed);
    }

    #[test]
    fn echo_cancellation_relaxes_the_gate() {
        let mut plain = EchoGate::new(true, 15.0);
        let mut aec = EchoGate::new(true, 15.0);
        aec.set_aec(true);
        let (mut p_hit, mut a_hit) = (false, false);
        for _ in 0..12 {
            p_hit |= plain.push(-35.0, -45.0, true, None) == Gate::BargeIn;
            a_hit |= aec.push(-35.0, -45.0, true, None) == Gate::BargeIn;
        }
        assert!(!p_hit && a_hit);

        let mut g = EchoGate::new(true, 15.0);
        g.set_aec(true);
        for _ in 0..60 {
            assert_eq!(g.push(-40.0, -55.0, true, Some(-20.0)), Gate::Mute);
        }

        g.set_aec(false);
        assert!((g.coupling_db + 12.0).abs() < 1e-6);
    }

    #[test]
    fn gate_needs_sustained_speech() {
        let mut g = EchoGate::new(true, 15.0);

        for _ in 0..6 {
            assert_eq!(g.push(-20.0, -45.0, true, None), Gate::Mute);
        }
        assert_eq!(g.push(-20.0, -45.0, true, None), Gate::BargeIn);

        let mut g = EchoGate::new(true, 15.0);
        for i in 0..60 {
            let l = if i % 3 == 0 { -20.0 } else { -60.0 };
            assert_eq!(g.push(l, -45.0, true, None), Gate::Mute);
        }

        let mut g = EchoGate::new(true, 15.0);
        let hit = (0..12).position(|i| {
            g.push(if i % 3 == 2 { -60.0 } else { -20.0 }, -45.0, true, None) == Gate::BargeIn
        });
        assert!(hit.is_some_and(|k| k <= 10), "{hit:?}");
    }

    #[test]
    fn reference_gate_ignores_our_echo() {
        let voice = |f: usize, freq: f32, amp: f32| -> Vec<f32> {
            (0..FRAME)
                .map(|i| {
                    let t = (f * FRAME + i) as f32 / RATE as f32;

                    let env = (0.5 + 0.5 * (t * 4.0 * std::f32::consts::TAU).sin()).powf(0.7);
                    amp * env * (t * freq * std::f32::consts::TAU).sin()
                })
                .collect()
        };
        let lvl = |x: &[f32]| {
            db((x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt() * 32768.0)
        };
        let threshold = -50.0;

        let play: Vec<Vec<f32>> = (0..100).map(|f| voice(f, 220.0, 0.18)).collect();
        let mut g = EchoGate::new(true, 15.0);
        let mut hist: Vec<f32> = vec![];
        for f in 0..play.len() {
            hist.push(lvl(&play[f]));
            let ref_db = hist.iter().rev().take(10).cloned().fold(-100.0, f32::max);
            let echo: Vec<f32> = play[f.saturating_sub(2)]
                .iter()
                .enumerate()
                .map(|(i, v)| v * 0.4 + 0.002 * ((i * 7919 % 97) as f32 / 97.0 - 0.5))
                .collect();
            assert_eq!(
                g.push(lvl(&echo), threshold, true, Some(ref_db)),
                Gate::Mute,
                "echo alone at frame {f}"
            );
        }

        let mut hit = None;
        for k in 0..30 {
            let f = 100 + k;
            let p = voice(f, 220.0, 0.18);
            hist.push(lvl(&p));
            let ref_db = hist.iter().rev().take(10).cloned().fold(-100.0, f32::max);
            let user = voice(f, 140.0, 0.6);
            let mic: Vec<f32> = p.iter().zip(&user).map(|(e, u)| e * 0.4 + u).collect();
            if g.push(lvl(&mic), threshold, true, Some(ref_db)) == Gate::BargeIn {
                hit = Some(k);
                break;
            }
        }
        let k = hit.expect("the user cuts in");
        assert!(
            (k + 1) as u32 * FRAME_MS <= 300,
            "within 300 ms ({} ms)",
            (k + 1) as u32 * FRAME_MS
        );

        let mut g = EchoGate::new(true, 15.0);
        for f in 0..40 {
            assert_eq!(
                g.push(-75.0, threshold, true, Some(lvl(&play[f]))),
                Gate::Mute
            );
        }
        assert!(
            g.coupling_db < -30.0,
            "learned there is no echo: {}",
            g.coupling_db
        );
        let hit =
            (0..12).position(|_| g.push(-32.0, threshold, true, Some(-18.0)) == Gate::BargeIn);
        assert!(hit.is_some());
    }
}
