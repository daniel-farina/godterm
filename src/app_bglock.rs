use crossterm::event::{KeyCode, KeyEvent};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::app::App;
use crate::pane::LaunchKind;
use crate::session_ids::{BgLock, NextLaunch};

const STOP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgChoice {
    Attach,
    Stop,
    Fork,
}

impl BgChoice {
    pub fn parse(s: &str) -> Option<BgChoice> {
        match s.trim().to_lowercase().as_str() {
            "attach" | "open" => Some(BgChoice::Attach),
            "stop" | "stop_and_resume" | "resume" => Some(BgChoice::Stop),
            "fork" | "copy" | "fork_session" => Some(BgChoice::Fork),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            BgChoice::Attach => "attach",
            BgChoice::Stop => "stop",
            BgChoice::Fork => "fork",
        }
    }
}

pub struct Stopping {
    pub uid: u64,
    pub short: String,

    pub resume: String,
    pub rx: Receiver<Result<(), String>>,
}

pub(crate) fn run_stop(
    bin: &str,
    short: &str,
    config_dir: &std::path::Path,
    pass: &[String],
) -> Result<(), String> {
    let mut c = std::process::Command::new(bin);
    c.args(["stop", short])
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    for (k, _) in std::env::vars() {
        if crate::pane::should_scrub(&k, pass) {
            c.env_remove(&k);
        }
    }
    let mut child = c
        .spawn()
        .map_err(|e| format!("could not run claude stop: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => return Ok(()),
            Ok(Some(s)) => {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = e.read_to_string(&mut err);
                }
                let err = err.trim();
                return Err(if err.is_empty() {
                    format!("claude stop ended with {s}")
                } else {
                    err.chars().take(200).collect()
                });
            }
            Ok(None) if start.elapsed() > STOP_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("claude stop did not finish in 20 s".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

impl App {
    pub(crate) fn note_bg_lock(&mut self, s: usize, t: usize) {
        let Some(a) = self.panes[s].account else {
            return;
        };
        if self.cfg.accounts[a].harness() != crate::harness::Harness::Claude {
            return;
        }
        let tab = &mut self.panes[s].tabs[t];
        let Some(short) = crate::session_ids::parse_bg_lock(&tab.screen_text()) else {
            return;
        };
        tab.restart_at = None;
        tab.bg_lock = Some(BgLock {
            short: short.clone(),
            session_id: tab.session_id.clone(),
            confirm_stop: false,
            stopping: false,
        });
        let uid = tab.uid;
        crate::log::info(&format!(
            "t{uid}: the resume of {} is held by background run {short} on {}",
            tab.session_id.as_deref().unwrap_or("?"),
            self.cfg.accounts[a].name
        ));
        self.flash(crate::session_ids::lock_choices(&short).trim().to_string());
    }

    pub(crate) fn bg_lock_key(&mut self, s: usize, t: usize, k: KeyEvent) {
        let Some(lock) = self.panes[s].tabs[t].bg_lock.clone() else {
            return;
        };
        if lock.stopping {
            self.flash(format!("Stopping the background run {}...", lock.short));
            return;
        }
        if lock.confirm_stop {
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                if let Err(e) = self.bg_lock_act(s, t, BgChoice::Stop) {
                    self.flash(e);
                }
            } else {
                if let Some(l) = self.panes[s].tabs[t].bg_lock.as_mut() {
                    l.confirm_stop = false;
                }
                self.flash(format!("Left the background run {} running", lock.short));
            }
            return;
        }
        let choice = match k.code {
            KeyCode::Enter | KeyCode::Char('a') | KeyCode::Char('A') => BgChoice::Attach,
            KeyCode::Char('f') | KeyCode::Char('F') => BgChoice::Fork,
            KeyCode::Char('s') | KeyCode::Char('S') => {
                if let Some(l) = self.panes[s].tabs[t].bg_lock.as_mut() {
                    l.confirm_stop = true;
                }
                self.flash(format!(
                    "Stop the background run {}? It may be doing work. y stops it and resumes here, any other key keeps it",
                    lock.short
                ));
                return;
            }
            _ => {
                self.flash(
                    crate::session_ids::lock_choices(&lock.short)
                        .trim()
                        .to_string(),
                );
                return;
            }
        };
        if let Err(e) = self.bg_lock_act(s, t, choice) {
            self.flash(e);
        }
    }

    pub fn bg_lock_act(&mut self, s: usize, t: usize, choice: BgChoice) -> Result<String, String> {
        let tab = &self.panes[s].tabs[t];
        let lock = tab
            .bg_lock
            .clone()
            .ok_or("that tab is not held by a background run")?;
        if lock.stopping {
            return Err(format!(
                "the background run {} is being stopped",
                lock.short
            ));
        }
        let a = self.panes[s].account.ok_or("that tab has no account")?;
        let uid = tab.uid;
        let short = lock.short.clone();
        let session = lock.session_id.clone().or(tab.session_id.clone());
        crate::log::info(&format!(
            "t{uid}: {} the background run {short}",
            choice.word()
        ));
        match choice {
            BgChoice::Attach => {
                let kind = self.panes[s].tabs[t].relaunch_kind();
                self.panes[s].tabs[t].next_launch = Some(NextLaunch::Attach(short.clone()));
                self.launch_tab(s, t, kind);
                Ok(format!(
                    "Attached the background run {short} in the tab; it keeps running"
                ))
            }
            BgChoice::Fork => {
                let id = session.ok_or("the tab has no session id to fork")?;
                self.panes[s].tabs[t].next_launch = Some(NextLaunch::Fork);
                self.launch_tab(s, t, LaunchKind::Resume(id));
                Ok("Opened a forked copy of the session in the tab; the background run keeps the original".into())
            }
            BgChoice::Stop => {
                let dir = self.cfg.accounts[a].config_dir();

                let held = crate::session_ids::bg_job(&dir, &short)
                    .and_then(|j| j.session_id)
                    .filter(|id| {
                        crate::session_ids::transcript(&dir, id)
                            .is_some_and(|p| crate::session_ids::has_messages(&p))
                    });
                let resume = held
                    .or(session)
                    .ok_or("the tab has no session id to resume")?;
                let (tx, rx) = std::sync::mpsc::channel();
                let (bin, pass, sh) = (
                    self.cfg.claude_bin(),
                    self.cfg.pass_env.clone(),
                    short.clone(),
                );
                std::thread::spawn(move || {
                    let _ = tx.send(run_stop(&bin, &sh, &dir, &pass));
                });
                if let Some(l) = self.panes[s].tabs[t].bg_lock.as_mut() {
                    l.stopping = true;
                    l.confirm_stop = false;
                }
                self.bg_stops.push(Stopping {
                    uid,
                    short: short.clone(),
                    resume,
                    rx,
                });
                self.flash(format!(
                    "Stopping the background run {short}, then resuming here"
                ));
                Ok(format!(
                    "Stopping the background run {short}; the tab resumes the session once it has stopped"
                ))
            }
        }
    }

    pub(crate) fn bglock_tick(&mut self) {
        if self.bg_stops.is_empty() {
            return;
        }
        let all = std::mem::take(&mut self.bg_stops);
        for st in all {
            let r = match st.rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.bg_stops.push(st);
                    continue;
                }
                Err(_) => Err("claude stop gave no answer".into()),
            };
            let Some((s, t)) = self.find_tab(st.uid) else {
                continue;
            };
            match r {
                Ok(()) => {
                    crate::log::info(&format!(
                        "t{}: stopped background run {}, resuming {}",
                        st.uid, st.short, st.resume
                    ));
                    self.panes[s].tabs[t].session_id = Some(st.resume.clone());
                    self.launch_tab(s, t, LaunchKind::Resume(st.resume));
                }
                Err(e) => {
                    crate::log::error(&format!(
                        "t{}: could not stop background run {}: {e}",
                        st.uid, st.short
                    ));
                    if let Some(l) = self.panes[s].tabs[t].bg_lock.as_mut() {
                        l.stopping = false;
                    }
                    self.flash(format!(
                        "Could not stop the background run {}: {e}",
                        st.short
                    ));
                }
            }
        }
    }

    pub fn session_copies(&self, id: &str) -> Result<Vec<crate::session_index::Row>, String> {
        use crate::session_ids::{bg_jobs, has_messages, id_matches, id_query, transcript};
        let q = id_query(id).unwrap_or_else(|| id.trim().to_lowercase());
        if q.is_empty() {
            return Err("id is required".into());
        }
        let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
        let of = |ids: &[String]| -> Vec<crate::session_index::Row> {
            ix.rows
                .iter()
                .filter(|r| ids.contains(&r.info.id))
                .cloned()
                .collect()
        };
        let mut rows: Vec<crate::session_index::Row> = ix
            .rows
            .iter()
            .filter(|r| id_matches(&q, &r.info.id))
            .cloned()
            .collect();
        if rows.is_empty() {
            let mut ids = vec![];
            for a in 0..self.cfg.accounts.len() {
                let dir = self.cfg.accounts[a].config_dir();
                for j in bg_jobs(&dir) {
                    if !id_matches(&q, &j.short) {
                        continue;
                    }
                    let own = j
                        .session_id
                        .clone()
                        .filter(|s| transcript(&dir, s).is_some_and(|p| has_messages(&p)));
                    if let Some(i) = own.or(j.forked_from.clone()) {
                        ids.push(i);
                    }
                }
            }
            rows = of(&ids);
        }
        if rows.is_empty() {
            return Err("no session with that id (call sessions first)".into());
        }
        if rows.iter().all(|r| r.info.parent.is_some()) {
            return Err("that is a subagent; open its parent session instead".into());
        }
        rows.retain(|r| r.info.parent.is_none());
        let mut ids: Vec<&str> = rows.iter().map(|r| r.info.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        if ids.len() > 1 {
            return Err(format!(
                "that id starts {} sessions ({}); give more of it",
                ids.len(),
                ids.iter()
                    .take(5)
                    .map(|i| crate::paths::head(i, 13))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(rows)
    }

    pub(crate) fn held_tab_for(&self, dir: Option<&str>, name: Option<&str>) -> Option<String> {
        let dir = dir.map(|d| crate::config::expand_tilde(d.trim()));
        let leaf = |p: &std::path::Path| {
            p.file_name()
                .map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        for (s, t) in self.all_tabs() {
            let tab = &self.panes[s].tabs[t];
            let Some(l) = &tab.bg_lock else {
                continue;
            };
            let same_dir = dir
                .as_ref()
                .is_some_and(|d| *d == tab.cwd || (!d.is_absolute() && leaf(d) == leaf(&tab.cwd)));
            let same_name = name.is_some_and(|n| {
                let n = n.trim().to_lowercase();
                n == tab.name().to_lowercase() || n == leaf(&tab.cwd)
            });
            if same_dir || same_name {
                return Some(format!(
                    "refused: the {} tab ({}) in that folder could not resume because the background run {} holds its session. Fix that tab with fix_resume (attach, stop or fork) instead of opening a fresh session; only if the user clearly asked for a new blank session there, call open_tab again with fresh true",
                    tab.name(),
                    crate::control::tab_id(tab.uid),
                    l.short
                ));
            }
        }
        None
    }

    pub(crate) fn fix_resume_plan(
        &mut self,
        args: &serde_json::Value,
    ) -> Result<(serde_json::Value, String, bool), String> {
        let word = args["choice"].as_str().unwrap_or("attach");
        let choice = BgChoice::parse(word)
            .ok_or_else(|| format!("choice: unknown \"{word}\" (attach, stop or fork)"))?;
        let (s, t) = self.one_tab(args)?;
        let tab = &self.panes[s].tabs[t];
        let id = crate::control::tab_id(tab.uid);
        let Some(lock) = tab.bg_lock.clone() else {
            return Err(format!(
                "{id} is not held by a background run (state: {}); read_tab {id} what=screen shows why it stopped",
                self.state_word(s, t)
            ));
        };
        let q = format!(
            "Stop the background run {} that holds the session of the {} tab ({id}) and resume it there? It may be doing work.",
            lock.short,
            tab.name()
        );
        Ok((
            serde_json::json!({"uid": tab.uid, "choice": choice.word(), "job": lock.short}),
            q,
            choice == BgChoice::Stop,
        ))
    }

    pub(crate) fn fix_resume_run(
        &mut self,
        plan: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let uid = plan["uid"].as_u64().unwrap_or(0);
        let (s, t) = self.find_tab(uid).ok_or("that tab was closed")?;
        let choice = BgChoice::parse(plan["choice"].as_str().unwrap_or("")).ok_or("bad plan")?;
        if self.panes[s].tabs[t]
            .bg_lock
            .as_ref()
            .map(|l| l.short.as_str())
            != plan["job"].as_str()
        {
            return Err(
                "that tab is no longer held by that background run; read_tab it again".into(),
            );
        }
        let say = self.bg_lock_act(s, t, choice)?;
        self.last_target = Some(uid);
        let a = self.panes[s]
            .account
            .map(|a| self.cfg.accounts[a].display().to_string());
        Ok(serde_json::json!({"ok": true, "result": {
            "tab": crate::control::tab_id(uid),
            "choice": choice.word(),
            "job": plan["job"],
            "account": a,
            "say": say,
        }}))
    }

    pub fn bg_lock_json(&self, s: usize, t: usize) -> Option<serde_json::Value> {
        let l = self.panes[s].tabs[t].bg_lock.as_ref()?;
        Some(serde_json::json!({
            "job": l.short,
            "session": l.session_id,
            "stopping": l.stopping,
            "fix": "fix_resume on this tab: attach (default, safe), stop (asks once; the run may be doing work) or fork",
        }))
    }
}
