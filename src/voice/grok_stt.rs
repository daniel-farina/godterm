//! Grok (xAI cloud) speech recognition: each finished utterance (after the
//! endpointer, the speaker lock and noise suppression, so background
//! voices never leave the machine) goes to `POST https://api.x.ai/v1/stt`
//! as a 16 kHz WAV in a multipart form, with the vocabulary as keyterms.
//! The reply is the text with word timings. The login is the one Grok
//! talk back uses (`[voice.grok]`: a grok login or the Keychain key).
//! Partials stay local (whisper or Apple) when they are on.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::grok_tts::{self, TtsError};
use crate::config::GrokTtsCfg;

pub const MODEL: &str = "grok-voice-transcribe-2.0";
/// The most keyterms the API takes, and their longest.
const MAX_TERMS: usize = 100;
const MAX_TERM: usize = 50;

/// The form fields for one utterance (before the file).
pub fn fields(language: &str, vocab: &[String]) -> Vec<(&'static str, String)> {
    let mut f = vec![("model", MODEL.to_string()), ("format", "true".into())];
    let lang = language.trim();
    if !lang.is_empty() && lang != "auto" {
        // whisper style "en" or a locale "en-US": the API takes the code.
        f.push((
            "language",
            lang.split(['-', '_']).next().unwrap_or(lang).to_lowercase(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for t in vocab {
        let t = t.trim();
        if t.is_empty() || t.chars().count() > MAX_TERM || !seen.insert(t.to_lowercase()) {
            continue;
        }
        if seen.len() > MAX_TERMS {
            break;
        }
        f.push(("keyterm", t.to_string()));
    }
    f
}

/// A multipart/form-data body: (content type, body).
pub fn multipart(fields: &[(&str, String)], wav: &[u8], boundary: &str) -> (String, Vec<u8>) {
    let mut b: Vec<u8> = vec![];
    for (k, v) in fields {
        b.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    b.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"utterance.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(wav);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), b)
}

/// The text of a reply (words joined when there is no text).
pub fn parse(v: &Value) -> Option<String> {
    if let Some(t) = v.get("text").and_then(Value::as_str) {
        return Some(t.trim().to_string());
    }
    let words: Vec<&str> = v
        .get("words")?
        .as_array()?
        .iter()
        .filter_map(|w| w.get("text").and_then(Value::as_str))
        .collect();
    Some(words.join(" "))
}

/// One request: the WAV, at most `timeout` for the reply.
pub fn transcribe(
    base: &str,
    wav: &[u8],
    language: &str,
    vocab: &[String],
    token: &str,
    timeout: Duration,
) -> Result<String, TtsError> {
    let boundary = format!("godterm{}", &crate::session_ops::new_uuid()[..12]);
    let (ctype, body) = multipart(&fields(language, vocab), wav, &boundary);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(timeout))
        .timeout_global(Some(timeout))
        .build()
        .into();
    let mut r = agent
        .post(&format!("{base}/stt"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", &ctype)
        .send(&body[..])
        .map_err(|e| match e {
            ureq::Error::StatusCode(401) => TtsError::Auth,
            ureq::Error::StatusCode(403) => TtsError::Denied,
            ureq::Error::StatusCode(429) => TtsError::RateLimited,
            ureq::Error::StatusCode(c) => TtsError::Http(c),
            ureq::Error::Timeout(_) => TtsError::Timeout,
            e => TtsError::Network(e.to_string()),
        })?;
    let v: Value = r
        .body_mut()
        .read_json()
        .map_err(|e| TtsError::Network(format!("bad reply: {e}")))?;
    parse(&v).ok_or_else(|| TtsError::Network("no text in the reply".into()))
}

/// The recognizer: the shared Grok login, and a pause after a lasting
/// failure (no login, 401, 403) so every utterance does not retry it.
pub struct GrokStt {
    pub auth: GrokTtsCfg,
    pub base: String,
    pub language: String,
    pub timeout: Duration,
    cred: Option<grok_tts::Cred>,
    down: Option<(Instant, String)>,
    /// Logins xAI refused (auto passes over them).
    refused: Vec<String>,
    /// Where the login came from (for the HUD), never the token.
    pub from: String,
}

impl GrokStt {
    pub fn new(auth: GrokTtsCfg, language: &str, timeout_ms: u32) -> GrokStt {
        GrokStt {
            auth,
            base: grok_tts::API.into(),
            language: language.to_string(),
            timeout: Duration::from_millis(timeout_ms.clamp(1000, 30_000) as u64),
            cred: None,
            down: None,
            refused: vec![],
            from: String::new(),
        }
    }

    /// Why it is skipped now, after a lasting failure.
    pub fn down_reason(&self) -> Option<&str> {
        self.down
            .as_ref()
            .filter(|(at, _)| at.elapsed() < super::engines::GROK_RETRY)
            .map(|(_, w)| w.as_str())
    }

    /// The utterance's text, or why not (the caller falls back to local
    /// recognition).
    pub fn transcribe(&mut self, samples: &[i16], vocab: &[String]) -> Result<String, String> {
        if let Some(w) = self.down_reason() {
            return Err(w.to_string());
        }
        let wav = super::audio::wav_bytes(samples);
        // A refused login (auto): the next one, at most a few times.
        for _ in 0..4 {
            if self.cred.is_none() {
                match grok_tts::credential_except(
                    &self.auth,
                    &grok_tts::sources_now(),
                    grok_tts::key_store().as_ref(),
                    &self.refused,
                ) {
                    Ok(c) => {
                        self.from = c.from.clone();
                        self.cred = Some(c);
                    }
                    Err(e) => {
                        self.down = Some((Instant::now(), e.reason()));
                        return Err(e.reason());
                    }
                }
            }
            let (token, id) = self
                .cred
                .as_ref()
                .map(|c| (c.token.clone(), c.id.clone()))
                .unwrap_or_default();
            match transcribe(
                &self.base,
                &wav,
                &self.language,
                vocab,
                &token,
                self.timeout,
            ) {
                Ok(t) => return Ok(t),
                Err(e) if e.lasting() && !id.is_empty() => {
                    crate::log::info(&format!(
                        "grok stt: {} refused ({}), trying the next login",
                        self.from,
                        e.reason()
                    ));
                    grok_tts::note_refused(&id);
                    self.refused.push(id);
                    self.cred = None;
                }
                Err(e) => {
                    if e.lasting() {
                        self.down = Some((Instant::now(), e.reason()));
                        self.cred = None;
                    }
                    return Err(e.reason());
                }
            }
        }
        Err("no grok login was accepted".into())
    }
}

/// The last Settings > Test recognition result.
pub static LAST_CHECK: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Settings > Test recognition: record 3 s from the microphone, send it
/// to Grok, and say what came back and how long it took.
pub fn test_recognition(cfg: crate::config::VoiceCfg) {
    *LAST_CHECK.lock().unwrap_or_else(|e| e.into_inner()) =
        Some("recording 3 s, say something...".into());
    std::thread::spawn(move || {
        let line = (|| -> Result<String, String> {
            if cfg!(test) || crate::voice::tts::audio_muted() {
                return Err("no microphone in tests".into());
            }
            let mut cap = super::capture::open(&cfg, &super::Source::Mic(cfg.device.clone()))
                .map_err(|e| format!("cannot open the mic: {e}"))?;
            let mut pcm: Vec<i16> = vec![];
            let mut frame = vec![];
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(3) {
                frame.clear();
                if cap.frame(&mut frame).is_none() {
                    break;
                }
                pcm.extend_from_slice(&frame);
            }
            drop(cap);
            let mut g = GrokStt::new(cfg.grok.clone(), &cfg.language, cfg.grok_stt.timeout_ms);
            let t = Instant::now();
            let text = g.transcribe(&pcm, &[])?;
            Ok(format!(
                "heard \"{}\" in {} ms ({})",
                text,
                t.elapsed().as_millis(),
                g.from
            ))
        })()
        .unwrap_or_else(|e| format!("not working: {e}"));
        crate::log::info(&format!("grok stt test: {line}"));
        *LAST_CHECK.lock().unwrap_or_else(|e| e.into_inner()) = Some(line);
    });
}

/// The vocabulary string the recognizers share ("hey god, t12, botmesh")
/// as keyterms.
pub fn terms(vocab: &str) -> Vec<String> {
    vocab
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_reply() {
        let f = fields("en-US", &terms("GodTerm, botmesh, , godterm, t12"));
        assert_eq!(f[0], ("model", MODEL.to_string()));
        assert!(f.contains(&("language", "en".into())));
        let keys: Vec<&String> = f
            .iter()
            .filter(|(k, _)| *k == "keyterm")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(
            keys,
            ["GodTerm", "botmesh", "t12"],
            "deduplicated, no empties"
        );
        assert!(!fields("auto", &[]).iter().any(|(k, _)| *k == "language"));
        let (ct, body) = multipart(&f, b"RIFFdata", "BOUND");
        assert_eq!(ct, "multipart/form-data; boundary=BOUND");
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("name=\"model\"\r\n\r\ngrok-voice-transcribe-2.0\r\n"));
        assert!(s.contains(
            "filename=\"utterance.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFFdata\r\n--BOUND--"
        ));
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/grok_stt_reply.json")).unwrap();
        assert_eq!(parse(&v).as_deref(), Some("Open a new tab on account two."));
        let words: Value = serde_json::json!({"words": [{"text": "hey"}, {"text": "god"}]});
        assert_eq!(parse(&words).as_deref(), Some("hey god"));
    }

    fn serve(status: u16, body: &'static str) -> String {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut req = vec![];
                let mut chunk = [0u8; 8192];
                loop {
                    let Ok(n) = s.read(&mut chunk) else { break };
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&chunk[..n]);
                    let t = String::from_utf8_lossy(&req).to_lowercase();
                    if let Some(end) = t.find("\r\n\r\n") {
                        let len = t
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if req.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let _ = s.write_all(
                    format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                        .as_bytes(),
                );
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Write);
                let _ = s.read(&mut chunk);
            }
        });
        format!("http://{addr}/v1")
    }

    #[test]
    fn errors_map_and_lasting_ones_pause_it() {
        let wav = super::super::audio::wav_bytes(&[0i16; 1600]);
        let t = Duration::from_secs(3);
        let ok = serve(200, r#"{"text":"open a tab"}"#);
        assert_eq!(
            transcribe(&ok, &wav, "en", &[], "tok", t).unwrap(),
            "open a tab"
        );
        for (code, want) in [
            (401, TtsError::Auth),
            (403, TtsError::Denied),
            (429, TtsError::RateLimited),
        ] {
            let b = serve(code, "{}");
            assert_eq!(
                transcribe(&b, &wav, "en", &[], "tok", t),
                Err(want),
                "{code}"
            );
        }
        // No login at all: a lasting reason, and it stays out a while.
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("godterm-gstt-{}", std::process::id()));
        crate::config::testing::set_home(&d);
        let mut g = GrokStt::new(GrokTtsCfg::default(), "en", 3000);
        let e = g.transcribe(&[0; 1600], &[]).unwrap_err();
        assert!(e.contains("logged in"), "{e}");
        assert!(g.down_reason().is_some());
        let _ = std::fs::remove_dir_all(&d);
    }
}
