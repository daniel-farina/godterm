use serde_json::Value;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use crate::sessions::{lossy_lines, snippet, SessionInfo};

pub fn list_codex(home: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut dirs = vec![(home.join("sessions"), 0)];
    while let Some((d, depth)) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 3 {
                    dirs.push((p, depth + 1));
                }
            } else if is_codex(&p) {
                out.push(p);
            }
        }
    }
    out
}

pub fn list_cursor(home: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return out;
    };
    for p in projects.flatten() {
        let Ok(ids) = std::fs::read_dir(p.path().join("agent-transcripts")) else {
            continue;
        };
        for d in ids.flatten() {
            let id = d.file_name().to_string_lossy().into_owned();
            let f = d.path().join(format!("{id}.jsonl"));
            if f.is_file() {
                out.push(f);
            }
        }
    }
    out
}

pub fn is_codex(p: &Path) -> bool {
    p.file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.starts_with("rollout-") && f.ends_with(".jsonl"))
}

pub fn is_cursor(p: &Path) -> bool {
    p.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|f| f == "agent-transcripts")
}

fn prompt_text(t: &str) -> Option<String> {
    let t = t.trim();
    let t = t
        .strip_prefix("<user_query>")
        .map(|r| r.trim_end().trim_end_matches("</user_query>"))
        .unwrap_or(t)
        .trim();
    (!t.is_empty() && !t.starts_with('<')).then(|| t.to_string())
}

fn texts(c: &Value) -> Vec<String> {
    match c {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str().map(str::to_string))
            .collect(),
        _ => vec![],
    }
}

fn base(path: &Path) -> SessionInfo {
    let meta = std::fs::metadata(path).ok();
    SessionInfo {
        path: path.to_path_buf(),
        modified: meta.and_then(|m| m.modified().ok()),
        ..Default::default()
    }
}

fn when(s: &str) -> Option<std::time::SystemTime> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(std::time::SystemTime::from)
}

pub fn parse_codex(path: &Path) -> Option<SessionInfo> {
    let f = std::fs::File::open(path).ok()?;
    let mut info = base(path);
    let (mut asked, mut fallback) = (vec![], vec![]);
    for line in lossy_lines(BufReader::new(f)) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let p = &v["payload"];
        match v["type"].as_str() {
            Some("session_meta") if info.id.is_empty() => {
                let src = &p["source"];
                if src.get("subagent").is_some() || src.as_str() == Some("subagent") {
                    return None;
                }
                info.headless = src.as_str() == Some("exec");
                info.id = p["id"].as_str().unwrap_or_default().to_string();
                info.cwd = p["cwd"].as_str().unwrap_or_default().to_string();
                info.created = p["timestamp"]
                    .as_str()
                    .or(v["timestamp"].as_str())
                    .and_then(when);
            }
            Some("turn_context") => {
                if let Some(m) = p["model"].as_str() {
                    info.model = Some(m.to_string());
                }
            }
            Some("event_msg") => match p["type"].as_str() {
                Some("user_message") => {
                    if let Some(t) = p["message"].as_str().and_then(prompt_text) {
                        asked.push(t);
                    }
                }
                Some("agent_message") => info.messages += 1,
                Some("token_count") => {
                    let u = &p["info"]["total_token_usage"];
                    if let Some(i) = u["input_tokens"].as_u64() {
                        let cached = u["cached_input_tokens"].as_u64().unwrap_or(0);
                        info.tokens.input = i.saturating_sub(cached);
                        info.tokens.cache_read = cached;
                        info.tokens.output = u["output_tokens"].as_u64().unwrap_or(0);
                    }
                }
                _ => {}
            },
            Some("response_item") if p["type"] == "message" && p["role"] == "user" => {
                fallback.extend(texts(&p["content"]).iter().filter_map(|t| prompt_text(t)));
            }
            _ => {}
        }
    }
    if info.id.is_empty() {
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        info.id = stem.get(stem.len().saturating_sub(36)..)?.to_string();
    }
    let asked = if asked.is_empty() { fallback } else { asked };
    info.messages += asked.len();
    info.first_prompt = asked.first().cloned();
    info.last_prompt = asked.last().cloned();
    info.title = info.first_prompt.as_deref().map(|t| snippet(t, 80));
    Some(info)
}

pub fn cursor_folder(slug: &str) -> String {
    fn walk(at: &Path, rest: &[&str]) -> Option<PathBuf> {
        if rest.is_empty() {
            return Some(at.to_path_buf());
        }

        for k in (1..=rest.len()).rev() {
            for sep in ["-", ".", "_", " "] {
                let name = rest[..k].join(sep);
                let p = at.join(&name);
                if p.is_dir() {
                    if let Some(found) = walk(&p, &rest[k..]) {
                        return Some(found);
                    }
                }
                if k == 1 {
                    break;
                }
            }
        }
        None
    }
    let parts: Vec<&str> = slug.split('-').filter(|s| !s.is_empty()).collect();
    walk(Path::new("/"), &parts)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("/{}", parts.join("/")))
}

pub fn parse_cursor(path: &Path) -> Option<SessionInfo> {
    let f = std::fs::File::open(path).ok()?;
    let mut info = base(path);
    info.id = path.file_stem()?.to_string_lossy().into_owned();
    let slug = path.parent()?.parent()?.parent()?.file_name()?;
    info.cwd = cursor_folder(&slug.to_string_lossy());
    let mut asked = vec![];
    for line in lossy_lines(BufReader::new(f)) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let role = v["role"]
            .as_str()
            .or(v["message"]["role"].as_str())
            .or(v["type"].as_str());
        let content = if v["message"].is_object() {
            &v["message"]["content"]
        } else {
            &v["content"]
        };
        match role {
            Some("user") => asked.extend(texts(content).iter().filter_map(|t| prompt_text(t))),
            Some("assistant") => info.messages += 1,
            _ => {}
        }
    }
    info.messages += asked.len();
    info.first_prompt = asked.first().cloned();
    info.last_prompt = asked.last().cloned();
    info.title = info.first_prompt.as_deref().map(|t| snippet(t, 80));
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("godterm-other-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn write(p: &Path, lines: &[&str]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn codex_main_sessions_only() {
        let t = scratch("codex");
        let day = t.join("sessions/2026/10/20");
        let main =
            day.join("rollout-2026-10-20T10-00-00-0199aaaa-0000-7000-8000-000000000001.jsonl");
        write(
            &main,
            &[
                r#"{"timestamp":"2026-10-20T10:00:00Z","type":"session_meta","payload":{"id":"0199aaaa-0000-7000-8000-000000000001","timestamp":"2026-10-20T10:00:00Z","cwd":"/work/acme-api","source":"cli"}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#,
                r#"{"type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
                r#"{"type":"event_msg","payload":{"type":"user_message","message":"Fix the acme login flow"}}"#,
                r#"{"type":"event_msg","payload":{"type":"agent_message","message":"Done."}}"#,
            ],
        );
        let sub =
            day.join("rollout-2026-10-20T10-05-00-0199aaaa-0000-7000-8000-000000000002.jsonl");
        write(
            &sub,
            &[
                r#"{"type":"session_meta","payload":{"id":"0199aaaa-0000-7000-8000-000000000002","cwd":"/work/acme-api","source":{"subagent":{"other":"review"}}}}"#,
            ],
        );
        std::fs::write(day.join("notes.txt"), "x").unwrap();
        let mut found = list_codex(&t);
        found.sort();
        assert_eq!(found.len(), 2);
        let i = parse_codex(&main).unwrap();
        assert_eq!(i.id, "0199aaaa-0000-7000-8000-000000000001");
        assert_eq!(i.cwd, "/work/acme-api");
        assert_eq!(i.first_prompt.as_deref(), Some("Fix the acme login flow"));
        assert_eq!(i.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(i.messages, 2);
        assert!(parse_codex(&sub).is_none(), "a subagent rollout is skipped");

        let ix = crate::session_index::Shared::default();
        let src = (
            crate::app_sessions::Src::Account(0),
            crate::harness::Harness::Codex,
            t.clone(),
            "codex 1".to_string(),
        );
        crate::session_index::rebuild(&ix, &[src]);
        let rows = &ix.lock().unwrap().rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].info.id, i.id);
        assert_eq!(rows[0].harness, crate::harness::Harness::Codex);
    }

    #[test]
    fn cursor_transcripts_and_folders() {
        let t = scratch("cursor");
        let id = "5b1c0000-0000-4000-8000-0000000000aa";
        let tr = t.join(format!(
            "projects/work-acme-web/agent-transcripts/{id}/{id}.jsonl"
        ));
        write(
            &tr,
            &[
                r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>\nAdd a dark theme to acme web\n</user_query>"}]}}"#,
                r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Added."}]}}"#,
            ],
        );

        write(
            &tr.with_file_name("sub-1.jsonl"),
            &[r#"{"role":"user","message":{"content":"x"}}"#],
        );
        let found = list_cursor(&t);
        assert_eq!(found, vec![tr.clone()]);
        assert!(is_cursor(&tr) && !is_codex(&tr));
        let i = parse_cursor(&tr).unwrap();
        assert_eq!(i.id, id);
        assert_eq!(
            i.first_prompt.as_deref(),
            Some("Add a dark theme to acme web")
        );
        assert_eq!(i.messages, 2);
        assert_eq!(
            i.cwd, "/work/acme/web",
            "no such folder: dashes read as slashes"
        );

        let real = t.join("acme-web");
        std::fs::create_dir_all(&real).unwrap();
        let slug = real.to_string_lossy().replace('/', "-");
        assert_eq!(cursor_folder(&slug), real.to_string_lossy());
    }
}
