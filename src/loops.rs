use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const EXPIRY_DAYS: i64 = 7;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Cron,

    Wakeup,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub enum Confidence {
    High,

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

    pub wake_at: Option<DateTime<Utc>>,
    pub deleted: bool,
    pub confidence: Confidence,

    pub every: Option<String>,
}

impl Loop {
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
        if let Some(e) = &self.every {
            return if self.recurring {
                e.clone()
            } else {
                format!("once: {e}")
            };
        }
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

#[derive(Debug, Default)]
pub struct Scanner {
    offset: u64,
    loops: Vec<Loop>,

    pending: HashMap<String, (String, Value, Option<DateTime<Utc>>)>,
}

impl Scanner {
    pub fn loops(&self) -> &[Loop] {
        &self.loops
    }

    pub fn update(&mut self, path: &Path) {
        let Ok(mut f) = std::fs::File::open(path) else {
            return;
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = Scanner::default();
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
                        break;
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
                    l.fires += 1;
                    l.last_fire = ts.or(l.last_fire);
                }
            }
        }

        if let Some(calls) = o.get("tool_calls").and_then(Value::as_array) {
            for c in calls {
                let name = c.get("name").and_then(Value::as_str).unwrap_or("");
                if matches!(name, "scheduler_create" | "scheduler_delete") {
                    let id = c
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let input = match c.get("arguments") {
                        Some(Value::String(a)) => serde_json::from_str(a).unwrap_or(Value::Null),
                        Some(v) => v.clone(),
                        None => Value::Null,
                    };
                    self.pending.insert(id, (name.to_string(), input, ts));
                }
            }
        }
        if o.get("type").and_then(Value::as_str) == Some("tool_result") {
            if let Some(id) = o.get("tool_call_id").and_then(Value::as_str) {
                if let Some((name, input, uts)) = self.pending.remove(id) {
                    let text = o.get("content").and_then(Value::as_str).unwrap_or("");
                    self.result(&name, &input, text, uts.or(ts));
                }
            }
            return;
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
                let id = text
                    .strip_prefix("Scheduled ")
                    .and_then(|t| t.split_whitespace().nth(2))
                    .filter(|id| id.chars().all(|c| c.is_ascii_hexdigit()) && id.len() >= 6);

                let Some(id) = id else { return };

                if self.loops.iter().any(|l| l.id == id) {
                    return;
                }
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
                    every: None,
                });
            }
            "scheduler_create" => {
                let Some(rest) = text
                    .strip_prefix("Scheduled task created (ID: ")
                    .or_else(|| text.strip_prefix("Scheduled task updated (ID: "))
                else {
                    return;
                };
                let inner = rest.split(')').next().unwrap_or("");
                let (id, every) = match inner.split_once(", ") {
                    Some((i, e)) => (i.trim(), Some(e.trim().to_string())),
                    None => (inner.trim(), None),
                };
                if id.is_empty() {
                    return;
                }
                let prompt = s("prompt");
                let recurring = input
                    .get("recurring")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                match self.loops.iter_mut().find(|l| l.id == id && !l.deleted) {
                    Some(l) => {
                        if let Some(p) = prompt {
                            l.prompt = p;
                        }
                        l.every = every.or(l.every.take());
                        l.recurring = recurring;
                    }
                    None => self.loops.push(Loop {
                        id: id.to_string(),
                        kind: Kind::Cron,
                        cron: None,
                        prompt: prompt.unwrap_or_default(),
                        recurring,
                        durable: input
                            .get("durable")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        created: ts,
                        last_fire: None,
                        fires: 0,
                        wake_at: None,
                        deleted: false,
                        confidence: Confidence::High,
                        every,
                    }),
                }
            }
            "scheduler_delete" => {
                let id = s("id").or_else(|| s("task_id"));
                if let Some(id) = id {
                    if text.contains("cancelled") {
                        for l in self.loops.iter_mut().filter(|l| l.id == id) {
                            l.deleted = true;
                        }
                    }
                }
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
                        every: None,
                    }),
                }
            }
            _ => {}
        }
    }
}

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
                every: None,
            })
        })
        .collect()
}

pub(crate) fn field_matches(f: &str, v: u32, min: u32) -> bool {
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
        v >= lo && v <= hi && (v - lo).is_multiple_of(step)
    })
}

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

pub fn join_and(v: &[String]) -> String {
    match v.len() {
        0 => String::new(),
        1 => v[0].clone(),
        n => format!("{} and {}", v[..n - 1].join(", "), v[n - 1]),
    }
}

fn plain_nums(f: &str, lo: u32, hi: u32) -> Option<Vec<u32>> {
    let mut v = vec![];
    for part in f.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
            if a > b || b > hi {
                return None;
            }
            v.extend(a..=b);
        } else {
            let n: u32 = part.parse().ok()?;
            if n < lo || n > hi {
                return None;
            }
            v.push(n);
        }
    }
    v.sort_unstable();
    v.dedup();
    Some(v)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAYS: [&str; 7] = [
    "Sundays",
    "Mondays",
    "Tuesdays",
    "Wednesdays",
    "Thursdays",
    "Fridays",
    "Saturdays",
];

fn cron_days(dom: &str, mon: &str, dow: &str) -> Option<String> {
    match (dom, mon, dow) {
        ("*", "*", "*") => Some("every day".into()),
        ("*", "*", d) => {
            let mut v: Vec<u32> = plain_nums(d, 0, 7)?.into_iter().map(|n| n % 7).collect();
            v.sort_unstable();
            v.dedup();
            if v == [1, 2, 3, 4, 5] {
                return Some("on weekdays".into());
            }
            if v == [0, 6] {
                return Some("on weekends".into());
            }
            let names: Vec<String> = v.iter().map(|&n| DAYS[n as usize].to_string()).collect();
            Some(format!("on {}", join_and(&names)))
        }
        (d, "*", "*") => {
            let v = plain_nums(d, 1, 31)?;
            let names: Vec<String> = v.iter().map(|n| n.to_string()).collect();
            Some(format!(
                "on day{} {} of every month",
                if v.len() == 1 { "" } else { "s" },
                join_and(&names)
            ))
        }
        (d, m, "*") => {
            let ds = plain_nums(d, 1, 31)?;
            let ms = plain_nums(m, 1, 12)?;
            let mut v = vec![];
            for mo in &ms {
                for dd in &ds {
                    v.push(format!("{} {dd}", MONTHS[(*mo - 1) as usize]));
                }
            }
            (v.len() <= 6).then(|| format!("on {}", join_and(&v)))
        }
        _ => None,
    }
}

pub fn describe_cron(expr: &str) -> String {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return expr.to_string();
    }
    let Some(days) = cron_days(f[2], f[3], f[4]) else {
        return human_cron(expr);
    };
    let tail = |s: String| {
        if days == "every day" {
            s
        } else {
            format!("{s}, {days}")
        }
    };
    let mins = plain_nums(f[0], 0, 59);
    match (f[0], f[1]) {
        ("*", "*") => return tail("every minute".into()),
        (m, "*") if m.starts_with("*/") => {
            return tail(format!("every {} minutes", &m[2..]));
        }
        (_, "*") => {
            if let Some(ms) = &mins {
                let v: Vec<String> = ms.iter().map(|m| format!(":{m:02}")).collect();
                return tail(format!("every hour at {}", join_and(&v)));
            }
        }
        (_, h) if h.starts_with("*/") => {
            if let Some(ms) = &mins {
                let v: Vec<String> = ms.iter().map(|m| format!(":{m:02}")).collect();
                return tail(format!("every {} hours at {}", &h[2..], join_and(&v)));
            }
        }
        (_, h) => {
            if let (Some(ms), Some(hs)) = (&mins, plain_nums(h, 0, 23)) {
                let at = if ms.len() * hs.len() <= 8 {
                    let mut v = vec![];
                    for hh in &hs {
                        for mm in ms {
                            v.push(format!("{hh}:{mm:02}"));
                        }
                    }
                    join_and(&v)
                } else {
                    let mm: Vec<String> = ms.iter().map(|m| format!(":{m:02}")).collect();
                    let hh: Vec<String> = hs.iter().map(|h| h.to_string()).collect();
                    format!("{} past hours {}", join_and(&mm), join_and(&hh))
                };
                return format!("at {at} {days}");
            }
        }
    }
    human_cron(expr)
}

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
        assert!(sc.loops()[0].active("2026-10-02T00:00:00Z".parse().unwrap()));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{}", serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"CronDelete","input":{"id":"e0e3839a"}}]}})).unwrap();
        writeln!(f, "{}", serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t9","content":"Cancelled job e0e3839a."}]}})).unwrap();
        drop(f);
        sc.update(&p);
        assert!(sc.loops()[0].deleted);
        let _ = std::fs::remove_file(p);
    }
}
