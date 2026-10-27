use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::kokoro::{self, Kokoro, Provider};
use super::player::Player;
use crate::config::{app_home, expand_tilde, VoiceCfg};

pub fn audio_muted() -> bool {
    cfg!(test) || crate::config::env_var("NO_AUDIO").is_some()
}

pub enum WebAudio {
    Pcm(Vec<f32>, u32),

    Stop,

    Duck(f32),

    Said(String),
}

type WebOut = Box<dyn Fn(WebAudio) + Send>;
static WEB_OUT: Mutex<Option<WebOut>> = Mutex::new(None);

pub fn set_web_out(f: Option<WebOut>) {
    *WEB_OUT.lock().unwrap_or_else(|e| e.into_inner()) = f;

    super::player::web_stop();
    WEB_DUCK.store(1.0f32.to_bits(), Ordering::Relaxed);
}

fn web_out(a: WebAudio) -> bool {
    match WEB_OUT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(f) => {
            match &a {
                WebAudio::Pcm(s, rate) => super::player::web_clip(s, *rate),
                WebAudio::Stop => super::player::web_stop(),
                WebAudio::Duck(_) | WebAudio::Said(_) => {}
            }
            f(a);
            true
        }
        None => false,
    }
}

static WEB_DUCK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

pub fn web_duck(v: f32) {
    let v = v.clamp(0.0, 1.0);
    if !web_on() || WEB_DUCK.swap(v.to_bits(), Ordering::Relaxed) == v.to_bits() {
        return;
    }
    web_out(WebAudio::Duck(v));
}

fn web_on() -> bool {
    WEB_OUT.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

pub fn web_chime() -> bool {
    if !web_on() {
        return false;
    }
    let rate = 24000u32;
    let mut v = Vec::with_capacity(rate as usize / 5);
    for (f, n) in [(880.0f32, rate / 10), (1320.0, rate / 10)] {
        for i in 0..n {
            let t = i as f32 / rate as f32;

            let env = (i.min(n - i) as f32 / 240.0).min(1.0);
            v.push((t * f * std::f32::consts::TAU).sin() * 0.18 * env);
        }
    }
    web_out(WebAudio::Pcm(v, rate))
}

#[derive(Default)]
pub struct WebBatch {
    buf: Vec<f32>,
    rate: u32,

    pub clips: usize,

    pub secs: f64,
}

const WEB_FIRST_S: f64 = 0.35;

const WEB_NEXT_S: f64 = 1.2;

impl WebBatch {
    pub fn push(&mut self, samples: &[f32], rate: u32) -> Option<(Vec<f32>, u32)> {
        let mut out = None;
        if rate != self.rate && !self.buf.is_empty() {
            out = self.take();
        }
        self.rate = rate;
        self.buf.extend_from_slice(samples);
        let want = if self.clips == 0 {
            WEB_FIRST_S
        } else {
            WEB_NEXT_S
        };
        if out.is_none() && self.buf.len() as f64 >= want * f64::from(rate.max(1)) {
            out = self.take();
        }
        out
    }

    pub fn take(&mut self) -> Option<(Vec<f32>, u32)> {
        if self.buf.is_empty() || self.rate == 0 {
            return None;
        }
        self.clips += 1;
        self.secs += self.buf.len() as f64 / f64::from(self.rate);
        Some((std::mem::take(&mut self.buf), self.rate))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Confirm,

    Announce,
}

pub fn chunk_sentences(text: &str) -> Vec<String> {
    const MAX: usize = 180;
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
            continue;
        }
        cur.push(c);
        let next = chars.get(i + 1).copied();
        let at_space = next.is_none_or(char::is_whitespace);
        let end = matches!(c, '.' | '!' | '?') && at_space && !is_abbrev(&cur);
        let soft = matches!(c, ',' | ';' | ':') && at_space && cur.chars().count() > MAX / 2;
        if end || soft || cur.chars().count() >= MAX && c == ' ' {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

pub fn split_first(mut chunks: Vec<String>) -> Vec<String> {
    let Some(first) = chunks.first() else {
        return chunks;
    };
    if first.chars().count() <= 60 {
        return chunks;
    }
    let cut = first
        .char_indices()
        .find(|&(i, c)| i >= 15 && matches!(c, ',' | ';' | ':') && first[i + 1..].starts_with(' '));
    if let Some((i, _)) = cut {
        let (a, b) = (
            first[..=i].trim().to_string(),
            first[i + 1..].trim().to_string(),
        );
        if !b.is_empty() {
            chunks.splice(0..1, [a, b]);
        }
    }
    chunks
}

fn is_abbrev(s: &str) -> bool {
    let w = s.split_whitespace().last().unwrap_or("").to_lowercase();
    matches!(
        w.as_str(),
        "e.g." | "i.e." | "etc." | "vs." | "dr." | "mr." | "mrs." | "ms." | "st." | "no."
    )
}

pub fn kokoro_voices(path: &Path) -> Vec<String> {
    let Ok(data) = std::fs::read(path) else {
        return Vec::new();
    };
    zip_names(&data)
        .into_iter()
        .filter_map(|n| n.strip_suffix(".npy").map(str::to_string))
        .collect()
}

pub fn zip_names(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 46 <= data.len() {
        if &data[i..i + 4] == b"PK\x01\x02" {
            let n = u16::from_le_bytes([data[i + 28], data[i + 29]]) as usize;
            let extra = u16::from_le_bytes([data[i + 30], data[i + 31]]) as usize;
            let comment = u16::from_le_bytes([data[i + 32], data[i + 33]]) as usize;
            if let Some(name) = data.get(i + 46..i + 46 + n) {
                out.push(String::from_utf8_lossy(name).into_owned());
            }
            i += 46 + n + extra + comment;
        } else {
            i += 1;
        }
    }
    out.sort();
    out
}

pub fn only_tts_changed(a: &VoiceCfg, b: &VoiceCfg) -> bool {
    let mut x = a.clone();
    x.tts = b.tts;
    x.tts_engine = b.tts_engine.clone();
    x.tts_fallback = b.tts_fallback.clone();
    x.local = b.local.clone();
    x.grok = b.grok.clone();
    x.tts_voice = b.tts_voice.clone();
    x.kokoro_voice = b.kokoro_voice.clone();
    x.kokoro_model = b.kokoro_model.clone();
    x.kokoro_voices = b.kokoro_voices.clone();
    x.espeak = b.espeak.clone();
    x.tts_provider = b.tts_provider.clone();
    x.tts_threads = b.tts_threads;
    x.output_device = b.output_device.clone();
    x.pronounce = b.pronounce.clone();
    x.tts_speed = b.tts_speed;
    x.tts_volume = b.tts_volume;
    x.speak_confirm = b.speak_confirm;
    x.speak_readback = b.speak_readback;
    x.announce = b.announce;
    x.conversation = b.conversation;
    x == *b
}

pub fn fixes(cfg: &VoiceCfg) -> Vec<(String, String)> {
    cfg.pronounce
        .iter()
        .filter_map(|e| {
            let (w, say) = e.split_once('=')?;
            let w = w.trim();
            (!w.is_empty()).then(|| (w.to_string(), say.trim().to_string()))
        })
        .collect()
}

pub fn kokoro_problem(cfg: &VoiceCfg) -> Option<String> {
    let model = expand_tilde(&cfg.kokoro_model);
    let voices = expand_tilde(&cfg.kokoro_voices);
    if !model.exists() {
        return Some(format!("model missing: {}", model.display()));
    }
    if !voices.exists() {
        return Some(format!("voices missing: {}", voices.display()));
    }
    if !Path::new(&cfg.espeak).exists() && which(&cfg.espeak).is_none() {
        return Some(format!("{} not found (brew install espeak-ng)", cfg.espeak));
    }
    None
}

pub fn effective_engine(cfg: &VoiceCfg) -> (&'static str, Option<String>) {
    if !cfg.tts {
        return ("off", None);
    }
    match cfg.tts_engine.as_str() {
        "off" => ("off", None),
        "say" => ("say", None),
        "grok" => ("grok", None),
        e @ ("chatterbox" | "ming") => match super::local_tts::problem(e, cfg) {
            None => (if e == "ming" { "ming" } else { "chatterbox" }, None),
            Some(why) => match kokoro_problem(cfg) {
                None => ("kokoro", Some(format!("{e} {why}"))),
                Some(_) => ("say", Some(format!("{e} {why}"))),
            },
        },
        _ => match kokoro_problem(cfg) {
            None => ("kokoro", None),
            Some(why) => ("say", Some(why)),
        },
    }
}

pub fn provider_of(cfg: &VoiceCfg) -> Provider {
    match cfg.tts_provider.as_str() {
        "coreml" => Provider::CoreMl,
        _ => Provider::Cpu,
    }
}

pub fn load_kokoro(cfg: &VoiceCfg) -> anyhow::Result<Kokoro> {
    load_kokoro_with(cfg, true)
}

fn load_kokoro_with(cfg: &VoiceCfg, strict: bool) -> anyhow::Result<Kokoro> {
    let mut k = Kokoro::load(
        &expand_tilde(&cfg.kokoro_model),
        &expand_tilde(&cfg.kokoro_voices),
        &cfg.espeak,
        provider_of(cfg),
        cfg.tts_threads,
    )?;
    let mut voice = cfg.kokoro_voice.clone();
    if k.voices.style(&voice, 1).is_none() {
        if strict {
            anyhow::bail!("voice {} is not in {}", cfg.kokoro_voice, cfg.kokoro_voices);
        }
        voice = k.voices.names().into_iter().next().unwrap_or_default();
    }

    let _ = k.synth("Ready.", &voice, 1.0);
    let _ = k.synth(
        "Claude wants to run the tests in the project folder, then commit the changes and push.",
        &voice,
        1.0,
    );
    Ok(k)
}

enum Job {
    Say { gen: u64, text: String },

    Config(Box<VoiceCfg>),
}

pub struct Speaker {
    tx: Sender<Job>,
    gen: Arc<AtomicU64>,

    player: Arc<Mutex<Option<u32>>>,

    flush: Arc<AtomicBool>,

    pub engine: Arc<Mutex<String>>,
    speaking_flag: Arc<AtomicBool>,

    said: Arc<Mutex<VecDeque<String>>>,
}

impl Speaker {
    #[cfg(test)]
    pub fn idle() -> Speaker {
        let (tx, _rx) = mpsc::channel();
        Speaker {
            tx,
            gen: Arc::new(AtomicU64::new(0)),
            player: Arc::new(Mutex::new(None)),
            flush: Arc::new(AtomicBool::new(false)),
            engine: Arc::new(Mutex::new("idle".into())),
            speaking_flag: Arc::new(AtomicBool::new(true)),
            said: Default::default(),
        }
    }

    #[cfg(test)]
    pub fn stops(&self) -> u64 {
        self.gen.load(Ordering::SeqCst)
    }

    pub fn start(cfg: &VoiceCfg, speaking: Arc<AtomicBool>) -> Speaker {
        Speaker::start_with(cfg, speaking, false)
    }

    pub fn start_voice(cfg: &VoiceCfg, speaking: Arc<AtomicBool>) -> Speaker {
        Speaker::start_with(cfg, speaking, true)
    }

    fn start_with(cfg: &VoiceCfg, speaking: Arc<AtomicBool>, browser: bool) -> Speaker {
        let (tx, rx) = mpsc::channel();
        let gen = Arc::new(AtomicU64::new(0));
        let player = Arc::new(Mutex::new(None));
        let flush = Arc::new(AtomicBool::new(false));
        let engine = Arc::new(Mutex::new(String::from("starting")));
        let speaking_flag = Arc::clone(&speaking);
        let said: Arc<Mutex<VecDeque<String>>> = Default::default();
        let sd = Arc::clone(&said);
        let (c, g, pl, fl, en) = (
            cfg.clone(),
            Arc::clone(&gen),
            Arc::clone(&player),
            Arc::clone(&flush),
            Arc::clone(&engine),
        );
        let _ = std::thread::Builder::new()
            .name("tts".into())
            .spawn(move || {
                Worker {
                    cfg: c,
                    rx,
                    gen: g,
                    player: pl,
                    flush: fl,
                    engine: en,
                    said: sd,
                    speaking,
                    kokoro: None,
                    grok: None,
                    chatterbox: None,
                    ming: None,
                    out: None,
                    seq: 0,
                    browser,
                    web_said: None,
                    kokoro_failed: None,
                }
                .run()
            });
        Speaker {
            tx,
            gen,
            player,
            flush,
            engine,
            speaking_flag,
            said,
        }
    }

    pub fn said(&self) -> Vec<String> {
        self.said
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    pub fn say(&self, text: &str) {
        self.stop();
        let _ = self.tx.send(Job::Say {
            gen: self.gen.load(Ordering::SeqCst),
            text: text.to_string(),
        });
    }

    pub fn enqueue(&self, text: &str) {
        let _ = self.tx.send(Job::Say {
            gen: self.gen.load(Ordering::SeqCst),
            text: text.to_string(),
        });
    }

    pub fn speaking(&self) -> bool {
        self.speaking_flag.load(Ordering::SeqCst)
    }

    pub fn configure(&self, cfg: &VoiceCfg) {
        let _ = self.tx.send(Job::Config(Box::new(cfg.clone())));
    }

    pub fn stop(&self) {
        self.gen.fetch_add(1, Ordering::SeqCst);
        self.flush.store(true, Ordering::SeqCst);

        self.speaking_flag.store(false, Ordering::SeqCst);
        web_out(WebAudio::Stop);
        if let Some(pid) = self.player.lock().unwrap_or_else(|e| e.into_inner()).take() {
            crate::platform::kill(pid as i32, crate::platform::SIGTERM);
        }
    }

    pub fn engine(&self) -> String {
        self.engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

struct Worker {
    cfg: VoiceCfg,
    rx: Receiver<Job>,
    gen: Arc<AtomicU64>,
    player: Arc<Mutex<Option<u32>>>,
    flush: Arc<AtomicBool>,
    engine: Arc<Mutex<String>>,
    said: Arc<Mutex<VecDeque<String>>>,
    speaking: Arc<AtomicBool>,
    kokoro: Option<Kokoro>,
    grok: Option<super::engines::GrokEngine>,

    chatterbox: Option<super::local_tts::LocalEngine>,
    ming: Option<super::local_tts::LocalEngine>,
    out: Option<Player>,
    seq: u64,

    browser: bool,

    web_said: Option<String>,

    kokoro_failed: Option<Instant>,
}

impl Worker {
    fn set_engine(&self, s: String) {
        crate::log::info(&format!("tts: {s}"));
        *self.engine.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    fn load(&mut self) {
        self.kokoro = None;
        let chain = super::engines::chain(&self.cfg);
        self.grok = chain
            .contains(&"grok")
            .then(|| super::engines::GrokEngine::new(self.cfg.grok.clone()));
        let mut kokoro_note = None;
        if chain.contains(&"kokoro") {
            match load_kokoro(&self.cfg) {
                Ok(k) => {
                    kokoro_note = Some(format!(
                        "kokoro {} ({:?}, {} threads, loaded in {} ms)",
                        self.cfg.kokoro_voice, k.provider, k.threads, k.load_ms
                    ));
                    self.kokoro = Some(k);
                }
                Err(e) => kokoro_note = Some(format!("kokoro failed: {e}")),
            }
        }
        self.load_locals(&chain);
        self.set_engine(self.describe(&chain, kokoro_note));
    }

    fn load_locals(&mut self, chain: &[&str]) {
        for id in ["chatterbox", "ming"] {
            let want = chain.contains(&id);
            let slot = if id == "ming" {
                &mut self.ming
            } else {
                &mut self.chatterbox
            };
            if !want {
                *slot = None;
                continue;
            }
            let e = slot.get_or_insert_with(|| super::local_tts::LocalEngine::new(id, &self.cfg));
            e.set_cfg(&self.cfg);
            if !audio_muted() {
                let _ = e.start();
            }
        }
    }

    fn locals_loaded(&self) -> bool {
        self.chatterbox.as_ref().is_some_and(|e| e.loaded())
            || self.ming.as_ref().is_some_and(|e| e.loaded())
    }

    fn describe(&self, chain: &[&str], kokoro_note: Option<String>) -> String {
        if chain.is_empty() {
            return "off".into();
        }
        let parts: Vec<String> = chain
            .iter()
            .map(|e| match *e {
                "grok" => match self.grok.as_ref().and_then(|g| g.down_reason()) {
                    Some(w) => format!("grok {} (unavailable: {w})", self.cfg.grok.voice),
                    None => format!("grok {}", self.cfg.grok.voice),
                },
                id @ ("chatterbox" | "ming") => {
                    let e = if id == "ming" {
                        &self.ming
                    } else {
                        &self.chatterbox
                    };
                    let voice = super::local_tts::voice_label(id, &self.cfg);
                    match e {
                        Some(e) => match e.down_reason() {
                            Some(w) => format!("{id} {voice} (unavailable: {w})"),
                            None if e.ready() => format!("{id} {voice}"),
                            None if e.loaded() => format!("{id} {voice} (loading)"),
                            None => format!("{id} {voice} (unloaded while idle)"),
                        },
                        None => format!("{id} {voice}"),
                    }
                }
                "kokoro" => match (&self.kokoro, &kokoro_note) {
                    (Some(_), Some(n)) => n.clone(),
                    (Some(_), None) => format!("kokoro {}", self.cfg.kokoro_voice),
                    (None, Some(n)) => n.clone(),
                    (None, None) => {
                        format!("kokoro {} (unloaded while idle)", self.cfg.kokoro_voice)
                    }
                },
                e => e.to_string(),
            })
            .collect();
        let mut out = parts[0].clone();
        if parts.len() > 1 {
            out.push_str(&format!(", then {}", parts[1..].join(", ")));
        }

        if self.cfg.tts_engine == "kokoro" && !chain.contains(&"kokoro") {
            if let Some(why) = kokoro_problem(&self.cfg) {
                out = format!("{out} (kokoro unavailable: {why})");
            }
        }
        let want = self.cfg.tts_engine.as_str();
        if matches!(want, "chatterbox" | "ming") && !chain.contains(&want) {
            if let Some(why) = super::local_tts::problem(want, &self.cfg) {
                out = format!("{out} ({want} unavailable: {why})");
                super::local_tts::note_once(
                    want,
                    format!(
                        "{} is unavailable ({why}); speaking with {} instead.",
                        super::local_tts::info(want).map(|i| i.name).unwrap_or(want),
                        chain.first().copied().unwrap_or("nothing")
                    ),
                );
            }
        }
        out
    }

    fn run(mut self) {
        self.load();
        let mut idle_since = Instant::now();
        loop {
            let job = match self.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(j) => j,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let after = self.cfg.tts_unload_after_s;
                    if after > 0
                        && idle_since.elapsed() >= Duration::from_secs(after as u64)
                        && (self.kokoro.is_some() || self.out.is_some() || self.locals_loaded())
                    {
                        self.kokoro = None;
                        self.out = None;
                        for e in [&mut self.chatterbox, &mut self.ming].into_iter().flatten() {
                            e.unload();
                        }
                        self.set_engine(format!(
                            "kokoro {} (unloaded while idle)",
                            self.cfg.kokoro_voice
                        ));
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            idle_since = Instant::now();
            if let Job::Say { .. } = job {
                let (cfg, client) = self.effective();
                let chain = super::engines::chain(&cfg);
                if client.is_some() {
                    self.ensure(&cfg, &chain);
                } else if self.kokoro.is_none() && chain.contains(&"kokoro") {
                    self.load();
                }
            }
            match job {
                Job::Config(c) => {
                    let reload = c.kokoro_model != self.cfg.kokoro_model
                        || c.kokoro_voices != self.cfg.kokoro_voices
                        || c.tts_engine != self.cfg.tts_engine
                        || c.tts_fallback != self.cfg.tts_fallback
                        || c.grok != self.cfg.grok
                        || c.tts_provider != self.cfg.tts_provider
                        || c.tts_threads != self.cfg.tts_threads
                        || c.tts != self.cfg.tts
                        || c.local.venv != self.cfg.local.venv
                        || (self.kokoro.is_some()
                            && self
                                .kokoro
                                .as_ref()
                                .is_some_and(|k| k.voices.style(&c.kokoro_voice, 1).is_none()));
                    let device = c.output_device != self.cfg.output_device;
                    self.cfg = *c;

                    for e in [&mut self.chatterbox, &mut self.ming].into_iter().flatten() {
                        e.set_cfg(&self.cfg);
                    }
                    if device {
                        self.out = None;
                    }
                    if reload {
                        self.load();
                    } else {
                        let chain = super::engines::chain(&self.cfg);
                        self.set_engine(self.describe(&chain, None));
                    }
                }
                Job::Say { gen, text } => {
                    if gen != self.gen.load(Ordering::SeqCst) {
                        continue;
                    }
                    self.flush.store(false, Ordering::SeqCst);
                    self.speaking.store(true, Ordering::SeqCst);
                    self.speak(gen, &text);

                    std::thread::sleep(Duration::from_millis(250));
                    self.speaking.store(false, Ordering::SeqCst);
                }
            }
        }
    }

    fn effective(&self) -> (VoiceCfg, Option<u64>) {
        if self.browser && web_on() {
            if let Some((client, choice)) = super::web_tts::route() {
                return (
                    super::web_tts::web_cfg(&self.cfg, choice.as_ref()),
                    Some(client),
                );
            }
        }
        (self.cfg.clone(), None)
    }

    fn ensure(&mut self, cfg: &VoiceCfg, chain: &[&str]) {
        let retry = self
            .kokoro_failed
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
        if chain.contains(&"kokoro") && self.kokoro.is_none() && retry {
            match load_kokoro_with(cfg, false) {
                Ok(k) => {
                    crate::log::info(&format!(
                        "tts: kokoro loaded for the browser ({:?}, {} threads, {} ms)",
                        k.provider, k.threads, k.load_ms
                    ));
                    self.kokoro = Some(k);
                    self.kokoro_failed = None;
                }
                Err(e) => {
                    crate::log::info(&format!("tts: kokoro failed for the browser: {e}"));
                    self.kokoro_failed = Some(Instant::now());
                }
            }
        }
        if chain.contains(&"grok") && self.grok.is_none() {
            self.grok = Some(super::engines::GrokEngine::new(cfg.grok.clone()));
        }
        for id in ["chatterbox", "ming"] {
            let slot = if id == "ming" {
                &mut self.ming
            } else {
                &mut self.chatterbox
            };
            if chain.contains(&id) && slot.is_none() {
                let mut e = super::local_tts::LocalEngine::new(id, cfg);
                if !audio_muted() {
                    let _ = e.start();
                }
                *slot = Some(e);
            }
        }
    }

    fn speaks_with(cfg: &VoiceCfg, engine: &str) -> String {
        match engine {
            "kokoro" => format!("kokoro {}", cfg.kokoro_voice),
            "grok" => format!("grok {}", cfg.grok.voice),
            "say" => format!(
                "say {}",
                cfg.tts_voice.as_deref().unwrap_or("(system voice)")
            ),
            id @ ("chatterbox" | "ming") => {
                format!("{id} {}", super::local_tts::voice_label(id, cfg))
            }
            e => e.to_string(),
        }
    }

    fn current(&self, gen: u64) -> bool {
        gen == self.gen.load(Ordering::SeqCst)
    }

    fn speak(&mut self, gen: u64, text: &str) {
        let web = web_on();
        if audio_muted() && !web {
            crate::log::info(&format!("tts (muted, not played): {text}"));
            return;
        }

        let (mut cfg, client) = if web {
            self.effective()
        } else {
            (self.cfg.clone(), None)
        };
        let chain = super::engines::chain(&cfg);
        if let Some(client) = client {
            if let Some(k) = &self.kokoro {
                if k.voices.style(&cfg.kokoro_voice, 1).is_none() {
                    cfg.kokoro_voice = ["af_heart", self.cfg.kokoro_voice.as_str()]
                        .into_iter()
                        .find(|v| k.voices.style(v, 1).is_some())
                        .unwrap_or("af_heart")
                        .to_string();
                }
            }
            let first = chain.first().copied().unwrap_or("nothing");
            let mut line = Self::speaks_with(&cfg, first);
            if first != cfg.tts_engine {
                line = format!("{line} ({} is unavailable here)", cfg.tts_engine);
            }
            let line = format!("voice: browser speech with {line} (client {client})");
            if self.web_said.as_deref() != Some(line.as_str()) {
                crate::log::info(&line);
                self.web_said = Some(line);
            }
        } else {
            self.web_said = None;
        }

        if let Some(g) = self.grok.as_mut() {
            g.cfg = cfg.grok.clone();
        }
        for e in [&mut self.chatterbox, &mut self.ming].into_iter().flatten() {
            e.set_cfg(&cfg);
        }
        let text = kokoro::apply_fixes(text, &fixes(&cfg));
        let t0 = Instant::now();
        let mut first = true;

        let mut web_s = 0f64;
        let mut web_t0: Option<Instant> = None;
        let mut batch = WebBatch::default();
        if !web && self.out.is_none() && !chain.is_empty() {
            let t = Instant::now();
            match Player::open(&self.cfg.output_device, Arc::clone(&self.flush)) {
                Ok(p) => {
                    crate::log::info(&format!(
                        "tts: opened {} in {} ms",
                        p.device,
                        t.elapsed().as_millis()
                    ));
                    self.out = Some(p);
                }
                Err(e) => {
                    crate::log::info(&format!("tts: audio output failed ({e}), using afplay"))
                }
            }
        }
        let mut fallback: Option<(std::process::Child, PathBuf)> = None;
        let vol = self.cfg.tts_volume.clamp(0.0, 1.0);
        let speed = self.cfg.tts_speed.clamp(0.5, 2.0);
        let say_dir = app_home().join("voice").join("tts");
        for sentence in split_first(chunk_sentences(&text)) {
            if !self.current(gen) {
                break;
            }
            {
                let mut s = self.said.lock().unwrap_or_else(|e| e.into_inner());
                s.push_back(sentence.clone());
                while s.len() > 3 {
                    s.pop_front();
                }
            }
            if web {
                web_out(WebAudio::Said(sentence.clone()));
            }

            let mut held: Vec<f32> = vec![];
            let mut held_rate = kokoro::SAMPLE_RATE;
            let gens = Arc::clone(&self.gen);
            let out = self.out.as_ref();
            let mut sink = |samples: &[f32], rate: u32| -> bool {
                if gens.load(Ordering::SeqCst) != gen {
                    return false;
                }
                if first {
                    crate::log::info(&format!(
                        "tts: first audio after {} ms",
                        t0.elapsed().as_millis()
                    ));
                    first = false;
                }
                if web {
                    if samples.is_empty() {
                        return true;
                    }
                    let v: Vec<f32> = samples.iter().map(|x| x * vol).collect();
                    let Some((clip, r)) = batch.push(&v, rate) else {
                        return true;
                    };
                    let n = clip.len();
                    if web_out(WebAudio::Pcm(clip, r)) {
                        web_t0.get_or_insert_with(Instant::now);
                        web_s += n as f64 / r.max(1) as f64;
                        return true;
                    }
                }
                match out {
                    Some(o) => o.push(samples, rate, vol),
                    None => {
                        held_rate = rate;
                        held.extend_from_slice(samples);
                    }
                }
                true
            };
            let mut say = super::engines::SayEngine {
                voice: cfg.tts_voice.clone(),
                speed,
                dir: say_dir.clone(),
            };
            let mut kk = self.kokoro.as_mut().map(|k| super::engines::KokoroEngine {
                k,
                voice: cfg.kokoro_voice.clone(),
                speed,
            });
            let mut engines: Vec<&mut dyn super::engines::TtsEngine> = vec![];

            let mut grok = self.grok.as_mut();
            let mut kslot = kk.as_mut();
            let mut sslot = Some(&mut say);
            let mut cslot = self.chatterbox.as_mut();
            let mut mslot = self.ming.as_mut();
            for e in &chain {
                let next: Option<&mut dyn super::engines::TtsEngine> = match *e {
                    "grok" => grok.take().map(|g| g as &mut dyn super::engines::TtsEngine),
                    "chatterbox" => cslot
                        .take()
                        .map(|c| c as &mut dyn super::engines::TtsEngine),
                    "ming" => mslot
                        .take()
                        .map(|m| m as &mut dyn super::engines::TtsEngine),
                    "kokoro" => kslot
                        .take()
                        .map(|k| k as &mut dyn super::engines::TtsEngine),
                    _ => sslot
                        .take()
                        .map(|x| x as &mut dyn super::engines::TtsEngine),
                };
                engines.extend(next);
            }
            let (who, failed) =
                super::engines::speak_with_fallback(&mut engines, &sentence, &mut sink);
            drop(engines);

            if let Some((clip, r)) = batch.take() {
                let n = clip.len();
                if self.current(gen) && web_out(WebAudio::Pcm(clip, r)) {
                    web_t0.get_or_insert_with(Instant::now);
                    web_s += n as f64 / r.max(1) as f64;
                }
            }
            for (name, why) in &failed {
                crate::log::info(&format!("tts: {name} failed on {sentence:?}: {why}"));

                let id = name.split(' ').next().unwrap_or("");
                if matches!(id, "chatterbox" | "ming") && !why.contains("still loading") {
                    super::local_tts::note_once(
                        id,
                        format!(
                            "{} could not speak ({why}); {} speaks instead.",
                            super::local_tts::info(id).map(|i| i.name).unwrap_or(id),
                            who.as_deref().unwrap_or("nothing")
                        ),
                    );
                }
            }
            if !failed.is_empty() {
                let note = match &who {
                    Some(w) => format!("{w} ({} unavailable: {})", failed[0].0, failed[0].1),
                    None => format!("nothing spoke ({})", failed[0].1),
                };
                self.set_engine(note);
            } else if who
                .as_deref()
                .is_some_and(|w| w.starts_with("chatterbox") || w.starts_with("ming"))
            {
                let now = self
                    .engine
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if now.contains("loading") || now.contains("unavailable") {
                    let mac = super::engines::chain(&self.cfg);
                    self.set_engine(self.describe(&mac, None));
                }
                let ms = [&self.chatterbox, &self.ming]
                    .into_iter()
                    .flatten()
                    .find(|e| who.as_deref().is_some_and(|w| w.starts_with(e.id)))
                    .and_then(|e| e.last_first_audio_ms);
                if let Some(ms) = ms {
                    crate::log::info(&format!("tts: local first audio {ms} ms after the request"));
                }
            }
            if let Some(ms) = self.grok.as_ref().and_then(|g| g.last_first_audio_ms) {
                if who.as_deref().is_some_and(|w| w.starts_with("grok")) {
                    crate::log::info(&format!(
                        "tts: grok first audio {ms} ms after the request ({})",
                        self.grok.as_ref().map(|g| g.from.as_str()).unwrap_or("")
                    ));
                }
            }
            if !held.is_empty() && self.current(gen) {
                let f = self.tmp("wav");
                let w = if held_rate == kokoro::SAMPLE_RATE {
                    kokoro::wav(&held)
                } else {
                    kokoro::wav(&super::player::resample(
                        &held,
                        held_rate,
                        kokoro::SAMPLE_RATE,
                    ))
                };
                if std::fs::write(&f, w).is_ok() {
                    self.wait_fallback(&mut fallback);
                    fallback = self.afplay(&f).map(|c| (c, f));
                }
            }
        }
        self.wait_fallback(&mut fallback);

        if let Some(t) = web_t0 {
            crate::log::info(&format!(
                "tts: {:.1} s of speech to the browser in {} clips",
                web_s, batch.clips
            ));

            let end = (t + Duration::from_secs_f64(web_s))
                .max(super::player::web_end().unwrap_or(t))
                + Duration::from_millis(150);
            while Instant::now() < end {
                if self.flush.swap(false, Ordering::SeqCst) || !self.current(gen) {
                    web_out(WebAudio::Stop);
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }

        while let Some(out) = &self.out {
            if self.flush.swap(false, Ordering::SeqCst) || !self.current(gen) {
                out.clear();
                if let Some(g) = &self.grok {
                    super::engines::TtsEngine::cancel(g);
                }
                for e in [&self.chatterbox, &self.ming].into_iter().flatten() {
                    super::engines::TtsEngine::cancel(e);
                }
                break;
            }
            if out.pending_s() <= 0.0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        *self.player.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn tmp(&mut self, ext: &str) -> PathBuf {
        let dir = app_home().join("voice").join("tts");
        let _ = std::fs::create_dir_all(&dir);
        self.seq += 1;
        dir.join(format!("{}-{}.{ext}", std::process::id(), self.seq))
    }

    fn wait_fallback(&self, playing: &mut Option<(std::process::Child, PathBuf)>) {
        if let Some((mut p, f)) = playing.take() {
            let _ = p.wait();
            let _ = std::fs::remove_file(f);
        }
    }

    fn afplay(&self, file: &Path) -> Option<std::process::Child> {
        let vol = self.cfg.tts_volume.clamp(0.0, 1.0);
        let c = Command::new("afplay")
            .args(["-v", &format!("{vol:.2}")])
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        *self.player.lock().unwrap_or_else(|e| e.into_inner()) = Some(c.id());
        Some(c)
    }
}

pub fn bench(out_dir: &Path, play: bool) -> anyhow::Result<()> {
    let cfg = crate::config::Config::load_or_init()?.voice;
    if let Some(p) = kokoro_problem(&cfg) {
        anyhow::bail!("{p}");
    }
    let short = "Approved, account two.";
    let long = "Claude wants to run the test suite in the godterm folder, then commit the changes and push the branch, which will take a few minutes.";
    let fx = fixes(&cfg);
    std::fs::create_dir_all(out_dir)?;
    for provider in [Provider::Cpu, Provider::CoreMl] {
        let t = Instant::now();
        let mut k = match Kokoro::load(
            &expand_tilde(&cfg.kokoro_model),
            &expand_tilde(&cfg.kokoro_voices),
            &cfg.espeak,
            provider,
            cfg.tts_threads,
        ) {
            Ok(k) => k,
            Err(e) => {
                println!("{provider:?}: failed to load: {e}");
                continue;
            }
        };
        let load = t.elapsed().as_millis();
        let t = Instant::now();
        let _ = k.synth("Ready.", &cfg.kokoro_voice, 1.0);
        let warm = t.elapsed().as_millis();
        println!(
            "{provider:?}: loaded in {load} ms, {} threads, warm-up {warm} ms",
            k.threads
        );
        for (label, text) in [("short", short), ("long", long)] {
            let text = kokoro::apply_fixes(text, &fx);
            let mut best: Option<kokoro::Timing> = None;
            let mut last = Vec::new();
            for _ in 0..3 {
                let (s, t) = k.synth(&text, &cfg.kokoro_voice, cfg.tts_speed)?;
                if best.is_none_or(|b| t.infer_ms + t.phonemize_ms < b.infer_ms + b.phonemize_ms) {
                    best = Some(t);
                }
                last = s;
            }
            let b = best.unwrap_or_default();
            println!(
                "  {label:<5} {:.2}s audio: first audio {} ms (phonemes {} ms, inference {} ms), rtf {:.3}",
                b.audio_s,
                b.phonemize_ms + b.infer_ms,
                b.phonemize_ms,
                b.infer_ms,
                b.rtf()
            );
            let f = out_dir.join(format!("{label}-{provider:?}.wav").to_lowercase());
            std::fs::write(&f, kokoro::wav(&last))?;
            if play && provider == Provider::Cpu {
                let _ = Command::new("afplay").arg(&f).status();
            }
        }
        println!(
            "  phonemes: {}",
            k.phonemes(&kokoro::apply_fixes(long, &fx))?
        );
    }
    println!("wavs in {}", out_dir.display());
    Ok(())
}

pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return Path::new(name).is_file().then(|| PathBuf::from(name));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin".into(), "/usr/local/bin".into()])
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences() {
        assert_eq!(
            chunk_sentences("Approved. Account two is next! Is that ok?"),
            vec!["Approved.", "Account two is next!", "Is that ok?"]
        );

        assert_eq!(
            chunk_sentences("Version 2.5 is out, e.g. on main. See src/app.rs now."),
            vec!["Version 2.5 is out, e.g. on main.", "See src/app.rs now."]
        );
        assert_eq!(chunk_sentences("one\n\ntwo"), vec!["one", "two"]);
        assert!(chunk_sentences("   ").is_empty());
        assert_eq!(
            chunk_sentences("no stop at the end"),
            vec!["no stop at the end"]
        );

        let long = format!(
            "{}, and then {}",
            "word ".repeat(25).trim(),
            "more ".repeat(20).trim()
        );
        let c = chunk_sentences(&long);
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(c.iter().all(|s| s.chars().count() <= 180));

        let c = chunk_sentences(&"abc ".repeat(100));
        assert!(
            c.len() >= 2 && c.iter().all(|s| s.chars().count() <= 181),
            "{c:?}"
        );
    }

    #[test]
    fn zip_central_directory() {
        let mut z = vec![0u8; 7];
        for name in ["af_heart.npy", "am_michael.npy"] {
            let mut e = vec![0u8; 46];
            e[..4].copy_from_slice(b"PK\x01\x02");
            e[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
            e.extend_from_slice(name.as_bytes());
            z.extend(e);
        }
        assert_eq!(zip_names(&z), vec!["af_heart.npy", "am_michael.npy"]);
        let p = std::env::temp_dir().join(format!("cg-voices-{}.bin", std::process::id()));
        std::fs::write(&p, &z).unwrap();
        assert_eq!(kokoro_voices(&p), vec!["af_heart", "am_michael"]);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn first_chunk_is_short() {
        let c = split_first(chunk_sentences(
            "Claude wants to run the test suite in the folder, then commit the changes. Done.",
        ));
        assert_eq!(
            c,
            vec![
                "Claude wants to run the test suite in the folder,",
                "then commit the changes.",
                "Done."
            ]
        );
        assert_eq!(
            split_first(vec!["Short, fine.".into()]),
            vec!["Short, fine."]
        );
        assert!(split_first(vec![]).is_empty());
    }

    #[test]
    fn pronunciation_entries() {
        let mut c = VoiceCfg::default();
        c.pronounce = vec![
            "godterm = clawed go".into(),
            "bad".into(),
            "=x".into(),
            "CLI=C L I".into(),
        ];
        assert_eq!(
            fixes(&c),
            vec![
                ("godterm".into(), "clawed go".into()),
                ("CLI".into(), "C L I".into())
            ]
        );
        assert!(VoiceCfg::default()
            .pronounce
            .iter()
            .any(|p| p.starts_with("JSON=")));
    }

    #[test]
    fn tts_only_changes() {
        let a = VoiceCfg::default();
        let mut b = a.clone();
        b.kokoro_voice = "am_michael".into();
        b.tts_speed = 1.2;
        assert!(only_tts_changed(&a, &b));
        b.device = "1".into();
        assert!(!only_tts_changed(&a, &b));
    }

    #[test]
    fn engine_choice() {
        let mut c = VoiceCfg::default();
        c.tts = false;
        assert_eq!(effective_engine(&c).0, "off");
        c.tts = true;
        c.tts_engine = "say".into();
        assert_eq!(effective_engine(&c), ("say", None));
        c.tts_engine = "kokoro".into();
        c.kokoro_model = "/nonexistent/kokoro.onnx".into();
        assert!(kokoro_problem(&c).unwrap().contains("model missing"));
        let (e, why) = effective_engine(&c);
        assert_eq!(e, "say");
        assert!(why.is_some());
    }

    #[test]
    fn browser_speech_goes_as_few_clips_never_empty() {
        let mut b = WebBatch::default();
        let mut clips = vec![];

        for i in 0..75 {
            let piece = if i % 5 == 0 {
                vec![]
            } else {
                vec![0.1f32; 1200]
            };
            clips.extend(b.push(&piece, 24000));
        }
        clips.extend(b.take());
        assert!(b.take().is_none(), "nothing held after the end");
        assert!(clips.iter().all(|(c, _)| !c.is_empty()));
        assert!(clips.len() <= 4, "{} clips", clips.len());

        assert!(clips[0].0.len() <= 24000 / 2);
        let total: usize = clips.iter().map(|(c, _)| c.len()).sum();
        assert_eq!(total, 60 * 1200);
        assert_eq!(b.clips, clips.len());
    }
}
