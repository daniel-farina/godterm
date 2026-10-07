//! Hands free voice control. A background engine thread owns the mic
//! (ffmpeg), the endpointer and whisper; it sends transcripts to the app,
//! which parses and runs them. Nothing here blocks the UI thread.

pub mod apple;
pub mod audio;
pub mod capture;
pub mod denoise;
pub mod devices;
pub mod engines;
pub mod eval;
pub mod grammar;
pub mod grok_stt;
pub mod grok_tts;
pub mod kokoro;
pub mod player;
pub mod speaker;
pub mod speaker_cli;
pub mod speaker_fbank;
pub mod speaker_gate;
pub mod stt;
pub mod tts;
pub mod wake_profile;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::AppEvent;
use crate::config::{expand_tilde, VoiceCfg};
use audio::{Capture, Endpointer, Source, VadCfg, VadEvent};
use stt::Whisper;

/// pids of helper processes (whisper-server, ffmpeg) so shutdown can stop
/// them even while the voice thread is busy.
static HELPERS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

pub fn track(pid: u32) {
    HELPERS.lock().unwrap_or_else(|e| e.into_inner()).push(pid);
}

pub fn untrack(pid: u32) {
    HELPERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|p| *p != pid);
}

/// Terminate every tracked helper. Safe to call more than once.
pub fn kill_helpers() {
    let pids: Vec<u32> = std::mem::take(&mut *HELPERS.lock().unwrap_or_else(|e| e.into_inner()));
    for pid in pids {
        crate::platform::kill(pid as i32, crate::platform::SIGTERM);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VoiceStatus {
    Starting,
    /// Listening for the wake word (always on mode).
    Idle,
    /// Push to talk armed or speech detected.
    Listening,
    Transcribing,
    /// Engine stopped; reason when it failed.
    Off(Option<String>),
}

/// Live input levels, written by the capture thread and read by the UI
/// without locks (f32 bits in atomics).
#[derive(Default)]
pub struct Levels {
    db: std::sync::atomic::AtomicU32,
    peak: std::sync::atomic::AtomicU32,
    floor: std::sync::atomic::AtomicU32,
    threshold: std::sync::atomic::AtomicU32,
    /// The mic is open.
    pub open: AtomicBool,
    /// Speech is being captured right now.
    pub speech: AtomicBool,
}

fn store(a: &std::sync::atomic::AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}
fn load(a: &std::sync::atomic::AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

impl Levels {
    /// (level, peak, noise floor, speech threshold) in dBFS.
    pub fn read(&self) -> (f32, f32, f32, f32) {
        (
            load(&self.db),
            load(&self.peak),
            load(&self.floor),
            load(&self.threshold),
        )
    }
}

#[derive(Debug, Clone)]
pub enum VoiceEvent {
    Status(VoiceStatus),
    /// A partial transcript of speech still going on, and how long the
    /// request took (ms).
    Partial(String, u128),
    /// A transcript; `ptt` when captured by push to talk (no wake word needed).
    Heard {
        /// Length and loudness of the utterance.
        stats: wake_profile::UttStats,
        text: String,
        ptt: bool,
    },
    Info(String),
    /// The user talked over our reply: playback was stopped. `stop_ms`:
    /// from the decision to the output being silent.
    BargeIn {
        speech_ms: u32,
        stop_ms: u32,
    },
    /// Speaker lock: a scored utterance, or enrollment progress.
    Speaker(speaker::Event),
}

pub enum Ctl {
    /// Capture the next utterance regardless of the wake word.
    PushToTalk,
    /// Stop the current push to talk capture early.
    StopTalk,
    SetVocabulary(String),
    SetProfile(Option<wake_profile::WakeProfile>),
    Speaker(speaker::Cmd),
    Shutdown,
}

/// Handle owned by the app.
pub struct Voice {
    ctl: Sender<Ctl>,
    pub levels: Arc<Levels>,
    speaking: Arc<AtomicBool>,
    speaker: Arc<tts::Speaker>,
    pub tts: bool,
    pub chime: bool,
}

impl Voice {
    pub fn start(
        cfg: &VoiceCfg,
        source: Source,
        always_on: bool,
        events: Sender<AppEvent>,
    ) -> Voice {
        let (tx, rx) = mpsc::channel();
        let speaking = Arc::new(AtomicBool::new(false));
        let levels = Arc::new(Levels::default());
        let speaker = Arc::new(tts::Speaker::start(cfg, Arc::clone(&speaking)));
        let mut ecfg = cfg.clone();
        if ecfg.engine == "grok" && !ecfg.grok_stt.local_partials {
            ecfg.partial_model = "off".into();
        }
        let grok_stt = (ecfg.engine == "grok").then(|| {
            grok_stt::GrokStt::new(ecfg.grok.clone(), &ecfg.language, ecfg.grok_stt.timeout_ms)
        });
        let engine = Engine {
            grok_stt: std::cell::RefCell::new(grok_stt),
            speaker: Arc::clone(&speaker),
            levels: Arc::clone(&levels),
            cfg: ecfg,
            source,
            always_on,
            rx,
            events,
            speaking: Arc::clone(&speaking),
        };
        let _ = std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || engine.run());
        Voice {
            ctl: tx,
            levels,
            speaking,
            speaker,
            tts: cfg.tts && cfg.tts_engine != "off",
            chime: cfg.chime,
        }
    }

    pub fn push_to_talk(&self) {
        let _ = self.ctl.send(Ctl::PushToTalk);
    }

    pub fn stop_talk(&self) {
        let _ = self.ctl.send(Ctl::StopTalk);
    }

    pub fn set_profile(&self, p: Option<wake_profile::WakeProfile>) {
        let _ = self.ctl.send(Ctl::SetProfile(p));
    }

    /// Speaker lock enrollment and reset.
    pub fn speaker(&self, c: speaker::Cmd) {
        let _ = self.ctl.send(Ctl::Speaker(c));
    }

    pub fn set_vocabulary(&self, v: String) {
        let _ = self.ctl.send(Ctl::SetVocabulary(v));
    }

    /// Speak, sentence by sentence (Kokoro or `say`). Capture is muted
    /// while it talks. A new phrase cuts off the old one.
    pub fn say(&self, text: &str) {
        if !self.tts || text.trim().is_empty() {
            return;
        }
        self.speaker.say(text);
    }

    /// New talk back settings (voice, speed, volume, engine) without a
    /// restart of the mic side.
    pub fn configure_tts(&mut self, cfg: &VoiceCfg) {
        self.tts = cfg.tts && cfg.tts_engine != "off";
        self.speaker.configure(cfg);
    }

    /// Speak after what is being said (does not interrupt).
    pub fn say_queued(&self, text: &str) {
        if self.tts && !text.trim().is_empty() {
            self.speaker.enqueue(text);
        }
    }

    /// Cut off whatever is being said.
    pub fn stop_speaking(&self) {
        self.speaker.stop();
    }

    /// Talking back right now.
    pub fn speaking(&self) -> bool {
        self.speaking.load(Ordering::SeqCst)
    }

    /// The talk back engine in use ("kokoro af_heart ...", "say (...)").
    pub fn tts_engine(&self) -> String {
        self.speaker.engine()
    }

    /// A short system sound when the wake word is heard.
    pub fn chime(&self) {
        if !self.chime || tts::audio_muted() {
            return;
        }
        let speaking = Arc::clone(&self.speaking);
        speaking.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            let _ = Command::new("afplay")
                .arg("/System/Library/Sounds/Tink.aiff")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            speaking.store(false, Ordering::SeqCst);
        });
    }

    pub fn shutdown(&self) {
        let _ = self.ctl.send(Ctl::Shutdown);
        self.stop_speaking();
        kill_helpers();
    }
}

impl Drop for Voice {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Engine {
    /// Grok speech recognition (engine = grok), local ones as fallback.
    grok_stt: std::cell::RefCell<Option<grok_stt::GrokStt>>,
    speaker: Arc<tts::Speaker>,
    levels: Arc<Levels>,
    cfg: VoiceCfg,
    source: Source,
    always_on: bool,
    rx: Receiver<Ctl>,
    events: Sender<AppEvent>,
    speaking: Arc<AtomicBool>,
}

pub fn vad_cfg(c: &VoiceCfg) -> VadCfg {
    VadCfg {
        threshold: c.vad_threshold,
        min_rms: c.vad_min_rms,
        end_silence_ms: c.end_silence_ms,
        min_speech_ms: c.min_speech_ms,
        max_utterance_s: c.max_utterance_s,
        preroll_frames: (c.preroll_ms / audio::FRAME_MS).clamp(1, 50) as usize,
        fixed_floor: c
            .noise_floor
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|d| (-100.0..0.0).contains(d))
            .map(audio::rms_of_db),
    }
}

/// The wake screening model: `wake_model` if set to a path, or with
/// "auto" a tiny or base ggml model next to the main one. None when absent.
pub fn wake_model_path(c: &VoiceCfg) -> Option<std::path::PathBuf> {
    match c.wake_model.as_str() {
        "" | "off" | "none" => None,
        "auto" => {
            let main = expand_tilde(&c.model);
            let dir = main.parent()?.to_path_buf();
            [
                "ggml-tiny.en.bin",
                "ggml-base.en.bin",
                "ggml-tiny.bin",
                "ggml-base.bin",
            ]
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file() && *p != main)
        }
        path => Some(expand_tilde(path)).filter(|p| p.is_file()),
    }
}

/// How long a device or voice listing may take (ffmpeg's avfoundation
/// listing can hang on a permission prompt or a stuck audio system).
pub const LIST_TIMEOUT: Duration = Duration::from_secs(3);

/// avfoundation input device names, from `ffmpeg -list_devices`.
pub fn input_devices(ffmpeg: &str) -> Vec<String> {
    input_devices_timed(ffmpeg).unwrap_or_default()
}

/// The input devices, or why not (it timed out and was stopped).
pub fn input_devices_timed(ffmpeg: &str) -> Result<Vec<String>, String> {
    let o = crate::procs::output_timeout(
        Command::new(expand_tilde(ffmpeg)).args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ]),
        LIST_TIMEOUT,
    )?;
    Ok(parse_input_devices(&String::from_utf8_lossy(&o.stderr)))
}

pub fn parse_input_devices(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| !l.contains("audio devices"))
        .skip(1)
        .take_while(|l| l.contains("AVFoundation"))
        .filter_map(|l| l.rsplit("] ").next().map(str::to_string))
        .collect()
}

/// macOS `say` voices (English ones first).
pub fn say_voices() -> Vec<String> {
    let text = crate::procs::output_timeout(Command::new("say").args(["-v", "?"]), LIST_TIMEOUT)
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    parse_say_voices(&text)
}

pub fn parse_say_voices(text: &str) -> Vec<String> {
    let mut en = vec![];
    let mut other = vec![];
    for l in text.lines() {
        // "Samantha            en_US    # Hello, my name is Samantha."
        let Some((head, _)) = l.split_once('#') else {
            continue;
        };
        let mut parts: Vec<&str> = head.split_whitespace().collect();
        let Some(lang) = parts.pop() else { continue };
        let name = parts.join(" ");
        if name.is_empty() {
            continue;
        }
        if lang.starts_with("en") {
            en.push(name);
        } else {
            other.push(name);
        }
    }
    en.extend(other);
    en
}

/// The Apple speech helper: the signed copy shipped next to the binary in
/// GodTerm.app (release builds), else the one `godterm install` builds.
pub fn apple_helper_path() -> std::path::PathBuf {
    if let Some(p) = crate::config::env_var("SPEECH_BIN") {
        return std::path::PathBuf::from(p);
    }
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|e| e.canonicalize().ok())
        .and_then(|e| Some(e.parent()?.join("godterm-speech")));
    bundled
        .filter(|p| p.is_file())
        .unwrap_or_else(user_apple_helper_path)
}

/// Where `godterm install` compiles the Apple speech helper.
pub fn user_apple_helper_path() -> std::path::PathBuf {
    crate::config::app_home().join("bin").join("godterm-speech")
}

/// Where debug utterances go.
pub fn debug_dir() -> std::path::PathBuf {
    crate::config::app_home().join("voice").join("debug")
}

/// Keep this utterance (the exact WAV whisper got, and what came of it),
/// and only the newest 20.
pub fn save_debug(samples: &[i16], meta: serde_json::Value) {
    let d = debug_dir();
    if std::fs::create_dir_all(&d).is_err() {
        return;
    }
    let stem = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f").to_string();
    let _ = std::fs::write(d.join(format!("{stem}.wav")), audio::wav_bytes(samples));
    let _ = std::fs::write(
        d.join(format!("{stem}.json")),
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );
    let mut stems: Vec<String> = std::fs::read_dir(&d)
        .map(|r| {
            r.flatten()
                .filter_map(|e| {
                    e.path()
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .collect()
        })
        .unwrap_or_default();
    stems.sort();
    stems.dedup();
    if stems.len() > 20 {
        for s in &stems[..stems.len() - 20] {
            let _ = std::fs::remove_file(d.join(format!("{s}.wav")));
            let _ = std::fs::remove_file(d.join(format!("{s}.json")));
        }
    }
}

pub fn make_whisper(c: &VoiceCfg) -> Whisper {
    let mut w = Whisper::new(
        expand_tilde(&c.model),
        c.whisper_cli.clone(),
        c.whisper_server.clone(),
        c.language.clone(),
    );
    if c.whisper_threads > 0 {
        w.set_threads(c.whisper_threads);
    }
    w.beam = c.beam_size.clamp(1, 8);
    if c.whisper_vad {
        w.vad_model = Some(stt::vad_model_path()).filter(|p| p.is_file());
    }
    w
}

impl Engine {
    fn send(&self, ev: VoiceEvent) {
        let _ = self.events.send(AppEvent::Voice(ev));
    }

    fn status(&self, s: VoiceStatus) {
        self.send(VoiceEvent::Status(s));
    }

    fn run(self) {
        self.status(VoiceStatus::Starting);
        if self.cfg.whisper_vad && !stt::vad_model_path().is_file() && !cfg!(test) {
            if let Err(e) = stt::ensure_vad_model() {
                self.send(VoiceEvent::Info(format!(
                    "VAD model unavailable ({e:#}); whisper runs without it"
                )));
            }
        }
        // Which mic this really is, and whether it suits speech.
        if matches!(self.source, Source::Mic(_)) && !cfg!(test) {
            let (tx, dev) = (self.events.clone(), self.cfg.device.clone());
            std::thread::spawn(move || {
                let devs = devices::list();
                let name = devices::chosen(&devs, &dev)
                    .map(|d| format!("{} ({}, {} Hz)", d.name, d.transport, d.rate))
                    .unwrap_or_else(|| dev.clone());
                crate::log::info(&format!("voice: input {name}"));
                if let Some(a) = devices::advice(&devs, &dev) {
                    let _ = tx.send(AppEvent::Voice(VoiceEvent::Info(a)));
                }
            });
        }
        let mut whisper = make_whisper(&self.cfg);
        // Apple's on-device recognizer (voice.engine apple, or auto when the
        // helper works); whisper stays the fallback.
        let locale = if self.cfg.language.contains('-') {
            self.cfg.language.clone()
        } else if self.cfg.language == "en" {
            "en-US".to_string()
        } else {
            self.cfg.language.clone()
        };
        // With Grok, a local recognizer is the fallback (and gives the
        // partials): whisper, or Apple when that is the chosen fallback.
        let want_apple = !cfg!(test)
            && match self.cfg.engine.as_str() {
                "apple" => true,
                "auto" => apple::AppleSpeech::available(),
                "grok" => self.cfg.grok_stt.fallback == "apple" && apple::AppleSpeech::available(),
                _ => false,
            };
        let mut apple: Option<apple::AppleSpeech> = None;
        let mut apple_partial: Option<Arc<std::sync::Mutex<apple::AppleSpeech>>> = None;
        if want_apple {
            match apple::AppleSpeech::start(&locale, "final") {
                Ok(a) => {
                    apple = Some(a);
                    apple_partial = apple::AppleSpeech::start(&locale, "partial")
                        .ok()
                        .map(|a| Arc::new(std::sync::Mutex::new(a)));
                    self.send(VoiceEvent::Info("Apple on-device recognition ready".into()));
                }
                Err(e) => self.send(VoiceEvent::Info(format!(
                    "Apple recognition unavailable ({e:#}); using whisper"
                ))),
            }
        }
        let grok_only = self.cfg.engine == "grok" && whisper.check().is_err();
        if grok_only {
            self.send(VoiceEvent::Info(
                "Grok recognition, with no local fallback (whisper is not installed)".into(),
            ));
        }
        if apple.is_none() && !grok_only {
            if let Err(e) = whisper.check() {
                return self.status(VoiceStatus::Off(Some(e.to_string())));
            }
            // Load the model up front so the first command is quick.
            match whisper.start_server() {
                Ok(()) => self.send(VoiceEvent::Info(format!(
                    "whisper ready ({}, {} threads, beam {}{})",
                    whisper.gpu.clone().unwrap_or_else(|| "CPU".into()),
                    whisper.threads(),
                    whisper.beam,
                    if whisper.vad_model.is_some() {
                        ", VAD"
                    } else {
                        ""
                    }
                ))),
                Err(e) => self.send(VoiceEvent::Info(format!(
                    "whisper-server unavailable ({e}), using whisper-cli"
                ))),
            }
        }
        // Optional small model that only screens for the wake word, so the
        // large model runs only on speech addressed to us.
        let mut wake_whisper: Option<Whisper> = None;
        if self.always_on {
            if let Some(m) = wake_model_path(&self.cfg) {
                let mut w = Whisper::new(
                    m.clone(),
                    self.cfg.whisper_cli.clone(),
                    self.cfg.whisper_server.clone(),
                    self.cfg.language.clone(),
                );
                w.prompt = self.cfg.wake_words.join(", ");
                if w.start_server().is_ok() {
                    self.send(VoiceEvent::Info(format!(
                        "wake word screening with {}",
                        m.file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )));
                    wake_whisper = Some(w);
                }
            }
        }
        // Wake mode keeps the mic open; push to talk opens it per utterance
        // so the microphone indicator is only on while you talk.
        let mut cap: Option<Capture> = None;
        if self.always_on {
            match capture::open(&self.cfg, &self.source) {
                Ok(c) => cap = Some(c),
                Err(e) => {
                    return self
                        .status(VoiceStatus::Off(Some(format!("cannot start the mic: {e}"))))
                }
            }
        }
        // Noise suppression when the capture does not already do it.
        let mut denoiser: Option<denoise::Denoiser> = None;
        let mut cap_vp: Option<bool> = None;
        let mut ep = Endpointer::new(vad_cfg(&self.cfg));
        let mut frame = Vec::with_capacity(audio::FRAME);
        let mut ptt = false;
        let mut got_audio = false;
        let mut cap_started = Instant::now();
        let mut peak = -90f32;
        let mut peak_at = Instant::now();
        let mut last_partial = Instant::now();
        let partial_busy = Arc::new(AtomicBool::new(false));
        // Partials of the utterance going on (for debug_save_audio).
        let partial_log: Arc<std::sync::Mutex<Vec<serde_json::Value>>> = Default::default();
        let mut utt_started: Option<Instant> = None;
        let mut profile = wake_profile::WakeProfile::load();
        let mut gate = audio::EchoGate::new(self.cfg.barge_in, self.cfg.barge_margin_db);
        let mut barged = false;
        let mut muted: std::collections::VecDeque<Vec<i16>> = Default::default();
        let mut guard = speaker::Guard::new(&self.cfg);
        let idle = || {
            if self.always_on {
                VoiceStatus::Idle
            } else {
                VoiceStatus::Off(None)
            }
        };
        self.status(idle());
        loop {
            // Control messages (block briefly when the mic is closed).
            loop {
                let msg = if cap.is_none() {
                    match self.rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(m) => Ok(m),
                        Err(mpsc::RecvTimeoutError::Timeout) => Err(TryRecvError::Empty),
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            Err(TryRecvError::Disconnected)
                        }
                    }
                } else {
                    self.rx.try_recv()
                };
                match msg {
                    Ok(Ctl::PushToTalk) => {
                        // Talking to us cuts off whatever we were saying.
                        self.speaker.stop();
                        barged = true;
                        if cap.is_none() {
                            // The model was unloaded when the mic failed.
                            if apple.is_none() {
                                let _ = whisper.start_server();
                            }
                            match capture::open(&self.cfg, &self.source) {
                                Ok(c) => {
                                    cap = Some(c);
                                    cap_started = Instant::now();
                                    got_audio = false;
                                }
                                Err(e) => {
                                    self.status(VoiceStatus::Off(Some(format!(
                                        "cannot start the mic: {e}"
                                    ))));
                                    continue;
                                }
                            }
                        }
                        ptt = true;
                        ep.reset();
                        ep.forced = true;
                        self.status(VoiceStatus::Listening);
                    }
                    Ok(Ctl::StopTalk) => {
                        if let Some(u) = ep.flush() {
                            let partials = std::mem::take(
                                &mut *partial_log.lock().unwrap_or_else(|e| e.into_inner()),
                            );
                            self.finish(
                                &mut whisper,
                                apple.as_mut(),
                                wake_whisper.as_mut(),
                                &u,
                                ptt,
                                profile.as_ref(),
                                &mut guard,
                                (
                                    partials,
                                    utt_started.take().map(|t| t.elapsed().as_millis() as u64),
                                ),
                            );
                        }
                        ptt = false;
                        if !self.always_on {
                            cap = None;
                        }
                        self.status(idle());
                    }
                    Ok(Ctl::SetVocabulary(v)) => {
                        let ctx: Vec<String> = v
                            .split(", ")
                            .map(str::to_string)
                            .filter(|w| !w.is_empty())
                            .take(100)
                            .collect();
                        if let Some(a) = apple.as_mut() {
                            a.context = ctx.clone();
                        }
                        if let Some(p) = &apple_partial {
                            if let Ok(mut p) = p.lock() {
                                p.context = ctx;
                            }
                        }
                        whisper.prompt = v;
                    }
                    Ok(Ctl::SetProfile(p)) => profile = p,
                    Ok(Ctl::Speaker(c)) => {
                        if let Some(ev) = guard.command(c) {
                            self.send(VoiceEvent::Speaker(ev));
                        }
                    }
                    Ok(Ctl::Shutdown) | Err(TryRecvError::Disconnected) => {
                        drop(cap);
                        whisper.shutdown();
                        return;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            if cap.is_none() {
                self.levels.open.store(false, Ordering::Relaxed);
                self.levels.speech.store(false, Ordering::Relaxed);
            }
            let Some(c) = cap.as_mut() else { continue };
            // A new capture: echo cancellation in it relaxes the echo
            // gate, and denoising runs only when it has none.
            if cap_vp != Some(c.vp) {
                cap_vp = Some(c.vp);
                gate.set_aec(c.vp);
                denoiser = capture::denoise_on(&self.cfg, c.vp).then(denoise::Denoiser::new);
            }
            if c.frame(&mut frame).is_none() {
                // End of stream (file input) or ffmpeg died (mic denied).
                if let Some(u) = ep.flush() {
                    self.finish(
                        &mut whisper,
                        apple.as_mut(),
                        wake_whisper.as_mut(),
                        &u,
                        ptt || !self.always_on,
                        profile.as_ref(),
                        &mut guard,
                        (vec![], None),
                    );
                }
                let reason = c.error_text();
                cap = None;
                cap_vp = None;
                let file = matches!(self.source, Source::File { .. });
                let msg = if file {
                    Some("voice file finished".to_string())
                } else if !got_audio && cap_started.elapsed() < Duration::from_secs(5) {
                    Some(format!(
                        "no microphone access ({}); allow the terminal app in System Settings > Privacy > Microphone",
                        if reason.is_empty() { "stream ended" } else { reason.as_str() }
                    ))
                } else if reason.is_empty() {
                    Some("microphone stream ended".to_string())
                } else {
                    Some(reason)
                };
                self.status(VoiceStatus::Off(msg));
                // No mic: do not keep the speech model (about 2 GB) loaded
                // for nothing; it loads again when the mic comes back.
                if !file && !got_audio {
                    crate::log::info("voice: mic unavailable, unloading the speech model");
                    whisper.shutdown();
                }
                if self.always_on || file {
                    // Do not spin on a dead mic: wait for push to talk or shutdown.
                    while let Ok(m) = self.rx.recv() {
                        match m {
                            Ctl::Shutdown => {
                                whisper.shutdown();
                                return;
                            }
                            Ctl::PushToTalk if !file => {
                                if apple.is_none() {
                                    let _ = whisper.start_server();
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                ptt = false;
                continue;
            }
            got_audio = true;
            audio::apply_gain(&mut frame, self.cfg.gain_db);
            if let Some(d) = denoiser.as_mut() {
                d.process(&mut frame);
            }
            // Level meter: frame level, a peak that holds then falls back,
            // and the endpointer's floor and threshold.
            let l = audio::db(audio::rms(&frame));
            {
                store(&self.levels.db, l);
                if l >= peak || peak_at.elapsed() > Duration::from_millis(900) {
                    peak = if l >= peak { l } else { (peak - 1.5).max(l) };
                    if l >= peak {
                        peak_at = Instant::now();
                    }
                }
                store(&self.levels.peak, peak);
                store(&self.levels.floor, audio::db(ep.floor()));
                store(&self.levels.threshold, audio::db(ep.threshold()));
                self.levels.open.store(true, Ordering::Relaxed);
            }
            // Do not hear ourselves: the mic is muted while we talk back,
            // unless someone talks over us (barge-in, when enabled).
            let talking = self.speaking.load(Ordering::SeqCst);
            if !talking {
                barged = false;
            }
            // The loudest thing we played in the last 400 ms: our own echo
            // can only be about that loud (times the learned coupling).
            let played = player::played_db(Duration::from_millis(400));
            match gate.push(l, audio::db(ep.threshold()), talking && !barged, played) {
                audio::Gate::Mute => {
                    muted.push_back(frame.clone());
                    if muted.len() > 12 {
                        muted.pop_front();
                    }
                    ep.reset();
                    continue;
                }
                audio::Gate::BargeIn => {
                    let t0 = Instant::now();
                    *player::FLUSHED_AT.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    self.speaker.stop();
                    barged = true;
                    // How quickly the output went quiet (the callback flushes).
                    let mut stop_ms = 0;
                    while t0.elapsed() < Duration::from_millis(60) {
                        if let Some(at) =
                            *player::FLUSHED_AT.lock().unwrap_or_else(|e| e.into_inner())
                        {
                            stop_ms = at.saturating_duration_since(t0).as_millis() as u32;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    crate::log::info(&format!("voice: barge-in: user speech for {} ms over the echo (coupling {:.0} dB), audio stopped in {stop_ms} ms", gate.need_ms, gate.coupling_db));
                    self.send(VoiceEvent::BargeIn {
                        speech_ms: gate.need_ms,
                        stop_ms,
                    });
                    // Capture it now, wake word or not.
                    if !self.always_on {
                        ptt = true;
                    }
                    // Replay the loud start so the first word is not lost.
                    for f in muted.drain(..) {
                        let _ = ep.push(&f);
                    }
                }
                audio::Gate::Feed => muted.clear(),
            }
            if !self.always_on && !ptt {
                continue;
            }
            let (ev, utt) = ep.push(&frame);
            if ev == VadEvent::Start {
                utt_started = Some(Instant::now());
                partial_log
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
            }
            if ev == VadEvent::Start && self.always_on && !ptt {
                self.status(VoiceStatus::Listening);
            }
            self.levels
                .speech
                .store(ep.snapshot().is_some(), Ordering::Relaxed);
            // Partial transcript of the speech so far, at most one request
            // in flight, on the small model's server when there is one.
            if let (Some(buf), Some(ap)) = (ep.snapshot(), apple_partial.as_ref()) {
                if self.cfg.partial_model != "off"
                    && buf.len() > audio::RATE as usize * 6 / 10
                    && ep.trailing_silence_ms() < 150
                    && last_partial.elapsed() >= Duration::from_millis(400)
                    && !partial_busy.swap(true, Ordering::AcqRel)
                {
                    last_partial = Instant::now();
                    let max = audio::RATE as usize * 8;
                    let pcm = buf[buf.len().saturating_sub(max)..].to_vec();
                    let (busy, tx, ap) = (
                        Arc::clone(&partial_busy),
                        self.events.clone(),
                        Arc::clone(ap),
                    );
                    let plog = Arc::clone(&partial_log);
                    std::thread::spawn(move || {
                        let t0 = Instant::now();
                        if let Ok(mut a) = ap.lock() {
                            if let Ok(h) = a.transcribe(&pcm) {
                                plog.lock().unwrap_or_else(|e| e.into_inner()).push(serde_json::json!({"audio_ms": pcm.len() as u64 * 1000 / audio::RATE as u64, "took_ms": t0.elapsed().as_millis() as u64, "text": h.decoded.text}));
                                if !h.decoded.text.is_empty() {
                                    let _ = tx.send(AppEvent::Voice(VoiceEvent::Partial(
                                        h.decoded.text,
                                        t0.elapsed().as_millis(),
                                    )));
                                }
                            }
                        }
                        busy.store(false, Ordering::Release);
                    });
                }
            } else if let Some(buf) = ep.snapshot() {
                let port = match self.cfg.partial_model.as_str() {
                    "off" => None,
                    "main" => whisper.port(),
                    _ => wake_whisper
                        .as_ref()
                        .and_then(|w| w.port())
                        .or_else(|| whisper.port()),
                };
                if let Some(port) = port {
                    // Not once speech is trailing off: the final is near and
                    // must not wait behind a partial. At most the last 8 s.
                    if buf.len() > audio::RATE as usize * 6 / 10
                        && ep.trailing_silence_ms() < 150
                        && last_partial.elapsed() >= Duration::from_millis(400)
                        && !partial_busy.swap(true, Ordering::AcqRel)
                    {
                        last_partial = Instant::now();
                        let max = audio::RATE as usize * 8;
                        let wav = audio::wav_bytes(&buf[buf.len().saturating_sub(max)..]);
                        let busy = Arc::clone(&partial_busy);
                        let tx = self.events.clone();
                        let lang = self.cfg.language.clone();
                        let prompt = whisper.prompt.clone();
                        let plog = Arc::clone(&partial_log);
                        let audio_ms = buf.len() as u64 * 1000 / audio::RATE as u64;
                        std::thread::spawn(move || {
                            let t0 = Instant::now();
                            if let Ok(t) =
                                stt::post_inference_opts(port, &wav, &lang, &prompt, true)
                            {
                                plog.lock().unwrap_or_else(|e| e.into_inner()).push(serde_json::json!({"audio_ms": audio_ms, "took_ms": t0.elapsed().as_millis() as u64, "text": t}));
                                if !t.trim().is_empty() {
                                    let _ = tx.send(AppEvent::Voice(VoiceEvent::Partial(
                                        t,
                                        t0.elapsed().as_millis(),
                                    )));
                                }
                            }
                            busy.store(false, Ordering::Release);
                        });
                    }
                }
            }
            if let Some(u) = utt {
                let partials =
                    std::mem::take(&mut *partial_log.lock().unwrap_or_else(|e| e.into_inner()));
                let ended = utt_started.take().map(|t| t.elapsed().as_millis() as u64);
                self.finish(
                    &mut whisper,
                    apple.as_mut(),
                    wake_whisper.as_mut(),
                    &u,
                    ptt,
                    profile.as_ref(),
                    &mut guard,
                    (partials, ended),
                );
                ptt = false;
                if !self.always_on {
                    cap = None;
                }
                self.status(idle());
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        whisper: &mut Whisper,
        apple: Option<&mut apple::AppleSpeech>,
        wake: Option<&mut Whisper>,
        samples: &[i16],
        ptt: bool,
        profile: Option<&wake_profile::WakeProfile>,
        guard: &mut speaker::Guard,
        debug: (Vec<serde_json::Value>, Option<u64>),
    ) {
        // Speaker lock: enroll the user's clip, or drop speech that is not
        // the user (another voice, or someone far away) before it costs a
        // transcription.
        if guard.enrolling() && ptt {
            let ev = match guard.enroll_clip(samples) {
                Ok(n) => speaker::Event::Clips(n),
                Err(e) => speaker::Event::Error(format!("{e:#}")),
            };
            self.send(VoiceEvent::Speaker(ev));
        } else {
            let check = guard.check(samples, ptt, self.cfg.mode == "open");
            if check.verdict.is_some() || check.far.is_some() {
                let why = check.reason();
                if let Some(v) = check.verdict {
                    crate::log::info(&format!(
                        "voice: speaker score {:.2} (threshold {:.2}, {} ms, {} windows){}",
                        v.score,
                        v.threshold,
                        v.ms,
                        v.windows,
                        if check.applies { "" } else { ", not enforced" }
                    ));
                }
                self.send(VoiceEvent::Speaker(speaker::Event::Checked(check)));
                if let Some(why) = why {
                    crate::log::info(&format!("voice: {why}, ignored"));
                    if self.cfg.debug_save_audio {
                        save_debug(
                            samples,
                            serde_json::json!({"at": chrono::Local::now().to_rfc3339(), "rejected": why, "stats": wake_profile::UttStats::of(samples)}),
                        );
                    }
                    return;
                }
            }
        }
        self.status(VoiceStatus::Transcribing);
        if let (false, Some(w)) = (ptt, wake) {
            if let Ok(t) = w.transcribe(samples) {
                let norm = grammar::normalize(&t);
                // Small models sometimes drop the wake word itself, so only
                // skip longer speech; short phrases get the large model.
                let words = norm.split(' ').filter(|w| !w.is_empty()).count();
                if self.cfg.mode != "open"
                    && words > 4
                    && wake_profile::strip_wake_with(
                        profile,
                        &norm,
                        &self.cfg.wake_words,
                        self.cfg.wake_sensitivity,
                    )
                    .is_none()
                {
                    // Not for us: skip the large model entirely.
                    return self.send(VoiceEvent::Info(format!("ignored (no wake word): {t}")));
                }
            }
        }
        let t0 = Instant::now();
        let raw = samples;
        let mut leveled;
        let mut gain = 0.0;
        let samples: &[i16] = if self.cfg.auto_gain {
            leveled = samples.to_vec();
            gain = audio::normalize(&mut leveled, -20.0, 24.0);
            &leveled
        } else {
            samples
        };
        // Grok first when chosen; a failure (no login, no credits, a
        // timeout) falls back to the local recognizer for this utterance.
        let mut engine_used = if apple.is_some() { "apple" } else { "whisper" };
        let mut grok_text: Option<String> = None;
        if let Some(g) = self.grok_stt.borrow_mut().as_mut() {
            match g.transcribe(samples, &grok_stt::terms(&whisper.prompt)) {
                Ok(t) => {
                    crate::log::info(&format!(
                        "voice: grok transcript in {} ms ({})",
                        t0.elapsed().as_millis(),
                        g.from
                    ));
                    engine_used = "grok";
                    grok_text = Some(t);
                }
                Err(why) => {
                    crate::log::info(&format!("voice: grok recognition failed ({why}); local"));
                    self.send(VoiceEvent::Info(format!(
                        "Grok recognition unavailable ({why}); using {engine_used}"
                    )));
                }
            }
        }
        let res = match (grok_text, apple) {
            (Some(text), _) => Ok(stt::Decoded {
                text,
                ..Default::default()
            }),
            (None, Some(a)) => a.transcribe(samples).map(|h| {
                // n-best: an alternative that is exactly an instant command.
                let table = crate::instant::table(&self.cfg.instant);
                match apple::prefer_instant(&h.decoded.text, &h.alternatives, &table) {
                    Some(alt) => stt::Decoded {
                        text: alt,
                        ..h.decoded
                    },
                    None => h.decoded,
                }
            }),
            (None, None) => whisper.transcribe_full(samples),
        };
        if self.cfg.debug_save_audio {
            let (text, conf, ns, lp) = match &res {
                Ok(d) => (d.text.clone(), d.conf, d.no_speech, d.logprob),
                Err(e) => (format!("(error: {e})"), None, None, None),
            };
            save_debug(
                samples,
                serde_json::json!({
                    "at": chrono::Local::now().to_rfc3339(), "ptt": ptt, "final": text, "confidence": conf, "no_speech_prob": ns, "avg_logprob": lp,
                    "stt_ms": t0.elapsed().as_millis() as u64, "utterance_wall_ms": debug.1, "partials": debug.0,
                    "stats": wake_profile::UttStats::of(raw), "engine": engine_used, "model": self.cfg.model, "beam": whisper.beam, "vad": whisper.vad_model.is_some(),
                    "language": self.cfg.language, "prompt": whisper.prompt, "device": self.cfg.device, "gain_db": self.cfg.gain_db, "auto_gain_db": gain,
                }),
            );
        }
        match res {
            Ok(d) if !d.text.trim().is_empty() => self.send(VoiceEvent::Heard {
                text: d.text,
                ptt,
                stats: wake_profile::UttStats {
                    stt_ms: t0.elapsed().as_millis() as u32,
                    conf: d.conf,
                    no_speech: d.no_speech,
                    logprob: d.logprob,
                    ..wake_profile::UttStats::of(raw)
                },
            }),
            Ok(_) => {}
            Err(e) => self.send(VoiceEvent::Info(format!("transcription failed: {e}"))),
        }
    }
}
