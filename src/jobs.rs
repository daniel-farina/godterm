//! GodTerm-managed jobs: scheduled prompts that GodTerm itself keeps.
//!
//! Claude's own loops (CronCreate) live in one session's memory and die
//! with it (recurring ones also expire after 7 days); durable ones in
//! `<project>/.claude/scheduled_tasks.json` fire only while a claude
//! session runs in that project. A job here is kept in
//! `~/.godterm/jobs.json` (owner only, written atomically) and fired by
//! GodTerm's own clock: it delivers the prompt to its target tab, waking
//! the tab first when it was closed or archived. This file is the pure
//! part: the record, the store and the schedule (what is due at an
//! injected `now`, what a missed run does); app_jobs.rs runs it.

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A run later than this after its time counts as missed (a restart, a
/// sleep): the catch-up policy decides. Ticks are far finer.
pub const GRACE_S: i64 = 120;
/// A job with an end date is warned about this long before it.
pub const WARN_BEFORE_S: i64 = 24 * 3600;
/// The shortest interval.
pub const MIN_EVERY: u32 = 1;

/// What happens to a run that was due while GodTerm was not running (or
/// the Mac slept).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchUp {
    /// Run it once now, however many were missed.
    #[default]
    Once,
    /// Forget them; wait for the next regular time.
    Skip,
}

impl CatchUp {
    pub fn parse(s: &str) -> Option<CatchUp> {
        match s.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
            "once" | "run_once" | "run_once_if_missed" | "catch_up" => Some(CatchUp::Once),
            "skip" | "skip_missed" | "none" => Some(CatchUp::Skip),
            _ => None,
        }
    }
    pub fn word(self) -> &'static str {
        match self {
            CatchUp::Once => "once",
            CatchUp::Skip => "skip",
        }
    }
}

/// Where a job's prompt goes. A tab is found by its id while it is open,
/// else by its session (open elsewhere, archived, recently closed), else
/// the session is resumed in its folder on its account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Target {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// claude or grok.
    #[serde(default)]
    pub provider: String,
    /// Account name (config `name`).
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub cwd: String,
    /// What the tab was called.
    #[serde(default)]
    pub label: String,
}

/// How the last run went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    /// sent, received, started, done, failed, skipped or missed.
    pub state: String,
    #[serde(default)]
    pub detail: String,
    /// Unix seconds.
    pub at: i64,
}

impl RunResult {
    /// Nothing more will change.
    pub fn is_final(&self) -> bool {
        matches!(
            self.state.as_str(),
            "done" | "failed" | "skipped" | "unconfirmed"
        )
    }
    pub fn failed(&self) -> bool {
        self.state == "failed"
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Short id for the assistant ("a3f9c1d2").
    pub id: String,
    pub name: String,
    pub prompt: String,
    /// Five field cron expression (local time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// Every N minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_min: Option<u32>,
    /// One run at this Unix time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    pub target: Target,
    /// user or assistant.
    #[serde(default)]
    pub created_by: String,
    pub enabled: bool,
    #[serde(default)]
    pub catch_up: CatchUp,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<RunResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<i64>,
    /// No runs after this (Unix seconds); warned about a day before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_at: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub warned_end: bool,
    #[serde(default)]
    pub runs: u32,
    /// Why it stopped by itself: "ended" (its end date passed) or
    /// "completed" (a one time job ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    /// Adopted from a session loop: its id there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted_from: Option<String>,
    /// The machine it runs on, for a job with no session (GodTerm Sync;
    /// a job with a session runs where its lease is).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

impl Job {
    /// "every 15 minutes", "daily at 09:00", "once at Oct 14 15:00".
    pub fn cadence(&self) -> String {
        if let Some(n) = self.every_min {
            return every_words(n);
        }
        if let Some(c) = &self.cron {
            return crate::loops::human_cron(c);
        }
        if let Some(t) = self.at {
            return format!("once at {}", local_words(t));
        }
        "no schedule".into()
    }

    /// Armed: will run again by itself.
    pub fn active(&self) -> bool {
        self.enabled && self.finished.is_none()
    }

    /// The schedule is valid (exactly one kind).
    pub fn has_schedule(&self) -> bool {
        usize::from(self.cron.is_some())
            + usize::from(self.every_min.is_some())
            + usize::from(self.at.is_some())
            == 1
    }
}

/// "Oct 14 15:00" in local time.
pub fn local_words(t: i64) -> String {
    Local
        .timestamp_opt(t, 0)
        .single()
        .map(|d| d.format("%b %-d %H:%M").to_string())
        .unwrap_or_default()
}

/// 15 -> "every 15 minutes", 120 -> "every 2 hours".
pub fn every_words(n: u32) -> String {
    if n == 1 {
        "every minute".into()
    } else if n.is_multiple_of(1440) {
        if n == 1440 {
            "every day".into()
        } else {
            format!("every {} days", n / 1440)
        }
    } else if n.is_multiple_of(60) {
        if n == 60 {
            "every hour".into()
        } else {
            format!("every {} hours", n / 60)
        }
    } else {
        format!("every {n} minutes")
    }
}

/// An interval in words ("every 5 minutes", "2 hours", "30m", "1d") in
/// minutes. This is how Grok's tasks and the user's phrases give it.
pub fn parse_every(s: &str) -> Option<u32> {
    let t = s.trim().to_lowercase();
    let t = t.trim_start_matches("every").trim();
    if t == "minute" || t == "min" {
        return Some(1);
    }
    if t == "hour" {
        return Some(60);
    }
    if t == "day" {
        return Some(1440);
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: u32 = digits.parse().ok().filter(|n| *n > 0)?;
    let unit = t[digits.len()..].trim();
    let mult = match unit {
        "m" | "min" | "mins" | "minute" | "minutes" => 1,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60,
        "d" | "day" | "days" => 1440,
        _ => return None,
    };
    n.checked_mul(mult)
}

fn is_cron_field(f: &str) -> bool {
    !f.is_empty()
        && f.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '*' | ',' | '/' | '-'))
}

/// Check a five field cron expression: shape and that it fires within a
/// year.
pub fn validate_cron(expr: &str) -> Result<(), String> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 || !f.iter().all(|x| is_cron_field(x)) {
        return Err(format!(
            "\"{expr}\" is not a five field cron expression (minute hour day month weekday, like 0 9 * * 1-5)"
        ));
    }
    if next_cron(expr, Local::now()).is_none() {
        return Err(format!("\"{expr}\" never fires within a year"));
    }
    Ok(())
}

/// The next time a cron expression fires after `after`, up to 400 days
/// ahead (loops.rs looks 8 days: enough for a session loop, not for a
/// monthly job).
pub fn next_cron(expr: &str, after: DateTime<Local>) -> Option<DateTime<Local>> {
    if let Some(t) = crate::loops::next_fire(expr, after) {
        return Some(t);
    }
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return None;
    }
    let first = after.date_naive();
    for off in 0..=400i64 {
        let d = first + chrono::Duration::days(off);
        let dow = d.weekday().num_days_from_sunday();
        let day_ok = crate::loops::field_matches(f[2], d.day(), 1)
            && crate::loops::field_matches(f[3], d.month(), 1)
            && (crate::loops::field_matches(f[4], dow, 0)
                || (dow == 0 && crate::loops::field_matches(f[4], 7, 0)));
        if !day_ok {
            continue;
        }
        for h in 0..24 {
            if !crate::loops::field_matches(f[1], h, 0) {
                continue;
            }
            for m in 0..60 {
                if !crate::loops::field_matches(f[0], m, 0) {
                    continue;
                }
                let Some(t) = Local
                    .with_ymd_and_hms(d.year(), d.month(), d.day(), h, m, 0)
                    .earliest()
                else {
                    continue;
                };
                if t > after.with_second(0).unwrap_or(after) && t > after {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// The next scheduled time strictly after `now` (Unix seconds). An
/// interval keeps its rhythm from `anchor` (the time it was due), so a
/// late tick does not drift it.
pub fn next_after(j: &Job, now: i64, anchor: Option<i64>) -> Option<i64> {
    if let Some(n) = j.every_min {
        let step = i64::from(n.max(MIN_EVERY)) * 60;
        let a = anchor.unwrap_or(now);
        if a > now {
            return Some(a);
        }
        let k = (now - a) / step + 1;
        return Some(a + k * step);
    }
    if let Some(c) = &j.cron {
        let local = Local.timestamp_opt(now, 0).single()?;
        return next_cron(c, local).map(|t| t.timestamp());
    }
    j.at.filter(|t| *t > now)
}

/// What the clock asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Run it now. `missed_by`: seconds late when it was missed and the
    /// policy says run once.
    Fire { id: String, missed_by: Option<i64> },
    /// A missed run forgotten by the skip policy.
    Skipped { id: String, missed_by: i64 },
    /// Its end date passed: it stopped.
    Ended { id: String },
    /// Its end date is near (said once).
    EndSoon { id: String, at: i64 },
    /// A one time job ran.
    Completed { id: String },
}

/// Advance every job to `now`: what is due, what was missed, what ended.
/// Pure: the caller delivers the events.
pub fn due(jobs: &mut [Job], now: i64) -> Vec<Event> {
    let mut ev = vec![];
    for j in jobs.iter_mut() {
        if !j.active() {
            continue;
        }
        if let Some(end) = j.end_at {
            if now >= end {
                j.enabled = false;
                j.finished = Some("ended".into());
                j.next_run = None;
                ev.push(Event::Ended { id: j.id.clone() });
                continue;
            }
            if end - now <= WARN_BEFORE_S && !j.warned_end {
                j.warned_end = true;
                ev.push(Event::EndSoon {
                    id: j.id.clone(),
                    at: end,
                });
            }
        }
        let Some(n) = j.next_run else {
            j.next_run = next_after(j, now, None);
            continue;
        };
        if n > now {
            continue;
        }
        let late = now - n;
        let one_time = j.at.is_some();
        j.next_run = if one_time {
            None
        } else {
            next_after(j, now, Some(n))
        };
        let missed = late > GRACE_S;
        if missed && j.catch_up == CatchUp::Skip {
            j.last_result = Some(RunResult {
                state: "skipped".into(),
                detail: format!(
                    "missed by {} and skipped (catch up: skip)",
                    span_words(late)
                ),
                at: now,
            });
            if one_time {
                j.enabled = false;
                j.finished = Some("completed".into());
            }
            ev.push(Event::Skipped {
                id: j.id.clone(),
                missed_by: late,
            });
            continue;
        }
        ev.push(Event::Fire {
            id: j.id.clone(),
            missed_by: missed.then_some(late),
        });
        if one_time {
            j.enabled = false;
            j.finished = Some("completed".into());
            ev.push(Event::Completed { id: j.id.clone() });
        }
    }
    ev
}

/// 90 -> "1 minute", 7300 -> "2 hours".
pub fn span_words(s: i64) -> String {
    let s = s.max(0);
    let (n, u) = if s < 120 {
        (s, "second")
    } else if s < 7200 {
        (s / 60, "minute")
    } else if s < 172_800 {
        (s / 3600, "hour")
    } else {
        (s / 86_400, "day")
    };
    format!("{n} {u}{}", if n == 1 { "" } else { "s" })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub jobs: Vec<Job>,
}

pub fn path() -> PathBuf {
    crate::config::app_home().join("jobs.json")
}

impl Store {
    /// The saved jobs; a damaged file is moved aside (recover.rs), not
    /// overwritten by the next save.
    pub fn load() -> Store {
        crate::recover::load_json(&path()).unwrap_or_default()
    }

    /// Write atomically (a temp file, then rename), owner only.
    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        crate::config::write_private(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, p)
    }

    pub fn add(&mut self, mut j: Job) -> String {
        if j.id.is_empty() || self.jobs.iter().any(|x| x.id == j.id) {
            j.id = crate::session_ops::new_uuid()[..8].to_string();
        }
        let id = j.id.clone();
        self.jobs.push(j);
        id
    }

    pub fn find(&self, id: &str) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// A job by id, a unique id prefix, or its name.
    pub fn resolve(&self, key: &str) -> Result<&Job, String> {
        let k = key.trim();
        if let Some(j) = self.find(k) {
            return Ok(j);
        }
        let by_prefix: Vec<&Job> = self
            .jobs
            .iter()
            .filter(|j| !k.is_empty() && j.id.starts_with(k))
            .collect();
        if by_prefix.len() == 1 {
            return Ok(by_prefix[0]);
        }
        let lk = k.to_lowercase();
        let by_name: Vec<&Job> = self
            .jobs
            .iter()
            .filter(|j| !lk.is_empty() && j.name.to_lowercase().contains(&lk))
            .collect();
        match by_name.len() {
            1 => Ok(by_name[0]),
            0 => Err(format!("no job \"{key}\" (list_jobs shows them)")),
            _ => Err(format!(
                "\"{key}\" matches {} jobs ({}): use an id",
                by_name.len(),
                by_name
                    .iter()
                    .map(|j| format!("{} {}", j.id, j.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<Job> {
        let i = self.jobs.iter().position(|j| j.id == id)?;
        Some(self.jobs.remove(i))
    }
}

/// A new job's moment of creation.
pub fn now_ts() -> i64 {
    Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::testing::LOCK as ENV_LOCK;

    fn job(id: &str) -> Job {
        Job {
            id: id.into(),
            name: "morning check".into(),
            prompt: "status of the build?".into(),
            cron: None,
            every_min: Some(15),
            at: None,
            target: Target {
                uid: Some(7),
                session_id: Some("sid-1".into()),
                provider: "claude".into(),
                account: "work".into(),
                cwd: "/w/api".into(),
                label: "api".into(),
            },
            created_by: "assistant".into(),
            enabled: true,
            catch_up: CatchUp::Once,
            created_at: 1_000_000,
            last_run: None,
            last_result: None,
            next_run: Some(1_000_000 + 900),
            end_at: None,
            warned_end: false,
            runs: 0,
            finished: None,
            adopted_from: None,
            machine: None,
        }
    }

    #[test]
    fn store_round_trips_atomically_and_owner_only() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("godterm-jobs-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        crate::config::testing::set_home(&home);
        assert!(Store::load().jobs.is_empty());
        let mut s = Store::default();
        let mut j = job("j1");
        j.last_result = Some(RunResult {
            state: "done".into(),
            detail: "".into(),
            at: 5,
        });
        j.end_at = Some(9_999_999);
        s.add(j);
        let mut c = job("j2");
        c.every_min = None;
        c.cron = Some("0 9 * * 1-5".into());
        c.catch_up = CatchUp::Skip;
        s.add(c);
        s.save().unwrap();
        assert!(!path().with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "owner only");
        }
        let back = Store::load();
        assert_eq!(back, s);
        assert_eq!(back.find("j2").unwrap().catch_up, CatchUp::Skip);
        // Garbage reads as empty, never panics.
        std::fs::write(path(), "{nope").unwrap();
        assert!(Store::load().jobs.is_empty());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn ids_stay_unique_and_resolve_by_prefix_or_name() {
        let mut s = Store::default();
        let a = s.add(job("j1"));
        let b = s.add(job("j1"));
        assert_ne!(a, b);
        s.find_mut(&b).unwrap().name = "weekly report".into();
        assert_eq!(s.resolve(&a).unwrap().id, a);
        assert_eq!(s.resolve("weekly").unwrap().id, b);
        assert!(s.resolve("zzz").is_err());
        // Two names that match need an id.
        s.find_mut(&a).unwrap().name = "weekly digest".into();
        assert!(s.resolve("weekly").unwrap_err().contains("matches 2"));
    }

    #[test]
    fn intervals_and_words() {
        assert_eq!(parse_every("every 5 minutes"), Some(5));
        assert_eq!(parse_every("2 hours"), Some(120));
        assert_eq!(parse_every("30m"), Some(30));
        assert_eq!(parse_every("every day"), Some(1440));
        assert_eq!(parse_every("1d"), Some(1440));
        assert_eq!(parse_every("soon"), None);
        assert_eq!(parse_every("0m"), None);
        assert_eq!(every_words(15), "every 15 minutes");
        assert_eq!(every_words(120), "every 2 hours");
        assert_eq!(every_words(60), "every hour");
        assert_eq!(every_words(1), "every minute");
        assert_eq!(job("x").cadence(), "every 15 minutes");
    }

    #[test]
    fn cron_is_checked_and_found_far_ahead() {
        assert!(validate_cron("0 9 * * 1-5").is_ok());
        assert!(validate_cron("*/10 * * * *").is_ok());
        assert!(validate_cron("tomorrow").is_err());
        assert!(validate_cron("0 9 * *").is_err());
        assert!(validate_cron("0 9 31 2 *").is_err(), "Feb 31 never fires");
        // A monthly job is beyond loops.rs' 8 days.
        let now = Local.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap();
        let t = next_cron("30 8 1 * *", now).unwrap();
        assert_eq!((t.month(), t.day(), t.hour(), t.minute()), (11, 1, 8, 30));
        // A yearly one.
        let t = next_cron("0 0 25 12 *", now).unwrap();
        assert_eq!((t.year(), t.month(), t.day()), (2026, 12, 25));
    }

    #[test]
    fn an_interval_keeps_its_rhythm_from_when_it_was_due() {
        let mut j = job("a");
        j.every_min = Some(10);
        // Due at 1000, handled 45 s late: next is 1600, not 1645.
        assert_eq!(next_after(&j, 1045, Some(1000)), Some(1600));
        // Several periods behind: the next regular time after now.
        assert_eq!(next_after(&j, 1000 + 3 * 600 + 5, Some(1000)), Some(3400));
        assert_eq!(next_after(&j, 50, None), Some(650));
    }

    #[test]
    fn due_fires_on_time_and_schedules_the_next() {
        let mut jobs = vec![job("a")];
        jobs[0].next_run = Some(2000);
        assert!(due(&mut jobs, 1999).is_empty());
        let ev = due(&mut jobs, 2005);
        assert_eq!(
            ev,
            vec![Event::Fire {
                id: "a".into(),
                missed_by: None
            }]
        );
        assert_eq!(jobs[0].next_run, Some(2900));
        // Not again until then.
        assert!(due(&mut jobs, 2100).is_empty());
    }

    #[test]
    fn a_missed_run_follows_the_catch_up_policy() {
        // Once: many periods missed, one run, then the regular rhythm.
        let mut jobs = vec![job("a"), job("b")];
        jobs[0].next_run = Some(2000);
        jobs[1].next_run = Some(2000);
        jobs[1].catch_up = CatchUp::Skip;
        let ev = due(&mut jobs, 2000 + 5 * 900 + 30);
        assert_eq!(
            ev[0],
            Event::Fire {
                id: "a".into(),
                missed_by: Some(5 * 900 + 30)
            }
        );
        assert_eq!(
            ev[1],
            Event::Skipped {
                id: "b".into(),
                missed_by: 5 * 900 + 30
            }
        );
        assert_eq!(ev.len(), 2);
        assert_eq!(jobs[0].next_run, Some(2000 + 6 * 900));
        assert_eq!(jobs[1].next_run, Some(2000 + 6 * 900));
        let r = jobs[1].last_result.as_ref().unwrap();
        assert_eq!(r.state, "skipped");
        assert!(r.detail.contains("missed by"), "{}", r.detail);
        // Within the grace it is simply on time.
        let mut j = vec![job("c")];
        j[0].next_run = Some(3000);
        j[0].catch_up = CatchUp::Skip;
        let ev = due(&mut j, 3000 + GRACE_S);
        assert_eq!(
            ev,
            vec![Event::Fire {
                id: "c".into(),
                missed_by: None
            }]
        );
    }

    #[test]
    fn paused_jobs_do_not_fire_and_one_time_jobs_complete() {
        let mut jobs = vec![job("p"), job("o")];
        jobs[0].enabled = false;
        jobs[0].next_run = Some(10);
        jobs[1].every_min = None;
        jobs[1].at = Some(500);
        jobs[1].next_run = Some(500);
        let ev = due(&mut jobs, 505);
        assert_eq!(
            ev,
            vec![
                Event::Fire {
                    id: "o".into(),
                    missed_by: None
                },
                Event::Completed { id: "o".into() }
            ]
        );
        assert!(!jobs[1].active());
        assert_eq!(jobs[1].finished.as_deref(), Some("completed"));
        assert!(due(&mut jobs, 100_000).is_empty());
    }

    #[test]
    fn an_end_date_warns_once_then_stops_it_loudly() {
        let mut jobs = vec![job("e")];
        jobs[0].next_run = Some(1_000_000 + 900);
        jobs[0].end_at = Some(1_000_000 + 3 * 86_400);
        // Far from the end: nothing.
        assert!(due(&mut jobs, 1_000_000).is_empty());
        // Inside the last day: the warning, once, and it still runs.
        let t = 1_000_000 + 2 * 86_400 + 3600;
        jobs[0].next_run = Some(t);
        let ev = due(&mut jobs, t + 1);
        assert!(ev.contains(&Event::EndSoon {
            id: "e".into(),
            at: 1_000_000 + 3 * 86_400
        }));
        assert!(ev.iter().any(|e| matches!(e, Event::Fire { .. })));
        let ev = due(&mut jobs, t + 2000);
        assert!(!ev.iter().any(|e| matches!(e, Event::EndSoon { .. })));
        // Past it: ended, not silently dropped.
        let ev = due(&mut jobs, 1_000_000 + 3 * 86_400 + 1);
        assert_eq!(ev, vec![Event::Ended { id: "e".into() }]);
        assert_eq!(jobs[0].finished.as_deref(), Some("ended"));
        assert!(jobs[0].next_run.is_none());
    }

    #[test]
    fn a_job_without_a_next_time_gets_one() {
        let mut jobs = vec![job("n")];
        jobs[0].next_run = None;
        assert!(due(&mut jobs, 5000).is_empty());
        assert_eq!(jobs[0].next_run, Some(5900));
    }
}
