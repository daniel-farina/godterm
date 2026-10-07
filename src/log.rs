//! Append only log at `~/.godterm/godterm.log` (rotated at 2 MB).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn path() -> PathBuf {
    crate::config::app_home().join("godterm.log")
}

fn write(level: &str, msg: &str) {
    let p = path();
    if let Some(d) = p.parent() {
        let _ = fs::create_dir_all(d);
    }
    if fs::metadata(&p)
        .map(|m| m.len() > 2_000_000)
        .unwrap_or(false)
    {
        let _ = fs::rename(&p, p.with_extension("log.1"));
    }
    if let Ok(mut f) = {
        use crate::platform::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&p)
    } {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(f, "{ts} {level} {msg}");
    }
}

pub fn info(msg: &str) {
    write("INFO", msg);
}

pub fn error(msg: &str) {
    write("ERROR", msg);
}
