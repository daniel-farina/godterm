//! Update mode: while it is on, GodTerm tells you, as things move, what
//! changed in each active project, like a colleague keeping you posted
//! ("EXELON: the test deploy finished the plan and is applying now.
//! Hyper: three of seven steps done, now applying the migration.").
//!
//! Every few seconds it reads what is new: the part of each Claude tab's
//! transcript written since the last read (text, tools by kind, to do
//! progress, subagents, background commands, errors, a turn that
//! ended), the screen state of every tab (working, ready, waiting for
//! approval: all there is for Grok, Codex, Cursor, Antigravity and
//! OpenCode), scheduled jobs that ran, and requests that were dropped
//! without a word. An event (an approval, an error, a finished turn, a
//! subagent or job done) is said at the next quiet moment once the
//! minimum gap since the last update is over; plain progress waits for
//! the roundup every few minutes. Nothing new: nothing is said. Several
//! projects go in one short utterance, the most important first, and
//! what "X finished" or "X needs approval" already said is not repeated.
//!
//! It never talks over the user (the quiet gate's rules: not mid
//! sentence, not right after they spoke or typed, not mid turn), and
//! waits while paused, brainstorming or in direct mode. With the voice
//! off, the speaker or mic muted, or voice asleep, the same words show as
//! one line in the assistant panel (every update is logged there too).
//! "What's the latest?" gives a roundup on demand, even with the mode
//! off. No model is called: the wording is built from the facts
//! (updates.rs), so it costs nothing.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::{App, View};
use crate::pane::Activity;
use crate::updates::{self, Cursor, Facts, Phrase, Said};

/// The card shown on first use (and from the ⓘ).
pub const EXPLAIN: &str = "Update mode: I keep you posted on every active project, a sentence or two each: what finished, what needs your approval, errors, steps done. Big news comes at once (at most every minute), the rest in a roundup every few minutes, and only when something changed. Say 'stop updates' to end it, or 'what's the latest' any time.";
/// Its spoken one line version.
pub const EXPLAIN_SPOKEN: &str =
    "Update mode. I'll tell you what changes in each active project. Say stop updates to end it.";

/// How often transcripts and screens are read.
const SCAN_EVERY: Duration = Duration::from_secs(5);

/// One tab followed by update mode.
#[derive(Debug, Clone)]
pub struct Track {
    pub path: Option<PathBuf>,
    pub cur: Cursor,
    pub act: Activity,
    pub bg_wait: bool,
    /// Its latest to do list (for "still on it" when asked).
    pub todo: Option<updates::Todo>,
}

#[derive(Default)]
pub struct UpdatesState {
    /// The explanation card shows in the panel.
    pub card: bool,
    pub tabs: HashMap<u64, Track>,
    /// What changed since the last update, by project key (a tab's uid,
    /// or a job target's), with the project's spoken name.
    pub pending: BTreeMap<u64, (String, Facts)>,
    /// "X needs approval" (true) or "X finished" (false) already said
    /// since the last update.
    pub announced: Vec<(u64, bool)>,
    /// Each job's last result time seen.
    pub jobs_seen: HashMap<String, i64>,
    pub jobs_primed: bool,
    pub last_said: Option<Instant>,
    pub last_round: Option<Instant>,
    pub scanned: Option<Instant>,
    /// Active projects at the last scan (the chip).
    pub active: usize,
    /// Each project's last line, so the same progress is not said twice.
    pub last_line: HashMap<u64, String>,
    /// What it said (or showed), newest last (the panel and tests).
    pub said: Vec<String>,
    /// Tests: the clock.
    pub clock: Option<Instant>,
}

/// A job target without an open tab gets a key of its own.
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

    /// Now, on update mode's clock.
    pub fn updates_now(&self) -> Instant {
        self.updates.clock.unwrap_or_else(Instant::now)
    }

    fn updates_gap(&self) -> Duration {
        Duration::from_secs(self.cfg.voice.updates_gap_s.max(10) as u64)
    }

    fn updates_every(&self) -> Duration {
        Duration::from_secs(self.cfg.voice.updates_every_min.max(1) as u64 * 60)
    }

    /// Something else has the floor: updates wait (and pile up).
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

    /// Said out loud, or only shown: no voice, a muted speaker or mic,
    /// voice asleep.
    pub fn updates_spoken(&self) -> bool {
        self.tts_on()
            && !self.cfg.voice.speaker_muted
            && !self.voice.muted
            && !self.voice.asleep
            && (self.voice.engine.is_some() || self.voice.preview.is_some() || self.cfg.voice.tts)
    }

    /// "● updates · 4 projects · next in 2m" for the composer.
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

    /// The palette, the menus, Settings: on or off.
    pub fn toggle_update_mode(&mut self) {
        let on = !self.updates_on();
        let say = self.set_update_mode(on, true);
        self.flash(say);
    }

    /// Turn it on (from now: nothing old is news) or off. With `speak`,
    /// its line is said. Returns that line.
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
        // From now on: every tab and job as it is now is the baseline.
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

    /// The names of the active projects (working, waiting, background
    /// work), as said.
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
        if self.updates.pending.contains_key(&tab.uid) {
            return true;
        }
        self.pane_harness(s) == Some(crate::harness::Harness::Claude)
            && self.transcript_path(s, t).is_some_and(|p| {
                crate::activity::running_subagents(&p, std::time::SystemTime::now()) > 0
            })
    }

    /// The focused tab the user is looking at (terminal in front): its
    /// news is on screen already.
    fn updates_in_view(&self, s: usize, t: usize) -> bool {
        self.voice.term_focused == Some(true)
            && s == self.focus
            && t == self.panes[s].active
            && self.view == View::Grid
    }

    /// Read what is new everywhere into `pending`. `ask`: an on demand
    /// roundup, so a tab first seen counts its recent past.
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
                        // The background work it waited on is done.
                        if tr.bg_wait && !bg && act == Activity::Ready {
                            facts.finished = true;
                        }
                        tr.act = act;
                        tr.bg_wait = bg;
                        if path != tr.path {
                            // Another session in this tab: a new one from
                            // its start, a resumed one from its end.
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
                // Direct mode says this tab's news itself; a tab in view
                // shows it.
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
        // Gone tabs: forgotten (a dropped request still waits to be said).
        self.updates.tabs.retain(|u, _| seen.contains(u));
        self.updates_scan_jobs();
        self.updates.active = active;
    }

    /// Jobs whose run ended well since the last scan (a failure is said
    /// at once by the jobs code itself).
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

    /// A request to a tab was dropped and nothing was said about it.
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

    /// The announcer said "X needs approval" or "X finished".
    pub fn updates_note_announced(&mut self, uid: u64, approval: bool) {
        if self.updates_on() && !self.updates.announced.contains(&(uid, approval)) {
            self.updates.announced.push((uid, approval));
        }
    }

    /// The pending news as lines, most important first; the projects it
    /// covers. Approvals no longer waiting are dropped.
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
            // The same progress line as last time is no news.
            if p == updates::Prio::Progress && self.updates.last_line.get(key) == Some(&line) {
                continue;
            }
            out.push((p, line, *key));
        }
        out
    }

    /// Every tick: read what is new now and then, and say it when due.
    pub fn updates_tick(&mut self) {
        if !self.updates_on() {
            // Off (also from Settings): the next start is a new baseline.
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
            // Nothing to say at the roundup: start the next one.
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

    /// Say (or show) the lines as one update; the pending news is spent.
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

    /// One update: a line in the panel, and said when it can be.
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
        let at = chrono::Local::now().format("%H:%M");
        self.note(format!("Update {at}: {text}"));
        if self.updates_spoken() {
            self.told_record("update mode", text, crate::told::Channel::Spoken, false);
            self.assistant.quiet.said.push(text.to_string());
            self.speak_assistant(text);
        }
    }

    /// "What's the latest?": what changed since the last update, or how
    /// things stand when nothing did. Works with the mode off too.
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

    /// How the active projects stand (nothing new to tell).
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
                    _ if tab.bg_wait => format!("{name} has background work running"),
                    _ => continue,
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

    /// Before an utterance is routed: "update mode", "stop updates",
    /// "what's the latest" (and "quiet" while it is on). True when it was
    /// handled here.
    pub fn updates_intercept(&mut self, command: &str) -> bool {
        let Some(p) = updates::parse_phrase(command) else {
            return false;
        };
        // In direct mode "what's the latest" is for the tab's agent.
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

    /// The panel's ⓘ: the card shown (or hidden again).
    pub fn toggle_updates_card(&mut self) {
        self.updates.card = !self.updates.card;
        if self.updates.card {
            self.assistant.show = true;
            self.speak(EXPLAIN_SPOKEN);
        }
    }

    /// The update_mode tool: on, off, or a roundup now.
    pub fn update_mode_tool(&mut self, args: &Value) -> Result<Value, String> {
        if args.get("now").and_then(Value::as_bool).unwrap_or(false) {
            // Shown in the panel; the tool's say is the same words.
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

    /// For the state block.
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
