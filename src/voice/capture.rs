//! Which mic capture to use, and the Apple one.
//!
//! On macOS the `godterm-speech` helper can capture through AVAudioEngine
//! with voice processing on: Apple's echo cancellation, noise suppression
//! and automatic gain, the same processing FaceTime uses, and it honors
//! the Voice Isolation mic mode picked in Control Center. It streams the
//! same 16 kHz mono s16le as ffmpeg, so the rest of the pipeline does not
//! change. ffmpeg stays the fallback (and the only capture on Linux).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use super::audio::{Capture, Source};
use crate::config::VoiceCfg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Ffmpeg,
    Apple,
}

/// What the running capture reported (for the strip, Settings and doctor).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MicInfo {
    /// "apple" or "ffmpeg".
    pub backend: String,
    /// Voice processing is on.
    pub vp: bool,
    /// The system mic mode in effect ("standard", "voice_isolation",
    /// "wide_spectrum"), when known.
    pub mode: Option<String>,
    pub preferred: Option<String>,
    /// Why Apple capture was not used, when it was wanted.
    pub fallback: Option<String>,
}

static MIC: Mutex<Option<MicInfo>> = Mutex::new(None);

pub fn mic_info() -> Option<MicInfo> {
    MIC.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_info(i: MicInfo) {
    *MIC.lock().unwrap_or_else(|e| e.into_inner()) = Some(i);
}

/// The helper's `--mic-check` answer, once per run: (mode, preferred).
/// Never blocks: the check runs in the background (with a timeout) and
/// this is None until it is back. The screen asks this every frame.
pub fn helper_mic() -> Option<(String, String)> {
    match MIC_CHECK.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Check::Done(v) => return v,
        Check::Running => return None,
        Check::NotStarted => {}
    }
    start_mic_check();
    None
}

/// The same, waiting up to `limit` for it (the voice thread, before it
/// opens the mic; never the UI thread).
pub fn helper_mic_wait(limit: std::time::Duration) -> Option<(String, String)> {
    let t0 = std::time::Instant::now();
    loop {
        if let Check::Done(v) = MIC_CHECK.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return v;
        }
        let _ = helper_mic();
        if t0.elapsed() >= limit {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Check {
    NotStarted,
    Running,
    Done(Option<(String, String)>),
}

static MIC_CHECK: Mutex<Check> = Mutex::new(Check::NotStarted);

fn start_mic_check() {
    {
        let mut c = MIC_CHECK.lock().unwrap_or_else(|e| e.into_inner());
        if *c != Check::NotStarted {
            return;
        }
        if !cfg!(target_os = "macos") || cfg!(test) || !super::apple_helper_path().is_file() {
            *c = Check::Done(None);
            return;
        }
        *c = Check::Running;
    }
    std::thread::spawn(|| {
        let r = crate::procs::output_timeout(
            Command::new(super::apple_helper_path()).arg("--mic-check"),
            std::time::Duration::from_secs(3),
        );
        let v = match r {
            Ok(o) => parse_check(&String::from_utf8_lossy(&o.stdout)),
            Err(e) => {
                crate::log::info(&format!("voice: godterm-speech --mic-check {e}"));
                None
            }
        };
        *MIC_CHECK.lock().unwrap_or_else(|e| e.into_inner()) = Check::Done(v);
    });
}

pub fn parse_check(text: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(text.lines().next()?.trim()).ok()?;
    if v["mic"] != serde_json::Value::Bool(true) {
        return None;
    }
    Some((
        v["mode"].as_str().unwrap_or("unknown").to_string(),
        v["preferred"].as_str().unwrap_or("unknown").to_string(),
    ))
}

/// The capture `voice.capture` asks for: files always go through ffmpeg;
/// "auto" picks Apple on macOS when the helper supports it and the
/// default input is used (a chosen device stays with ffmpeg, which can
/// address it by name).
pub fn backend(cfg: &VoiceCfg, src: &Source, helper_ok: bool) -> Backend {
    let Source::Mic(dev) = src else {
        return Backend::Ffmpeg;
    };
    if !cfg!(target_os = "macos") {
        return Backend::Ffmpeg;
    }
    let default_dev = dev.is_empty() || dev == "default";
    match cfg.capture.as_str() {
        "apple" => Backend::Apple,
        "auto" if helper_ok && default_dev => Backend::Apple,
        _ => Backend::Ffmpeg,
    }
}

/// Denoise (RNNoise) for this capture: "auto" means on unless Apple voice
/// processing already cleans the signal.
pub fn denoise_on(cfg: &VoiceCfg, vp: bool) -> bool {
    match cfg.denoise.as_str() {
        "on" | "true" => true,
        "off" | "false" => false,
        _ => !vp,
    }
}

/// Open the mic (or file) with the configured capture, falling back to
/// ffmpeg when Apple capture fails and the setting is "auto".
pub fn open(cfg: &VoiceCfg, src: &Source) -> std::io::Result<Capture> {
    let want = backend(
        cfg,
        src,
        cfg.capture == "apple" || helper_mic_wait(std::time::Duration::from_secs(4)).is_some(),
    );
    let mut fallback = None;
    if want == Backend::Apple {
        match start_apple(cfg.voice_processing) {
            Ok(c) => return Ok(c),
            Err(e) if cfg.capture == "apple" => return Err(e),
            Err(e) => {
                crate::log::info(&format!("voice: Apple capture failed ({e}), using ffmpeg"));
                fallback = Some(e.to_string());
            }
        }
    }
    let c = Capture::start(&cfg.ffmpeg, src)?;
    if matches!(src, Source::Mic(_)) {
        set_info(MicInfo {
            backend: "ffmpeg".into(),
            fallback,
            ..Default::default()
        });
    }
    Ok(c)
}

/// One status line from the helper's stderr.
#[derive(Debug, Clone, PartialEq)]
pub enum HelperLine {
    Started {
        vp: bool,
        mode: Option<String>,
        preferred: Option<String>,
    },
    Mode(String),
    Error(String),
    Other(String),
}

pub fn parse_line(l: &str) -> HelperLine {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(l.trim()) else {
        return HelperLine::Other(l.trim().to_string());
    };
    let s = |k: &str| v[k].as_str().map(str::to_string);
    match v["event"].as_str() {
        Some("started") => HelperLine::Started {
            vp: v["vp"].as_bool().unwrap_or(false),
            mode: s("mode"),
            preferred: s("preferred"),
        },
        Some("mode") => HelperLine::Mode(s("mode").unwrap_or_default()),
        Some("error") => HelperLine::Error(s("error").unwrap_or_default()),
        _ => HelperLine::Other(l.trim().to_string()),
    }
}

/// Start `godterm-speech --mic`; waits for its first status line.
fn start_apple(vp: bool) -> std::io::Result<Capture> {
    use std::io::{Error, ErrorKind};
    if cfg!(test) || crate::config::env_var("NO_MIC").is_some() {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "the microphone is disabled (GODTERM_NO_MIC)",
        ));
    }
    let bin = super::apple_helper_path();
    let mut cmd = Command::new(&bin);
    cmd.arg("--mic");
    if !vp {
        cmd.arg("--no-vp");
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    super::track(child.id());
    let stderr = child.stderr.take();
    let err = Arc::new(Mutex::new(String::new()));
    let (tx, rx) = mpsc::channel::<Result<bool, String>>();
    if let Some(e) = stderr {
        let err = Arc::clone(&err);
        std::thread::spawn(move || {
            for l in BufReader::new(e).lines().map_while(Result::ok) {
                match parse_line(&l) {
                    HelperLine::Started {
                        vp,
                        mode,
                        preferred,
                    } => {
                        crate::log::info(&format!(
                            "voice: Apple capture, voice processing {}, mic mode {}",
                            if vp { "on" } else { "off" },
                            mode.as_deref().unwrap_or("unknown")
                        ));
                        set_info(MicInfo {
                            backend: "apple".into(),
                            vp,
                            mode,
                            preferred,
                            fallback: None,
                        });
                        let _ = tx.send(Ok(vp));
                    }
                    HelperLine::Mode(m) => {
                        crate::log::info(&format!("voice: mic mode now {m}"));
                        let mut g = MIC.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(i) = g.as_mut() {
                            i.mode = Some(m);
                        }
                    }
                    HelperLine::Error(e) => {
                        *err.lock().unwrap_or_else(|e| e.into_inner()) = e.clone();
                        let _ = tx.send(Err(e));
                    }
                    HelperLine::Other(o) if !o.is_empty() => {
                        *err.lock().unwrap_or_else(|e| e.into_inner()) = o;
                    }
                    HelperLine::Other(_) => {}
                }
            }
        });
    }
    let fail = |child: &mut std::process::Child, why: String| {
        let _ = child.kill();
        let _ = child.wait();
        super::untrack(child.id());
        Err(Error::other(why))
    };
    // A first mic permission prompt can hold the start up: then assume it
    // is coming and let the stream decide.
    let on = match rx.recv_timeout(Duration::from_secs(4)) {
        Ok(Ok(vp)) => vp,
        Ok(Err(e)) => return fail(&mut child, e),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return fail(&mut child, "godterm-speech --mic exited".into())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => vp,
    };
    Ok(Capture {
        child,
        err: Some(err),
        vp: on,
    })
}

/// Open the system mic mode picker (Voice Isolation, Wide Spectrum).
pub fn show_mic_modes() -> anyhow::Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("mic modes are a macOS feature");
    }
    if cfg!(test) {
        return Ok(());
    }
    if helper_mic().is_none() {
        anyhow::bail!(
            "the godterm-speech helper is missing or too old (godterm install builds it)"
        );
    }
    Command::new(super::apple_helper_path())
        .arg("--mic-modes")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// One line for doctor and Settings: capture, processing, mic mode.
pub fn describe(cfg: &VoiceCfg) -> String {
    if let Some(i) = mic_info() {
        let mode = i.mode.as_deref().unwrap_or("unknown");
        return match i.backend.as_str() {
            "apple" => format!(
                "Apple, voice processing {}, mic mode {mode}",
                if i.vp { "on" } else { "off" }
            ),
            _ => match &i.fallback {
                Some(f) => format!("ffmpeg (Apple capture failed: {f})"),
                None => "ffmpeg".into(),
            },
        };
    }
    let helper = helper_mic();
    match backend(cfg, &Source::Mic(cfg.device.clone()), helper.is_some()) {
        Backend::Apple => format!(
            "Apple (godterm-speech), voice processing {}, mic mode {}",
            if cfg.voice_processing { "on" } else { "off" },
            helper.map(|h| h.0).unwrap_or_else(|| "unknown".into())
        ),
        Backend::Ffmpeg => "ffmpeg".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_backend() {
        let mut c = VoiceCfg::default();
        let mic = Source::Mic("default".into());
        let file = Source::File {
            path: "x.wav".into(),
            realtime: false,
        };
        assert_eq!(backend(&c, &file, true), Backend::Ffmpeg);
        let mac = cfg!(target_os = "macos");
        let apple = if mac { Backend::Apple } else { Backend::Ffmpeg };
        assert_eq!(backend(&c, &mic, true), apple);
        assert_eq!(backend(&c, &mic, false), Backend::Ffmpeg, "no helper");
        assert_eq!(
            backend(&c, &Source::Mic("USB Mic".into()), true),
            Backend::Ffmpeg,
            "a chosen device stays with ffmpeg"
        );
        c.capture = "ffmpeg".into();
        assert_eq!(backend(&c, &mic, true), Backend::Ffmpeg);
        c.capture = "apple".into();
        assert_eq!(backend(&c, &mic, false), apple);
    }

    #[test]
    fn denoise_defaults() {
        let mut c = VoiceCfg::default();
        assert!(denoise_on(&c, false), "auto: on without voice processing");
        assert!(!denoise_on(&c, true), "auto: off with voice processing");
        c.denoise = "on".into();
        assert!(denoise_on(&c, true));
        c.denoise = "off".into();
        assert!(!denoise_on(&c, false));
    }

    #[test]
    fn parses_helper_lines() {
        assert_eq!(
            parse_line(
                r#"{"event":"started","vp":true,"mode":"voice_isolation","preferred":"standard","in_rate":48000}"#
            ),
            HelperLine::Started {
                vp: true,
                mode: Some("voice_isolation".into()),
                preferred: Some("standard".into())
            }
        );
        assert_eq!(
            parse_line(r#"{"event":"mode","mode":"standard"}"#),
            HelperLine::Mode("standard".into())
        );
        assert_eq!(
            parse_line(
                r#"{"error":"the microphone is disabled (GODTERM_NO_MIC)","event":"error"}"#
            ),
            HelperLine::Error("the microphone is disabled (GODTERM_NO_MIC)".into())
        );
        assert_eq!(parse_line("boom"), HelperLine::Other("boom".into()));
        assert_eq!(
            parse_check(
                r#"{"disabled":false,"mic":true,"mode":"standard","preferred":"voice_isolation"}"#
            ),
            Some(("standard".into(), "voice_isolation".into()))
        );
        assert_eq!(parse_check("usage: godterm-speech ..."), None);
    }

    #[test]
    fn tests_never_open_the_mic() {
        let c = VoiceCfg {
            capture: "apple".into(),
            ..Default::default()
        };
        let e = open(&c, &Source::Mic(String::new()))
            .err()
            .expect("refused");
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// The real helper (when built): it refuses the mic under
    /// GODTERM_NO_MIC, and its file mode frames audio like ffmpeg.
    #[test]
    fn helper_refuses_without_mic_and_frames_files() {
        let bin = super::super::apple_helper_path();
        let ok = Command::new(&bin)
            .arg("--mic-check")
            .output()
            .is_ok_and(|o| parse_check(&String::from_utf8_lossy(&o.stdout)).is_some());
        if !ok {
            return; // no helper with mic support here
        }
        let out = Command::new(&bin)
            .arg("--mic")
            .env("GODTERM_NO_MIC", "1")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(77));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("GODTERM_NO_MIC"));
        let dir = std::env::temp_dir().join(format!("gt-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("t.wav");
        // One second of a 440 Hz tone at 48 kHz.
        let pcm: Vec<i16> = (0..48_000)
            .map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 8000.0) as i16)
            .collect();
        let mut w = super::super::audio::wav_bytes(&pcm);
        w[24..28].copy_from_slice(&48_000u32.to_le_bytes());
        w[28..32].copy_from_slice(&96_000u32.to_le_bytes());
        std::fs::write(&wav, w).unwrap();
        let out = Command::new(&bin)
            .arg("--mic-file")
            .arg(&wav)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // 16 kHz s16le: about 32000 bytes for the second, even length.
        let n = out.stdout.len();
        assert!((31_000..=33_000).contains(&n) && n.is_multiple_of(2), "{n}");
    }
}
