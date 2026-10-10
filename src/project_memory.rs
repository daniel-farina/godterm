//! What the assistant saved about a project, subproject or session (a
//! short description, the names the user calls it, key subprojects,
//! tags), so a later `find` is instant instead of another deep search.
//! Kept in GodTerm's home as projects.json. An entry goes stale after
//! `STALE_DAYS`, or when the folder it describes changed a lot since.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Days after which a saved description is worth checking again.
pub const STALE_DAYS: i64 = 14;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Memo {
    /// "project" (a root or subproject folder) or "session".
    pub kind: String,
    /// The folder's absolute path, or the session id.
    pub key: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub subprojects: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The day a deep search last looked at it (YYYY-MM-DD).
    #[serde(default)]
    pub last_deep_search: Option<String>,
    /// The project index's fingerprint of the folder when saved: the
    /// newest mtime it read and how many folders it mapped.
    #[serde(default)]
    pub index_mtime: Option<u128>,
    #[serde(default)]
    pub index_entries: Option<usize>,
    #[serde(default)]
    pub updated: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub items: Vec<Memo>,
    /// Every folder a deep search looked in, with the day: kept even
    /// when nothing was saved about it yet.
    #[serde(default)]
    pub deep_searched: BTreeMap<String, String>,
}

pub fn path() -> PathBuf {
    crate::config::app_home().join("projects.json")
}

pub fn load() -> Store {
    std::fs::read(path())
        .ok()
        .and_then(|t| serde_json::from_slice(&t).ok())
        .unwrap_or_default()
}

pub fn save(s: &Store) -> Result<(), String> {
    let mut s = s.clone();
    s.version = 1;
    let t = serde_json::to_vec_pretty(&s).map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(crate::config::app_home());
    let tmp = path().with_extension("json.tmp");
    crate::config::write_private(&tmp, t).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path()).map_err(|e| e.to_string())
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

impl Store {
    pub fn get(&self, kind: &str, key: &str) -> Option<&Memo> {
        self.items.iter().find(|m| m.kind == kind && m.key == key)
    }

    /// The entry for (kind, key), made when missing.
    pub fn entry(&mut self, kind: &str, key: &str) -> &mut Memo {
        let i = match self
            .items
            .iter()
            .position(|m| m.kind == kind && m.key == key)
        {
            Some(i) => i,
            None => {
                self.items.push(Memo {
                    kind: kind.into(),
                    key: key.into(),
                    last_deep_search: (kind == "project")
                        .then(|| self.deep_searched.get(key).cloned())
                        .flatten(),
                    ..Default::default()
                });
                self.items.len() - 1
            }
        };
        &mut self.items[i]
    }

    /// A deep search looked in `root` today: noted for it and for every
    /// saved project inside it.
    pub fn note_deep_search(&mut self, root: &Path) {
        let key = root.to_string_lossy().into_owned();
        let day = today();
        self.deep_searched.insert(key.clone(), day.clone());
        for m in self.items.iter_mut().filter(|m| m.kind == "project") {
            if Path::new(&m.key).starts_with(root) {
                m.last_deep_search = Some(day.clone());
            }
        }
    }
}

/// The index's fingerprint of `folder`: the newest mtime its map read and
/// how many folders of the map lie inside it.
pub fn fingerprint(ix: &crate::project_index::Index, folder: &Path) -> Option<(u128, usize)> {
    let m = ix
        .roots
        .values()
        .filter(|m| folder.starts_with(&m.root))
        .max_by_key(|m| m.root.as_os_str().len())?;
    let rel = folder
        .strip_prefix(&m.root)
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = m
        .entries
        .iter()
        .filter(|e| rel.is_empty() || e.rel == rel || e.rel.starts_with(&format!("{rel}/")))
        .count();
    let t = m
        .watched
        .iter()
        .filter(|(p, _, _)| p.starts_with(folder))
        .map(|(_, t, _)| *t)
        .max()
        .unwrap_or(0);
    Some((t, n))
}

/// Why a saved entry should be checked again, if it should.
pub fn stale(
    m: &Memo,
    ix: &crate::project_index::Index,
    today: chrono::NaiveDate,
) -> Option<String> {
    let day = m
        .last_deep_search
        .as_deref()
        .or(Some(m.updated.get(..10).unwrap_or("")))
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    if let Some(d) = day {
        let age = (today - d).num_days();
        if age > STALE_DAYS {
            return Some(format!("saved {age} days ago"));
        }
    }
    if m.kind == "project" {
        if let (Some(n0), Some((_, n))) = (m.index_entries, fingerprint(ix, Path::new(&m.key))) {
            let d = n.abs_diff(n0);
            if d >= 10 || d * 4 > n0.max(4) {
                return Some(format!(
                    "the folder changed a lot since ({n0} folders then, {n} now)"
                ));
            }
        }
    }
    None
}

/// How well a saved entry answers the query (aliases and the
/// description first): (score, what matched).
pub fn score(m: &Memo, query: &str) -> Option<(f64, &'static str)> {
    use crate::project_index::{query_tokens, score_fields, Field};
    let q = query_tokens(query);
    let base = Path::new(&m.key)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut f: Vec<Field> = vec![];
    for a in &m.aliases {
        f.push(Field {
            text: a,
            tier: 1.0,
            context: false,
            why: "alias",
        });
    }
    f.push(Field {
        text: &m.description,
        tier: 0.97,
        context: false,
        why: "description",
    });
    if m.kind == "project" {
        f.push(Field {
            text: &base,
            tier: 1.0,
            context: false,
            why: "name",
        });
    }
    for s in &m.subprojects {
        f.push(Field {
            text: s,
            tier: 0.95,
            context: false,
            why: "subproject",
        });
    }
    for t in &m.tags {
        f.push(Field {
            text: t,
            tier: 0.9,
            context: false,
            why: "tag",
        });
    }
    let (sc, on, cov) = score_fields(&q, &f)?;
    (cov >= 0.5 && sc >= 0.5).then_some((sc, on))
}

/// The JSON a hit or a session row carries for a saved entry.
pub fn json(m: &Memo, stale: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "description": m.description,
        "aliases": m.aliases,
        "subprojects": m.subprojects,
        "tags": m.tags,
        "last_deep_search": m.last_deep_search,
        "updated": m.updated,
    });
    if let Some(s) = stale {
        v["stale"] = serde_json::json!(true);
        v["stale_reason"] = serde_json::json!(s);
    }
    v
}
