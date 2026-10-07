//! Talk back engines behind one interface: Kokoro (local neural), macOS
//! `say` (local) and Grok (xAI cloud). Each makes PCM that GodTerm plays
//! itself, so the echo reference, volume and barge-in work the same for
//! all. `tts_engine` is tried first, then `tts_fallback` in order, when an
//! engine fails or times out.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::grok_tts::{self, TtsError};
use super::kokoro::Kokoro;
use crate::config::{GrokTtsCfg, VoiceCfg};

/// One engine: speaks text as PCM chunks (samples, sample rate) handed to
/// `sink`, which returns false to stop (barge-in, a newer reply).
pub trait TtsEngine {
    fn name(&self) -> String;
    fn speak_stream(
        &mut self,
        text: &str,
        sink: &mut dyn FnMut(&[f32], u32) -> bool,
    ) -> Result<(), String>;
    #[allow(dead_code)] // the Settings picker reads the lists directly
    fn voices(&self) -> Vec<String>;
    fn cancel(&self);
}

/// The engines to try, in order: the chosen one, then the fallbacks
/// (Kokoro only when it is installed). Off (or talk back off) is none.
pub fn chain(cfg: &VoiceCfg) -> Vec<&'static str> {
    if !cfg.tts || cfg.tts_engine == "off" {
        return vec![];
    }
    let known = |s: &str| match s.trim() {
        "grok" => Some("grok"),
        "say" => Some("say"),
        "kokoro" => Some("kokoro"),
        _ => None,
    };
    let mut v: Vec<&'static str> = vec![];
    for e in
        std::iter::once(cfg.tts_engine.as_str()).chain(cfg.tts_fallback.iter().map(String::as_str))
    {
        let Some(n) = known(e) else { continue };
        if n == "kokoro" && super::tts::kokoro_problem(cfg).is_some() {
            continue;
        }
        if !v.contains(&n) {
            v.push(n);
        }
    }
    if v.is_empty() {
        v.push("say");
    }
    v
}

/// Speak `text` with the first engine that works; the names of those that
/// failed and why.
pub fn speak_with_fallback(
    engines: &mut [&mut dyn TtsEngine],
    text: &str,
    sink: &mut dyn FnMut(&[f32], u32) -> bool,
) -> (Option<String>, Vec<(String, String)>) {
    let mut failed = vec![];
    for e in engines.iter_mut() {
        match e.speak_stream(text, sink) {
            Ok(()) => return (Some(e.name()), failed),
            Err(why) => failed.push((e.name(), why)),
        }
    }
    (None, failed)
}

/// Kokoro, borrowed for one sentence.
pub struct KokoroEngine<'a> {
    pub k: &'a mut Kokoro,
    pub voice: String,
    pub speed: f32,
}

impl TtsEngine for KokoroEngine<'_> {
    fn name(&self) -> String {
        format!("kokoro {}", self.voice)
    }

    fn speak_stream(
        &mut self,
        text: &str,
        sink: &mut dyn FnMut(&[f32], u32) -> bool,
    ) -> Result<(), String> {
        let (samples, _) = self
            .k
            .synth(text, &self.voice, self.speed)
            .map_err(|e| e.to_string())?;
        sink(&samples, super::kokoro::SAMPLE_RATE);
        Ok(())
    }

    fn voices(&self) -> Vec<String> {
        // (Settings lists them from the voices file.)
        vec![]
    }

    fn cancel(&self) {}
}

/// macOS `say`, rendered to 16 bit PCM and played like the others.
pub struct SayEngine {
    pub voice: Option<String>,
    pub speed: f32,
    pub dir: PathBuf,
}

pub const SAY_RATE: u32 = 22_050;

/// The samples of a 16 bit mono WAV (any chunk layout).
pub fn wav_samples(b: &[u8]) -> Option<(Vec<f32>, u32)> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    let mut rate = SAY_RATE;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().ok()?) as usize;
        let body = b.get(i + 8..(i + 8 + len).min(b.len()))?;
        if id == b"fmt " && body.len() >= 8 {
            rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
        }
        if id == b"data" {
            let s = body
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect();
            return Some((s, rate));
        }
        i += 8 + len + (len & 1);
    }
    None
}

impl TtsEngine for SayEngine {
    fn name(&self) -> String {
        "say".into()
    }

    fn speak_stream(
        &mut self,
        text: &str,
        sink: &mut dyn FnMut(&[f32], u32) -> bool,
    ) -> Result<(), String> {
        let _ = std::fs::create_dir_all(&self.dir);
        let f = self.dir.join(format!(
            "say-{}-{}.wav",
            std::process::id(),
            crate::session_ops::new_uuid()[..8].to_string()
        ));
        let mut cmd = Command::new("say");
        if let Some(v) = self
            .voice
            .as_deref()
            .filter(|v| !v.is_empty() && *v != "system")
        {
            cmd.args(["-v", v]);
        }
        let wpm = (175.0 * self.speed.clamp(0.5, 2.0)).round() as u32;
        let ok = cmd
            .args(["-r", &wpm.to_string(), "--data-format=LEI16@22050", "-o"])
            .arg(&f)
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let data = std::fs::read(&f).ok();
        let _ = std::fs::remove_file(&f);
        let (samples, rate) = ok
            .then_some(data)
            .flatten()
            .and_then(|d| wav_samples(&d))
            .ok_or("say could not speak it")?;
        sink(&samples, rate);
        Ok(())
    }

    fn voices(&self) -> Vec<String> {
        vec![]
    }

    fn cancel(&self) {}
}

/// Grok (xAI): streams 24 kHz PCM while it is made.
pub struct GrokEngine {
    pub cfg: GrokTtsCfg,
    pub base: String,
    cred: Option<grok_tts::Cred>,
    /// A lasting failure (no login, 401, 403): skipped until then.
    down: Option<(Instant, String)>,
    cancel: Arc<AtomicBool>,
    /// For the HUD: where the credential came from.
    pub from: String,
    /// Time to first audio of the last sentence.
    pub last_first_audio_ms: Option<u64>,
    /// Logins xAI refused (source auto passes over them).
    refused: Vec<String>,
}

/// A lasting failure keeps Grok out this long before it is tried again.
pub const GROK_RETRY: Duration = Duration::from_secs(300);

impl GrokEngine {
    pub fn new(cfg: GrokTtsCfg) -> GrokEngine {
        GrokEngine {
            cfg,
            base: grok_tts::API.into(),
            cred: None,
            down: None,
            cancel: Arc::new(AtomicBool::new(false)),
            from: String::new(),
            last_first_audio_ms: None,
            refused: vec![],
        }
    }

    /// Why it is not speaking now, if it is skipped.
    pub fn down_reason(&self) -> Option<&str> {
        self.down
            .as_ref()
            .filter(|(at, _)| at.elapsed() < GROK_RETRY)
            .map(|(_, w)| w.as_str())
    }

    fn token(&mut self) -> Result<String, TtsError> {
        if self.cred.is_none() {
            let c = grok_tts::credential_except(
                &self.cfg,
                &grok_tts::sources_now(),
                grok_tts::key_store().as_ref(),
                &self.refused,
            )?;
            self.from = c.from.clone();
            self.cred = Some(c);
        }
        Ok(self
            .cred
            .as_ref()
            .map(|c| c.token.clone())
            .unwrap_or_default())
    }
}

impl TtsEngine for GrokEngine {
    fn name(&self) -> String {
        format!("grok {}", self.cfg.voice)
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
        let r = self.token().and_then(|tok| {
            grok_tts::speak(&self.base, text, &self.cfg, &tok, &self.cancel, &mut |s| {
                sink(s, grok_tts::SAMPLE_RATE)
            })
        });
        match r {
            Ok(st) => {
                self.last_first_audio_ms = Some(st.first_audio_ms);
                Ok(())
            }
            // Cut off on purpose: not a failure.
            Err(TtsError::Cancelled) => Ok(()),
            Err(e) => {
                let id = self.cred.as_ref().map(|c| c.id.clone()).unwrap_or_default();
                if e.lasting() && !id.is_empty() && matches!(e, TtsError::Auth | TtsError::Denied) {
                    // That login was refused: the next one (auto) from the
                    // next sentence; this one falls back.
                    grok_tts::note_refused(&id);
                    self.refused.push(id);
                    self.cred = None;
                } else if e.lasting() {
                    self.down = Some((Instant::now(), e.reason()));
                    self.cred = None;
                }
                Err(e.reason())
            }
        }
    }

    fn voices(&self) -> Vec<String> {
        grok_tts::KNOWN_VOICES
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        name: &'static str,
        fail: Option<&'static str>,
        calls: usize,
    }

    impl TtsEngine for Fake {
        fn name(&self) -> String {
            self.name.into()
        }
        fn speak_stream(
            &mut self,
            _t: &str,
            sink: &mut dyn FnMut(&[f32], u32) -> bool,
        ) -> Result<(), String> {
            self.calls += 1;
            match self.fail {
                Some(w) => Err(w.into()),
                None => {
                    sink(&[0.1, 0.2], 24_000);
                    Ok(())
                }
            }
        }
        fn voices(&self) -> Vec<String> {
            vec![]
        }
        fn cancel(&self) {}
    }

    #[test]
    fn chain_and_fallback() {
        let mut c = VoiceCfg {
            tts_engine: "grok".into(),
            tts_fallback: vec!["kokoro".into(), "say".into(), "grok".into(), "bogus".into()],
            kokoro_model: "/nonexistent/kokoro.onnx".into(),
            ..Default::default()
        };
        // Kokoro is not installed here: grok, then say.
        assert_eq!(chain(&c), vec!["grok", "say"]);
        c.tts_engine = "say".into();
        c.tts_fallback = vec![];
        assert_eq!(chain(&c), vec!["say"]);
        c.tts_engine = "off".into();
        assert!(chain(&c).is_empty());
        c.tts_engine = "grok".into();
        c.tts = false;
        assert!(chain(&c).is_empty());
        // A failing first engine falls back, and says why.
        let mut a = Fake {
            name: "grok eve",
            fail: Some("no audio in time"),
            calls: 0,
        };
        let mut b = Fake {
            name: "say",
            fail: None,
            calls: 0,
        };
        let mut got = 0;
        let (who, failed) = speak_with_fallback(&mut [&mut a, &mut b], "Hi.", &mut |s, _| {
            got += s.len();
            true
        });
        assert_eq!(who.as_deref(), Some("say"));
        assert_eq!(
            failed,
            vec![("grok eve".to_string(), "no audio in time".to_string())]
        );
        assert_eq!((a.calls, b.calls, got), (1, 1, 2));
        // All fail: nothing spoken.
        let mut x = Fake {
            name: "x",
            fail: Some("down"),
            calls: 0,
        };
        assert_eq!(
            speak_with_fallback(&mut [&mut x], "Hi.", &mut |_, _| true).0,
            None
        );
    }

    #[test]
    fn grok_without_a_login_falls_back_and_stays_out() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("godterm-engines-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        crate::config::testing::set_home(&d);
        let mut g = GrokEngine::new(GrokTtsCfg::default());
        // No grok account logged in: a lasting reason, never a network call.
        let e = g.speak_stream("Hi.", &mut |_, _| true).unwrap_err();
        assert!(e.contains("logged in"), "{e}");
        assert!(g.down_reason().is_some());
        let mut say = Fake {
            name: "say",
            fail: None,
            calls: 0,
        };
        let (who, failed) = speak_with_fallback(&mut [&mut g, &mut say], "Hi.", &mut |_, _| true);
        assert_eq!(who.as_deref(), Some("say"));
        assert!(failed[0].1.contains("logged in"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wav_from_say_parses() {
        let mut w = super::super::kokoro::wav(&[0.5, -0.5, 0.0]);
        let (s, rate) = wav_samples(&w).unwrap();
        assert_eq!((s.len(), rate), (3, 24_000));
        assert!((s[0] - 0.5).abs() < 0.01);
        w.truncate(10);
        assert!(wav_samples(&w).is_none());
    }
}
