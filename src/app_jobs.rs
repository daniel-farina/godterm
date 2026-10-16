//! GodTerm's own scheduler in the app: jobs (jobs.rs) fire from a tick,
//! each delivering its prompt to its target tab through the same
//! deliveries and receipts as any request. A target that was closed or
//! archived is woken first (restored from the archive, or reopened by its
//! session id on its account) without taking the screen; a busy tab gets
//! the prompt queued after its work. How each run ends is kept on the job
//! (`last_result`), a failure is said to the user and the assistant, and
//! the assistant has create_job, list_jobs, update_job, delete_job,
//! run_job_now and adopt_loop.

use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::{json, Value};

use crate::app::App;
use crate::app_quiet::Voice;
use crate::control::{plural, tab_id};
use crate::jobs::{self, CatchUp, Event, Job, RunResult, Target};
use crate::pane::{Activity, LaunchKind};
use crate::receipts::Receipt;

/// GodTerm does not run jobs in the first moments after it starts: tabs
/// are still coming back.
const BOOT_GRACE: Duration = Duration::from_secs(8);
/// Jobs are looked at this often.
const EVERY: Duration = Duration::from_millis(900);

/// A prompt a job sent, followed until its receipt ends.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRun {
    pub job: String,
    pub delivery: u64,
    /// The state of the delivery last put on the job (so a later note,
    /// like a skipped run, is not overwritten by the same state again).
    pub seen: String,
}

pub const DESC_CREATE: &str = "Create a GodTerm job: a prompt GodTerm itself sends to a tab on a schedule. Unlike a Claude /loop or CronCreate (which lives in one session's memory, dies when it exits, and expires after 7 days) a job is kept by GodTerm, survives restarts, and still fires when its tab was closed or archived: GodTerm wakes the tab (restores it from the archive or resumes its session on its account), then sends the prompt, queued after the tab's current work if it is busy. Works for Claude and Grok tabs. Use it for anything that must keep running. tab: the tab to send to (id, name); or archived: an archive id. prompt: exactly what the tab's agent gets each time. Schedule, exactly one of: cron (five fields, local time: \"0 9 * * 1-5\"), every (\"30 minutes\", \"2 hours\", \"1 day\") or at (one run: \"in 2 hours\", \"2026-10-15 09:00\"). catch_up: once (default: a run missed while GodTerm was off or the Mac asleep runs once when it is back) or skip. until: an optional end date (\"2026-12-31\", \"in 30 days\"): GodTerm warns the day before and says when it stops; without it the job never expires. name: a short label. Asks the user once. The result has the job id, its next run and what to tell the user.";
pub const DESC_LIST: &str = "GodTerm's jobs (scheduled prompts it keeps): id, name, prompt, schedule, tab and whether it is open, archived or closed, next run, last run and how it went (done, failed with the reason, skipped, missed), runs, end date. Filters: tab, status (active, paused, finished, failing), text. Also lists session loops (kind \"session\": Claude /loop and CronCreate jobs that die with their session, with when they expire; durable ones in .claude/scheduled_tasks.json; Grok scheduled tasks) so you can offer adopt_loop.";
pub const DESC_UPDATE: &str = "Change a GodTerm job (id or name): action pause or resume; or name, prompt, cron / every / at (a new schedule), tab or archived (a new target), catch_up (once or skip), until (an end date, or \"none\" to remove it). Pause, resume, name and catch_up apply at once; changing the prompt, the schedule, the target or the end date asks the user once (confirm_token). Resuming a job that ended needs a new until.";
pub const DESC_DELETE: &str = "Delete a GodTerm job for good (id or name). Asks the user once. To stop it for a while use update_job with action pause. Archiving a tab does not delete its jobs: they wake the tab when they run.";
pub const DESC_RUN: &str = "Run a GodTerm job now (id or name), outside its schedule: the prompt goes to its target tab (waking it if it is closed or archived; queued if busy). Its next scheduled run is unchanged. Returns the delivery state; say it was sent only when state is received, started or done.";
pub const DESC_ADOPT: &str = "Turn a tab's session loop (a Claude /loop or CronCreate job, or a Grok scheduled task, see loops in get_state or list_jobs) into a GodTerm job so it keeps running after the session ends: copies its prompt and schedule (a dynamic /loop has none: pass cron or every), targets the same tab, and by default asks the tab to cancel its own loop so it does not fire twice (keep_session_loop true leaves it). tab: the tab; loop: the loop id (optional when the tab has one). Also name, catch_up, until. Asks the user once.";

fn ok(v: Value) -> Value {
    json!({"ok": true, "result": v})
}

/// First `n` characters.
fn head(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        format!(
            "{}…",
            flat.chars().take(n.saturating_sub(1)).collect::<String>()
        )
    }
}

/// A moment typed or said: "in 2 hours", "2 days", an RFC 3339 time,
/// "2026-10-15 09:00", "2026-10-15T09:00"; a bare date is the end of that
/// day when `end_of_day`, else refused (an hour is needed).
pub fn parse_when(s: &str, now: i64, end_of_day: bool) -> Result<i64, String> {
    let t = s.trim();
    let low = t.to_lowercase();
    if let Some(m) = jobs::parse_every(low.strip_prefix("in ").unwrap_or(&low)) {
        return Ok(now + i64::from(m) * 60);
    }
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(t) {
        return Ok(d.timestamp());
    }
    for f in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(t, f) {
            if let Some(l) = Local.from_local_datetime(&n).earliest() {
                return Ok(l.timestamp());
            }
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        if end_of_day {
            if let Some(l) = d
                .and_hms_opt(23, 59, 0)
                .and_then(|n| Local.from_local_datetime(&n).earliest())
            {
                return Ok(l.timestamp());
            }
        }
        return Err(format!(
            "\"{t}\" needs a time of day (like {t} 09:00, or \"in 2 hours\")"
        ));
    }
    Err(format!(
        "I could not read \"{t}\" as a time (use \"in 2 hours\", or 2026-10-15 09:00)"
    ))
}

/// Seconds as "in 3:12" under an hour, else "in 2h", "in 3d".
fn rel_future(t: i64, now: i64) -> String {
    let s = t - now;
    if s <= 0 {
        return "due".into();
    }
    if s < 3600 {
        return format!("in {}:{:02}", s / 60, s % 60);
    }
    if s < 86_400 {
        return format!("in {}h", s / 3600);
    }
    format!("in {}d", s / 86_400)
}

/// "3m ago", "2h ago".
fn ago(t: i64, now: i64) -> String {
    let s = (now - t).max(0);
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

/// What a row of the schedules section is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedKind {
    /// A GodTerm job.
    Managed,
    /// A Claude loop in a session's memory.
    Session,
    /// In `.claude/scheduled_tasks.json`: fires only while a session runs
    /// in that project.
    Durable,
    /// A Grok scheduled task.
    Grok,
}

impl SchedKind {
    pub fn badge(self) -> &'static str {
        match self {
            SchedKind::Managed => "managed",
            SchedKind::Session => "session",
            SchedKind::Durable => "durable",
            SchedKind::Grok => "grok",
        }
    }
}

/// How the last run went, for the row's color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Bad,
    Warn,
    Plain,
}

/// One line of the section.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedRow {
    pub kind: SchedKind,
    /// Job id, or "uid:loop id" of a session loop: what a click acts on.
    pub key: String,
    /// Its tab, when open.
    pub uid: Option<u64>,
    pub label: String,
    pub tab: String,
    pub cadence: String,
    pub next: String,
    pub last: String,
    pub tone: Tone,
    pub paused: bool,
    /// Session loops: when it expires.
    pub expires: Option<String>,
    /// The job id or the loop id.
    pub id: String,
    /// "t12" while its tab is open, else empty.
    pub tab_id: String,
    /// The tab's folder (its last part) and account.
    pub folder: String,
    pub account: String,
    /// What it is grouped under: "t12" for an open tab, "tab:<label>"
    /// for a closed or archived target, "godterm" without one; and how
    /// that group is called ("t12 hacker sim", "GodTerm").
    pub group: String,
    pub group_label: String,
    /// A dynamic loop that wakes itself (no fixed schedule).
    pub dynamic: bool,
}

impl SchedRow {
    /// "schedule" or "loop", for the small label.
    pub fn shape(&self) -> &'static str {
        if self.dynamic {
            "loop"
        } else {
            "schedule"
        }
    }
}

/// The last part of a folder path.
pub(crate) fn folder_tail(p: &str) -> String {
    std::path::Path::new(p)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// The per row buttons.
pub const OP_RUN: u8 = 0;
pub const OP_PAUSE: u8 = 1;
pub const OP_DELETE: u8 = 2;
pub const OP_ADOPT: u8 = 3;
/// Session loops: ask the tab to cancel it (two clicks).
pub const OP_STOP: u8 = 4;
/// Go to its tab.
pub const OP_OPEN: u8 = 5;
/// A second click on delete within this long deletes.
pub(crate) const CONFIRM: Duration = Duration::from_secs(6);

impl App {
    /// Now, Unix seconds (a test may fix it).
    pub fn job_now(&self) -> i64 {
        self.job_clock.unwrap_or_else(jobs::now_ts)
    }

    fn jobs_save(&mut self) {
        if let Err(e) = self.jobs.save() {
            crate::log::info(&format!("jobs: could not save: {e}"));
            self.flash(format!("Could not save the jobs: {e}"));
        }
    }

    // ---- finding the target ----

    /// The open tab a target names: by its id, else by its session.
    pub fn job_tab(&self, t: &Target) -> Option<(usize, usize)> {
        if let Some(u) = t.uid {
            if let Some(x) = self.locate_uid(u) {
                return Some(x);
            }
        }
        let sid = t.session_id.as_ref()?;
        self.all_tabs().into_iter().find(|&(s, i)| {
            self.panes[s].tabs[i].session_id.as_ref() == Some(sid)
                && self.panes[s]
                    .account
                    .is_none_or(|a| t.account.is_empty() || self.cfg.accounts[a].name == t.account)
        })
    }

    /// Where the target is: open, archived (id) or closed.
    pub fn job_target_state(&self, t: &Target) -> &'static str {
        if self.job_tab(t).is_some() {
            "open"
        } else if self.job_archived(t).is_some() {
            "archived"
        } else {
            "closed"
        }
    }

    fn job_archived(&self, t: &Target) -> Option<String> {
        let sid = t.session_id.as_ref()?;
        self.archive
            .entries
            .iter()
            .find(|e| {
                e.session_id.as_ref() == Some(sid)
                    && (t.account.is_empty() || e.account == t.account)
            })
            .map(|e| e.id.clone())
    }

    /// The target for tab (s, i).
    pub fn job_target_of(&self, s: usize, i: usize) -> Target {
        let tab = &self.panes[s].tabs[i];
        let (provider, account) = match self.panes[s].account {
            Some(a) => (
                self.cfg.accounts[a].harness().name().to_string(),
                self.cfg.accounts[a].name.clone(),
            ),
            None => (String::new(), String::new()),
        };
        Target {
            uid: Some(tab.uid),
            session_id: tab.session_id.clone(),
            provider,
            account,
            cwd: tab.cwd.to_string_lossy().into_owned(),
            label: tab.name(),
        }
    }

    /// Keep each job's target up to date with its open tab (its session
    /// id appears once the tab has run; a rename changes the label).
    fn jobs_refresh_targets(&mut self) {
        let mut changed = false;
        for k in 0..self.jobs.jobs.len() {
            let Some((s, i)) = self.job_tab(&self.jobs.jobs[k].target) else {
                continue;
            };
            let fresh = self.job_target_of(s, i);
            if self.jobs.jobs[k].target != fresh {
                self.jobs.jobs[k].target = fresh;
                changed = true;
            }
        }
        if changed {
            self.jobs_save();
        }
    }

    /// Open the target if it is not, without taking the screen; returns
    /// its tab id and how it was woken.
    fn job_wake(&mut self, t: &Target) -> Result<(u64, Option<&'static str>), String> {
        if let Some((s, i)) = self.job_tab(t) {
            let tab = &self.panes[s].tabs[i];
            let uid = tab.uid;
            if tab.is_running() || tab.activity == Activity::Starting {
                return Ok((uid, None));
            }
            let kind = tab
                .pending
                .clone()
                .or_else(|| tab.session_id.clone().map(LaunchKind::Resume))
                .ok_or("its session ended and there is nothing to resume")?;
            let why = if tab.suspended {
                "resumed"
            } else {
                "restarted"
            };
            self.launch_tab(s, i, kind);
            return Ok((uid, Some(why)));
        }
        // Keep what is on screen: the reopen focuses the new tab.
        let focus = self.focus;
        let view = self.view;
        let actives: Vec<usize> = self.panes.iter().map(|p| p.active).collect();
        let sid = t.session_id.clone();
        let from_archive = self.job_archived(t).is_some();
        let result: Result<(), String> = (|| {
            if let Some(id) = self.job_archived(t) {
                return self.restore_archived(&id, None).map(|_| ());
            }
            let sid = sid.clone().ok_or(
                "its tab was closed and it never had a session, so there is nothing to resume",
            )?;
            // The Undo stack's copy would reopen it a second time.
            self.closed.retain(|c| c.session_id.as_ref() != Some(&sid));
            let c = crate::closed::ClosedTab {
                account: t.account.clone(),
                cwd: t.cwd.clone(),
                session_id: Some(sid),
                name: Some(t.label.clone()).filter(|l| !l.is_empty()),
                accent: None,
                loops: vec![],
                group: 0,
                closed_at: self.job_now(),
                label: t.label.clone(),
                tab_group: None,
                pinned: false,
            };
            self.reopen_closed_tab(c).map(|_| ())
        })();
        let uid = self
            .panes
            .get(self.focus)
            .map(|p| p.cur().uid)
            .filter(|_| result.is_ok());
        self.focus = focus.min(self.panes.len().saturating_sub(1));
        self.view = view;
        for (p, a) in actives.into_iter().enumerate() {
            if let Some(slot) = self.panes.get_mut(p) {
                if a < slot.tabs.len() {
                    slot.active = a;
                }
            }
        }
        result?;
        let uid = uid.ok_or("it did not open")?;
        Ok((
            uid,
            Some(if from_archive {
                "restored from the archive"
            } else {
                "reopened"
            }),
        ))
    }

    // ---- running ----

    fn set_result(&mut self, id: &str, state: &str, detail: impl Into<String>) {
        let now = self.job_now();
        if let Some(j) = self.jobs.find_mut(id) {
            j.last_result = Some(RunResult {
                state: state.into(),
                detail: detail.into(),
                at: now,
            });
        }
        self.state_dirty = true;
    }

    /// Send job `id`'s prompt to its tab now. `manual`: run_job_now (not
    /// held back by a run still going). Returns the delivery id, and what
    /// it took to open the tab.
    pub fn fire_job(
        &mut self,
        id: &str,
        missed_by: Option<i64>,
        manual: bool,
    ) -> Result<(u64, Option<&'static str>), String> {
        let j = self.jobs.find(id).cloned().ok_or("no such job")?;
        let busy_run = self.job_runs.iter().any(|r| {
            r.job == id
                && self.deliveries.iter().any(|d| {
                    d.id == r.delivery
                        && !d.rc.receipt.is_final()
                        && !matches!(d.stage, crate::control::Stage::Failed(_))
                })
        });
        if busy_run && !manual {
            let why = "the previous run is still going";
            self.set_result(id, "skipped", why);
            self.jobs_save();
            return Err(why.into());
        }
        let now = self.job_now();
        let (uid, woke) = match self.job_wake(&j.target) {
            Ok(x) => x,
            Err(e) => {
                self.job_failed(id, &format!("could not open its tab: {e}"));
                return Err(e);
            }
        };
        if let Some((s, i)) = self.locate_uid(uid) {
            let t = self.job_target_of(s, i);
            if let Some(job) = self.jobs.find_mut(id) {
                job.target = t;
            }
        }
        let d = self.deliver(uid, &j.prompt);
        self.job_runs.retain(|r| r.job != id);
        self.job_runs.push(JobRun {
            job: id.to_string(),
            delivery: d,
            seen: "sent".into(),
        });
        let busy = self
            .find_tab(uid)
            .is_some_and(|(s, i)| self.panes[s].tabs[i].activity == Activity::Working);
        let mut detail = vec![];
        if let Some(w) = woke {
            detail.push(format!("the tab was {w}"));
        }
        if busy {
            detail.push("queued after the tab's current work".to_string());
        }
        if let Some(m) = missed_by {
            detail.push(format!("a missed run, {} late", jobs::span_words(m)));
        }
        if let Some(job) = self.jobs.find_mut(id) {
            job.last_run = Some(now);
            job.runs += 1;
        }
        self.set_result(id, "sent", detail.join("; "));
        self.jobs_save();
        crate::log::info(&format!(
            "jobs: {id} ({}) sent to {} as delivery #{d}",
            j.name,
            tab_id(uid)
        ));
        Ok((d, woke))
    }

    fn job_failed(&mut self, id: &str, why: &str) {
        let Some(j) = self.jobs.find(id).cloned() else {
            return;
        };
        self.set_result(id, "failed", why);
        self.jobs_save();
        let src = crate::told::Source {
            tab: j.target.uid,
            account: None,
            fact: Some(format!("job {} ({}) failed", j.id, j.name)),
        };
        let msg = format!("The job \"{}\" ({}) failed: {why}.", j.name, j.id);
        self.told_from("scheduled job", src, |app| {
            app.assistant_note(format!(
                "{msg} It runs again {}; run_job_now tries it now.",
                match app.jobs.find(&j.id).and_then(|x| x.next_run) {
                    Some(n) => rel_future(n, app.job_now()),
                    None => "never (no later run)".into(),
                }
            ));
            app.flash(msg.clone());
            app.say_urgent(Voice::Reply, &msg);
        });
    }

    /// Follow the prompts jobs sent: their receipts become the job's last
    /// result.
    fn jobs_follow(&mut self) {
        let runs = self.job_runs.clone();
        let mut done = vec![];
        for r in runs {
            let same = |app: &App, state: &str| {
                app.job_runs
                    .iter()
                    .any(|x| x.job == r.job && x.delivery == r.delivery && x.seen == state)
            };
            let seen = self
                .deliveries
                .iter()
                .find(|d| d.id == r.delivery)
                .map(|d| (d.rc.receipt.clone(), d.stage.clone()));
            let (state, detail, fin) = match seen {
                None => (
                    "unconfirmed",
                    "its delivery record is gone".to_string(),
                    true,
                ),
                Some((Receipt::Done, _)) => ("done", String::new(), true),
                Some((Receipt::Started, _)) => ("started", String::new(), false),
                Some((Receipt::Received, _)) => (
                    "received",
                    "in the tab's queue behind its current work".to_string(),
                    false,
                ),
                Some((Receipt::Dropped(w), _)) => ("failed", format!("dropped: {w}"), true),
                Some((Receipt::Interrupted(w), _)) => ("failed", format!("interrupted: {w}"), true),
                Some((Receipt::Unconfirmed, _)) => (
                    "unconfirmed",
                    "typed in; this agent gives no receipt".to_string(),
                    true,
                ),
                Some((Receipt::Pending, crate::control::Stage::Failed(w))) => {
                    ("failed", format!("never went in: {w}"), true)
                }
                Some((Receipt::Pending, crate::control::Stage::Queued)) => (
                    "queued",
                    "waiting for the tab to be ready".to_string(),
                    false,
                ),
                Some((Receipt::Pending, _)) => ("sent", String::new(), false),
            };
            if !same(self, state) {
                if let Some(x) = self
                    .job_runs
                    .iter_mut()
                    .find(|x| x.job == r.job && x.delivery == r.delivery)
                {
                    x.seen = state.to_string();
                }
                // Keep what the run's start said (woke, missed) in front.
                let prev = self
                    .jobs
                    .find(&r.job)
                    .and_then(|j| j.last_result.as_ref().map(|x| x.detail.clone()))
                    .unwrap_or_default();
                if state == "failed" {
                    self.job_failed(&r.job, &detail);
                } else {
                    let keep = if detail.is_empty() {
                        prev
                    } else if prev.is_empty() || fin {
                        detail
                    } else {
                        format!("{prev}; {detail}")
                    };
                    self.set_result(&r.job, state, keep);
                    self.jobs_save();
                }
            }
            if fin {
                done.push(r.job);
            }
        }
        self.job_runs.retain(|r| !done.contains(&r.job));
    }

    /// Every second or so: fire what is due, follow what was sent, say
    /// what ended or is about to. Nothing runs in the first seconds.
    pub fn jobs_tick(&mut self) {
        if self.job_clock.is_none() {
            if self.jobs_boot.elapsed() < BOOT_GRACE || self.jobs_ticked.elapsed() < EVERY {
                return;
            }
            self.jobs_ticked = Instant::now();
        }
        if self.jobs.jobs.is_empty() && self.job_runs.is_empty() {
            return;
        }
        // A run from before a restart can no longer be followed.
        for k in 0..self.jobs.jobs.len() {
            let j = &self.jobs.jobs[k];
            if j.last_result.as_ref().is_some_and(|r| !r.is_final())
                && !self.job_runs.iter().any(|r| r.job == j.id)
            {
                let id = j.id.clone();
                self.set_result(
                    &id,
                    "unconfirmed",
                    "GodTerm restarted before it could confirm this run",
                );
            }
        }
        self.jobs_refresh_targets();
        let now = self.job_now();
        let ev = jobs::due(&mut self.jobs.jobs, now);
        if !ev.is_empty() {
            self.jobs_save();
        }
        for e in ev {
            self.job_event(e);
        }
        self.jobs_follow();
    }

    fn job_name(&self, id: &str) -> String {
        self.jobs
            .find(id)
            .map(|j| j.name.clone())
            .unwrap_or_default()
    }

    fn job_event(&mut self, e: Event) {
        match e {
            Event::Fire { id, missed_by } => {
                // (A failure was already said.)
                let _ = self.fire_job(&id, missed_by, false);
            }
            Event::Skipped { id, missed_by } => {
                let name = self.job_name(&id);
                self.assistant_note(format!(
                    "The job \"{name}\" ({id}) missed a run {} ago while GodTerm was not running and skipped it, as its catch up policy says.",
                    jobs::span_words(missed_by)
                ));
            }
            Event::Ended { id } => {
                let name = self.job_name(&id);
                let msg = format!(
                    "The job \"{name}\" ({id}) reached its end date and stopped. update_job with a new until, then action resume, keeps it going."
                );
                self.assistant_note(msg.clone());
                self.flash(format!("Job \"{name}\" reached its end date and stopped"));
                self.say_update(
                    Voice::Reply,
                    &format!("The job {name} reached its end date and stopped."),
                );
            }
            Event::EndSoon { id, at } => {
                let name = self.job_name(&id);
                let when = jobs::local_words(at);
                self.assistant_note(format!(
                    "The job \"{name}\" ({id}) ends {when} (within a day) and will stop then. update_job with a later until keeps it going."
                ));
                self.flash(format!("Job \"{name}\" ends {when}"));
                self.say_update(
                    Voice::Reply,
                    &format!("The job {name} ends {when}. Say so if you want it to keep going."),
                );
            }
            Event::Completed { id } => {
                crate::log::info(&format!("jobs: {id} ran its one time and is done"));
            }
        }
    }

    // ---- jobs of a tab (archiving) ----

    /// Jobs that target tab `uid`.
    pub fn jobs_owned_by(&self, uid: u64) -> Vec<&Job> {
        self.jobs
            .jobs
            .iter()
            .filter(|j| j.active() && j.target.uid == Some(uid))
            .collect()
    }

    /// "2 jobs (nightly check, digest) will wake it when they run", for
    /// archiving a tab; None without jobs.
    pub fn jobs_wake_note(&self, uid: u64) -> Option<String> {
        let js = self.jobs_owned_by(uid);
        if js.is_empty() {
            return None;
        }
        let names: Vec<String> = js.iter().take(3).map(|j| j.name.clone()).collect();
        Some(format!(
            "{} ({}{}) will wake it when {}",
            plural(js.len(), "job"),
            names.join(", "),
            if js.len() > 3 { ", …" } else { "" },
            if js.len() == 1 { "it runs" } else { "they run" }
        ))
    }

    // ---- the section's rows ----

    /// Managed jobs, then session loops, durable ones and Grok tasks.
    pub fn sched_rows(&self) -> Vec<SchedRow> {
        let now_ts = self.job_now();
        let mut v = vec![];
        let mut js: Vec<&Job> = self.jobs.jobs.iter().collect();
        js.sort_by_key(|j| (!j.active(), j.next_run.unwrap_or(i64::MAX)));
        for j in js {
            let tab = match self.job_target_state(&j.target) {
                "open" => j.target.label.clone(),
                other => format!("{} ({other})", j.target.label),
            };
            let (last, tone) = match &j.last_result {
                Some(r) => {
                    let mark = match r.state.as_str() {
                        "done" => "✓",
                        "failed" => "✗",
                        "skipped" => "–",
                        _ => "…",
                    };
                    (
                        format!("{mark} {} {}", r.state, ago(r.at, now_ts)),
                        match r.state.as_str() {
                            "done" => Tone::Good,
                            "failed" => Tone::Bad,
                            "skipped" => Tone::Warn,
                            _ => Tone::Plain,
                        },
                    )
                }
                None => ("not run yet".into(), Tone::Plain),
            };
            let next = match (&j.finished, j.enabled, j.next_run) {
                (Some(f), _, _) => f.clone(),
                (None, false, _) => "paused".into(),
                (None, true, Some(n)) => rel_future(n, now_ts),
                (None, true, None) => "–".into(),
            };
            let uid = self
                .job_tab(&j.target)
                .map(|(s, i)| self.panes[s].tabs[i].uid);
            let (group, group_label) = match uid {
                Some(u) => (tab_id(u), format!("{} {}", tab_id(u), j.target.label)),
                None if j.target.label.is_empty() && j.target.session_id.is_none() => {
                    ("godterm".to_string(), "GodTerm".to_string())
                }
                None => (format!("tab:{}", j.target.label), tab.clone()),
            };
            let account = self
                .cfg
                .accounts
                .iter()
                .find(|a| a.name == j.target.account)
                .map(|a| a.display().to_string())
                .unwrap_or_else(|| j.target.account.clone());
            v.push(SchedRow {
                kind: SchedKind::Managed,
                key: j.id.clone(),
                uid,
                id: j.id.clone(),
                tab_id: uid.map(tab_id).unwrap_or_default(),
                folder: if j.target.cwd.is_empty() {
                    String::new()
                } else {
                    folder_tail(&j.target.cwd)
                },
                account,
                group,
                group_label,
                dynamic: false,
                label: j.name.clone(),
                tab,
                cadence: j.cadence(),
                next,
                last,
                tone,
                paused: !j.enabled,
                expires: j.end_at.map(|e| format!("ends {}", jobs::local_words(e))),
            });
        }
        let now = chrono::Utc::now();
        for r in &self.loops {
            let kind = if r.lp.every.is_some() {
                SchedKind::Grok
            } else if r.lp.durable {
                SchedKind::Durable
            } else {
                SchedKind::Session
            };
            let next = match r.lp.next_fire(now) {
                Some(t) => rel_future(t.timestamp(), now.timestamp()),
                None => "–".into(),
            };
            let last = match r.lp.last_fire {
                Some(t) => format!("ran {}", ago(t.timestamp(), now.timestamp())),
                None => "not run yet".into(),
            };
            let folder = self
                .find_tab(r.uid)
                .map(|(s, i)| folder_tail(&self.panes[s].tabs[i].cwd.to_string_lossy()))
                .unwrap_or_default();
            v.push(SchedRow {
                kind,
                key: format!("{}:{}", r.uid, r.lp.id),
                uid: Some(r.uid),
                id: r.lp.id.clone(),
                tab_id: tab_id(r.uid),
                folder,
                account: r
                    .account
                    .map(|a| self.account_cfg(a).display().to_string())
                    .unwrap_or_default(),
                group: tab_id(r.uid),
                group_label: format!("{} {}", tab_id(r.uid), r.tab_name),
                dynamic: r.lp.kind == crate::loops::Kind::Wakeup,
                label: if r.lp.prompt.trim().is_empty() {
                    r.lp.id.clone()
                } else {
                    head(&r.lp.prompt, 60)
                },
                tab: r.tab_name.clone(),
                cadence: r.lp.cadence(),
                next,
                last,
                tone: Tone::Plain,
                paused: false,
                expires: r
                    .lp
                    .expires()
                    .map(|e| format!("expires {}", crate::loops::rel(e, now))),
            });
        }
        // Two rows of one tab with the same label (two crons with the
        // same prompt): their ids tell them apart.
        let n = v.len();
        let twins: Vec<bool> = (0..n)
            .map(|a| (0..n).any(|b| b != a && v[b].group == v[a].group && v[b].label == v[a].label))
            .collect();
        for (r, twin) in v.iter_mut().zip(twins) {
            if twin {
                r.label = format!("{} #{}", r.label, r.id);
            }
        }
        v
    }

    /// A click on a row: go to its tab.
    pub fn sched_open(&mut self, key: &str) {
        let rows = self.sched_rows();
        let Some(r) = rows.iter().find(|r| r.key == key) else {
            return;
        };
        match r.uid.and_then(|u| self.find_tab(u)) {
            Some((s, i)) => {
                self.panes[s].hidden = false;
                self.jump_to(s, i);
            }
            None => self.flash(format!(
                "{} is not open ({}): run it now to wake the tab, or restore it from Archived",
                r.tab, r.label
            )),
        }
    }

    /// A button on a row.
    pub fn sched_action(&mut self, key: &str, op: u8) {
        let rows = self.sched_rows();
        let Some(r) = rows.iter().find(|r| r.key == key).cloned() else {
            return;
        };
        match (r.kind, op) {
            (SchedKind::Managed, OP_RUN) => match self.fire_job(key, None, true) {
                Ok((_, woke)) => self.flash(format!(
                    "Running \"{}\" now{}",
                    r.label,
                    woke.map(|w| format!(" (the tab was {w})"))
                        .unwrap_or_default()
                )),
                Err(e) => self.flash(format!("Could not run \"{}\": {e}", r.label)),
            },
            (SchedKind::Managed, OP_PAUSE) => {
                let (paused, ended) = self
                    .jobs
                    .find(key)
                    .map(|j| (!j.enabled, j.finished.is_some()))
                    .unwrap_or((false, false));
                if ended {
                    self.flash("It ended: give it a new end date with the assistant, then resume");
                    return;
                }
                self.set_paused(key, !paused);
                self.flash(if paused {
                    format!("Resumed \"{}\"", r.label)
                } else {
                    format!("Paused \"{}\"", r.label)
                });
            }
            (SchedKind::Managed, OP_DELETE) => {
                let again = self
                    .sched_confirm
                    .as_ref()
                    .is_some_and(|(k, at)| k == key && at.elapsed() < CONFIRM);
                if again {
                    self.sched_confirm = None;
                    self.jobs.remove(key);
                    self.job_runs.retain(|x| x.job != key);
                    self.jobs_save();
                    self.flash(format!("Deleted the job \"{}\"", r.label));
                } else {
                    self.sched_confirm = Some((key.to_string(), Instant::now()));
                    self.flash(format!(
                        "Delete \"{}\"? Click delete again to delete it for good",
                        r.label
                    ));
                }
            }
            (_, OP_OPEN) => self.sched_open(key),
            (SchedKind::Managed, _) => {}
            (_, OP_ADOPT) => {
                self.sched_adopt(key);
            }
            (_, OP_RUN) => self.sched_run_loop(key),
            (_, OP_STOP) => {
                self.sched_stop_loop(key);
            }
            _ => {}
        }
    }

    /// Whether the delete button of this row waits for its second click.
    pub fn sched_delete_armed(&self, key: &str) -> bool {
        self.sched_confirm
            .as_ref()
            .is_some_and(|(k, at)| k == key && at.elapsed() < CONFIRM)
    }

    /// Open or close the section (kept in the state).
    pub fn toggle_sched(&mut self) {
        self.sched_open = !self.sched_open;
        self.state_dirty = true;
    }

    fn set_paused(&mut self, id: &str, pause: bool) {
        let now = self.job_now();
        let Some(j) = self.jobs.find_mut(id) else {
            return;
        };
        j.enabled = !pause;
        if !pause {
            // Resumed: the next regular time (what passed while paused
            // is not a missed run).
            j.next_run = jobs::next_after(j, now, None);
        }
        self.state_dirty = true;
        self.jobs_save();
    }

    // ---- adopting a session loop ----

    /// Turn the session loop `loop_id` of tab `uid` into a job. `cancel`:
    /// ask the tab to cancel its own loop.
    pub fn adopt_loop_now(
        &mut self,
        uid: u64,
        loop_id: &str,
        over: Option<Job>,
        cancel: bool,
    ) -> Result<Job, String> {
        let row = self
            .loops
            .iter()
            .find(|r| r.uid == uid && r.lp.id == loop_id)
            .cloned()
            .ok_or("that loop is not running any more")?;
        let (s, i) = self.find_tab(uid).ok_or("its tab is closed")?;
        let mut j = match over {
            Some(j) => j,
            None => self.job_from_loop(&row, s, i, "user")?,
        };
        j.id = String::new();
        let id = self.jobs.add(j);
        let job = self.jobs.find(&id).cloned().ok_or("not saved")?;
        self.jobs_save();
        if cancel {
            self.stop_loops(uid, vec![loop_id.to_string()]);
        }
        self.state_dirty = true;
        Ok(job)
    }

    /// The job a session loop becomes.
    pub fn job_from_loop(
        &self,
        row: &crate::app_loops::LoopRow,
        s: usize,
        i: usize,
        by: &str,
    ) -> Result<Job, String> {
        let l = &row.lp;
        if l.prompt.trim().is_empty() {
            return Err("that loop has no prompt".into());
        }
        let now = self.job_now();
        let (mut cron, mut every, mut at) = (None, None, None);
        if let Some(e) = &l.every {
            every = jobs::parse_every(e);
            if every.is_none() {
                return Err(format!(
                    "I could not read its interval \"{e}\": pass cron or every"
                ));
            }
            if !l.recurring {
                at = every.map(|m| now + i64::from(m) * 60);
                every = None;
            }
        } else if let Some(c) = &l.cron {
            if l.recurring {
                cron = Some(c.clone());
            } else {
                // A one time cron: its next time.
                at = crate::loops::next_fire(c, Local::now()).map(|t| t.timestamp());
                if at.is_none() {
                    return Err("that one time loop has no later time".into());
                }
            }
        } else {
            return Err(
                "a dynamic /loop has no fixed schedule: give a cron or an interval (every)".into(),
            );
        }
        let mut j = Job {
            id: String::new(),
            name: head(&l.prompt, 40),
            prompt: l.prompt.clone(),
            cron,
            every_min: every,
            at,
            target: self.job_target_of(s, i),
            created_by: by.into(),
            enabled: true,
            catch_up: CatchUp::Once,
            created_at: now,
            last_run: None,
            last_result: None,
            next_run: None,
            end_at: None,
            warned_end: false,
            runs: 0,
            finished: None,
            adopted_from: Some(l.id.clone()),
        };
        j.next_run = jobs::next_after(&j, now, None).or(j.at);
        Ok(j)
    }

    // ---- the assistant's tools ----

    fn job_state_words(&self, j: &Job) -> &'static str {
        match (&j.finished, j.enabled) {
            (Some(f), _) if f == "ended" => "ended",
            (Some(_), _) => "completed",
            (None, false) => "paused",
            _ => "active",
        }
    }

    pub fn job_json(&self, j: &Job) -> Value {
        let now = self.job_now();
        let mut v = json!({
            "id": j.id,
            "name": j.name,
            "prompt": head(&j.prompt, 200),
            "schedule": j.cadence(),
            "status": self.job_state_words(j),
            "tab": {
                "name": j.target.label,
                "id": self.job_tab(&j.target).map(|(s, i)| tab_id(self.panes[s].tabs[i].uid)),
                "is": self.job_target_state(&j.target),
                "account": j.target.account,
                "provider": j.target.provider,
                "folder": crate::config::tilde(std::path::Path::new(&j.target.cwd)),
            },
            "catch_up": j.catch_up.word(),
            "created_by": j.created_by,
            "runs": j.runs,
        });
        if let Some(n) = j.next_run.filter(|_| j.active()) {
            v["next_run"] = json!(format!("{} ({})", jobs::local_words(n), rel_future(n, now)));
        }
        if let Some(t) = j.last_run {
            v["last_run"] = json!(format!("{} ({})", jobs::local_words(t), ago(t, now)));
        }
        if let Some(r) = &j.last_result {
            v["last_result"] =
                json!({"state": r.state, "detail": r.detail, "when": ago(r.at, now)});
        }
        if let Some(e) = j.end_at {
            v["ends"] = json!(jobs::local_words(e));
        }
        if let Some(a) = &j.adopted_from {
            v["adopted_from_loop"] = json!(a);
        }
        v
    }

    /// get_state: a summary the assistant always has.
    pub fn jobs_state_json(&self) -> Value {
        let now = self.job_now();
        let active: Vec<&Job> = self.jobs.jobs.iter().filter(|j| j.active()).collect();
        let failing = self
            .jobs
            .jobs
            .iter()
            .filter(|j| j.last_result.as_ref().is_some_and(RunResult::failed))
            .count();
        let next = active
            .iter()
            .filter_map(|j| j.next_run.map(|n| (n, j)))
            .min_by_key(|(n, _)| *n);
        json!({
            "count": self.jobs.jobs.len(),
            "active": active.len(),
            "paused": self.jobs.jobs.iter().filter(|j| !j.enabled && j.finished.is_none()).count(),
            "failing": failing,
            "next": next.map(|(n, j)| format!("{} {}", j.name, rel_future(n, now))),
            "jobs": self.jobs.jobs.iter().take(12).map(|j| json!({
                "id": j.id, "name": j.name, "schedule": j.cadence(),
                "status": self.job_state_words(j), "tab": j.target.label,
                "tab_is": self.job_target_state(&j.target),
                "next": j.next_run.filter(|_| j.active()).map(|n| rel_future(n, now)),
                "last": j.last_result.as_ref().map(|r| r.state.clone()),
            })).collect::<Vec<_>>(),
            "note": "GodTerm jobs are kept by GodTerm: they survive restarts and wake closed or archived tabs. Session loops (loops above) live only in their session and die when it exits: for anything that must keep running, create_job or adopt_loop.",
        })
    }

    pub fn list_jobs_json(&self, args: &Value) -> Result<Value, String> {
        let status = args.get("status").and_then(Value::as_str).unwrap_or("");
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let tab = match args.get("tab") {
            Some(Value::String(t)) => Some(t.to_lowercase()),
            _ => None,
        };
        // A tab's agent sees its own tab's only (see tab_scoped).
        let only = args.get("_only_uid").and_then(Value::as_u64);
        let jobs: Vec<Value> = self
            .jobs
            .jobs
            .iter()
            .filter(|j| only.is_none_or(|u| j.target.uid == Some(u)))
            .filter(|j| match status {
                "active" => j.active(),
                "paused" => !j.enabled && j.finished.is_none(),
                "finished" => j.finished.is_some(),
                "failing" => j.last_result.as_ref().is_some_and(RunResult::failed),
                _ => true,
            })
            .filter(|j| {
                text.is_empty()
                    || j.name.to_lowercase().contains(&text)
                    || j.prompt.to_lowercase().contains(&text)
            })
            .filter(|j| {
                tab.as_ref().is_none_or(|t| {
                    j.target.label.to_lowercase().contains(t)
                        || j.target.uid.map(tab_id).as_deref() == Some(t.as_str())
                })
            })
            .map(|j| self.job_json(j))
            .collect();
        let now = chrono::Utc::now();
        let loops: Vec<Value> = self
            .loops
            .iter()
            .filter(|r| only.is_none_or(|u| r.uid == u))
            .map(|r| {
                let kind = if r.lp.every.is_some() {
                    "grok"
                } else if r.lp.durable {
                    "durable"
                } else {
                    "session"
                };
                json!({
                    "kind": kind, "loop": r.lp.id, "tab": tab_id(r.uid),
                    "prompt": head(&r.lp.prompt, 120), "schedule": r.lp.cadence(),
                    "expires": r.lp.expires().map(|e| crate::loops::rel(e, now)),
                    "dies_with_session": kind == "session",
                    "adoptable": "adopt_loop turns it into a GodTerm job",
                })
            })
            .collect();
        let say = format!(
            "{} and {} in sessions.",
            plural(jobs.len(), "GodTerm job"),
            plural(loops.len(), "loop")
        );
        Ok(ok(
            json!({"jobs": jobs, "session_loops": loops, "say": say}),
        ))
    }

    /// The arguments of create_job / update_job, applied to `j`.
    fn job_apply(&mut self, j: &mut Job, args: &Value, creating: bool) -> Result<(), String> {
        let now = self.job_now();
        let s = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_string)
        };
        if let Some(n) = s("name") {
            j.name = head(&n, 60);
        }
        if let Some(p) = s("prompt") {
            j.prompt = p;
        }
        // A target.
        if args.get("tab").is_some_and(|v| !v.is_null()) {
            let (sl, t) = self.one_tab(args)?;
            j.target = self.job_target_of(sl, t);
        } else if let Some(a) = s("archived") {
            let e = self
                .archive
                .find(&a)
                .cloned()
                .ok_or("that conversation is not in the archive (list_archived shows them)")?;
            j.target = Target {
                uid: None,
                session_id: e.session_id.clone(),
                provider: e.provider.clone(),
                account: e.account.clone(),
                cwd: e.cwd.clone(),
                label: e.label.clone(),
            };
        }
        // A schedule: exactly one kind.
        let cron = s("cron");
        let every = match args.get("every").or(args.get("every_minutes")) {
            Some(Value::Number(n)) => Some(
                u32::try_from(n.as_u64().unwrap_or(0))
                    .ok()
                    .filter(|m| *m >= jobs::MIN_EVERY)
                    .ok_or("every: at least 1 minute")?,
            ),
            Some(Value::String(t)) if !t.trim().is_empty() => {
                Some(jobs::parse_every(t).ok_or_else(|| {
                    format!("I could not read every \"{t}\" (like \"30 minutes\" or \"2 hours\")")
                })?)
            }
            _ => None,
        };
        let at = match s("at") {
            Some(t) => Some(parse_when(&t, now, false)?),
            None => None,
        };
        let given =
            usize::from(cron.is_some()) + usize::from(every.is_some()) + usize::from(at.is_some());
        if given > 1 {
            return Err("give one schedule: cron, every or at".into());
        }
        if given == 1 {
            j.cron = None;
            j.every_min = None;
            j.at = None;
            if let Some(c) = cron {
                jobs::validate_cron(&c)?;
                j.cron = Some(c);
            }
            j.every_min = every;
            if let Some(t) = at {
                if t <= now {
                    return Err("at is in the past".into());
                }
                j.at = Some(t);
            }
            j.next_run = jobs::next_after(j, now, None).or(j.at);
        }
        if creating && !j.has_schedule() {
            return Err("give a schedule: cron, every or at".into());
        }
        if let Some(c) = s("catch_up") {
            j.catch_up = CatchUp::parse(&c).ok_or("catch_up: once or skip")?;
        }
        match args.get("until") {
            Some(Value::String(u))
                if matches!(u.trim().to_lowercase().as_str(), "none" | "never" | "") =>
            {
                j.end_at = None;
                j.warned_end = false;
            }
            Some(Value::String(u)) => {
                let e = parse_when(u, now, true)?;
                if e <= now {
                    return Err("until is in the past".into());
                }
                j.end_at = Some(e);
                j.warned_end = false;
            }
            _ => {}
        }
        if let Some(e) = j.end_at {
            if let Some(n) = j.next_run {
                if n > e && creating {
                    return Err("it would never run before its end date".into());
                }
            }
        }
        if j.name.is_empty() {
            j.name = head(&j.prompt, 40);
        }
        Ok(())
    }

    fn blank_job(&self, by: &str) -> Job {
        Job {
            id: String::new(),
            name: String::new(),
            prompt: String::new(),
            cron: None,
            every_min: None,
            at: None,
            target: Target::default(),
            created_by: by.into(),
            enabled: true,
            catch_up: CatchUp::Once,
            created_at: self.job_now(),
            last_run: None,
            last_result: None,
            next_run: None,
            end_at: None,
            warned_end: false,
            runs: 0,
            finished: None,
            adopted_from: None,
        }
    }

    /// The question for a job about to be created or changed.
    fn job_question(&self, verb: &str, j: &Job) -> String {
        let when = match j.next_run {
            Some(n) => format!(", next {}", jobs::local_words(n)),
            None => String::new(),
        };
        let end = j
            .end_at
            .map(|e| format!(", until {}", jobs::local_words(e)))
            .unwrap_or_else(|| ", with no end date".into());
        format!(
            "{verb} the job \"{}\": send \"{}\" to {} {}{when}{end}?",
            j.name,
            head(&j.prompt, 70),
            if j.target.label.is_empty() {
                "its tab"
            } else {
                &j.target.label
            },
            j.cadence(),
        )
    }

    /// (plan, question, needs a yes) for the job tools.
    pub(crate) fn jobs_plan_for(
        &mut self,
        tool: &str,
        args: &Value,
    ) -> Result<(Value, String, bool), String> {
        match tool {
            "create_job" => {
                let mut j = self.blank_job("assistant");
                if args.get("tab").is_none() && args.get("archived").is_none() {
                    return Err("tab is required: which tab should the prompt go to".into());
                }
                if args
                    .get("prompt")
                    .and_then(Value::as_str)
                    .is_none_or(|p| p.trim().is_empty())
                {
                    return Err("prompt is required: what the tab should be asked each time".into());
                }
                self.job_apply(&mut j, args, true)?;
                let q = self.job_question("Create", &j);
                Ok((json!({"op": "create", "job": j}), q, true))
            }
            "update_job" => {
                let key = args
                    .get("job")
                    .or(args.get("id"))
                    .and_then(Value::as_str)
                    .ok_or("job is required (an id or name from list_jobs)")?;
                let old = self.jobs.resolve(key)?.clone();
                let mut j = old.clone();
                let action = args.get("action").and_then(Value::as_str).unwrap_or("");
                self.job_apply(&mut j, args, false)?;
                match action {
                    "pause" => j.enabled = false,
                    "resume" => {
                        if j.finished.as_deref() == Some("ended") && j.end_at == old.end_at {
                            return Err(
                                "it reached its end date: give a later until to resume it".into()
                            );
                        }
                        if j.at.is_some_and(|t| t <= self.job_now()) && j.finished.is_some() {
                            return Err("its one time already ran: give a new at".into());
                        }
                        j.enabled = true;
                        j.finished = None;
                        j.next_run = jobs::next_after(&j, self.job_now(), None).or(j.at);
                    }
                    "" => {}
                    _ => return Err("action: pause or resume".into()),
                }
                if j == old {
                    return Err("nothing to change: say what (action, name, prompt, cron, every, at, tab, catch_up, until)".into());
                }
                let outward = j.prompt != old.prompt
                    || j.cron != old.cron
                    || j.every_min != old.every_min
                    || j.at != old.at
                    || j.target != old.target
                    || j.end_at != old.end_at;
                let q = self.job_question("Change", &j);
                Ok((json!({"op": "update", "id": old.id, "job": j}), q, outward))
            }
            "delete_job" => {
                let key = args
                    .get("job")
                    .or(args.get("id"))
                    .and_then(Value::as_str)
                    .ok_or("job is required (an id or name from list_jobs)")?;
                let j = self.jobs.resolve(key)?.clone();
                let q = format!(
                    "Delete the job \"{}\" ({} to {}) for good?",
                    j.name,
                    j.cadence(),
                    j.target.label
                );
                Ok((json!({"op": "delete", "id": j.id}), q, true))
            }
            "adopt_loop" => {
                let (s, t) = self.one_tab(args)?;
                let uid = self.panes[s].tabs[t].uid;
                let here: Vec<crate::app_loops::LoopRow> = self
                    .loops
                    .iter()
                    .filter(|r| r.uid == uid)
                    .cloned()
                    .collect();
                let want = args.get("loop").and_then(Value::as_str);
                let row = match (want, here.len()) {
                    (Some(id), _) => here.iter().find(|r| r.lp.id == id).cloned(),
                    (None, 1) => here.first().cloned(),
                    (None, 0) => {
                        return Err("that tab has no session loops (get_state, loops)".into())
                    }
                    (None, _) => {
                        return Err(format!(
                            "that tab has {} loops ({}): pass loop",
                            here.len(),
                            here.iter()
                                .map(|r| r.lp.id.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                }
                .ok_or("that tab has no such loop")?;
                let mut j = self.job_from_loop_args(&row, s, t, args)?;
                self.job_apply(&mut j, args, false)?;
                if !j.has_schedule() {
                    return Err("give a schedule: cron, every or at".into());
                }
                j.created_by = "assistant".into();
                let keep = args
                    .get("keep_session_loop")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let q = format!(
                    "{}{}",
                    self.job_question("Adopt the loop as", &j),
                    if keep {
                        String::new()
                    } else {
                        " The tab's own loop is cancelled so it does not fire twice.".into()
                    }
                );
                Ok((
                    json!({"op": "adopt", "uid": uid, "loop": row.lp.id, "cancel": !keep, "job": j}),
                    q,
                    true,
                ))
            }
            _ => Err(format!("{tool} is not a job tool")),
        }
    }

    /// A job from a loop, with the schedule from the arguments when the
    /// loop has none (a dynamic /loop).
    fn job_from_loop_args(
        &self,
        row: &crate::app_loops::LoopRow,
        s: usize,
        t: usize,
        args: &Value,
    ) -> Result<Job, String> {
        match self.job_from_loop(row, s, t, "assistant") {
            Ok(j) => Ok(j),
            Err(e)
                if row.lp.cron.is_none()
                    && row.lp.every.is_none()
                    && (args.get("cron").is_some()
                        || args.get("every").is_some()
                        || args.get("at").is_some()) =>
            {
                let _ = e;
                let mut j = self.blank_job("assistant");
                j.name = head(&row.lp.prompt, 40);
                j.prompt = row.lp.prompt.clone();
                j.target = self.job_target_of(s, t);
                j.adopted_from = Some(row.lp.id.clone());
                Ok(j)
            }
            Err(e) => Err(e),
        }
    }

    /// Run a confirmed (or harmless) job plan.
    pub(crate) fn jobs_run_plan(
        &mut self,
        tool: &str,
        plan: &Value,
    ) -> Option<Result<Value, String>> {
        if !matches!(
            tool,
            "create_job" | "update_job" | "delete_job" | "adopt_loop"
        ) {
            return None;
        }
        Some(self.jobs_run_plan_inner(plan))
    }

    fn jobs_run_plan_inner(&mut self, plan: &Value) -> Result<Value, String> {
        let job = |p: &Value| -> Result<Job, String> {
            serde_json::from_value(p["job"].clone()).map_err(|e| format!("bad plan: {e}"))
        };
        match plan["op"].as_str().unwrap_or("") {
            "create" => {
                let j = job(plan)?;
                let id = self.jobs.add(j);
                self.jobs_save();
                let j = self.jobs.find(&id).cloned().ok_or("not saved")?;
                self.state_dirty = true;
                self.flash(format!("Job \"{}\" created: {}", j.name, j.cadence()));
                Ok(ok(json!({
                    "created": self.job_json(&j),
                    "say": format!("Done: \"{}\" runs {}{}. GodTerm keeps it, so it survives restarts and wakes the tab if it is closed or archived.", j.name, j.cadence(),
                        j.next_run.map(|n| format!(", first at {}", jobs::local_words(n))).unwrap_or_default()),
                })))
            }
            "update" => {
                let new = job(plan)?;
                let id = plan["id"].as_str().unwrap_or("").to_string();
                let slot = self.jobs.find_mut(&id).ok_or("that job is gone")?;
                *slot = new;
                let j = slot.clone();
                self.jobs_save();
                self.state_dirty = true;
                Ok(ok(json!({"updated": self.job_json(&j),
                    "say": format!("Updated \"{}\": {}, {}.", j.name, j.cadence(), self.job_state_words(&j))})))
            }
            "delete" => {
                let id = plan["id"].as_str().unwrap_or("").to_string();
                let j = self.jobs.remove(&id).ok_or("that job is gone")?;
                self.job_runs.retain(|r| r.job != id);
                self.jobs_save();
                self.state_dirty = true;
                Ok(ok(
                    json!({"deleted": j.id, "say": format!("Deleted the job \"{}\".", j.name)}),
                ))
            }
            "adopt" => {
                let uid = plan["uid"].as_u64().unwrap_or(0);
                let lp = plan["loop"].as_str().unwrap_or("").to_string();
                let cancel = plan["cancel"].as_bool().unwrap_or(true);
                let j = self.adopt_loop_now(uid, &lp, Some(job(plan)?), cancel)?;
                self.flash(format!("Job \"{}\" adopted from a session loop", j.name));
                Ok(ok(json!({
                    "adopted": self.job_json(&j),
                    "session_loop_cancelled": cancel,
                    "say": format!("GodTerm now keeps \"{}\" ({}).{}", j.name, j.cadence(),
                        if cancel { " The tab is being asked to cancel its own loop, which dies with the session." } else { " The session's own loop still runs too, so it may fire twice." }),
                })))
            }
            o => Err(format!("bad plan: {o}")),
        }
    }

    pub fn run_job_now_tool(&mut self, args: &Value) -> Result<Value, String> {
        let key = args
            .get("job")
            .or(args.get("id"))
            .and_then(Value::as_str)
            .ok_or("job is required (an id or name from list_jobs)")?;
        let id = self.jobs.resolve(key)?.id.clone();
        let name = self.job_name(&id);
        let (d, woke) = self.fire_job(&id, None, true)?;
        self.jobs_follow();
        let state = self
            .deliveries
            .iter()
            .find(|x| x.id == d)
            .map(|x| x.state_word())
            .unwrap_or("sent");
        Ok(ok(json!({
            "job": id, "delivery": d, "state": state,
            "woke_tab": woke,
            "say": format!("Sent \"{name}\" to its tab{}; its state is {state}.",
                woke.map(|w| format!(" (the tab was {w} first)")).unwrap_or_default()),
        })))
    }
}
