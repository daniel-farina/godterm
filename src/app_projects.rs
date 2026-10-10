//! The assistant's project tools: search_projects (look deeper inside
//! project folders on demand), describe_project and describe_session
//! (saved descriptions, so the next find is instant) and rename_session
//! (the harness's own session title), plus what find and sessions take
//! from the project index and the saved descriptions.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::app::App;
use crate::control::{plural, tab_id};

pub const TOOL_NAMES: &[&str] = &[
    "search_projects",
    "describe_project",
    "describe_session",
    "rename_session",
];

fn ok(v: Value) -> Value {
    json!({"ok": true, "result": v})
}

fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    match v? {
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| crate::sessions::snippet(s.trim(), 80))
                .filter(|s| !s.is_empty())
                .take(20)
                .collect(),
        ),
        Value::String(s) => Some(
            s.split(',')
                .map(|s| crate::sessions::snippet(s.trim(), 80))
                .filter(|s| !s.is_empty())
                .take(20)
                .collect(),
        ),
        _ => None,
    }
}

impl App {
    /// Folders worth mapping besides the sessions' own: open tabs, the
    /// tab history and saved project descriptions.
    pub fn project_extra_roots(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| t.cwd.clone()))
            .collect();
        for r in crate::tab_history::read_all().iter().rev().take(300) {
            v.push(PathBuf::from(&r.cwd));
        }
        for m in crate::project_memory::load().items {
            if m.kind == "project" {
                v.push(PathBuf::from(&m.key));
            }
        }
        v
    }

    /// The roots the project index maps (for open_path and
    /// search_projects).
    pub fn project_roots(&self) -> Vec<PathBuf> {
        let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<PathBuf> = ix.roots.keys().cloned().collect();
        v.sort();
        v
    }

    /// find's hits from saved descriptions and the project index:
    /// (score, hit).
    pub fn project_hits(&self, query: &str, limit: usize) -> Vec<(f64, Value)> {
        let mut out: Vec<(f64, Value)> = vec![];
        let store = crate::project_memory::load();
        let today = chrono::Local::now().date_naive();
        let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
        let mut saved_paths = std::collections::HashSet::new();
        for m in &store.items {
            let Some((sc, on)) = crate::project_memory::score(m, query) else {
                continue;
            };
            let stale = crate::project_memory::stale(m, &ix, today);
            let when = m
                .last_deep_search
                .as_deref()
                .map(|d| format!("deep-searched {d}"))
                .unwrap_or_else(|| format!("saved {}", m.updated.get(..10).unwrap_or("")));
            let why = format!("from saved description ({when}), matched its {on}");
            let saved = crate::project_memory::json(m, stale.as_deref());
            // Saved entries rank first: a small lift over the index.
            let sc = (sc + 0.05).min(1.05);
            if m.kind == "project" {
                saved_paths.insert(PathBuf::from(&m.key));
                out.push((sc, json!({"type": "project", "path": m.key, "folder": crate::config::tilde(Path::new(&m.key)), "label": if m.description.is_empty() { crate::config::tilde(Path::new(&m.key)) } else { crate::sessions::snippet(&m.description, 80) }, "why": why, "saved": saved, "then": "open_tab with dir set to path, open_path, or list_dir"})));
            } else {
                out.push((sc, json!({"type": "session", "id": m.key, "label": crate::sessions::snippet(&m.description, 80), "why": why, "saved": saved, "then": "open_session"})));
            }
        }
        for h in crate::project_index::search(&ix, query, limit) {
            if saved_paths.contains(&h.path) {
                continue;
            }
            let mut v = json!({"type": "project", "path": h.path.to_string_lossy(), "folder": crate::config::tilde(&h.path), "label": if h.rel.is_empty() { crate::config::tilde(&h.root) } else { h.rel.clone() }, "why": h.why, "root": crate::config::tilde(&h.root), "then": "open_tab with dir set to path, open_path, or list_dir"});
            if let Some(m) = store.get("project", &h.path.to_string_lossy()) {
                v["saved"] = crate::project_memory::json(m, None);
            }
            out.push((h.score, v));
        }
        out
    }

    /// Run a project tool; None when `tool` is not one of them.
    pub fn projects_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        if !TOOL_NAMES.contains(&tool) {
            return None;
        }
        Some(match tool {
            "search_projects" => self.search_projects(args),
            "describe_project" => self.save_description(args, "project"),
            "describe_session" => self.save_description(args, "session"),
            _ => self.planned("rename_session", args),
        })
    }

    fn search_projects(&mut self, args: &Value) -> Result<Value, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let q = s("query")
            .filter(|q| !q.trim().is_empty())
            .ok_or("query is required")?;
        let allowed = self.workspaces();
        let roots: Vec<PathBuf> = match s("path").or_else(|| s("root")) {
            Some(p) => {
                let p = crate::config::expand_tilde(&crate::fuzzy::spoken_punctuation(&p));
                if !p.is_dir() {
                    return Err(format!("{} is not a folder", crate::config::tilde(&p)));
                }
                if let Some(why) = crate::guard::denied(&p, &allowed) {
                    return Err(format!("refused: {why}"));
                }
                if p == crate::config::home_dir() || crate::config::home_dir().starts_with(&p) {
                    return Err("too wide: name a project folder, not the home folder".into());
                }
                vec![p]
            }
            None => {
                self.kick_index(false);
                let all = self.project_roots();
                // Nested roots are walked as part of their parent.
                all.iter()
                    .filter(|r| !all.iter().any(|o| o != *r && r.starts_with(o)))
                    .cloned()
                    .collect()
            }
        };
        if roots.is_empty() {
            return Ok(ok(
                json!({"hits": [], "say": "No project folders are known yet; name one with path."}),
            ));
        }
        let content = args
            .get("content")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut lim = crate::project_index::DeepLimits::default();
        if let Some(d) = args.get("depth").and_then(Value::as_u64) {
            lim.depth = d.clamp(1, 8) as usize;
        }
        if let Some(n) = args.get("limit").and_then(Value::as_u64) {
            lim.max_hits = n.clamp(1, 30) as usize;
        }
        let r = crate::project_index::deep_search(&roots, &q, content, &lim, &allowed);
        // Remember the day each root was deep-searched.
        let mut store = crate::project_memory::load();
        for root in &roots {
            store.note_deep_search(root);
        }
        let _ = crate::project_memory::save(&store);
        let hits: Vec<Value> = r
            .hits
            .iter()
            .map(|h| {
                let mut v = json!({"path": h.path.to_string_lossy(), "folder": crate::config::tilde(&h.path), "kind": h.kind, "why": h.why, "score": (h.score * 100.0).round() / 100.0});
                if let Some(l) = &h.line {
                    v["line"] = json!(l);
                }
                v
            })
            .collect();
        let say = if hits.is_empty() {
            format!(
                "Nothing named like that in {}{}.",
                plural(roots.len(), "project folder"),
                if content { " (names and contents)" } else { "" }
            )
        } else {
            format!(
                "{} in {}.",
                plural(hits.len(), "match"),
                plural(roots.len(), "project folder")
            )
        };
        let mut out = json!({"hits": hits, "roots": roots.iter().map(|r| crate::config::tilde(r)).collect::<Vec<_>>(), "entries_seen": r.seen, "files_read": r.files_read, "truncated": r.truncated, "timed_out": r.timed_out, "say": say});
        if !hits_found(&out) && !content {
            out["next"] = json!(
                "Try again with content true to grep inside files, or list_dir into a likely root."
            );
        } else if hits_found(&out) {
            out["next"] = json!("If this is what the user meant, save it with describe_project (aliases = what they called it) so find gets it at once next time.");
        }
        Ok(ok(out))
    }

    fn save_description(&mut self, args: &Value, kind: &str) -> Result<Value, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let key = if kind == "project" {
            let p = s("path").ok_or("path is required (the project or subproject folder)")?;
            let p = crate::config::expand_tilde(&p);
            let p = p.canonicalize().unwrap_or(p);
            if !p.is_dir() {
                return Err(format!("{} is not a folder", crate::config::tilde(&p)));
            }
            if let Some(why) = crate::guard::denied(&p, &self.workspaces()) {
                return Err(format!("refused: {why}"));
            }
            p.to_string_lossy().into_owned()
        } else {
            let id = s("id").ok_or("id is required (a session id from sessions or find)")?;
            self.kick_index(false);
            let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
            let m: Vec<&crate::session_index::Row> = ix
                .rows
                .iter()
                .filter(|r| r.info.id.starts_with(&id))
                .collect();
            match m.as_slice() {
                [r] => r.info.id.clone(),
                [] => return Err(format!("no session {id}")),
                _ => {
                    return Err(format!(
                        "{id} matches several sessions; give more of the id"
                    ))
                }
            }
        };
        let fp = (kind == "project")
            .then(|| {
                let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
                crate::project_memory::fingerprint(&ix, Path::new(&key))
            })
            .flatten();
        let mut store = crate::project_memory::load();
        let m = store.entry(kind, &key);
        if let Some(d) = s("description") {
            m.description = crate::sessions::snippet(d.trim(), 400);
        }
        if let Some(a) = strings(args.get("aliases")) {
            m.aliases = a;
        }
        if let Some(a) = strings(args.get("subprojects")) {
            m.subprojects = a;
        }
        if let Some(a) = strings(args.get("tags")) {
            m.tags = a;
        }
        if args.get("deep_searched").and_then(Value::as_bool) == Some(true) {
            m.last_deep_search = Some(crate::project_memory::today());
        }
        if let Some((t, n)) = fp {
            m.index_mtime = Some(t);
            m.index_entries = Some(n);
        }
        m.updated = chrono::Local::now().to_rfc3339();
        if m.description.is_empty() && m.aliases.is_empty() {
            return Err("give a description or aliases to save".into());
        }
        let saved = crate::project_memory::json(m, None);
        crate::project_memory::save(&store)?;
        crate::log::info(&format!("control: saved a description of {kind} {key}"));
        let shown = if kind == "project" {
            crate::config::tilde(Path::new(&key))
        } else {
            key.clone()
        };
        Ok(ok(
            json!({"saved": saved, "key": key, "say": format!("Saved what {shown} is.")}),
        ))
    }

    /// rename_session's plan: each session resolved, with how it is
    /// renamed (its tab's /rename, or the harness's title record). One
    /// session runs at once; several ask once.
    pub(crate) fn rename_plan_for(
        &mut self,
        args: &Value,
    ) -> Result<(Value, String, bool), String> {
        let ids: Vec<String> = match args.get("id").or(args.get("ids")) {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            _ => return Err("id is required (a session id, or a list)".into()),
        };
        let names: Vec<String> = match args.get("name").or(args.get("names")) {
            Some(Value::String(s)) => vec![s.clone(); ids.len()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            _ => return Err("name is required".into()),
        };
        if ids.is_empty() || names.len() != ids.len() {
            return Err("give one name, or one name per id".into());
        }
        self.kick_index(false);
        let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
        let mut items = vec![];
        let mut said = vec![];
        for (id, name) in ids.iter().zip(&names) {
            let name = crate::session_rename::clean_name(name)?;
            let m: Vec<&crate::session_index::Row> = ix
                .rows
                .iter()
                .filter(|r| r.info.id.starts_with(id.as_str()))
                .collect();
            let r = match m.as_slice() {
                [r] => *r,
                [] if ix.building && ix.built.is_none() => {
                    return Err("still indexing sessions; try again in a few seconds".into())
                }
                [] => return Err(format!("no session {id} (ids come from sessions or find)")),
                _ => {
                    return Err(format!(
                        "{id} matches several sessions; give more of the id"
                    ))
                }
            };
            said.push(format!(
                "\"{}\" to \"{name}\"",
                crate::sessions::snippet(&r.info.summary(), 40)
            ));
            items.push(json!({"id": r.info.id, "name": name, "harness": r.harness.name(), "path": r.info.path.to_string_lossy()}));
        }
        let q = if said.len() == 1 {
            format!("Rename {}?", said[0])
        } else {
            format!("Rename {} sessions: {}?", said.len(), said.join(", "))
        };
        let n = items.len();
        Ok((json!({"items": items}), q, n > 1))
    }

    pub(crate) fn rename_run(&mut self, plan: &Value) -> Result<Value, String> {
        let mut done = vec![];
        let mut failed = vec![];
        for it in plan["items"].as_array().into_iter().flatten() {
            let id = it["id"].as_str().unwrap_or("").to_string();
            let name = it["name"].as_str().unwrap_or("").to_string();
            let path = PathBuf::from(it["path"].as_str().unwrap_or(""));
            let grok = it["harness"].as_str() == Some(crate::harness::Harness::Grok.name());
            // Open in a tab that runs it: the harness's own /rename there,
            // and the tab gets the name too.
            let tab = (0..self.panes.len())
                .flat_map(|s| (0..self.panes[s].tabs.len()).map(move |t| (s, t)))
                .find(|&(s, t)| {
                    let x = &self.panes[s].tabs[t];
                    x.session_id.as_deref() == Some(id.as_str())
                        && (x.is_running() || x.pending.is_some() || x.suspended)
                });
            if let Some((s, t)) = tab {
                let uid = self.panes[s].tabs[t].uid;
                let d = self.deliver(uid, &format!("/rename {name}"));
                self.rename_tab(s, t, &name);
                crate::log::info(&format!(
                    "control: rename_session {id} via /rename in {}",
                    tab_id(uid)
                ));
                done.push(json!({"id": id, "name": name, "how": "/rename in its tab", "tab": tab_id(uid), "delivery": d}));
                continue;
            }
            let r = if grok {
                crate::session_rename::set_grok_title(&path, &name)
            } else {
                crate::session_rename::append_claude_title(&path, &id, &name)
            };
            match r {
                Ok(()) => {
                    crate::log::info(&format!(
                        "control: rename_session {id} to \"{name}\" in its {} file",
                        if grok { "summary" } else { "transcript" }
                    ));
                    done.push(json!({"id": id, "name": name, "how": if grok { "title in summary.json" } else { "title record in the transcript" }}));
                }
                Err(e) => failed.push(json!({"id": id, "error": e})),
            }
        }
        if !done.is_empty() {
            self.kick_index(true);
        }
        let say = match (done.len(), failed.len()) {
            (1, 0) => format!(
                "Renamed it to \"{}\".",
                done[0]["name"].as_str().unwrap_or("")
            ),
            (n, 0) => format!("Renamed {}.", plural(n, "session")),
            (n, f) => format!(
                "Renamed {}; {} could not be renamed.",
                plural(n, "session"),
                plural(f, "session")
            ),
        };
        Ok(ok(json!({"renamed": done, "failed": failed, "say": say})))
    }
}

fn hits_found(v: &Value) -> bool {
    v["hits"].as_array().is_some_and(|a| !a.is_empty())
}
