//! The full record of tabs: every open, close, move, rename, restart,
//! crash and take-over, appended to `~/.godterm/tab-history.jsonl` (owner
//! only), kept for `assistant.history_days`. The closed stack (20 tabs,
//! Undo) is for quick reopening; this is the history the assistant and
//! View ▾ > Tab history search.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use crate::app::App;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Record {
    /// Short id for reopen_tab.
    pub id: String,
    /// RFC 3339, local time.
    pub at: String,
    /// open, restart, close, crash, move, rename, takeover, reopen.
    pub event: String,
    pub account: String,
    #[serde(default)]
    pub harness: String,
    /// "t12".
    pub tab: String,
    pub name: String,
    pub cwd: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Who closed it: user, assistant, crash, exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Its state at the time (ready, working, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// "to Account 2", "was api", ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Its group in the account's list then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Loops it had when it closed (reopen sets them up again).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<crate::closed::SavedLoop>,
}

pub fn path() -> PathBuf {
    crate::config::app_home().join("tab-history.jsonl")
}

pub fn append(r: &Record) {
    let res = (|| -> std::io::Result<()> {
        use crate::platform::OpenOptionsExt;
        let p = path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&p)?;
        writeln!(f, "{}", serde_json::to_string(r).unwrap_or_default())
    })();
    if let Err(e) = res {
        crate::log::info(&format!("tab history: {e}"));
    }
}

/// Every record, oldest first.
pub fn read_all() -> Vec<Record> {
    let Ok(f) = std::fs::File::open(path()) else {
        return vec![];
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// Drop records older than `days` (0 keeps all). Returns how many went.
pub fn purge(days: u32) -> usize {
    if days == 0 {
        return 0;
    }
    let all = read_all();
    let cutoff = chrono::Local::now() - chrono::Duration::days(days as i64);
    let keep: Vec<&Record> = all
        .iter()
        .filter(|r| {
            chrono::DateTime::parse_from_rfc3339(&r.at)
                .map(|t| t >= cutoff)
                .unwrap_or(true)
        })
        .collect();
    let gone = all.len() - keep.len();
    if gone > 0 {
        let text: String = keep
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap_or_default()))
            .collect();
        let tmp = path().with_extension("jsonl.tmp");
        if crate::config::write_private(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path());
        }
    }
    gone
}

/// What tab_history and the view filter on.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub since: Option<std::time::SystemTime>,
    pub until: Option<std::time::SystemTime>,
    pub text: Option<String>,
    pub account: Option<String>,
    pub event: Option<String>,
    pub limit: usize,
}

/// Records matching `f`, newest first. Text matches the name, folder,
/// session id or tab id, tolerant of misheard names.
pub fn query(all: &[Record], f: &Filter) -> Vec<Record> {
    let q = f
        .text
        .as_deref()
        .map(crate::fuzzy::spoken_punctuation)
        .filter(|q| !q.trim().is_empty());
    let mut out: Vec<Record> = all
        .iter()
        .rev()
        .filter(|r| {
            let t = chrono::DateTime::parse_from_rfc3339(&r.at)
                .ok()
                .map(std::time::SystemTime::from);
            f.since.is_none_or(|s| t.is_some_and(|t| t >= s))
                && f.until.is_none_or(|u| t.is_some_and(|t| t <= u))
        })
        .filter(|r| {
            f.account
                .as_deref()
                .is_none_or(|a| r.account.eq_ignore_ascii_case(a))
        })
        .filter(|r| {
            f.event.as_deref().is_none_or(|e| {
                r.event.eq_ignore_ascii_case(e)
                    || (e == "closed" && r.event == "close")
                    || (e == "opened" && r.event == "open")
            })
        })
        .filter(|r| {
            q.as_deref().is_none_or(|q| {
                let hay = format!(
                    "{} {} {} {}",
                    r.name,
                    r.cwd,
                    r.session_id.as_deref().unwrap_or(""),
                    r.tab
                )
                .to_lowercase();
                hay.contains(&q.to_lowercase())
                    || crate::fuzzy::key(&hay).contains(&crate::fuzzy::key(q))
                    || crate::fuzzy::similarity(q, &r.name) >= 0.75
                    || crate::fuzzy::similarity(
                        q,
                        &crate::fuzzy::tail(&r.cwd, q.split('/').count().max(1)),
                    ) >= 0.75
            })
        })
        .cloned()
        .collect();
    out.truncate(if f.limit == 0 { 20 } else { f.limit });
    out
}

impl App {
    /// Record an event for tab `t` of pane `s`.
    pub fn tab_event(
        &mut self,
        s: usize,
        t: usize,
        event: &str,
        by: Option<&str>,
        detail: Option<String>,
    ) {
        let Some(slot) = self.panes.get(s) else {
            return;
        };
        let Some(tab) = slot.tabs.get(t) else { return };
        let Some(a) = slot.account else { return };
        let loops = if event == "close" || event == "crash" {
            self.loops
                .iter()
                .filter(|r| r.uid == tab.uid && !r.lp.durable)
                .map(|r| crate::closed::SavedLoop {
                    id: r.lp.id.clone(),
                    cron: r.lp.cron.clone(),
                    prompt: r.lp.prompt.clone(),
                    recurring: r.lp.recurring,
                    wakeup: r.lp.kind == crate::loops::Kind::Wakeup,
                })
                .collect()
        } else {
            vec![]
        };
        let r = Record {
            id: crate::session_ops::new_uuid()[..8].to_string(),
            at: chrono::Local::now().to_rfc3339(),
            event: event.to_string(),
            account: self.cfg.accounts[a].name.clone(),
            harness: self.cfg.accounts[a].harness().name().to_string(),
            tab: format!("t{}", tab.uid),
            name: tab.name(),
            cwd: self.tab_dir(s, t).to_string_lossy().into_owned(),
            session_id: tab.session_id.clone(),
            by: by.map(str::to_string),
            state: Some(self.state_word(s, t).to_string()),
            detail,
            group: tab.group.clone(),
            loops,
        };
        append(&r);
    }

    /// Resume the session of a history record (by record id or session
    /// id) in a tab, on its account or the best one of the same agent.
    pub fn reopen_record(&mut self, key: &str) -> Result<String, String> {
        let all = read_all();
        // A tab id ("t12"): old records (from before ids were kept across
        // restarts) reuse ids, so it resolves by the sessions it held.
        let as_tab = key
            .strip_prefix('t')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        let key = if as_tab {
            let mut sessions: Vec<&Record> = vec![];
            for r in all.iter().rev().filter(|r| r.tab == key) {
                if sessions.iter().all(|o| o.session_id != r.session_id) {
                    sessions.push(r);
                }
            }
            match sessions.as_slice() {
                [] => return Err(format!("no tab history record for {key}")),
                [one] => one.id.clone(),
                many => {
                    let list: Vec<String> = many
                        .iter()
                        .take(5)
                        .map(|r| {
                            format!(
                                "'{}' in {} (record {})",
                                r.name,
                                crate::config::tilde(std::path::Path::new(&r.cwd)),
                                r.id
                            )
                        })
                        .collect();
                    return Err(format!(
                        "{key} named more than one tab over time: {}. Ask which, or pass its record id",
                        list.join(", ")
                    ));
                }
            }
        } else {
            key.to_string()
        };
        let key = key.as_str();
        let r = all
            .iter()
            .rev()
            .find(|r| {
                r.id == key
                    || r.session_id
                        .as_deref()
                        .is_some_and(|s| s == key || (key.len() >= 6 && s.starts_with(key)))
            })
            .cloned()
            .ok_or_else(|| format!("no tab history record or session {key}"))?;
        if r.session_id.is_none() {
            return Err(format!(
                "'{}' never had a session to resume; open a new tab in {} instead",
                r.name, r.cwd
            ));
        }
        // Already open: go there.
        if let Some((s, t)) = self.all_open_tabs().into_iter().find(|&(s, t)| {
            self.panes[s].tabs[t].session_id == r.session_id && self.panes[s].tabs[t].is_running()
        }) {
            self.jump_to(s, t);
            return Ok(format!("'{}' is already open", r.name));
        }
        let c = crate::closed::ClosedTab {
            account: r.account.clone(),
            cwd: r.cwd.clone(),
            session_id: r.session_id.clone(),
            name: Some(r.name.clone()),
            accent: None,
            loops: r.loops.clone(),
            group: 0,
            closed_at: 0,
            label: r.name.clone(),
            tab_group: r.group.clone(),
            pinned: false,
        };
        let msg = self.reopen_closed_tab(c)?;
        self.flash(msg.clone());
        Ok(msg)
    }

    /// One search over sessions, the tab history and past chats, ranked:
    /// [{type, score, id, label, folder, when, session}].
    pub fn find_everywhere(
        &mut self,
        query: &str,
        since: Option<std::time::SystemTime>,
        limit: usize,
    ) -> Vec<serde_json::Value> {
        use serde_json::json;
        let q = crate::fuzzy::spoken_punctuation(query);
        let ql = q.to_lowercase();
        let qk = crate::fuzzy::key(&q);
        let words: Vec<String> = ql
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .map(str::to_string)
            .collect();
        // How well a name / folder / text answers the query, 0 to 1.
        let score = |name: &str, folder: &str, text: &str| -> f64 {
            let n = crate::fuzzy::similarity(&q, name);
            let f = crate::fuzzy::similarity(
                &q,
                &crate::fuzzy::tail(folder, q.split('/').count().max(1)),
            );
            let hay = format!("{name} {folder} {text}").to_lowercase();
            let contained = if hay.contains(&ql) || crate::fuzzy::key(&hay).contains(&qk) {
                0.95
            } else {
                0.0
            };
            let w = if words.is_empty() {
                0.0
            } else {
                words.iter().filter(|w| hay.contains(w.as_str())).count() as f64
                    / words.len() as f64
                    * 0.8
            };
            // The last part said ("pod mesh" of "lol slash pod mesh")
            // against the name, the folder's own name and words in the text.
            let last = q.rsplit('/').next().unwrap_or(&q);
            let base = folder.rsplit('/').next().unwrap_or("");
            let mut part =
                crate::fuzzy::similarity(last, name).max(crate::fuzzy::similarity(last, base));
            for word in hay
                .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
                .filter(|w| w.len() > 3)
            {
                part = part.max(crate::fuzzy::similarity(last, word));
            }
            n.max(f).max(contained).max(w).max(part * 0.95)
        };
        let mut hits: Vec<(f64, serde_json::Value)> = vec![];
        let ts = |t: Option<std::time::SystemTime>| {
            t.map(|t| {
                chrono::DateTime::<chrono::Local>::from(t)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
        };
        // Coding sessions.
        self.kick_index(false);
        {
            let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
            for r in ix
                .rows
                .iter()
                .filter(|r| r.info.parent.is_none() && !crate::sess_sort::headless(&r.info))
            {
                if since.is_some_and(|t| r.info.modified.is_none_or(|m| m < t)) {
                    continue;
                }
                let title = r.info.summary();
                let sc = score(
                    &title,
                    &r.info.cwd,
                    r.info.last_prompt.as_deref().unwrap_or(""),
                );
                if sc >= 0.6 {
                    hits.push((sc, json!({"type": "session", "id": r.info.id, "label": crate::sessions::snippet(&title, 80), "folder": crate::config::tilde(std::path::Path::new(&r.info.cwd)), "when": ts(r.info.modified), "source": r.source, "then": "open_session"})));
                }
            }
        }
        // Tabs from the history (latest record per session or tab).
        let mut seen = std::collections::HashSet::new();
        for r in read_all().iter().rev() {
            // Tab ids from before they were kept across restarts repeat,
            // so a record without a session is its tab in its folder.
            let k = r
                .session_id
                .clone()
                .unwrap_or_else(|| format!("{} {}", r.tab, r.cwd));
            if !seen.insert(k) {
                continue;
            }
            let at = chrono::DateTime::parse_from_rfc3339(&r.at)
                .ok()
                .map(std::time::SystemTime::from);
            if since.is_some_and(|t| at.is_none_or(|a| a < t)) {
                continue;
            }
            let sc = score(&r.name, &r.cwd, "");
            if sc >= 0.6 {
                hits.push((sc * 0.98, json!({"type": "tab", "id": r.id, "label": r.name, "folder": crate::config::tilde(std::path::Path::new(&r.cwd)), "when": ts(at), "last_event": r.event, "session": r.session_id, "then": "reopen_tab"})));
            }
        }
        // Your own chats.
        for c in crate::assistant_history::list().into_iter().take(200) {
            let sc = score(&c.first, "", &c.text);
            if sc >= 0.6 {
                hits.push((sc * 0.9, json!({"type": "chat", "id": c.id, "label": crate::sessions::snippet(&c.first, 80), "when": c.started, "then": "history action=detail"})));
            }
        }
        hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        hits.into_iter()
            .map(|(sc, mut v)| {
                v["score"] = json!((sc * 100.0).round() / 100.0);
                v
            })
            .collect()
    }

    /// The first tab that `f` accepts.
    #[cfg(test)]
    pub fn all_tabs_where(&self, f: impl Fn(&crate::pane::Pane) -> bool) -> Option<(usize, usize)> {
        self.all_open_tabs()
            .into_iter()
            .find(|&(s, t)| f(&self.panes[s].tabs[t]))
    }

    fn all_open_tabs(&self) -> Vec<(usize, usize)> {
        (0..self.panes.len())
            .flat_map(|s| (0..self.panes[s].tabs.len()).map(move |t| (s, t)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(event: &str, name: &str, cwd: &str, at: &str) -> Record {
        Record {
            id: name.into(),
            at: at.into(),
            event: event.into(),
            account: "one".into(),
            tab: "t1".into(),
            name: name.into(),
            cwd: cwd.into(),
            session_id: Some(format!("s-{name}")),
            ..Default::default()
        }
    }

    #[test]
    fn filters() {
        let now = chrono::Local::now();
        let y = (now - chrono::Duration::days(1)).to_rfc3339();
        let t = now.to_rfc3339();
        let all = vec![
            rec("open", "botmesh", "/u/lol/botmesh", &y),
            rec("close", "botmesh", "/u/lol/botmesh", &y),
            rec("close", "calc", "/u/calc", &t),
            rec("open", "api", "/u/api", &t),
        ];
        let closed = query(
            &all,
            &Filter {
                event: Some("closed".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            closed.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["calc", "botmesh"],
            "newest first"
        );
        let today = query(
            &all,
            &Filter {
                since: Some((now - chrono::Duration::hours(2)).into()),
                ..Default::default()
            },
        );
        assert_eq!(today.len(), 2);
        for q in ["bot mesh", "pod mesh", "lol slash botmesh"] {
            let hits = query(
                &all,
                &Filter {
                    text: Some(q.into()),
                    ..Default::default()
                },
            );
            assert!(
                !hits.is_empty() && hits.iter().all(|r| r.name == "botmesh"),
                "{q}"
            );
        }
        assert_eq!(
            query(
                &all,
                &Filter {
                    limit: 1,
                    ..Default::default()
                }
            )
            .len(),
            1
        );
    }
}
