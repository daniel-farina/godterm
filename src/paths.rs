//! The working directory shown for each tab: its live cwd (the claude
//! process's, or the latest `cwd` in its transcript), formatted to fit a
//! pane header next to the label and the email, and the actions on it
//! (copy, Finder, a terminal there).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::app::App;

/// A process's current directory (macOS: proc_pidinfo VNODEPATHINFO).
#[cfg(target_os = "macos")]
pub fn cwd_of_pid(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    let raw = &info.pvi_cdir.vip_path;
    let bytes: Vec<u8> = raw
        .iter()
        .flat_map(|row| row.iter().map(|c| *c as u8))
        .collect();
    let s = CStr::from_bytes_until_nul(&bytes).ok()?.to_str().ok()?;
    (!s.is_empty()).then(|| PathBuf::from(s))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn cwd_of_pid(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Windows: our own folder; another process's is not readable without
/// its PEB (callers fall back to the tab's launch folder).
#[cfg(windows)]
pub fn cwd_of_pid(pid: u32) -> Option<PathBuf> {
    (pid == std::process::id())
        .then(|| std::env::current_dir().ok())
        .flatten()
}

/// The newest `"cwd"` recorded in a transcript (where claude's shell is).
pub fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(256 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let i = text.rfind("\"cwd\":\"")?;
    let rest = &text[i + 7..];
    let end = rest.find('"')?;
    let p = PathBuf::from(rest[..end].replace("\\/", "/"));
    p.is_dir().then_some(p)
}

/// "~/code/api" for paths under home.
pub fn tilde(p: &Path) -> String {
    crate::config::tilde(p)
}

/// Cut `s` to `max` chars in the middle: "godterm-fe…auth-login".
pub fn middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".chars().take(max).collect();
    }
    let room = max - 1;
    let head = room.div_ceil(2);
    let tail = room - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// Shorten a path to `max` chars by dropping middle components, keeping
/// the first one and the last one whole: "~/code/…/calculator". When even
/// "…/last" is too long, the last component is cut in the middle.
pub fn shorten_path(path: &str, max: usize) -> String {
    let n = path.chars().count();
    if n <= max {
        return path.to_string();
    }
    // Split on the separator the path uses (\\ for Windows folders).
    let sep = if path.contains('\\') && !path.contains('/') {
        '\\'
    } else {
        '/'
    };
    let parts: Vec<&str> = path.split(sep).collect();
    let last = parts.last().copied().unwrap_or("");
    // Keep a head of 1..k components and the last one.
    for keep in (1..parts.len().saturating_sub(1)).rev() {
        let cand = format!("{}{sep}…{sep}{last}", parts[..keep].join(&sep.to_string()));
        if cand.chars().count() <= max {
            return cand;
        }
    }
    let cand = format!("…{sep}{last}");
    if cand.chars().count() <= max {
        return cand;
    }
    if last.chars().count() <= max {
        return last.to_string();
    }
    crate::app_privacy::middle_truncate(last, max)
}

/// What fits after the label in `room` columns: (path, email), each with
/// its " · " separator counted. The label is never touched; the path's
/// last component comes next; the email goes (cut, then dropped) before
/// the path shrinks below its last component.
pub fn header_fit(
    path: Option<&str>,
    email: Option<&str>,
    room: usize,
) -> (Option<String>, Option<String>) {
    const SEP: usize = 3; // " · "
    let w = |s: &str| s.chars().count();
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        let e = email
            .and_then(|e| {
                (room > SEP + 3).then(|| crate::app_privacy::middle_truncate(e, room - SEP))
            })
            .filter(|e| !e.is_empty());
        return (None, e);
    };
    let last = path.rsplit('/').next().unwrap_or(path);
    let min_path = w(&format!("…/{last}")).min(w(path));
    if room < SEP + 3 {
        return (None, None);
    }
    // Path whole, plus as much email as fits (at least 10 chars of it).
    if let Some(e) = email {
        let left = room.saturating_sub(SEP + w(path));
        if left >= SEP + w(e) {
            return (Some(path.to_string()), Some(e.to_string()));
        }
        if left >= SEP + 10 {
            return (
                Some(path.to_string()),
                Some(crate::app_privacy::middle_truncate(e, left - SEP)),
            );
        }
        // Shorter path with 10 chars of email, if the path keeps its last part.
        let for_path = room.saturating_sub(SEP + SEP + 10);
        if for_path >= min_path {
            return (
                Some(shorten_path(path, for_path)),
                Some(crate::app_privacy::middle_truncate(
                    e,
                    room - SEP - SEP - w(&shorten_path(path, for_path)),
                )),
            );
        }
    }
    // No email: the path, shortened from the middle.
    (Some(shorten_path(path, room - SEP)), None)
}

const POLL: Duration = Duration::from_secs(2);

impl App {
    /// Refresh the live directory of each visible pane's shown tab, at
    /// most every 2 s per tab.
    pub fn refresh_paths(&mut self) {
        if !self.cfg.show_path {
            return;
        }
        for s in 0..self.panes.len() {
            if self.panes[s].hidden || self.pane_rects.get(s).is_none_or(|r| r.width == 0) {
                continue;
            }
            let t = self.panes[s].active;
            if self.panes[s].tabs[t]
                .cwd_polled
                .is_some_and(|p| p.elapsed() < POLL)
            {
                continue;
            }
            let from_transcript = self.transcript_path(s, t).and_then(|p| transcript_cwd(&p));
            let tab = &mut self.panes[s].tabs[t];
            tab.cwd_polled = Some(Instant::now());
            let live = from_transcript
                .or_else(|| tab.pid().and_then(cwd_of_pid))
                .filter(|p| p.is_dir());
            let same =
                |a: &Path, b: &Path| a == b || a.canonicalize().ok() == b.canonicalize().ok();
            tab.live_cwd = live.filter(|p| !same(p, &tab.cwd));
        }
    }

    /// The folder shown for a tab: its live cwd, else where it started.
    pub fn tab_dir(&self, s: usize, t: usize) -> PathBuf {
        let tab = &self.panes[s].tabs[t];
        tab.live_cwd.clone().unwrap_or_else(|| tab.cwd.clone())
    }

    pub fn copy_path(&mut self, s: usize) {
        let d = self.tab_dir(s, self.panes[s].active);
        let text = d.to_string_lossy().into_owned();
        // (Demo mode leaves the user's clipboard alone.)
        let quiet = cfg!(test) || crate::demo::active();
        if !quiet {
            // (In the background: the screen never waits on pbcopy.)
            crate::procs::copy_to_clipboard(text);
        }
        self.flash(format!("Copied {}", tilde(&d)));
    }

    pub fn open_in_finder(&mut self, s: usize) {
        let d = self.tab_dir(s, self.panes[s].active);
        if !cfg!(test) && !crate::demo::active() {
            let _ = std::process::Command::new("open").arg(&d).spawn();
        }
        self.flash(format!("Opened {} in Finder", tilde(&d)));
    }

    /// A new iTerm tab (or Terminal window) in the tab's folder.
    pub fn terminal_here(&mut self, s: usize) {
        let d = self.tab_dir(s, self.panes[s].active);
        if !cfg!(test) && !crate::demo::active() {
            let iterm = Path::new("/Applications/iTerm.app").is_dir();
            let app = if iterm { "iTerm" } else { "Terminal" };
            let _ = std::process::Command::new("open")
                .args(["-a", app])
                .arg(&d)
                .spawn();
        }
        self.flash(format!("Terminal at {}", tilde(&d)));
    }
}

#[cfg(test)]
mod sep_tests {
    #[test]
    fn windows_folders_shorten_on_backslashes() {
        let p = r"~\code\clients\acme\godterm-header-test-folder-x";
        let s = super::shorten_path(p, 40);
        assert_eq!(s, r"~\code\…\godterm-header-test-folder-x");
        assert_eq!(super::shorten_path("~/a/b/c/dddd", 10), "~/a/…/dddd");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_from_the_middle() {
        assert_eq!(shorten_path("~/code/calculator", 40), "~/code/calculator");
        assert_eq!(
            shorten_path("~/code/clients/acme/calculator", 22),
            "~/code/…/calculator"
        );
        assert_eq!(
            shorten_path("~/code/clients/acme/calculator", 18),
            "~/…/calculator"
        );
        assert_eq!(
            shorten_path("~/code/clients/acme/calculator", 13),
            "…/calculator"
        );
        assert_eq!(
            shorten_path("~/code/clients/acme/calculator", 10),
            "calculator"
        );
        assert_eq!(
            shorten_path("~/x/a-very-long-project-name", 12)
                .chars()
                .count(),
            12
        );
    }

    #[test]
    fn label_then_path_then_email() {
        let p = Some("~/code/clients/calculator");
        let e = Some("someone@example.com");
        // Plenty of room: both whole.
        assert_eq!(
            header_fit(p, e, 80),
            (
                Some("~/code/clients/calculator".into()),
                Some("someone@example.com".into())
            )
        );
        // The email is cut first.
        let (pp, ee) = header_fit(p, e, 3 + 25 + 3 + 12);
        assert_eq!(pp.as_deref(), Some("~/code/clients/calculator"));
        assert_eq!(ee.unwrap().chars().count(), 12);
        // Then the path shrinks from the middle, the email kept at 10.
        let (pp, ee) = header_fit(p, e, 3 + 20 + 3 + 10);
        assert_eq!(pp.as_deref(), Some("~/code/…/calculator"));
        assert_eq!(ee.unwrap().chars().count(), 10 + 1);
        // Then the email goes before the last component does.
        let (pp, ee) = header_fit(p, e, 3 + 14);
        assert_eq!((pp.as_deref(), ee), (Some("~/…/calculator"), None));
        let (pp, ee) = header_fit(p, e, 3 + 10);
        assert_eq!((pp.as_deref(), ee), (Some("calculator"), None));
        // Privacy mode: no email, the path stays.
        assert_eq!(
            header_fit(p, None, 40),
            (Some("~/code/clients/calculator".into()), None)
        );
        assert_eq!(
            header_fit(None, e, 40),
            (None, Some("someone@example.com".into()))
        );
        assert_eq!(header_fit(p, e, 2), (None, None));
    }

    #[test]
    fn transcript_cwd_is_the_latest() {
        let d = std::env::temp_dir().join(format!("godterm-cwd-{}", std::process::id()));
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let t = d.join("t.jsonl");
        std::fs::write(
            &t,
            format!(
                "{}\n{}\n",
                serde_json::json!({"cwd": d, "type": "user"}),
                serde_json::json!({"cwd": sub, "type": "user"})
            ),
        )
        .unwrap();
        assert_eq!(transcript_cwd(&t), Some(sub));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn own_cwd_via_proc_pidinfo() {
        let me = cwd_of_pid(std::process::id()).expect("our own cwd");
        assert_eq!(
            me.canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }
}
