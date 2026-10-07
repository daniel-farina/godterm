//! Loops: scheduled prompts in running claude sessions.
//!
//! claude 2.1.29x keeps `CronCreate` jobs in memory by default
//! ("Session-only (not written to disk, dies when Claude exits)"); only
//! `durable: true` ones go to `<project>/.claude/scheduled_tasks.json`.
//! So loops are rebuilt from each running tab's transcript:
//! - `CronCreate {cron, prompt, recurring}` with the result "Scheduled
//!   recurring job <id> (...)" or "Scheduled one-shot task <id> (...)";
//! - `CronDelete {id}` with "Cancelled job <id>.";
//! - fires: user entries with `"turnOrigin":"scheduled"` and
//!   `"scheduledTaskId":"<id>"`;
//! - `ScheduleWakeup {delaySeconds, prompt}` (a dynamic `/loop`), stopped
//!   by `ScheduleWakeup {stop: true}`.
//!
//! Recurring session jobs auto-expire 7 days after creation; one-shots
//! delete themselves after firing.

use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const EXPIRY_DAYS: i64 = 7;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Cron,
    /// A dynamic `/loop` driven by ScheduleWakeup.
    Wakeup,
}

/// How sure we are the loop is still armed.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub enum Confidence {
    /// From the job's own scheduling result or the durable file.
    High,
    /// Inferred (dynamic loops: armed until stopped or the session ends).
    Medium,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Loop {
    pub id: String,
    pub kind: Kind,
    pub cron: Option<String>,
    pub prompt: String,
    pub recurring: bool,
    pub durable: bool,
    pub created: Option<DateTime<Utc>>,
    pub last_fire: Option<DateTime<Utc>>,
    pub fires: usize,
    /// Wakeup loops: when the next wakeup is due.
    pub wake_at: Option<DateTime<Utc>>,
    pub deleted: bool,
    pub confidence: Confidence,
}

impl Loop {
    /// Still armed at `now` (in a session that is running).
    pub fn active(&self, now: DateTime<Utc>) -> bool {
        if self.deleted {
            return false;
        }
        if self.kind == Kind::Cron && !self.recurring && self.fires > 0 {
            return false;
        }
        if self.kind == Kind::Cron && self.recurring && !self.durable {
            if let Some(c) = self.created {
                if now - c > Duration::days(EXPIRY_DAYS) {
                    return false;
                }
            }
        }
        true
    }

    pub fn expires(&self) -> Option<DateTime<Utc>> {
        (self.kind == Kind::Cron && self.recurring && !self.durable)
            .then(|| self.created.map(|c| c + Duration::days(EXPIRY_DAYS)))
            .flatten()
    }

    pub fn cadence(&self) -> String {
        match (&self.kind, &self.cron) {
            (Kind::Wakeup, _) => "dynamic (wakes itself)".into(),
            (_, Some(c)) => {
                let h = human_cron(c);
                if self.recurring {
                    h
                } else {
                    format!("once: {h}")
                }
            }
            _ => "?".into(),
        }
    }

    pub fn next_fire(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self.kind {
            Kind::Wakeup => self.wake_at.filter(|w| *w > now - Duration::minutes(1)),
            Kind::Cron => {
                let c = self.cron.as_ref()?;
                let local = next_fire(c, now.with_timezone(&Local))?;
                Some(local.with_timezone(&Utc))
            }
        }
    }
}

fn parse_ts(o: &Value) -> Option<DateTime<Utc>> {
    o.get("timestamp")?.as_str()?.parse().ok()
}

/// Incremental transcript scanner: keeps its place so each refresh reads
/// only what was appended.
#[derive(Debug, Default)]
pub struct Scanner {
    offset: u64,
    loops: Vec<Loop>,
    /// tool_use id -> (name, input, timestamp)
    pending: HashMap<String, (String, Value, Option<DateTime<Utc>>)>,
}

impl Scanner {
    pub fn loops(&self) -> &[Loop] {
        &self.loops
    }

    /// Read new lines from `path`.
    pub fn update(&mut self, path: &Path) {
        let Ok(mut f) = std::fs::File::open(path) else {
            return;
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = Scanner::default(); // rewritten: start over
        }
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut r = BufReader::new(f.by_ref());
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if !line.ends_with('\n') {
                        break; // half written: read it next time
                    }
                    self.offset += n as u64;
                    if line.len() > 8 << 20 {
                        continue;
                    }
                    if let Ok(o) = serde_json::from_str::<Value>(&line) {
                        self.entry(&o);
                    }
                }
            }
        }
    }

    pub fn entry(&mut self, o: &Value) {
        let ts = parse_ts(o);
        if o.get("turnOrigin").and_then(Value::as_str) == Some("scheduled") {
            if let Some(id) = o.get("scheduledTaskId").and_then(Value::as_str) {
                if let Some(l) = self.loops.iter_mut().find(|l| l.id == id) {
                    l.fires += 1;
                    l.last_fire = ts.or(l.last_fire);
                } else if let Some(l) = self
                    .loops
                    .iter_mut()
                    .rev()
                    .find(|l| l.kind == Kind::Wakeup && !l.deleted)
                {
                    // Wakeup fires carry their own task id.
                    l.fires += 1;
                    l.last_fire = ts.or(l.last_fire);
                }
            }
        }
        let Some(content) = o.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        for b in content {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                    if matches!(name, "CronCreate" | "CronDelete" | "ScheduleWakeup") {
                        let id = b
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let input = b.get("input").cloned().unwrap_or(Value::Null);
                        self.pending.insert(id, (name.to_string(), input, ts));
                    }
                }
                Some("tool_result") => {
                    let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                    let Some((name, input, uts)) = self.pending.remove(id) else {
                        continue;
                    };
                    let text = match b.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Array(a)) => a
                            .iter()
                            .filter_map(|x| x.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    };
                    self.result(&name, &input, &text, uts.or(ts));
                }
                _ => {}
            }
        }
    }

    fn result(&mut self, name: &str, input: &Value, text: &str, ts: Option<DateTime<Utc>>) {
        let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
        match name {
            "CronCreate" => {
                // "Scheduled recurring job e0e3839a (Every 15 minutes)..."
                let id = text
                    .strip_prefix("Scheduled ")
                    .and_then(|t| t.split_whitespace().nth(2))
                    .filter(|id| id.chars().all(|c| c.is_ascii_hexdigit()) && id.len() >= 6);
                let Some(id) = id else { return }; // denied or failed
                self.loops.push(Loop {
                    id: id.to_string(),
                    kind: Kind::Cron,
                    cron: s("cron"),
                    prompt: s("prompt").unwrap_or_default(),
                    recurring: input
                        .get("recurring")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    durable: input
                        .get("durable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        || text.contains("Persisted"),
                    created: ts,
                    last_fire: None,
                    fires: 0,
                    wake_at: None,
                    deleted: false,
                    confidence: Confidence::High,
                });
            }
            "CronDelete" => {
                if let Some(id) = s("id") {
                    if text.starts_with("Cancelled") || text.contains("already been removed") {
                        for l in self.loops.iter_mut().filter(|l| l.id == id) {
                            l.deleted = true;
                        }
                    }
                }
            }
            "ScheduleWakeup" => {
                if input.get("stop").and_then(Value::as_bool) == Some(true) {
                    for l in self.loops.iter_mut().filter(|l| l.kind == Kind::Wakeup) {
                        l.deleted = true;
                    }
                    return;
                }
                if !text.starts_with("Next wakeup scheduled") {
                    return;
                }
                let delay = input
                    .get("delaySeconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let wake = ts.map(|t| t + Duration::seconds(delay as i64));
                let prompt = s("prompt").unwrap_or_default();
                match self
                    .loops
                    .iter_mut()
                    .find(|l| l.kind == Kind::Wakeup && !l.deleted)
                {
                    Some(l) => {
                        l.wake_at = wake;
                        if !prompt.is_empty() {
                            l.prompt = prompt;
                        }
                    }
                    None => self.loops.push(Loop {
                        id: "wakeup".into(),
                        kind: Kind::Wakeup,
                        cron: None,
                        prompt,
                        recurring: true,
                        durable: false,
                        created: ts,
                        last_fire: None,
                        fires: 0,
                        wake_at: wake,
                        deleted: false,
                        confidence: Confidence::Medium,
                    }),
                }
            }
            _ => {}
        }
    }
}

/// Durable jobs of a project: `<cwd>/.claude/scheduled_tasks.json`.
pub fn durable(cwd: &Path) -> Vec<Loop> {
    let Ok(text) = std::fs::read_to_string(cwd.join(".claude").join("scheduled_tasks.json")) else {
        return vec![];
    };
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let arr = match &v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o
            .get("tasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => vec![],
    };
    arr.iter()
        .filter_map(|t| {
            Some(Loop {
                id: t.get("id")?.as_str()?.to_string(),
                kind: Kind::Cron,
                cron: t.get("cron").and_then(Value::as_str).map(str::to_string),
                prompt: t
                    .get("prompt")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                recurring: t.get("recurring").and_then(Value::as_bool).unwrap_or(false),
                durable: true,
                created: t
                    .get("createdAt")
                    .and_then(Value::as_i64)
                    .and_then(|ms| Utc.timestamp_millis_opt(ms).single()),
                last_fire: None,
                fires: 0,
                wake_at: None,
                deleted: false,
                confidence: Confidence::High,
            })
        })
        .collect()
}

/// One cron field: does `v` match it?
fn field_matches(f: &str, v: u32, min: u32) -> bool {
    f.split(',').any(|part| {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().unwrap_or(1).max(1)),
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, u32::MAX)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().unwrap_or(0), b.parse().unwrap_or(0))
        } else {
            let n: u32 = range.parse().unwrap_or(u32::MAX);
            if step > 1 {
                (n, u32::MAX)
            } else {
                (n, n)
            }
        };
        v >= lo && v <= hi && (v - lo) % step == 0
    })
}

/// The next time a 5 field cron expression fires after `after` (local
/// time), searching up to 8 days ahead.
pub fn next_fire(expr: &str, after: DateTime<Local>) -> Option<DateTime<Local>> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return None;
    }
    let mut t = after.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
    for _ in 0..(8 * 24 * 60) {
        let dow = t.weekday().num_days_from_sunday();
        if field_matches(f[0], t.minute(), 0)
            && field_matches(f[1], t.hour(), 0)
            && field_matches(f[2], t.day(), 1)
            && field_matches(f[3], t.month(), 1)
            && (field_matches(f[4], dow, 0) || (dow == 0 && field_matches(f[4], 7, 0)))
        {
            return Some(t);
        }
        t += Duration::minutes(1);
    }
    None
}

/// "*/15 * * * *" -> "every 15 minutes".
pub fn human_cron(expr: &str) -> String {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return expr.to_string();
    }
    let all = |i: usize| f[i] == "*";
    match (f[0], f[1]) {
        (m, "*") if all(2) && all(3) && all(4) => {
            if let Some(n) = m.strip_prefix("*/") {
                return format!("every {n} minutes");
            }
            if m == "*" {
                return "every minute".into();
            }
            let mins: Vec<String> = m.split(',').map(|x| format!(":{:0>2}", x)).collect();
            if mins.len() == 1 {
                return format!("hourly at {}", mins[0]);
            }
            format!("hourly at {}", mins.join(", "))
        }
        (m, h) if all(2) && all(3) && all(4) && m.chars().all(|c| c.is_ascii_digit()) => {
            if let Some(n) = h.strip_prefix("*/") {
                return format!("every {n} hours at :{m:0>2}");
            }
            if h.chars().all(|c| c.is_ascii_digit()) {
                return format!("daily at {h:0>2}:{m:0>2}");
            }
            expr.to_string()
        }
        (m, h)
            if all(4)
                && m.chars().all(|c| c.is_ascii_digit())
                && h.chars().all(|c| c.is_ascii_digit()) =>
        {
            let month = f[3].parse::<u32>().ok().and_then(|mo| {
                [
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec",
                ]
                .get(mo.wrapping_sub(1) as usize)
                .copied()
            });
            match month {
                Some(mo) => format!("{mo} {} {h:0>2}:{m:0>2}", f[2]),
                None => format!("day {} at {h:0>2}:{m:0>2}", f[2]),
            }
        }
        _ => expr.to_string(),
    }
}

/// "in 4m", "3h ago".
pub fn rel(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let d = t - now;
    let (s, fut) = if d.num_seconds() >= 0 {
        (d.num_seconds(), true)
    } else {
        (-d.num_seconds(), false)
    };
    let txt = if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    };
    if fut {
        format!("in {txt}")
    } else {
        format!("{txt} ago")
    }
}

/// Scanners per transcript path.
#[derive(Default)]
pub struct LoopCache {
    pub scanners: HashMap<PathBuf, Scanner>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: Value) -> String {
        format!("{v}\n")
    }

    fn transcript() -> String {
        let mut s = String::new();
        s += &line(
            serde_json::json!({"type":"assistant","timestamp":"2026-10-01T10:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"CronCreate","input":{"cron":"*/15 * * * *","prompt":"continue the map","recurring":true}}]}}),
        );
        s += &line(
            serde_json::json!({"type":"user","timestamp":"2026-10-01T10:00:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Scheduled recurring job e0e3839a (Every 15 minutes). Session-only (not written to disk, dies when Claude exits). Auto-expires after 7 days."}]}}),
        );
        s += &line(
            serde_json::json!({"type":"assistant","timestamp":"2026-10-01T10:01:00Z","message":{"content":[{"type":"tool_use","id":"t2","name":"CronCreate","input":{"cron":"12 6 30 9 *","prompt":"loop is over","recurring":false}}]}}),
        );
        s += &line(
            serde_json::json!({"type":"user","timestamp":"2026-10-01T10:01:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"Scheduled one-shot task 05e59cec (12 6 30 9 *). Session-only."}]}}),
        );
        // A denied one is not a loop.
        s += &line(
            serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"CronCreate","input":{"cron":"* * * * *","prompt":"x"}}]}}),
        );
        s += &line(
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":"Permission for this action was denied"}]}}),
        );
        for (i, t) in ["10:15", "10:30", "10:45"].iter().enumerate() {
            s += &line(
                serde_json::json!({"type":"user","isMeta":true,"turnOrigin":"scheduled","scheduledTaskId":"e0e3839a","timestamp":format!("2026-10-01T{t}:00Z"),"message":{"content":format!("continue the map {i}")}}),
            );
        }
        s
    }

    #[test]
    fn rebuilds_loops_from_a_transcript() {
        let mut sc = Scanner::default();
        for l in transcript().lines() {
            sc.entry(&serde_json::from_str(l).unwrap());
        }
        let ls = sc.loops();
        assert_eq!(ls.len(), 2);
        let a = &ls[0];
        assert_eq!((a.id.as_str(), a.recurring, a.fires), ("e0e3839a", true, 3));
        assert_eq!(a.cadence(), "every 15 minutes");
        assert_eq!(
            a.last_fire.unwrap().to_rfc3339(),
            "2026-10-01T10:45:00+00:00"
        );
        assert_eq!(a.confidence, Confidence::High);
        let now: DateTime<Utc> = "2026-10-02T00:00:00Z".parse().unwrap();
        assert!(a.active(now));
        assert!(
            !a.active("2026-10-09T00:00:00Z".parse().unwrap()),
            "expired after 7 days"
        );
        assert_eq!(
            a.expires().unwrap().to_rfc3339(),
            "2026-10-08T10:00:00+00:00"
        );
        let b = &ls[1];
        assert!(!b.recurring);
        assert!(
            b.cadence().starts_with("once: Sep 30 06:12"),
            "{}",
            b.cadence()
        );
        // Deleting.
        sc.entry(&serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"CronDelete","input":{"id":"e0e3839a"}}]}}));
        sc.entry(&serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t9","content":"Cancelled job e0e3839a."}]}}));
        assert!(!sc.loops()[0].active(now));
    }

    #[test]
    fn wakeup_loops() {
        let mut sc = Scanner::default();
        sc.entry(&serde_json::json!({"type":"assistant","timestamp":"2026-10-01T10:00:00Z","message":{"content":[{"type":"tool_use","id":"w1","name":"ScheduleWakeup","input":{"delaySeconds":1800,"prompt":"/loop keep going","reason":"r"}}]}}));
        sc.entry(&serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"w1","content":"Next wakeup scheduled for 10:30:00 (in 1800s)."}]}}));
        let l = &sc.loops()[0];
        assert_eq!((l.kind, l.confidence), (Kind::Wakeup, Confidence::Medium));
        assert_eq!(l.wake_at.unwrap().to_rfc3339(), "2026-10-01T10:30:00+00:00");
        sc.entry(&serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"w2","name":"ScheduleWakeup","input":{"stop":true}}]}}));
        sc.entry(&serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"w2","content":"Loop stopped"}]}}));
        assert!(!sc.loops()[0].active(Utc::now()));
    }

    #[test]
    fn incremental_file_reads() {
        let p = std::env::temp_dir().join(format!("cg-loops-{}.jsonl", std::process::id()));
        let t = transcript();
        let (a, b) = t.split_at(t.find("\n").unwrap() + 1);
        std::fs::write(&p, a).unwrap();
        let mut sc = Scanner::default();
        sc.update(&p);
        assert!(sc.loops().is_empty(), "result not seen yet");
        std::fs::write(&p, format!("{a}{b}")).unwrap();
        sc.update(&p);
        assert_eq!(sc.loops().len(), 2);
        assert_eq!(sc.loops()[0].fires, 3);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn cron_math() {
        let at = |s: &str| {
            Local
                .from_local_datetime(
                    &chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap(),
                )
                .unwrap()
        };
        assert_eq!(
            next_fire("*/15 * * * *", at("2026-10-01 10:07")).unwrap(),
            at("2026-10-01 10:15")
        );
        assert_eq!(
            next_fire("7,22,37,52 * * * *", at("2026-10-01 10:22")).unwrap(),
            at("2026-10-01 10:37")
        );
        assert_eq!(
            next_fire("30 14 * * *", at("2026-10-01 15:00")).unwrap(),
            at("2026-10-02 14:30")
        );
        assert_eq!(
            next_fire("0 9 * * 1-5", at("2026-10-03 10:00")).unwrap(),
            at("2026-10-05 09:00"),
            "Saturday to Monday"
        );
        assert!(next_fire("bad", at("2026-10-01 10:00")).is_none());
        assert_eq!(human_cron("*/15 * * * *"), "every 15 minutes");
        assert_eq!(
            human_cron("7,22,37,52 * * * *"),
            "hourly at :07, :22, :37, :52"
        );
        assert_eq!(human_cron("5 * * * *"), "hourly at :05");
        assert_eq!(human_cron("30 14 * * *"), "daily at 14:30");
        assert_eq!(human_cron("0 */2 * * *"), "every 2 hours at :00");
        assert_eq!(human_cron("40 4 1 10 *"), "Oct 1 04:40");
        let now: DateTime<Utc> = "2026-10-01T10:00:00Z".parse().unwrap();
        assert_eq!(rel("2026-10-01T10:04:00Z".parse().unwrap(), now), "in 4m");
        assert_eq!(rel("2026-10-01T07:00:00Z".parse().unwrap(), now), "3h ago");
    }

    #[test]
    fn durable_file() {
        let d = std::env::temp_dir().join(format!("cg-durable-{}", std::process::id()));
        std::fs::create_dir_all(d.join(".claude")).unwrap();
        std::fs::write(
            d.join(".claude/scheduled_tasks.json"),
            r#"[{"id":"ab12cd34","cron":"0 9 * * *","prompt":"morning check","createdAt":1790000000000,"recurring":true}]"#,
        )
        .unwrap();
        let v = durable(&d);
        assert_eq!(v.len(), 1);
        assert!(v[0].durable && v[0].recurring && v[0].expires().is_none());
        assert!(durable(Path::new("/nonexistent")).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn appended_delete_is_seen() {
        use std::io::Write;
        let p = std::env::temp_dir().join(format!("cg-loops-app-{}.jsonl", std::process::id()));
        std::fs::write(&p, transcript()).unwrap();
        let mut sc = Scanner::default();
        sc.update(&p);
        assert!(sc.loops()[0].active(Utc::now() - Duration::days(4)));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{}", serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"CronDelete","input":{"id":"e0e3839a"}}]}})).unwrap();
        writeln!(f, "{}", serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t9","content":"Cancelled job e0e3839a."}]}})).unwrap();
        drop(f);
        sc.update(&p);
        assert!(sc.loops()[0].deleted);
        let _ = std::fs::remove_file(p);
    }
}
