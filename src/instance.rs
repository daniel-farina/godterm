//! One GodTerm per home: an exclusive lock on `<home>/godterm.lock`, held
//! for the life of the TUI. A second start explains and points at the
//! running one (the launcher focuses its window instead of opening
//! another), so sessions never run twice and the control socket is never
//! taken over.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Held while this process runs the TUI.
pub struct Lock(#[allow(dead_code)] File);

/// Who holds the lock.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Holder {
    pub pid: u32,
    #[serde(default)]
    pub tty: Option<String>,
    #[serde(default)]
    pub terminal: Option<String>,
}

pub fn path() -> PathBuf {
    crate::config::app_home().join("godterm.lock")
}

fn open() -> std::io::Result<File> {
    use crate::platform::{DirBuilderExt, OpenOptionsExt};
    let home = crate::config::app_home();
    if !home.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&home)?;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path())
}

fn try_lock(f: &File) -> bool {
    crate::platform::try_lock(f)
}

/// Who holds it, in a file of its own: on Windows the lock is mandatory,
/// so nobody else can read the locked file itself.
fn holder_path() -> PathBuf {
    crate::config::app_home().join("godterm.lock.json")
}

fn read_holder(f: &mut File) -> Holder {
    if let Some(h) = std::fs::read_to_string(holder_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Holder>(&s).ok())
        .filter(|h| h.pid != 0)
    {
        return h;
    }
    let mut s = String::new();
    let _ = f.seek(SeekFrom::Start(0));
    let _ = f.read_to_string(&mut s);
    serde_json::from_str(&s).unwrap_or_default()
}

/// Take the lock, or say who has it.
pub fn acquire() -> Result<Lock, Holder> {
    let mut f = match open() {
        Ok(f) => f,
        // No lock file possible (read only home): do not block starting.
        Err(_) => return Err(Holder::default()),
    };
    if !try_lock(&f) {
        return Err(read_holder(&mut f));
    }
    let me = Holder {
        pid: std::process::id(),
        tty: crate::window::own_tty(),
        terminal: crate::window::terminal_app().map(str::to_string),
    };
    let json = serde_json::to_string(&me).unwrap_or_default();
    let _ = crate::config::write_private(&holder_path(), &json);
    let _ = f.set_len(0);
    let _ = f.seek(SeekFrom::Start(0));
    let _ = f.write_all(json.as_bytes());
    let _ = f.flush();
    Ok(Lock(f))
}

/// The running instance, if any (without taking the lock).
pub fn running() -> Option<Holder> {
    let mut f = std::fs::File::options()
        .read(true)
        .write(true)
        .open(path())
        .ok()?;
    if try_lock(&f) {
        crate::platform::unlock(&f);
        return None;
    }
    Some(read_holder(&mut f))
}

/// Bring the running instance's terminal window to the front.
#[cfg(target_os = "macos")]
pub fn focus(h: &Holder) -> bool {
    // Tests (and GODTERM_NO_OPEN) never drive the user's terminal.
    if cfg!(test) || crate::config::env_var("NO_OPEN").is_some() {
        return false;
    }
    let Some(tty) = h.tty.as_deref() else {
        return false;
    };
    let script = match h.terminal.as_deref() {
        Some("iTerm") => format!(
            r#"tell application "iTerm"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if tty of s is "{tty}" then
          select w
          select t
          activate
          return "ok"
        end if
      end repeat
    end repeat
  end repeat
end tell"#
        ),
        _ => format!(
            r#"tell application "Terminal"
  repeat with w in windows
    repeat with t in tabs of w
      if tty of t is "{tty}" then
        set selected of t to true
        set index of w to 1
        activate
        return "ok"
      end if
    end repeat
  end repeat
end tell"#
        ),
    };
    crate::window::osascript(false, &script, std::time::Duration::from_secs(3))
        .is_some_and(|o| o.trim() == "ok")
}

#[cfg(not(target_os = "macos"))]
pub fn focus(_h: &Holder) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_is_refused_and_names_the_holder() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("godterm-lock-{}", std::process::id()));
        crate::config::testing::set_home(&home);
        assert!(running().is_none());
        let first = acquire().expect("first");
        // flock is per open file: a second open in this process conflicts
        // just like another process would.
        let second = acquire();
        assert_eq!(second.err().map(|h| h.pid), Some(std::process::id()));
        assert_eq!(running().map(|h| h.pid), Some(std::process::id()));
        drop(first);
        // A process another test forks at this moment holds a copy of the
        // descriptor until it execs (close on exec): give it a moment.
        let t0 = std::time::Instant::now();
        while running().is_some() && t0.elapsed() < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(running().is_none());
        assert!(acquire().is_ok());
        let _ = std::fs::remove_dir_all(&home);
    }
}
