use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::app::App;
use crate::pane::Activity;

pub const FILE: &str = "apps.json";

pub const KINDS: &[&str] = &[
    "app",
    "game",
    "utility",
    "dashboard",
    "page",
    "tool",
    "library",
    "other",
];

pub const SCHEMA: &str = r#"{"version":1,"generated_at":"<UTC time, ISO 8601>","apps":[{"name":"Short name","kind":"app","description":"One or two plain sentences.","path":"/abs/path/to/app","project":"/abs/path/to/project/root","tech":"Main languages and frameworks","run":"optional short how to launch","tags":["a","few","words"]}]}"#;

const SHOWN_SESSIONS: usize = 30;
const LISTED_FOLDERS: usize = 40;
const PREFILL_WAIT: Duration = Duration::from_secs(180);
const WORKTREES: &str = "/.claude/worktrees/";

pub fn catalog_path() -> PathBuf {
    crate::config::app_home().join(FILE)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogApp {
    pub name: String,
    pub kind: String,
    pub description: String,
    pub path: String,
    pub project: String,
    pub tech: String,
    pub run: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    pub generated_at: String,
    pub apps: Vec<CatalogApp>,
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").trim().to_string()
}

fn folder(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let p = crate::config::expand_tilde(raw);
    let s = p.to_string_lossy();
    let cut = s.trim_end_matches(['/', '\\']);
    if cut.is_empty() {
        s.into_owned()
    } else {
        cut.to_string()
    }
}

pub fn folder_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| path.to_string())
}

pub fn kind_of(raw: &str) -> String {
    let k = raw.trim().to_lowercase();
    if KINDS.contains(&k.as_str()) {
        k
    } else {
        "other".into()
    }
}

pub fn parse(raw: &str) -> Catalog {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return Catalog::default();
    };
    let mut out = Catalog {
        generated_at: text(&v, "generated_at"),
        apps: vec![],
    };
    for a in v["apps"].as_array().into_iter().flatten() {
        if !a.is_object() {
            continue;
        }
        let mut path = folder(&text(a, "path"));
        let project = folder(&text(a, "project"));
        if path.is_empty() {
            path = project.clone();
        } else if Path::new(&path).is_relative() && !project.is_empty() {
            path = Path::new(&project)
                .join(&path)
                .to_string_lossy()
                .into_owned();
        }
        let mut name = text(a, "name");
        if name.is_empty() {
            name = folder_name(&path);
        }
        if name.is_empty() {
            continue;
        }
        out.apps.push(CatalogApp {
            name,
            kind: kind_of(&text(a, "kind")),
            description: text(a, "description"),
            project: if project.is_empty() {
                path.clone()
            } else {
                project
            },
            path,
            tech: text(a, "tech"),
            run: text(a, "run"),
            tags: a["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
        });
    }
    out
}

pub fn load(path: &Path) -> Catalog {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sess {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub modified: Option<SystemTime>,
    pub harness: String,
    pub source: String,
    pub gone: bool,
}

impl Sess {
    pub fn of(r: &crate::session_index::Row) -> Sess {
        let i = &r.info;
        let title = i
            .name
            .as_deref()
            .or(i.title.as_deref())
            .map(|t| t.split(" ($").next().unwrap_or(t).to_string())
            .or_else(|| {
                i.first_prompt
                    .as_deref()
                    .map(|p| crate::sessions::snippet(p, 100))
            })
            .unwrap_or_else(|| i.id.clone());
        Sess {
            id: i.id.clone(),
            title,
            cwd: i.cwd.clone(),
            modified: i.modified,
            harness: r.harness.name().to_string(),
            source: r.source.clone(),
            gone: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub app: CatalogApp,
    pub described: bool,
    pub missing: bool,
    pub project_missing: bool,
    pub sessions: Vec<Sess>,
}

impl Entry {
    pub fn last(&self) -> Option<SystemTime> {
        self.sessions.first().and_then(|s| s.modified)
    }
}

pub fn project_root(cwd: &str) -> String {
    let flat = cwd.replace('\\', "/");
    match flat.find(WORKTREES) {
        Some(i) if i > 0 => cwd[..i].to_string(),
        _ => folder(cwd),
    }
}

fn inside(cwd: &str, root: &str) -> bool {
    !root.is_empty() && Path::new(cwd).starts_with(Path::new(root))
}

fn newest_first(v: &mut [Sess]) {
    v.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
}

pub fn merge(catalog: &Catalog, sessions: &[Sess], exists: &dyn Fn(&Path) -> bool) -> Vec<Entry> {
    let mut seen: HashMap<String, bool> = HashMap::new();
    let mut on_disk = |p: &str| -> bool {
        if let Some(&known) = seen.get(p) {
            return known;
        }
        let there = exists(Path::new(p));
        seen.insert(p.to_string(), there);
        there
    };
    let sessions: Vec<Sess> = sessions
        .iter()
        .map(|s| Sess {
            gone: !s.cwd.trim().is_empty() && !on_disk(&s.cwd),
            ..s.clone()
        })
        .collect();
    let sessions = &sessions[..];
    let mut out: Vec<Entry> = vec![];
    let mut claimed = vec![false; sessions.len()];
    for a in &catalog.apps {
        let mut mine = vec![];
        for (i, s) in sessions.iter().enumerate() {
            let root = project_root(&s.cwd);
            let hit = [&a.path, &a.project]
                .iter()
                .any(|r| inside(&s.cwd, r) || inside(&root, r));
            if hit {
                claimed[i] = true;
                mine.push(s.clone());
            }
        }
        newest_first(&mut mine);
        out.push(Entry {
            missing: !a.path.is_empty() && !on_disk(&a.path),
            project_missing: !a.project.is_empty() && !on_disk(&a.project),
            app: a.clone(),
            described: true,
            sessions: mine,
        });
    }
    let mut loose: HashMap<String, Vec<Sess>> = HashMap::new();
    for (i, s) in sessions.iter().enumerate() {
        if claimed[i] || s.cwd.trim().is_empty() {
            continue;
        }
        loose
            .entry(project_root(&s.cwd))
            .or_default()
            .push(s.clone());
    }
    let mut rest: Vec<Entry> = loose
        .into_iter()
        .map(|(root, mut ss)| {
            newest_first(&mut ss);
            let gone = !on_disk(&root);
            Entry {
                missing: gone,
                project_missing: gone,
                app: CatalogApp {
                    name: folder_name(&root),
                    path: root.clone(),
                    project: root,
                    ..Default::default()
                },
                described: false,
                sessions: ss,
            }
        })
        .collect();
    rest.sort_by(|a, b| {
        b.last()
            .cmp(&a.last())
            .then_with(|| a.app.path.cmp(&b.app.path))
    });
    out.extend(rest);
    out
}

pub fn to_survey(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| !e.described && !e.missing)
        .map(|e| e.app.path.clone())
        .collect()
}

fn millis(t: Option<SystemTime>) -> Option<u64> {
    t.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

pub fn entry_json(e: &Entry) -> Value {
    let a = &e.app;
    let sessions: Vec<Value> = e
        .sessions
        .iter()
        .take(SHOWN_SESSIONS)
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "when": crate::session_index::rel(s.modified),
                "when_ms": millis(s.modified),
                "harness": s.harness,
                "source": s.source,
                "folder": crate::config::tilde(Path::new(&s.cwd)),
                "cwd": s.cwd,
                "gone": s.gone,
            })
        })
        .collect();
    json!({
        "name": a.name,
        "kind": if e.described { a.kind.as_str() } else { "undescribed" },
        "description": a.description,
        "path": a.path,
        "shown_path": crate::config::tilde(Path::new(&a.path)),
        "project": a.project,
        "project_name": folder_name(&a.project),
        "tech": a.tech,
        "run": a.run,
        "tags": a.tags,
        "described": e.described,
        "missing": e.missing,
        "project_missing": e.project_missing,
        "last": e.last().map(|t| crate::session_index::rel(Some(t))),
        "last_ms": millis(e.last()),
        "conversations": e.sessions.len(),
        "sessions": sessions,
    })
}

fn rules(catalog: &Path) -> String {
    format!(
        "Read only README files and core source files. Do not open config or json files (the catalog file itself is the one exception), nothing that may hold secrets (.env files, keys, tokens, credentials) and nothing under /Volumes/Private. \
Then update the catalog file {} and keep every existing entry that you did not look at exactly as it is; create the file when it is missing. \
The file is valid JSON with this shape: {SCHEMA} \
kind is one of {}. path is the folder of the app and can be a subfolder of project; several apps can share one project. description is one or two plain sentences about what it is and does. Set generated_at to the current time. \
When you are done, tell me in a few lines which entries you added or changed.",
        catalog.display(),
        KINDS.join(", ")
    )
}

pub fn describe_prompt(catalog: &Path, folder: &str) -> String {
    format!(
        "Describe what is built in {folder} for my personal app catalog: every app, game, utility, dashboard, page, tool or library in it, one entry each. {}",
        rules(catalog)
    )
}

pub fn refresh_prompt(catalog: &Path, folders: &[String]) -> String {
    let mut p = String::from(
        "Refresh my personal app catalog: the list of every app, game, utility, dashboard, page, tool or library built on this machine. Check that the entries already in the catalog still match their folders (leave an entry in place when its folder is gone)",
    );
    if folders.is_empty() {
        p.push_str(". ");
    } else {
        p.push_str(", and add entries for these project folders that are not described yet: ");
        p.push_str(
            &folders
                .iter()
                .take(LISTED_FOLDERS)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        );
        if folders.len() > LISTED_FOLDERS {
            p.push_str(&format!(
                " (and {} more, ask me before going on to those)",
                folders.len() - LISTED_FOLDERS
            ));
        }
        p.push_str(". ");
    }
    p.push_str(&rules(catalog));
    p
}

pub fn rebuild_prompt(folder: &str, transcript: Option<&Path>) -> String {
    let history = match transcript {
        Some(t) => format!(
            "The earlier conversation is not loaded in this tab: read it first from its transcript file {}. ",
            t.display()
        ),
        None => "Work from what this conversation's history shows. ".to_string(),
    };
    format!(
        "The project that used to be in {folder} is gone from disk: the folder was deleted and is now empty or missing. Help me restore it. {history}\
First look in the history for a git remote of this project (a clone URL, the output of git remote, a push, a pull request link). If you find one, show it to me and offer to clone it into {folder}; do not clone before I say yes. \
If there is no remote, or I say no, recreate the files in {folder} from the history: the contents that were written, edited or shown there, in their latest version. \
Do not invent contents that the history does not show, do not restore secrets (keys, tokens, values of .env files) and touch nothing under /Volumes/Private. \
When you are done, say plainly what was restored and list what could not be recovered."
    )
}

#[derive(Debug, Clone, PartialEq)]
pub enum Restore {
    Present(PathBuf),
    Ask(PathBuf),
    Created(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Prefill {
    pub uid: u64,
    pub text: String,
    pub at: Instant,
}

impl App {
    pub fn app_entries(&mut self) -> (Catalog, Vec<Entry>, bool) {
        self.kick_index(false);
        let mut catalog = load(&catalog_path());
        let vault = |p: &str| crate::guard::in_vault(Path::new(p));
        catalog
            .apps
            .retain(|a| !vault(&a.path) && !vault(&a.project));
        let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
        let sessions: Vec<Sess> = ix
            .rows
            .iter()
            .filter(|r| r.info.parent.is_none() && !crate::sess_sort::headless(&r.info))
            .filter(|r| !vault(&r.info.cwd))
            .map(Sess::of)
            .collect();
        let indexing = ix.building && ix.built.is_none();
        drop(ix);
        let entries = merge(&catalog, &sessions, &|p| p.exists());
        (catalog, entries, indexing)
    }

    pub fn app_listing(&mut self) -> Value {
        let (catalog, entries, indexing) = self.app_entries();
        let base = crate::config::expand_tilde(&self.cfg.new_tab_base);
        json!({
            "catalog": crate::config::tilde(&catalog_path()),
            "generated_at": catalog.generated_at,
            "indexing": indexing,
            "workspace": crate::config::tilde(&base),
            "kinds": KINDS,
            "apps": entries.iter().map(entry_json).collect::<Vec<_>>(),
        })
    }

    pub fn app_session_row(&self, id: &str) -> Option<crate::session_index::Row> {
        if id.trim().is_empty() {
            return None;
        }
        let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
        ix.rows
            .iter()
            .filter(|r| r.info.id == id && r.info.parent.is_none())
            .max_by_key(|r| r.info.modified)
            .cloned()
    }

    pub fn app_restore_folder(&mut self, id: &str, confirmed: bool) -> Result<Restore, String> {
        let row = self
            .app_session_row(id)
            .ok_or("that conversation is not in the index")?;
        let cwd = row.info.cwd.trim();
        if cwd.is_empty() || !Path::new(cwd).is_absolute() {
            return Err("that conversation has no folder to bring back".into());
        }
        let p = PathBuf::from(cwd);
        crate::guard::resolve_outside_vault(&p)?;
        if p.is_dir() {
            return Ok(Restore::Present(p));
        }
        if !confirmed {
            return Ok(Restore::Ask(p));
        }
        let made = crate::control::new_folder(&p, true, &self.workspaces())?;
        crate::log::info(&format!(
            "apps: recreated the empty folder {} for conversation {id}, confirmed in the web app",
            made.display()
        ));
        Ok(Restore::Created(p))
    }

    pub fn app_prefill(&mut self, v: &Value) -> Result<Value, String> {
        let (s, t) = self.one_tab(&json!({ "tab": v["tab"] }))?;
        let uid = self.panes[s].tabs[t].uid;
        let catalog = catalog_path();
        let text = match v["what"].as_str() {
            Some("describe") => {
                let path = v["path"].as_str().map(str::trim).unwrap_or("");
                if path.is_empty() {
                    return Err("path is required".into());
                }
                describe_prompt(&catalog, &folder(path))
            }
            Some("rebuild") => {
                let path = v["path"].as_str().map(str::trim).unwrap_or("");
                if path.is_empty() {
                    return Err("path is required".into());
                }
                let transcript = if v["resumed"].as_bool() == Some(true) {
                    None
                } else {
                    let id = v["session"].as_str().unwrap_or("");
                    Some(
                        self.app_session_row(id)
                            .map(|r| r.info.path)
                            .ok_or("that conversation is not in the index")?,
                    )
                };
                rebuild_prompt(&folder(path), transcript.as_deref())
            }
            Some("refresh") => {
                let (_, entries, _) = self.app_entries();
                refresh_prompt(&catalog, &to_survey(&entries))
            }
            _ => return Err("unknown prompt".into()),
        };
        self.app_prefills.retain(|p| p.uid != uid);
        self.app_prefills.push(Prefill {
            uid,
            text,
            at: Instant::now(),
        });
        self.app_prefill_tick();
        Ok(json!({"tab": crate::control::tab_id(uid)}))
    }

    pub fn app_prefill_tick(&mut self) {
        if self.app_prefills.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.app_prefills);
        for mut p in waiting {
            p.uid = self.current_uid(p.uid);
            let Some((s, t)) = self.find_tab(p.uid) else {
                continue;
            };
            let tab = &self.panes[s].tabs[t];
            let ready = tab.activity == Activity::Ready
                && tab.is_running()
                && !crate::pane::typed_draft(
                    tab.parser
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .screen(),
                );
            if ready {
                let tab = &mut self.panes[s].tabs[t];
                tab.reset_scroll();
                tab.write(&crate::keys::encode_paste(&p.text, true));
                crate::log::info(&format!(
                    "apps: prompt placed in {} ({} chars), not sent",
                    crate::control::tab_id(p.uid),
                    p.text.len()
                ));
            } else if p.at.elapsed() < PREFILL_WAIT {
                self.app_prefills.push(p);
            } else {
                crate::log::info(&format!(
                    "apps: {} never became ready, prompt dropped",
                    crate::control::tab_id(p.uid)
                ));
            }
        }
    }

    pub fn web_apps(&mut self, client: u64, v: Value) {
        let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) else {
            return;
        };
        let out = Arc::clone(&c.out);
        let reply = |ok: bool, mut body: Value| {
            body["t"] = json!("apps");
            body["id"] = v["id"].clone();
            body["op"] = v["op"].clone();
            body["ok"] = json!(ok);
            out.text(body.to_string());
        };
        match v["op"].as_str() {
            Some("list") => reply(true, self.app_listing()),
            Some("restore") => {
                let id = v["session"].as_str().unwrap_or("");
                let shown = |p: &Path| crate::config::tilde(p);
                match self.app_restore_folder(id, v["confirm"].as_bool() == Some(true)) {
                    Ok(Restore::Present(p)) => {
                        reply(true, json!({"folder": shown(&p), "created": false}))
                    }
                    Ok(Restore::Created(p)) => {
                        reply(true, json!({"folder": shown(&p), "created": true}))
                    }
                    Ok(Restore::Ask(p)) => reply(
                        false,
                        json!({"needs_confirmation": true, "folder": shown(&p),
                               "error": format!("{} is gone: confirm to recreate it as an empty folder", shown(&p))}),
                    ),
                    Err(e) => reply(false, json!({ "error": e })),
                }
            }
            Some("prefill") => match self.app_prefill(&v) {
                Ok(b) => reply(true, b),
                Err(e) => reply(false, json!({ "error": e })),
            },
            _ => reply(false, json!({"error": "unknown request"})),
        }
    }
}
