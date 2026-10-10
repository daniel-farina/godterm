//! Renaming a coding session the way its harness does, so the name also
//! shows in `claude --resume` and Grok's own picker. Claude Code's
//! /rename appends a `custom-title` and an `agent-name` record to the
//! transcript; Grok's sets `generated_title` with `title_is_manual` in
//! the session's summary.json. A session running in a tab gets the
//! harness's own `/rename` command instead (see app_projects.rs).

use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

/// A session name as given: one line, no control characters, at most
/// 100 characters.
pub fn clean_name(s: &str) -> Result<String, String> {
    let n: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(100)
        .collect();
    let n = n.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() {
        return Err("name is required".into());
    }
    Ok(n)
}

/// Append Claude Code's title records to a transcript in one write (an
/// O_APPEND write this small lands whole, even while claude appends).
pub fn append_claude_title(path: &Path, id: &str, name: &str) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    if !path.is_file() {
        return Err("transcript gone".into());
    }
    let mut buf = String::new();
    // A transcript that does not end in a newline gets one first.
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > 0 {
        let mut b = [0u8; 1];
        f.seek(SeekFrom::Start(len - 1))
            .map_err(|e| e.to_string())?;
        f.read_exact(&mut b).map_err(|e| e.to_string())?;
        if b[0] != b'\n' {
            buf.push('\n');
        }
    }
    for v in [
        json!({"type": "custom-title", "customTitle": name, "sessionId": id}),
        json!({"type": "agent-name", "agentName": name, "sessionId": id}),
    ] {
        buf.push_str(&v.to_string());
        buf.push('\n');
    }
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(buf.as_bytes()).map_err(|e| e.to_string())
}

/// Set a Grok session's manual title in its summary.json (written to a
/// temp file and renamed over it, permissions kept).
pub fn set_grok_title(dir: &Path, name: &str) -> Result<(), String> {
    let p = dir.join("summary.json");
    let text = std::fs::read_to_string(&p).map_err(|e| format!("summary.json: {e}"))?;
    let mut v: Value = serde_json::from_str(&text).map_err(|e| format!("summary.json: {e}"))?;
    let o = v.as_object_mut().ok_or("summary.json is not an object")?;
    o.insert("generated_title".into(), json!(name));
    o.insert("title_is_manual".into(), json!(true));
    let out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    let tmp = dir.join(".summary.json.godterm-tmp");
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    if let Ok(m) = std::fs::metadata(&p) {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}
