//! Damaged state files (state.json, jobs.json, config.toml cut short by a
//! crash or a full disk, or edited by hand into something that does not
//! parse): moved aside so GodTerm starts clean instead of failing, with a
//! one-line note in the log and on the status bar.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Move `path` aside as `<name>.corrupt-<stamp>` (never over an earlier
/// backup), log why and keep a note for the UI. None when it could not be
/// moved (it is then left where it is).
pub fn quarantine(path: &Path, why: &str) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut bak = path.with_file_name(format!("{name}.corrupt-{stamp}"));
    let mut n = 1;
    while bak.exists() {
        bak = path.with_file_name(format!("{name}.corrupt-{stamp}-{n}"));
        n += 1;
    }
    if let Err(e) = std::fs::rename(path, &bak) {
        crate::log::error(&format!(
            "{} is damaged ({why}) and could not be moved aside: {e}",
            path.display()
        ));
        return None;
    }
    let why = one_line(why);
    let note = format!(
        "{name} was damaged ({why}); saved as {} and started clean",
        bak.file_name().unwrap_or_default().to_string_lossy()
    );
    crate::log::error(&format!("{note} ({})", bak.display()));
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).push(note);
    Some(bak)
}

/// The notes so far (each once).
pub fn take_notes() -> Vec<String> {
    std::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()))
}

/// A parser's message on one short line.
fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 80 {
        let cut: String = s.chars().take(79).collect();
        format!("{cut}…")
    } else {
        s
    }
}

/// Read a JSON state file. Missing: None. Present but not valid (empty,
/// truncated, not JSON, the wrong shape): moved aside, None.
pub fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            let why = if bytes.iter().all(|b| b.is_ascii_whitespace()) {
                "empty".to_string()
            } else {
                e.to_string()
            };
            quarantine(path, &why);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("godterm-recover-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn backups(d: &Path, name: &str) -> Vec<PathBuf> {
        std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("{name}.corrupt-"))
            })
            .collect()
    }

    #[test]
    fn a_damaged_json_file_is_moved_aside_and_reads_as_missing() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = dir("json");
        let p = d.join("s.json");
        assert_eq!(load_json::<serde_json::Value>(&p), None, "missing");
        assert!(backups(&d, "s.json").is_empty(), "nothing to move");
        std::fs::write(&p, "{\"a\": 1}").unwrap();
        assert_eq!(load_json::<serde_json::Value>(&p).unwrap()["a"], 1);
        take_notes();
        for (bad, why) in [
            ("", "empty"),
            ("  \n", "empty"),
            ("{\"a\": [1, 2", "EOF"),
            ("\u{0}\u{0}\u{0}", "expected value"),
        ] {
            std::fs::write(&p, bad).unwrap();
            assert_eq!(load_json::<serde_json::Value>(&p), None, "{bad:?}");
            assert!(!p.exists(), "moved away");
            // (Other tests may add notes of their own files meanwhile.)
            let notes: Vec<String> = take_notes()
                .into_iter()
                .filter(|n| n.starts_with("s.json "))
                .collect();
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(notes[0].starts_with("s.json was damaged"), "{notes:?}");
            assert!(notes[0].contains(why), "{notes:?}");
            assert!(!notes[0].contains('\n'));
        }
        // Each damaged copy kept, none overwritten.
        let b = backups(&d, "s.json");
        assert_eq!(b.len(), 4, "{b:?}");
        assert!(b
            .iter()
            .any(|p| std::fs::read(p).unwrap() == b"{\"a\": [1, 2"));
        // The wrong shape counts as damaged too.
        #[derive(serde::Deserialize, Debug)]
        #[allow(dead_code)]
        struct S {
            a: u32,
        }
        std::fs::write(&p, "{\"a\": \"x\"}").unwrap();
        assert!(load_json::<S>(&p).is_none());
        assert!(!p.exists());
        take_notes();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_long_parser_message_is_one_short_line() {
        let s = one_line(&format!("line one\n  line two {}", "x".repeat(200)));
        assert!(!s.contains('\n'));
        assert_eq!(s.chars().count(), 80);
        assert!(s.starts_with("line one line two"));
    }
}
