use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const STALE_DAYS: i64 = 14;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Memo {
    pub kind: String,

    pub key: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub subprojects: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,

    #[serde(default)]
    pub last_deep_search: Option<String>,

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

pub fn folder_key(p: &Path) -> String {
    let c = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    plain(&c.to_string_lossy(), cfg!(windows))
}

fn plain(p: &str, windows: bool) -> String {
    if !windows {
        return p.to_string();
    }
    let s = match p.strip_prefix(r"\\?\UNC\") {
        Some(r) => format!(r"\\{r}"),
        None => p.strip_prefix(r"\\?\").unwrap_or(p).to_string(),
    };
    s.replace('/', "\\")
}

fn same_folder(key: &str, folder: &str) -> bool {
    crate::guard::same(Path::new(key), Path::new(folder))
}

impl Memo {
    fn is(&self, kind: &str, key: &str) -> bool {
        self.kind == kind
            && if kind == "project" {
                same_folder(&self.key, key)
            } else {
                self.key == key
            }
    }
}

impl Store {
    pub fn get(&self, kind: &str, key: &str) -> Option<&Memo> {
        self.items.iter().find(|m| m.is(kind, key))
    }

    pub fn deep_searched_on(&self, folder: &str) -> Option<&String> {
        self.deep_searched
            .iter()
            .find(|(k, _)| same_folder(k, folder))
            .map(|(_, d)| d)
    }

    pub fn entry(&mut self, kind: &str, key: &str) -> &mut Memo {
        let i = match self.items.iter().position(|m| m.is(kind, key)) {
            Some(i) => i,
            None => {
                self.items.push(Memo {
                    kind: kind.into(),
                    key: key.into(),
                    last_deep_search: (kind == "project")
                        .then(|| self.deep_searched_on(key).cloned())
                        .flatten(),
                    ..Default::default()
                });
                self.items.len() - 1
            }
        };
        &mut self.items[i]
    }

    pub fn note_deep_search(&mut self, root: &Path) {
        let key = folder_key(root);
        let day = today();
        self.deep_searched.retain(|k, _| !same_folder(k, &key));
        self.deep_searched.insert(key.clone(), day.clone());
        let root = Path::new(&key);
        for m in self.items.iter_mut().filter(|m| m.kind == "project") {
            let p = Path::new(&m.key);
            if crate::guard::same(p, root) || crate::guard::inside(p, root) {
                m.last_deep_search = Some(day.clone());
            }
        }
    }
}

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
