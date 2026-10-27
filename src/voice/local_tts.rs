use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::engines::TtsEngine;
use crate::config::{app_home, expand_tilde, LocalTtsCfg, VoiceCfg};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalInfo {
    pub id: &'static str,
    pub name: &'static str,

    pub repo: &'static str,
    pub disk_gb: f32,
    pub ram_gb: f32,

    pub first_audio: &'static str,
    pub rtf: f32,
    pub license: &'static str,
    pub blurb: &'static str,
}

pub const CHATTERBOX: LocalInfo = LocalInfo {
    id: "chatterbox",
    name: "Chatterbox Turbo",
    repo: "mlx-community/chatterbox-turbo-fp16",
    disk_gb: 2.9,
    ram_gb: 5.2,
    first_audio: "0.2 s",
    rtf: 0.26,
    license: "MIT (its audio carries an inaudible watermark)",
    blurb: "More natural, and it can clone a voice from a short clip",
};

pub const MING: LocalInfo = LocalInfo {
    id: "ming",
    name: "Ming-3.0",
    repo: "mlx-community/Ming-3.0-tts-0.5B-bf16",
    disk_gb: 2.9,
    ram_gb: 4.2,
    first_audio: "1.5 to 4 s",
    rtf: 0.6,
    license: "MIT",
    blurb: "Five built in English voices, or a voice designed from a description",
};

pub const LOCALS: &[LocalInfo] = &[CHATTERBOX, MING];

pub fn info(id: &str) -> Option<&'static LocalInfo> {
    LOCALS.iter().find(|l| l.id == id)
}

pub const MING_VOICES: &[&str] = &[
    "US_Emma",
    "US_Stella",
    "US_James",
    "UK_George",
    "UK_Charlotte",
];

pub const MLX_AUDIO: &str =
    "mlx-audio @ git+https://github.com/Blaizzy/mlx-audio@24547fd21503d897b52924306608ea727a9f9405";

pub const HELPER_PY: &str = include_str!("local_helper.py");

pub fn supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

pub fn voices_dir() -> PathBuf {
    app_home().join("voices")
}

pub fn venv_dir(cfg: &LocalTtsCfg) -> PathBuf {
    if cfg.venv.trim().is_empty() {
        voices_dir().join("venv")
    } else {
        expand_tilde(&cfg.venv)
    }
}

pub fn venv_python(cfg: &LocalTtsCfg) -> PathBuf {
    venv_dir(cfg).join("bin").join("python")
}

pub fn marker(id: &str) -> PathBuf {
    voices_dir().join(format!("{id}.installed"))
}

pub fn installed(id: &str, cfg: &LocalTtsCfg) -> bool {
    marker(id).exists() && venv_python(cfg).exists()
}

pub fn hub_cache() -> PathBuf {
    if cfg!(test) {
        return app_home().join("hf-test-cache");
    }
    if let Some(p) = std::env::var_os("HF_HUB_CACHE") {
        return PathBuf::from(p);
    }
    std::env::var_os("HF_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home_dir().join(".cache").join("huggingface"))
        .join("hub")
}

pub fn model_cache_dir(repo: &str) -> PathBuf {
    hub_cache().join(format!("models--{}", repo.replace('/', "--")))
}

pub fn problem(id: &str, cfg: &VoiceCfg) -> Option<String> {
    if !supported() {
        return Some("needs a Mac with Apple Silicon".into());
    }
    if info(id).is_none() {
        return Some("unknown engine".into());
    }
    if !marker(id).exists() {
        return Some("not installed (Settings > Voice and Audio)".into());
    }
    if !venv_python(&cfg.local).exists() {
        return Some(format!(
            "its Python environment is missing ({}); install it again",
            crate::config::tilde(&venv_dir(&cfg.local))
        ));
    }
    None
}

pub fn voice_label(id: &str, cfg: &VoiceCfg) -> String {
    match id {
        "chatterbox" => match cfg.local.chatterbox_ref.trim() {
            "" => "default voice".into(),
            r => format!(
                "clone of {}",
                Path::new(r)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| r.to_string())
            ),
        },
        _ => {
            if cfg.local.ming_voice == "design" && !cfg.local.ming_design.trim().is_empty() {
                "designed voice".into()
            } else {
                cfg.local.ming_voice.clone()
            }
        }
    }
}

static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static NOTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn note_once(key: &str, text: String) {
    let mut seen = NOTED.lock().unwrap_or_else(|e| e.into_inner());
    if seen.iter().any(|k| k == key) {
        return;
    }
    seen.push(key.to_string());
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).push(text);
}

pub fn take_notes() -> Vec<String> {
    std::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(test)]
pub fn reset_notes() {
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    NOTED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub log: PathBuf,
}

pub fn spec_for(id: &str, cfg: &LocalTtsCfg) -> Spec {
    let dir = voices_dir();
    let script = dir.join("local_helper.py");
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::read_to_string(&script).ok().as_deref() != Some(HELPER_PY) {
        let _ = std::fs::write(&script, HELPER_PY);
    }
    let repo = info(id).map(|i| i.repo).unwrap_or_default();
    Spec {
        program: venv_python(cfg),
        args: vec![
            "-I".into(),
            script.to_string_lossy().into_owned(),
            "--engine".into(),
            id.into(),
            "--model".into(),
            repo.into(),
        ],
        log: dir.join(format!("{id}-helper.log")),
    }
}

#[derive(Debug)]
enum Msg {
    Ready { load_ms: u64 },
    Audio { id: u64, rate: u32, pcm: Vec<f32> },
    Done { id: u64 },
    Error { id: Option<u64>, msg: String },
    Pong,
    Gone,
}

const MAX_FRAME: usize = 64 << 20;

fn read_msgs(out: impl Read, tx: mpsc::Sender<Msg>) {
    let mut r = BufReader::new(out);
    loop {
        let mut line = String::new();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => {
                let _ = tx.send(Msg::Gone);
                return;
            }
            Ok(_) => {}
        }
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let bytes = v["bytes"].as_u64().unwrap_or(0) as usize;
        let mut pcm = vec![];
        if bytes > 0 {
            if bytes > MAX_FRAME || !bytes.is_multiple_of(4) {
                let _ = tx.send(Msg::Gone);
                return;
            }
            let mut b = vec![0u8; bytes];
            if r.read_exact(&mut b).is_err() {
                let _ = tx.send(Msg::Gone);
                return;
            }
            pcm = b
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
        }
        let id = v["id"].as_u64();
        let msg = match v["ev"].as_str().unwrap_or("") {
            "ready" => Msg::Ready {
                load_ms: v["load_ms"].as_u64().unwrap_or(0),
            },
            "audio" => Msg::Audio {
                id: id.unwrap_or(0),
                rate: v["rate"].as_u64().unwrap_or(24_000) as u32,
                pcm,
            },
            "done" => Msg::Done {
                id: id.unwrap_or(0),
            },
            "error" => Msg::Error {
                id,
                msg: v["msg"].as_str().unwrap_or("error").to_string(),
            },
            "pong" => Msg::Pong,
            _ => continue,
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}

pub struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Msg>,
    pub started: Instant,

    pub load_ms: Option<u64>,
    next: u64,

    pub request_failed: bool,
}

impl Helper {
    pub fn spawn(spec: &Spec) -> Result<Helper, String> {
        if let Some(d) = spec.log.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let err = std::fs::File::create(&spec.log)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .env("PYTHONUNBUFFERED", "1")
            .env("HF_HUB_OFFLINE", "1")
            .env("TOKENIZERS_PARALLELISM", "false")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(err)
            .spawn()
            .map_err(|e| format!("could not start the helper: {e}"))?;
        super::track(child.id());
        let out = child.stdout.take().ok_or("no helper output")?;
        let stdin = child.stdin.take();
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("local-tts-read".into())
            .spawn(move || read_msgs(out, tx));
        Ok(Helper {
            child,
            stdin,
            rx,
            started: Instant::now(),
            load_ms: None,
            next: 0,
            request_failed: false,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn send(&mut self, v: &Value) -> Result<(), String> {
        let w = self.stdin.as_mut().ok_or("helper input closed")?;
        writeln!(w, "{v}")
            .and_then(|_| w.flush())
            .map_err(|e| format!("helper stopped ({e})"))
    }

    pub fn wait_ready(&mut self, t: Duration) -> Result<bool, String> {
        if self.load_ms.is_some() {
            return Ok(true);
        }
        let end = Instant::now() + t;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Msg::Ready { load_ms }) => {
                    self.load_ms = Some(load_ms);
                    return Ok(true);
                }
                Ok(Msg::Error { msg, .. }) => return Err(msg),
                Ok(Msg::Gone) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.exit_reason())
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Ok(false),
            }
        }
    }

    fn exit_reason(&mut self) -> String {
        std::thread::sleep(Duration::from_millis(30));
        match self.child.try_wait() {
            Ok(Some(st)) => format!("the helper exited ({st})"),
            _ => "the helper closed its output".into(),
        }
    }

    #[allow(dead_code)]
    pub fn ping(&mut self, t: Duration) -> bool {
        if self.send(&json!({"op": "ping"})).is_err() {
            return false;
        }
        let end = Instant::now() + t;
        while let Ok(m) = self
            .rx
            .recv_timeout(end.saturating_duration_since(Instant::now()))
        {
            if matches!(m, Msg::Pong) {
                return true;
            }
        }
        false
    }

    pub fn speak(
        &mut self,
        mut req: Value,
        first: Duration,
        gap: Duration,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(&[f32], u32) -> bool,
    ) -> Result<Option<u64>, String> {
        self.next += 1;
        self.request_failed = false;
        let id = self.next;
        req["op"] = json!("speak");
        req["id"] = json!(id);
        let t0 = Instant::now();
        self.send(&req)?;
        let mut first_ms = None;
        let mut stopped = false;
        loop {
            let wait = if stopped {
                Duration::from_secs(5)
            } else if first_ms.is_none() {
                first
            } else {
                gap
            };
            match self.rx.recv_timeout(wait) {
                Ok(Msg::Audio { id: i, rate, pcm }) if i == id => {
                    if stopped {
                        continue;
                    }
                    if first_ms.is_none() {
                        first_ms = Some(t0.elapsed().as_millis() as u64);
                    }
                    if cancel.load(Ordering::SeqCst) || !sink(&pcm, rate) {
                        stopped = true;
                        self.send(&json!({"op": "cancel", "id": id}))?;
                    }
                }
                Ok(Msg::Done { id: i }) if i == id => return Ok(first_ms),
                Ok(Msg::Error { id: Some(i), msg }) if i == id => {
                    self.request_failed = !stopped;
                    return if stopped { Ok(first_ms) } else { Err(msg) };
                }
                Ok(Msg::Error { id: None, msg }) => return Err(msg),
                Ok(Msg::Gone) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.exit_reason())
                }

                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    if stopped {
                        return Err("the helper did not stop in time".into());
                    }
                    return Err(match first_ms {
                        None => format!("no audio within {} ms", first.as_millis()),
                        Some(_) => "the helper stalled".into(),
                    });
                }
            }
            if cancel.load(Ordering::SeqCst) && !stopped {
                stopped = true;
                self.send(&json!({"op": "cancel", "id": id}))?;
            }
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.stdin.take();
        let pid = self.child.id();
        let end = Instant::now() + Duration::from_millis(300);
        while Instant::now() < end {
            if !matches!(self.child.try_wait(), Ok(None)) {
                super::untrack(pid);
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        super::untrack(pid);
    }
}

const MAX_CRASHES: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(600);
pub const LOCAL_RETRY: Duration = Duration::from_secs(300);

pub struct LocalEngine {
    pub id: &'static str,
    cfg: LocalTtsCfg,
    pub speed: f32,
    spec: Spec,
    helper: Option<Helper>,
    crashes: Vec<Instant>,
    down: Option<(Instant, String)>,
    cancel: Arc<AtomicBool>,

    pub last_first_audio_ms: Option<u64>,
}

impl LocalEngine {
    pub fn new(id: &'static str, cfg: &VoiceCfg) -> LocalEngine {
        let spec = spec_for(id, &cfg.local);
        LocalEngine::with_spec(id, cfg, spec)
    }

    pub fn with_spec(id: &'static str, cfg: &VoiceCfg, spec: Spec) -> LocalEngine {
        LocalEngine {
            id,
            cfg: cfg.local.clone(),
            speed: cfg.tts_speed.clamp(0.5, 2.0),
            spec,
            helper: None,
            crashes: vec![],
            down: None,
            cancel: Arc::new(AtomicBool::new(false)),
            last_first_audio_ms: None,
        }
    }

    pub fn set_cfg(&mut self, cfg: &VoiceCfg) {
        self.cfg = cfg.local.clone();
        self.speed = cfg.tts_speed.clamp(0.5, 2.0);
    }

    pub fn loaded(&self) -> bool {
        self.helper.is_some()
    }

    pub fn ready(&self) -> bool {
        self.helper.as_ref().is_some_and(|h| h.load_ms.is_some())
    }

    #[allow(dead_code)]
    pub fn pid(&self) -> Option<u32> {
        self.helper.as_ref().map(Helper::pid)
    }

    pub fn start(&mut self) -> Result<(), String> {
        if let Some(h) = self.helper.as_mut() {
            if h.alive() {
                return Ok(());
            }
            self.helper = None;
        }
        match Helper::spawn(&self.spec) {
            Ok(h) => {
                crate::log::info(&format!(
                    "tts: {} helper started (pid {})",
                    self.id,
                    h.pid()
                ));
                self.helper = Some(h);
                Ok(())
            }
            Err(e) => {
                self.crashed(&e);
                Err(e)
            }
        }
    }

    pub fn unload(&mut self) {
        if self.helper.take().is_some() {
            crate::log::info(&format!("tts: {} helper stopped (idle)", self.id));
        }
    }

    pub fn down_reason(&self) -> Option<&str> {
        self.down
            .as_ref()
            .filter(|(at, _)| at.elapsed() < LOCAL_RETRY)
            .map(|(_, w)| w.as_str())
    }

    fn crashed(&mut self, why: &str) {
        self.helper = None;
        let now = Instant::now();
        self.crashes
            .retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        self.crashes.push(now);
        crate::log::info(&format!("tts: {} helper failed: {why}", self.id));
        if self.crashes.len() >= MAX_CRASHES {
            self.down = Some((
                now,
                format!("stopped {MAX_CRASHES} times ({why}); paused for 5 minutes"),
            ));
            self.crashes.clear();
        }
    }

    fn request(&self, text: &str) -> Value {
        match self.id {
            "chatterbox" => json!({"text": text, "ref": self.cfg.chatterbox_ref.trim()}),
            _ => {
                let design = self.cfg.ming_voice == "design";
                let voice = if MING_VOICES.contains(&self.cfg.ming_voice.as_str()) {
                    self.cfg.ming_voice.as_str()
                } else {
                    "US_Emma"
                };
                json!({
                    "text": text,
                    "voice": voice,
                    "instruct": if design { self.cfg.ming_design.trim() } else { "" },
                })
            }
        }
    }

    pub fn first_timeout(&self) -> Duration {
        let ms = self.cfg.timeout_ms.max(1000) as u64;
        Duration::from_millis(if self.id == "ming" { ms * 2 } else { ms })
    }
}

impl TtsEngine for LocalEngine {
    fn name(&self) -> String {
        format!(
            "{} ({})",
            self.id,
            info(self.id).map(|i| i.name).unwrap_or("")
        )
    }

    fn speak_stream(
        &mut self,
        text: &str,
        sink: &mut dyn FnMut(&[f32], u32) -> bool,
    ) -> Result<(), String> {
        if let Some(w) = self.down_reason() {
            return Err(w.to_string());
        }
        self.cancel.store(false, Ordering::SeqCst);
        if self.helper.as_mut().is_none_or(|h| !h.alive()) {
            self.start()?;
        }
        let load_timeout = Duration::from_secs(self.cfg.load_timeout_s.max(5) as u64);
        let first = self.first_timeout();
        let req = self.request(text);
        let Some(h) = self.helper.as_mut() else {
            return Err("the helper is not running".into());
        };

        match h.wait_ready(first) {
            Ok(true) => {}
            Ok(false) => {
                if h.started.elapsed() >= load_timeout {
                    let why = format!("did not load in {} s", load_timeout.as_secs());
                    self.crashed(&why);
                    return Err(why);
                }
                return Err("still loading the model".into());
            }
            Err(e) => {
                let why = format!("{e} (log: {})", crate::config::tilde(&self.spec.log));
                self.crashed(&why);
                return Err(why);
            }
        }
        let gap = Duration::from_secs(15);
        let mut st = Stretch::new(self.speed);
        let cancel = Arc::clone(&self.cancel);
        let r = {
            let mut feed = |pcm: &[f32], rate: u32| -> bool {
                if st.identity() {
                    sink(pcm, rate)
                } else {
                    let out = st.push(pcm, rate);
                    out.is_empty() || sink(&out, rate)
                }
            };
            h.speak(req, first, gap, &cancel, &mut feed)
        };
        match r {
            Ok(ms) => {
                let tail = st.finish();
                if !tail.is_empty() && !cancel.load(Ordering::SeqCst) {
                    sink(&tail, st.rate);
                }
                self.last_first_audio_ms = ms;
                Ok(())
            }
            Err(e) => {
                if !self.helper.as_ref().is_some_and(|h| h.request_failed) {
                    self.crashed(&e);
                }
                Err(e)
            }
        }
    }

    fn voices(&self) -> Vec<String> {
        match self.id {
            "ming" => MING_VOICES.iter().map(|s| s.to_string()).collect(),
            _ => vec![],
        }
    }

    fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

pub struct Stretch {
    speed: f32,
    pub rate: u32,
    input: Vec<f32>,

    pos: f64,

    tail: Vec<f32>,
    started: bool,
}

impl Stretch {
    pub fn new(speed: f32) -> Stretch {
        Stretch {
            speed: speed.clamp(0.5, 2.0),
            rate: 24_000,
            input: vec![],
            pos: 0.0,
            tail: vec![],
            started: false,
        }
    }

    pub fn identity(&self) -> bool {
        (self.speed - 1.0).abs() < 0.02
    }

    fn sizes(&self) -> (usize, usize, usize) {
        let n = (self.rate as usize / 25).max(64);
        (n, n / 2, n / 4)
    }

    pub fn push(&mut self, pcm: &[f32], rate: u32) -> Vec<f32> {
        if !self.started {
            self.rate = rate.max(8000);
            self.started = true;
        }
        self.input.extend_from_slice(pcm);
        self.run(false)
    }

    pub fn finish(&mut self) -> Vec<f32> {
        if self.identity() {
            return vec![];
        }
        let mut out = self.run(true);
        out.append(&mut self.tail);
        self.input.clear();
        self.pos = 0.0;
        out
    }

    fn run(&mut self, last: bool) -> Vec<f32> {
        let (n, hs, tol) = self.sizes();
        let ha = hs as f64 * self.speed as f64;
        let mut out = vec![];
        loop {
            let base = self.pos.round() as usize;
            let need = base + n + tol;
            if need > self.input.len() && (!last || base >= self.input.len()) {
                break;
            }

            let mut best = base;
            if !self.tail.is_empty() {
                let lo = base.saturating_sub(tol);
                let hi = (base + tol).min(self.input.len().saturating_sub(hs));
                let mut best_c = f32::MIN;
                let mut k = lo;
                while k <= hi {
                    let c: f32 = (0..hs.min(self.input.len() - k))
                        .map(|i| self.input[k + i] * self.tail[i])
                        .sum();
                    if c > best_c {
                        best_c = c;
                        best = k;
                    }
                    k += 2;
                }
            }
            let frame: Vec<f32> = (0..n)
                .map(|i| self.input.get(best + i).copied().unwrap_or(0.0))
                .collect();

            #[allow(clippy::needless_range_loop)]
            for i in 0..hs {
                let w = 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / hs as f32).cos();
                let prev = self.tail.get(i).copied().unwrap_or(0.0);
                let a = if self.tail.is_empty() { 1.0 } else { w };
                out.push(prev * (1.0 - a) + frame[i] * a);
            }
            self.tail = frame[hs..].to_vec();
            self.pos += ha;
            if last && best + n >= self.input.len() {
                break;
            }
        }

        let keep_from = (self.pos as usize).saturating_sub(tol);
        if keep_from > n * 4 {
            self.input.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Install {
    Running { id: String, step: String },
    Done { id: String, msg: String },
    Failed { id: String, msg: String },
}

static INSTALL: Mutex<Option<Install>> = Mutex::new(None);

pub fn install_state() -> Option<Install> {
    INSTALL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_state(s: Option<Install>) {
    *INSTALL.lock().unwrap_or_else(|e| e.into_inner()) = s;
}

pub fn take_finished() -> Option<Install> {
    let mut g = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(&*g, Some(Install::Done { .. } | Install::Failed { .. })) {
        g.take()
    } else {
        None
    }
}

pub fn installing() -> bool {
    matches!(install_state(), Some(Install::Running { .. }))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub what: String,
    pub argv: Vec<String>,
}

#[cfg_attr(test, allow(dead_code))]
pub fn find_python(cfg: &LocalTtsCfg) -> (Option<PathBuf>, Option<PathBuf>) {
    let w = |n: &str| super::tts::which(n);
    if cfg.python != "auto" && !cfg.python.trim().is_empty() {
        return (None, Some(expand_tilde(&cfg.python)));
    }
    let uv = w("uv").or_else(|| {
        let p = crate::config::home_dir().join(".local/bin/uv");
        p.is_file().then_some(p)
    });
    let py = ["python3.12", "python3.13", "python3.11", "python3.10"]
        .iter()
        .find_map(|n| w(n));
    (uv, py)
}

pub fn install_steps(
    id: &str,
    cfg: &LocalTtsCfg,
    uv: Option<&Path>,
    python: Option<&Path>,
) -> Result<Vec<Step>, String> {
    let info = info(id).ok_or("unknown engine")?;
    let venv = venv_dir(cfg);
    let vpy = venv_python(cfg);
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut steps = vec![];
    if !vpy.exists() {
        let argv = match (uv, python) {
            (Some(uv), _) => vec![
                s(uv),
                "venv".into(),
                "--python".into(),
                "3.12".into(),
                s(&venv),
            ],
            (None, Some(py)) => vec![s(py), "-m".into(), "venv".into(), s(&venv)],
            (None, None) => {
                return Err("no Python 3.10 to 3.13 or uv found (brew install python@3.12)".into())
            }
        };
        steps.push(Step {
            what: "making the Python environment".into(),
            argv,
        });
    }
    let pip = match uv {
        Some(uv) => vec![
            s(uv),
            "pip".into(),
            "install".into(),
            "--python".into(),
            s(&vpy),
            MLX_AUDIO.into(),
        ],
        None => vec![
            s(&vpy),
            "-m".into(),
            "pip".into(),
            "install".into(),
            MLX_AUDIO.into(),
        ],
    };
    steps.push(Step {
        what: "installing mlx-audio (about 400 MB)".into(),
        argv: pip,
    });
    steps.push(Step {
        what: format!("downloading {} (about {:.1} GB)", info.name, info.disk_gb),
        argv: vec![
            s(&vpy),
            "-I".into(),
            "-c".into(),
            format!(
                "from huggingface_hub import snapshot_download; print(snapshot_download({:?}))",
                info.repo
            ),
        ],
    });
    Ok(steps)
}

#[cfg(test)]
pub static STARTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn start_install(id: &str, vc: &VoiceCfg) {
    if installing() {
        return;
    }
    set_state(Some(Install::Running {
        id: id.to_string(),
        step: "starting".into(),
    }));
    #[cfg(test)]
    {
        STARTED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(id.to_string());
        set_state(None);
        let _ = vc;
    }
    #[cfg(not(test))]
    {
        let id = id.to_string();
        let cfg = vc.local.clone();
        let _ = std::thread::Builder::new()
            .name("local-tts-install".into())
            .spawn(move || {
                let r = run_install(&id, &cfg);
                set_state(Some(match r {
                    Ok(msg) => Install::Done { id, msg },
                    Err(msg) => Install::Failed { id, msg },
                }));
            });
    }
}

#[cfg(not(test))]
fn run_install(id: &str, cfg: &LocalTtsCfg) -> Result<String, String> {
    let info = info(id).ok_or("unknown engine")?;
    if !supported() {
        return Err("local voices need a Mac with Apple Silicon".into());
    }
    let dir = voices_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (uv, py) = find_python(cfg);
    let steps = install_steps(id, cfg, uv.as_deref(), py.as_deref())?;
    let log_path = dir.join(format!("install-{id}.log"));
    let n = steps.len();
    let model_dir = model_cache_dir(info.repo);
    for (i, st) in steps.iter().enumerate() {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| e.to_string())?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        crate::log::info(&format!("local voices: {}: {}", id, st.what));
        let mut cmd = Command::new(&st.argv[0]);
        cmd.args(&st.argv[1..])
            .env_remove("HF_HUB_OFFLINE")
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err);
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", st.what))?;
        let downloading = i + 1 == n;
        loop {
            match child.try_wait() {
                Ok(Some(s)) if s.success() => break,
                Ok(Some(s)) => {
                    return Err(format!(
                        "{} failed ({s}); see {}",
                        st.what,
                        crate::config::tilde(&log_path)
                    ))
                }
                Ok(None) => {}
                Err(e) => return Err(e.to_string()),
            }
            let pct = downloading.then(|| {
                let have = dir_bytes(&model_dir) as f64;
                ((have / (info.disk_gb as f64 * 1e9)) * 100.0).min(99.0) as u32
            });
            set_state(Some(Install::Running {
                id: id.to_string(),
                step: match pct {
                    Some(p) => format!("step {}/{n}: {} {p}%", i + 1, st.what),
                    None => format!("step {}/{n}: {}", i + 1, st.what),
                },
            }));
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    let _ = std::fs::write(dir.join("local_helper.py"), HELPER_PY);
    std::fs::write(
        marker(id),
        json!({"repo": info.repo, "mlx_audio": MLX_AUDIO, "at": chrono::Local::now().to_rfc3339()})
            .to_string(),
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "{} is installed. Choose it as the engine to use it.",
        info.name
    ))
}

#[cfg_attr(test, allow(dead_code))]
pub fn dir_bytes(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.filter_map(|e| e.ok())
        .map(|e| {
            let md = std::fs::symlink_metadata(e.path());
            match md {
                Ok(m) if m.is_dir() => dir_bytes(&e.path()),
                Ok(m) if m.is_file() => m.len(),
                _ => 0,
            }
        })
        .sum()
}

pub fn remove_paths(id: &str, cfg: &LocalTtsCfg) -> Vec<PathBuf> {
    let mut v = vec![marker(id)];
    if let Some(i) = info(id) {
        v.push(model_cache_dir(i.repo));
    }
    let others = LOCALS.iter().any(|l| l.id != id && marker(l.id).exists());
    if !others {
        v.push(venv_dir(cfg));
        v.push(voices_dir().join("local_helper.py"));
    }
    v
}

pub fn remove(id: &str, cfg: &LocalTtsCfg) -> Result<String, String> {
    let name = info(id).map(|i| i.name).ok_or("unknown engine")?;
    for p in remove_paths(id, cfg) {
        let r = if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else if p.exists() {
            std::fs::remove_file(&p)
        } else {
            Ok(())
        };
        r.map_err(|e| format!("could not remove {}: {e}", crate::config::tilde(&p)))?;
    }
    Ok(format!("{name} removed."))
}

pub fn status(id: &str, cfg: &VoiceCfg) -> String {
    if !supported() {
        return "unavailable: needs a Mac with Apple Silicon".into();
    }
    match install_state() {
        Some(Install::Running { id: i, step }) if i == id => return format!("installing: {step}"),
        Some(Install::Failed { id: i, msg }) if i == id => return format!("install failed: {msg}"),
        _ => {}
    }
    match problem(id, cfg) {
        None => "installed".into(),
        Some(w) if w.starts_with("not installed") => "not installed".into(),
        Some(w) => w,
    }
}

pub fn consent_lines(id: &str) -> Vec<String> {
    let Some(i) = info(id) else {
        return vec![];
    };

    vec![
        format!("Install {}, a local voice?", i.name),
        format!("{}.", i.blurb),
        format!(
            "Download: about {:.1} GB (the model) and 0.4 GB (mlx-audio, in ~/.godterm/voices).",
            i.disk_gb
        ),
        format!("Memory: about {:.0} GB while it speaks.", i.ram_gb),
        format!(
            "Speed here: first audio {}, {:.2}x real time.",
            i.first_audio, i.rtf
        ),
        format!("License: {}.", i.license),
        "It runs on this Mac only; nothing is sent anywhere.".into(),
        "Remove takes it away again. Kokoro stays the default and the fallback.".into(),
    ]
}

pub fn bench_command(args: &[String]) -> anyhow::Result<()> {
    let arg = |n: &str| {
        args.iter()
            .position(|a| a == n)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let engine = arg("--engine").unwrap_or_else(|| "chatterbox".into());
    let id: &'static str = if engine == "ming" {
        "ming"
    } else {
        "chatterbox"
    };
    let py = PathBuf::from(arg("--python").ok_or_else(|| anyhow::anyhow!("--python PY"))?);
    let out = PathBuf::from(arg("--out").ok_or_else(|| anyhow::anyhow!("--out WAV"))?);
    let dir = out
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("helper-{id}"));
    std::fs::create_dir_all(&dir)?;
    let script = dir.join("local_helper.py");
    std::fs::write(&script, HELPER_PY)?;
    let spec = Spec {
        program: py,
        args: vec![
            "-I".into(),
            script.to_string_lossy().into_owned(),
            "--engine".into(),
            id.into(),
            "--model".into(),
            info(id).map(|i| i.repo).unwrap_or_default().into(),
        ],
        log: dir.join("helper.log"),
    };
    let mut vc = VoiceCfg::default();
    if let Some(v) = arg("--voice") {
        vc.local.ming_voice = v;
    }
    if let Some(r) = arg("--ref") {
        vc.local.chatterbox_ref = r;
    }
    if let Some(s) = arg("--speed").and_then(|s| s.parse().ok()) {
        vc.tts_speed = s;
    }
    let mut e = LocalEngine::with_spec(id, &vc, spec);
    let t = Instant::now();
    e.start().map_err(|e| anyhow::anyhow!(e))?;
    let ok = e
        .helper
        .as_mut()
        .map(|h| h.wait_ready(Duration::from_secs(120)))
        .transpose()
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("{id}: ready={ok:?} after {} ms", t.elapsed().as_millis());
    let mut all: Vec<f32> = vec![];
    for s in [
        "ACME: the test deploy finished the plan and is applying now.",
        "Hey, Claude Code on Account 2 needs your approval to run the database migration.",
        "Say yes or no.",
    ] {
        let t = Instant::now();
        let mut first = None;
        let mut n = 0usize;
        let mut rate = 24_000;
        e.speak_stream(s, &mut |pcm, r| {
            first.get_or_insert(t.elapsed().as_millis());
            n += pcm.len();
            rate = r;
            all.extend(super::player::resample(pcm, r, super::kokoro::SAMPLE_RATE));
            true
        })
        .map_err(|e| anyhow::anyhow!(e))?;
        let audio = n as f32 / rate as f32;
        println!(
            "  first audio {} ms, {audio:.2} s audio in {:.2} s: {s}",
            first.unwrap_or(0),
            t.elapsed().as_secs_f32()
        );
    }
    std::fs::write(&out, super::kokoro::wav(&all))?;
    println!("wrote {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("godterm-local-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const FAKE: &str = r#"
import json, os, struct, sys, math, time
out = sys.stdout.buffer
mode = sys.argv[1] if len(sys.argv) > 1 else "ok"
def send(h, pcm=None):
    data = b""
    if pcm is not None:
        data = struct.pack("<%df" % len(pcm), *pcm)
        h["bytes"] = len(data)
    out.write((json.dumps(h) + "\n").encode()); out.write(data); out.flush()
if mode == "fail_load":
    send({"ev": "error", "msg": "load failed: no model"}); sys.exit(2)
if mode == "slow_load":
    time.sleep(3)
send({"ev": "ready", "rate": 24000, "load_ms": 5})
for line in sys.stdin:
    req = json.loads(line)
    if req.get("op") == "ping":
        send({"ev": "pong"}); continue
    if req.get("op") != "speak":
        continue
    if mode == "crash":
        sys.exit(3)
    rid = req["id"]
    if mode == "bad_request":
        send({"ev": "error", "id": rid, "msg": "AssertionError: Audio prompt must be longer than 5 seconds!"})
        continue
    with open(os.environ.get("FAKE_LOG", os.devnull), "a") as f:
        f.write(json.dumps(req) + "\n")
    for k in range(4):
        send({"ev": "audio", "id": rid, "rate": 24000}, [math.sin(i / 10.0) * 0.5 for i in range(2400)])
    send({"ev": "done", "id": rid, "cancelled": False})
"#;

    fn python() -> Option<PathBuf> {
        super::super::tts::which("python3")
    }

    fn fake_spec(dir: &Path, mode: &str) -> Option<Spec> {
        let py = python()?;
        let script = dir.join("fake_helper.py");
        std::fs::write(&script, FAKE).unwrap();
        Some(Spec {
            program: py,
            args: vec![
                "-I".into(),
                script.to_string_lossy().into_owned(),
                mode.into(),
            ],
            log: dir.join("helper.log"),
        })
    }

    #[test]
    fn helper_protocol_speaks_and_cancels() {
        let d = fake_dir("proto");
        let Some(spec) = fake_spec(&d, "ok") else {
            eprintln!("no python3: skipped");
            return;
        };
        let mut h = Helper::spawn(&spec).unwrap();
        assert!(h.wait_ready(Duration::from_secs(20)).unwrap());
        assert!(h.ping(Duration::from_secs(5)));
        let cancel = AtomicBool::new(false);
        let mut got = 0usize;
        let first = h
            .speak(
                json!({"text": "Hello."}),
                Duration::from_secs(10),
                Duration::from_secs(10),
                &cancel,
                &mut |pcm, rate| {
                    assert_eq!(rate, 24_000);
                    got += pcm.len();
                    true
                },
            )
            .unwrap();
        assert!(first.is_some());
        assert_eq!(got, 4 * 2400);

        let mut chunks = 0;
        h.speak(
            json!({"text": "Long."}),
            Duration::from_secs(10),
            Duration::from_secs(10),
            &cancel,
            &mut |_, _| {
                chunks += 1;
                false
            },
        )
        .unwrap();
        assert_eq!(chunks, 1);
        let mut got = 0;
        h.speak(
            json!({"text": "Again."}),
            Duration::from_secs(10),
            Duration::from_secs(10),
            &cancel,
            &mut |pcm, _| {
                got += pcm.len();
                true
            },
        )
        .unwrap();
        assert_eq!(got, 4 * 2400);
        let pid = h.pid();
        drop(h);

        std::thread::sleep(Duration::from_millis(100));
        assert!(!crate::platform::alive(pid));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn engine_restarts_after_a_crash_and_stays_out_after_three() {
        let d = fake_dir("crash");
        let Some(spec) = fake_spec(&d, "crash") else {
            return;
        };
        let cfg = VoiceCfg::default();
        let mut e = LocalEngine::with_spec("chatterbox", &cfg, spec);
        for _ in 0..MAX_CRASHES {
            let r = e.speak_stream("Hi.", &mut |_, _| true);
            assert!(r.is_err(), "{r:?}");
            assert!(!e.loaded(), "a crashed helper is dropped");
        }
        let why = e.down_reason().expect("kept out after three crashes");
        assert!(why.contains("paused"), "{why}");

        assert!(e.speak_stream("Hi.", &mut |_, _| true).is_err());
        assert!(!e.loaded());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn engine_speaks_unloads_and_starts_again() {
        let d = fake_dir("engine");
        let Some(spec) = fake_spec(&d, "ok") else {
            return;
        };
        let mut cfg = VoiceCfg::default();
        cfg.local.chatterbox_ref = "~/clip.wav".into();
        let mut e = LocalEngine::with_spec("chatterbox", &cfg, spec);
        e.start().unwrap();
        let mut n = 0;

        e.speak_stream("Hello there.", &mut |p, _| {
            n += p.len();
            true
        })
        .unwrap();
        assert_eq!(n, 9600);
        assert!(e.ready());
        let pid = e.pid().unwrap();
        e.unload();
        assert!(!e.loaded());
        std::thread::sleep(Duration::from_millis(100));
        assert!(!crate::platform::alive(pid));

        e.speak_stream("Back.", &mut |_, _| true).unwrap();
        assert_ne!(e.pid(), Some(pid));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn requests_carry_the_voice() {
        let mut cfg = VoiceCfg::default();
        let spec = Spec {
            program: "/nonexistent".into(),
            args: vec![],
            log: std::env::temp_dir().join("x.log"),
        };
        cfg.local.chatterbox_ref = " ~/me.wav ".into();
        let c = LocalEngine::with_spec("chatterbox", &cfg, spec.clone());
        assert_eq!(c.request("Hi.")["ref"], "~/me.wav");
        cfg.local.ming_voice = "UK_George".into();
        let m = LocalEngine::with_spec("ming", &cfg, spec.clone());
        assert_eq!(m.request("Hi.")["voice"], "UK_George");
        assert_eq!(m.request("Hi.")["instruct"], "");
        cfg.local.ming_voice = "design".into();
        cfg.local.ming_design = "a calm narrator".into();
        let m = LocalEngine::with_spec("ming", &cfg, spec.clone());
        assert_eq!(m.request("Hi.")["instruct"], "a calm narrator");
        assert!(m.first_timeout() > c.first_timeout());

        let mut bad = LocalEngine::with_spec("ming", &cfg, spec);
        assert!(bad.speak_stream("Hi.", &mut |_, _| true).is_err());
    }

    #[test]
    fn a_refused_request_keeps_the_helper() {
        let d = fake_dir("badreq");
        let Some(spec) = fake_spec(&d, "bad_request") else {
            return;
        };
        let cfg = VoiceCfg::default();
        let mut e = LocalEngine::with_spec("chatterbox", &cfg, spec);
        for _ in 0..4 {
            let err = e.speak_stream("Hi.", &mut |_, _| true).unwrap_err();
            assert!(err.contains("5 seconds"), "{err}");
        }

        assert!(e.ready() && e.down_reason().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_load_is_reported() {
        let d = fake_dir("failload");
        let Some(spec) = fake_spec(&d, "fail_load") else {
            return;
        };
        let cfg = VoiceCfg::default();
        let mut e = LocalEngine::with_spec("ming", &cfg, spec);
        let err = e.speak_stream("Hi.", &mut |_, _| true).unwrap_err();
        assert!(err.contains("load failed"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn slow_load_falls_back_until_ready() {
        let d = fake_dir("slow");
        let Some(spec) = fake_spec(&d, "slow_load") else {
            return;
        };
        let mut cfg = VoiceCfg::default();
        cfg.local.timeout_ms = 1000;
        let mut e = LocalEngine::with_spec("chatterbox", &cfg, spec);
        let err = e.speak_stream("Hi.", &mut |_, _| true).unwrap_err();
        assert!(err.contains("still loading"), "{err}");
        assert!(e.loaded(), "it keeps loading");
        std::thread::sleep(Duration::from_secs(3));
        e.speak_stream("Hi.", &mut |_, _| true).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stretch_changes_length_not_rate() {
        let tone: Vec<f32> = (0..48_000).map(|i| (i as f32 / 12.0).sin() * 0.3).collect();
        for speed in [0.75f32, 1.5] {
            let mut st = Stretch::new(speed);
            let mut out = vec![];
            for c in tone.chunks(4000) {
                out.extend(st.push(c, 24_000));
            }
            out.extend(st.finish());
            let want = tone.len() as f32 / speed;
            let got = out.len() as f32;
            assert!(
                (got - want).abs() / want < 0.08,
                "speed {speed}: {got} vs {want}"
            );
            assert!(out.iter().all(|s| s.abs() <= 0.31));
        }
        assert!(Stretch::new(1.0).identity());
    }

    #[test]
    fn install_steps_and_remove_paths() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = fake_dir("install");
        crate::config::testing::set_home(&d);
        let cfg = LocalTtsCfg::default();
        assert!(venv_dir(&cfg).starts_with(&d));
        assert!(
            hub_cache().starts_with(&d),
            "tests never touch the real cache"
        );
        let uv = PathBuf::from("/x/uv");
        let s = install_steps("chatterbox", &cfg, Some(&uv), None).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].argv[..2], ["/x/uv".to_string(), "venv".into()]);
        assert!(s[1].argv.contains(&MLX_AUDIO.to_string()));
        assert!(s[2].argv[3].contains("mlx-community/chatterbox-turbo-fp16"));
        let s = install_steps("ming", &cfg, None, Some(Path::new("/x/python3.12"))).unwrap();
        assert_eq!(s[0].argv[1..3], ["-m".to_string(), "venv".into()]);
        assert!(install_steps("ming", &cfg, None, None).is_err());
        assert!(install_steps("bogus", &cfg, Some(&uv), None).is_err());

        std::fs::create_dir_all(venv_dir(&cfg).join("bin")).unwrap();
        std::fs::write(venv_python(&cfg), "").unwrap();
        std::fs::write(marker("chatterbox"), "{}").unwrap();
        std::fs::write(marker("ming"), "{}").unwrap();
        assert!(installed("chatterbox", &cfg));

        assert_eq!(
            install_steps("ming", &cfg, Some(&uv), None).unwrap().len(),
            2
        );

        let p = remove_paths("chatterbox", &cfg);
        assert!(p.contains(&marker("chatterbox")));
        assert!(p.contains(&model_cache_dir(CHATTERBOX.repo)));
        assert!(!p.contains(&venv_dir(&cfg)));
        remove("chatterbox", &cfg).unwrap();
        assert!(!installed("chatterbox", &cfg) && venv_python(&cfg).exists());

        assert!(remove_paths("ming", &cfg).contains(&venv_dir(&cfg)));
        remove("ming", &cfg).unwrap();
        assert!(!venv_dir(&cfg).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn consent_names_size_memory_speed_and_license() {
        let l = consent_lines("chatterbox").join(" ");
        for w in ["2.9 GB", "5 GB", "0.2 s", "MIT", "Remove"] {
            assert!(l.contains(w), "{w}: {l}");
        }
        assert!(consent_lines("ming").join(" ").contains("Ming-3.0"));
    }

    #[test]
    fn a_finished_install_is_reported_once() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        set_state(Some(Install::Running {
            id: "ming".into(),
            step: "step 3/3".into(),
        }));
        assert!(installing() && take_finished().is_none());
        set_state(Some(Install::Failed {
            id: "ming".into(),
            msg: "no network".into(),
        }));
        assert!(!installing());
        assert!(matches!(take_finished(), Some(Install::Failed { .. })));
        set_state(Some(Install::Done {
            id: "ming".into(),
            msg: "ok".into(),
        }));
        assert!(matches!(take_finished(), Some(Install::Done { .. })));
        assert!(take_finished().is_none());
    }

    #[test]
    fn notes_come_once() {
        reset_notes();
        note_once("chatterbox", "a".into());
        note_once("chatterbox", "b".into());
        assert_eq!(take_notes(), vec!["a".to_string()]);
        assert!(take_notes().is_empty());
    }
}
