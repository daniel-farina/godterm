use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::pane::{Activity, Pane};
use crate::slot::Slot;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabGroup {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabSort {
    #[default]
    Manual,

    Active,

    Recent,

    Opened,
    Name,
    Folder,

    State,
}

pub const SORTS: &[TabSort] = &[
    TabSort::Manual,
    TabSort::Active,
    TabSort::Recent,
    TabSort::Name,
    TabSort::State,
    TabSort::Opened,
    TabSort::Folder,
];

impl TabSort {
    pub fn name(self) -> &'static str {
        match self {
            TabSort::Manual => "manual",
            TabSort::Active => "active",
            TabSort::Recent => "recent",
            TabSort::Opened => "opened",
            TabSort::Name => "name",
            TabSort::Folder => "folder",
            TabSort::State => "state",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TabSort::Manual => "Manual (drag order)",
            TabSort::Active => "Most active (working first)",
            TabSort::Recent => "Most recent activity",
            TabSort::Opened => "Created (newest first)",
            TabSort::Name => "Name",
            TabSort::Folder => "Folder",
            TabSort::State => "Status (blocked and confirm first)",
        }
    }

    pub fn parse(s: &str) -> Option<TabSort> {
        let s = s.trim().to_lowercase();
        let has = |w: &[&str]| w.iter().any(|x| s.contains(x));
        Some(if has(&["manual", "drag", "custom", "my order"]) {
            TabSort::Manual
        } else if has(&["open", "newest", "created", "creation", "oldest"]) {
            TabSort::Opened
        } else if has(&["recent", "last used", "latest", "last activity"]) {
            TabSort::Recent
        } else if has(&["active", "busy", "busiest", "working"]) {
            TabSort::Active
        } else if has(&["activ"]) {
            TabSort::Recent
        } else if has(&["name", "alpha"]) {
            TabSort::Name
        } else if has(&["folder", "path", "dir", "project"]) {
            TabSort::Folder
        } else if has(&["state", "status", "approval", "blocked", "confirm", "needs"]) {
            TabSort::State
        } else {
            return None;
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Tab(usize),
    Group(usize),

    ArchHead,
    Arch(usize),
}

pub const RESORT_EVERY: Duration = Duration::from_secs(3);

fn state_rank(t: &Pane) -> u8 {
    match t.activity {
        Activity::Permission => 0,
        Activity::Working => 1,
        _ => 2,
    }
}

impl Slot {
    pub fn group_of(&self, t: usize) -> Option<usize> {
        let g = self.tabs.get(t)?.group.as_ref()?;
        self.groups.iter().position(|x| &x.name == g)
    }

    pub fn find_group(&self, name: &str) -> Option<usize> {
        let n = name.trim().to_lowercase();
        self.groups.iter().position(|g| g.name.to_lowercase() == n)
    }

    pub fn accent_of(&self, t: usize) -> Option<String> {
        let tab = self.tabs.get(t)?;
        tab.accent
            .clone()
            .or_else(|| self.group_of(t).and_then(|g| self.groups[g].color.clone()))
    }

    fn sorted(&self, mut ix: Vec<usize>, rank: &dyn Fn(usize) -> u8) -> Vec<usize> {
        let t = &self.tabs;
        match self.sort {
            TabSort::Manual => {}
            TabSort::Active => ix.sort_by_key(|&i| {
                let out = t[i].last_output_at();
                (
                    t[i].activity != Activity::Working,
                    std::cmp::Reverse(out.max(t[i].activity_since)),
                )
            }),
            TabSort::Recent => ix.sort_by_key(|&i| std::cmp::Reverse(t[i].last_activity())),
            TabSort::Opened => ix.sort_by_key(|&i| std::cmp::Reverse(t[i].opened)),
            TabSort::Name => ix.sort_by_key(|&i| t[i].name().to_lowercase()),
            TabSort::Folder => ix.sort_by_key(|&i| t[i].cwd.to_string_lossy().to_lowercase()),
            TabSort::State => ix.sort_by_key(|&i| rank(i)),
        }
        ix
    }

    pub fn tab_order(&self, ungrouped_top: bool, hold: bool) -> Vec<usize> {
        self.tab_order_by(ungrouped_top, hold, &|i| state_rank(&self.tabs[i]))
    }

    pub fn tab_order_by(
        &self,
        ungrouped_top: bool,
        hold: bool,
        rank: &dyn Fn(usize) -> u8,
    ) -> Vec<usize> {
        let fresh = self.fresh_order(ungrouped_top, rank);
        if !matches!(self.sort, TabSort::Recent | TabSort::Active) {
            *self.order_cache.borrow_mut() = None;
            return fresh;
        }
        let uids: Vec<u64> = fresh.iter().map(|&i| self.tabs[i].uid).collect();
        let mut cache = self.order_cache.borrow_mut();
        if let Some((at, old)) = cache.as_ref() {
            let same_set = old.len() == uids.len() && uids.iter().all(|u| old.contains(u));
            if same_set && (hold || at.elapsed() < RESORT_EVERY) {
                return old.iter().filter_map(|u| self.find_uid(*u)).collect();
            }
        }
        *cache = Some((Instant::now(), uids));
        fresh
    }

    fn fresh_order(&self, ungrouped_top: bool, rank: &dyn Fn(usize) -> u8) -> Vec<usize> {
        let n = self.tabs.len();
        let pinned: Vec<usize> = (0..n).filter(|&i| self.tabs[i].pinned).collect();
        let loose: Vec<usize> = (0..n)
            .filter(|&i| !self.tabs[i].pinned && self.group_of(i).is_none())
            .collect();
        let mut out = self.sorted(pinned, rank);
        if ungrouped_top {
            out.extend(self.sorted(loose.clone(), rank));
        }
        for g in 0..self.groups.len() {
            let m: Vec<usize> = (0..n)
                .filter(|&i| !self.tabs[i].pinned && self.group_of(i) == Some(g))
                .collect();
            out.extend(self.sorted(m, rank));
        }
        if !ungrouped_top {
            out.extend(self.sorted(loose, rank));
        }
        out
    }

    pub fn rows(&self, ungrouped_top: bool, hold: bool) -> Vec<Row> {
        self.rows_by(ungrouped_top, hold, &|i| state_rank(&self.tabs[i]), None)
    }

    pub fn rows_by(
        &self,
        ungrouped_top: bool,
        hold: bool,
        rank: &dyn Fn(usize) -> u8,
        only: Option<&[bool]>,
    ) -> Vec<Row> {
        let mut order = self.tab_order_by(ungrouped_top, hold, rank);
        if let Some(m) = only {
            order.retain(|&i| m.get(i).copied().unwrap_or(false));
        }
        let mut rows = vec![];
        let mut shown = vec![false; self.groups.len()];

        for &i in &order {
            let g = (!self.tabs[i].pinned).then(|| self.group_of(i)).flatten();
            if let Some(g) = g {
                if !shown[g] {
                    shown[g] = true;
                    rows.push(Row::Group(g));
                }
                if self.groups[g].collapsed && only.is_none() {
                    continue;
                }
            }
            rows.push(Row::Tab(i));
        }
        if only.is_none() {
            for (g, s) in shown.iter().enumerate() {
                if !s {
                    rows.push(Row::Group(g));
                }
            }
        }
        rows
    }

    pub fn members(&self, g: usize) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&i| self.group_of(i) == Some(g))
            .collect()
    }

    pub fn group_state(&self, g: usize) -> Activity {
        let m = self.members(g);
        if m.iter()
            .any(|&i| self.tabs[i].activity == Activity::Permission)
        {
            Activity::Permission
        } else if m
            .iter()
            .any(|&i| self.tabs[i].activity == Activity::Working)
        {
            Activity::Working
        } else {
            Activity::Idle
        }
    }

    pub fn group_tabs(&mut self, tabs: &[usize], name: &str, color: Option<String>) -> usize {
        let g = match self.find_group(name) {
            Some(g) => g,
            None => {
                self.groups.push(TabGroup {
                    name: name.trim().to_string(),
                    color: None,
                    collapsed: false,
                });
                self.groups.len() - 1
            }
        };
        if color.is_some() {
            self.groups[g].color = color;
        }
        let gname = self.groups[g].name.clone();
        for &t in tabs {
            if let Some(tab) = self.tabs.get_mut(t) {
                tab.group = Some(gname.clone());
            }
        }
        g
    }

    pub fn ungroup(&mut self, g: usize) {
        if g >= self.groups.len() {
            return;
        }
        let name = self.groups.remove(g).name;
        for t in &mut self.tabs {
            if t.group.as_deref() == Some(name.as_str()) {
                t.group = None;
            }
        }
    }

    pub fn rename_group(&mut self, g: usize, to: &str) {
        let Some(old) = self.groups.get(g).map(|x| x.name.clone()) else {
            return;
        };
        let to = to.trim().to_string();
        for t in &mut self.tabs {
            if t.group.as_deref() == Some(old.as_str()) {
                t.group = Some(to.clone());
            }
        }
        self.groups[g].name = to;
    }

    pub fn step_tab(&mut self, ungrouped_top: bool, dir: isize, only: Option<&[bool]>) {
        let mut order = self.tab_order(ungrouped_top, true);
        if let Some(m) = only {
            order.retain(|&i| m.get(i).copied().unwrap_or(false));
        }
        if order.is_empty() {
            return;
        }
        let at = order.iter().position(|&i| i == self.active).unwrap_or(0) as isize;
        let n = order.len() as isize;
        self.active = order[((at + dir).rem_euclid(n)) as usize];
        self.sidebar_scroll = crate::slot::FOLLOW;
    }

    pub fn repair_groups(&mut self) {
        let names: Vec<String> = self.tabs.iter().filter_map(|t| t.group.clone()).collect();
        for n in names {
            if self.find_group(&n).is_none() {
                self.groups.push(TabGroup {
                    name: n,
                    color: None,
                    collapsed: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> Slot {
        let mut s = Slot::new(Some(0), "/w/api".into());
        for p in ["/w/web", "/w/docs", "/w/exp1", "/w/exp2"] {
            s.add_tab(p.into());
        }
        s
    }

    fn names(s: &Slot, order: &[usize]) -> Vec<String> {
        order.iter().map(|&i| s.tabs[i].name()).collect()
    }

    #[test]
    fn groups_pins_and_rows() {
        let mut s = slot();
        s.group_tabs(&[3, 4], "experiments", Some("sage".into()));
        s.tabs[2].pinned = true;

        assert_eq!(
            names(&s, &s.tab_order(false, false)),
            ["docs", "exp1", "exp2", "api", "web"]
        );
        assert_eq!(
            names(&s, &s.tab_order(true, false)),
            ["docs", "api", "web", "exp1", "exp2"]
        );
        assert_eq!(
            s.rows(false, false),
            vec![
                Row::Tab(2),
                Row::Group(0),
                Row::Tab(3),
                Row::Tab(4),
                Row::Tab(0),
                Row::Tab(1)
            ]
        );

        s.groups[0].collapsed = true;
        s.tabs[4].activity = Activity::Working;
        assert_eq!(
            s.rows(false, false),
            vec![Row::Tab(2), Row::Group(0), Row::Tab(0), Row::Tab(1)]
        );
        assert_eq!(s.group_state(0), Activity::Working);
        s.tabs[3].activity = Activity::Permission;
        assert_eq!(s.group_state(0), Activity::Permission);

        assert_eq!(s.accent_of(3).as_deref(), Some("sage"));
        s.tabs[3].accent = Some("clay".into());
        assert_eq!(s.accent_of(3).as_deref(), Some("clay"));
        assert_eq!(s.accent_of(0), None);

        s.rename_group(0, "lab");
        assert_eq!(s.tabs[4].group.as_deref(), Some("lab"));
        s.ungroup(0);
        assert!(s.groups.is_empty() && s.tabs[4].group.is_none());

        s.active = 2;
        s.step_tab(false, 1, None);
        assert_eq!(s.active, 0);
    }

    #[test]
    fn every_sort_mode_and_the_hysteresis() {
        let mut s = slot();
        s.tabs[0].custom_name = Some("zeta".into());
        s.tabs[4].activity = Activity::Permission;
        s.tabs[1].activity = Activity::Working;
        let order = |s: &Slot| names(s, &s.tab_order(false, false));
        s.sort = TabSort::Name;
        assert_eq!(order(&s), ["docs", "exp1", "exp2", "web", "zeta"]);
        s.sort = TabSort::Folder;
        assert_eq!(order(&s), ["zeta", "docs", "exp1", "exp2", "web"]);
        s.sort = TabSort::State;
        assert_eq!(order(&s)[..2], ["exp2", "web"]);
        s.sort = TabSort::Opened;
        let now = std::time::SystemTime::now();
        for (k, t) in s.tabs.iter_mut().enumerate() {
            t.opened = now - Duration::from_secs(100 - k as u64);
        }
        assert_eq!(order(&s), ["exp2", "exp1", "docs", "web", "zeta"]);
        s.sort = TabSort::Manual;
        assert_eq!(order(&s), ["zeta", "web", "docs", "exp1", "exp2"]);

        s.sort = TabSort::Recent;
        let base = Instant::now();
        for (k, t) in s.tabs.iter_mut().enumerate() {
            t.last_input = base + Duration::from_millis(10 * k as u64);
        }
        assert_eq!(order(&s)[0], "exp2");
        s.tabs[0].last_input = base + Duration::from_secs(5);
        assert_eq!(order(&s)[0], "exp2", "held for a moment");
        if let Some((at, _)) = s.order_cache.borrow_mut().as_mut() {
            *at -= RESORT_EVERY;
        }
        assert_eq!(
            names(&s, &s.tab_order(false, true))[0],
            "exp2",
            "not while hovering"
        );
        assert_eq!(order(&s)[0], "zeta", "then it moves");
        for (w, want) in [
            ("most recent", TabSort::Recent),
            ("recently opened", TabSort::Opened),
            ("by name", TabSort::Name),
            ("folder", TabSort::Folder),
            ("state", TabSort::State),
            ("manual", TabSort::Manual),
        ] {
            assert_eq!(TabSort::parse(w), Some(want), "{w}");
        }
    }
}
