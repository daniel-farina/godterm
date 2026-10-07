//! Speech to text with whisper.cpp. A long lived `whisper-server` keeps the
//! model in memory; `whisper-cli` is the fallback.

use anyhow::{anyhow, bail, Context, Result};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::audio::wav_bytes;

/// Threads for whisper: about half of the idle cores, 2 to 8.
pub fn whisper_threads() -> usize {
    let ncpu = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let idle_pct = Command::new("top")
        .args(["-l", "1", "-n", "0"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("CPU usage"))
                .and_then(|l| {
                    l.split(',')
                        .find(|p| p.contains("idle"))
                        .map(str::to_string)
                })
        })
        .and_then(|p| {
            p.trim()
                .split('%')
                .next()
                .and_then(|n| n.trim().parse::<f64>().ok())
        })
        .unwrap_or(50.0);
    let free = (ncpu as f64 * idle_pct / 100.0).floor() as usize;
    (free / 2).clamp(2, 8)
}

pub struct Whisper {
    model: PathBuf,
    cli: String,
    server_bin: String,
    language: String,
    threads: usize,
    server: Option<(Child, u16)>,
    server_failed: bool,
    /// Vocabulary hint (account labels and command words).
    pub prompt: String,
    /// Beam size for finals (1: greedy).
    pub beam: u32,
    /// whisper.cpp's own voice activity detection (silero model path).
    pub vad_model: Option<PathBuf>,
    /// What the server said about the GPU, once it is up.
    pub gpu: Option<String>,
}

/// A transcript and how sure whisper was (mean word probability).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decoded {
    pub text: String,
    pub conf: Option<f32>,
    /// whisper's own estimate that the audio holds no speech.
    pub no_speech: Option<f32>,
    /// Mean of the segments' average log probability.
    pub logprob: Option<f32>,
}

/// How a request decodes.
#[derive(Debug, Clone, Copy)]
pub struct Opts {
    /// Partials: greedy, no temperature fallback (never holds the server).
    pub fast: bool,
    /// Beam size for a final (1 or less: greedy).
    pub beam: u32,
}

/// Mean word probability and no-speech probability from whisper-server's
/// verbose_json (punctuation-only words left out).
pub fn confidence(v: &serde_json::Value) -> (Option<f32>, Option<f32>) {
    let mut ps = vec![];
    let mut ns: Option<f32> = None;
    for seg in v["segments"].as_array().into_iter().flatten() {
        let _ = &seg["avg_logprob"];
        if let Some(n) = seg["no_speech_prob"].as_f64() {
            ns = Some(ns.map_or(n as f32, |m: f32| m.max(n as f32)));
        }
        for w in seg["words"].as_array().into_iter().flatten() {
            let word = w["word"].as_str().unwrap_or("");
            if word.chars().any(|c| c.is_alphanumeric()) {
                if let Some(p) = w["probability"].as_f64() {
                    ps.push(p as f32);
                }
            }
        }
    }
    let conf = (!ps.is_empty()).then(|| ps.iter().sum::<f32>() / ps.len() as f32);
    (conf, ns)
}

/// Where whisper-server writes its log (model load, Metal, errors).
pub fn server_log() -> PathBuf {
    crate::config::app_home()
        .join("voice")
        .join("whisper-server.log")
}

/// The GPU line of a whisper-server log ("Metal: Apple M5 Max"), if any.
pub fn gpu_of_log(log: &str) -> Option<String> {
    if let Some(l) = log.lines().find(|l| l.contains("GPU name:")) {
        return Some(format!(
            "Metal: {}",
            l.split("GPU name:").nth(1).unwrap_or("").trim()
        ));
    }
    log.lines()
        .any(|l| l.contains("ggml_metal") || l.contains("using Metal"))
        .then(|| "Metal".to_string())
}

/// The silero VAD model for whisper.cpp, in the models folder.
pub fn vad_model_path() -> PathBuf {
    crate::config::home_dir().join(".cache/whisper-models/ggml-silero-v5.1.2.bin")
}

pub const VAD_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

/// Fetch the VAD model (about 1 MB) when it is missing. Returns its path.
pub fn ensure_vad_model() -> Result<PathBuf> {
    let p = vad_model_path();
    if p.is_file() {
        return Ok(p);
    }
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut resp = ureq::get(VAD_URL)
        .call()
        .context("downloading the VAD model")?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(20 << 20)
        .read_to_vec()?;
    if bytes.len() < 100_000 {
        bail!("VAD model download was too small ({} bytes)", bytes.len());
    }
    let tmp = p.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &p)?;
    crate::log::info(&format!(
        "voice: downloaded the VAD model to {}",
        p.display()
    ));
    Ok(p)
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()?
        .local_addr()
        .ok()
        .map(|a| a.port())
}

impl Whisper {
    pub fn new(model: PathBuf, cli: String, server_bin: String, language: String) -> Whisper {
        Whisper {
            model,
            cli,
            server_bin,
            language,
            threads: whisper_threads(),
            server: None,
            server_failed: false,
            prompt: String::new(),
            beam: 5,
            vad_model: None,
            gpu: None,
        }
    }

    /// Check that the model and a whisper binary exist.
    pub fn check(&self) -> Result<()> {
        if !self.model.is_file() {
            bail!("whisper model not found at {}", self.model.display());
        }
        let has = |b: &str| Path::new(b).is_file() || which(b);
        if !has(&self.server_bin) && !has(&self.cli) {
            bail!("neither {} nor {} found", self.server_bin, self.cli);
        }
        Ok(())
    }

    pub fn set_threads(&mut self, n: usize) {
        self.threads = n.clamp(1, 32);
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Start whisper-server and wait for it to load the model.
    pub fn start_server(&mut self) -> Result<()> {
        if self.server.is_some() || self.server_failed {
            return Ok(());
        }
        let port = free_port().ok_or_else(|| anyhow!("no free port"))?;
        let log_path = server_log();
        if let Some(d) = log_path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let log = std::fs::File::create(&log_path).ok();
        let mut cmd = Command::new(&self.server_bin);
        cmd.args(["-m"])
            .arg(&self.model)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-t", &self.threads.to_string(), "-l", &self.language]);
        if let Some(v) = self.vad_model.as_ref().filter(|v| v.is_file()) {
            cmd.arg("--vad").arg("-vm").arg(v);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        match log {
            Some(f) => cmd.stderr(f),
            None => cmd.stderr(Stdio::null()),
        };
        let child = cmd.spawn();
        let mut child = match child {
            Ok(c) => {
                super::track(c.id());
                c
            }
            Err(e) => {
                self.server_failed = true;
                return Err(e).context("starting whisper-server");
            }
        };
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(60) {
            if let Ok(Some(_)) = child.try_wait() {
                super::untrack(child.id());
                self.server_failed = true;
                bail!("whisper-server exited during startup");
            }
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                super::track(child.id());
                self.server = Some((child, port));
                self.gpu = std::fs::read_to_string(&log_path)
                    .ok()
                    .and_then(|l| gpu_of_log(&l));
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = child.kill();
        let _ = child.wait();
        super::untrack(child.id());
        self.server_failed = true;
        bail!("whisper-server did not come up in 60 s")
    }

    /// Transcribe 16 kHz mono samples.
    pub fn transcribe(&mut self, samples: &[i16]) -> Result<String> {
        self.transcribe_full(samples).map(|d| d.text)
    }

    /// Transcribe with beam search and temperature fallback, with the
    /// confidence when the server reports it.
    pub fn transcribe_full(&mut self, samples: &[i16]) -> Result<Decoded> {
        let wav = wav_bytes(samples);
        if self.server.is_none() && !self.server_failed {
            let _ = self.start_server();
        }
        if let Some((_, port)) = &self.server {
            match post_decode(
                *port,
                &wav,
                &self.language,
                &self.prompt,
                Opts {
                    fast: false,
                    beam: self.beam,
                },
            ) {
                Ok(d) => return Ok(d),
                Err(_) => {
                    // Server died: drop it and fall back for this utterance.
                    if let Some((mut c, _)) = self.server.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                        super::untrack(c.id());
                    }
                }
            }
        }
        self.via_cli(&wav).map(|t| Decoded {
            text: clean(&t),
            ..Default::default()
        })
    }

    /// Port of the running whisper-server, for partial transcripts made
    /// from another thread.
    pub fn port(&self) -> Option<u16> {
        self.server.as_ref().map(|(_, p)| *p)
    }
}

/// Transcribe a WAV through a whisper-server on `port` (a final).
pub fn post_inference(port: u16, wav: &[u8], language: &str, prompt: &str) -> Result<String> {
    post_decode(
        port,
        wav,
        language,
        prompt,
        Opts {
            fast: false,
            beam: 5,
        },
    )
    .map(|d| d.text)
}

/// `fast`: greedy, no temperature fallback (a partial must never hold the
/// server for long; re-decoding at higher temperatures is what made long
/// finals take seconds).
pub fn post_inference_opts(
    port: u16,
    wav: &[u8],
    language: &str,
    prompt: &str,
    fast: bool,
) -> Result<String> {
    post_decode(
        port,
        wav,
        language,
        prompt,
        Opts {
            fast,
            beam: if fast { 1 } else { 5 },
        },
    )
    .map(|d| d.text)
}

/// One request to whisper-server's /inference.
pub fn post_decode(
    port: u16,
    wav: &[u8],
    language: &str,
    prompt: &str,
    o: Opts,
) -> Result<Decoded> {
    {
        let boundary = "godtermBoundary7d1a";
        let mut body = Vec::with_capacity(wav.len() + 512);
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        };
        field(
            "response_format",
            if o.fast { "json" } else { "verbose_json" },
        );
        field("temperature", "0.0");
        if o.fast {
            field("temperature_inc", "0.0");
        } else if o.beam > 1 {
            // Beam search on finals; temperature fallback stays on.
            field("beam_size", &o.beam.to_string());
            field("best_of", &o.beam.to_string());
        }
        field("language", language);
        if !prompt.is_empty() {
            field("prompt", prompt);
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"u.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(wav);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        let mut resp = agent
            .post(&format!("http://127.0.0.1:{port}/inference"))
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body[..])?;
        let text = resp.body_mut().read_to_string()?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        let t = v
            .get("text")
            .and_then(|t| t.as_str())
            .map(clean)
            .ok_or_else(|| anyhow!("no text in whisper reply"))?;
        let (conf, no_speech) = confidence(&v);
        let lps: Vec<f64> = v["segments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["avg_logprob"].as_f64())
            .collect();
        let logprob =
            (!lps.is_empty()).then(|| (lps.iter().sum::<f64>() / lps.len() as f64) as f32);
        Ok(Decoded {
            text: t,
            conf,
            no_speech,
            logprob,
        })
    }
}

impl Whisper {
    fn via_cli(&self, wav: &[u8]) -> Result<String> {
        let path = std::env::temp_dir().join(format!("godterm-utt-{}.wav", std::process::id()));
        std::fs::write(&path, wav)?;
        let mut cmd = Command::new(&self.cli);
        cmd.arg("-m").arg(&self.model).arg("-f").arg(&path).args([
            "-nt",
            "-np",
            "-l",
            &self.language,
            "-t",
            &self.threads.to_string(),
        ]);
        if !self.prompt.is_empty() {
            cmd.args(["--prompt", &self.prompt]);
        }
        let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output();
        let _ = std::fs::remove_file(&path);
        let out = out.context("running whisper-cli")?;
        if !out.status.success() {
            bail!("whisper-cli failed ({})", out.status);
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn shutdown(&mut self) {
        if let Some((mut c, _)) = self.server.take() {
            let _ = c.kill();
            let _ = c.wait();
            super::untrack(c.id());
        }
    }
}

impl Drop for Whisper {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// Strip whisper's non speech markers like "[BLANK_AUDIO]" or "(music)".
pub fn clean(t: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_markers() {
        assert_eq!(clean(" [BLANK_AUDIO]\n"), "");
        assert_eq!(
            clean(" Hey go, approve. (keyboard clicking)"),
            "Hey go, approve."
        );
    }

    #[test]
    fn confidence_from_verbose_json() {
        let v: serde_json::Value = serde_json::from_str(r#"{"text":" close all tabs.","segments":[{"no_speech_prob":0.01,"words":[{"word":" close","probability":0.2},{"word":" all","probability":0.8},{"word":" tabs","probability":0.9},{"word":".","probability":0.1}]}]}"#).unwrap();
        let (c, n) = confidence(&v);
        assert!((c.unwrap() - 0.6333).abs() < 0.01, "{c:?}");
        assert_eq!(n, Some(0.01));
        assert_eq!(confidence(&serde_json::json!({"text": "x"})), (None, None));
        assert_eq!(
            gpu_of_log("ggml_metal_device_init: GPU name:   MTL0 (Apple M5 Max)\n"),
            Some("Metal: MTL0 (Apple M5 Max)".into())
        );
        assert_eq!(gpu_of_log("cpu only"), None);
    }

    #[test]
    fn thread_count_is_bounded() {
        let t = whisper_threads();
        assert!((2..=8).contains(&t));
    }
}
