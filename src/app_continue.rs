use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app::App;
use crate::control::tab_id;
use crate::pane::{Activity, Pane};
use crate::state::TabState;

pub const CONTINUE_PROMPT: &str = "GodTerm restarted while you were working. Continue exactly where you left off; if the last step was interrupted, re-check its state before redoing it; if you were waiting for an approval, ask for it again.";

pub const MARK_MAX_AGE_S: u64 = 30 * 60;

const SETTLE: Duration = Duration::from_millis(1500);

const GIVE_UP: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub at: u64,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Waiting {
    pub uid: u64,
    pub mark: Mark,
    pub since: Instant,
    launched: bool,
}

#[derive(Debug, Default)]
pub struct ContinueState {
    pub marks: HashMap<u64, Mark>,

    pub waiting: Vec<Waiting>,

    pub note: Option<(String, Instant)>,

    working: HashSet<u64>,
    prompts: HashMap<u64, String>,
    last_look: Option<Instant>,
    last_refresh: Option<Instant>,
}

impl ContinueState {
    pub fn read_mark(&mut self, uid: u64, t: &TabState) {
        if t.was_working {
            if let Some(at) = t.working_at {
                self.marks.insert(
                    uid,
                    Mark {
                        at,
                        prompt: t.last_prompt.clone(),
                    },
                );
            }
        }
    }

    pub fn note_text(&self) -> Option<&str> {
        self.note
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(1800))
            .map(|(s, _)| s.as_str())
    }
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn background_busy(screen: &str) -> bool {
    if crate::activity::screen_agents(screen).count() > 0 {
        return true;
    }
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(4)..];
    tail.iter().any(|l| {
        let l = l.to_lowercase();
        let words: Vec<&str> = l
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        words.windows(2).any(|w| {
            w[0].parse::<u32>().is_ok_and(|n| n > 0)
                && matches!(
                    w[1],
                    "background" | "shell" | "shells" | "agent" | "agents" | "subagents"
                )
        })
    })
}

pub fn is_working(activity: Activity, running: bool, screen: &str) -> bool {
    if !running {
        return false;
    }
    match activity {
        Activity::Working | Activity::Permission => true,
        Activity::Ready => background_busy(screen),
        _ => false,
    }
}

fn tab_working(t: &Pane) -> bool {
    if !t.is_running() {
        return false;
    }
    match t.activity {
        Activity::Working | Activity::Permission => true,
        Activity::Ready => {
            let screen = t
                .parser
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .screen()
                .contents();
            is_working(t.activity, true, &screen)
        }
        _ => false,
    }
}

impl App {
    pub fn working_mark(&self, t: &Pane) -> TabState {
        if tab_working(t) {
            return TabState {
                was_working: true,
                working_at: Some(now_s()),
                last_prompt: self.cont.prompts.get(&t.uid).cloned(),
                ..Default::default()
            };
        }

        if let Some(w) = self.cont.waiting.iter().find(|w| w.uid == t.uid) {
            return TabState {
                was_working: true,
                working_at: Some(w.mark.at),
                last_prompt: w.mark.prompt.clone(),
                ..Default::default()
            };
        }
        TabState::default()
    }

    pub fn watch_working(&mut self) {
        if self
            .cont
            .last_look
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.cont.last_look = Some(Instant::now());
        let mut now = HashSet::new();
        let mut fresh = vec![];
        for (s, slot) in self.panes.iter().enumerate() {
            for (t, tab) in slot.tabs.iter().enumerate() {
                if !tab.transient && tab_working(tab) {
                    now.insert(tab.uid);
                    if !self.cont.working.contains(&tab.uid) {
                        fresh.push((s, t, tab.uid));
                    }
                }
            }
        }
        for (s, t, uid) in fresh {
            if let Some(p) = self.last_user_text(s, t) {
                let p: String = p.chars().take(500).collect();
                self.cont.prompts.insert(uid, p);
            }
        }
        if now != self.cont.working {
            self.cont.working = now;
            self.state_dirty = true;
            self.cont.last_refresh = Some(Instant::now());
        } else if !now.is_empty()
            && self
                .cont
                .last_refresh
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
        {
            self.state_dirty = true;
            self.cont.last_refresh = Some(Instant::now());
        }
        let working = &self.cont.working;
        self.cont.prompts.retain(|u, _| working.contains(u));
    }

    pub fn plan_continue(&mut self) {
        let marks = std::mem::take(&mut self.cont.marks);
        if marks.is_empty() {
            return;
        }
        if !self.cfg.restore_continue_working {
            crate::log::info(&format!(
                "continue: restore_continue_working is off; {} working tab{} resume without a prompt",
                marks.len(),
                if marks.len() == 1 { "" } else { "s" }
            ));
            return;
        }
        let now = now_s();
        let mut names = vec![];
        for slot in &self.panes {
            for tab in &slot.tabs {
                let id = tab_id(tab.uid);
                let Some(m) = marks.get(&tab.uid) else {
                    crate::log::info(&format!("continue: {id} idle: no prompt"));
                    continue;
                };
                let age = now.saturating_sub(m.at);
                if age > MARK_MAX_AGE_S {
                    crate::log::info(&format!(
                        "continue: {id} was working {age} s before the restart, too long ago: no prompt"
                    ));
                    continue;
                }
                if tab.session_id.is_none() {
                    crate::log::info(&format!(
                        "continue: {id} was working but has no session to resume: no prompt"
                    ));
                    continue;
                }
                crate::log::info(&format!(
                    "continue {id} (was working {age} s before the restart){}",
                    m.prompt
                        .as_ref()
                        .map(|p| format!(
                            "; last prompt: {}",
                            p.chars().take(80).collect::<String>()
                        ))
                        .unwrap_or_default()
                ));
                names.push(tab.uid);
                self.cont.waiting.push(Waiting {
                    uid: tab.uid,
                    mark: m.clone(),
                    since: Instant::now(),
                    launched: false,
                });
            }
        }
        if !names.is_empty() {
            let uids = names;
            let names: Vec<String> = uids.iter().map(|&u| self.spoken_tab_name(u)).collect();
            let text = format!(
                "Restarted; continued {} working tab{}: {}",
                names.len(),
                if names.len() == 1 { "" } else { "s" },
                names.join(", ")
            );
            let src = crate::told::Source {
                tab: uids.first().copied().filter(|_| uids.len() == 1),
                account: None,
                fact: Some(format!(
                    "working before the restart: {}",
                    uids.iter()
                        .map(|&u| tab_id(u))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            };
            self.told_from("restart", src, |app| {
                app.told_record("restart", &text, crate::told::Channel::Panel, false)
            });
            self.cont.note = Some((text, Instant::now()));
        }
    }

    pub fn continue_tick(&mut self) {
        self.watch_working();
        if self.cont.waiting.is_empty() {
            return;
        }
        let ws = std::mem::take(&mut self.cont.waiting);
        let mut keep = vec![];
        for mut w in ws {
            let id = tab_id(w.uid);
            let Some((s, t)) = self.find_tab(w.uid) else {
                crate::log::info(&format!("continue: {id} was closed: no prompt"));
                continue;
            };
            let tab = &self.panes[s].tabs[t];
            if !tab.is_running() {
                let logged_in = self.panes[s]
                    .account
                    .and_then(|a| self.accounts.get(a))
                    .is_some_and(|a| a.login.logged_in());
                if tab.pending.is_some() && !w.launched && logged_in {
                    let kind = tab.pending.clone().unwrap();
                    w.launched = true;
                    self.launch_tab(s, t, kind);
                } else if tab.pending.is_none() && !tab.suspended {
                    crate::log::info(&format!("continue: {id} is not running: no prompt"));
                    continue;
                }
            } else {
                match tab.activity {
                    Activity::Ready if tab.activity_since.elapsed() >= SETTLE => {
                        crate::log::info(&format!("continue: sending {id} the continue prompt"));
                        self.deliver(w.uid, CONTINUE_PROMPT);
                        continue;
                    }
                    Activity::Working => {
                        crate::log::info(&format!(
                            "continue: {id} is already working again: no prompt"
                        ));
                        continue;
                    }
                    Activity::Exited => {
                        crate::log::info(&format!("continue: {id} exited: no prompt"));
                        continue;
                    }
                    _ => {}
                }
            }
            if w.since.elapsed() >= GIVE_UP {
                crate::log::info(&format!("continue: {id} never got ready: no prompt"));
                continue;
            }
            keep.push(w);
        }
        keep.append(&mut self.cont.waiting);
        self.cont.waiting = keep;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_vs_idle_vs_approval_vs_alive() {
        let spin = "✻ Pondering… (12s · esc to interrupt)\n> \n";
        let idle = "> \n? for shortcuts\n";
        let ask = "Do you want to proceed?\n❯ 1. Yes\n  2. No\n";
        assert!(is_working(crate::pane::detect_activity(spin), true, spin));
        assert!(!is_working(crate::pane::detect_activity(idle), true, idle));

        assert!(is_working(crate::pane::detect_activity(ask), true, ask));

        assert!(!is_working(Activity::Starting, true, "loading"));
        assert!(!is_working(Activity::Working, false, spin));
        assert!(!is_working(Activity::Exited, false, ""));

        let bg = "> \n  ⏵⏵ bypass permissions on · 2 background tasks\n";
        assert!(is_working(Activity::Ready, true, bg));
        let shell = "> \n  1 shell · ? for shortcuts\n";
        assert!(is_working(Activity::Ready, true, shell));
        assert!(!is_working(
            Activity::Ready,
            true,
            "> \n  0 background tasks · ? for shortcuts\n"
        ));
        assert!(is_working(Activity::Permission, true, bg));
    }

    fn app(tag: &str) -> (App, std::path::PathBuf) {
        let home =
            std::env::temp_dir().join(format!("godterm-continue-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        crate::config::testing::set_home(&home);
        let (tx, _rx) = std::sync::mpsc::channel();
        (App::new(crate::config::Config::default(), tx), home)
    }

    fn saved(app: &App, marks: &[(u64, bool, Option<u64>)]) -> crate::state::AppState {
        let mut st = app.snapshot_state();
        let tabs = &mut st.slots[0].tabs;
        tabs.clear();
        for (uid, w, at) in marks {
            tabs.push(TabState {
                cwd: "/".into(),
                session_id: Some(format!("s{uid}")),
                uid: Some(*uid),
                was_working: *w,
                working_at: *at,
                last_prompt: w.then(|| "build it".to_string()),
                ..Default::default()
            });
        }
        st
    }

    #[test]
    fn marks_restore_plan_and_persist() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut a, home) = app("plan");
        let now = now_s();
        let st = saved(
            &a,
            &[
                (501, true, Some(now - 40)),
                (502, false, None),
                (503, true, Some(now - 3 * 86400)),
            ],
        );
        a.restore(st);
        assert_eq!(a.cont.marks.len(), 2);
        a.plan_continue();
        let uids: Vec<u64> = a.cont.waiting.iter().map(|w| w.uid).collect();
        assert_eq!(uids, vec![501], "only the recent working tab");
        assert_eq!(a.cont.waiting[0].mark.prompt.as_deref(), Some("build it"));
        assert!(a
            .cont
            .note_text()
            .unwrap()
            .contains("continued 1 working tab"));

        let again = a.snapshot_state();
        let t: Vec<&TabState> = again.slots[0]
            .tabs
            .iter()
            .filter(|t| t.was_working)
            .collect();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].uid, Some(501));
        assert_eq!(t[0].working_at, Some(now - 40));

        again.save().unwrap();
        let back = crate::state::AppState::load().unwrap();
        assert!(back.slots[0]
            .tabs
            .iter()
            .any(|t| t.was_working && t.uid == Some(501)));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn setting_off_plans_nothing() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut a, home) = app("off");
        a.cfg.restore_continue_working = false;
        let st = saved(&a, &[(601, true, Some(now_s() - 5))]);
        a.restore(st);
        a.plan_continue();
        assert!(a.cont.waiting.is_empty());
        assert!(a.cont.note.is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn not_ready_gets_no_prompt_yet() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut a, home) = app("notready");
        let st = saved(&a, &[(701, true, Some(now_s() - 5))]);
        a.restore(st);
        a.plan_continue();

        for _ in 0..3 {
            a.continue_tick();
        }
        assert_eq!(a.cont.waiting.len(), 1);
        assert!(
            a.deliveries.is_empty(),
            "nothing typed before the prompt is ready"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
