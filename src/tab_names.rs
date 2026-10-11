//! How a tab is named when GodTerm tells the user about it ("the hyper
//! demo finished", "the trading room bot needs approval"): never only an
//! account and a tab number. In order: the tab's own name when it means
//! something (not the user's name, not the home folder, not a generic
//! folder), the saved project or session description or alias
//! (project_memory), the project or subproject name from the project
//! index, the session's own title, then a few words of the tab's current
//! task or first prompt. "on Account N" is added only when two tabs would
//! otherwise sound the same.

use std::path::Path;

use crate::app::App;
use crate::project_memory::Store;

/// Folder names that say nothing about the work in them.
const GENERIC: &[&str] = &[
    "~",
    "tmp",
    "temp",
    "src",
    "code",
    "dev",
    "projects",
    "project",
    "repos",
    "work",
    "desktop",
    "downloads",
    "documents",
    "home",
    "users",
    "newtabs",
    "new tab",
    "claude",
    "untitled",
    "scratch",
    "test",
    "tests",
];

/// Whether a tab name (a custom name or its folder's) tells which work it
/// is.
pub fn meaningful(name: &str, user: Option<&str>) -> bool {
    let n = name.trim().to_lowercase();
    if n.is_empty() || n.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return false;
    }
    if user.is_some_and(|u| !u.is_empty() && n == u.to_lowercase()) {
        return false;
    }
    !GENERIC.contains(&n.as_str())
}

/// A few words of a longer text ("Trading room bot that posts..." ->
/// "the trading room bot that posts").
fn few_words(text: &str, n: usize) -> String {
    let words: Vec<&str> = text
        .split(['.', '\n', ';', ':'])
        .next()
        .unwrap_or("")
        .split_whitespace()
        .take(n)
        .collect();
    words
        .join(" ")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

/// The user's name as a folder sees it: the home folder's own name.
fn user_name() -> Option<String> {
    crate::config::home_dir()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

impl App {
    /// The name GodTerm says for tab `uid`, with "on Account N" when
    /// another tab would sound the same.
    pub fn spoken_tab_name(&self, uid: u64) -> String {
        let store = crate::project_memory::load();
        let Some(base) = self.tab_base_name(&store, uid) else {
            return "a tab that is closed now".into();
        };
        let clash = self.all_tabs().into_iter().any(|(s, t)| {
            let other = self.panes[s].tabs[t].uid;
            other != uid
                && self
                    .tab_base_name(&store, other)
                    .is_some_and(|o| o.eq_ignore_ascii_case(&base))
        });
        let account = self
            .find_tab(uid)
            .and_then(|(s, _)| self.panes[s].account)
            .map(|a| self.cfg.accounts[a].display().to_string());
        match account {
            Some(a) if clash => format!("{base} on {a}"),
            _ => base,
        }
    }

    /// The name before any clash is resolved; None when the tab is gone.
    pub(crate) fn tab_base_name(&self, store: &Store, uid: u64) -> Option<String> {
        let (s, t) = self.find_tab(uid)?;
        let tab = &self.panes[s].tabs[t];
        let user = user_name();
        let at_home = tab.cwd == crate::config::home_dir();
        // 1. The tab's own name (custom, else its folder's).
        let own = tab.name();
        let custom = tab
            .custom_name
            .as_ref()
            .is_some_and(|n| !n.trim().is_empty());
        if meaningful(&own, user.as_deref()) && (custom || !at_home) {
            return Some(own);
        }
        // 2. What was saved about the project or the session.
        let memo = store
            .get("project", &crate::project_memory::folder_key(&tab.cwd))
            .filter(|_| !at_home)
            .or_else(|| {
                tab.session_id
                    .as_ref()
                    .and_then(|sid| store.get("session", sid))
            });
        if let Some(m) = memo {
            if let Some(a) = m.aliases.iter().find(|a| !a.trim().is_empty()) {
                return Some(format!("the {}", a.trim()));
            }
            let d = few_words(&m.description, 5);
            if !d.is_empty() {
                return Some(format!("the {}", lower_first(&d)));
            }
        }
        // 3. The project or subproject it is in.
        if !at_home {
            if let Some(p) = self.project_name_of(&tab.cwd, user.as_deref()) {
                return Some(p);
            }
        }
        // 4. The session's own title.
        if let Some(sid) = &tab.session_id {
            let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = ix.rows.iter().find(|r| &r.info.id == sid) {
                if let Some(n) = r.info.name.as_ref().or(r.info.title.as_ref()) {
                    let n = few_words(n, 6);
                    if !n.is_empty() {
                        return Some(format!("the {n} session"));
                    }
                }
                if let Some(p) = &r.info.first_prompt {
                    return Some(task_words(p));
                }
            }
        }
        // 5. What it was asked to do.
        let task = self
            .deliveries
            .iter()
            .rev()
            .find(|d| d.uid == uid)
            .map(|d| d.text.clone())
            .or_else(|| {
                self.assistant
                    .follow_ups
                    .iter()
                    .rev()
                    .find(|f| f.uid == uid)
                    .map(|f| f.question.clone())
            });
        if let Some(task) = task {
            return Some(task_words(&task));
        }
        Some(if tab.session_id.is_none() {
            "a fresh tab".to_string()
        } else {
            format!("the tab in {}", crate::config::tilde(&tab.cwd))
        })
    }

    /// The project index's name for a folder: the subproject's manifest
    /// name, else the repo or root folder's name.
    fn project_name_of(&self, cwd: &Path, user: Option<&str>) -> Option<String> {
        let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
        let (root, map) = ix
            .roots
            .iter()
            .filter(|(r, _)| cwd.starts_with(r))
            .max_by_key(|(r, _)| r.components().count())?;
        // The index keeps "/" separators; Windows folders use "\\".
        let rel = cwd
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        let sub = map
            .entries
            .iter()
            .filter(|e| {
                !e.rel.is_empty() && (rel == e.rel || rel.starts_with(&format!("{}/", e.rel)))
            })
            .max_by_key(|e| e.rel.len());
        if let Some(e) = sub {
            let n = e
                .name
                .as_deref()
                .map(|n| n.rsplit('/').next().unwrap_or(n).to_string())
                .unwrap_or_else(|| e.rel.rsplit('/').next().unwrap_or(&e.rel).to_string());
            if meaningful(&n, user) {
                return Some(n);
            }
        }
        let n = map
            .remote
            .clone()
            .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))?;
        meaningful(&n, user).then_some(n)
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        // Keep acronyms ("API server") as they are.
        Some(f) if c.clone().next().is_some_and(|n| n.is_lowercase()) => {
            f.to_lowercase().collect::<String>() + c.as_str()
        }
        _ => s.to_string(),
    }
}

/// "the tab working on 'fix the login bug'".
fn task_words(task: &str) -> String {
    let w = few_words(task, 5);
    if w.is_empty() {
        "a tab".into()
    } else {
        format!("the tab working on '{w}'")
    }
}
