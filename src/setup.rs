//! First run setup (`godterm setup`) and the `godterm doctor` checks.

use anyhow::Result;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::{expand_tilde, valid_name, AccountCfg, Config};
use crate::creds;
use crate::theme;

/// Account directory name from a label: lowercase letters, digits and '-'.
pub fn slug(label: &str, taken: &[String], n: usize) -> String {
    let mut s: String = label
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    let base = if valid_name(&s) {
        s
    } else {
        format!("account{n}")
    };
    let mut name = base.clone();
    let mut i = 2;
    while taken.contains(&name) {
        name = format!("{base}-{i}");
        i += 1;
    }
    name
}

fn ask(input: &mut dyn BufRead, prompt: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    io::stdout().flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(default.to_string());
    }
    // Drop control characters and escape sequences (stray arrow keys).
    let mut clean = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip "ESC [ ... final byte".
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() || n == '~' {
                        break;
                    }
                }
            }
        } else if !c.is_control() {
            clean.push(c);
        }
    }
    let t = clean.trim();
    Ok(if t.is_empty() {
        default.to_string()
    } else {
        t.to_string()
    })
}

/// Ask for a number of accounts (1 to 16). Keeps every other setting of `base`.
pub fn ask_accounts(input: &mut dyn BufRead, base: Config) -> Result<Config> {
    println!("godterm setup\n");
    println!("Each account gets its own Claude Code login, kept apart from your normal ~/.claude.");
    let n: usize = loop {
        let a = ask(input, "How many Claude accounts do you want to use", "2")?;
        match a.parse::<usize>() {
            Ok(n) if (1..=16).contains(&n) => break n,
            _ => println!("  please enter a number from 1 to 16 (more can be added later)"),
        }
    };
    let mut accounts: Vec<AccountCfg> = vec![];
    for i in 1..=n {
        println!("\nAccount {i} of {n}");
        let default_label = base
            .accounts
            .get(i - 1)
            .map(|a| a.display().to_string())
            .unwrap_or_else(|| {
                ["Work", "Personal", "Client", "Spare"]
                    .get(i - 1)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("Account {i}"))
            });
        let label = ask(input, "  Label", &default_label)?;
        let harness = if crate::harness::grok::installed(base.grok_bin.as_deref()) {
            let h = ask(
                input,
                "  Agent: claude (Claude Code) or grok (Grok Build)",
                base.accounts
                    .get(i - 1)
                    .map(|a| a.harness.as_str())
                    .unwrap_or("claude"),
            )?;
            crate::harness::Harness::of(&h).name().to_string()
        } else {
            crate::config::default_harness()
        };
        let dir = loop {
            let d = ask(input, "  Default directory for new sessions", "~")?;
            if expand_tilde(&d).is_dir() {
                break d;
            }
            println!("  {d} does not exist");
        };
        let taken: Vec<String> = accounts.iter().map(|a| a.name.clone()).collect();
        let name = base
            .accounts
            .get(i - 1)
            .filter(|a| a.display() == label)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| slug(&label, &taken, i));
        accounts.push(AccountCfg {
            name,
            label,
            color: theme::ROTATION[(i - 1) % theme::ROTATION.len()].into(),
            cwd: Some(dir),
            args: vec![],
            permission_mode: None,
            auto_trust: None,
            show_email: None,
            new_tab_base: None,
            scrollback_lines: None,
            config_dir: None,
            harness,
        });
    }
    println!("\nNext: godterm opens and logs each account in, one at a time.");
    println!("For each one, pick \"Claude account with subscription\", open the link in a browser");
    println!(
        "signed in to that account (a separate browser profile helps), and paste the code back.\n"
    );
    Ok(Config { accounts, ..base })
}

/// Run the questions on the real terminal and save the result.
pub fn interactive_setup(base: Config) -> Result<Config> {
    let stdin = io::stdin();
    let mut lock = stdin.lock();
    let cfg = ask_accounts(&mut lock, base)?;
    cfg.save()?;
    println!("Saved {}", Config::path().display());
    Ok(cfg)
}

pub fn stdin_is_tty() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let p = expand_tilde(bin);
    if p.components().count() > 1 {
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(bin))
            .find(|c| c.is_file())
    })
}

fn version_of(bin: &Path) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8(out.stdout)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
}

struct Report {
    problems: usize,
    /// Privacy mode: emails are masked.
    private: bool,
}

impl Report {
    fn m(&self, s: &str) -> String {
        if self.private {
            crate::app_privacy::mask_emails(s)
        } else {
            s.to_string()
        }
    }
    fn ok(&self, what: &str, detail: &str) {
        println!("  ok    {what:<22} {}", self.m(detail));
    }
    fn warn(&mut self, what: &str, detail: &str, fix: &str) {
        self.problems += 1;
        println!("  FIX   {what:<22} {}\n        -> {fix}", self.m(detail));
    }
    fn info(&self, what: &str, detail: &str) {
        println!("  info  {what:<22} {}", self.m(detail));
    }
}

/// Privacy mode is on (config, the runtime toggle, or --privacy).
pub fn privacy_on(cfg: Option<&Config>) -> bool {
    std::env::args().any(|a| a == "--privacy")
        || cfg.is_some_and(|c| c.privacy)
        || crate::state::AppState::load().and_then(|s| s.privacy) == Some(true)
}

/// `godterm doctor`: check every dependency and print fixes.
pub fn doctor() -> Result<()> {
    let mut r = Report {
        problems: 0,
        private: privacy_on(Config::load_or_init().ok().as_ref()),
    };
    println!("godterm doctor\n");
    println!("Core");
    let cfg = match Config::load_or_init() {
        Ok(c) => {
            r.ok("config", &Config::path().display().to_string());
            c
        }
        Err(e) => {
            r.warn(
                "config",
                &format!("{e:#}"),
                "fix the TOML syntax shown above, or move the file away and run godterm setup",
            );
            Config::default()
        }
    };
    match which(&cfg.claude_bin()) {
        Some(p) => r.ok("claude", &format!("{} ({})", p.display(), version_of(&p).unwrap_or_default())),
        None => r.warn(
            "claude",
            &format!("{} not found on PATH", cfg.claude_bin()),
            "install Claude Code (curl -fsSL https://claude.ai/install.sh | bash) or set claude_bin in config.toml",
        ),
    }
    if cfg.accounts.is_empty() {
        r.warn("accounts", "none configured", "run godterm setup");
    }

    println!("\nAccounts (each has its own CLAUDE_CONFIG_DIR and keychain item)");
    for a in &cfg.accounts {
        let dir = a.config_dir();
        let svc = creds::keychain_service(&dir.to_string_lossy());
        let mut q = Command::new("security");
        q.args(["find-generic-password", "-s", &svc]);
        let in_keychain = creds::output_with_timeout(q, std::time::Duration::from_secs(5))
            .map(|o| o.status.success())
            .unwrap_or(false);
        let file = dir.join(".credentials.json").is_file();
        let profile = creds::load_profile(&dir);
        let who = profile.email.unwrap_or_else(|| "no profile yet".into());
        let detail = format!("{svc} ({who})");
        if in_keychain || file {
            r.ok(
                &format!("[{}]", a.name),
                &format!(
                    "{detail}, logged in via {}",
                    if in_keychain { "keychain" } else { "file" }
                ),
            );
        } else {
            r.warn(
                &format!("[{}]", a.name),
                &format!("{detail}, not logged in"),
                "start godterm, focus the pane and press Enter (or Ctrl-a I)",
            );
        }
        let mode = cfg.mode_for(
            cfg.accounts
                .iter()
                .position(|x| x.name == a.name)
                .unwrap_or(0),
        );
        let args = crate::config::permission_args(&mode).join(" ");
        r.info(
            &format!("[{}] permissions", a.name),
            &format!(
                "{mode}{}",
                if args.is_empty() {
                    String::new()
                } else {
                    format!(" ({args})")
                }
            ),
        );
        if !a.work_dir().is_dir() {
            r.warn(
                &format!("[{}] cwd", a.name),
                &format!("{:?} missing", a.cwd),
                "set cwd for this account in config.toml",
            );
        }
    }

    println!("\nVoice (optional)");
    let vc = &cfg.voice;
    match which(&vc.ffmpeg) {
        Some(p) => r.ok("ffmpeg", &p.display().to_string()),
        None => r.warn("ffmpeg", "not found", "brew install ffmpeg"),
    }
    let server = which(&vc.whisper_server);
    let cli = which(&vc.whisper_cli);
    match (&server, &cli) {
        (Some(s), _) => r.ok("whisper-server", &s.display().to_string()),
        (None, Some(c)) => r.ok(
            "whisper-cli",
            &format!(
                "{} (no server: slower, model reloads per command)",
                c.display()
            ),
        ),
        (None, None) => r.warn(
            "whisper",
            "whisper-server and whisper-cli not found",
            "brew install whisper-cpp",
        ),
    }
    let model = expand_tilde(&vc.model);
    match std::fs::metadata(&model) {
        Ok(m) => r.ok("model", &format!("{} ({} MB)", model.display(), m.len() / 1_000_000)),
        Err(_) => r.warn(
            "model",
            &format!("{} missing", model.display()),
            "download one, e.g. curl -L -o ~/.cache/whisper-models/ggml-large-v3-turbo.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        ),
    }
    match crate::voice::wake_model_path(vc) {
        Some(p) => r.ok("wake model", &p.display().to_string()),
        None => r.info(
            "wake model",
            "none: the large model screens the wake word (fine, a bit more CPU)",
        ),
    }
    match crate::voice::tts::effective_engine(vc) {
        ("kokoro", _) => r.ok(
            "talk back",
            &format!(
                "kokoro {} on {} ({}; onnxruntime is linked into the binary, no dylib needed)",
                vc.kokoro_voice, vc.tts_provider, vc.espeak
            ),
        ),
        ("say", Some(why)) => r.warn(
            "talk back",
            &format!("say, because kokoro is unavailable: {why}"),
            "brew install espeak-ng; models from https://github.com/thewh1teagle/kokoro-onnx/releases (model-files-v1.0) in ~/.cache/kokoro-onnx",
        ),
        (e, _) => r.info("talk back", e),
    }
    if vc.engine == "grok" {
        use crate::voice::grok_tts;
        let all = grok_tts::sources_now();
        match grok_tts::credential(&vc.grok, &all, grok_tts::key_store().as_ref()) {
            Ok(c) => match grok_tts::fetch_voices(grok_tts::API, &c.token) {
                Ok(_) => r.ok(
                    "recognition",
                    &format!(
                        "grok ({}), {} reaches api.x.ai; microphone audio goes to xAI; fallback {}",
                        crate::voice::grok_stt::MODEL,
                        c.from,
                        vc.grok_stt.fallback
                    ),
                ),
                Err(e) => r.warn(
                    "recognition",
                    &format!("grok: {}: {}", c.from, e.reason()),
                    "Settings > Voice and Audio > Speech recognition: another login or an API key",
                ),
            },
            Err(e) => r.warn(
                "recognition",
                &format!("grok: {}", e.reason()),
                "log in to grok in a grok account (or ~/.grok), or set an xAI API key in Settings",
            ),
        }
    } else {
        r.info("recognition", &vc.engine);
    }
    let chain = crate::voice::engines::chain(vc);
    r.info("talk back order", &chain.join(", then "));
    if chain.contains(&"grok") {
        use crate::voice::grok_tts;
        let all = grok_tts::sources_now();
        match grok_tts::credential(&vc.grok, &all, grok_tts::key_store().as_ref()) {
            Ok(c) => {
                let t = std::time::Instant::now();
                match grok_tts::fetch_voices(grok_tts::API, &c.token) {
                    Ok(v) => r.ok(
                        "grok voice",
                        &format!(
                            "{} reaches api.x.ai ({} voices, {} ms), voice {}; spoken text goes to xAI",
                            c.from,
                            v.len(),
                            t.elapsed().as_millis(),
                            vc.grok.voice
                        ),
                    ),
                    Err(e) => r.warn(
                        "grok voice",
                        &format!("{}: {}", c.from, e.reason()),
                        "Settings > Voice and Audio > Grok voice: pick another login or set an API key",
                    ),
                }
            }
            Err(e) => r.warn(
                "grok voice",
                &e.reason(),
                "log in to grok in a grok account (or ~/.grok), or set an xAI API key in Settings",
            ),
        }
    }
    if which(&vc.ffmpeg).is_some() {
        let audio = crate::voice::input_devices(&vc.ffmpeg);
        if audio.is_empty() {
            r.warn(
                "microphones",
                "none listed",
                "connect a microphone, or check ffmpeg was built with avfoundation",
            );
        } else {
            r.ok("microphones", &audio.join(", "));
        }
        r.info(
            "mic permission",
            "macOS asks the terminal app on first use; if voice says no access, enable it in System Settings > Privacy & Security > Microphone",
        );
    }
    // Background voices: capture, voice processing, mic mode, speaker lock.
    let cap = crate::voice::capture::describe(vc);
    if cfg!(target_os = "macos") && !cap.starts_with("Apple") && vc.capture != "ffmpeg" {
        r.warn(
            "capture",
            &cap,
            "godterm install builds the godterm-speech helper (Apple voice processing, Voice Isolation)",
        );
    } else {
        r.ok("capture", &cap);
    }
    let apple_vp = cap.starts_with("Apple") && vc.voice_processing;
    r.info(
        "denoise",
        if crate::voice::capture::denoise_on(vc, apple_vp) {
            "RNNoise on (non speech noise)"
        } else {
            "off (Apple voice processing cleans the signal)"
        },
    );
    let spk = crate::voice::speaker::model_path(&vc.speaker_model);
    match (spk.is_file(), vc.speaker_lock.as_str()) {
        (_, "off") => r.info("speaker lock", "off"),
        (false, _) => r.warn(
            "speaker model",
            &format!("{} missing", spk.display()),
            "godterm voice install-speaker (about 27 MB, checked by sha256)",
        ),
        (true, lock) => {
            r.ok("speaker model", &spk.display().to_string());
            match crate::voice::speaker::SpeakerProfile::load() {
                Some(p) => r.ok("speaker lock", &format!("{lock}: {}", p.summary())),
                None => r.warn(
                    "speaker lock",
                    &format!("{lock}, but no voice profile"),
                    "Settings > Voice and Audio > Train my voice",
                ),
            }
        }
    }

    println!("\nInstall");
    let st = crate::install::Status::read();
    match (&st.app, &st.app_runs) {
        (Some(_), Some(b)) if b.exists() => r.ok("app", &st.lines()[0]),
        (Some(_), _) => r.warn(
            "app",
            &st.lines()[0],
            "the binary it runs is gone: godterm install",
        ),
        (None, _) => r.info(
            "app",
            "not installed: godterm install (adds GodTerm to ~/Applications)",
        ),
    }
    match &st.cli {
        Some(t) if t.exists() && st.cli_on_path => r.ok("command", &st.lines()[1]),
        Some(t) if t.exists() => r.warn(
            "command",
            &st.lines()[1],
            "add  export PATH=\"$HOME/.local/bin:$PATH\"  to ~/.zshrc",
        ),
        Some(_) => r.warn(
            "command",
            &st.lines()[1],
            "it points at a missing binary: godterm install",
        ),
        None => r.info(
            "command",
            "not installed: godterm install (links ~/.local/bin/godterm)",
        ),
    }

    println!("\nTerminal");
    let enhanced = if stdin_is_tty() {
        let _ = crossterm::terminal::enable_raw_mode();
        let v = matches!(
            crossterm::terminal::supports_keyboard_enhancement(),
            Ok(true)
        );
        let _ = crossterm::terminal::disable_raw_mode();
        Some(v)
    } else {
        None
    };
    match enhanced {
        Some(true) => r.ok("key release events", "supported: hold Ctrl-a space to talk"),
        Some(false) => r.info("key release events", "not supported here: Ctrl-a space toggles talking (iTerm2 3.5+, kitty, WezTerm or Ghostty support hold to talk)"),
        None => r.info("key release events", "not a terminal, skipped"),
    }
    let term = std::env::var("TERM_PROGRAM").unwrap_or_else(|_| "unknown".into());
    r.info(
        "terminal",
        &format!("{term}, TERM={}", std::env::var("TERM").unwrap_or_default()),
    );
    println!(
        "\n{}",
        if r.problems == 0 {
            "Everything looks good.".to_string()
        } else {
            format!("{} thing(s) to fix.", r.problems)
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Work Stuff!", &[], 1), "work-stuff");
        assert_eq!(slug("Work", &["work".into()], 2), "work-2");
        assert_eq!(slug("   ", &[], 3), "account3");
    }

    #[test]
    fn asks_for_accounts() {
        let tmp = std::env::temp_dir();
        let t = tmp.to_string_lossy();
        let answers = format!("0\n2\nWork\n/definitely/missing\n{t}\nHo\x1b[1~me\n\n");
        let mut input = io::Cursor::new(answers.into_bytes());
        let cfg = ask_accounts(
            &mut input,
            Config {
                accounts: vec![],
                ..Config::default()
            },
        )
        .unwrap();
        assert_eq!(cfg.accounts.len(), 2);
        assert_eq!(cfg.accounts[0].name, "work");
        assert_eq!(cfg.accounts[0].cwd.as_deref(), Some(&*t));
        assert_eq!(cfg.accounts[1].label, "Home");
        assert_eq!(cfg.accounts[1].cwd.as_deref(), Some("~"));
        assert_ne!(cfg.accounts[0].color, cfg.accounts[1].color);
    }
}
