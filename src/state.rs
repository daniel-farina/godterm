//! Open tabs persisted to `~/.godterm/state.json` so they reopen on restart.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::app_home;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TabState {
    pub cwd: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Custom tab name, if renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Accent color from the tab's menu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// Its id (t12): kept across restarts, so what the assistant remembers
    /// about t12 still means this tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u64>,
    /// Its group's name in the account's list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// When it was first opened (seconds since 1970).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SlotState {
    /// Account name (not index, so config edits do not shuffle slots).
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub tabs: Vec<TabState>,
    /// The pane's tab list is collapsed to a strip.
    #[serde(default)]
    pub sidebar_collapsed: bool,
    /// Per pane tab list position override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_pos: Option<crate::slot::TabPos>,
    /// Tab list width dragged by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_w: Option<u16>,
    /// Hidden from the layout (running in the background).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Tab groups (name, color, collapsed), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<crate::tab_groups::TabGroup>,
    /// The list's sort.
    #[serde(default, skip_serializing_if = "is_manual")]
    pub sort: crate::tab_groups::TabSort,
}

fn is_manual(s: &crate::tab_groups::TabSort) -> bool {
    *s == crate::tab_groups::TabSort::Manual
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionsView {
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub headless: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AppState {
    #[serde(default)]
    pub focus: usize,
    /// The next tab id: ids are never handed out twice, also across
    /// restarts (a remembered t12 is that tab, or no tab at all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_uid: Option<u64>,
    #[serde(default)]
    pub slots: Vec<SlotState>,
    /// The terminal window's last size and position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<crate::window::SavedWindow>,
    /// Recently used folders for the New Tab dialog.
    #[serde(default)]
    pub recents: Vec<crate::picker::RecentDir>,
    /// Runtime toggles (Ctrl-a e, Ctrl-a E) that override config.toml
    /// until the setting is changed there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_email: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<bool>,
    /// Memory saver toggled with the Mem button / Ctrl-a Z.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_saver: Option<bool>,
    /// Layout picked with Ctrl-a L (overrides `layout` until it changes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// Sessions view sort and filters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_view: Option<SessionsView>,
    /// Recently closed tabs, oldest first (Ctrl-a W reopens).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_tabs: Vec<crate::closed::ClosedTab>,
    /// Tab list width: "narrow", "normal" (default) or "wide".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_list_width: Option<String>,
    /// Dragged column and row sizes per shape.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub ratios: crate::layout::RatioMap,
}

impl AppState {
    pub fn path() -> PathBuf {
        app_home().join("state.json")
    }

    pub fn load() -> Option<AppState> {
        let text = fs::read_to_string(Self::path()).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        crate::config::write_private(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
        fs::rename(tmp, path)
    }
}

/// Claude's project dir name for a cwd: every non alphanumeric char is '-'.
pub fn encode_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The newest transcript for `cwd` written since `since`, skipping ids in
/// `taken`. Used to learn the session id of a tab we started fresh.
pub fn detect_session(
    config_dir: &Path,
    cwd: &Path,
    since: SystemTime,
    taken: &[String],
) -> Option<String> {
    let dir = config_dir.join("projects").join(encode_project_dir(cwd));
    let since = since - std::time::Duration::from_secs(5);
    let mut best: Option<(SystemTime, String)> = None;
    for e in fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if taken.contains(&id) {
            continue;
        }
        let Ok(m) = e.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if m >= since && best.as_ref().is_none_or(|(t, _)| m > *t) {
            best = Some((m, id));
        }
    }
    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_claude() {
        assert_eq!(
            encode_project_dir(Path::new("/Users/alex/usage")),
            "-Users-alex-usage"
        );
        assert_eq!(
            encode_project_dir(Path::new("/tmp/claude-501/x_y.z")),
            "-tmp-claude-501-x-y-z"
        );
    }

    #[test]
    fn roundtrips_and_detects() {
        let st = AppState {
            recents: vec![],
            window: None,
            show_email: Some(false),
            memory_saver: Some(true),
            privacy: Some(true),
            focus: 1,
            next_uid: Some(40),
            slots: vec![SlotState {
                account: Some("work".into()),
                active: 0,
                tabs: vec![TabState {
                    cwd: "/x".into(),
                    session_id: Some("abc".into()),
                    name: Some("api".into()),
                    accent: Some("teal".into()),
                    uid: Some(12),
                    group: Some("backend".into()),
                    pinned: true,
                    opened: Some(1_700_000_000),
                }],
                sidebar_collapsed: true,
                tab_pos: Some(crate::slot::TabPos::Right),
                sidebar_w: Some(30),
                hidden: true,
                groups: vec![crate::tab_groups::TabGroup {
                    name: "backend".into(),
                    color: Some("sage".into()),
                    collapsed: true,
                }],
                sort: crate::tab_groups::TabSort::Recent,
            }],
            layout: Some("focus".into()),
            tab_list_width: Some("wide".into()),
            closed_tabs: vec![crate::closed::ClosedTab {
                account: "work".into(),
                cwd: "/x".into(),
                session_id: Some("abc".into()),
                name: None,
                accent: None,
                loops: vec![crate::closed::SavedLoop {
                    id: "j1".into(),
                    cron: Some("*/5 * * * *".into()),
                    prompt: "hi".into(),
                    recurring: true,
                    wakeup: false,
                }],
                group: 7,
                closed_at: 1,
                tab_group: None,
                pinned: false,
                label: "x".into(),
            }],
            sessions_view: Some(SessionsView {
                sort: "tokens".into(),
                reverse: true,
                group: "project".into(),
                headless: true,
            }),
            ratios: [(
                "2x1".to_string(),
                crate::layout::Ratios {
                    cols: vec![2.0, 1.0],
                    rows: vec![],
                },
            )]
            .into(),
        };
        let text = serde_json::to_string(&st).unwrap();
        assert_eq!(serde_json::from_str::<AppState>(&text).unwrap(), st);
        assert_eq!(
            serde_json::from_str::<AppState>("{}").unwrap(),
            AppState::default()
        );

        let root = std::env::temp_dir().join(format!("godterm-state-{}", std::process::id()));
        let cwd = Path::new("/Users/test/proj");
        let pdir = root.join("projects").join(encode_project_dir(cwd));
        fs::create_dir_all(&pdir).unwrap();
        let start = SystemTime::now();
        fs::write(pdir.join("old.jsonl"), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(pdir.join("new.jsonl"), "{}").unwrap();
        assert_eq!(
            detect_session(&root, cwd, start, &[]).as_deref(),
            Some("new")
        );
        assert_eq!(
            detect_session(&root, cwd, start, &["new".into()]).as_deref(),
            Some("old")
        );
        let _ = fs::remove_dir_all(&root);
    }
}
