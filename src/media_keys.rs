use std::sync::mpsc::Sender;
use std::sync::Mutex;

use crate::app::AppEvent;

pub const DEBOUNCE_MS: u64 = 550;

pub const RECLAIM_QUIET_MS: u64 = 1_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Cmd {
    TogglePlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Toggle,

    StopOnly,
}

impl Cmd {
    pub fn action(self) -> Action {
        match self {
            Cmd::Pause | Cmd::Stop => Action::StopOnly,
            Cmd::TogglePlayPause | Cmd::Play | Cmd::Next | Cmd::Previous => Action::Toggle,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    pub cmd: Cmd,
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    On,
    Off,
    Ignored(String),
}

#[derive(Debug, Default)]
pub struct Gate {
    last_ms: Option<u64>,
}

impl Gate {
    pub fn decide(&mut self, p: Press, now_ms: u64, listening: bool, reclaimed_ms: u64) -> Verdict {
        if reclaimed_ms > 0 && now_ms.saturating_sub(reclaimed_ms) < RECLAIM_QUIET_MS {
            return Verdict::Ignored(format!(
                "{} {} ms after a reclaim of Now Playing",
                p.source,
                now_ms.saturating_sub(reclaimed_ms)
            ));
        }
        if p.cmd.action() == Action::StopOnly && !listening {
            return Verdict::Ignored(format!(
                "{} while the mic is off (pause or stop never turns it on: unplug or route change?)",
                p.source
            ));
        }
        if let Some(last) = self.last_ms {
            let gap = now_ms.saturating_sub(last);
            if gap < DEBOUNCE_MS {
                return Verdict::Ignored(format!("{} {gap} ms after the last press", p.source));
            }
        }
        self.last_ms = Some(now_ms);
        if listening {
            Verdict::Off
        } else {
            Verdict::On
        }
    }
}

pub trait Backend {
    fn pump(&mut self, listening: bool, speaking: bool);

    fn release(&mut self);

    fn reclaimed_ms(&self) -> u64 {
        0
    }
}

type Factory = Box<dyn Fn(Sender<AppEvent>) -> Option<Box<dyn Backend>>>;

pub struct MediaKeys {
    backend: Option<Box<dyn Backend>>,
    pub gate: Gate,
    factory: Factory,

    on: bool,
}

impl Default for MediaKeys {
    fn default() -> Self {
        MediaKeys {
            backend: None,
            gate: Gate::default(),
            factory: Box::new(platform_backend),
            on: false,
        }
    }
}

impl MediaKeys {
    #[cfg(test)]
    pub fn with_factory(
        f: impl Fn(Sender<AppEvent>) -> Option<Box<dyn Backend>> + 'static,
    ) -> Self {
        MediaKeys {
            backend: None,
            gate: Gate::default(),
            factory: Box::new(f),
            on: false,
        }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    pub fn claimed(&self) -> bool {
        self.backend.is_some()
    }

    pub fn set(&mut self, on: bool, tx: &Sender<AppEvent>) -> bool {
        if on == self.on {
            return false;
        }
        self.on = on;
        if on {
            self.backend = (self.factory)(tx.clone());
            log(&format!(
                "headphone tap on: {}",
                if self.backend.is_some() {
                    "Now Playing claimed"
                } else {
                    "no media key support on this platform"
                }
            ));
        } else if let Some(mut b) = self.backend.take() {
            b.release();
            log("headphone tap off: Now Playing and the remote commands given back");
        }
        true
    }

    pub fn pump(&mut self, listening: bool, speaking: bool) {
        if let Some(b) = self.backend.as_mut() {
            b.pump(listening, speaking);
        }
    }

    pub fn decide(&mut self, p: Press, listening: bool) -> Verdict {
        let reclaimed = self.backend.as_ref().map(|b| b.reclaimed_ms()).unwrap_or(0);
        let v = self.gate.decide(p, now_ms(), listening, reclaimed);
        log(&match &v {
            Verdict::Ignored(why) => format!("ignored {why}"),
            v => format!(
                "{} ({:?}) mic {} -> {}",
                p.source,
                p.cmd,
                if listening { "on" } else { "off" },
                if *v == Verdict::On { "on" } else { "off" }
            ),
        });
        v
    }
}

impl Drop for MediaKeys {
    fn drop(&mut self) {
        if let Some(mut b) = self.backend.take() {
            b.release();
        }
    }
}

fn platform_backend(tx: Sender<AppEvent>) -> Option<Box<dyn Backend>> {
    if cfg!(test) {
        let _ = tx;
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        crate::media_keys_mac::start(tx).map(|b| Box::new(b) as Box<dyn Backend>)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = tx;
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct Route {
    pub out_dev: u32,
    pub out_rate: u32,
    pub out_bt: bool,
    pub in_dev: u32,
    pub in_rate: u32,
    pub in_bt: bool,
    pub in_running: bool,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Route {
    pub fn call_mode(&self) -> bool {
        (self.in_bt && self.in_running)
            || (self.out_bt && self.out_rate > 0 && self.out_rate <= 24_000)
    }

    pub fn describe(&self) -> String {
        format!(
            "output {}{} {} Hz, input {}{} {} Hz{}, {}",
            self.out_dev,
            if self.out_bt { " (bluetooth)" } else { "" },
            self.out_rate,
            self.in_dev,
            if self.in_bt { " (bluetooth)" } else { "" },
            self.in_rate,
            if self.in_running { " recording" } else { "" },
            if self.call_mode() {
                "call mode (HFP)"
            } else {
                "media mode"
            }
        )
    }
}

#[derive(Debug, Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct RouteWatch {
    last: Option<Route>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl RouteWatch {
    pub fn feed(&mut self, r: Route) -> (Option<String>, bool) {
        let prev = self.last.replace(r);
        if prev == Some(r) {
            return (None, false);
        }
        let back = prev.is_some_and(|p| p.call_mode()) && !r.call_mode();
        (Some(format!("route: {}", r.describe())), back)
    }
}

pub fn tap_test(secs: u64, with_mic: bool) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let Some(mut b) = platform_backend(tx) else {
        anyhow::bail!("no headphone tap support on this platform yet (macOS only)");
    };
    ECHO.store(true, std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "Now Playing claimed for {secs} s: tap your headphones or press the play/pause key{}",
        if with_mic {
            "; the mic is open (open mic): double tap to close it, again to open it"
        } else {
            ""
        }
    );
    let vc = if with_mic {
        crate::config::Config::load_or_init()?.voice
    } else {
        Default::default()
    };
    let mut mic = if with_mic { TestMic::open(&vc) } else { None };
    let mut gate = Gate::default();
    let mut listening = mic.is_some();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        b.pump(listening, false);
        let mut presses = vec![];
        if let Ok(AppEvent::MediaKey(p)) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
            presses.push(p);
        }
        for ev in crate::voice::capture::take_events() {
            println!(
                "{}",
                serde_json::json!({"helper": format!("{ev:?}"), "at_ms": now_ms()})
            );
            if let Some(p) = crate::voice::capture::helper_press(&ev) {
                presses.push(p);
            }
        }
        if mic.as_ref().is_some_and(|m| m.ended()) {
            println!("{}", serde_json::json!({"mic": "stream ended by itself"}));
            mic = None;
            listening = false;
        }
        if let Some(m) = mic.as_mut() {
            m.report();
        }
        for p in presses {
            let v = gate.decide(p, now_ms(), listening, b.reclaimed_ms());
            let what = match &v {
                Verdict::On => "on".to_string(),
                Verdict::Off => "off".to_string(),
                Verdict::Ignored(why) => format!("ignored: {why}"),
            };
            log(&format!("tap-test: {} ({:?}) -> {what}", p.source, p.cmd));
            println!(
                "{}",
                serde_json::json!({"source": p.source, "cmd": format!("{:?}", p.cmd), "mic": what, "at_ms": now_ms()})
            );
            match v {
                Verdict::On => {
                    listening = true;
                    if with_mic && mic.is_none() {
                        mic = TestMic::open(&vc);
                    }
                }
                Verdict::Off => {
                    listening = false;
                    if mic.take().is_some() {
                        println!(
                            "{}",
                            serde_json::json!({"mic": "closed", "at_ms": now_ms()})
                        );
                    }
                }
                Verdict::Ignored(_) => {}
            }
        }
    }
    drop(mic);
    b.release();
    ECHO.store(false, std::sync::atomic::Ordering::Relaxed);
    eprintln!("released: play/pause goes to the other apps again");
    Ok(())
}

struct TestMic {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
    level: std::sync::Arc<std::sync::atomic::AtomicU32>,
    shown: std::time::Instant,
}

impl TestMic {
    fn open(vc: &crate::config::VoiceCfg) -> Option<TestMic> {
        use crate::voice::{audio, capture};
        let cap = match capture::open_helper_mic(vc.voice_processing, true) {
            Ok(c) => {
                println!(
                    "{}",
                    serde_json::json!({"mic": "open", "capture": "godterm-speech", "vp": c.vp, "at_ms": now_ms()})
                );
                c
            }
            Err(e) => {
                println!(
                    "{}",
                    serde_json::json!({"mic": format!("the helper failed ({e}); ffmpeg instead, with no helper events")})
                );
                match audio::Capture::start(&vc.ffmpeg, &audio::Source::Mic(vc.device.clone())) {
                    Ok(c) => c,
                    Err(e) => {
                        println!(
                            "{}",
                            serde_json::json!({"mic": format!("cannot open: {e}")})
                        );
                        return None;
                    }
                }
            }
        };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let level = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (s, e, l) = (stop.clone(), ended.clone(), level.clone());
        std::thread::spawn(move || {
            let mut cap = cap;
            let mut frame = vec![];
            while !s.load(std::sync::atomic::Ordering::Relaxed) {
                if cap.frame(&mut frame).is_none() {
                    e.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
                l.store(
                    audio::db(audio::rms(&frame)).to_bits(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        });
        Some(TestMic {
            stop,
            ended,
            level,
            shown: std::time::Instant::now(),
        })
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn report(&mut self) {
        if self.shown.elapsed() >= std::time::Duration::from_secs(3) {
            self.shown = std::time::Instant::now();
            let db = f32::from_bits(self.level.load(std::sync::atomic::Ordering::Relaxed));
            println!(
                "{}",
                serde_json::json!({"mic": "open", "level_db": (db * 10.0).round() / 10.0})
            );
        }
    }
}

impl Drop for TestMic {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

static LOG_LOCK: Mutex<()> = Mutex::new(());

static ECHO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn log(msg: &str) {
    if ECHO.load(std::sync::atomic::Ordering::Relaxed) {
        println!("{}", serde_json::json!({"log": msg}));
    }
    crate::log::info(&format!("media-keys: {msg}"));
    let _g = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = crate::config::app_home().join("media-keys.log");

    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::remove_file(&path);
    }
    use crate::platform::OpenOptionsExt;
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
    {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(cmd: Cmd) -> Press {
        Press {
            cmd,
            source: "test",
        }
    }

    #[test]
    fn commands_map_to_toggle_or_off_only() {
        assert_eq!(Cmd::TogglePlayPause.action(), Action::Toggle);
        assert_eq!(Cmd::Play.action(), Action::Toggle);
        assert_eq!(Cmd::Next.action(), Action::Toggle);
        assert_eq!(Cmd::Previous.action(), Action::Toggle);
        assert_eq!(Cmd::Pause.action(), Action::StopOnly);
        assert_eq!(Cmd::Stop.action(), Action::StopOnly);
    }

    #[test]
    fn a_tap_toggles() {
        let mut g = Gate::default();
        assert_eq!(
            g.decide(press(Cmd::TogglePlayPause), 10_000, false, 0),
            Verdict::On
        );
        assert_eq!(
            g.decide(press(Cmd::TogglePlayPause), 11_000, true, 0),
            Verdict::Off
        );
        assert_eq!(g.decide(press(Cmd::Play), 12_000, false, 0), Verdict::On);
    }

    #[test]
    fn pause_only_turns_off() {
        let mut g = Gate::default();

        assert!(matches!(
            g.decide(press(Cmd::Pause), 10_000, false, 0),
            Verdict::Ignored(_)
        ));
        assert!(matches!(
            g.decide(press(Cmd::Stop), 11_000, false, 0),
            Verdict::Ignored(_)
        ));
        assert_eq!(g.decide(press(Cmd::Pause), 12_000, true, 0), Verdict::Off);
    }

    #[test]
    fn one_press_counts_once() {
        let mut g = Gate::default();
        assert_eq!(
            g.decide(press(Cmd::TogglePlayPause), 10_000, false, 0),
            Verdict::On
        );

        assert!(matches!(
            g.decide(press(Cmd::TogglePlayPause), 10_040, true, 0),
            Verdict::Ignored(_)
        ));
        assert_eq!(
            g.decide(press(Cmd::TogglePlayPause), 10_000 + DEBOUNCE_MS, true, 0),
            Verdict::Off
        );
    }

    #[test]
    fn a_reclaim_when_the_headset_leaves_call_mode() {
        let media = Route {
            out_dev: 80,
            out_rate: 48_000,
            out_bt: true,
            in_dev: 81,
            in_rate: 16_000,
            in_bt: true,
            in_running: false,
        };
        let call = Route {
            out_rate: 16_000,
            in_running: true,
            ..media
        };
        assert!(!media.call_mode() && call.call_mode());
        let mut w = RouteWatch::default();
        let (said, reclaim) = w.feed(media);
        assert!(
            said.unwrap().contains("media mode") && !reclaim,
            "first reading"
        );
        assert_eq!(w.feed(media), (None, false), "no change, nothing");

        let (said, reclaim) = w.feed(call);
        assert!(said.unwrap().contains("call mode") && !reclaim);

        let closing = Route {
            in_running: false,
            ..call
        };
        assert!(closing.call_mode());
        assert!(!w.feed(closing).1);
        assert!(w.feed(media).1, "back in media mode: reclaim");
        assert_eq!(w.feed(media), (None, false));

        let builtin = Route {
            in_dev: 90,
            in_bt: false,
            in_running: true,
            in_rate: 48_000,
            ..media
        };
        assert!(!builtin.call_mode());
        assert!(!w.feed(builtin).1);
    }

    #[test]
    fn ghosts_after_a_reclaim_are_dropped() {
        let mut g = Gate::default();
        assert!(matches!(
            g.decide(press(Cmd::Play), 10_500, false, 10_000),
            Verdict::Ignored(_)
        ));
        assert_eq!(
            g.decide(press(Cmd::Play), 11_600, false, 10_000),
            Verdict::On
        );
    }
}
