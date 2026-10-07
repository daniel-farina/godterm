//! A grid slot: one account, several claude sessions as tabs.

use std::path::PathBuf;

use crate::pane::Pane;

/// Where a pane shows its tab list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabPos {
    Top,
    Left,
    Right,
}

impl TabPos {
    pub fn parse(s: &str) -> Option<TabPos> {
        match s.trim().to_ascii_lowercase().as_str() {
            "top" => Some(TabPos::Top),
            "left" => Some(TabPos::Left),
            "right" => Some(TabPos::Right),
            _ => None,
        }
    }

    pub fn next(self) -> TabPos {
        match self {
            TabPos::Top => TabPos::Left,
            TabPos::Left => TabPos::Right,
            TabPos::Right => TabPos::Top,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TabPos::Top => "top",
            TabPos::Left => "left",
            TabPos::Right => "right",
        }
    }
}

/// `sidebar_scroll` value meaning "scroll to the active tab".
pub const FOLLOW: usize = usize::MAX;

pub struct Slot {
    pub account: Option<usize>,
    pub tabs: Vec<Pane>,
    pub active: usize,
    /// Tab list collapsed to a narrow strip of numbers and markers.
    pub sidebar_collapsed: bool,
    /// First tab shown in the tab list when there are more than fit;
    /// `FOLLOW` keeps the active tab in view.
    pub sidebar_scroll: usize,
    /// Per pane override of where the tab list goes (None: the global one).
    pub tab_pos: Option<TabPos>,
    /// Tab list width dragged by hand (None: a share of the pane).
    pub sidebar_w: Option<u16>,
    pub(crate) size: (u16, u16),
    /// Hidden from the layout; its tabs keep running in the background.
    pub hidden: bool,
    /// Named groups in this account's tab list, in their order.
    pub groups: Vec<crate::tab_groups::TabGroup>,
    /// How the list is sorted (within pinned, each group, and the rest).
    pub sort: crate::tab_groups::TabSort,
    /// The last order shown, so a sort by activity does not jump around
    /// under the mouse: (when, tab uids).
    pub order_cache: std::cell::RefCell<Option<(std::time::Instant, Vec<u64>)>>,
}

/// What each tab is called in lists: its name, made unique within the
/// pane. Tabs named after the same folder basename get the parent folder
/// ("api/server", "web/server"), and any still alike a number ("x 2").
pub fn tab_labels(slot: &Slot) -> Vec<String> {
    let names: Vec<String> = slot.tabs.iter().map(|t| t.name()).collect();
    let mut out = names.clone();
    for (i, t) in slot.tabs.iter().enumerate() {
        let custom = t.custom_name.as_ref().is_some_and(|n| !n.trim().is_empty());
        if custom || names.iter().filter(|n| **n == names[i]).count() < 2 {
            continue;
        }
        if let Some(parent) = t.cwd.parent().and_then(|p| p.file_name()) {
            out[i] = format!("{}/{}", parent.to_string_lossy(), names[i]);
        }
    }
    let snapshot = out.clone();
    for i in 0..out.len() {
        let same = snapshot[..i].iter().filter(|n| **n == snapshot[i]).count();
        if same > 0 {
            out[i] = format!("{} {}", snapshot[i], same + 1);
        }
    }
    out
}

impl Slot {
    pub fn new(account: Option<usize>, cwd: PathBuf) -> Slot {
        Slot {
            account,
            tabs: vec![Pane::new(cwd)],
            active: 0,
            sidebar_collapsed: false,
            sidebar_scroll: FOLLOW,
            tab_pos: None,
            sidebar_w: None,
            size: (24, 80),
            hidden: false,
            groups: vec![],
            sort: Default::default(),
            order_cache: Default::default(),
        }
    }

    pub fn cur(&self) -> &Pane {
        &self.tabs[self.active]
    }

    pub fn cur_mut(&mut self) -> &mut Pane {
        &mut self.tabs[self.active]
    }

    /// Append a tab and make it active. Returns its index.
    pub fn add_tab(&mut self, cwd: PathBuf) -> usize {
        let mut p = Pane::new(cwd);
        p.resize(self.size.0, self.size.1);
        self.tabs.push(p);
        self.active = self.tabs.len() - 1;
        self.sidebar_scroll = FOLLOW;
        self.active
    }

    /// Close tab `i` (killing its process). A slot always keeps one tab.
    pub fn close_tab(&mut self, i: usize, fallback_cwd: PathBuf) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs[i].kill();
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            let mut p = Pane::new(fallback_cwd);
            p.resize(self.size.0, self.size.1);
            self.tabs.push(p);
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > i {
            self.active -= 1;
        }
    }

    pub fn next_tab(&mut self) {
        self.sidebar_scroll = FOLLOW;
        self.active = (self.active + 1) % self.tabs.len();
    }

    pub fn prev_tab(&mut self) {
        self.sidebar_scroll = FOLLOW;
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
    }

    pub fn select(&mut self, i: usize) -> bool {
        if i < self.tabs.len() {
            self.active = i;
            self.sidebar_scroll = FOLLOW;
            true
        } else {
            false
        }
    }

    /// Resize every tab so background tabs are correct when shown.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.size = (rows, cols);
        for t in &mut self.tabs {
            t.resize(rows, cols);
        }
    }

    pub fn find_uid(&self, uid: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.uid == uid)
    }

    pub fn kill_all(&mut self) {
        for t in &mut self.tabs {
            t.kill();
        }
    }

    pub fn any_running(&self) -> bool {
        self.tabs.iter().any(|t| t.is_running())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn colliding_names_get_parents_then_numbers() {
        let mut s = Slot::new(None, "/w/api/server".into());
        for p in [
            "/w/web/server",
            "/w/web2/server",
            "/x/web/server",
            "/w/solo",
        ] {
            s.add_tab(p.into());
        }
        s.tabs[4].custom_name = Some("server".into());
        let l = tab_labels(&s);
        assert_eq!(
            l,
            vec![
                "api/server",
                "web/server",
                "web2/server",
                "web/server 2",
                "server"
            ]
        );
    }

    use super::*;

    #[test]
    fn tab_bookkeeping() {
        let mut s = Slot::new(Some(0), "/a".into());
        assert_eq!(s.add_tab("/b".into()), 1);
        assert_eq!(s.add_tab("/c".into()), 2);
        assert_eq!(s.cur().name(), "c");
        s.next_tab();
        assert_eq!(s.active, 0);
        s.prev_tab();
        assert_eq!(s.active, 2);
        s.select(1);
        s.close_tab(0, "/z".into());
        assert_eq!(s.active, 0);
        assert_eq!(s.cur().name(), "b");
        s.close_tab(0, "/z".into());
        s.close_tab(0, "/z".into());
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.cur().name(), "z");
        assert!(!s.select(5));
    }
}
