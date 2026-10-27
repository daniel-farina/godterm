use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::{App, View};
use crate::pane::Activity;
use crate::updates::{self, Cursor, Facts, Phrase, Said};

pub const EXPLAIN: &str = "Update mode: I keep you posted on every active project, a sentence or two each: what finished, what needs your approval, errors, steps done. Big news comes at once (at most every minute), the rest in a roundup every few minutes, and only when something changed. Say 'stop updates' to end it, or 'what's the latest' any time.";

pub const EXPLAIN_SPOKEN: &str =
    "Update mode. I'll tell you what changes in each active project. Say stop updates to end it.";

const SCAN_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct Track {
    pub path: Option<PathBuf>,
    pub cur: Cursor,
    pub act: Activity,
    pub bg_wait: bool,

    pub todo: Option<updates::Todo>,
}

#[derive(Default)]
pub struct UpdatesState {
    pub card: bool,
    pub tabs: HashMap<u64, Track>,

    pub pending: BTreeMap<u64, (String, Facts)>,

    pub announced: Vec<(u64, bool)>,

    pub jobs_seen: HashMap<String, i64>,
    pub jobs_primed: bool,
    pub last_said: Option<Instant>,
    pub last_round: Option<Instant>,
    pub scanned: Option<Instant>,

    pub active: usize,

    pub last_line: HashMap<u64, String>,

    pub said: Vec<String>,

    pub clock: Option<Instant>,
}

fn key_of(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish() | (1 << 63)
}

impl App {
    pub fn updates_on(&self) -> bool {
        self.cfg.voice.updates
    }

    pub fn updates_now(&self) -> Instant {
        self.updates.clock.unwrap_or_else(Instant::now)
    }

    fn updates_gap(&self) -> Duration {
        Duration::from_secs(self.cfg.voice.updates_gap_s.max(10) as u64)
    }

    fn updates_every(&self) -> Duration {
        Duration::from_secs(self.cfg.voice.updates_every_min.max(1) as u64 * 60)
    }

    pub fn updates_held(&self) -> Option<&'static str> {
        if self.paused() {
            Some("paused")
        } else if self.brainstorm_on() {
            Some("brainstorm")
        } else if self.direct.on.is_some() {
            Some("direct mode")
        } else {
            None
        }
    }

    pub fn updates_spoken(&self) -> bool {
        self.tts_on()
            && !self.cfg.voice.speaker_muted
            && !self.voice.muted
            && !self.voice.asleep
            && (self.voice.engine.is_some() || self.voice.preview.is_some() || self.cfg.voice.tts)
    }

    pub fn updates_chip(&self) -> Option<String> {
        if !self.updates_on() {
            return None;
        }
        let n = self.updates.active;
        let projects = if n == 1 {
            "1 project".to_string()
        } else {
            format!("{n} projects")
        };
        let when = match self.updates_held() {
            Some(why) => format!("waiting ({why})"),
            None => {
                let now = self.updates_now();
                let last = self.updates.last_round.unwrap_or(now);
                format!(
                    "next in {}",
                    updates::short_span(updates::next_in(now, last, self.updates_every()))
                )
            }
        };
        Some(format!("● updates · {projects} · {when}"))
    }

    pub fn toggle_update_mode(&mut self) {
        let on = !self.updates_on();
        let say = self.set_update_mode(on, true);
        self.flash(say);
    }

    pub fn set_update_mode(&mut self, on: bool, speak: bool) -> String {
        if on == self.updates_on() {
            return if on {
                "Update mode is already on. Say stop updates to end it.".into()
            } else {
                "Update mode was not on.".into()
            };
        }
        crate::log::info(&format!("updates: {}", if on { "on" } else { "off" }));
        self.cfg.voice.updates = on;
        if !cfg!(test) {
            let _ = crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global("voice.updates"),
                Some(toml_edit::value(on)),
            );
            self.config_mtime = crate::app::config_mtime();
        }
        self.updates.pending.clear();
        self.updates.announced.clear();
        self.updates.last_line.clear();
        if !on {
            self.updates.tabs.clear();
            self.updates.card = false;
            let line = "Update mode off.".to_string();
            if speak {
                self.speak(&line);
            }
            return line;
        }

        self.updates.tabs.clear();
        self.updates.jobs_primed = false;
        self.updates_scan(false);
        self.updates.pending.clear();
        let now = self.updates_now();
        self.updates.last_round = Some(now);
        self.updates.last_said = None;
        self.updates.scanned = Some(now);
        let first = !self.cfg.voice.updates_explained;
        if first {
            self.updates.card = true;
            self.assistant.show = true;
            self.cfg.voice.updates_explained = true;
            if !cfg!(test) {
                let _ = crate::settings::write(
                    &crate::config::Config::path(),
                    &crate::settings::Key::Global("voice.updates_explained"),
                    Some(toml_edit::value(true)),
                );
                self.config_mtime = crate::app::config_mtime();
            }
        }
        let names = self.updates_active_names();
        let line = match names.len() {
            0 => {
                "Update mode on. Nothing is running right now; I'll tell you when something moves."
                    .to_string()
            }
            1 => format!("Update mode on. I'll keep you posted on {}.", names[0]),
            n if n <= 4 => format!(
                "Update mode on. I'll keep you posted on {} projects: {}.",
                updates::number(n),
                join_names(&names)
            ),
            n => format!(
                "Update mode on. I'll keep you posted on {} active projects.",
                updates::number(n)
            ),
        };
        if speak {
            self.speak(&line);
        }
        line
    }

    fn updates_active_names(&self) -> Vec<String> {
        let mut v = vec![];
        for s in 0..self.panes.len() {
            for t in 0..self.panes[s].tabs.len() {
                if self.updates_tab_active(s, t) {
                    let n = self.spoken_tab_name(self.panes[s].tabs[t].uid);
                    if !v.contains(&n) {
                        v.push(n);
                    }
                }
            }
        }
        v
    }

    fn updates_tab_active(&self, s: usize, t: usize) -> bool {
        let tab = &self.panes[s].tabs[t];
        if matches!(tab.activity, Activity::Working | Activity::Permission) || tab.bg_wait {
            return true;
        }

        if self.tab_background_work(s, t).is_some() {
            return true;
        }
        if self.updates.pending.contains_key(&tab.uid) {
            return true;
        }
        self.pane_harness(s) == Some(crate::harness::Harness::Claude)
            && self.transcript_path(s, t).is_some_and(|p| {
                crate::activity::running_subagents(&p, std::time::SystemTime::now()) > 0
            })
    }

    fn updates_in_view(&self, s: usize, t: usize) -> bool {
        self.voice.term_focused == Some(true)
            && s == self.focus
            && t == self.panes[s].active
            && self.view == View::Grid
    }

    pub fn updates_scan(&mut self, ask: bool) {
        let mut seen: Vec<u64> = vec![];
        let mut active = 0;
        for s in 0..self.panes.len() {
            let claude = self.pane_harness(s) == Some(crate::harness::Harness::Claude);
            for t in 0..self.panes[s].tabs.len() {
                let (uid, act, bg) = {
                    let tab = &self.panes[s].tabs[t];
                    (tab.uid, tab.activity, tab.bg_wait)
                };
                seen.push(uid);
                let path = if claude {
                    self.transcript_path(s, t)
                } else {
                    None
                };
                let mut facts = Facts::default();
                match self.updates.tabs.get_mut(&uid) {
                    None => {
                        let offset = path
                            .as_deref()
                            .map(|p| updates::start_offset(p, ask))
                            .unwrap_or(0);
                        let mut tr = Track {
                            path: path.clone(),
                            cur: Cursor {
                                offset,
                                ..Default::default()
                            },
                            act,
                            bg_wait: bg,
                            todo: None,
                        };
                        if ask {
                            if let Some(p) = &path {
                                facts.merge(updates::read_new(p, &mut tr.cur));
                            }
                        }
                        self.updates.tabs.insert(uid, tr);
                    }
                    Some(tr) => {
                        facts.merge(updates::screen_facts(tr.act, act, bg));

                        if tr.bg_wait && !bg && act == Activity::Ready {
                            facts.finished = true;
                        }
                        tr.act = act;
                        tr.bg_wait = bg;
                        if path != tr.path {
                            tr.cur = Cursor {
                                offset: path.as_deref().map(updates::switched_offset).unwrap_or(0),
                                ..Default::default()
                            };
                            tr.path = path.clone();
                        }
                        if let Some(p) = &path {
                            facts.merge(updates::read_new(p, &mut tr.cur));
                        }
                    }
                }
                if let Some(td) = &facts.todo {
                    if let Some(tr) = self.updates.tabs.get_mut(&uid) {
                        tr.todo = Some(td.clone());
                    }
                }

                let skip = self.direct_with(uid) || self.updates_in_view(s, t);
                if !skip && !facts.is_empty() {
                    let name = self.spoken_tab_name(uid);
                    let e = self
                        .updates
                        .pending
                        .entry(uid)
                        .or_insert_with(|| (name.clone(), Facts::default()));
                    e.0 = name;
                    e.1.merge(facts);
                }
                if self.updates_tab_active(s, t) {
                    active += 1;
                }
            }
        }

        self.updates.tabs.retain(|u, _| seen.contains(u));
        self.updates_scan_jobs();
        self.updates.active = active;
    }

    fn updates_scan_jobs(&mut self) {
        let primed = self.updates.jobs_primed;
        let mut news: Vec<(u64, String, String)> = vec![];
        for j in &self.jobs.jobs {
            let Some(r) = &j.last_result else {
                continue;
            };
            if !r.is_final() {
                continue;
            }
            let prev = self.updates.jobs_seen.insert(j.id.clone(), r.at);
            if !primed || prev == Some(r.at) || r.state != "done" {
                continue;
            }
            let (key, name) = match self.job_tab(&j.target) {
                Some((s, t)) => {
                    let uid = self.panes[s].tabs[t].uid;
                    (uid, self.spoken_tab_name(uid))
                }
                None => {
                    let label = if j.target.label.is_empty() {
                        std::path::Path::new(&j.target.cwd)
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "a project".into())
                    } else {
                        j.target.label.clone()
                    };
                    (key_of(&label), label)
                }
            };
            news.push((key, name, j.name.clone()));
        }
        self.updates.jobs_primed = true;
        for (key, name, job) in news {
            let e = self
                .updates
                .pending
                .entry(key)
                .or_insert_with(|| (name, Facts::default()));
            e.1.jobs_done.push(job);
        }
    }

    pub fn updates_note_dropped(&mut self, uid: u64) {
        if !self.updates_on() {
            return;
        }
        let name = self.spoken_tab_name(uid);
        let e = self
            .updates
            .pending
            .entry(uid)
            .or_insert_with(|| (name, Facts::default()));
        e.1.dropped += 1;
    }

    #[allow(dead_code)]
    pub fn updates_note_announced(&mut self, uid: u64, approval: bool) {
        if self.updates_on() && !self.updates.announced.contains(&(uid, approval)) {
            self.updates.announced.push((uid, approval));
        }
    }

    fn updates_lines(&self) -> Vec<(updates::Prio, String, u64)> {
        let mut out = vec![];
        for (key, (name, f)) in &self.updates.pending {
            let mut f = f.clone();
            if f.approval {
                let still = self
                    .find_tab(*key)
                    .is_some_and(|(s, t)| self.panes[s].tabs[t].activity == Activity::Permission);
                f.approval = still;
            }
            let said = Said {
                approval: self.updates.announced.contains(&(*key, true)),
                finished: self.updates.announced.contains(&(*key, false)),
            };
            let Some((p, line)) = updates::say_project(name, &f, said) else {
                continue;
            };

            if p == updates::Prio::Progress && self.updates.last_line.get(key) == Some(&line) {
                continue;
            }
            out.push((p, line, *key));
        }
        out
    }

    pub fn updates_tick(&mut self) {
        if !self.updates_on() {
            if self.updates.last_round.is_some() {
                self.updates.tabs.clear();
                self.updates.pending.clear();
                self.updates.announced.clear();
                self.updates.last_round = None;
                self.updates.jobs_primed = false;
            }
            return;
        }
        let now = self.updates_now();
        if self
            .updates
            .scanned
            .is_some_and(|t| now.saturating_duration_since(t) < SCAN_EVERY)
        {
            return;
        }
        self.updates.scanned = Some(now);
        self.updates_scan(false);
        if self.updates.last_round.is_none() {
            self.updates.last_round = Some(now);
        }
        if self.updates_held().is_some() || !self.quiet_moment() {
            return;
        }
        let lines = self.updates_lines();
        let event = lines.iter().any(|l| l.0 != updates::Prio::Progress);
        let due = updates::due(
            now,
            self.updates.last_said,
            self.updates.last_round.unwrap_or(now),
            self.updates_gap(),
            self.updates_every(),
            event,
            !lines.is_empty(),
        );
        if !due {
            if lines.is_empty()
                && self
                    .updates
                    .last_round
                    .is_some_and(|t| now.saturating_duration_since(t) >= self.updates_every())
            {
                self.updates.last_round = Some(now);
                self.updates.pending.clear();
                self.updates.announced.clear();
            }
            return;
        }
        self.updates_deliver(lines, false);
    }

    fn updates_deliver(&mut self, lines: Vec<(updates::Prio, String, u64)>, asked: bool) {
        let now = self.updates_now();
        for (_, l, k) in &lines {
            self.updates.last_line.insert(*k, l.clone());
        }
        let text = updates::batch(lines.into_iter().map(|(p, l, _)| (p, l)).collect());
        self.updates.pending.clear();
        self.updates.announced.clear();
        self.updates.last_said = Some(now);
        self.updates.last_round = Some(now);
        self.updates_out(&text, asked);
    }

    fn updates_out(&mut self, text: &str, asked: bool) {
        if text.trim().is_empty() {
            return;
        }
        crate::log::info(&format!(
            "updates: {}{text}",
            if asked { "asked: " } else { "" }
        ));
        self.updates.said.push(text.to_string());
        let n = self.updates.said.len();
        if n > 30 {
            self.updates.said.drain(..n - 30);
        }

        if !asked {
            self.update_event(text);
            return;
        }

        let at = chrono::Local::now().format("%H:%M");
        self.note(format!("Update {at}: {text}"));
        if self.updates_spoken() {
            self.told_record("update mode", text, crate::told::Channel::Spoken, false);
            self.assistant.quiet.said.push(text.to_string());
            self.speak_assistant(text);
        }
    }

    pub fn updates_roundup_now(&mut self) -> String {
        self.updates_scan(true);
        let lines = self.updates_lines();
        if !lines.is_empty() {
            let text = updates::batch(lines.iter().map(|(p, l, _)| (*p, l.clone())).collect());
            self.updates_deliver(lines, true);
            return text;
        }
        let text = self.updates_status_words();
        self.updates.pending.clear();
        self.updates.announced.clear();
        self.updates.last_said = Some(self.updates_now());
        self.updates_out(&text, true);
        text
    }

    fn updates_status_words(&self) -> String {
        let mut parts: Vec<String> = vec![];
        for s in 0..self.panes.len() {
            for t in 0..self.panes[s].tabs.len() {
                let tab = &self.panes[s].tabs[t];
                let name = self.spoken_tab_name(tab.uid);
                let todo = self
                    .updates
                    .tabs
                    .get(&tab.uid)
                    .and_then(|tr| tr.todo.clone())
                    .filter(|td| td.total > 0 && td.done < td.total)
                    .map(|td| {
                        format!(
                            ", {} of {} steps done",
                            updates::number(td.done),
                            updates::number(td.total)
                        )
                    })
                    .unwrap_or_default();
                let line = match tab.activity {
                    Activity::Permission => format!("{name} is waiting for your approval"),
                    Activity::Working => format!("{name} is still working{todo}"),
                    _ => match self.tab_background_work(s, t) {
                        Some(w) if w != "background work" => {
                            format!("{name} is busy in the background: {w}")
                        }
                        Some(_) => format!("{name} has background work running"),
                        None => continue,
                    },
                };
                if !parts.contains(&line) {
                    parts.push(line);
                }
            }
        }
        let lead = if self.updates_on() || self.updates.last_said.is_some() {
            "No news since the last update."
        } else {
            "Nothing new."
        };
        match parts.len() {
            0 => format!("{lead} Nothing is running right now."),
            _ => {
                let mut s = format!("{lead} ");
                let shown: Vec<String> =
                    parts.iter().take(updates::MAX_PROJECTS).cloned().collect();
                s.push_str(&capitalize(&join_names(&shown)));
                s.push('.');
                s
            }
        }
    }

    pub fn updates_intercept(&mut self, command: &str) -> bool {
        let Some(p) = updates::parse_phrase(command) else {
            return false;
        };

        if self.direct.on.is_some() && matches!(p, Phrase::Now | Phrase::Quiet) {
            return false;
        }
        crate::log::info(&format!("updates: phrase {p:?}: {command}"));
        match p {
            Phrase::On => {
                self.set_update_mode(true, true);
            }
            Phrase::Off => {
                self.set_update_mode(false, true);
            }
            Phrase::Quiet if self.updates_on() => {
                if let Some(v) = &self.voice.engine {
                    v.stop_speaking();
                }
                self.set_update_mode(false, true);
            }
            Phrase::Quiet => return false,
            Phrase::Now => {
                if let Some(v) = &self.voice.engine {
                    v.stop_speaking();
                }
                self.updates_roundup_now();
            }
        }
        self.voice.action = Some(match p {
            Phrase::On => "update mode on".into(),
            Phrase::Now => "updates: roundup".into(),
            _ => "update mode off".into(),
        });
        true
    }

    pub fn toggle_updates_card(&mut self) {
        self.updates.card = !self.updates.card;
        if self.updates.card {
            self.assistant.show = true;
            self.speak(EXPLAIN_SPOKEN);
        }
    }

    pub fn update_mode_tool(&mut self, args: &Value) -> Result<Value, String> {
        if args.get("now").and_then(Value::as_bool).unwrap_or(false) {
            self.updates_scan(true);
            let lines = self.updates_lines();
            let text = if lines.is_empty() {
                self.updates_status_words()
            } else {
                updates::batch(lines.iter().map(|(p, l, _)| (*p, l.clone())).collect())
            };
            self.updates.pending.clear();
            self.updates.announced.clear();
            self.updates.last_said = Some(self.updates_now());
            for (_, l, k) in &lines {
                self.updates.last_line.insert(*k, l.clone());
            }
            return Ok(json!({"ok": true, "result": {
                "update_mode": self.updates_on(),
                "say": text,
                "next": "Say exactly the say text and nothing else."
            }}));
        }
        let on = args.get("on").and_then(Value::as_bool).unwrap_or(true);
        let say = self.set_update_mode(on, false);
        Ok(json!({"ok": true, "result": {
            "update_mode": self.updates_on(),
            "say": say,
            "next": if on { "Say exactly the say text. From now on GodTerm itself says short progress updates of the active projects; do not add your own." } else { "Say exactly the say text." }
        }}))
    }

    pub fn updates_state_words(&self) -> Option<String> {
        self.updates_on().then(|| {
            format!(
                "update mode: on, {} active projects (GodTerm itself says progress updates every {} min and at events; never repeat them)",
                self.updates.active,
                self.cfg.voice.updates_every_min.max(1)
            )
        })
    }
}

fn join_names(v: &[String]) -> String {
    match v.len() {
        0 => String::new(),
        1 => v[0].clone(),
        n => format!("{} and {}", v[..n - 1].join(", "), v[n - 1]),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => format!("{}{}", f.to_uppercase(), c.as_str()),
        None => String::new(),
    }
}
