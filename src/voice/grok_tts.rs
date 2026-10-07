//! Grok (xAI cloud) text to speech. `POST https://api.x.ai/v1/tts` with
//! `output_format {codec: pcm, sample_rate: 24000}` streams raw 16 bit
//! little endian mono PCM back as it is made, so playback starts on the
//! first chunk. Two ways to authenticate, both a bearer token:
//!   - a grok login (the Grok Build OAuth token in a slot's or ~/.grok's
//!     auth.json, the same token grok itself sends to api.x.ai/v1/stt),
//!   - an xAI API key, kept in the Keychain (service GodTerm-xai-api-key),
//!     never in config.toml.
//! Tokens and keys are never logged or shown.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config::GrokTtsCfg;

pub const API: &str = "https://api.x.ai/v1";
pub const SAMPLE_RATE: u32 = 24_000;
/// Keychain service (and the file or secret-service name elsewhere).
pub const KEY_SERVICE: &str = "GodTerm-xai-api-key";
const KEY_ACCOUNT: &str = "godterm";

/// The built in voices (GET /v1/tts/voices, October 2026), for the picker
/// before the live list is fetched.
pub const KNOWN_VOICES: &[&str] = &[
    "eve", "ara", "leo", "rex", "sal", "altair", "atlas", "aurora", "carina", "castor", "celeste",
    "cosmo", "helios", "helix", "iris", "kepler", "liora", "lumen", "luna", "lux", "naksh",
    "orion", "perseus", "rigel", "sirius", "ursa", "zagan", "zenith",
];

/// The JSON body for one sentence.
pub fn request_body(text: &str, c: &GrokTtsCfg) -> Value {
    let or = |s: &str, d: &'static str| {
        let s = s.trim();
        if s.is_empty() {
            d.to_string()
        } else {
            s.to_string()
        }
    };
    json!({
        "text": text,
        "voice_id": or(&c.voice, "eve"),
        "language": or(&c.language, "en"),
        "output_format": {"codec": "pcm", "sample_rate": SAMPLE_RATE},
        "speed": ((c.speed as f64).clamp(0.7, 1.5) * 100.0).round() / 100.0,
    })
}

/// 16 bit little endian PCM to floats, across chunk boundaries that may
/// split a sample.
#[derive(Default)]
pub struct Pcm {
    carry: Option<u8>,
}

impl Pcm {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<f32> {
        let mut out = Vec::with_capacity(bytes.len() / 2 + 1);
        let mut it = bytes.iter().copied();
        if let Some(lo) = self.carry.take() {
            match it.next() {
                Some(hi) => out.push(i16::from_le_bytes([lo, hi]) as f32 / 32768.0),
                None => {
                    self.carry = Some(lo);
                    return out;
                }
            }
        }
        loop {
            match (it.next(), it.next()) {
                (Some(lo), Some(hi)) => out.push(i16::from_le_bytes([lo, hi]) as f32 / 32768.0),
                (Some(lo), None) => {
                    self.carry = Some(lo);
                    break;
                }
                _ => break,
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    pub id: String,
    pub name: String,
    pub gender: String,
}

pub fn parse_voices(v: &Value) -> Vec<Voice> {
    v.get("voices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|x| {
            let id = x.get("voice_id").and_then(Value::as_str)?.to_string();
            Some(Voice {
                name: x
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                gender: x
                    .get("gender")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                id,
            })
        })
        .collect()
}

/// Why a request did not speak.
#[derive(Debug, Clone, PartialEq)]
pub enum TtsError {
    /// No login or key to use.
    NoCredential(String),
    /// 401: the login expired or the key is wrong.
    Auth,
    /// 403: the team may not use it (out of credits, no subscription).
    Denied,
    RateLimited,
    /// No audio within the timeout.
    Timeout,
    Http(u16),
    Network(String),
    Cancelled,
}

impl TtsError {
    pub fn reason(&self) -> String {
        match self {
            TtsError::NoCredential(w) => w.clone(),
            TtsError::Auth => "the grok login expired or the key is not valid (401); log in again in that account or set an API key".into(),
            TtsError::Denied => "xAI refused it (403): out of credits or no Grok subscription on that account".into(),
            TtsError::RateLimited => "rate limited by xAI (429)".into(),
            TtsError::Timeout => "no audio in time".into(),
            TtsError::Http(c) => format!("xAI answered HTTP {c}"),
            TtsError::Network(e) => format!("network: {e}"),
            TtsError::Cancelled => "cancelled".into(),
        }
    }

    /// Not worth retrying every sentence: skip grok for a while.
    pub fn lasting(&self) -> bool {
        matches!(
            self,
            TtsError::NoCredential(_) | TtsError::Auth | TtsError::Denied
        )
    }
}

impl std::fmt::Display for TtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason())
    }
}

fn status_error(code: u16) -> TtsError {
    match code {
        401 => TtsError::Auth,
        403 => TtsError::Denied,
        429 => TtsError::RateLimited,
        c => TtsError::Http(c),
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(timeout))
        .timeout_recv_response(Some(timeout))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .build()
        .into()
}

fn net_error(e: ureq::Error) -> TtsError {
    match e {
        ureq::Error::StatusCode(c) => status_error(c),
        ureq::Error::Timeout(_) => TtsError::Timeout,
        e => TtsError::Network(e.to_string()),
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    /// Request sent to the first audio byte.
    pub first_audio_ms: u64,
    pub total_ms: u64,
    pub samples: usize,
}

/// Speak one sentence: `sink` gets the samples (24 kHz mono) as they
/// arrive and returns false to stop (barge-in, a newer reply).
pub fn speak(
    base: &str,
    text: &str,
    c: &GrokTtsCfg,
    token: &str,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(&[f32]) -> bool,
) -> Result<Stats, TtsError> {
    let t0 = Instant::now();
    let timeout = Duration::from_millis(c.timeout_ms.clamp(500, 60_000) as u64);
    let resp = agent(timeout)
        .post(&format!("{base}/tts"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "audio/pcm")
        .send_json(request_body(text, c))
        .map_err(net_error)?;
    let mut resp = resp;
    let mut body = resp.body_mut().as_reader();
    let mut pcm = Pcm::default();
    let mut st = Stats::default();
    // 50 ms of audio per read keeps the first chunk early.
    let mut buf = vec![0u8; 2400];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(TtsError::Cancelled);
        }
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(TtsError::Timeout),
            Err(e) => return Err(TtsError::Network(e.to_string())),
        };
        if st.samples == 0 {
            st.first_audio_ms = t0.elapsed().as_millis() as u64;
        }
        let s = pcm.push(&buf[..n]);
        st.samples += s.len();
        if !s.is_empty() && !sink(&s) {
            return Err(TtsError::Cancelled);
        }
    }
    st.total_ms = t0.elapsed().as_millis() as u64;
    Ok(st)
}

/// The voices this credential can use (free; used by Test and doctor).
pub fn fetch_voices(base: &str, token: &str) -> Result<Vec<Voice>, TtsError> {
    let mut r = agent(Duration::from_secs(6))
        .get(&format!("{base}/tts/voices"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/json")
        .call()
        .map_err(net_error)?;
    let v: Value = r
        .body_mut()
        .read_json()
        .map_err(|e| TtsError::Network(format!("bad voices reply: {e}")))?;
    Ok(parse_voices(&v))
}

// ---- Credentials ----

/// A grok login that can speak: a grok account slot or the main ~/.grok.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// "main" or the account name (what `voice.grok.source` holds).
    pub id: String,
    pub label: String,
    pub home: PathBuf,
    /// auth.json is there (its contents are only read to speak).
    pub logged_in: bool,
}

/// The grok accounts of `cfg`, then the main ~/.grok.
pub fn sources(cfg: &crate::config::Config) -> Vec<Source> {
    let mut v: Vec<Source> = cfg
        .accounts
        .iter()
        .filter(|a| a.harness() == crate::harness::Harness::Grok)
        .map(|a| {
            let home = a.config_dir();
            Source {
                id: a.name.clone(),
                label: a.display().to_string(),
                logged_in: home.join("auth.json").is_file(),
                home,
            }
        })
        .collect();
    let main = crate::config::dirs().main_grok;
    v.push(Source {
        id: "main".into(),
        label: "Main ~/.grok".into(),
        logged_in: main.join("auth.json").is_file(),
        home: main,
    });
    v
}

/// Logins xAI refused this run (out of credits, expired), by source id,
/// for the Settings picker ("out of credits") and auto's choice.
static REFUSED: std::sync::Mutex<Vec<(String, chrono::DateTime<chrono::Local>)>> =
    std::sync::Mutex::new(Vec::new());

pub fn note_refused(id: &str) {
    let mut r = REFUSED.lock().unwrap_or_else(|e| e.into_inner());
    if !id.is_empty() && !r.iter().any(|x| x.0 == id) {
        r.push((id.to_string(), chrono::Local::now()));
    }
}

/// When xAI last refused that login ("out of credits at 10:20").
pub fn refused_at(id: &str) -> Option<chrono::DateTime<chrono::Local>> {
    REFUSED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|x| x.0 == id)
        .map(|x| x.1)
}

/// Forget a refusal (the user logged in again, or refreshed).
pub fn clear_refusal(id: &str) {
    REFUSED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|x| x.0 != id);
}

#[cfg(test)]
pub fn clear_refused() {
    REFUSED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

pub fn refused_ids() -> Vec<String> {
    REFUSED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|x| x.0.clone())
        .collect()
}

/// What auto signs in with now: the first logged in login xAI has not
/// refused.
pub fn auto_choice<'a>(all: &'a [Source], refused: &[String]) -> Option<&'a Source> {
    all.iter().find(|s| s.logged_in && !refused.contains(&s.id))
}

/// The sources from config.toml (the speech worker has no Config).
pub fn sources_now() -> Vec<Source> {
    let cfg = std::fs::read_to_string(crate::config::Config::path())
        .ok()
        .and_then(|t| crate::config::Config::parse(&t).ok())
        .unwrap_or_default();
    sources(&cfg)
}

/// Which source `voice.grok.source` picks: the named one, or for "auto"
/// the first logged in account, then ~/.grok.
pub fn pick_source<'a>(want: &str, all: &'a [Source]) -> Option<&'a Source> {
    match want.trim() {
        "" | "auto" => all.iter().find(|s| s.logged_in),
        w => all.iter().find(|s| s.id == w),
    }
}

/// A token to speak with, and where it came from (for the HUD, never the
/// token itself).
pub struct Cred {
    pub token: String,
    pub from: String,
    /// The source id ("main", an account) for a grok login; "" for a key.
    pub id: String,
}

pub fn credential(c: &GrokTtsCfg, all: &[Source], store: &dyn KeyStore) -> Result<Cred, TtsError> {
    credential_except(c, all, store, &[])
}

/// With source "auto", logins that xAI refused (401, 403: an expired
/// login, a team with no credits) are passed over for the next one.
pub fn credential_except(
    c: &GrokTtsCfg,
    all: &[Source],
    store: &dyn KeyStore,
    refused: &[String],
) -> Result<Cred, TtsError> {
    let others: Vec<Source> = all
        .iter()
        .filter(|s| !refused.contains(&s.id))
        .cloned()
        .collect();
    let all: &[Source] = if c.source == "auto" || c.source.is_empty() {
        &others
    } else {
        all
    };
    if c.auth == "api_key" {
        return match store.get() {
            Ok(Some(k)) => Ok(Cred {
                token: k,
                from: format!("API key ({})", store.place()),
                id: String::new(),
            }),
            Ok(None) => Err(TtsError::NoCredential(
                "no xAI API key saved (Settings > Voice and Audio > Grok voice)".into(),
            )),
            Err(e) => Err(TtsError::NoCredential(format!("API key unreadable: {e}"))),
        };
    }
    let s = pick_source(&c.source, all).ok_or_else(|| {
        TtsError::NoCredential(if c.source == "auto" || c.source.is_empty() {
            if refused.is_empty() {
                "no grok account is logged in".to_string()
            } else {
                "xAI refused every grok login here (expired, or no credits)".to_string()
            }
        } else {
            format!("no grok login named {}", c.source)
        })
    })?;
    match crate::harness::grok::token(&s.home) {
        Some(token) => Ok(Cred {
            token,
            from: format!("grok login {}", s.label),
            id: s.id.clone(),
        }),
        None => Err(TtsError::NoCredential(format!(
            "{} is not logged in to grok",
            s.label
        ))),
    }
}

/// Where the xAI API key is kept. Behind a trait so tests never touch the
/// real Keychain.
pub trait KeyStore: Send + Sync {
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, key: &str) -> Result<(), String>;
    fn remove(&self) -> Result<(), String>;
    /// "Keychain", "secret service", "~/.godterm file".
    fn place(&self) -> &'static str;
}

/// The login Keychain through /usr/bin/security; the key goes in on stdin
/// (`security -i`), never on a command line.
pub struct MacKeychain;

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<String> {
    if s.is_empty() || s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?)
        .ok()
        .filter(|t| t.chars().all(|c| c.is_ascii_graphic()))
}

/// What `security -w` printed: the text, or hex when it chose hex.
pub fn keychain_text(out: &str) -> String {
    let s = out.trim_end_matches('\n');
    if s.starts_with("xai-") {
        return s.to_string();
    }
    unhex(s).unwrap_or_else(|| s.to_string())
}

impl KeyStore for MacKeychain {
    fn get(&self) -> Result<Option<String>, String> {
        let out = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-a",
                KEY_ACCOUNT,
                "-s",
                KEY_SERVICE,
                "-w",
            ])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("running security: {e}"))?;
        if !out.status.success() {
            return Ok(None);
        }
        let s = keychain_text(&String::from_utf8_lossy(&out.stdout));
        Ok((!s.is_empty()).then_some(s))
    }

    fn set(&self, key: &str) -> Result<(), String> {
        use std::io::Write;
        let line = format!(
            "add-generic-password -U -a \"{KEY_ACCOUNT}\" -s \"{KEY_SERVICE}\" -l \"GodTerm xAI API key\" -X \"{}\"\n",
            hex(key)
        );
        if line.len() > 4000 {
            return Err("that key is too long".into());
        }
        let mut child = std::process::Command::new("security")
            .arg("-i")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("running security: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        let st = child.wait().map_err(|e| e.to_string())?;
        if !st.success() || !matches!(self.get(), Ok(Some(_))) {
            return Err("the Keychain did not take it".into());
        }
        Ok(())
    }

    fn remove(&self) -> Result<(), String> {
        let st = std::process::Command::new("security")
            .args([
                "delete-generic-password",
                "-a",
                KEY_ACCOUNT,
                "-s",
                KEY_SERVICE,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        // Not there is removed too.
        let _ = st;
        Ok(())
    }

    fn place(&self) -> &'static str {
        "Keychain"
    }
}

/// Linux: the secret service through `secret-tool` (the key on stdin).
pub struct SecretTool;

impl KeyStore for SecretTool {
    fn get(&self) -> Result<Option<String>, String> {
        let out = std::process::Command::new("secret-tool")
            .args(["lookup", "service", KEY_SERVICE])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((out.status.success() && !s.is_empty()).then_some(s))
    }

    fn set(&self, key: &str) -> Result<(), String> {
        use std::io::Write;
        let mut child = std::process::Command::new("secret-tool")
            .args([
                "store",
                "--label=GodTerm xAI API key",
                "service",
                KEY_SERVICE,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(key.as_bytes())
            .map_err(|e| e.to_string())?;
        match child.wait() {
            Ok(s) if s.success() => Ok(()),
            _ => Err("secret-tool could not store it".into()),
        }
    }

    fn remove(&self) -> Result<(), String> {
        let _ = std::process::Command::new("secret-tool")
            .args(["clear", "service", KEY_SERVICE])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        Ok(())
    }

    fn place(&self) -> &'static str {
        "secret service"
    }
}

/// An owner only (0600) file under GodTerm's home: Linux without a secret
/// service, and every unit test.
pub struct FileStore(pub PathBuf);

impl KeyStore for FileStore {
    fn get(&self) -> Result<Option<String>, String> {
        match std::fs::read_to_string(&self.0) {
            Ok(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, key: &str) -> Result<(), String> {
        if let Some(d) = self.0.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        crate::config::write_private(&self.0, key.trim()).map_err(|e| e.to_string())
    }

    fn remove(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.0) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn place(&self) -> &'static str {
        "~/.godterm file"
    }
}

/// The store for this machine. Tests always get a file in the test home.
pub fn key_store() -> Box<dyn KeyStore> {
    let file = || {
        FileStore(
            crate::config::app_home()
                .join("secrets")
                .join("xai-api-key"),
        )
    };
    if cfg!(test) {
        return Box::new(file());
    }
    if cfg!(target_os = "macos") {
        return Box::new(MacKeychain);
    }
    if crate::voice::tts::which("secret-tool").is_some() {
        return Box::new(SecretTool);
    }
    Box::new(file())
}

/// Whether a key is saved, cached so the Settings screen can show it every
/// frame without asking the Keychain: 0 unknown, 1 none, 2 saved.
static KEY_STATUS: AtomicU8 = AtomicU8::new(0);

pub fn key_saved() -> Option<bool> {
    match KEY_STATUS.load(Ordering::SeqCst) {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

/// Look again (off the UI thread).
pub fn refresh_key_status() {
    let saved = matches!(key_store().get(), Ok(Some(_)));
    KEY_STATUS.store(if saved { 2 } else { 1 }, Ordering::SeqCst);
}

pub fn save_key(key: &str) -> Result<&'static str, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("empty key".into());
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("a key has no spaces".into());
    }
    let s = key_store();
    s.set(key)?;
    KEY_STATUS.store(2, Ordering::SeqCst);
    Ok(s.place())
}

pub fn remove_key() -> Result<(), String> {
    key_store().remove()?;
    KEY_STATUS.store(1, Ordering::SeqCst);
    Ok(())
}

/// Test the configured credential with the (free) voices list:
/// "ok: grok login demo8, 28 voices" or why not.
pub fn check(c: &GrokTtsCfg, base: &str) -> Result<String, String> {
    let all = sources_now();
    let cred = credential(c, &all, key_store().as_ref()).map_err(|e| e.reason())?;
    let t0 = Instant::now();
    let v = fetch_voices(base, &cred.token).map_err(|e| e.reason())?;
    Ok(format!(
        "{} works: {} voices, answered in {} ms",
        cred.from,
        v.len(),
        t0.elapsed().as_millis()
    ))
}

/// The voices for the picker: fetched with the configured credential
/// (free), else the known list.
pub fn live_voices(c: &GrokTtsCfg) -> Vec<String> {
    let all = sources_now();
    let known = || KNOWN_VOICES.iter().map(|s| s.to_string()).collect();
    if cfg!(test) {
        return known();
    }
    match credential(c, &all, key_store().as_ref()) {
        Ok(cred) => fetch_voices(API, &cred.token)
            .map(|v| v.into_iter().map(|x| x.id).collect())
            .unwrap_or_else(|_| known()),
        Err(_) => known(),
    }
}

/// The last Settings > Test result, shown under the rows.
pub static LAST_CHECK: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Test the credential in the background (Settings > Test).
pub fn check_in_background(c: GrokTtsCfg) {
    *LAST_CHECK.lock().unwrap_or_else(|e| e.into_inner()) = Some("testing...".into());
    std::thread::spawn(move || {
        let r = if cfg!(test) {
            Err("not in tests".to_string())
        } else {
            check(&c, API)
        };
        let line = match r {
            Ok(s) => s,
            Err(e) => format!("not working: {e}"),
        };
        crate::log::info(&format!("grok tts test: {line}"));
        *LAST_CHECK.lock().unwrap_or_else(|e| e.into_inner()) = Some(line);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_decode() {
        let c = GrokTtsCfg {
            voice: "ara".into(),
            speed: 2.0,
            ..Default::default()
        };
        let b = request_body("Hello.", &c);
        assert_eq!(b["voice_id"], "ara");
        assert_eq!(b["language"], "en");
        assert_eq!(b["output_format"]["codec"], "pcm");
        assert_eq!(b["output_format"]["sample_rate"], 24000);
        assert_eq!(b["speed"], 1.5, "clamped to what xAI takes");
        // A real reply's first bytes (16 bit LE mono 24 kHz), split oddly.
        let bytes = include_bytes!("../../tests/fixtures/grok_tts_hello.pcm");
        let mut p = Pcm::default();
        let mut all = p.push(&bytes[..7]);
        all.extend(p.push(&bytes[7..]));
        assert_eq!(all.len(), bytes.len() / 2);
        let i0 = i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0;
        assert_eq!(all[0], i0);
        assert!(all.iter().all(|s| (-1.0..=1.0).contains(s)));
        assert!(
            all.iter().any(|s| s.abs() > 0.05),
            "real speech, not silence"
        );
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/grok_tts_voices.json"))
                .unwrap();
        let voices = parse_voices(&v);
        assert!(voices.len() >= 20);
        assert!(voices.iter().any(|x| x.id == "eve" && x.gender == "female"));
        for k in KNOWN_VOICES {
            assert!(voices.iter().any(|x| x.id == *k), "{k}");
        }
    }

    #[test]
    fn hex_and_text_from_the_keychain() {
        assert_eq!(keychain_text("xai-abc123\n"), "xai-abc123");
        assert_eq!(keychain_text(&format!("{}\n", hex("xai-Zz9"))), "xai-Zz9");
        assert_eq!(keychain_text("plain-key"), "plain-key");
    }

    #[test]
    fn credentials_by_auth_and_source() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("godterm-grokcred-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        crate::config::testing::set_home(&d.join("home"));
        let a = d.join("a");
        let b = d.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(
            b.join("auth.json"),
            r#"{"tokens":{"access_token":"tok-b"}}"#,
        )
        .unwrap();
        let all = vec![
            Source {
                id: "a".into(),
                label: "A".into(),
                home: a.clone(),
                logged_in: false,
            },
            Source {
                id: "b".into(),
                label: "B".into(),
                home: b.clone(),
                logged_in: true,
            },
        ];
        let store = FileStore(d.join("key"));
        let mut c = GrokTtsCfg::default();
        // auto: the first logged in one.
        let cr = credential(&c, &all, &store).ok().unwrap();
        assert_eq!(
            (cr.token.as_str(), cr.from.as_str()),
            ("tok-b", "grok login B")
        );
        c.source = "a".into();
        let e = credential(&c, &all, &store).err().unwrap();
        assert!(e.lasting() && e.reason().contains("not logged in"), "{e}");
        // API key: none, then saved (owner only), then removed.
        c.auth = "api_key".into();
        assert!(matches!(
            credential(&c, &all, &store),
            Err(TtsError::NoCredential(_))
        ));
        store.set("xai-test-key").unwrap();
        #[cfg(unix)]
        {
            use crate::platform::PermissionsExt;
            let m = std::fs::metadata(d.join("key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o077, 0, "owner only");
        }
        assert_eq!(
            credential(&c, &all, &store).ok().unwrap().token,
            "xai-test-key"
        );
        store.remove().unwrap();
        assert!(credential(&c, &all, &store).is_err());
        // The store tests use is never the Keychain.
        assert_eq!(key_store().place(), "~/.godterm file");
        save_key("xai-from-settings").unwrap();
        assert_eq!(key_saved(), Some(true));
        let cfg_text = std::fs::read_to_string(crate::config::Config::path()).unwrap_or_default();
        assert!(!cfg_text.contains("xai-from-settings"));
        remove_key().unwrap();
        assert_eq!(key_saved(), Some(false));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A local stand in for api.x.ai: streams PCM in chunks, or answers
    /// with an error status.
    fn serve(status: u16, body: Vec<u8>) -> String {
        use std::io::Write;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                // Read the whole request first: closing with unread bytes
                // resets the connection on Windows before the client reads
                // the reply.
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut req: Vec<u8> = vec![];
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(n) = s.read(&mut chunk) else { break };
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&req).to_lowercase();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if req.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: audio/pcm\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                );
                let _ = s.write_all(head.as_bytes());
                for c in body.chunks(1001) {
                    let _ = s.write_all(format!("{:x}\r\n", c.len()).as_bytes());
                    let _ = s.write_all(c);
                    let _ = s.write_all(b"\r\n");
                    let _ = s.flush();
                }
                let _ = s.write_all(b"0\r\n\r\n");
                let _ = s.flush();
                // Close gracefully: no more writes, then wait for the
                // client to finish reading and hang up.
                let _ = s.shutdown(std::net::Shutdown::Write);
                let _ = s.read(&mut chunk);
            }
        });
        format!("http://{addr}/v1")
    }

    #[test]
    fn streams_and_maps_errors_without_network() {
        let bytes = include_bytes!("../../tests/fixtures/grok_tts_hello.pcm").to_vec();
        let base = serve(200, bytes.clone());
        let c = GrokTtsCfg::default();
        let cancel = AtomicBool::new(false);
        let mut chunks = 0;
        let mut got = 0;
        let st = speak(&base, "Hi.", &c, "tok", &cancel, &mut |s| {
            chunks += 1;
            got += s.len();
            true
        })
        .unwrap();
        assert_eq!(got, bytes.len() / 2);
        assert_eq!(st.samples, got);
        assert!(chunks > 1, "played as it arrives");
        // Barge-in: the sink says stop.
        let base = serve(200, bytes.clone());
        let r = speak(&base, "Hi.", &c, "tok", &cancel, &mut |_| false);
        assert_eq!(r, Err(TtsError::Cancelled));
        for (code, want) in [
            (401, TtsError::Auth),
            (403, TtsError::Denied),
            (429, TtsError::RateLimited),
        ] {
            let base = serve(code, b"{}".to_vec());
            let r = speak(&base, "Hi.", &c, "tok", &cancel, &mut |_| true);
            assert_eq!(r, Err(want), "{code}");
        }
        assert!(TtsError::Auth.lasting() && !TtsError::Timeout.lasting());
    }
}
