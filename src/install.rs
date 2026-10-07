//! `godterm install` / `godterm uninstall`: the GodTerm.app launcher in
//! ~/Applications, the `godterm` command in ~/.local/bin, LaunchServices
//! registration, and (only when asked) a Dock icon.
//!
//! The app and the symlink point at this binary's real path, so a rebuild
//! takes effect at the next launch without reinstalling.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const ICON: &[u8] = include_bytes!("../assets/godterm.icns");
const LSREGISTER: &str =
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

pub fn app_path() -> PathBuf {
    crate::config::home_dir()
        .join("Applications")
        .join("GodTerm.app")
}

pub fn cli_link() -> PathBuf {
    crate::config::home_dir()
        .join(".local")
        .join("bin")
        .join("godterm")
}

/// This binary, symlinks resolved.
pub fn real_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this binary")?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// What is installed, for doctor and Settings > About.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub app: Option<PathBuf>,
    /// The binary the app launches.
    pub app_runs: Option<PathBuf>,
    /// Where ~/.local/bin/godterm points (None: missing).
    pub cli: Option<PathBuf>,
    pub cli_on_path: bool,
}

impl Status {
    pub fn read() -> Status {
        let app = app_path();
        let app_runs = std::fs::read_to_string(app.join("Contents/Resources/bin.path"))
            .ok()
            .map(|s| PathBuf::from(s.trim()));
        let link = cli_link();
        let cli = std::fs::read_link(&link)
            .ok()
            .map(|t| {
                if t.is_absolute() {
                    t
                } else {
                    link.parent().unwrap_or(Path::new("/")).join(t)
                }
            })
            .or_else(|| link.is_file().then(|| link.clone()));
        Status {
            app: app.is_dir().then_some(app),
            app_runs,
            cli,
            cli_on_path: on_path(link.parent().unwrap_or(Path::new("/"))),
        }
    }

    /// One line per part, for screens.
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![];
        v.push(match (&self.app, &self.app_runs) {
            (Some(a), Some(b)) => format!(
                "App: {} (runs {})",
                crate::config::tilde(a),
                crate::config::tilde(b)
            ),
            (Some(a), None) => format!("App: {}", crate::config::tilde(a)),
            (None, _) => "App: not installed (godterm install)".into(),
        });
        v.push(match &self.cli {
            Some(t) => format!(
                "Command: {} -> {}{}",
                crate::config::tilde(&cli_link()),
                crate::config::tilde(t),
                if self.cli_on_path {
                    ""
                } else {
                    " (~/.local/bin is not on PATH)"
                }
            ),
            None => "Command: not installed (godterm install)".into(),
        });
        v
    }
}

pub fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}

fn plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>GodTerm</string>
  <key>CFBundleDisplayName</key><string>GodTerm</string>
  <key>CFBundleIdentifier</key><string>dev.godterm.launcher</string>
  <key>CFBundleVersion</key><string>{version}</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>GodTerm</string>
  <key>CFBundleIconFile</key><string>godterm</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSMicrophoneUsageDescription</key><string>GodTerm listens for voice commands to control your Claude Code sessions. Audio is transcribed locally and never leaves this Mac.</string>
  <key>NSAppleEventsUsageDescription</key><string>GodTerm opens a terminal window to run godterm.</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#
    )
}

/// The command the terminal window runs (clean environment, title, and a
/// pause on exit so errors stay readable).
fn run_sh(bin: &Path) -> String {
    format!(
        r#"#!/bin/zsh -l
# Opened from inside a Claude Code session, the environment would carry its
# markers (CLAUDECODE, CLAUDE_CODE_*), which make every child claude act as
# a subagent with transcripts off. Start clean.
for v in $(env | sed -nE 's/^(CLAUDECODE|CLAUDE_CODE_[A-Za-z0-9_]*|CLAUDE_PID|CLAUDE_EFFORT)=.*/\1/p'); do
  unset "$v"
done
printf '\033]0;godterm\007'
"{bin}" "$@"
code=$?
echo
echo "godterm exited with code $code. Press Enter to close this window."
read -r _
"#,
        bin = bin.display()
    )
}

/// The bundle executable: opens iTerm (or Terminal) running run.sh, at
/// the remembered window position.
const LAUNCHER: &str = r#"#!/bin/sh
res="$(cd "$(dirname "$0")/../Resources" && pwd)"
run="$res/run.sh"
bin="$(cat "$res/bin.path")"
# Already running: bring its window forward instead of starting another.
if env -u CLAUDECODE "$bin" window focus >/dev/null 2>&1; then
  exit 0
fi
bounds="$(env -u CLAUDECODE "$bin" window place 2>/dev/null | head -1)"
set -- $bounds
if [ -d "/Applications/iTerm.app" ] || [ -d "$HOME/Applications/iTerm.app" ]; then
  osascript - "$run" "${1:-}" "${2:-}" "${3:-}" "${4:-}" <<'OSA'
on run argv
  set cmd to item 1 of argv
  tell application "iTerm"
    activate
    set w to (create window with default profile command cmd)
    if (item 2 of argv) is not "" then
      set bounds of w to {(item 2 of argv) as integer, (item 3 of argv) as integer, (item 4 of argv) as integer, (item 5 of argv) as integer}
    end if
  end tell
end run
OSA
else
  osascript - "$run" "${1:-}" "${2:-}" "${3:-}" "${4:-}" <<'OSA'
on run argv
  tell application "Terminal"
    activate
    do script quoted form of (item 1 of argv)
    if (item 2 of argv) is not "" then
      set bounds of front window to {(item 2 of argv) as integer, (item 3 of argv) as integer, (item 4 of argv) as integer, (item 5 of argv) as integer}
    end if
  end tell
end run
OSA
fi
"#;

fn chmod_x(p: &Path) -> Result<()> {
    use crate::platform::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Write the app bundle at `app` launching `bin`.
pub fn write_app(app: &Path, bin: &Path) -> Result<()> {
    if app.exists() {
        std::fs::remove_dir_all(app)
            .with_context(|| format!("removing the old {}", app.display()))?;
    }
    let contents = app.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(contents.join("Resources"))?;
    std::fs::write(contents.join("Resources/godterm.icns"), ICON)?;
    std::fs::write(
        contents.join("Info.plist"),
        plist(env!("CARGO_PKG_VERSION")),
    )?;
    let run = contents.join("Resources/run.sh");
    std::fs::write(&run, run_sh(bin))?;
    chmod_x(&run)?;
    std::fs::write(
        contents.join("Resources/bin.path"),
        format!("{}\n", bin.display()),
    )?;
    let exe = contents.join("MacOS/GodTerm");
    std::fs::write(&exe, LAUNCHER)?;
    chmod_x(&exe)?;
    Ok(())
}

/// Point `link` at `bin`. An existing symlink is replaced; a regular file
/// is left alone (returns false).
pub fn link_cli(link: &Path, bin: &Path) -> Result<bool> {
    if let Some(d) = link.parent() {
        std::fs::create_dir_all(d)?;
    }
    match std::fs::symlink_metadata(link) {
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(link).ok().as_deref() == Some(bin) {
                return Ok(true);
            }
            std::fs::remove_file(link)?;
        }
        Ok(_) => return Ok(false),
        Err(_) => {}
    }
    crate::platform::symlink(bin, link)?;
    Ok(true)
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    matches!(line.trim(), "y" | "Y" | "yes")
}

fn in_dock(app: &Path) -> bool {
    Command::new("defaults")
        .args(["read", "com.apple.dock", "persistent-apps"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&*app.to_string_lossy()))
        .unwrap_or(false)
}

fn add_to_dock(app: &Path) -> Result<()> {
    if in_dock(app) {
        println!("dock         already there");
        return Ok(());
    }
    if !confirm("Add GodTerm to the Dock (restarts the Dock)?") {
        println!("dock         skipped");
        return Ok(());
    }
    let entry = format!(
        "<dict><key>tile-data</key><dict><key>file-data</key><dict><key>_CFURLString</key><string>{}</string><key>_CFURLStringType</key><integer>0</integer></dict></dict></dict>",
        app.display()
    );
    let ok = Command::new("defaults")
        .args([
            "write",
            "com.apple.dock",
            "persistent-apps",
            "-array-add",
            &entry,
        ])
        .status()?
        .success();
    if !ok {
        bail!("defaults write failed");
    }
    let _ = Command::new("killall").arg("Dock").status();
    println!("dock         added");
    Ok(())
}

/// `godterm install [--dock]`.
pub fn install(dock: bool) -> Result<()> {
    let bin = real_exe()?;
    let app = app_path();
    write_app(&app, &bin)?;
    let _ = Command::new("touch").arg(&app).status();
    if Path::new(LSREGISTER).exists() {
        let _ = Command::new(LSREGISTER).arg("-f").arg(&app).status();
    }
    println!("app          {} (runs {})", app.display(), bin.display());
    let link = cli_link();
    match link_cli(&link, &bin)? {
        true => println!("command      {} -> {}", link.display(), bin.display()),
        false => println!(
            "command      {} is a regular file, left as is",
            link.display()
        ),
    }
    if !on_path(link.parent().unwrap_or(Path::new("/"))) {
        println!("             ~/.local/bin is not on PATH: add  export PATH=\"$HOME/.local/bin:$PATH\"  to ~/.zshrc");
    }
    replace_old_install(&bin)?;
    match build_speech_helper() {
        Ok(p) => println!("speech       {} (Apple on-device recognition)", p.display()),
        Err(e) => println!("speech       Apple helper not built ({e:#}); whisper is used"),
    }
    if dock {
        add_to_dock(&app)?;
    }
    println!("Open GodTerm from ~/Applications, Spotlight or Launchpad, or run godterm.");
    Ok(())
}

const SPEECH_SRC: &str = include_str!("../helpers/godterm-speech/main.swift");
const SPEECH_PLIST: &str = include_str!("../helpers/godterm-speech/Info.plist");

/// Compile `godterm-speech` (Apple SpeechAnalyzer, macOS 26+) with swiftc
/// into ~/.godterm/bin, when the source changed.
pub fn build_speech_helper() -> Result<PathBuf> {
    let out = crate::voice::user_apple_helper_path();
    let dir = out.parent().context("no bin dir")?.to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let src = dir.join("godterm-speech.swift");
    let plist = dir.join("godterm-speech.plist");
    if out.is_file() && std::fs::read_to_string(&src).is_ok_and(|s| s == SPEECH_SRC) {
        return Ok(out);
    }
    std::fs::write(&src, SPEECH_SRC)?;
    std::fs::write(&plist, SPEECH_PLIST)?;
    let st = Command::new("xcrun")
        .args(["swiftc", "-O", "-target", "arm64-apple-macos26.0"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .args([
            "-Xlinker",
            "-sectcreate",
            "-Xlinker",
            "__TEXT",
            "-Xlinker",
            "__info_plist",
            "-Xlinker",
        ])
        .arg(&plist)
        .stdout(std::process::Stdio::null())
        .status()
        .context("running xcrun swiftc (install the Xcode command line tools)")?;
    if !st.success() {
        let _ = std::fs::remove_file(&src);
        anyhow::bail!("swiftc failed");
    }
    Ok(out)
}

/// The claudego app and command from before the rename: the app goes, the
/// `claudego` command becomes a shim that says so and runs godterm.
fn replace_old_install(bin: &Path) -> Result<()> {
    let home = crate::config::home_dir();
    let old_app = home.join("Applications").join("ClaudeGo.app");
    if old_app.is_dir() {
        println!(
            "old app      removing {} (claudego is now GodTerm)",
            old_app.display()
        );
        std::fs::remove_dir_all(&old_app)?;
        if Path::new(LSREGISTER).exists() {
            let _ = Command::new(LSREGISTER).arg("-u").arg(&old_app).status();
        }
    }
    let old_link = home.join(".local").join("bin").join("claudego");
    let is_link = std::fs::symlink_metadata(&old_link).is_ok_and(|m| m.file_type().is_symlink());
    let is_shim =
        std::fs::read_to_string(&old_link).is_ok_and(|t| t.contains("renamed to godterm"));
    if is_link || is_shim {
        std::fs::remove_file(&old_link)?;
        std::fs::write(&old_link, shim(bin))?;
        chmod_x(&old_link)?;
        println!(
            "old command  {} now says \"renamed to godterm\" and runs godterm",
            old_link.display()
        );
    }
    Ok(())
}

/// The `claudego` shim.
pub fn shim(bin: &Path) -> String {
    format!("#!/bin/sh\necho \"claudego was renamed to godterm; running godterm\" >&2\nexec \"{}\" \"$@\"\n", bin.display())
}

/// `godterm uninstall [--purge]`.
pub fn uninstall(purge: bool) -> Result<()> {
    let app = app_path();
    let link = cli_link();
    println!("This removes {} and {}.", app.display(), link.display());
    if !confirm("Uninstall?") {
        println!("Nothing removed.");
        return Ok(());
    }
    if app.exists() {
        std::fs::remove_dir_all(&app)?;
        println!("removed      {}", app.display());
    }
    if std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(&link)?;
        println!("removed      {}", link.display());
    }
    if in_dock(&app) {
        println!("             the Dock still shows GodTerm: drag it out to remove it");
    }
    let home = crate::config::app_home();
    if purge {
        if confirm(&format!(
            "Also delete {} (config, logins of every account, state)?",
            home.display()
        )) {
            std::fs::remove_dir_all(&home)?;
            println!("removed      {}", home.display());
        }
    } else {
        println!(
            "kept         {} (config and account logins; --purge deletes it)",
            home.display()
        );
    }
    Ok(())
}

/// (A macOS app bundle and a symlinked command: Unix only.)
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn writes_a_bundle_and_links() {
        let d = std::env::temp_dir().join(format!("cg-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let app = d.join("Applications/GodTerm.app");
        let bin = PathBuf::from("/opt/godterm/target/release/godterm");
        write_app(&app, &bin).unwrap();
        let c = app.join("Contents");
        assert!(std::fs::read_to_string(c.join("Info.plist"))
            .unwrap()
            .contains("dev.godterm.launcher"));
        assert_eq!(
            std::fs::read_to_string(c.join("Resources/bin.path"))
                .unwrap()
                .trim(),
            bin.to_string_lossy()
        );
        assert!(std::fs::read_to_string(c.join("Resources/run.sh"))
            .unwrap()
            .contains("\"/opt/godterm/target/release/godterm\" \"$@\""));
        assert!(c.join("Resources/godterm.icns").is_file());
        use crate::platform::PermissionsExt;
        assert!(
            std::fs::metadata(c.join("MacOS/GodTerm"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111
                != 0
        );
        // Rewriting replaces it cleanly.
        write_app(&app, &bin).unwrap();
        // The command symlink: created, replaced, and a regular file kept.
        let link = d.join("bin/godterm");
        assert!(link_cli(&link, &bin).unwrap());
        assert_eq!(std::fs::read_link(&link).unwrap(), bin);
        let other = PathBuf::from("/elsewhere/godterm");
        assert!(link_cli(&link, &other).unwrap());
        assert_eq!(std::fs::read_link(&link).unwrap(), other);
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "not a link").unwrap();
        assert!(!link_cli(&link, &bin).unwrap());
        let _ = std::fs::remove_dir_all(d);
    }
}
