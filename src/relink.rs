use std::path::{Path, PathBuf};

use crate::harness::Harness;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Found,
    Missing,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: PathBuf,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lost {
    pub id: String,
    pub dir: PathBuf,
    pub why: String,
}

pub fn screen_says_missing(screen: &str) -> bool {
    screen
        .lines()
        .any(|l| l.trim_start().starts_with("No conversation found"))
}

fn canon(p: &Path) -> PathBuf {
    crate::fspath::canonical(p).unwrap_or_else(|_| p.to_path_buf())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    !a.as_os_str().is_empty() && (a == b || canon(a) == canon(b))
}

fn slugs(cwd: &Path) -> Vec<String> {
    let mut v = vec![crate::state::encode_project_dir(cwd)];
    let c = crate::state::encode_project_dir(&canon(cwd));
    if !v.contains(&c) {
        v.push(c);
    }
    v
}

fn subdirs(p: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

pub fn locate(h: Harness, home: &Path, id: &str) -> Vec<Hit> {
    if !Harness::safe_session_id(id) || id.contains('.') {
        return vec![];
    }
    let mut out = vec![];
    match h {
        Harness::Claude => {
            for proj in subdirs(&home.join("projects")) {
                let name = proj
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if crate::app_resume::is_assistant_project(&name) {
                    continue;
                }
                let f = proj.join(format!("{id}.jsonl"));
                if f.is_file() {
                    let cwd = crate::sessions::parse_file_quick(&f)
                        .map(|i| i.cwd)
                        .unwrap_or_default();
                    out.push(Hit { path: f, cwd });
                }
            }
        }
        Harness::Grok => {
            for d in subdirs(&home.join("sessions")) {
                let s = d.join(id);
                if s.join("summary.json").is_file() {
                    let cwd = crate::harness::grok::parse_session_any(&s, true)
                        .map(|i| i.cwd)
                        .unwrap_or_default();
                    out.push(Hit { path: s, cwd });
                }
            }
        }
        Harness::Cursor => {
            for p in subdirs(&home.join("projects")) {
                let f = p
                    .join("agent-transcripts")
                    .join(id)
                    .join(format!("{id}.jsonl"));
                if f.is_file() {
                    let cwd = crate::session_other::parse_cursor(&f)
                        .map(|i| i.cwd)
                        .unwrap_or_default();
                    out.push(Hit { path: f, cwd });
                }
            }
        }
        Harness::Codex => {
            for f in crate::session_other::list_codex(home) {
                let named = f
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().contains(id));
                if named {
                    let cwd = crate::session_other::parse_codex(&f)
                        .map(|i| i.cwd)
                        .unwrap_or_default();
                    out.push(Hit { path: f, cwd });
                }
            }
        }
        _ => {}
    }
    out
}

pub fn status(h: Harness, home: &Path, cwd: &Path, id: &str) -> Status {
    match h {
        Harness::Claude | Harness::Grok | Harness::Cursor => {
            let hits = locate(h, home, id);
            let slugs = slugs(cwd);
            let here = hits.iter().any(|x| {
                same_dir(Path::new(&x.cwd), cwd)
                    || (h == Harness::Claude
                        && x.path
                            .parent()
                            .and_then(|p| p.file_name())
                            .is_some_and(|n| slugs.iter().any(|s| n == s.as_str())))
            });
            if here {
                Status::Found
            } else {
                Status::Missing
            }
        }
        Harness::Codex => {
            if locate(h, home, id).is_empty() {
                Status::Missing
            } else {
                Status::Found
            }
        }
        _ => Status::Unknown,
    }
}

pub fn check_dir(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("a folder path is required".into());
    }
    let p = crate::config::expand_tilde(raw);
    if !p.is_absolute() {
        return Err(format!("{raw} is not a full path (start it with / or ~)"));
    }
    if crate::guard::in_vault(&p) || crate::guard::in_vault(&canon(&p)) {
        return Err("that folder is in the private vault; GodTerm never touches it".into());
    }
    if !p.exists() {
        return Err(format!("{} does not exist", crate::config::tilde(&p)));
    }
    if !p.is_dir() {
        return Err(format!("{} is not a folder", crate::config::tilde(&p)));
    }
    Ok(p)
}

fn percent(p: &Path) -> String {
    let mut s = String::new();
    for b in p.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn grok_dir_for(home: &Path, dir: &Path) -> PathBuf {
    let root = home.join("sessions");
    for d in subdirs(&root) {
        for s in subdirs(&d) {
            let same = crate::harness::grok::parse_session_any(&s, true)
                .is_some_and(|i| same_dir(Path::new(&i.cwd), dir));
            if same {
                return d;
            }
        }
    }
    root.join(percent(dir))
}

pub fn copy_into(
    h: Harness,
    home: &Path,
    hit: &Hit,
    id: &str,
    dir: &Path,
) -> Result<PathBuf, String> {
    if !Harness::safe_session_id(id) || id.contains('.') {
        return Err("that session id is not a plain identifier".into());
    }
    let target = canon(dir);
    let e = |e: anyhow::Error| format!("copying the conversation: {e:#}");
    match h {
        Harness::Claude => {
            let dst_dir = home
                .join("projects")
                .join(crate::state::encode_project_dir(&target));
            let dst = dst_dir.join(format!("{id}.jsonl"));
            if dst.exists() {
                return Ok(dst);
            }
            crate::session_ops::copy_tree(&hit.path, &dst).map_err(e)?;
            let side = hit.path.with_extension("");
            if side.is_dir() {
                let d = dst.with_extension("");
                if !d.exists() {
                    crate::session_ops::copy_tree(&side, &d).map_err(e)?;
                }
            }
            Ok(dst)
        }
        Harness::Grok => {
            let dst = grok_dir_for(home, &target).join(id);
            if dst.exists() {
                return Ok(dst);
            }
            crate::session_ops::copy_tree(&hit.path, &dst).map_err(e)?;
            let sum = dst.join("summary.json");
            if let Ok(text) = std::fs::read_to_string(&sum) {
                if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(c) = v.pointer_mut("/info/cwd") {
                        *c = serde_json::json!(target.to_string_lossy());
                        let _ =
                            std::fs::write(&sum, serde_json::to_string_pretty(&v).unwrap_or(text));
                    }
                }
            }
            Ok(dst)
        }
        Harness::Cursor => {
            let slug = crate::state::encode_project_dir(&target);
            let dst = home
                .join("projects")
                .join(slug.trim_start_matches('-'))
                .join("agent-transcripts")
                .join(id);
            if dst.exists() {
                return Ok(dst);
            }
            let src = hit.path.parent().ok_or("no transcript folder")?;
            crate::session_ops::copy_tree(src, &dst).map_err(e)?;
            Ok(dst)
        }
        _ => Err("this agent keeps its conversations where any folder finds them".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee";

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("godterm-relink-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        crate::fspath::canonical(&d).unwrap()
    }

    fn claude_session(home: &Path, project: &Path, id: &str) -> PathBuf {
        let slug = crate::state::encode_project_dir(project);
        let dir = home.join("projects").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(format!("{id}.jsonl"));
        let line = format!(
            "{}\n",
            serde_json::json!({
                "type": "user",
                "cwd": project.display().to_string(),
                "sessionId": id,
                "message": {"role": "user", "content": "hello"},
            })
        );
        std::fs::write(&f, line).unwrap();
        f
    }

    #[test]
    fn claude_found_under_its_own_folder_missing_elsewhere() {
        let t = scratch("claude");
        let home = t.join("home");
        let old = t.join("old");
        let new = t.join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        claude_session(&home, &old, ID);
        assert_eq!(status(Harness::Claude, &home, &old, ID), Status::Found);
        assert_eq!(status(Harness::Claude, &home, &new, ID), Status::Missing);
        assert_eq!(
            status(Harness::Claude, &home, &new, "nothing-here"),
            Status::Missing
        );
        let hits = locate(Harness::Claude, &home, ID);
        assert_eq!(hits.len(), 1);
        assert_eq!(Path::new(&hits[0].cwd), old.as_path());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn claude_copy_keeps_the_original_and_then_resolves() {
        let t = scratch("copy");
        let home = t.join("home");
        let old = t.join("old");
        let new = t.join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let src = claude_session(&home, &old, ID);
        std::fs::create_dir_all(src.with_extension("").join("subagents")).unwrap();
        std::fs::write(src.with_extension("").join("subagents/a.jsonl"), "{}\n").unwrap();
        let hit = locate(Harness::Claude, &home, ID).remove(0);
        let dst = copy_into(Harness::Claude, &home, &hit, ID, &new).unwrap();
        assert!(dst.is_file() && src.is_file());
        assert!(dst.with_extension("").join("subagents/a.jsonl").is_file());
        assert_eq!(status(Harness::Claude, &home, &new, ID), Status::Found);
        assert_eq!(status(Harness::Claude, &home, &old, ID), Status::Found);
        let again = copy_into(Harness::Claude, &home, &hit, ID, &new).unwrap();
        assert_eq!(again, dst);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn grok_follows_the_recorded_folder_and_the_copy_rewrites_only_itself() {
        let t = scratch("grok");
        let home = t.join("home");
        let old = t.join("old");
        let new = t.join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let s = home.join("sessions").join(percent(&old)).join(ID);
        std::fs::create_dir_all(&s).unwrap();
        let summary = serde_json::json!({"info": {"id": ID, "cwd": old.to_string_lossy()}, "num_chat_messages": 2});
        std::fs::write(s.join("summary.json"), summary.to_string()).unwrap();
        std::fs::write(s.join("chat.jsonl"), "{}\n").unwrap();
        assert_eq!(status(Harness::Grok, &home, &old, ID), Status::Found);
        assert_eq!(status(Harness::Grok, &home, &new, ID), Status::Missing);
        let hit = locate(Harness::Grok, &home, ID).remove(0);
        let dst = copy_into(Harness::Grok, &home, &hit, ID, &new).unwrap();
        assert!(dst.starts_with(home.join("sessions").join(percent(&new))));
        assert_eq!(status(Harness::Grok, &home, &new, ID), Status::Found);
        let orig: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(s.join("summary.json")).unwrap())
                .unwrap();
        assert_eq!(orig["info"]["cwd"], old.to_string_lossy().as_ref());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn codex_is_found_anywhere_and_unknown_agents_are_not_judged() {
        let t = scratch("codex");
        let home = t.join("home");
        let day = home.join("sessions/2026/10/01");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join(format!("rollout-2026-10-01T10-00-00-{ID}.jsonl")),
            "{}\n",
        )
        .unwrap();
        assert_eq!(status(Harness::Codex, &home, &t, ID), Status::Found);
        assert_eq!(
            status(Harness::Codex, &home, &t, "other-id"),
            Status::Missing
        );
        assert_eq!(status(Harness::OpenCode, &home, &t, ID), Status::Unknown);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn folder_checks() {
        let t = scratch("dir");
        let file = t.join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(check_dir(t.to_str().unwrap()).is_ok());
        assert!(check_dir("").is_err());
        assert!(check_dir("relative/dir").unwrap_err().contains("full path"));
        assert!(check_dir(t.join("nope").to_str().unwrap())
            .unwrap_err()
            .contains("does not exist"));
        assert!(check_dir(file.to_str().unwrap())
            .unwrap_err()
            .contains("not a folder"));
        #[cfg(unix)]
        {
            assert!(check_dir("/Volumes/Private/x")
                .unwrap_err()
                .contains("vault"));
        }
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn unsafe_ids_find_nothing() {
        let t = scratch("ids");
        assert!(locate(Harness::Claude, &t, "../x").is_empty());
        assert!(locate(Harness::Claude, &t, "").is_empty());
        assert!(screen_says_missing(
            "\n No conversation found with session ID: abc\n"
        ));
        assert!(!screen_says_missing("all good"));
        let _ = std::fs::remove_dir_all(&t);
    }
}
