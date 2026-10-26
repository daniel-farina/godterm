//! The assistant's git_status tool: the git state of every project (or
//! one) from the project index's cached facts, brought up to date first
//! (cheap: only repos whose refs or index changed, or that a tab just
//! worked in, run git). refresh true also fetches every repo with a
//! remote, in the background.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::app::App;
use crate::control::plural;
use crate::project_git::{self, Row};

/// Rows shown at most (the summary counts every one).
const MAX_ROWS: usize = 60;
/// One fetch may take this long per repo.
const FETCH_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

fn names(rows: &[&Row], n: usize) -> String {
    let mut v: Vec<String> = rows
        .iter()
        .take(n)
        .map(|r| {
            r.path
                .file_name()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_else(|| crate::config::tilde(&r.path))
        })
        .collect();
    if rows.len() > n {
        v.push(format!("{} more", rows.len() - n));
    }
    v.join(", ")
}

/// What a filter means, for the spoken answer.
fn filter_words(f: &str) -> &'static str {
    match f {
        "no_remote" => {
            "are only local (no remote, a remote that is only a folder here, or not in git)"
        }
        "not_github" => "are not on GitHub",
        "dirty" => "have uncommitted changes",
        "unpushed" => "have commits or a branch not pushed",
        "behind" => "are behind their remote (as of the last fetch)",
        _ => "",
    }
}

impl App {
    /// The git facts of `dir` from the cache, one line (never runs git).
    pub fn git_line(&self, dir: &Path) -> Option<String> {
        let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
        if ix.git.is_empty() {
            return None;
        }
        project_git::line_for(&ix, dir)
    }

    /// "40 projects: 31 on GitHub, 5 only local, 3 dirty, 2 unpushed"
    /// from the cache, or None before the first git pass.
    pub fn git_summary_line(&self) -> Option<String> {
        let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
        if ix.git.is_empty() {
            return None;
        }
        let rows = project_git::rows(&ix);
        let s = project_git::summary(&rows);
        let n = |k: &str| s[k].as_u64().unwrap_or(0);
        Some(format!(
            "{}: {} on GitHub, {} with no remote, {} not in git, {} with uncommitted changes, {} with unpushed work (git_status for details)",
            plural(n("projects") as usize, "project"),
            n("on_github"),
            n("no_remote") + n("local_path_remote_only"),
            n("not_a_git_repo"),
            n("dirty"),
            n("unpushed"),
        ))
    }

    /// A tab finished a turn in `dir`: recompute its repo's facts soon.
    pub fn git_tab_finished(&mut self, dir: &Path) {
        let Some(top) = project_git::mark_stale(&self.project_index, dir) else {
            return;
        };
        if cfg!(test) {
            return;
        }
        let shared = std::sync::Arc::clone(&self.project_index);
        std::thread::spawn(move || {
            project_git::update(&shared, Some(&[top]), &project_git::Limits::tool(), false);
        });
    }

    /// The folders `project` names: a path, else the best index hit.
    fn git_project(&mut self, q: &str) -> Result<Vec<PathBuf>, String> {
        let p = crate::config::expand_tilde(&crate::fuzzy::spoken_punctuation(q.trim()));
        if p.is_absolute() {
            if !p.is_dir() {
                return Err(format!("{} is not a folder", crate::config::tilde(&p)));
            }
            if let Some(why) = crate::guard::denied(&p, &self.workspaces()) {
                return Err(format!("refused: {why}"));
            }
            return Ok(vec![p]);
        }
        let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
        let hits = crate::project_index::search(&ix, q, 5);
        // A project root beats a folder inside one.
        let best = hits
            .iter()
            .find(|h| h.rel.is_empty() && h.score >= 0.7)
            .or(hits.first())
            .map(|h| h.path.clone());
        best.map(|b| vec![b])
            .ok_or_else(|| format!("no project named like \"{q}\" (try find, or give its path)"))
    }

    pub(crate) fn git_status(&mut self, args: &Value) -> Result<Value, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let filter = s("filter")
            .unwrap_or_else(|| "all".into())
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "_");
        let filter = match filter.as_str() {
            "local" | "local_only" | "only_local" | "no_remotes" => "no_remote".to_string(),
            "uncommitted" | "changes" | "modified" => "dirty".to_string(),
            "ahead" | "not_pushed" => "unpushed".to_string(),
            _ => filter,
        };
        if !project_git::FILTERS.contains(&filter.as_str()) {
            return Err(format!(
                "filter must be one of {}",
                project_git::FILTERS.join(", ")
            ));
        }
        let refresh = args.get("refresh").and_then(Value::as_bool) == Some(true);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, 200) as usize)
            .unwrap_or(MAX_ROWS);
        let shared = std::sync::Arc::clone(&self.project_index);
        crate::project_index::load(&shared);
        self.kick_index(false);
        let only = match s("project").filter(|p| !p.trim().is_empty()) {
            Some(q) => Some(self.git_project(&q)?),
            None => None,
        };
        let (empty, building) = {
            let ix = shared.lock().unwrap_or_else(|e| e.into_inner());
            (ix.roots.is_empty(), ix.building)
        };
        if empty && only.is_none() {
            let say = if building {
                "Still indexing the projects; ask again in a few seconds."
            } else {
                "No project folders are known yet."
            };
            return Ok(json!({"ok": true, "result": {"rows": [], "say": say}}));
        }
        let st = project_git::update(
            &shared,
            only.as_deref(),
            &project_git::Limits::tool(),
            false,
        );
        let (all, fetching) = {
            let ix = shared.lock().unwrap_or_else(|e| e.into_inner());
            let mut rows = project_git::rows(&ix);
            if let Some(o) = &only {
                let tops: Vec<PathBuf> = o
                    .iter()
                    .map(|p| project_git::repo_top(p).unwrap_or_else(|| p.clone()))
                    .collect();
                rows.retain(|r| tops.contains(&r.path));
                if rows.is_empty() {
                    // A folder the index does not know yet.
                    rows = tops
                        .iter()
                        .map(|t| Row {
                            path: t.clone(),
                            facts: ix.git.get(t).cloned(),
                            inside: vec![],
                            holds: 0,
                        })
                        .collect();
                }
            }
            (rows, ix.git_fetching)
        };
        let summary = project_git::summary(&all);
        let picked: Vec<&Row> = all.iter().filter(|r| r.matches(&filter)).collect();
        let shown: Vec<&Row> = picked.iter().take(limit).copied().collect();
        let n = |k: &str| summary[k].as_u64().unwrap_or(0);
        let mut say = if let (Some(_), [r]) = (&only, picked.as_slice()) {
            match &r.facts {
                Some(f) => format!("{}: {}.", crate::config::tilde(&r.path), f.line()),
                None => format!("{} is not a git repo.", crate::config::tilde(&r.path)),
            }
        } else if filter == "all" {
            format!(
                "{}: {} on GitHub, {} with another host only, {} with no remote, {} with only a local folder remote, {} not in git. {} with uncommitted changes, {} with unpushed work, {} behind (as of the last fetch).",
                plural(n("projects") as usize, "project"),
                n("on_github"),
                n("other_host_only"),
                n("no_remote"),
                n("local_path_remote_only"),
                n("not_a_git_repo"),
                n("dirty"),
                n("unpushed"),
                n("behind"),
            )
        } else if picked.is_empty() {
            format!(
                "None of the {} {}.",
                plural(n("projects") as usize, "project"),
                filter_words(&filter)
            )
        } else {
            format!(
                "{} of {} {}: {}.",
                picked.len(),
                plural(n("projects") as usize, "project"),
                filter_words(&filter),
                names(&picked, 8)
            )
        };
        let mut out = json!({
            "filter": filter,
            "table": project_git::table(&shown),
            "summary": summary,
            "as_of": "local refs: ahead, behind and on-remote are as of each repo's last fetch (shown per row); nothing was fetched unless refresh is true",
            "checked": {"repos": st.repos, "recomputed": st.computed, "unchanged": st.reused, "took_ms": st.took_ms},
        });
        if picked.len() > shown.len() {
            out["more"] = json!(picked.len() - shown.len());
        }
        if only.is_some() {
            out["repos"] = json!(picked
                .iter()
                .map(|r| {
                    let mut v = r.facts.as_ref().map(|f| f.json()).unwrap_or_else(
                        || json!({"repo": crate::config::tilde(&r.path), "git": false}),
                    );
                    if !r.inside.is_empty() {
                        v["known_folders_inside"] = json!(r
                            .inside
                            .iter()
                            .map(|p| crate::config::tilde(p))
                            .collect::<Vec<_>>());
                    }
                    v
                })
                .collect::<Vec<_>>());
        }
        if refresh {
            // Every repo in scope, not only the filtered ones: a fetch is
            // what shows which are behind.
            let tops: Vec<PathBuf> = all
                .iter()
                .filter_map(|r| r.facts.as_ref())
                .filter(|f| !f.remotes.is_empty() && f.error.is_none())
                .map(|f| f.top.clone())
                .collect();
            if fetching {
                out["fetch"] = json!("a fetch is already running");
            } else if tops.is_empty() {
                out["fetch"] = json!("nothing to fetch: none of these has a remote");
            } else {
                shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .git_fetching = true;
                let k = tops.len();
                let work = move || {
                    let (ok, failed) = project_git::fetch_all(&tops, FETCH_LIMIT);
                    crate::log::info(&format!(
                        "project git: fetched {ok} repos, {} failed",
                        failed.len()
                    ));
                    for (t, e) in &failed {
                        crate::log::info(&format!(
                            "project git: fetch {} failed: {e}",
                            crate::config::tilde(t)
                        ));
                    }
                    project_git::update(&shared, Some(&tops), &project_git::Limits::tool(), true);
                    shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .git_fetching = false;
                };
                if cfg!(test) {
                    work();
                    out["fetch"] = json!(format!("fetched {}", plural(k, "repo")));
                } else {
                    std::thread::spawn(work);
                    out["fetch"] = json!(format!(
                        "fetching {} in the background (no prompts, 30 s each); ask again in a minute for numbers as of now",
                        plural(k, "repo")
                    ));
                    say.push_str(&format!(
                        " Fetching {} now to check the remotes.",
                        plural(k, "repo")
                    ));
                }
            }
        }
        out["say"] = json!(say);
        Ok(json!({"ok": true, "result": out}))
    }
}

/// `godterm projects [--git] [--filter F] [--fetch] [--in DIR]`: the git
/// state of every project, for humans, with how long the cold and warm
/// passes took. --in DIR lists DIR's subfolders instead of the index
/// (nothing of GodTerm's own is read or written then).
pub fn command(args: &[String]) -> anyhow::Result<()> {
    let flag = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let filter = flag("--filter").unwrap_or_else(|| "all".into());
    if !project_git::FILTERS.contains(&filter.as_str()) {
        anyhow::bail!(
            "--filter must be one of {}",
            project_git::FILTERS.join(", ")
        );
    }
    let shared: crate::project_index::Shared = Default::default();
    let t0 = std::time::Instant::now();
    if let Some(dir) = flag("--in") {
        let dir = crate::config::expand_tilde(&dir);
        let mut subs: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.path())
            .collect();
        subs.sort();
        let lim = crate::project_index::Limits::default();
        let mut ix = shared.lock().unwrap_or_else(|e| e.into_inner());
        for s in subs {
            if crate::guard::denied(&s, &[]).is_none() {
                let m = crate::project_index::scan_root(&s, &lim, &[]);
                ix.roots.insert(s, m);
            }
        }
    } else {
        let (tx, _rx) = std::sync::mpsc::channel();
        let app = App::new(crate::config::Config::load_or_init()?, tx);
        crate::session_index::rebuild(&app.session_index, &app.index_sources());
        crate::project_index::refresh_from_sessions(
            &shared,
            &app.session_index,
            &app.project_extra_roots(),
            false,
            &app.workspaces(),
        );
    }
    let index_ms = t0.elapsed().as_millis();
    let lim = project_git::Limits::tool();
    let cold = project_git::update(&shared, None, &lim, true);
    let warm = project_git::update(&shared, None, &lim, false);
    if args.iter().any(|a| a == "--fetch") {
        let tops: Vec<PathBuf> = {
            let ix = shared.lock().unwrap_or_else(|e| e.into_inner());
            ix.git
                .values()
                .filter(|f| !f.remotes.is_empty())
                .map(|f| f.top.clone())
                .collect()
        };
        let t = std::time::Instant::now();
        let (ok, failed) = project_git::fetch_all(&tops, FETCH_LIMIT);
        println!(
            "fetched {ok} of {} repos in {} ms",
            tops.len(),
            t.elapsed().as_millis()
        );
        for (t, e) in failed {
            println!("  fetch failed: {}: {e}", crate::config::tilde(&t));
        }
        project_git::update(&shared, Some(&tops), &lim, true);
    }
    let ix = shared.lock().unwrap_or_else(|e| e.into_inner());
    let rows = project_git::rows(&ix);
    let picked: Vec<&Row> = rows.iter().filter(|r| r.matches(&filter)).collect();
    print!("{}", project_git::table(&picked));
    println!();
    println!(
        "{}",
        serde_json::to_string_pretty(&project_git::summary(&rows))?
    );
    println!(
        "{} roots indexed in {index_ms} ms; git cold: {} repos in {} ms ({} threads), warm: {} unchanged in {} ms",
        ix.roots.len(),
        cold.computed,
        cold.took_ms,
        lim.threads,
        warm.reused,
        warm.took_ms
    );
    Ok(())
}
