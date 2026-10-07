//! The New Tab dialog: where the tab starts (a new dated folder under a base
//! path, a recent folder, any typed path) or which session it resumes.

use std::path::{Path, PathBuf};

use crate::config::{expand_tilde, home_dir};
use crate::sessions::{snippet, SessionInfo};

#[derive(Debug, Clone, PartialEq)]
pub enum PickAction {
    NewIn(PathBuf),
    Resume { id: String, cwd: PathBuf },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    Recent,
    Resume,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickItem {
    pub kind: ItemKind,
    pub label: String,
    pub detail: String,
    pub action: PickAction,
}

/// A recently used folder, as stored in state.json.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecentDir {
    pub path: String,
    /// Account name it was used with.
    #[serde(default)]
    pub account: String,
    /// Unix seconds.
    #[serde(default)]
    pub last_used: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecentRow {
    pub path: PathBuf,
    pub last_used: i64,
    pub sessions: usize,
}

/// What the dialog is editing.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Typing the new folder's name.
    Name,
    /// Editing the base path; Enter saves it as the default.
    Base(String),
    /// Typing any existing path (Tab completes).
    OpenExisting(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    New,
    Exists,
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    pub slot: usize,
    pub account: usize,
    pub base: PathBuf,
    pub name: String,
    pub mode: Mode,
    /// Selected list row; None means the name field.
    pub sel: Option<usize>,
    pub create_folder: bool,
    pub git_init: bool,
    pub all_accounts: bool,
    /// The name is still the generated default: typing replaces it.
    pub pristine: bool,
    pub recents: Vec<RecentRow>,
    resumes: Vec<PickItem>,
}

pub fn tilde(p: &str) -> String {
    let home = home_dir().to_string_lossy().into_owned();
    match p.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => p.to_string(),
    }
}

/// The default folder name: `pattern` (strftime) formatted for `now`, with
/// the lowest `-N` suffix (from 1) that does not exist under `base`.
pub fn dated_name(pattern: &str, base: &Path, now: chrono::DateTime<chrono::Local>) -> String {
    let stem = now
        .format(if pattern.trim().is_empty() {
            "%Y-%m-%d"
        } else {
            pattern
        })
        .to_string();
    let stem: String = stem
        .chars()
        .filter(|c| *c != '/' && !c.is_control())
        .collect();
    for n in 1..10_000 {
        let name = format!("{stem}-{n}");
        if !base.join(&name).exists() {
            return name;
        }
    }
    stem
}

/// Check a folder name typed by the user.
pub fn validate_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("type a folder name".into());
    }
    if n.starts_with('/') || n.starts_with('~') || std::path::Path::new(n).is_absolute() {
        return Err("a name, not a path (use Open existing for paths)".into());
    }
    if n.split('/').any(|p| p == ".." || p == ".") {
        return Err("no . or .. parts".into());
    }
    if n.chars().any(|c| c.is_control() || c == ':') {
        return Err("no control characters or ':'".into());
    }
    Ok(())
}

/// Whether a folder can be created in `dir` (it exists and is writable).
pub fn writable(dir: &Path) -> bool {
    crate::platform::writable(dir)
}

/// Complete a typed path to the longest common prefix of matching folders.
pub fn complete_dir(input: &str) -> String {
    let expanded = expand_tilde(input);
    let s = expanded.to_string_lossy().into_owned();
    let (dir, prefix) = match s.rfind('/') {
        Some(i) => (s[..=i].to_string(), s[i + 1..].to_string()),
        None => return input.to_string(),
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return input.to_string();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix) && (prefix.starts_with('.') || !n.starts_with('.')))
        .collect();
    names.sort();
    let Some(first) = names.first().cloned() else {
        return input.to_string();
    };
    let mut common = first.clone();
    for n in &names[1..] {
        while !n.starts_with(&common) {
            common.pop();
        }
    }
    let mut out = format!("{dir}{common}");
    if names.len() == 1 {
        out.push('/');
    }
    // Keep the ~ the user typed.
    if input.starts_with('~') {
        tilde(&out)
    } else {
        out
    }
}

/// Recent folders for the dialog, newest first: recorded recents (of this
/// account unless `all`), then session folders not listed yet, at most
/// `limit`, each with how many sessions ran there.
pub fn recent_rows(
    recents: &[RecentDir],
    account: &str,
    all: bool,
    sessions: &[SessionInfo],
    limit: usize,
) -> Vec<RecentRow> {
    let mut rows: Vec<RecentRow> = vec![];
    let mut list: Vec<&RecentDir> = recents
        .iter()
        .filter(|r| all || r.account == account)
        .collect();
    list.sort_by_key(|r| std::cmp::Reverse(r.last_used));
    let count = |p: &str| sessions.iter().filter(|s| s.cwd == p).count();
    for r in list {
        let p = PathBuf::from(&r.path);
        if rows.iter().any(|x| x.path == p) {
            continue;
        }
        rows.push(RecentRow {
            path: p,
            last_used: r.last_used,
            sessions: count(&r.path),
        });
    }
    for s in sessions {
        let p = PathBuf::from(&s.cwd);
        if rows.iter().any(|x| x.path == p) || !p.is_dir() {
            continue;
        }
        let t = s
            .modified
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        rows.push(RecentRow {
            path: p,
            last_used: t,
            sessions: count(&s.cwd),
        });
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.last_used));
    rows.truncate(limit);
    rows
}

/// Insert or refresh a recent folder, keeping at most `limit` per account.
pub fn remember(recents: &mut Vec<RecentDir>, path: &Path, account: &str, now: i64, limit: usize) {
    let p = path.to_string_lossy().into_owned();
    recents.retain(|r| !(r.path == p && r.account == account));
    recents.insert(
        0,
        RecentDir {
            path: p,
            account: account.into(),
            last_used: now,
        },
    );
    let mut seen = 0;
    recents.retain(|r| {
        if r.account != account {
            return true;
        }
        seen += 1;
        seen <= limit.max(1)
    });
}

impl Picker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        slot: usize,
        account: usize,
        base: PathBuf,
        pattern: &str,
        create_folder: bool,
        recents: Vec<RecentRow>,
        sessions: &[SessionInfo],
    ) -> Picker {
        let name = dated_name(pattern, &base, chrono::Local::now());
        let mut p = Picker {
            slot,
            account,
            base,
            name,
            mode: Mode::Name,
            sel: None,
            create_folder,
            git_init: false,
            all_accounts: false,
            pristine: true,
            recents,
            resumes: vec![],
        };
        p.set_sessions(sessions);
        p
    }

    /// Refresh the Resume section once sessions finished loading.
    pub fn set_sessions(&mut self, sessions: &[SessionInfo]) {
        self.resumes = sessions
            .iter()
            .take(15)
            .map(|s| PickItem {
                kind: ItemKind::Resume,
                label: snippet(&s.summary(), 60),
                detail: tilde(&s.cwd),
                action: PickAction::Resume {
                    id: s.id.clone(),
                    cwd: PathBuf::from(&s.cwd),
                },
            })
            .collect();
    }

    /// Recent folders then sessions to resume.
    pub fn items(&self) -> Vec<PickItem> {
        let now = chrono::Utc::now().timestamp();
        let mut v: Vec<PickItem> = self
            .recents
            .iter()
            .map(|r| PickItem {
                kind: ItemKind::Recent,
                label: tilde(&r.path.to_string_lossy()),
                detail: format!(
                    "{}{}",
                    ago(now - r.last_used),
                    if r.sessions > 0 {
                        format!(" · {} session(s)", r.sessions)
                    } else {
                        String::new()
                    }
                ),
                action: PickAction::NewIn(r.path.clone()),
            })
            .collect();
        v.extend(self.resumes.iter().cloned());
        v
    }

    /// The folder Create & open would use.
    pub fn target(&self) -> PathBuf {
        self.base.join(self.name.trim())
    }

    /// What Create & open would do with the folder.
    pub fn status(&self) -> Status {
        if let Err(e) = validate_name(&self.name) {
            return Status::Invalid(e);
        }
        let t = self.target();
        if t.is_dir() {
            return Status::Exists;
        }
        if t.exists() {
            return Status::Invalid("a file with that name exists".into());
        }
        if !self.create_folder {
            return Status::Invalid("does not exist; tick Create folder".into());
        }
        // The nearest existing ancestor must be writable.
        let mut anc = t.parent().map(Path::to_path_buf);
        while let Some(a) = anc.clone() {
            if a.is_dir() {
                return if writable(&a) {
                    Status::New
                } else {
                    Status::Invalid(format!(
                        "no permission to write in {}",
                        tilde(&a.to_string_lossy())
                    ))
                };
            }
            anc = a.parent().map(Path::to_path_buf);
        }
        Status::Invalid("base folder does not exist".into())
    }

    /// The text being edited right now.
    pub fn input_mut(&mut self) -> &mut String {
        match &mut self.mode {
            Mode::Name => &mut self.name,
            Mode::Base(b) | Mode::OpenExisting(b) => b,
        }
    }

    pub fn type_char(&mut self, c: char) {
        if self.sel.is_some() && self.mode == Mode::Name {
            self.sel = None;
        }
        // The first key replaces the generated name, like selected text.
        if self.mode == Mode::Name && self.pristine {
            self.name.clear();
        }
        self.pristine = false;
        self.input_mut().push(c);
    }

    pub fn backspace(&mut self) {
        if self.mode == Mode::Name {
            self.pristine = false;
        }
        self.input_mut().pop();
    }

    pub fn move_sel(&mut self, d: isize) {
        let n = self.items().len() as isize;
        if n == 0 {
            self.sel = None;
            return;
        }
        // -1 is the name field.
        let cur = self.sel.map(|s| s as isize).unwrap_or(-1);
        let next = (cur + d).clamp(-1, n - 1);
        self.sel = (next >= 0).then_some(next as usize);
    }

    pub fn chosen(&self) -> Option<PickAction> {
        self.sel
            .and_then(|i| self.items().get(i).map(|x| x.action.clone()))
    }

    pub fn remove_recent(&mut self, i: usize) -> Option<PathBuf> {
        (i < self.recents.len()).then(|| self.recents.remove(i).path)
    }
}

fn ago(secs: i64) -> String {
    match secs.max(0) {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("godterm-newtab-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dated_names_skip_existing() {
        let base = tmp("dated");
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-06T10:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        let day = now.format("%Y-%m-%d").to_string();
        assert_eq!(dated_name("%Y-%m-%d", &base, now), format!("{day}-1"));
        std::fs::create_dir(base.join(format!("{day}-1"))).unwrap();
        std::fs::create_dir(base.join(format!("{day}-2"))).unwrap();
        assert_eq!(dated_name("%Y-%m-%d", &base, now), format!("{day}-3"));
        assert_eq!(
            dated_name("work-%Y", &base, now),
            format!("work-{}-1", now.format("%Y"))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn preview_status_and_validation() {
        let base = tmp("status");
        let mut p = Picker::new(0, 0, base.clone(), "%Y-%m-%d", true, vec![], &[]);
        assert_eq!(p.target(), base.join(&p.name));
        assert_eq!(p.status(), Status::New);
        std::fs::create_dir(p.target()).unwrap();
        assert_eq!(p.status(), Status::Exists);
        p.name = "../x".into();
        assert!(matches!(p.status(), Status::Invalid(_)));
        p.name = "/abs".into();
        assert!(matches!(p.status(), Status::Invalid(_)));
        p.name = "fresh".into();
        p.create_folder = false;
        assert!(matches!(p.status(), Status::Invalid(m) if m.contains("Create folder")));
        p.create_folder = true;
        p.name = "nested/deeper".into();
        assert_eq!(p.status(), Status::New);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn recents_order_limit_and_sessions() {
        let mut r = vec![];
        remember(&mut r, Path::new("/a"), "work", 10, 3);
        remember(&mut r, Path::new("/b"), "work", 20, 3);
        remember(&mut r, Path::new("/c"), "home", 30, 3);
        remember(&mut r, Path::new("/a"), "work", 40, 3);
        remember(&mut r, Path::new("/d"), "work", 50, 3);
        remember(&mut r, Path::new("/e"), "work", 60, 3);
        let work: Vec<&str> = r
            .iter()
            .filter(|x| x.account == "work")
            .map(|x| x.path.as_str())
            .collect();
        assert_eq!(work, vec!["/e", "/d", "/a"], "newest first, 3 per account");
        let sessions = vec![SessionInfo {
            cwd: "/a".into(),
            id: "s".into(),
            messages: 2,
            ..Default::default()
        }];
        let rows = recent_rows(&r, "work", false, &sessions, 10);
        assert_eq!(
            rows.iter()
                .map(|x| x.path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["/e", "/d", "/a"]
        );
        assert_eq!(rows[2].sessions, 1);
        let all = recent_rows(&r, "work", true, &[], 2);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].path, PathBuf::from("/e"));
    }

    #[test]
    fn completes_dirs() {
        let base = tmp("complete");
        std::fs::create_dir(base.join("alpha-one")).unwrap();
        std::fs::create_dir(base.join("alpha-two")).unwrap();
        std::fs::create_dir(base.join("beta")).unwrap();
        let b = base.to_string_lossy();
        assert_eq!(complete_dir(&format!("{b}/al")), format!("{b}/alpha-"));
        assert_eq!(complete_dir(&format!("{b}/be")), format!("{b}/beta/"));
        assert_eq!(complete_dir(&format!("{b}/zz")), format!("{b}/zz"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn selection_moves_between_field_and_list() {
        let rows = vec![RecentRow {
            path: "/a".into(),
            last_used: 0,
            sessions: 0,
        }];
        let mut p = Picker::new(0, 0, "/tmp".into(), "%Y", true, rows, &[]);
        assert_eq!(p.sel, None);
        p.move_sel(1);
        assert_eq!(p.chosen(), Some(PickAction::NewIn("/a".into())));
        p.move_sel(-1);
        assert_eq!(p.sel, None);
        p.type_char('x');
        assert_eq!(p.name, "x", "typing replaces the default name");
        p.type_char('y');
        assert_eq!(p.name, "xy");
        p.mode = Mode::OpenExisting(String::new());
        p.type_char('/');
        assert_eq!(p.mode, Mode::OpenExisting("/".into()));
        assert_eq!(p.remove_recent(0), Some(PathBuf::from("/a")));
    }
}
