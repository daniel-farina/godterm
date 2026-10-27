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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MicInfo {
    pub backend: String,

    pub vp: bool,

    pub mode: Option<String>,
    pub preferred: Option<String>,

    pub fallback: Option<String>,
}

static MIC: Mutex<Option<MicInfo>> = Mutex::new(None);

pub fn mic_info() -> Option<MicInfo> {
    MIC.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_info(i: MicInfo) {
    *MIC.lock().unwrap_or_else(|e| e.into_inner()) = Some(i);
}

pub fn helper_mic() -> Option<(String, String)> {
    match MIC_CHECK.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Check::Done(v) => return v,
        Check::Running => return None,
        Check::NotStarted => {}
    }
    start_mic_check();
    None
}

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

pub fn backend(cfg: &VoiceCfg, src: &Source, helper_ok: bool) -> Backend {
    backend_for(cfg, src, helper_ok, false)
}

pub fn backend_for(cfg: &VoiceCfg, src: &Source, helper_ok: bool, bt_default: bool) -> Backend {
    let Source::Mic(dev) = src else {
        return Backend::Ffmpeg;
    };
    if !cfg!(target_os = "macos") {
        return Backend::Ffmpeg;
    }
    let default_dev = dev.is_empty() || dev == "default";
    match cfg.capture.as_str() {
        "apple" => Backend::Apple,
        "auto" if helper_ok && default_dev && !bt_default => Backend::Apple,
        _ => Backend::Ffmpeg,
    }
}

pub fn denoise_on(cfg: &VoiceCfg, vp: bool) -> bool {
    match cfg.denoise.as_str() {
        "on" | "true" => true,
        "off" | "false" => false,
        _ => !vp,
    }
}

pub fn open(cfg: &VoiceCfg, src: &Source) -> std::io::Result<Capture> {
    if let Source::Web(mic) = src {
        return Ok(Capture::web(mic));
    }
    let bt_default = cfg.capture == "auto"
        && cfg!(target_os = "macos")
        && matches!(src, Source::Mic(d) if d.is_empty() || d == "default")
        && crate::voice::devices::chosen(&crate::voice::devices::list(), "default")
            .is_some_and(|d| d.transport == "bluetooth");
    if bt_default {
        crate::log::info("voice: the default mic is a Bluetooth headset: recording with ffmpeg");
    }
    let want = backend_for(
        cfg,
        src,
        cfg.capture == "apple" || helper_mic_wait(std::time::Duration::from_secs(4)).is_some(),
        bt_default,
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

#[derive(Debug, Clone, PartialEq)]
pub enum HelperLine {
    Started {
        vp: bool,
        mode: Option<String>,
        preferred: Option<String>,
    },
    Mode(String),
    Error(String),

    InputMute(bool),

    Remote(String),

    Diag(String),
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum HelperEvent {
    InputMute(bool),
    Remote(String),
    Diag(String),
}

static EVENTS: Mutex<std::collections::VecDeque<HelperEvent>> =
    Mutex::new(std::collections::VecDeque::new());

fn push_event(e: HelperEvent) {
    let mut q = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    q.push_back(e);
    while q.len() > 64 {
        q.pop_front();
    }
}

pub fn take_events() -> Vec<HelperEvent> {
    EVENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect()
}

pub fn helper_press(e: &HelperEvent) -> Option<crate::media_keys::Press> {
    use crate::media_keys::{Cmd, Press};
    let (cmd, source) = match e {
        HelperEvent::InputMute(true) => (Cmd::Pause, "helper:inputMute"),
        HelperEvent::Remote(c) => match c.as_str() {
            "togglePlayPause" => (Cmd::TogglePlayPause, "helper:remote:togglePlayPause"),
            "play" => (Cmd::Play, "helper:remote:play"),
            "pause" => (Cmd::Pause, "helper:remote:pause"),
            "stop" => (Cmd::Stop, "helper:remote:stop"),
            "next" => (Cmd::Next, "helper:remote:next"),
            "prev" => (Cmd::Previous, "helper:remote:prev"),
            _ => return None,
        },
        _ => return None,
    };
    Some(Press { cmd, source })
}

pub fn read_status(
    from: impl std::io::Read,
    tx: mpsc::Sender<Result<bool, String>>,
    err: Arc<Mutex<String>>,
) {
    for l in BufReader::new(from).lines().map_while(Result::ok) {
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
            HelperLine::InputMute(m) => {
                crate::media_keys::log(&format!(
                    "helper: AVAudioApplication input mute {} (the headset mute gesture)",
                    if m { "on" } else { "off" }
                ));
                push_event(HelperEvent::InputMute(m));
            }
            HelperLine::Remote(c) => {
                crate::media_keys::log(&format!("helper: remote:{c} (in the recording process)"));
                push_event(HelperEvent::Remote(c));
            }
            HelperLine::Diag(d) => {
                crate::media_keys::log(&format!("helper: {d}"));
                crate::log::info(&format!("voice: helper: {d}"));
                push_event(HelperEvent::Diag(d));
            }
            HelperLine::Other(o) if !o.is_empty() => {
                *err.lock().unwrap_or_else(|e| e.into_inner()) = o;
            }
            HelperLine::Other(_) => {}
        }
    }
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
        Some("input_mute") => HelperLine::InputMute(v["muted"].as_bool().unwrap_or(false)),
        Some("remote") => HelperLine::Remote(s("cmd").unwrap_or_default()),
        Some("route" | "restarted" | "retry" | "path" | "note") => {
            HelperLine::Diag(l.trim().to_string())
        }
        _ => HelperLine::Other(l.trim().to_string()),
    }
}

fn start_apple(vp: bool) -> std::io::Result<Capture> {
    if cfg!(test) || crate::config::env_var("NO_MIC").is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the microphone is disabled (GODTERM_NO_MIC)",
        ));
    }
    start_helper(&super::apple_helper_path(), vp, false)
}

pub fn open_helper_mic(vp: bool, remote: bool) -> std::io::Result<Capture> {
    if crate::config::env_var("NO_MIC").is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the microphone is disabled (GODTERM_NO_MIC)",
        ));
    }
    start_helper(&super::apple_helper_path(), vp, remote)
}

fn start_helper(bin: &std::path::Path, vp: bool, remote: bool) -> std::io::Result<Capture> {
    use std::io::Error;
    let mut cmd = Command::new(bin);
    cmd.arg("--mic");
    if !vp {
        cmd.arg("--no-vp");
    }
    if remote {
        cmd.arg("--remote");
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
        std::thread::spawn(move || read_status(e, tx, err));
    }
    let fail = |child: &mut std::process::Child, why: String| {
        let _ = child.kill();
        let _ = child.wait();
        super::untrack(child.id());
        Err(Error::other(why))
    };

    let on = match rx.recv_timeout(Duration::from_secs(4)) {
        Ok(Ok(vp)) => vp,
        Ok(Err(e)) => return fail(&mut child, e),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return fail(&mut child, "godterm-speech --mic exited".into())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => vp,
    };
    Ok(Capture {
        child: Some(child),
        web: None,
        err: Some(err),
        vp: on,
    })
}

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

        assert_eq!(
            backend_for(&c, &mic, true, true),
            Backend::Ffmpeg,
            "a Bluetooth headset records with ffmpeg"
        );
        c.capture = "ffmpeg".into();
        assert_eq!(backend(&c, &mic, true), Backend::Ffmpeg);
        c.capture = "apple".into();
        assert_eq!(backend(&c, &mic, false), apple);
        assert_eq!(backend_for(&c, &mic, true, true), apple);
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
    fn helper_events_map_to_presses() {
        use crate::media_keys::Cmd;
        assert_eq!(
            parse_line(r#"{"event":"input_mute","muted":true,"source":"AVAudioApplication"}"#),
            HelperLine::InputMute(true)
        );
        assert_eq!(
            parse_line(r#"{"cmd":"togglePlayPause","event":"remote","source":"helper"}"#),
            HelperLine::Remote("togglePlayPause".into())
        );
        let route = r#"{"event":"route","restarts":1,"what":"engine configuration changed"}"#;
        assert_eq!(parse_line(route), HelperLine::Diag(route.into()));

        let p = helper_press(&HelperEvent::InputMute(true)).unwrap();
        assert_eq!((p.cmd, p.source), (Cmd::Pause, "helper:inputMute"));
        assert!(helper_press(&HelperEvent::InputMute(false)).is_none());
        let p = helper_press(&HelperEvent::Remote("play".into())).unwrap();
        assert_eq!(p.cmd, Cmd::Play);
        assert!(helper_press(&HelperEvent::Diag("x".into())).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_fake_helper_starts_without_vp_and_reports_the_mute_gesture() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("gt-fakehelper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("godterm-speech");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
echo '{"error":"com.apple.coreaudio.avfaudio error -10875","event":"retry","mixer":true,"vp":true}' >&2
echo '{"event":"started","in_rate":16000,"mode":"standard","vp":false,"vp_error":"-10875"}' >&2
echo '{"event":"input_mute","muted":true,"source":"AVAudioApplication"}' >&2
head -c 640 /dev/zero
sleep 2
"#,
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cap = start_helper(&bin, true, false).expect("started without voice processing");

        super::super::untrack(cap.child.as_ref().map_or(0, |c| c.id()));
        assert!(!cap.vp, "the helper said it runs without voice processing");
        let mut frame = vec![];
        assert!(cap.frame(&mut frame).is_some(), "audio comes");
        let mut got = vec![];
        for _ in 0..50 {
            got.extend(take_events());
            if got.contains(&HelperEvent::InputMute(true)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(got.contains(&HelperEvent::InputMute(true)), "{got:?}");
        assert!(got
            .iter()
            .any(|e| matches!(e, HelperEvent::Diag(d) if d.contains("-10875"))));
        let press = got.iter().find_map(helper_press).unwrap();
        assert_eq!(press.source, "helper:inputMute");
        drop(cap);
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn helper_refuses_without_mic_and_frames_files() {
        let bin = super::super::apple_helper_path();
        let ok = Command::new(&bin)
            .arg("--mic-check")
            .output()
            .is_ok_and(|o| parse_check(&String::from_utf8_lossy(&o.stdout)).is_some());
        if !ok {
            return;
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

        let n = out.stdout.len();
        assert!((31_000..=33_000).contains(&n) && n.is_multiple_of(2), "{n}");
    }
}
