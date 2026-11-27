use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use crate::app::App;
use crate::app_sessions::Src;
use crate::harness::Harness;
use crate::session_index::Row;

pub const PAGE_MAX: usize = 100;

pub struct Filter {
    pub query: String,
    pub harness: Option<Harness>,
    pub account: Option<Option<usize>>,
}

pub fn matches(f: &Filter, r: &Row) -> bool {
    if f.harness.is_some_and(|h| r.harness != h) {
        return false;
    }
    if let Some(a) = f.account {
        let this = match r.src {
            Src::Account(i) => Some(i),
            _ => None,
        };
        if this != a {
            return false;
        }
    }
    true
}

pub fn text_score(query: &str, r: &Row) -> Option<usize> {
    let q = crate::fuzzy::spoken_punctuation(query);
    if q.trim().is_empty() {
        return Some(0);
    }
    let i = &r.info;
    let hay = format!(
        "{} {} {} {} {} {}",
        i.name.as_deref().unwrap_or(""),
        i.title.as_deref().unwrap_or(""),
        i.first_prompt.as_deref().unwrap_or(""),
        i.last_prompt.as_deref().unwrap_or(""),
        i.cwd,
        r.harness.name()
    );
    crate::session_index::score(&q, &hay).or_else(|| {
        let f = crate::session_index::fuzzy_score(&q, i, None);
        (f >= 0.78).then_some((f * 100.0) as usize)
    })
}

impl App {
    pub(crate) fn lost_sessions(&mut self, args: &Value) -> Result<Value, String> {
        self.kick_index(false);
        let query = args
            .get("query")
            .or_else(|| args.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, PAGE_MAX as u64) as usize;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let harness = args
            .get("harness")
            .and_then(Value::as_str)
            .filter(|h| !h.is_empty() && *h != "all")
            .map(Harness::of);
        let account = match args.get("account") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() || s == "all" => None,
            Some(Value::String(s)) if s == "main" => Some(None),
            Some(v) => Some(Some(self.account_arg(v)?)),
        };
        let with_headless = args["include_headless"].as_bool() == Some(true);
        let f = Filter {
            query,
            harness,
            account,
        };

        let open: HashSet<String> = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| t.session_id.clone())
            .collect();
        let closed: HashMap<String, (String, String)> = {
            let mut m = HashMap::new();
            for r in crate::tab_history::read_all() {
                if r.event == "close" {
                    if let Some(id) = r.session_id.clone().filter(|i| !i.is_empty()) {
                        m.insert(id, (r.name.clone(), r.at.chars().take(16).collect()));
                    }
                }
            }
            m
        };
        let own = self.own_pids();
        let live: HashMap<String, crate::takeover::Live> = self
            .live_sessions()
            .into_iter()
            .map(|l| (l.session_id.clone(), l))
            .collect();
        let procs = crate::activity::process_table_cached();

        let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
        let building = ix.building && ix.built.is_none();
        let mut hits: Vec<(usize, &Row)> = ix
            .rows
            .iter()
            .filter(|r| r.info.parent.is_none())
            .filter(|r| with_headless || !crate::sess_sort::headless(&r.info))
            .filter(|r| !open.contains(&r.info.id))
            .filter(|r| matches(&f, r))
            .filter_map(|r| text_score(&f.query, r).map(|s| (s, r)))
            .collect();
        if f.query.trim().is_empty() {
            hits.sort_by_key(|(_, r)| std::cmp::Reverse(r.info.modified));
        } else {
            hits.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then(b.1.info.modified.cmp(&a.1.info.modified))
            });
        }
        let mut seen = HashSet::new();
        hits.retain(|(_, r)| seen.insert((r.harness, r.info.id.clone())));
        let total = hits.len();

        let rows: Vec<Value> = hits
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(_, r)| {
                let mut v = crate::session_index::row_json(r, None, false);
                let (acct_n, acct_label) = match r.src {
                    Src::Account(a) => (
                        Some(a + 1),
                        self.cfg
                            .accounts
                            .get(a)
                            .map(|c| c.display().to_string())
                            .unwrap_or_default(),
                    ),
                    _ => (None, r.source.clone()),
                };
                v["account"] = json!(acct_n);
                v["account_label"] = json!(acct_label);
                v["folder"] = v["project"].clone();
                v["folder_exists"] = json!(std::path::Path::new(&r.info.cwd).is_dir());
                if let Some((name, at)) = closed.get(&r.info.id) {
                    v["closed_tab"] = json!({"name": name, "at": at.replace('T', " ")});
                }
                let running = live
                    .get(&r.info.id)
                    .map(|l| {
                        json!({"pid": l.pid, "tty": l.tty, "terminal": crate::takeover::terminal_of(l.pid), "desktop": procs.iter().any(|p| p.pid == l.pid && is_desktop_cmd(&p.cmd))})
                    })
                    .or_else(|| {
                        procs
                            .iter()
                            .find(|p| {
                                !own.contains(&p.pid)
                                    && p.cmd.contains(r.info.id.as_str())
                                    && p.cmd.contains(r.harness.name())
                            })
                            .map(|p| json!({"pid": p.pid, "desktop": is_desktop_cmd(&p.cmd)}))
                    });
                v["running"] = json!(running.is_some());
                if let Some(x) = running {
                    v["external"] = x;
                    v["warning"] = json!("Running in a terminal right now: a second copy can fork the conversation. take_over_session moves it here instead.");
                }
                v["session_id"] = v["id"].clone();
                v
            })
            .collect();
        let say = if rows.is_empty() {
            "No sessions outside GodTerm match.".to_string()
        } else {
            format!(
                "{} outside GodTerm, {} shown.",
                crate::control::plural(total, "session"),
                rows.len()
            )
        };
        Ok(json!({
            "sessions": rows, "matched": total, "shown": rows.len(), "offset": offset,
            "indexing": building, "indexed": ix.rows.len(), "say": say
        }))
    }
}

fn is_desktop_cmd(cmd: &str) -> bool {
    cmd.to_lowercase().contains("claude.app") || cmd.contains("Application Support/Claude/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionInfo;

    fn row(h: Harness, src: Src, id: &str, prompt: &str) -> Row {
        let info: SessionInfo = serde_json::from_value(json!({
            "id": id, "path": "/x/a.jsonl", "cwd": "/Users/x/proj", "modified": null, "messages": 3,
            "first_prompt": prompt, "title": null, "model": null,
            "tokens": {"input":0,"output":0,"cache_creation":0,"cache_read":0,"context":0},
            "last_prompt": null, "subagents": 0, "parent": null
        }))
        .unwrap();
        Row {
            src,
            harness: h,
            source: "main".into(),
            info,
        }
    }

    #[test]
    fn filters_and_text() {
        let r = row(Harness::Grok, Src::Account(1), "abc", "fix the login flow");
        let f = |h, a| Filter {
            query: String::new(),
            harness: h,
            account: a,
        };
        assert!(matches(&f(None, None), &r));
        assert!(matches(&f(Some(Harness::Grok), Some(Some(1))), &r));
        assert!(!matches(&f(Some(Harness::Claude), None), &r));
        assert!(!matches(&f(None, Some(None)), &r));
        assert!(!matches(&f(None, Some(Some(0))), &r));
        assert!(text_score("login", &r).is_some());
        assert!(text_score("zebra giraffe", &r).is_none());
        assert_eq!(text_score("", &r), Some(0));
    }
}
