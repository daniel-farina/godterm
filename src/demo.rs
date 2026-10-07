//! `godterm --demo`: GodTerm on made up accounts and sessions, for demos
//! and the promo video.
//!
//! Everything lives in its own home (`~/.godterm-demo`, or
//! `$GODTERM_DEMO_HOME`): six fake accounts (four Claude, two Grok) with
//! fake logins, usage read from fixture files (one burns down live),
//! sixteen tabs running the demo agent (`demo_agent`) instead of claude
//! or grok, and a scripted assistant brain. The user's real `~/.godterm`,
//! `~/.claude`, `~/.grok`, keychain and accounts are never read or
//! written: the lookups that would reach them check `assert_safe` and
//! panic in demo mode.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Set (to "1") for the demo GodTerm and everything it starts.
pub const ENV: &str = "GODTERM_DEMO";
/// The marker beside the demo agent links in `<home>/bin`.
pub const AGENT_MARKER: &str = ".godterm-demo-agent";

/// Demo mode is on (this process was started with `--demo`).
pub fn active() -> bool {
    std::env::var(ENV).is_ok_and(|v| v == "1")
}

/// Where the demo keeps everything.
pub fn home() -> PathBuf {
    std::env::var("GODTERM_DEMO_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home_dir().join(".godterm-demo"))
}

/// Panics when any of the dirs points at the user's real data. Called by
/// `config::dirs()` in demo mode, so nothing can reach it.
pub fn assert_safe(d: &crate::config::Dirs) {
    for p in [&d.app_home, &d.main_claude, &d.main_grok] {
        assert!(
            !crate::config::is_real_user_dir(p),
            "demo isolation: {} is a real user dir",
            p.display()
        );
    }
}

/// The keychain is never read in demo mode (fake logins are files).
pub fn keychain_allowed() -> bool {
    debug_assert!(!active(), "demo isolation: keychain read in demo mode");
    !active()
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// `godterm __demo-say`: Kokoro to WAV files, never played. For the video
/// narration. `--voice V --speed S --out F.wav TEXT`, or `--batch F.json
/// --outdir D` with `[{"id", "text", "voice"?, "speed"?}]`. Uses the
/// stock voice settings, not the user's config.
pub fn say_command(args: &[String]) -> Result<()> {
    use crate::voice::kokoro::{self, Kokoro, Provider};
    let cfg = crate::config::VoiceCfg::default();
    let ex = crate::config::expand_tilde;
    let mut k = Kokoro::load(
        &ex(&cfg.kokoro_model),
        &ex(&cfg.kokoro_voices),
        &cfg.espeak,
        Provider::Cpu,
        0,
    )?;
    let fx = crate::voice::tts::fixes(&cfg);
    let mut jobs: Vec<(PathBuf, String, String, f32)> = vec![];
    let voice = arg(args, "--voice").unwrap_or_else(|| cfg.kokoro_voice.clone());
    let speed: f32 = arg(args, "--speed")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    if let Some(b) = arg(args, "--batch") {
        let out = PathBuf::from(arg(args, "--outdir").unwrap_or_else(|| ".".into()));
        std::fs::create_dir_all(&out)?;
        let list: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&b)?)?;
        for j in list {
            let id = j["id"].as_str().context("batch entry without id")?;
            jobs.push((
                out.join(format!("{id}.wav")),
                j["text"].as_str().unwrap_or("").to_string(),
                j["voice"].as_str().unwrap_or(&voice).to_string(),
                j["speed"].as_f64().map(|s| s as f32).unwrap_or(speed),
            ));
        }
    } else {
        let out = arg(args, "--out").context("--out F.wav (or --batch)")?;
        let skip: Vec<&str> = ["--voice", "--speed", "--out"].to_vec();
        let mut words = vec![];
        let mut i = 0;
        while i < args.len() {
            if skip.contains(&args[i].as_str()) {
                i += 2;
                continue;
            }
            words.push(args[i].clone());
            i += 1;
        }
        jobs.push((PathBuf::from(out), words.join(" "), voice, speed));
    }
    let mut report = vec![];
    for (path, text, voice, speed) in jobs {
        let t = kokoro::apply_fixes(&text, &fx);
        // Sentence by sentence (Kokoro's input limit), joined with a short
        // breath between them.
        let mut all: Vec<f32> = vec![];
        for s in crate::voice::tts::chunk_sentences(&t) {
            let (samples, _) = k.synth(&s, &voice, speed)?;
            if !all.is_empty() {
                all.extend(std::iter::repeat_n(0.0, 24_000 * 18 / 100));
            }
            all.extend(samples);
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&path, kokoro::wav(&all))?;
        let secs = all.len() as f64 / 24_000.0;
        println!("{:.2}s  {}  {}", secs, voice, path.display());
        report.push(json!({"file": path, "seconds": secs, "voice": voice, "text": text}));
    }
    if let Some(r) = arg(args, "--report") {
        std::fs::write(r, serde_json::to_string_pretty(&report)?)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Starting the demo

/// `godterm --demo`: build the demo world and point this process (and
/// everything it starts) at it. Options: `--demo-manual` (no automatic
/// usage burn; a driver script sets usage over the control socket),
/// `--demo-no-external` (no external session in tmux).
pub fn enter(args: &[String]) -> Result<()> {
    let home = home();
    let p = crate::demo_seed::seed(&home)?;
    std::env::set_var(ENV, "1");
    std::env::set_var("GODTERM_HOME", &p.app);
    std::env::set_var("GODTERM_MAIN_DIR", &p.main_claude);
    std::env::set_var("GODTERM_MAIN_GROK_DIR", &p.main_grok);
    std::env::set_var("GODTERM_USAGE_FIXTURE", &p.usage);
    // The driver script and godterm mcp may change things (sandboxed).
    std::env::set_var("GODTERM_CONTROL_RW", "1");
    // Nothing opens on the desktop (Finder, editors, the browser).
    std::env::set_var("GODTERM_NO_OPEN", "1");
    for v in [
        "CLAUDEGO_HOME",
        "CLAUDEGO_MAIN_DIR",
        "CLAUDEGO_USAGE_FIXTURE",
    ] {
        std::env::remove_var(v);
    }
    // Checked now, before anything else looks.
    assert_safe(&crate::config::dirs());
    let manual = args.iter().any(|a| a == "--demo-manual");
    if !manual {
        // Work burns from 40% to 0 over 90 s, starting 20 s in.
        start_burn(crate::demo_seed::BURN_ACCOUNT, 40.0, 0.0, 90.0, 20.0);
    }
    if !args.iter().any(|a| a == "--demo-no-external") {
        start_external(&p);
    }
    Ok(())
}

/// The tmux server the external session runs in.
const TMUX_SOCKET: &str = "godterm-demo";

/// A claude "in another terminal": the demo agent on the main dir in a
/// tmux session of its own (`tmux -L godterm-demo attach` to watch it).
fn start_external(p: &crate::demo_seed::Paths) {
    if crate::voice::tts::which("tmux").is_none() {
        return;
    }
    let _ = std::process::Command::new("tmux")
        .args(["-L", TMUX_SOCKET, "kill-server"])
        .stderr(std::process::Stdio::null())
        .status();
    let cwd = p.work.join("legacy-api");
    let agent = p.bin.join("claude");
    let _ = std::process::Command::new("tmux")
        .args(["-L", TMUX_SOCKET, "new-session", "-d", "-s", "external"])
        .args(["-x", "110", "-y", "32", "-c"])
        .arg(&cwd)
        .arg("env")
        .arg(format!("CLAUDE_CONFIG_DIR={}", p.main_claude.display()))
        .arg(&agent)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// On the way out: stop the external session.
pub fn leave() {
    if active() {
        let _ = std::process::Command::new("tmux")
            .args(["-L", TMUX_SOCKET, "kill-server"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Demo touches on the app: the voice strip shows the wake word mode
/// (no mic is opened and nothing is ever spoken aloud).
pub fn tune(app: &mut crate::app::App) {
    app.voice.always_on = true;
    app.voice.status = crate::voice::VoiceStatus::Idle;
    app.flash("Demo mode: made up accounts and sessions (~/.godterm-demo)");
}

// ---------------------------------------------------------------------
// Usage over time

struct Burn {
    account: String,
    from: f64,
    to: f64,
    secs: f64,
    start: std::time::Instant,
}

static BURNS: std::sync::Mutex<Vec<Burn>> = std::sync::Mutex::new(vec![]);
static TIMELINE: std::sync::Once = std::sync::Once::new();

fn acct(name: &str) -> Option<&'static crate::demo_seed::Acct> {
    let n = name.to_lowercase();
    crate::demo_seed::ACCOUNTS
        .iter()
        .find(|a| a.name == n || a.label.to_lowercase() == n)
}

/// Write an account's usage fixture now.
pub fn set_usage(name: &str, five_left: Option<f64>, week_left: Option<f64>) -> Result<()> {
    let a = acct(name).with_context(|| format!("no demo account {name}"))?;
    let dir = PathBuf::from(crate::config::env_var("USAGE_FIXTURE").context("no fixture dir")?);
    crate::config::write_private(
        &dir.join(format!("{}.json", a.name)),
        crate::demo_seed::usage_json(a, five_left.unwrap_or(a.five), week_left.unwrap_or(a.week)),
    )?;
    Ok(())
}

/// Burn `account`'s 5 hour window from `from` to `to` % left over `secs`,
/// after `delay` seconds.
pub fn start_burn(account: &str, from: f64, to: f64, secs: f64, delay: f64) {
    {
        let mut b = BURNS.lock().unwrap_or_else(|e| e.into_inner());
        b.retain(|x| x.account != account);
        b.push(Burn {
            account: account.to_string(),
            from,
            to,
            secs: secs.max(0.1),
            start: std::time::Instant::now() + std::time::Duration::from_secs_f64(delay.max(0.0)),
        });
    }
    TIMELINE.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("demo-usage".into())
            .spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let now = std::time::Instant::now();
                let mut b = BURNS.lock().unwrap_or_else(|e| e.into_inner());
                for x in b.iter() {
                    if now < x.start {
                        continue;
                    }
                    let t = ((now - x.start).as_secs_f64() / x.secs).min(1.0);
                    let left = x.from + (x.to - x.from) * t;
                    let _ = set_usage(&x.account, Some(left), None);
                }
                b.retain(|x| now < x.start || (now - x.start).as_secs_f64() < x.secs + 1.0);
            });
    });
}

// ---------------------------------------------------------------------
// The control API's demo tools (only in demo mode)

static SCREEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(vec![]);

/// Keep the last drawn frame as text (for `demo_screen`).
pub fn capture(buf: &ratatui::buffer::Buffer) {
    let a = buf.area;
    let mut lines = Vec::with_capacity(a.height as usize);
    for y in a.top()..a.bottom() {
        let mut s = String::with_capacity(a.width as usize);
        let mut skip = 0;
        for x in a.left()..a.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            let w = unicode_width::UnicodeWidthStr::width(sym);
            s.push_str(if sym.is_empty() { " " } else { sym });
            skip = w.saturating_sub(1);
        }
        lines.push(s);
    }
    *SCREEN.lock().unwrap_or_else(|e| e.into_inner()) = lines;
}

/// demo_state, demo_screen, demo_usage, demo_burn, demo_voice. None for
/// other tools.
pub fn control_tool(
    app: &mut crate::app::App,
    tool: &str,
    args: &Value,
) -> Option<Result<Value, String>> {
    if !tool.starts_with("demo_") || !active() {
        return None;
    }
    let f = |k: &str| args.get(k).and_then(Value::as_f64);
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    Some(match tool {
        "demo_state" => Ok(json!({"ok": true, "result": app.state_json()})),
        "demo_screen" => {
            let lines = SCREEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
            Ok(json!({"ok": true, "result": {"rows": lines.len(), "lines": lines}}))
        }
        "demo_usage" => {
            let a = s("account").unwrap_or_default();
            BURNS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|b| b.account != a);
            set_usage(&a, f("five_hour_left"), f("week_left"))
                .map(|_| json!({"ok": true}))
                .map_err(|e| e.to_string())
        }
        "demo_burn" => {
            start_burn(
                &s("account").unwrap_or_else(|| crate::demo_seed::BURN_ACCOUNT.into()),
                f("from").unwrap_or(40.0),
                f("to").unwrap_or(0.0),
                f("secs").unwrap_or(30.0),
                f("delay").unwrap_or(0.0),
            );
            Ok(json!({"ok": true}))
        }
        "demo_voice" => {
            use crate::voice::{VoiceEvent, VoiceStatus};
            if let Some(st) = s("status") {
                app.on_voice(VoiceEvent::Status(match st.as_str() {
                    "listening" => VoiceStatus::Listening,
                    "transcribing" => VoiceStatus::Transcribing,
                    _ => VoiceStatus::Idle,
                }));
            }
            if let Some(p) = s("partial") {
                app.on_voice(VoiceEvent::Partial(p, 180));
            }
            if let Some(h) = s("heard") {
                let words = h.split_whitespace().count() as u32;
                app.on_voice(VoiceEvent::Status(VoiceStatus::Idle));
                app.on_voice(VoiceEvent::Heard {
                    stats: crate::voice::wake_profile::UttStats {
                        ms: 300 * words.max(1),
                        mean_db: -28.0,
                        peak_db: -12.0,
                        stt_ms: 140,
                        speech_ms: 280 * words.max(1),
                        conf: Some(0.94),
                        no_speech: Some(0.01),
                        logprob: Some(-0.2),
                    },
                    text: h,
                    ptt: false,
                });
            }
            Ok(json!({"ok": true}))
        }
        _ => Err(format!("unknown demo tool {tool}")),
    })
}
