//! Usage aware failover: when a tab's account runs out (its binding usage
//! bucket under usage.failover_pct, or the tab says it hit its limit), a
//! calm notice offers to move the session to the account of the same
//! provider with the most left. One yes moves it (Ctrl-a F, a click on
//! the notice, or "move it" to the assistant); "auto" moves idle tabs by
//! itself and says so. Nothing moves without one of those.

use std::time::Instant;

use crate::app::App;
use crate::pane::{Activity, LaunchKind};

/// A move on offer.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub uid: u64,
    pub from: usize,
    pub to: usize,
    /// "Account 3 is out for the week (resets Fri 1 PM)".
    pub why: String,
    /// The tab said it hit its limit (not only the numbers).
    pub from_output: bool,
    pub at: Instant,
}

/// What claude, grok and friends print when a limit stops them.
const LIMIT_SIGNS: &[&str] = &[
    "usage limit reached",
    "hit your limit",
    "reached your usage limit",
    "limit reached",
    "out of usage",
    "weekly limit left: 0%",
    "limit will reset",
];

/// Whether a tab's screen says it hit a usage limit (its last lines).
pub fn limit_error(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(12)..]
        .join("\n")
        .to_lowercase();
    LIMIT_SIGNS.iter().any(|s| tail.contains(s))
}

impl App {
    /// The account's binding bucket, by its provider (generic).
    fn binding_left(&self, a: usize) -> Option<crate::providers::Bucket> {
        let b = self.account_buckets(a)?;
        crate::providers::binding(&b).cloned()
    }

    /// The account of the same provider with the most left (over the
    /// threshold), to move a tab of `slot` to.
    pub fn failover_target(&self, slot: usize) -> Option<(usize, f64)> {
        let min = self.cfg.usage.failover_pct;
        self.move_targets(slot)
            .into_iter()
            .filter(|m| m.disabled.is_none())
            .filter_map(|m| {
                let l = self.binding_left(m.account)?.left_pct;
                (l > min.max(2.0)).then_some((m.account, l))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Why account `a` counts as out, if it does.
    fn out_reason(&self, a: usize, from_output: bool) -> Option<String> {
        let name = self.cfg.accounts[a].display().to_string();
        let k = self.binding_left(a);
        let low = k
            .as_ref()
            .filter(|k| k.left_pct < self.cfg.usage.failover_pct);
        let resets = |k: &crate::providers::Bucket| {
            k.resets_at
                .map(|t| format!(" (resets {})", crate::app_provider::reset_word(t)))
                .unwrap_or_default()
        };
        match (low, from_output) {
            (Some(k), _) if k.left_pct < 1.0 => Some(format!(
                "{name} is out {}{}",
                if k.long == "weekly" {
                    "for the week".to_string()
                } else {
                    format!("of its {} limit", k.long)
                },
                resets(k)
            )),
            (Some(k), _) => Some(format!(
                "{name} has {:.0}% left ({} limit){}",
                k.left_pct,
                k.long,
                resets(k)
            )),
            (None, true) => Some(format!("{name} hit its usage limit")),
            (None, false) => None,
        }
    }

    /// Look at every tab (from tick): make, keep or drop offers, and in
    /// auto mode move the idle ones.
    pub fn failover_scan(&mut self) {
        let mode = self.cfg.usage.auto_failover.clone();
        if mode == "off" {
            self.failover.clear();
            return;
        }
        let mut seen = vec![];
        for slot in 0..self.panes.len() {
            let Some(a) = self.panes[slot].account else {
                continue;
            };
            for t in 0..self.panes[slot].tabs.len() {
                let tab = &self.panes[slot].tabs[t];
                if !tab.is_running() || tab.kind == LaunchKind::Login {
                    continue;
                }
                let uid = tab.uid;
                let said = limit_error(
                    &tab.parser
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .screen()
                        .contents(),
                );
                let Some(why) = self.out_reason(a, said) else {
                    continue;
                };
                let Some((to, _)) = self.failover_target(slot) else {
                    continue;
                };
                seen.push(uid);
                if self.failover_declined.contains(&(uid, a)) {
                    continue;
                }
                match self.failover.iter_mut().find(|o| o.uid == uid) {
                    Some(o) => {
                        o.to = to;
                        o.why = why;
                        o.from_output = said;
                    }
                    None => {
                        crate::log::info(&format!("failover: offer t{uid}: {why}"));
                        self.failover.push(Offer {
                            uid,
                            from: a,
                            to,
                            why,
                            from_output: said,
                            at: Instant::now(),
                        });
                    }
                }
            }
        }
        // Gone or fine again: no offer.
        self.failover.retain(|o| seen.contains(&o.uid));
        if mode == "auto" {
            let idle: Vec<u64> = self
                .failover
                .iter()
                .filter(|o| {
                    self.find_tab(o.uid).is_some_and(|(s, t)| {
                        !matches!(
                            self.panes[s].tabs[t].activity,
                            Activity::Working | Activity::Permission
                        )
                    })
                })
                .map(|o| o.uid)
                .collect();
            for uid in idle {
                self.accept_failover(Some(uid), true);
            }
        }
    }

    /// The offer the notice shows: the focused tab's, else the oldest.
    pub fn failover_offer(&self) -> Option<&Offer> {
        let focused = self.panes.get(self.focus).map(|p| p.cur().uid);
        self.failover
            .iter()
            .find(|o| Some(o.uid) == focused)
            .or_else(|| self.failover.first())
    }

    /// "Account 3 is out for the week (resets Fri 13:00). Move this
    /// session to Account 4 (100% left)?"
    pub fn failover_text(&self, o: &Offer) -> String {
        let left = self
            .binding_left(o.to)
            .map(|k| format!(" ({:.0}% left)", k.left_pct))
            .unwrap_or_default();
        format!(
            "{}. Move this session to {}{left}?",
            o.why,
            self.cfg.accounts[o.to].display()
        )
    }

    /// The yes: move the offered tab (`uid`, else the shown offer).
    /// `auto`: moved by usage.auto_failover, so it says so.
    pub fn accept_failover(&mut self, uid: Option<u64>, auto: bool) -> Option<String> {
        let o = match uid {
            Some(u) => self.failover.iter().find(|o| o.uid == u)?.clone(),
            None => self.failover_offer()?.clone(),
        };
        self.failover.retain(|x| x.uid != o.uid);
        let (s, t) = self.find_tab(o.uid)?;
        let name = self.panes[s].tabs[t].name();
        let to = self.cfg.accounts[o.to].display().to_string();
        if matches!(self.panes[s].tabs[t].activity, Activity::Working) {
            // Busy: it moves once its turn ends.
            self.queued_moves.push(crate::app_tabmove::QueuedMove {
                uid: o.uid,
                target: o.to,
                copy: false,
                same_folder: true,
            });
        } else {
            self.move_tab_now(s, t, o.to, false, true);
        }
        let msg = if auto {
            format!("{}, so I moved {name} to {to}.", o.why)
        } else {
            format!("Moving {name} to {to}.")
        };
        crate::log::info(&format!("failover: t{} to {to} (auto {auto})", o.uid));
        self.flash(msg.clone());
        if auto {
            self.note(msg.clone());
            self.speak_as(crate::voice::tts::Kind::Announce, &msg);
        }
        Some(msg)
    }

    /// Not now: no more offers for this tab while it stays on that account.
    pub fn decline_failover(&mut self) {
        if let Some(o) = self.failover_offer().cloned() {
            self.failover_declined.push((o.uid, o.from));
            self.failover.retain(|x| x.uid != o.uid);
        }
    }

    /// The state block's line: offers the user can take by voice.
    pub fn failover_state_line(&self) -> String {
        if self.failover.is_empty() {
            return String::new();
        }
        let v: Vec<String> = self
            .failover
            .iter()
            .map(|o| format!("t{} ({}) -> a{}", o.uid, o.why, o.to + 1))
            .collect();
        format!(
            "move offers (account running out): {}; when the user says \"move it\" or yes, move_tab that tab to that account; never without their yes\n",
            v.join("; ")
        )
    }
}
