//! Project .env files, as is (docs/SYNC.md 2.7). Every `.env` and
//! `.env.*` (not `.env.example` / `.env.sample`) at a known project root
//! and up to three folders down is captured, whether or not the project
//! has a repo; nothing under a never folder, no symlinks. Stored at
//! `env/<project-key>/<relpath>` in the state repo, never in the
//! project's own repo: a file git would not ignore gets a warning.
//! Restored with mode 0600 through a temp file (O_NOFOLLOW) and a rename;
//! a local file that differs is kept, the other machine's copy written
//! next to it as `.env.from-<machine>`, and the changed key names (never
//! values) are reported.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::Ctx;

/// Folders never walked into.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    "venv",
];

pub fn is_env_name(n: &str) -> bool {
    (n == ".env" || n.starts_with(".env."))
        && !matches!(n, ".env.example" | ".env.sample" | ".env.template")
        && !n.contains(".from-")
        && !n.ends_with(".sync-tmp")
}

/// Known project roots: tab folders (open, closed, archived), the
/// projects the assistant described, and bound repos.
pub fn project_roots(ctx: &Ctx) -> Vec<PathBuf> {
    let mut v: BTreeSet<PathBuf> = BTreeSet::new();
    let read = |f: &str| -> serde_json::Value {
        std::fs::read(ctx.app_home.join(f))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    };
    let st = read("state.json");
    for s in st["slots"].as_array().into_iter().flatten() {
        for t in s["tabs"].as_array().into_iter().flatten() {
            if let Some(c) = t["cwd"].as_str() {
                v.insert(PathBuf::from(c));
            }
        }
    }
    for c in st["closed_tabs"].as_array().into_iter().flatten() {
        if let Some(c) = c["cwd"].as_str() {
            v.insert(PathBuf::from(c));
        }
    }
    for e in read("archive.json")["entries"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(c) = e["cwd"].as_str() {
            v.insert(PathBuf::from(c));
        }
    }
    for m in read("projects.json")["items"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if m["kind"] == "project" {
            if let Some(c) = m["key"].as_str() {
                v.insert(PathBuf::from(c));
            }
        }
    }
    if let Some(o) = read("git-bindings.json")["projects"].as_object() {
        for k in o.keys() {
            v.insert(ctx.expand(k));
        }
    }
    v.into_iter()
        .filter(|p| p.is_absolute() && p.is_dir() && *p != ctx.home)
        .filter(|p| ctx.refused(p).is_none() && !super::paths::is_temp(p))
        .collect()
}

/// The .env files of one project root (path relative to it, path).
pub fn env_files(root: &Path, never: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut out = vec![];
    walk(root, root, 3, never, &mut out);
    out
}

fn walk(root: &Path, d: &Path, depth: u32, never: &[PathBuf], out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    let mut ents: Vec<_> = rd.flatten().collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let n = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if md.file_type().is_symlink() {
            continue;
        }
        if md.is_dir() {
            if depth > 0
                && !n.starts_with('.')
                && !SKIP_DIRS.contains(&n.as_str())
                && super::paths::refused(&p, never).is_none()
            {
                walk(root, &p, depth - 1, never, out);
            }
        } else if md.is_file() && is_env_name(&n) && md.len() <= 1 << 20 {
            if let Ok(r) = p.strip_prefix(root) {
                out.push((r.to_string_lossy().replace('\\', "/"), p));
            }
        }
    }
}

/// Whether git would leave a file out of the project's repo: Some(true)
/// ignored, Some(false) NOT ignored (warn), None no repo there.
pub fn ignored(root: &Path, rel: &str) -> Option<bool> {
    // Read only: check-ignore never writes.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--", rel])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CEILING_DIRECTORIES", root.parent().unwrap_or(root))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Write a restored .env: refuses a symlink (or a folder refused by the
/// never list) at the target, mode 0600, a temp file opened with
/// O_NOFOLLOW and O_EXCL, then renamed over.
pub fn write_env(p: &Path, data: &[u8], never: &[PathBuf]) -> Result<()> {
    use std::io::Write;
    if let Some(why) = super::paths::refused(p, never) {
        bail!("{why}");
    }
    if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("{} is a symlink: not replaced", p.display());
    }
    let dir = p.parent().ok_or_else(|| anyhow::anyhow!("no folder"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.sync-tmp",
        p.file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, p)?;
    Ok(())
}

/// KEY=value pairs of a .env text.
fn pairs(t: &str) -> BTreeMap<String, String> {
    t.lines()
        .filter_map(|l| {
            let l = l.trim();
            let l = l.strip_prefix("export ").unwrap_or(l);
            if l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// The keys that differ between two .env texts (names only).
pub fn changed_keys(a: &[u8], b: &[u8]) -> Vec<String> {
    let (x, y) = (
        pairs(&String::from_utf8_lossy(a)),
        pairs(&String::from_utf8_lossy(b)),
    );
    let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
    keys.into_iter()
        .filter(|k| x.get(*k) != y.get(*k))
        .cloned()
        .collect()
}

/// The name a remote copy gets next to a differing local file.
pub fn keep_both_path(p: &Path, machine: &str) -> PathBuf {
    let slug: String = machine
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let n = p
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    p.with_file_name(format!("{n}.from-{slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_restore_is_private_and_refuses_links() {
        let d = std::env::temp_dir().join(format!("gt-sync-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("proj/api/deep/er/est")).unwrap();
        for f in [
            ".env",
            ".env.local",
            ".env.example",
            "api/.env",
            "api/deep/er/est/.env",
        ] {
            std::fs::write(d.join("proj").join(f), "A=1\n").unwrap();
        }
        let found: Vec<String> = env_files(&d.join("proj"), &[])
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        assert_eq!(found, vec![".env", ".env.local", "api/.env"]);
        // Written 0600, through a rename.
        let p = d.join("proj/.env");
        write_env(&p, b"A=2\nB=3\n", &[]).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"A=2\nB=3\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
            // A symlink at the target is never followed or replaced.
            std::fs::write(d.join("elsewhere"), "keep").unwrap();
            std::os::unix::fs::symlink(d.join("elsewhere"), d.join("proj/.env.prod")).unwrap();
            assert!(write_env(&d.join("proj/.env.prod"), b"X=1", &[]).is_err());
            assert_eq!(
                std::fs::read_to_string(d.join("elsewhere")).unwrap(),
                "keep"
            );
            // A folder linked into the vault is refused.
            std::os::unix::fs::symlink("/Volumes/Private/p", d.join("vp")).unwrap();
            assert!(write_env(&d.join("vp/.env"), b"X=1", &[]).is_err());
        }
        assert_eq!(
            changed_keys(b"A=1\nB=2\n#C=1\n", b"A=1\nB=3\nD=4\n"),
            vec!["B", "D"]
        );
        assert_eq!(
            keep_both_path(&p, "Mac Book"),
            d.join("proj/.env.from-Mac-Book")
        );
        assert!(!is_env_name(".env.from-x") && is_env_name(".env.production"));
        // check-ignore: not a repo, then a repo that ignores .env only.
        assert_eq!(ignored(&d.join("proj"), ".env"), None);
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(d.join("proj"))
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            std::fs::write(d.join("proj/.gitignore"), ".env\n").unwrap();
            assert_eq!(ignored(&d.join("proj"), ".env"), Some(true));
            assert_eq!(ignored(&d.join("proj"), ".env.local"), Some(false));
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
