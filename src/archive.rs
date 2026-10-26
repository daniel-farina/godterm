//! Archived conversations: a tab put away. Its process (and what it owns:
//! loops, a background run holding its session) is stopped and its pane tab
//! closed, but everything needed to resume it exactly is kept in
//! `~/.godterm/archive.json` (owner only, written atomically): provider,
//! account, folder, session id, name, color, pin, group, model, loops and
//! when it was last active and archived. Only this entry is ever deleted
//! by "Delete from archive"; the session's own files are never touched.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::closed::{ClosedTab, SavedLoop};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Archived {
    /// Short id for the assistant ("a3f9c1d2").
    pub id: String,
    /// claude or grok.
    pub provider: String,
    /// Account name (config `name`).
    pub account: String,
    pub cwd: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// The tab's own name, if set.
    #[serde(default)]
    pub name: Option<String>,
    /// What the tab was called in the list.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<SavedLoop>,
    /// The tab's Chrome profile folder (its Browser binding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    /// Unix seconds.
    pub last_activity: i64,
    pub archived_at: i64,
    /// user or assistant.
    #[serde(default)]
    pub by: String,
}

impl Archived {
    /// The record `reopen_one` resumes from.
    pub fn to_closed(&self) -> ClosedTab {
        ClosedTab {
            account: self.account.clone(),
            cwd: self.cwd.clone(),
            session_id: self.session_id.clone(),
            name: self.name.clone(),
            accent: self.accent.clone(),
            loops: self.loops.clone(),
            group: 0,
            closed_at: self.archived_at,
            label: self.label.clone(),
            tab_group: self.tab_group.clone(),
            pinned: self.pinned,
            browser: self.browser.clone(),
        }
    }

    /// What a search looks at.
    pub fn search_fields(&self) -> Vec<String> {
        vec![
            self.label.clone(),
            self.name.clone().unwrap_or_default(),
            crate::paths::tilde(std::path::Path::new(&self.cwd)),
            self.account.clone(),
            self.provider.clone(),
            "archived".into(),
        ]
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub entries: Vec<Archived>,
}

pub fn path() -> PathBuf {
    crate::config::app_home().join("archive.json")
}

impl Store {
    /// The archive; a damaged file is moved aside (recover.rs), not
    /// overwritten by the next save.
    pub fn load() -> Store {
        crate::recover::load_json(&path()).unwrap_or_default()
    }

    /// Write atomically (a temp file, then rename), owner only.
    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        crate::config::write_private(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, p)
    }

    /// Add an entry; the same session archived again replaces the old one.
    pub fn add(&mut self, mut a: Archived) -> String {
        if let Some(sid) = &a.session_id {
            self.entries
                .retain(|e| e.session_id.as_ref() != Some(sid) || e.account != a.account);
        }
        if a.id.is_empty() || self.entries.iter().any(|e| e.id == a.id) {
            a.id = crate::session_ops::new_uuid()[..8].to_string();
        }
        let id = a.id.clone();
        self.entries.push(a);
        id
    }

    pub fn find(&self, id: &str) -> Option<&Archived> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn remove(&mut self, id: &str) -> Option<Archived> {
        let i = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(i))
    }

    /// Newest archived first.
    pub fn newest_first(&self) -> Vec<&Archived> {
        let mut v: Vec<&Archived> = self.entries.iter().collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.archived_at));
        v
    }
}

/// Idle for at least `hours` at `now` (both Unix seconds).
pub fn idle_at_least(last_activity: i64, now: i64, hours: f64) -> bool {
    (now - last_activity) as f64 >= hours * 3600.0
}

/// Text typed in a tab's input box and not sent, read from its screen
/// (Claude's `> text` / `│ > text` line near the bottom; the greyed
/// `Try "..."` hint is not text).
pub fn unsent_input(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let k = lines.len().saturating_sub(8);
    for l in lines[k..].iter().rev() {
        let t = l.trim().trim_start_matches(['│', '|']).trim_start();
        let Some(rest) = t.strip_prefix('>').or_else(|| t.strip_prefix('❯')) else {
            continue;
        };
        let rest = rest.trim_end_matches(['│', '|']).trim();
        if rest.is_empty() || rest.starts_with("Try \"") {
            return None;
        }
        return Some(rest.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::testing::LOCK as ENV_LOCK;

    fn entry(id: &str, sid: Option<&str>) -> Archived {
        Archived {
            id: id.into(),
            provider: "claude".into(),
            account: "work".into(),
            cwd: "/w/api".into(),
            session_id: sid.map(str::to_string),
            name: Some("billing".into()),
            label: "billing".into(),
            accent: Some("#7a9".into()),
            pinned: true,
            tab_group: Some("api".into()),
            model: Some("opus".into()),
            loops: vec![SavedLoop {
                id: "j1".into(),
                cron: Some("*/5 * * * *".into()),
                prompt: "check".into(),
                recurring: true,
                wakeup: false,
            }],
            browser: None,
            last_activity: 1000,
            archived_at: 2000,
            by: "user".into(),
        }
    }

    #[test]
    fn store_round_trips_atomically() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("godterm-archive-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        crate::config::testing::set_home(&home);
        assert!(Store::load().entries.is_empty());
        let mut s = Store::default();
        s.add(entry("a1", Some("sid-1")));
        s.add(entry("a2", None));
        s.save().unwrap();
        assert!(!path().with_extension("json.tmp").exists());
        let back = Store::load();
        assert_eq!(back, s);
        assert_eq!(back.find("a1").unwrap().model.as_deref(), Some("opus"));
        // Garbage on disk reads as empty, never panics.
        std::fs::write(path(), "{not json").unwrap();
        assert!(Store::load().entries.is_empty());
    }

    #[test]
    fn same_session_replaces_and_ids_stay_unique() {
        let mut s = Store::default();
        s.add(entry("a1", Some("sid-1")));
        let id = s.add(entry("a1", Some("sid-1")));
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].id, id);
        let other = s.add(entry("a1", Some("sid-2")));
        assert_ne!(other, id, "a taken id is replaced");
        assert_eq!(s.entries.len(), 2);
        assert!(s.remove(&other).is_some());
        assert!(s.remove(&other).is_none());
    }

    #[test]
    fn closed_record_keeps_everything_to_resume() {
        let c = entry("a1", Some("sid-1")).to_closed();
        assert_eq!(c.session_id.as_deref(), Some("sid-1"));
        assert_eq!(c.cwd, "/w/api");
        assert_eq!(c.account, "work");
        assert_eq!(c.loops.len(), 1);
        assert!(c.pinned);
        assert_eq!(c.tab_group.as_deref(), Some("api"));
    }

    #[test]
    fn idle_threshold_with_an_injected_clock() {
        let now = 1_000_000;
        let day = 86400;
        assert!(idle_at_least(now - 3 * day, now, 48.0));
        assert!(idle_at_least(now - 2 * day, now, 48.0), "exactly two days");
        assert!(!idle_at_least(now - day, now, 48.0));
    }

    #[test]
    fn unsent_text_is_read_from_the_input_box() {
        let empty = "output\n\n╭────────╮\n│ >      │\n╰────────╯\n  ? for shortcuts";
        assert_eq!(unsent_input(empty), None);
        let hint = "╭────╮\n│ > Try \"fix the tests\" │\n╰────╯";
        assert_eq!(unsent_input(hint), None);
        let typed = "╭────────────╮\n│ > half a thought │\n╰────────────╯\n  ? for shortcuts";
        assert_eq!(unsent_input(typed).as_deref(), Some("half a thought"));
        assert_eq!(unsent_input("no box here"), None);
    }
}
