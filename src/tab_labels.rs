//! The small status label at the end of a tab's row: "blocked" (a
//! permission, plan or trust prompt is open), "confirm" (the assistant
//! waits on the user's yes about this tab), "limit" (its screen says it
//! hit a usage limit), "error" (claude exited with an error or failed to
//! start), "background" (its turn ended but background agents or shells
//! it started still run: not finished yet), "finished" (a real turn ended,
//! with nothing left running, while the user looked elsewhere) and
//! "queued" (a prompt on its way in). One label per tab, in that
//! order. All from real state: the tab's activity, pending_confirms, the
//! failover scan, the process state, the finished mark and deliveries.

use ratatui::style::Color;

use crate::app::{App, View};
use crate::pane::{Activity, PaneState};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TabLabel {
    Blocked,
    Confirm,
    Limit,
    Error,
    Background,
    Finished,
    Queued,
}

/// Every label, in precedence order.
pub const ALL: [TabLabel; 7] = [
    TabLabel::Blocked,
    TabLabel::Confirm,
    TabLabel::Limit,
    TabLabel::Error,
    TabLabel::Background,
    TabLabel::Finished,
    TabLabel::Queued,
];

/// How much of a label fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Full,
    Short,
    Dot,
}

impl TabLabel {
    pub fn word(self) -> &'static str {
        match self {
            TabLabel::Blocked => "blocked",
            TabLabel::Confirm => "confirm",
            TabLabel::Limit => "limit",
            TabLabel::Error => "error",
            TabLabel::Background => "background",
            TabLabel::Finished => "finished",
            TabLabel::Queued => "queued",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            TabLabel::Blocked => "blk",
            TabLabel::Confirm => "ask",
            TabLabel::Limit => "lim",
            TabLabel::Error => "err",
            TabLabel::Background => "bg",
            TabLabel::Finished => "done",
            TabLabel::Queued => "q",
        }
    }

    pub fn color(self) -> Color {
        match self {
            TabLabel::Blocked => theme::SAND,
            TabLabel::Confirm => theme::MIST,
            TabLabel::Limit => theme::CLAY,
            TabLabel::Error => theme::RUST,
            TabLabel::Background => theme::SLATE,
            TabLabel::Finished => theme::SAGE,
            TabLabel::Queued => theme::SLATE,
        }
    }

    /// The hover text.
    pub fn hint(self) -> &'static str {
        match self {
            TabLabel::Blocked => "Blocked: it waits for your approval",
            TabLabel::Confirm => "The assistant asked you to confirm something about this tab",
            TabLabel::Limit => "It hit a usage limit",
            TabLabel::Error => "claude ended with an error; Enter restarts it",
            TabLabel::Background => {
                "Its turn ended, but background agents or shells it started still run"
            }
            TabLabel::Finished => "Finished its work while you were elsewhere",
            TabLabel::Queued => "A prompt is waiting to go in",
        }
    }

    /// The pill's text at a fit: " finished ", " done ", or a dot.
    pub fn pill(self, fit: Fit) -> String {
        match fit {
            Fit::Full => format!(" {} ", self.word()),
            Fit::Short => format!(" {} ", self.short()),
            Fit::Dot => "●".into(),
        }
    }
}

/// The widest form of `l` that leaves the name `min_name` columns of
/// `room`, with one space before the pill.
pub fn fit(l: TabLabel, room: usize, min_name: usize) -> Fit {
    let need = |f: Fit| l.pill(f).chars().count() + 1 + min_name;
    if room >= need(Fit::Full) {
        Fit::Full
    } else if room >= need(Fit::Short) {
        Fit::Short
    } else {
        Fit::Dot
    }
}

/// `c` mixed into `bg` (a Reset background counts as the usual dark one).
pub fn tint(bg: Color, c: Color, pct: u16) -> Color {
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (30, 32, 35),
    };
    let ((r0, g0, b0), (r1, g1, b1)) = (rgb(bg), rgb(c));
    let mix = |a: u8, b: u8| ((a as u16 * (100 - pct) + b as u16 * pct) / 100) as u8;
    Color::Rgb(mix(r0, r1), mix(g0, g1), mix(b0, b1))
}

/// "2 finished · 1 blocked" (or "2 done · 1 blk" when `short`).
pub fn summary(counts: &[(TabLabel, usize)], short: bool) -> String {
    counts
        .iter()
        .map(|(l, n)| format!("{n} {}", if short { l.short() } else { l.word() }))
        .collect::<Vec<_>>()
        .join(" · ")
}

impl App {
    /// Tabs (by uid) an answerable confirmation is about.
    pub fn confirm_uids(&self) -> Vec<u64> {
        let mut out = vec![];
        for p in self.pending_confirms.iter().filter(|p| p.answerable()) {
            if let Some(u) = p.plan.get("uid").and_then(|v| v.as_u64()) {
                out.push(u);
            }
            if let Some(us) = p.plan.get("uids").and_then(|v| v.as_array()) {
                out.extend(us.iter().filter_map(|v| v.as_u64()));
            }
        }
        out
    }

    /// The tab the user looks at: the focused pane's shown tab.
    pub fn tab_in_view(&self, s: usize, t: usize) -> bool {
        self.view == View::Grid
            && s == self.focus
            && self.panes.get(s).is_some_and(|p| p.active == t)
    }

    /// The label on tab `t` of pane `s`, if any (highest first).
    pub fn tab_label(&self, s: usize, t: usize) -> Option<TabLabel> {
        let tab = self.panes.get(s)?.tabs.get(t)?;
        let uid = tab.uid;
        if tab.activity == Activity::Permission {
            return Some(TabLabel::Blocked);
        }
        if self.confirm_uids().contains(&uid) {
            return Some(TabLabel::Confirm);
        }
        if tab.is_running() && self.limit_hit.contains(&uid) {
            return Some(TabLabel::Limit);
        }
        if tab.crashed() || matches!(tab.state, PaneState::Failed(_)) {
            return Some(TabLabel::Error);
        }
        if tab.bg_wait && tab.activity == Activity::Ready {
            return Some(TabLabel::Background);
        }
        if tab.finished && !self.tab_in_view(s, t) {
            return Some(TabLabel::Finished);
        }
        if self.queued_for(uid) > 0 {
            return Some(TabLabel::Queued);
        }
        None
    }

    /// The user looked at this tab: no longer "finished".
    pub fn mark_seen(&mut self, s: usize, t: usize) {
        if let Some(tab) = self.panes.get_mut(s).and_then(|p| p.tabs.get_mut(t)) {
            if tab.finished {
                tab.finished = false;
                self.state_dirty = true;
            }
        }
    }

    /// Labels of the tabs of pane `i` that the list does not show as rows
    /// (the list collapsed or absent: all; else those in folded groups),
    /// counted, in precedence order.
    pub fn hidden_label_counts(&self, i: usize, all: bool) -> Vec<(TabLabel, usize)> {
        let Some(slot) = self.panes.get(i) else {
            return vec![];
        };
        let shown: Vec<usize> = if all {
            vec![]
        } else {
            slot.rows(self.ungrouped_top(), false)
                .into_iter()
                .filter_map(|r| match r {
                    crate::tab_groups::Row::Tab(t) => Some(t),
                    _ => None,
                })
                .collect()
        };
        let labels: Vec<TabLabel> = (0..slot.tabs.len())
            .filter(|t| !shown.contains(t))
            .filter_map(|t| self.tab_label(i, t))
            .collect();
        ALL.iter()
            .map(|l| (*l, labels.iter().filter(|x| *x == l).count()))
            .filter(|(_, n)| *n > 0)
            .collect()
    }
}
