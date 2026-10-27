use std::collections::HashMap;
use std::time::Instant;

use serde_json::{json, Value};

use crate::app::App;
use crate::control::{plural, tab_id};
use crate::pane::Activity;

pub const FINISHED_WINDOW_MIN: u64 = 30;

#[derive(Debug, Clone, Default)]
pub struct WorkSeen {
    pub at: Option<Instant>,

    pub tabs: HashMap<u64, (String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    NeedsYou,
    Working,
    Finished,
    Idle,
    Off,
}

impl Group {
    pub fn key(self) -> &'static str {
        match self {
            Group::NeedsYou => "needs_you",
            Group::Working => "working",
            Group::Finished => "finished",
            Group::Idle => "idle",
            Group::Off => "off",
        }
    }
}

pub fn screen_last_line(screen: &str) -> Option<String> {
    screen
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find(|l| {
            let low = l.to_lowercase();
            !(l.starts_with('❯')
                || l.starts_with('>')
                || l.starts_with('─')
                || l.starts_with('│')
                || l.starts_with('╭')
                || l.starts_with('╰')
                || l.starts_with('┃')
                || low.contains("for shortcuts")
                || low.contains("esc to interrupt")
                || low.contains("to interrupt")
                || low.contains("accept edits")
                || low.contains("bypass permissions")
                || low.starts_with("tokens")
                || low.starts_with("context"))
        })
        .map(|l| crate::sessions::snippet(l, 160))
}

fn asks_user(text: &str) -> bool {
    text.trim()
        .trim_end_matches(['*', '`', ')', '"', '\u{201d}', ' '])
        .ends_with('?')
}

fn sentence(s: String) -> String {
    if s.ends_with(['.', '!', '?']) {
        s
    } else {
        s + "."
    }
}

fn hm(at: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
}

impl App {
    pub fn work_status_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        if tool != "work_status" {
            return None;
        }
        Some(Ok(
            json!({"ok": true, "result": self.work_status_json(args)}),
        ))
    }

    pub(crate) fn work_entry(&self, s: usize, t: usize) -> (Group, Value) {
        let slot = &self.panes[s];
        let tab = &slot.tabs[t];
        let uid = tab.uid;
        let act = self.tab_activity(s, t);
        let transcript = self.transcript_path(s, t);
        let tail = transcript.as_deref().and_then(crate::sessions::tail_status);
        let last_write_min = act.last_write_s.map(|x| x / 60);
        let since_activity_min = {
            let a = tab.activity_since.elapsed().as_secs() / 60;
            last_write_min.map_or(a, |w| w.min(a))
        };
        let label = self.tab_label(s, t);
        let bg = act.background() > 0;
        let running = tab.is_running();

        let mut needs: Option<String> = None;
        if tab.activity == Activity::Permission {
            let what = self
                .request_of(s, t)
                .map(|r| r.summary())
                .unwrap_or_else(|| "an approval".into());
            needs = Some(format!(
                "approval: {}",
                crate::sessions::snippet(&what, 120)
            ));
        } else if running && self.limit_hit.contains(&uid) {
            needs = Some("usage limit hit: move it or wait for the reset".into());
        } else if let Some(a) = slot
            .account
            .filter(|&a| !self.accounts[a].login.logged_in())
        {
            needs = Some(format!("a login: account {} is logged out", a + 1));
        } else if let Some(l) = label.filter(|l| {
            matches!(
                l,
                crate::tab_labels::TabLabel::Confirm
                    | crate::tab_labels::TabLabel::Error
                    | crate::tab_labels::TabLabel::Blocked
            )
        }) {
            needs = Some(match l {
                crate::tab_labels::TabLabel::Confirm => "a yes to a question GodTerm asked".into(),
                crate::tab_labels::TabLabel::Blocked => "its resume is blocked (fix_resume)".into(),
                _ => "it hit an error (read_tab screen)".into(),
            });
        } else if running
            && matches!(tab.activity, Activity::Ready)
            && !bg
            && tail
                .as_ref()
                .is_some_and(|x| x.finished && x.last_text.as_deref().is_some_and(asks_user))
        {
            needs = Some("an answer: its last message is a question to you".into());
        }

        let claude_like = transcript.is_some();
        let (doing, in_tool, doing_at) = match &tail {
            Some(x) => (
                x.last_text
                    .as_deref()
                    .map(|t| crate::sessions::first_sentences(t.trim(), 220)),
                x.tool.as_ref().map(|name| match &x.tool_arg {
                    Some(a) => format!("{name}: {a}"),
                    None => name.clone(),
                }),
                x.last_at.as_deref().and_then(hm),
            ),
            None => (None, None, None),
        };
        let screen_line = if running && (!claude_like || doing.is_none()) {
            screen_last_line(&tab.screen_text())
        } else {
            None
        };

        let group = if !running || matches!(tab.activity, Activity::Exited | Activity::Idle) {
            Group::Off
        } else if needs.is_some() {
            Group::NeedsYou
        } else if matches!(tab.activity, Activity::Working | Activity::Starting) || bg {
            Group::Working
        } else if since_activity_min < FINISHED_WINDOW_MIN {
            Group::Finished
        } else {
            Group::Idle
        };

        let jobs: Vec<String> = self
            .jobs
            .jobs
            .iter()
            .filter(|j| j.target.uid == Some(uid) && j.active())
            .map(|j| j.name.clone())
            .collect();
        let loops = self.loops_in(uid);
        let bg_sessions = self.tab_background_runs(uid);
        let asked: Vec<String> = self
            .assistant
            .follow_ups
            .iter()
            .filter(|f| f.uid == uid && f.state == crate::app_followup::FuState::Waiting)
            .map(|f| crate::sessions::snippet(&f.question, 80))
            .collect();

        let mut v = json!({
            "id": tab_id(uid),
            "name": tab.name(),
            "folder": crate::config::tilde(&tab.cwd),
            "account": slot.account.map(|a| a + 1),
            "account_label": slot.account.map(|a| self.cfg.accounts[a].display().to_string()),
            "group": group.key(),
            "state": self.state_word(s, t),
            "minutes_since_activity": since_activity_min,
        });
        if let Some(n) = &needs {
            v["needs"] = json!(n);
        }
        if act.subagents_running > 0 {
            v["working_through"] = json!(act.subagents_line());
            v["subagents"] = json!(act
                .subagents
                .iter()
                .map(|a| {
                    let mut e = json!({"age": crate::sessions::rel_secs(a.age_s)});
                    if let Some(n) = &a.name {
                        e["name"] = json!(n);
                    }
                    if let Some(t) = &a.task {
                        e["task"] = json!(t);
                    }
                    if let Some(t) = &a.tool {
                        e["in_tool"] = json!(t);
                    }
                    if a.idle_s >= 60 {
                        e["quiet_for"] = json!(crate::sessions::rel_secs(a.idle_s));
                    }
                    e
                })
                .collect::<Vec<_>>());
        }
        if act.background_shells > 0 {
            v["background_shells"] = json!(act.background_shells);
        }
        if act.background_tasks > 0 {
            v["background_tasks"] = json!(act.background_tasks);
        }
        if let Some(d) = &doing {
            v["last_line"] = json!(d);
        }
        if let Some(at) = &doing_at {
            v["last_line_at"] = json!(at);
        }
        if let Some(t) = &in_tool {
            if tab.activity == Activity::Working || bg {
                v["in_tool"] = json!(t);
            }
        }
        if let Some(l) = &screen_line {
            v["screen_line"] = json!(l);
        }
        if let Some(u) = tail.as_ref().and_then(|x| x.last_user.as_deref()) {
            v["last_prompt"] = json!(crate::sessions::snippet(u, 120));
        }
        if let Some(l) = label {
            v["label"] = json!(l.word());
        }
        if !jobs.is_empty() {
            v["jobs"] = json!(jobs);
        }
        if loops > 0 {
            v["loops"] = json!(loops);
        }
        if !bg_sessions.is_empty() {
            v["background_sessions"] = json!(bg_sessions.len());
        }
        let queued = self.queued_for(uid);
        if queued > 0 {
            v["queued_prompts"] = json!(queued);
        }
        if !asked.is_empty() {
            v["awaiting_its_answer_to"] = json!(asked);
        }
        if let Some(m) = self.tab_model_label(s, t) {
            v["model"] = json!(m);
        }
        (group, v)
    }

    fn work_fingerprint(v: &Value) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            v["group"].as_str().unwrap_or(""),
            v["needs"].as_str().unwrap_or(""),
            v["working_through"].as_str().unwrap_or(""),
            v["last_line"].as_str().unwrap_or(""),
            v["in_tool"].as_str().unwrap_or(""),
        )
    }

    pub fn work_status_json(&mut self, args: &Value) -> Value {
        let since_last = args.get("since").and_then(Value::as_str) == Some("last");
        let since_min: Option<u64> = match args.get("since") {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => {
                let t = s
                    .trim()
                    .trim_end_matches(['m', ' '])
                    .trim_end_matches("min");
                t.trim().parse().ok()
            }
            _ => None,
        };
        let only: Option<Vec<(usize, usize)>> = args
            .get("tab")
            .filter(|v| !v.is_null())
            .and_then(|_| self.tab_targets(args, false).ok());
        let mut groups: Vec<(Group, Value)> = vec![];
        let tabs = only.unwrap_or_else(|| self.all_tabs());
        for (s, t) in tabs {
            groups.push(self.work_entry(s, t));
        }

        groups.sort_by(|a, b| {
            a.0.cmp(&b.0).then_with(|| {
                let m = |v: &Value| v["minutes_since_activity"].as_u64().unwrap_or(0);
                m(&a.1).cmp(&m(&b.1))
            })
        });

        let prev = std::mem::take(&mut self.assistant.work_seen);
        let mut seen = WorkSeen {
            at: Some(Instant::now()),
            tabs: HashMap::new(),
        };
        let mut changed: Vec<String> = vec![];
        let mut unchanged = 0usize;
        for (g, v) in &mut groups {
            let uid = v["id"]
                .as_str()
                .and_then(|i| i.trim_start_matches('t').parse::<u64>().ok())
                .unwrap_or(0);
            let fp = Self::work_fingerprint(v);
            let was = prev.tabs.get(&uid);
            let is_changed = if since_last {
                was.is_none_or(|(_, f)| *f != fp)
            } else if let Some(m) = since_min {
                v["minutes_since_activity"].as_u64().unwrap_or(u64::MAX) < m
            } else {
                false
            };
            if since_last || since_min.is_some() {
                v["changed"] = json!(is_changed);
                if is_changed {
                    changed.push(v["id"].as_str().unwrap_or("").to_string());
                } else {
                    unchanged += 1;
                }
                if let Some((g0, _)) = was.filter(|_| since_last) {
                    if g0 != g.key() {
                        v["was"] = json!(g0);
                    }
                }
            }
            seen.tabs.insert(uid, (g.key().to_string(), fp));
        }
        let ago = prev.at.map(|t| t.elapsed().as_secs());
        self.assistant.work_seen = seen;

        let mut out = json!({});
        let mut counts: Vec<String> = vec![];
        for g in [
            Group::NeedsYou,
            Group::Working,
            Group::Finished,
            Group::Idle,
        ] {
            let items: Vec<Value> = groups
                .iter()
                .filter(|(x, _)| *x == g)
                .map(|(_, v)| v.clone())
                .collect();
            if !items.is_empty() {
                let n = items.len();
                counts.push(match g {
                    Group::NeedsYou => format!("{n} need you"),
                    Group::Working => {
                        let via = items
                            .iter()
                            .filter(|v| v.get("working_through").is_some())
                            .count();
                        if via > 0 {
                            format!("{n} working ({via} through agents)")
                        } else {
                            format!("{n} working")
                        }
                    }
                    Group::Finished => {
                        format!("{n} finished in the last {FINISHED_WINDOW_MIN} min")
                    }
                    Group::Idle => format!("{n} idle"),
                    Group::Off => String::new(),
                });
            }
            out[g.key()] = json!(items);
        }
        let off: Vec<Value> = groups
            .iter()
            .filter(|(x, _)| *x == Group::Off)
            .map(|(_, v)| json!({"id": v["id"], "name": v["name"], "state": v["state"]}))
            .collect();
        if !off.is_empty() {
            counts.push(format!("{} off", off.len()));
            out["off"] = json!(off);
        }

        let runs: Vec<Value> = self
            .busy
            .runs
            .iter()
            .filter(|r| r.outcome == crate::app_busy::Outcome::Going)
            .map(|r| {
                let task = self
                    .deliveries
                    .iter()
                    .find(|d| d.id == r.delivery)
                    .map(|d| crate::sessions::snippet(&d.text, 80))
                    .unwrap_or_default();
                json!({"for_tab": tab_id(self.current_uid(r.uid)), "mode": format!("{:?}", r.mode()).to_lowercase(), "task": task})
            })
            .collect();
        if !runs.is_empty() {
            out["assistant_background_sessions"] = json!(runs);
        }
        out["summary"] = json!(counts.join(", "));
        out["order"] = json!("Tell needs_you first, then working, then finished; idle last or not at all. A tab with working_through is working through those agents: never call it idle.");
        if since_last || since_min.is_some() {
            out["since"] = json!({
                "changed": changed,
                "unchanged": unchanged,
                "previous_call_s_ago": ago,
                "note": if since_last && ago.is_none() { "first call: everything counts as changed" } else { "changed = its group, need, agents or last line differ" },
            });
        }
        out
    }

    pub fn work_digest(status: &Value, skip: usize, max: usize) -> (Vec<String>, usize) {
        let mut all: Vec<String> = vec![];
        let name = |v: &Value| {
            v["name"]
                .as_str()
                .unwrap_or("a tab")
                .replace(['-', '_'], " ")
        };
        for v in status["needs_you"].as_array().into_iter().flatten() {
            all.push(format!(
                "{} needs you: {}.",
                name(v),
                v["needs"].as_str().unwrap_or("an answer")
            ));
        }
        for v in status["working"].as_array().into_iter().flatten() {
            let via = v["working_through"]
                .as_str()
                .map(|w| format!(" through {w}"))
                .unwrap_or_default();
            let doing = v["in_tool"]
                .as_str()
                .map(|t| format!(", in {t}"))
                .or_else(|| v["last_line"].as_str().map(|l| format!(": {l}")))
                .or_else(|| v["screen_line"].as_str().map(|l| format!(": {l}")))
                .unwrap_or_default();
            all.push(sentence(format!("{} is working{via}{doing}", name(v))));
        }
        for v in status["finished"].as_array().into_iter().flatten() {
            let m = v["minutes_since_activity"].as_u64().unwrap_or(0);
            let line = v["last_line"]
                .as_str()
                .map(|l| format!(": {l}"))
                .unwrap_or_default();
            all.push(sentence(format!(
                "{} finished {} ago{line}",
                name(v),
                plural(m as usize, "minute")
            )));
        }
        let rest = all.len().saturating_sub(skip + max);
        let out: Vec<String> = all.into_iter().skip(skip).take(max).collect();
        (out, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_last_line_skips_the_prompt_box() {
        let s = "⏺ Running cargo test\n  ⎿  Running 12 tests\n\n───────\n❯ \n───────\n  ? for shortcuts\n";
        assert_eq!(screen_last_line(s).as_deref(), Some("⎿ Running 12 tests"));
        assert_eq!(screen_last_line("❯ \n"), None);
    }

    #[test]
    fn the_digest_orders_needs_working_finished_and_continues() {
        let st = json!({
            "needs_you": [{"name": "hyper", "needs": "approval: Bash(rm -rf build)"}],
            "working": [
                {"name": "godterm", "working_through": "3 subagents (general-purpose (36 min))", "last_line": "Waiting for the agents."},
                {"name": "plant-growth", "in_tool": "Bash: ffmpeg -i mimosa.mp4"},
                {"name": "notes", "screen_line": "Thinking..."},
            ],
            "finished": [{"name": "scout", "minutes_since_activity": 12, "last_line": "Done: the rm was skipped."}],
            "idle": [{"name": "old"}],
        });
        let (first, rest) = App::work_digest(&st, 0, 3);
        assert_eq!(
            first,
            vec![
                "hyper needs you: approval: Bash(rm -rf build).",
                "godterm is working through 3 subagents (general-purpose (36 min)): Waiting for the agents.",
                "plant growth is working, in Bash: ffmpeg -i mimosa.mp4.",
            ]
        );
        assert_eq!(rest, 2);
        let (more, rest) = App::work_digest(&st, 3, 3);
        assert_eq!(
            more,
            vec![
                "notes is working: Thinking...",
                "scout finished 12 minutes ago: Done: the rm was skipped.",
            ]
        );
        assert_eq!(rest, 0);
        assert!(asks_user("Should I go ahead with the rotation?"));
        assert!(asks_user("Ready to rotate the keys. **Shall I?**"));
        assert!(!asks_user("Done: rotated the keys."));
    }
}
