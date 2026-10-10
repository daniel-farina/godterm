//! One index of every session on this Mac, for the assistant's `sessions`
//! tool: every GodTerm account (Claude Code and Grok), the main `~/.claude`
//! and the main `~/.grok`. Built in the background and refreshed by file
//! mtimes, so a query is a filter over memory. Only metadata and short
//! snippets ever leave it; `detail` reads one transcript on request.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::app_sessions::Src;
use crate::harness::Harness;
use crate::sessions::SessionInfo;

#[derive(Debug, Clone)]
pub struct Row {
    pub src: Src,
    pub harness: Harness,
    pub source: String,
    pub info: SessionInfo,
}

#[derive(Default)]
pub struct Index {
    pub rows: Vec<Row>,
    pub built: Option<Instant>,
    pub building: bool,
    pub took_ms: u128,
    /// Parsed files by path, with (mtime ns, size): kept across builds and
    /// on disk, so only new or changed transcripts are read again.
    files: HashMap<PathBuf, (u128, u64, SessionInfo)>,
    loaded: bool,
}

pub type Shared = Arc<Mutex<Index>>;

/// Where sessions live: (source, harness, home, label).
pub type Source = (Src, Harness, PathBuf, String);

pub fn disk_path() -> PathBuf {
    crate::config::app_home().join("session-index.json")
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Disk {
    version: u32,
    files: Vec<(PathBuf, u128, u64, SessionInfo)>,
}

const DISK_VERSION: u32 = 5;

fn stamp(p: &Path) -> Option<(u128, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((t, m.len()))
}

/// What one source holds: main transcripts with their subagent files.
enum Item {
    Claude { main: PathBuf, subs: Vec<PathBuf> },
    Grok { summary: PathBuf },
}

fn list(h: Harness, home: &Path) -> Vec<Item> {
    let mut out = vec![];
    match h {
        Harness::Claude => {
            let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
                return out;
            };
            for proj in projects
                .flatten()
                .filter(|p| p.file_type().is_ok_and(|t| t.is_dir()))
            {
                let Ok(files) = std::fs::read_dir(proj.path()) else {
                    continue;
                };
                for f in files.flatten() {
                    let p = f.path();
                    if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let subs = std::fs::read_dir(p.with_extension("").join("subagents"))
                        .map(|r| {
                            r.flatten()
                                .map(|e| e.path())
                                .filter(|x| x.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                                .collect()
                        })
                        .unwrap_or_default();
                    out.push(Item::Claude { main: p, subs });
                }
            }
        }
        Harness::Grok => {
            let Ok(dirs) = std::fs::read_dir(home.join("sessions")) else {
                return out;
            };
            for d in dirs.flatten() {
                let Ok(ss) = std::fs::read_dir(d.path()) else {
                    continue;
                };
                for x in ss.flatten() {
                    let summary = x.path().join("summary.json");
                    if summary.is_file() {
                        out.push(Item::Grok { summary });
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn parse_one(p: &Path) -> Option<SessionInfo> {
    if p.file_name().is_some_and(|f| f == "summary.json") {
        crate::harness::grok::parse_session_any(p.parent()?, true)
    } else {
        crate::sessions::parse_file_quick(p)
    }
}

/// Scan every source (in the caller's thread) into the index: list files,
/// keep what has not changed since the last build (in memory or on disk),
/// parse the rest in parallel, then save.
pub fn rebuild(shared: &Shared, sources: &[Source]) {
    let t0 = Instant::now();
    let (mut files, loaded) = {
        let mut ix = shared.lock().unwrap_or_else(|e| e.into_inner());
        ix.building = true;
        (std::mem::take(&mut ix.files), ix.loaded)
    };
    if !loaded {
        if let Ok(t) = std::fs::read(disk_path()) {
            if let Ok(d) = serde_json::from_slice::<Disk>(&t) {
                if d.version == DISK_VERSION {
                    files.extend(d.files.into_iter().map(|(p, m, l, i)| (p, (m, l, i))));
                }
            }
        }
    }
    let listed: Vec<(usize, Vec<Item>)> = sources
        .iter()
        .enumerate()
        .map(|(k, (_, h, home, _))| (k, list(*h, home)))
        .collect();
    // Every file with its stamp; the changed ones get parsed.
    let mut all: Vec<(PathBuf, (u128, u64))> = vec![];
    for (_, items) in &listed {
        for it in items {
            let ps: Vec<&PathBuf> = match it {
                Item::Claude { main, subs } => std::iter::once(main).chain(subs.iter()).collect(),
                Item::Grok { summary } => vec![summary],
            };
            for p in ps {
                if let Some(st) = stamp(p) {
                    all.push((p.clone(), st));
                }
            }
        }
    }
    let todo: Vec<(PathBuf, (u128, u64))> = all
        .iter()
        .filter(|(p, st)| files.get(p).is_none_or(|(m, l, _)| (*m, *l) != *st))
        .cloned()
        .collect();
    let changed = !todo.is_empty();
    if changed {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 16);
        // Biggest first, handed out one at a time: no worker is left
        // holding a pile of large transcripts.
        let mut todo = todo.clone();
        todo.sort_by_key(|(_, st)| std::cmp::Reverse(st.1));
        let next = std::sync::atomic::AtomicUsize::new(0);
        let parsed: Vec<(PathBuf, (u128, u64), SessionInfo)> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..workers)
                .map(|_| {
                    sc.spawn(|| {
                        let mut out = vec![];
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some((p, st)) = todo.get(i) else { break };
                            if let Some(info) = parse_one(p) {
                                out.push((p.clone(), *st, info));
                            }
                        }
                        out
                    })
                })
                .collect();
            hs.into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for (p, st, i) in parsed {
            files.insert(p, (st.0, st.1, i));
        }
    }
    let live: std::collections::HashSet<&PathBuf> = all.iter().map(|(p, _)| p).collect();
    let before = files.len();
    files.retain(|p, _| live.contains(p));
    let removed = files.len() != before;
    // Rows: main sessions with subagents rolled in, subagents marked.
    let mut rows = vec![];
    for (k, items) in &listed {
        let (src, h, _, label) = &sources[*k];
        for it in items {
            match it {
                Item::Claude { main, subs } => {
                    let Some((_, _, info)) = files.get(main) else {
                        continue;
                    };
                    if info.messages == 0 {
                        continue;
                    }
                    let mut info = info.clone();
                    for sp in subs {
                        if let Some((_, _, si)) = files.get(sp) {
                            info.subagents += 1;
                            info.tokens.add(&si.tokens);
                            let mut si = si.clone();
                            si.parent = Some(info.id.clone());
                            rows.push(Row {
                                src: *src,
                                harness: *h,
                                source: label.clone(),
                                info: si,
                            });
                        }
                    }
                    rows.push(Row {
                        src: *src,
                        harness: *h,
                        source: label.clone(),
                        info,
                    });
                }
                Item::Grok { summary } => {
                    if let Some((_, _, info)) = files.get(summary) {
                        rows.push(Row {
                            src: *src,
                            harness: *h,
                            source: label.clone(),
                            info: info.clone(),
                        });
                    }
                }
            }
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.info.modified));
    if (changed || removed || !loaded) && !cfg!(test) {
        let d = Disk {
            version: DISK_VERSION,
            files: files
                .iter()
                .map(|(p, (m, l, i))| (p.clone(), *m, *l, i.clone()))
                .collect(),
        };
        if let Ok(t) = serde_json::to_vec(&d) {
            let tmp = disk_path().with_extension("json.tmp");
            if crate::config::write_private(&tmp, t).is_ok() {
                let _ = std::fs::rename(&tmp, disk_path());
            }
        }
    }
    let mut ix = shared.lock().unwrap_or_else(|e| e.into_inner());
    ix.rows = rows;
    ix.files = files;
    ix.loaded = true;
    ix.building = false;
    ix.built = Some(Instant::now());
    ix.took_ms = t0.elapsed().as_millis();
    crate::log::info(&format!(
        "session index: {} sessions from {} sources in {} ms ({} files read)",
        ix.rows.iter().filter(|r| r.info.parent.is_none()).count(),
        sources.len(),
        ix.took_ms,
        todo.len()
    ));
}

/// "today", "yesterday", "this week", "last week", "3 days", "2 hours",
/// "2026-10-01" or an RFC 3339 time, as a point in time.
pub fn parse_when(s: &str, now: chrono::DateTime<chrono::Local>) -> Option<SystemTime> {
    use chrono::{Duration as D, TimeZone};
    let s = s.trim().to_lowercase();
    let midnight = |d: chrono::NaiveDate| {
        chrono::Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
            .single()
    };
    let t = match s.as_str() {
        "today" => midnight(now.date_naive())?,
        "yesterday" => midnight(now.date_naive() - D::days(1))?,
        "this week" | "week" => now - D::days(7),
        "last week" => now - D::days(14),
        "this month" | "month" => now - D::days(30),
        "hour" | "last hour" => now - D::hours(1),
        _ => {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
                midnight(d)?
            } else if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&s) {
                t.with_timezone(&chrono::Local)
            } else {
                let mut it = s.split_whitespace();
                let n: i64 = it.next()?.parse().ok()?;
                match it.next()?.trim_end_matches('s') {
                    "minute" | "min" => now - D::minutes(n),
                    "hour" => now - D::hours(n),
                    "day" => now - D::days(n),
                    "week" => now - D::weeks(n),
                    _ => return None,
                }
            }
        }
    };
    Some(SystemTime::from(t))
}

/// Every query word somewhere in the text (case insensitive); more
/// matches score higher. None when a word is missing.
/// How well a spoken query names a session, 0 to 1: its name or title,
/// the folder's name, and runs of words in the title and first prompt,
/// compared by spelling and sound ("rarion refactor" -> RARIUM REFACTOR).
pub fn fuzzy_score(
    query: &str,
    info: &crate::sessions::SessionInfo,
    live_name: Option<&str>,
) -> f64 {
    let q = crate::fuzzy::spoken_punctuation(query);
    let n = q.split_whitespace().count().max(1);
    let mut best = 0.0f64;
    let mut try_text = |t: &str| {
        best = best.max(crate::fuzzy::similarity(&q, t));
        let words: Vec<&str> = t
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .filter(|w| !w.is_empty())
            .collect();
        for k in [n, n + 1] {
            for win in words.windows(k.min(words.len().max(1))) {
                best = best.max(crate::fuzzy::similarity(&q, &win.join(" ")) * 0.97);
            }
        }
    };
    for t in [
        info.name.as_deref(),
        live_name,
        info.title.as_deref(),
        info.first_prompt.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        try_text(&crate::sessions::snippet(t, 160));
    }
    let base = info.cwd.rsplit('/').next().unwrap_or("");
    best = best.max(crate::fuzzy::similarity(&q, base));
    best
}

pub fn score(query: &str, hay: &str) -> Option<usize> {
    let h = hay.to_lowercase();
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if words.is_empty() {
        return Some(0);
    }
    let mut s = 0;
    for w in &words {
        let n = h.matches(w.as_str()).count();
        if n == 0 {
            // A near miss: the word's first 5 letters.
            let stem: String = w.chars().take(5).collect();
            if stem.len() < 4 || !h.contains(&stem) {
                return None;
            }
        }
        s += n.max(1);
    }
    Some(s)
}

pub fn rel(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "?".into() };
    let s = SystemTime::now()
        .duration_since(t)
        .unwrap_or_default()
        .as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// What a session row says to the assistant.
pub fn row_json(r: &Row, open_tab: Option<String>, live: bool) -> Value {
    let i = &r.info;
    // grok records its cost; Claude Code does not (and estimating it from
    // cache reads would be wrong), so there is none.
    let cost = i
        .title
        .as_deref()
        .and_then(|t| t.rsplit_once("($"))
        .map(|(_, c)| c.trim_end_matches(')').to_string());
    json!({
        "id": i.id,
        "harness": r.harness.name(),
        "source": r.source,
        "project": crate::config::tilde(Path::new(&i.cwd)),
        "title": i.name.as_deref().or(i.title.as_deref()).map(|t| t.split(" ($").next().unwrap_or(t).to_string()),
        "first_prompt": i.first_prompt.as_deref().map(|p| crate::sessions::snippet(p, 100)),
        "last_activity": rel(i.modified),
        "messages": i.messages,
        "estimated": i.approx.then_some(true),
        "tokens": {"new_input": i.tokens.input + i.tokens.cache_creation, "output": i.tokens.output, "largest_context": i.tokens.context},
        "cost_usd": cost,
        "subagents": (i.subagents > 0).then_some(i.subagents),
        "subagent_of": i.parent,
        "open_in_tab": open_tab,
        "live": live,
    })
}

/// A short account of one Claude Code session: first prompt, the last few
/// exchanges, files touched, how it ended.
pub fn detail_claude(path: &Path) -> Value {
    use std::io::BufReader;
    let Ok(f) = std::fs::File::open(path) else {
        return json!({"error": "transcript gone"});
    };
    let mut prompts: Vec<String> = vec![];
    let mut replies: Vec<String> = vec![];
    let mut files: Vec<String> = vec![];
    for line in crate::sessions::lossy_lines(BufReader::new(f)) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["isSidechain"].as_bool().unwrap_or(false) && !crate::sessions::is_subagent_file(path) {
            continue;
        }
        match v["type"].as_str() {
            Some("user") => {
                if let Some(t) = v["message"]["content"]
                    .as_str()
                    .filter(|t| !t.starts_with('<'))
                {
                    prompts.push(crate::sessions::snippet(t, 200));
                }
            }
            Some("assistant") => {
                for b in v["message"]["content"].as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("text") => {
                            if let Some(t) = b["text"].as_str() {
                                replies.push(crate::sessions::snippet(t, 240));
                            }
                        }
                        Some("tool_use") => {
                            if let Some(p) = b["input"]["file_path"]
                                .as_str()
                                .or(b["input"]["notebook_path"].as_str())
                            {
                                let p = crate::config::tilde(Path::new(p));
                                if !files.contains(&p) {
                                    files.push(p);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let last = |v: &[String], n: usize| v[v.len().saturating_sub(n)..].to_vec();
    json!({
        "first_prompt": prompts.first(),
        "recent_prompts": last(&prompts, 3),
        "recent_replies": last(&replies, 2),
        "files_touched": files.iter().take(15).collect::<Vec<_>>(),
        "outcome": replies.last().map(|r| crate::sessions::first_sentences(r, 240)),
    })
}

pub fn detail_grok(dir: &Path) -> Value {
    let s: Value = std::fs::read_to_string(dir.join("summary.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    json!({
        "title": s["generated_title"],
        "summary": s["session_summary"].as_str().map(|t| crate::sessions::snippet(t, 600)),
        "model": s["current_model_id"],
        "messages": s["num_chat_messages"],
        "last_active": s["last_active_at"],
    })
}

/// Refresh at most this often in the background.
pub const STALE: Duration = Duration::from_secs(45);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_and_score() {
        let now = chrono::Local::now();
        assert!(parse_when("today", now).unwrap() <= SystemTime::now());
        assert!(parse_when("yesterday", now).unwrap() < parse_when("today", now).unwrap());
        assert!(parse_when("3 days", now).is_some() && parse_when("2026-10-01", now).is_some());
        assert!(parse_when("whenever", now).is_none());
        assert!(score("api login", "Fix the login flow in ~/code/api").is_some());
        assert!(
            score("calculator", "Build a calculatr app").is_some(),
            "near miss"
        );
        assert!(score("grok", "Fix the login flow").is_none());
    }
}
