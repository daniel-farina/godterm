//! godterm: up to four Claude Code sessions side by side, one account each.

mod activity;
mod add_account;
mod admin;
mod app;
mod app_admin;
mod app_assistant;
mod app_failover;
mod app_followup;
mod app_grid;
mod app_layout;
mod app_learned;
mod app_loops;
mod app_memory;
mod app_mouse;
mod app_openmic;
mod app_pause;
mod app_privacy;
mod app_provider;
mod app_remote;
mod app_select;
mod app_sessions;
mod app_settings;
mod app_setup;
mod app_speaker;
mod app_sysprompt;
mod app_tabgroups;
mod app_tabhist;
mod app_tabmove;
mod app_talkback;
mod app_update;
mod app_voice;
mod app_wake;
mod assistant;
mod assistant_history;
mod assistant_memory;
#[cfg(test)]
mod click_focus_tests;
mod clock;
mod closed;
mod color_pick;
mod config;
#[cfg(test)]
mod confirm_click_tests;
mod control;
mod creds;
mod demo;
mod demo_agent;
mod demo_seed;
mod deps;
#[cfg(test)]
mod failover_tests;
mod fuzzy;
mod grid_layout;
#[cfg(test)]
mod grid_tests;
mod grok_logins;
mod guard;
mod harness;
mod hits;
mod install;
mod instance;
mod instant;
mod keys;
mod layout;
mod learned;
#[cfg(test)]
mod live_tests;
mod livemap;
mod log;
mod loops;
mod mcp;
mod menus;
mod migrate;
mod migrate_accounts;
#[cfg(test)]
mod newfolder_tests;
mod notify;
mod palette;
mod pane;
#[cfg(test)]
mod panel_view_tests;
mod panel_views;
// A real PTY and a shell script stub: Unix only.
#[cfg(all(test, unix))]
mod paste_enter_tests;
mod paths;
mod picker;
mod platform;
mod procs;
mod prompt;
#[cfg(test)]
mod provider_tests;
mod providers;
#[cfg(test)]
mod remote_tests;
mod risk;
mod select;
mod sess_sort;
mod session_index;
mod session_ops;
mod sessions;
mod settings;
mod setup;
#[cfg(test)]
mod setup_tests;
mod slot;
mod splash;
#[cfg(test)]
mod stale_tests;
mod state;
mod syscheck;
mod sysprompt;
mod tab_groups;
mod tab_history;
#[cfg(test)]
mod tabgroup_tests;
mod takeover;
#[cfg(test)]
mod talkback_tests;
mod test_guard;
#[cfg(test)]
mod test_stub;
mod theme;
mod trust;
mod ui;
mod ui_assistant;
mod ui_chrome;
mod ui_livemap;
mod ui_settings;
mod ui_updates;
mod update;
#[cfg(test)]
mod update_ui_tests;
mod usage;
mod usage_share;
mod voice;
#[cfg(test)]
mod wake_tests;
mod window;

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use app::{App, AppEvent};
use config::Config;

const USAGE_TEXT: &str = "\
godterm: run up to four Claude Code sessions side by side, each on its own account.

USAGE:
    godterm [--voice] [--wake] [--voice-file F]
                        start the TUI; --voice starts voice control (push to
                        talk), --wake listens for the wake word, --voice-file
                        feeds a recording through the voice pipeline (testing)
    godterm --demo     demo mode: made up accounts and sessions in ~/.godterm-demo
                        (never touches your real accounts, ~/.claude or ~/.grok)
    godterm setup      guided setup: choose accounts, then log each one in
    godterm doctor     check claude, voice tools, mic, terminal and keychain slots
    godterm window reset|place|show
                        forget / compute / show the saved window position
    godterm status     print accounts, config dirs, keychain service names and login state
    godterm usage      like status, plus live usage from the OAuth usage API
    godterm voice-test [--mic N | --file F [--levels] [--partials] [--echo]]
    godterm tts-test [--out DIR] [--play]   time Kokoro on CPU and CoreML
    godterm tts-test --grok TEXT [--source NAME] [--voice V] [--out F.wav]
                                           time one Grok (xAI) sentence, not played
    godterm voice install-speaker | speaker | speaker-eval --dir D | denoise --file F
                        speaker lock: fetch the voice model, show the profile, evaluate offline
    godterm migrate [--dry-run]    move a claudego install to ~/.godterm (runs once on its own)
    godterm migrate-accounts [--dry-run]
                                   move account slots into ~/.godterm/accounts, carrying logins over
    godterm mcp                    MCP server (stdio) with the tools of the running godterm
    godterm install [--dock]       the GodTerm app in ~/Applications and the godterm command in ~/.local/bin
    godterm uninstall [--purge]    remove them (--purge also deletes ~/.godterm)
                        check the voice setup; with the mic, show live levels,
                        noise floor and threshold for N seconds (default 5),
                        then how each utterance was heard and understood
    godterm --version  print the version
    godterm help       show this text

Config lives at ~/.godterm/config.toml (override the root with GODTERM_HOME).
Inside the TUI press Ctrl-a then ? for key bindings.";

#[derive(Default)]
struct TuiOpts {
    setup: bool,
    voice: bool,
    wake: bool,
    voice_file: Option<String>,
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() -> Result<()> {
    // Started as "claude" or "grok" by a test process: never run (a test
    // that launched this binary as an agent must not start a chain).
    if std::env::var_os(test_guard::ENV).is_some() {
        let a0 = std::env::args().next().unwrap_or_default();
        if matches!(procs::program_name(&a0).as_str(), "claude" | "grok") {
            std::process::exit(99);
        }
    }
    // `godterm --demo` links this binary as its demo claude and grok.
    if let Some(flavor) = demo_agent::invoked_as() {
        return demo_agent::run(flavor);
    }
    // --test-control (read by control::test_control) works with any mode.
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--test-control" && a != "--control-rw")
        .collect();
    // Demo mode: its own home with made up accounts (see demo.rs).
    if args.iter().any(|a| a == "--demo") {
        demo::enter(&args)?;
        let r = run_tui(TuiOpts::default());
        demo::leave();
        return r;
    }
    let arg = args.first().cloned();
    // The Windows Ctrl-C helper (see procs::interrupt): nothing else runs.
    if arg.as_deref() == Some("__ctrl-c") {
        let pid: u32 = args.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
        #[cfg(windows)]
        let code = if pid == 0 { 1 } else { procs::send_ctrl_c(pid) };
        #[cfg(unix)]
        let code = if pid != 0 && procs::interrupt(pid) {
            0
        } else {
            1
        };
        std::process::exit(code);
    }
    // The first launch splash on its own, to preview it (hidden).
    if arg.as_deref() == Some("splash") {
        return splash::run(&args[1..]);
    }
    // claudego -> GodTerm, once, only when the app itself starts (the TUI
    // or setup). Every other command leaves it alone and says it is due.
    let launches = match arg.as_deref() {
        None | Some("setup") => true,
        Some(a) => a.starts_with("--voice") || a == "--wake",
    };
    if launches {
        migrate::auto();
    } else if migrate::pending() && !matches!(arg.as_deref(), Some("migrate" | "mcp" | "window")) {
        eprintln!("godterm: migration pending, run godterm");
    }
    match arg.as_deref() {
        Some("__demo-say") => demo::say_command(&args[1..]),
        None => run_tui(TuiOpts::default()),
        Some(a) if a.starts_with("--voice") || a == "--wake" => run_tui(TuiOpts {
            voice: true,
            wake: args.iter().any(|a| a == "--wake"),
            voice_file: flag_value(&args, "--voice-file"),
            ..Default::default()
        }),
        Some("voice-test") => voice_test(
            flag_value(&args, "--file"),
            flag_value(&args, "--mic")
                .or_else(|| flag_value(&args, "--seconds"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
            args.iter().any(|a| a == "--levels"),
            args.iter().any(|a| a == "--partials"),
            args.iter().any(|a| a == "--echo"),
        ),
        Some("tts-test") if args.iter().any(|a| a == "--phonemes") => {
            // One line of text in, one line of Kokoro phonemes out.
            let cfg = Config::load_or_init()?.voice;
            let e = voice::kokoro::Espeak {
                bin: cfg.espeak.clone(),
                voice: "en-us".into(),
            };
            let v = voice::kokoro::vocab();
            for line in io::stdin().lines() {
                println!("{}", e.phonemize(&line?, &v)?);
            }
            Ok(())
        }
        Some("tts-test") if flag_value(&args, "--grok").is_some() => {
            // One sentence through Grok, timed, written to a WAV (never
            // played). The credential's source is printed, never the token.
            let cfg = Config::load_or_init()?;
            let mut g = cfg.voice.grok.clone();
            if let Some(v) = flag_value(&args, "--voice") {
                g.voice = v;
            }
            if let Some(s) = flag_value(&args, "--source") {
                g.source = s;
            }
            use voice::grok_tts;
            let all = grok_tts::sources(&cfg);
            let cred = grok_tts::credential(&g, &all, grok_tts::key_store().as_ref())
                .map_err(|e| anyhow::anyhow!(e.reason()))?;
            let text = flag_value(&args, "--grok").unwrap_or_default();
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let mut samples: Vec<f32> = vec![];
            let st = grok_tts::speak(grok_tts::API, &text, &g, &cred.token, &cancel, &mut |s| {
                samples.extend_from_slice(s);
                true
            })
            .map_err(|e| anyhow::anyhow!(e.reason()))?;
            let out = flag_value(&args, "--out")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| config::app_home().join("voice").join("grok-test.wav"));
            if let Some(d) = out.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&out, voice::kokoro::wav(&samples))?;
            println!("credential   {}", cred.from);
            println!("voice        {}", g.voice);
            println!("first audio  {} ms after the request", st.first_audio_ms);
            println!(
                "done         {} ms for {:.2} s of audio",
                st.total_ms,
                st.samples as f32 / grok_tts::SAMPLE_RATE as f32
            );
            println!("wrote        {}", out.display());
            Ok(())
        }
        Some("tts-test") if flag_value(&args, "--say").is_some() => {
            // Speak through the real pipeline (sentence streaming, output
            // device, pronunciation fixes). --volume 0 checks it silently.
            let mut cfg = Config::load_or_init()?.voice;
            if let Some(v) = flag_value(&args, "--volume").and_then(|v| v.parse().ok()) {
                cfg.tts_volume = v;
            }
            if let Some(v) = flag_value(&args, "--voice") {
                cfg.kokoro_voice = v;
            }
            let speaking = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let sp = voice::tts::Speaker::start(&cfg, std::sync::Arc::clone(&speaking));
            let t = Instant::now();
            while sp.engine() == "starting" && t.elapsed() < Duration::from_secs(30) {
                std::thread::sleep(Duration::from_millis(20));
            }
            println!("engine       {}", sp.engine());
            let t = Instant::now();
            sp.say(&flag_value(&args, "--say").unwrap_or_default());
            while !speaking.load(std::sync::atomic::Ordering::SeqCst)
                && t.elapsed() < Duration::from_secs(5)
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            while speaking.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
            println!(
                "spoke        in {:.2}s (see ~/.godterm/godterm.log for time to first audio)",
                t.elapsed().as_secs_f32()
            );
            Ok(())
        }
        Some("tts-test") => voice::tts::bench(
            &flag_value(&args, "--out")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| config::app_home().join("voice").join("tts-test")),
            args.iter().any(|a| a == "--play"),
        ),
        Some("setup") => run_tui(TuiOpts {
            setup: true,
            ..Default::default()
        }),
        Some("doctor") => setup::doctor(),
        Some("voice") => voice::speaker_cli::command(&args[1..]),
        Some("mcp") => mcp::run(),
        Some("migrate") => migrate::command(args.iter().any(|a| a == "--dry-run")),
        Some("grok-compat") => {
            // Write the compat switches into every grok slot's config.toml.
            let cfg = Config::load_or_init()?;
            for a in cfg
                .accounts
                .iter()
                .filter(|a| a.harness() == harness::Harness::Grok)
            {
                harness::grok::write_slot_compat(&a.config_dir(), &cfg.grok_claude_compat);
                println!(
                    "{}: {}",
                    a.name,
                    a.config_dir().join("config.toml").display()
                );
            }
            Ok(())
        }
        Some("index") => {
            // Build the session index once and say how long it took.
            let (tx, _rx) = std::sync::mpsc::channel();
            let app = app::App::new(Config::load_or_init()?, tx);
            let shared = std::sync::Arc::clone(&app.session_index);
            let sources = app.index_sources();
            for round in ["cold or from disk", "warm"] {
                session_index::rebuild(&shared, &sources);
                let ix = shared.lock().unwrap();
                println!(
                    "{round}: {} sessions in {} ms",
                    ix.rows.iter().filter(|r| r.info.parent.is_none()).count(),
                    ix.took_ms
                );
            }
            Ok(())
        }
        Some("stt-eval") => voice::eval::command(
            flag_value(&args, "--out").map(std::path::PathBuf::from),
            flag_value(&args, "--grok").and_then(|n| n.parse().ok()),
        ),
        Some("migrate-accounts") => {
            migrate_accounts::command(args.iter().any(|a| a == "--dry-run"))
        }
        Some("update") => update::command(&args[1..]),
        Some("install") => install::install(args.iter().any(|a| a == "--dock")),
        Some("uninstall") => install::uninstall(args.iter().any(|a| a == "--purge")),
        Some("window") => window_cmd(args.get(1).map(String::as_str)),
        Some("status") => print_status(false),
        Some("usage") => print_status(true),
        Some("help" | "-h" | "--help") => {
            println!("{USAGE_TEXT}");
            Ok(())
        }
        Some("-V" | "--version" | "version") => {
            println!(
                "godterm {} ({} {})",
                env!("CARGO_PKG_VERSION"),
                env!("GODTERM_GIT_SHA"),
                env!("GODTERM_BUILD_DATE")
            );
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown command {other:?}\n\n{USAGE_TEXT}");
            std::process::exit(2);
        }
    }
}

/// `godterm window place`: where the launcher should put the window, as
/// "left top right bottom" (nothing when remember_window is off).
/// `godterm window reset`: forget the saved window.
fn window_cmd(sub: Option<&str>) -> Result<()> {
    let cfg = Config::load_or_init().unwrap_or_default();
    let mut st = state::AppState::load().unwrap_or_default();
    match sub {
        Some("reset") => {
            st.window = None;
            st.save()?;
            println!("window position forgotten");
        }
        Some("place") | None => {
            if !cfg.remember_window {
                return Ok(());
            }
            let screens = window::query_screens();
            if let Some(r) = window::place(st.window.as_ref(), &screens) {
                let b = r.bounds();
                println!("{} {} {} {}", b[0], b[1], b[2], b[3]);
            }
        }
        // The launcher: bring a running GodTerm forward instead of
        // starting another (exit 0 when it did).
        Some("focus") => match instance::running() {
            Some(h) if instance::focus(&h) => println!("focused pid {}", h.pid),
            Some(h) => {
                println!("running as pid {} (could not focus its window)", h.pid);
                std::process::exit(1);
            }
            None => std::process::exit(1),
        },
        Some("show") => {
            println!("saved: {:?}", st.window);
            for s in window::query_screens() {
                println!(
                    "screen {:?} frame {:?} visible {:?}",
                    s.id, s.frame, s.visible
                );
            }
        }
        Some(other) => {
            eprintln!("unknown window command {other:?}: use place, focus, reset or show");
            std::process::exit(2);
        }
    }
    Ok(())
}

fn print_status(with_usage: bool) -> Result<()> {
    let cfg = Config::load_or_init()?;
    println!("config: {}", Config::path().display());
    for a in &cfg.accounts {
        let dir = a.config_dir();
        let snap = app::snapshot_of(a, with_usage);
        println!();
        println!("[{}] {}", a.name, a.display());
        println!("  config dir   {}", dir.display());
        println!(
            "  keychain     {}",
            creds::keychain_service(&dir.to_string_lossy())
        );
        match snap.login.source {
            Some(src) => {
                let exp = snap
                    .login
                    .expires_at
                    .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|| "?".into());
                println!(
                    "  login        yes (from {}), token expires {exp}",
                    src.label()
                );
            }
            None => println!("  login        no"),
        }
        if let Some(e) = &snap.profile.email {
            if setup::privacy_on(Some(&cfg)) {
                println!("  email        (hidden: privacy mode)");
            } else {
                println!("  email        {e}");
            }
        }
        if let Some(o) = &snap.profile.org_name {
            println!("  org          {o}");
        }
        if let Some(s) = snap
            .login
            .subscription
            .as_ref()
            .or(snap.profile.billing_type.as_ref())
        {
            println!("  plan         {s}");
        }
        match snap.usage {
            Some(Ok(u)) => {
                for w in u.windows {
                    let reset = w
                        .resets_at
                        .map(|t| format!(", resets in {}", usage::countdown(t, chrono::Utc::now())))
                        .unwrap_or_default();
                    println!("  {:<12} {:>5.1}%{reset}", w.label, w.utilization);
                }
            }
            Some(Err(e)) => println!("  usage        {e}"),
            None => {}
        }
    }
    Ok(())
}

fn voice_test(
    file: Option<String>,
    seconds: u32,
    levels: bool,
    partials: bool,
    echo: bool,
) -> Result<()> {
    use voice::audio::{Capture, Endpointer, Source};
    use voice::grammar;
    let cfg = Config::load_or_init()?;
    let vc = &cfg.voice;
    let whisper = voice::make_whisper(vc);
    println!("model        {}", config::expand_tilde(&vc.model).display());
    println!("server       {}", vc.whisper_server);
    println!("cli          {}", vc.whisper_cli);
    println!("ffmpeg       {}", vc.ffmpeg);
    println!("wake words   {}", vc.wake_words.join(", "));
    println!("threads      {}", whisper.threads());
    println!(
        "wake model   {}",
        voice::wake_model_path(vc)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none (large model screens the wake word)".into())
    );
    println!(
        "vad          threshold x{} over noise floor, min rms {}, end after {} ms silence",
        vc.vad_threshold, vc.vad_min_rms, vc.end_silence_ms
    );
    if let Err(e) = whisper.check() {
        println!("PROBLEM      {e}");
        std::process::exit(1);
    }
    let mut whisper = whisper;
    let t0 = Instant::now();
    match whisper.start_server() {
        Ok(()) => println!("server       ready in {:.1}s", t0.elapsed().as_secs_f32()),
        Err(e) => println!("server       unavailable ({e}), using whisper-cli"),
    }
    let instant = instant::table(&vc.instant);
    whisper.prompt = vc.wake_words.join(", ");
    // Partials run on the small model when there is one.
    let mut small = None;
    if partials {
        if let Some(m) = voice::wake_model_path(vc) {
            let mut w = voice::stt::Whisper::new(
                m.clone(),
                vc.whisper_cli.clone(),
                vc.whisper_server.clone(),
                vc.language.clone(),
            );
            if w.start_server().is_ok() {
                println!("partials     {}", m.display());
                small = Some(w);
            }
        }
    }
    let partial_port = small
        .as_ref()
        .and_then(|w| w.port())
        .or_else(|| whisper.port());
    let mut partial_at = 0usize;
    let mut partial_ms: Vec<u128> = vec![];
    let mut gate = voice::audio::EchoGate::new(vc.barge_in, vc.barge_margin_db);
    let (mut echo_muted, mut echo_barge) = (0usize, 0usize);
    let src = match &file {
        Some(f) => Source::File {
            path: f.clone(),
            realtime: false,
        },
        None => {
            println!(
                "listening for {seconds}s on device {:?}, speak now...",
                vc.device
            );
            Source::Mic(vc.device.clone())
        }
    };
    let mut cap = Capture::start(&vc.ffmpeg, &src)?;
    let mut ep = Endpointer::new(voice::vad_cfg(vc));
    let mut frame = vec![];
    let mut utts = vec![];
    let frames_max = (seconds as usize * voice::audio::RATE as usize) / voice::audio::FRAME;
    let mut n = 0;
    // Live meter for the mic (or with --levels for a file).
    let show_levels = file.is_none() || levels;
    let mut peak = 0f32;
    while cap.frame(&mut frame).is_some() {
        n += 1;
        voice::audio::apply_gain(&mut frame, vc.gain_db);
        let level = voice::audio::rms(&frame);
        peak = peak.max(level);
        // --echo: the file plays as if it were our own talk back coming
        // through the speakers, so nothing may get through.
        if echo {
            match gate.push(
                voice::audio::db(level),
                voice::audio::db(ep.threshold()),
                true,
                None,
            ) {
                voice::audio::Gate::Mute => {
                    echo_muted += 1;
                    ep.reset();
                    continue;
                }
                voice::audio::Gate::BargeIn => echo_barge += 1,
                voice::audio::Gate::Feed => {}
            }
        }
        let (_, utt) = ep.push(&frame);
        if let (true, Some(port), Some(buf)) = (partials, partial_port, ep.snapshot()) {
            let step = voice::audio::RATE as usize * 4 / 10;
            if buf.len() > voice::audio::RATE as usize * 6 / 10 && buf.len() >= partial_at + step {
                partial_at = buf.len();
                let t = Instant::now();
                let wav = voice::audio::wav_bytes(buf);
                let text = voice::stt::post_inference(port, &wav, &vc.language, &whisper.prompt)
                    .unwrap_or_else(|e| format!("(failed: {e})"));
                let ms = t.elapsed().as_millis();
                partial_ms.push(ms);
                println!(
                    "  partial at {:.1}s: \"{}\" ({ms} ms)",
                    buf.len() as f32 / voice::audio::RATE as f32,
                    text.trim()
                );
            }
        } else if ep.snapshot().is_none() {
            partial_at = 0;
        }
        if show_levels && n % 5 == 0 {
            let width = 30usize;
            let filled = ((peak / 8000.0).min(1.0) * width as f32) as usize;
            let state = if !ep.calibrated() {
                "calibrating"
            } else if peak > ep.threshold() {
                "SPEECH"
            } else {
                "quiet"
            };
            print!(
                "\r  {}{} {:>4.0} dBFS  rms {:>5.0}  floor {:>5.0}  threshold {:>5.0}  {:<11}",
                "█".repeat(filled),
                "░".repeat(width - filled),
                voice::audio::db(peak),
                peak,
                ep.floor(),
                ep.threshold(),
                state
            );
            let _ = io::stdout().flush();
            peak = 0.0;
        }
        if let Some(u) = utt {
            if show_levels {
                println!();
            }
            utts.push(u);
        }
        if file.is_none() && n >= frames_max {
            break;
        }
    }
    if show_levels {
        println!();
    }
    if n == 0 {
        println!("PROBLEM      no audio: {}", cap.error_text());
        std::process::exit(1);
    }
    if let Some(u) = ep.flush() {
        utts.push(u);
    }
    if echo {
        println!(
            "echo         {echo_muted} frames muted, {echo_barge} barge-in(s) (barge_in = {}), {} utterance(s) got through",
            vc.barge_in,
            utts.len()
        );
    }
    println!(
        "audio        {:.1}s read, {} utterance(s)",
        n as f32 * 0.03,
        utts.len()
    );
    for (i, u) in utts.iter().enumerate() {
        let t = Instant::now();
        let text = whisper.transcribe(u)?;
        let norm = grammar::normalize(&text);
        let woke = grammar::strip_wake(&norm, &vc.wake_words);
        let said = woke.unwrap_or(&norm);
        let route = match instant::match_instant(said, &instant) {
            Some(i) => format!("instant {i:?}"),
            None => "the assistant".to_string(),
        };
        println!(
            "\n[{}] {:.1}s audio, {:.2}s to transcribe\n  heard   {text}\n  wake    {}\n  goes to {route}",
            i + 1,
            u.len() as f32 / voice::audio::RATE as f32,
            t.elapsed().as_secs_f32(),
            if woke.is_some() { "yes" } else { "no" },
        );
    }
    if !partial_ms.is_empty() {
        partial_ms.sort();
        println!(
            "\npartials     {} requests, median {} ms, max {} ms",
            partial_ms.len(),
            partial_ms[partial_ms.len() / 2],
            partial_ms[partial_ms.len() - 1]
        );
    }
    if let Some(mut w) = small {
        w.shutdown();
    }
    whisper.shutdown();
    Ok(())
}

/// The TUI, then (after a restart to update) the new version in its place.
fn run_tui(opts: TuiOpts) -> Result<()> {
    let r = run_tui_inner(opts);
    if r.is_ok() {
        update::relaunch_if_requested();
    }
    r
}

fn run_tui_inner(opts: TuiOpts) -> Result<()> {
    // One GodTerm per home: a second one would resume the same sessions
    // twice and take the control socket.
    let _instance = match instance::acquire() {
        Ok(l) => Some(l),
        Err(h) if h.pid != 0 => {
            let at = h
                .tty
                .as_deref()
                .map(|t| format!(" in {t}"))
                .unwrap_or_default();
            eprintln!("GodTerm is already running (pid {}{at}).", h.pid);
            if instance::focus(&h) {
                eprintln!("Switched to its window.");
            } else {
                eprintln!("Switch to that window, or quit it (Ctrl-a q) to start here.");
            }
            std::process::exit(1);
        }
        Err(_) => None,
    };
    config::secure_home();
    // First run (no config yet), no accounts, or `godterm setup`: ask the
    // questions on the plain terminal, then walk through the logins.
    let first_run = !Config::path().exists();
    // On a first run the config is written only once setup completes, so
    // an interrupted setup (Ctrl-C) is offered again next time.
    let will_setup = opts.setup || (first_run && setup::stdin_is_tty());
    let mut cfg = if first_run && will_setup {
        Config::default()
    } else {
        Config::load_or_init()?
    };
    // The splash, once on a new home (before Setup) and once per update.
    if !opts.setup {
        splash::at_start(&cfg);
    }
    let mut onboarding = false;
    if opts.setup || ((first_run || cfg.accounts.is_empty()) && setup::stdin_is_tty()) {
        cfg = setup::interactive_setup(cfg)?;
        onboarding = true;
    }
    let (tx, rx) = mpsc::channel::<AppEvent>();
    theme::detect_truecolor();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        crossterm::event::EnableFocusChange
    )?;
    // Lets Shift+Enter and friends be told apart, where the terminal allows.
    let enhanced = matches!(supports_keyboard_enhancement(), Ok(true));
    if enhanced {
        let _ = execute!(
            stdout,
            // Event types give key release, which makes hold to talk work.
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        );
    }
    let restore = move || {
        let mut out = io::stdout();
        if enhanced {
            let _ = execute!(out, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            out,
            crossterm::event::DisableFocusChange,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        let _ = out.flush();
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        voice::kill_helpers();
        log::error(&format!("panic: {info}"));
        default_hook(info);
    }));

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    // Input thread feeds the same channel as PTY readers.
    let itx = tx.clone();
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            let mut last_key: Option<Instant> = None;
            while let Ok(ev) = event::read() {
                let ev = match ev {
                    event::Event::Key(k) => {
                        let burst =
                            last_key.is_some_and(|t| t.elapsed() < Duration::from_millis(15));
                        last_key = Some(Instant::now());
                        AppEvent::KeyRead(k, burst)
                    }
                    ev => AppEvent::Input(ev),
                };
                if itx.send(ev).is_err() {
                    return;
                }
            }
            // The terminal went away (window closed): shut down cleanly.
            let _ = itx.send(AppEvent::InputClosed);
        })?;
    // SIGTERM, SIGHUP (window closed), SIGINT and SIGQUIT end the loop so
    // every child (claude, whisper-server, ffmpeg) is stopped on the way out.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for &sig in crate::platform::EXIT_SIGNALS {
        let _ = signal_hook::flag::register(sig, std::sync::Arc::clone(&stop));
    }

    pane::set_scrollback(cfg.scrollback_lines);
    let ctl_tx = tx.clone();
    let mut app = App::new(cfg, tx);
    app.apply_buffers();
    // The control API for the assistant (and godterm mcp).
    let ctl_token = match control::serve(ctl_tx) {
        Ok(t) => Some(t),
        Err(e) => {
            log::info(&format!("control socket unavailable: {e:#}"));
            app.flash(format!(
                "The assistant's control socket is unavailable: {e:#}"
            ));
            None
        }
    };
    // Size the panes before the first spawn so claude starts at the right size.
    terminal.draw(|f| ui::draw(f, &mut app))?;
    // Restarted into a new version: every tab comes back at once.
    if std::env::var_os(update::EAGER_ENV).is_some() {
        std::env::remove_var(update::EAGER_ENV);
        app.force_eager = true;
    }
    let first_run = !crate::state::AppState::path().exists();
    app.autostart();
    app.start_update_checker();
    app.maybe_show_setup(first_run);
    if onboarding {
        app.start_onboarding();
    } else {
        app.maybe_start_tour();
    }
    app.voice.file = opts.voice_file.clone();
    app.voice.hold_supported = enhanced;
    if demo::active() {
        demo::tune(&mut app);
    }
    log::info(&format!("keyboard enhancement (hold to talk): {enhanced}"));
    if app.cfg.voice.mute_on_start {
        // Muted, remembering the mode it would have started in.
        app.voice.muted = true;
        app.voice.muted_prev = if opts.voice || app.cfg.voice.enabled {
            match app.cfg.voice.mode.as_str() {
                "open" => 3,
                "wake" => 2,
                _ => 1,
            }
        } else {
            0
        };
    } else if opts.voice || app.cfg.voice.enabled {
        if app.cfg.voice.mode == "open" && !opts.wake {
            app.voice.open_mic = true;
            app.start_voice(true);
        } else {
            let wake = opts.wake || app.cfg.voice.mode != "push";
            app.start_voice(wake);
        }
    }
    log::info("godterm started");

    let frame_budget = Duration::from_millis(16);
    let mut last_draw = Instant::now() - frame_budget;
    let mut last_tick = Instant::now();
    let mut dirty = true;
    let mut mouse_on = true;
    let result = loop {
        // The voice level meter animates at 25 fps while the mic is open.
        // The voice meter (25 fps) and the live map (30 fps) animate.
        let anim = app.anim_interval();
        if anim.is_some_and(|d| last_draw.elapsed() >= d) {
            dirty = true;
        }
        let timeout = if dirty {
            frame_budget.saturating_sub(last_draw.elapsed())
        } else if let Some(d) = anim {
            d.saturating_sub(last_draw.elapsed())
        } else {
            Duration::from_millis(200)
        };
        match rx.recv_timeout(timeout) {
            Ok(ev) => {
                app.handle(ev);
                dirty = true;
                // Drain what is queued, but never for longer than a frame:
                // under a flood of output the UI must still draw, tick and
                // notice quit.
                let budget = Instant::now();
                while budget.elapsed() < frame_budget {
                    match rx.try_recv() {
                        Ok(ev) => app.handle(ev),
                        Err(_) => break,
                    }
                    if app.quit {
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
        if last_tick.elapsed() >= Duration::from_millis(250) {
            app.tick();
            last_tick = Instant::now();
            dirty = true;
        }
        if app.quit {
            break Ok(());
        }
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            log::info("signal received, shutting down");
            break Ok(());
        }
        // Ctrl-a m releases the mouse so the terminal can select text.
        if app.mouse_capture != mouse_on {
            mouse_on = app.mouse_capture;
            let mut out = io::stdout();
            let _ = if mouse_on {
                execute!(out, EnableMouseCapture)
            } else {
                execute!(out, DisableMouseCapture)
            };
            // Back from select mode: a full redraw drops the terminal's
            // own selection highlight (a full width band otherwise stays
            // over everything, the assistant panel included).
            if mouse_on {
                let _ = terminal.clear();
            }
            dirty = true;
        }
        // Under a steady stream of output 30 frames a second are plenty;
        // typing and other events still draw at 60.
        let budget = if app.streaming() {
            Duration::from_millis(33)
        } else {
            frame_budget
        };
        if dirty && last_draw.elapsed() >= budget {
            // One frame at once (terminals that support it), with the
            // cursor hidden while cells are written: no cursor flashing
            // across the screen. The frame shows it again at the input.
            let _ = crossterm::queue!(
                io::stdout(),
                crossterm::terminal::BeginSynchronizedUpdate,
                crossterm::cursor::Hide
            );
            let drawn = terminal.draw(|f| ui::draw(f, &mut app));
            let _ = crossterm::execute!(io::stdout(), crossterm::terminal::EndSynchronizedUpdate);
            match drawn {
                Ok(f) if demo::active() => demo::capture(f.buffer),
                Ok(_) => {}
                Err(e) => break Err(e.into()),
            }
            last_draw = Instant::now();
            dirty = false;
        }
    };

    app.shutdown();
    if let Some(t) = &ctl_token {
        control::cleanup(t);
    }
    restore();
    result
}
