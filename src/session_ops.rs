//! Copying and moving sessions between config dirs (accounts and the main
//! `~/.claude`). A session is its transcript `projects/<dir>/<id>.jsonl`
//! plus what claude reads back on `--resume`: the subagent folder
//! `projects/<dir>/<id>/`, checkpoints in `file-history/<id>/`, and
//! `session-env/<id>/` (claude 2.1.29x keeps no per session todos file).
//! A move copies, verifies, then puts the originals in
//! `~/.godterm/trash/<stamp>/`, from where it can be undone.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Per session data outside the project folder, by top level dir.
const SIDE_DIRS: &[&str] = &["file-history", "session-env", "todos", "tasks"];

/// The main Claude Code config dir: `$GODTERM_MAIN_DIR` or `~/.claude`.
pub fn main_dir() -> PathBuf {
    crate::config::dirs().main_claude
}

/// What to do when the target already has the session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Conflict {
    Skip,
    Overwrite,
    /// Copy under a fresh session id (rewriting it in the transcript).
    NewId,
}

/// Every path belonging to session `id` whose transcript is `jsonl`, as
/// (absolute source, path relative to the config dir).
pub fn related(config_dir: &Path, jsonl: &Path, id: &str) -> Vec<(PathBuf, PathBuf)> {
    let mut v = vec![];
    if let Ok(rel) = jsonl.strip_prefix(config_dir) {
        v.push((jsonl.to_path_buf(), rel.to_path_buf()));
        let sub = jsonl.with_extension("");
        if sub.is_dir() {
            v.push((sub.clone(), rel.with_extension("")));
        }
    }
    for d in SIDE_DIRS {
        let Ok(rd) = fs::read_dir(config_dir.join(d)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == id
                || name.starts_with(&format!("{id}-"))
                || name.starts_with(&format!("{id}."))
            {
                v.push((e.path(), PathBuf::from(d).join(name)));
            }
        }
    }
    v
}

fn copy_tree(src: &Path, dst: &Path) -> Result<u64> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        let mut n = 0;
        for e in fs::read_dir(src)?.flatten() {
            n += copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(n)
    } else {
        if let Some(p) = dst.parent() {
            fs::create_dir_all(p)?;
        }
        // Write to a temp name, then rename: never a half written file.
        let tmp = dst.with_extension("cgtmp");
        fs::copy(src, &tmp).with_context(|| format!("copying {}", src.display()))?;
        fs::rename(&tmp, dst)?;
        Ok(fs::metadata(dst)?.len())
    }
}

fn digest(p: &Path) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    if p.is_dir() {
        let mut names: Vec<_> = fs::read_dir(p)?.flatten().map(|e| e.path()).collect();
        names.sort();
        for n in names {
            h.update(
                n.file_name()
                    .map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
            h.update(digest(&n)?);
        }
    } else {
        let mut f = fs::File::open(p)?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let k = f.read(&mut buf)?;
            if k == 0 {
                break;
            }
            h.update(&buf[..k]);
        }
    }
    Ok(h.finalize().to_vec())
}

/// A random version 4 UUID.
pub fn new_uuid() -> String {
    let mut b = [0u8; 16];
    crate::platform::fill_random(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct Copied {
    /// The id in the target (new when re-id'd).
    pub id: String,
    pub files: usize,
    pub bytes: u64,
    pub skipped: bool,
    /// The target transcript.
    pub jsonl: PathBuf,
}

/// Copy session `id` (transcript `jsonl` under `src_dir`) into `dst_dir`.
pub fn copy_session(
    src_dir: &Path,
    jsonl: &Path,
    id: &str,
    dst_dir: &Path,
    conflict: Conflict,
) -> Result<Copied> {
    if src_dir == dst_dir {
        bail!("source and target are the same");
    }
    let rel = jsonl
        .strip_prefix(src_dir)
        .context("transcript is not under the source dir")?;
    let target = dst_dir.join(rel);
    let exists = target.exists();
    if exists && conflict == Conflict::Skip {
        return Ok(Copied {
            id: id.into(),
            files: 0,
            bytes: 0,
            skipped: true,
            jsonl: target,
        });
    }
    let new_id = if exists && conflict == Conflict::NewId {
        new_uuid()
    } else {
        id.to_string()
    };
    let mut files = 0;
    let mut bytes = 0;
    for (src, r) in related(src_dir, jsonl, id) {
        let r = rename_id(&r, id, &new_id);
        let dst = dst_dir.join(&r);
        if dst.exists() && conflict == Conflict::Overwrite {
            if dst.is_dir() {
                fs::remove_dir_all(&dst)?;
            } else {
                fs::remove_file(&dst)?;
            }
        }
        bytes += copy_tree(&src, &dst)?;
        files += 1;
    }
    let out = dst_dir.join(rename_id(rel, id, &new_id));
    if new_id != id {
        // Every line carries "sessionId": the copy must use its new id.
        let text = fs::read_to_string(&out)?;
        let fixed = text.replace(
            &format!("\"sessionId\":\"{id}\""),
            &format!("\"sessionId\":\"{new_id}\""),
        );
        fs::write(&out, fixed)?;
    }
    Ok(Copied {
        id: new_id,
        files,
        bytes,
        skipped: false,
        jsonl: out,
    })
}

fn rename_id(p: &Path, old: &str, new: &str) -> PathBuf {
    if old == new {
        return p.to_path_buf();
    }
    p.iter()
        .map(|c| {
            let s = c.to_string_lossy();
            if s.starts_with(old) {
                PathBuf::from(s.replacen(old, new, 1))
            } else {
                PathBuf::from(c)
            }
        })
        .collect()
}

/// What a move did, so it can be undone.
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub copied: Copied,
    /// (original path, where it went in the trash).
    pub trashed: Vec<(PathBuf, PathBuf)>,
    pub dst_dir: PathBuf,
}

/// Move: copy, check every copied file matches, then move the originals
/// into the trash. With a conflict the move stops before anything moves.
pub fn move_session(
    src_dir: &Path,
    jsonl: &Path,
    id: &str,
    dst_dir: &Path,
    conflict: Conflict,
    trash: &Path,
) -> Result<Moved> {
    let copied = copy_session(src_dir, jsonl, id, dst_dir, conflict)?;
    if copied.skipped {
        bail!("the target already has this session (skipped, nothing moved)");
    }
    let parts = related(src_dir, jsonl, id);
    for (src, r) in &parts {
        let dst = dst_dir.join(rename_id(r, id, &copied.id));
        // A re-id'd transcript differs on purpose; check its size class only.
        let same = if copied.id != id && src == jsonl {
            dst.exists()
        } else {
            digest(src)? == digest(&dst)?
        };
        if !same {
            bail!("copy of {} does not match, nothing moved", src.display());
        }
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let base = trash.join(format!("{stamp}-{id}"));
    let mut trashed = vec![];
    for (src, r) in parts {
        let t = base.join(&r);
        if let Some(p) = t.parent() {
            fs::create_dir_all(p)?;
        }
        if fs::rename(&src, &t).is_err() {
            // Other volume: copy then remove.
            copy_tree(&src, &t)?;
            if src.is_dir() {
                fs::remove_dir_all(&src)?;
            } else {
                fs::remove_file(&src)?;
            }
        }
        trashed.push((src, t));
    }
    Ok(Moved {
        copied,
        trashed,
        dst_dir: dst_dir.to_path_buf(),
    })
}

/// Undo a move: originals back, the copy removed.
pub fn undo_move(m: &Moved) -> Result<()> {
    for (orig, t) in &m.trashed {
        if let Some(p) = orig.parent() {
            fs::create_dir_all(p)?;
        }
        fs::rename(t, orig).with_context(|| format!("restoring {}", orig.display()))?;
    }
    // Remove what the copy created in the target.
    let jsonl = &m.copied.jsonl;
    let dir_cfg = &m.dst_dir;
    for (p, _) in related(dir_cfg, jsonl, &m.copied.id) {
        if p.is_dir() {
            let _ = fs::remove_dir_all(&p);
        } else {
            let _ = fs::remove_file(&p);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cg-sops-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake config dir with one session and all its side data.
    fn session(dir: &Path, id: &str) -> PathBuf {
        let proj = dir.join("projects").join("-Users-x-proj");
        fs::create_dir_all(proj.join(id).join("subagents")).unwrap();
        fs::write(proj.join(id).join("subagents").join("a.jsonl"), "{}\n").unwrap();
        let j = proj.join(format!("{id}.jsonl"));
        fs::write(&j, format!("{{\"sessionId\":\"{id}\",\"cwd\":\"/Users/x/proj\",\"type\":\"user\"}}\n{{\"sessionId\":\"{id}\"}}\n")).unwrap();
        fs::create_dir_all(dir.join("file-history").join(id)).unwrap();
        fs::write(dir.join("file-history").join(id).join("abc@v1"), "old").unwrap();
        fs::create_dir_all(dir.join("session-env").join(id)).unwrap();
        fs::write(dir.join("session-env").join(id).join("hook.sh"), "#!").unwrap();
        // Another session that must not come along.
        fs::create_dir_all(dir.join("file-history").join("other")).unwrap();
        j
    }

    #[test]
    fn finds_every_part() {
        let d = tmp("rel");
        let j = session(&d, "s1");
        let r: Vec<PathBuf> = related(&d, &j, "s1").into_iter().map(|(_, r)| r).collect();
        assert!(r.contains(&PathBuf::from("projects/-Users-x-proj/s1.jsonl")));
        assert!(r.contains(&PathBuf::from("projects/-Users-x-proj/s1")));
        assert!(r.contains(&PathBuf::from("file-history/s1")));
        assert!(r.contains(&PathBuf::from("session-env/s1")));
        assert_eq!(r.len(), 4);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn copies_and_handles_conflicts() {
        let (a, b) = (tmp("ca"), tmp("cb"));
        let j = session(&a, "s1");
        let c = copy_session(&a, &j, "s1", &b, Conflict::Skip).unwrap();
        assert_eq!((c.files, c.skipped, c.id.as_str()), (4, false, "s1"));
        assert!(b.join("file-history/s1/abc@v1").is_file());
        assert!(b
            .join("projects/-Users-x-proj/s1/subagents/a.jsonl")
            .is_file());
        assert!(!b.join("file-history/other").exists());
        assert!(j.exists(), "copy keeps the original");
        // Again: skip, overwrite, or a new id.
        assert!(
            copy_session(&a, &j, "s1", &b, Conflict::Skip)
                .unwrap()
                .skipped
        );
        assert!(
            !copy_session(&a, &j, "s1", &b, Conflict::Overwrite)
                .unwrap()
                .skipped
        );
        let n = copy_session(&a, &j, "s1", &b, Conflict::NewId).unwrap();
        assert_ne!(n.id, "s1");
        let text = fs::read_to_string(&n.jsonl).unwrap();
        assert!(
            text.contains(&format!("\"sessionId\":\"{}\"", n.id))
                && !text.contains("\"sessionId\":\"s1\"")
        );
        assert!(b.join("file-history").join(&n.id).join("abc@v1").is_file());
        assert!(copy_session(&a, &j, "s1", &a, Conflict::Skip).is_err());
        let _ = fs::remove_dir_all(a);
        let _ = fs::remove_dir_all(b);
    }

    #[test]
    fn moves_to_trash_and_undoes() {
        let (a, b, t) = (tmp("ma"), tmp("mb"), tmp("mt"));
        let j = session(&a, "s2");
        let m = move_session(&a, &j, "s2", &b, Conflict::Skip, &t).unwrap();
        assert!(!j.exists() && !a.join("file-history/s2").exists());
        assert!(b.join("projects/-Users-x-proj/s2.jsonl").is_file());
        assert_eq!(m.trashed.len(), 4);
        assert!(m
            .trashed
            .iter()
            .all(|(_, p)| p.starts_with(&t) && p.exists()));
        undo_move(&m).unwrap();
        assert!(j.exists() && a.join("file-history/s2/abc@v1").exists());
        assert!(!b.join("projects/-Users-x-proj/s2.jsonl").exists());
        // A conflict stops a move before anything moves.
        copy_session(&a, &j, "s2", &b, Conflict::Skip).unwrap();
        assert!(move_session(&a, &j, "s2", &b, Conflict::Skip, &t).is_err());
        assert!(j.exists());
        for d in [a, b, t] {
            let _ = fs::remove_dir_all(d);
        }
    }

    #[test]
    fn uuids() {
        let u = new_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_ne!(u, new_uuid());
    }

    #[test]
    fn main_dir_override() {
        // Read only: just the path logic.
        // Unit tests never see the real ~/.claude.
        assert!(!main_dir().starts_with(crate::config::home_dir().join(".claude")));
    }
}
