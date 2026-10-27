use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::merge::{self, Base, Pick};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Plain,

    Env,
}

#[derive(Debug, Clone)]
pub struct Area {
    pub name: String,

    pub root: Option<PathBuf>,

    pub items: BTreeMap<String, PathBuf>,
    pub kind: Kind,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub to_home: Vec<String>,
    pub to_repo: Vec<String>,

    pub kept_both: Vec<(String, Vec<String>)>,
    pub errors: Vec<String>,

    pub base: Base,
}

fn index_path(wc: &Path, area: &str) -> PathBuf {
    wc.join(area).join(".index.json")
}

pub fn sync_area(
    a: &Area,
    wc: &Path,
    base: &Base,
    now: i64,
    machine: &str,
    never: &[PathBuf],
) -> Outcome {
    let mut out = Outcome::default();
    let mut local: BTreeMap<String, Value> = BTreeMap::new();
    let mut bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (k, p) in &a.items {
        if let Ok(b) = std::fs::read(p) {
            local.insert(k.clone(), json!({"sha": super::sha_hex(&b)}));
            bytes.insert(k.clone(), b);
        }
    }
    let remote = merge::load_doc(&index_path(wc, &a.name));

    let by_path = wc.join(&a.name).join(".by.json");
    let mut by: BTreeMap<String, String> = std::fs::read(&by_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let writer = |k: &str, by: &BTreeMap<String, String>| {
        by.get(k)
            .cloned()
            .unwrap_or_else(|| "another-machine".into())
    };
    let m = merge::merge(&merge::stamp(&local, base, now), &remote, base, now);
    let mut unplaced: Vec<String> = vec![];
    let repo_file = |k: &str| wc.join(&a.name).join(k);
    for (k, pick) in &m.picks {
        let e = &m.doc[k];
        let res: Result<()> = (|| {
            match pick {
                Pick::Same => {
                    if !e.deleted && !repo_file(k).is_file() {
                        if let Some(b) = bytes.get(k) {
                            super::write_file(&repo_file(k), b)?;
                        }
                    }
                }
                Pick::Local | Pick::Conflict { local_won: true } => {
                    if e.deleted {
                        let _ = std::fs::remove_file(repo_file(k));
                    } else if let Some(b) = bytes.get(k) {
                        super::write_file(&repo_file(k), b)?;
                        by.insert(k.clone(), machine.to_string());
                    }
                    out.to_repo.push(k.clone());
                }
                Pick::Remote | Pick::Conflict { local_won: false } => {
                    let Some(root) = &a.root else {
                        anyhow::bail!("not placed: no folder for it here");
                    };
                    let dst = root.join(k);
                    if e.deleted {
                        if a.kind == Kind::Plain {
                            let _ = std::fs::remove_file(&dst);
                            out.to_home.push(k.clone());
                        }
                        return Ok(());
                    }
                    let Ok(rb) = std::fs::read(repo_file(k)) else {
                        anyhow::bail!("{}/{k} is missing in the state repo", a.name);
                    };
                    if super::sha_hex(&rb) != e.value["sha"].as_str().unwrap_or("") {
                        anyhow::bail!("{}/{k} does not match its hash", a.name);
                    }
                    match a.kind {
                        Kind::Plain => {
                            if let Some(why) = super::paths::refused(&dst, &[]) {
                                anyhow::bail!("{why}");
                            }
                            super::write_file(&dst, &rb)?;
                        }
                        Kind::Env => {
                            let both = matches!(pick, Pick::Conflict { .. })
                                && bytes.get(k).is_some_and(|lb| *lb != rb);
                            if both {
                                let lb = &bytes[k];
                                let p2 = super::env::keep_both_path(&dst, &writer(k, &by));
                                super::env::write_env(&p2, &rb, never)?;
                                out.kept_both.push((
                                    format!("{}/{k}", a.name),
                                    super::env::changed_keys(lb, &rb),
                                ));

                                super::write_file(&repo_file(k), lb)?;
                                by.insert(k.clone(), machine.to_string());
                                return Ok(());
                            }
                            super::env::write_env(&dst, &rb, never)?;
                        }
                    }
                    out.to_home.push(k.clone());
                }
            }
            Ok(())
        })();
        if let Err(e) = res {
            unplaced.push(k.clone());
            if a.root.is_some() {
                out.errors.push(format!("{}/{k}: {e:#}", a.name));
            }
        }
    }

    let mut doc = m.doc.clone();
    for (f, _) in &out.kept_both {
        let k = f.strip_prefix(&format!("{}/", a.name)).unwrap_or(f);
        if let (Some(e), Some(v)) = (doc.get_mut(k), local.get(k)) {
            e.value = v.clone();
            e.updated = e.updated.max(now);
        }
    }
    let _ = super::write_file(&index_path(wc, &a.name), &merge::doc_bytes(&doc));
    by.retain(|k, _| doc.get(k).is_some_and(|e| !e.deleted));
    if !by.is_empty() {
        let _ = super::write_file(
            &by_path,
            &serde_json::to_vec_pretty(&by).unwrap_or_default(),
        );
    }
    out.base = merge::Merged {
        doc,
        picks: m.picks,
    }
    .base();
    for k in unplaced {
        out.base.remove(&k);
    }
    out
}

pub fn repo_areas(wc: &Path, prefix: &str) -> Vec<String> {
    std::fs::read_dir(wc.join(prefix))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join(".index.json").is_file())
        .map(|e| format!("{prefix}/{}", e.file_name().to_string_lossy()))
        .collect()
}
