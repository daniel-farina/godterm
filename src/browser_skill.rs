use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const BSK_RULE: &str = "Bash(bsk:*)";

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Install,

    Update,

    Current,

    KeptNewer,
}

impl Action {
    pub fn word(&self) -> &'static str {
        match self {
            Action::Install => "install",
            Action::Update => "update",
            Action::Current => "already current",
            Action::KeptNewer => "kept (newer copy)",
        }
    }
}

fn tree(dir: &Path) -> io::Result<BTreeMap<String, [u8; 32]>> {
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, [u8; 32]>) -> io::Result<()> {
        for e in fs::read_dir(d)? {
            let e = e?;
            let ft = e.file_type()?;
            let p = e.path();
            if ft.is_dir() {
                walk(base, &p, out)?;
            } else if ft.is_file() {
                let rel = p
                    .strip_prefix(base)
                    .map_err(io::Error::other)?
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.insert(rel, Sha256::digest(fs::read(&p)?).into());
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

fn newest(dir: &Path) -> Option<SystemTime> {
    let mut best: Option<SystemTime> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).ok()?.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                    best = Some(best.map_or(m, |b| b.max(m)));
                }
            }
        }
    }
    best
}

pub fn source_ok(src: &Path) -> bool {
    src.join("SKILL.md").is_file()
}

pub fn plan(src: &Path, dest: &Path, force: bool) -> Result<Action, String> {
    if !source_ok(src) {
        return Err(format!("no skill at {} (no SKILL.md)", src.display()));
    }
    if !dest.join("SKILL.md").is_file() {
        return Ok(Action::Install);
    }
    let (a, b) = (
        tree(src).map_err(|e| format!("cannot read the source skill: {e}"))?,
        tree(dest).map_err(|e| format!("cannot read {}: {e}", dest.display()))?,
    );
    if a == b {
        return Ok(Action::Current);
    }
    if !force {
        if let (Some(s), Some(d)) = (newest(src), newest(dest)) {
            if d > s {
                return Ok(Action::KeptNewer);
            }
        }
    }
    Ok(Action::Update)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let ft = e.file_type()?;
        let t = to.join(e.file_name());
        if ft.is_dir() {
            copy_tree(&e.path(), &t)?;
        } else if ft.is_file() {
            fs::copy(e.path(), &t)?;
        }
    }
    Ok(())
}

pub fn install(src: &Path, dest: &Path) -> Result<(), String> {
    if !source_ok(src) {
        return Err(format!("no skill at {} (no SKILL.md)", src.display()));
    }
    let parent = dest.parent().ok_or("bad destination")?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let stem = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pid = std::process::id();
    let tmp = parent.join(format!(".{stem}.tmp-{pid}"));
    let old = parent.join(format!(".{stem}.old-{pid}"));
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::remove_dir_all(&old);
    if let Err(e) = copy_tree(src, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("copy failed: {e}"));
    }
    let had = dest.exists();
    if had {
        if let Err(e) = fs::rename(dest, &old) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(format!("cannot replace the old copy: {e}"));
        }
    }
    if let Err(e) = fs::rename(&tmp, dest) {
        if had {
            let _ = fs::rename(&old, dest);
        }
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("cannot put the new copy in place: {e}"));
    }
    if had {
        let _ = fs::remove_dir_all(&old);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Merge {
    Added { backup: Option<PathBuf> },
    AlreadyAllowed,
}

pub fn allow_rule(path: &Path, rule: &str) -> Result<Merge, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut doc: Value = match &text {
        Some(t) if t.trim().is_empty() => json!({}),
        Some(t) => serde_json::from_str(t)
            .map_err(|e| format!("{} is not valid JSON ({e}); left as it is", path.display()))?,
        None => json!({}),
    };
    let Some(root) = doc.as_object_mut() else {
        return Err(format!(
            "{} is not a JSON object; left as it is",
            path.display()
        ));
    };
    let perms = root.entry("permissions").or_insert_with(|| json!({}));
    let Some(perms) = perms.as_object_mut() else {
        return Err(format!(
            "permissions in {} is not an object; left as it is",
            path.display()
        ));
    };
    let allow = perms.entry("allow").or_insert_with(|| json!([]));
    let Some(allow) = allow.as_array_mut() else {
        return Err(format!(
            "permissions.allow in {} is not a list; left as it is",
            path.display()
        ));
    };
    if allow.iter().any(|v| v.as_str() == Some(rule)) {
        return Ok(Merge::AlreadyAllowed);
    }
    allow.push(json!(rule));
    let out = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
    let dir = path.parent().ok_or("bad settings path")?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let backup = if text.is_some() {
        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut b = dir.join(format!("{name}.godterm-bak-{secs}"));
        let mut n = 1;
        while b.exists() {
            b = dir.join(format!("{name}.godterm-bak-{secs}-{n}"));
            n += 1;
        }
        fs::copy(path, &b).map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
        Some(b)
    } else {
        None
    };
    let tmp = dir.join(format!(".settings.json.tmp-{}", std::process::id()));
    fs::write(&tmp, out).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    if let Ok(m) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, m.permissions());
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(Merge::Added { backup })
}
