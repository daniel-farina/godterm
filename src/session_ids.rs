//! Session ids as people say them, and Claude Code's background runs.
//!
//! - A session id given in full, as an 8 character prefix, or cut short
//!   when pasted ("d34c6258-af8c-417b-8e66-53936c24b49", one char short)
//!   matches the session it starts.
//! - Claude Code 2.1.29x can send a session to the background (`/bg`,
//!   `claude --bg`). The background run is a job with a short id (the
//!   first 8 characters of its own session id, a fork of the original:
//!   `--session-id <new> --fork-session --resume <old>`), recorded in
//!   `<config dir>/jobs/<short>/state.json`. Its fresh transcript must
//!   never replace the tab's real conversation, and a resume that hits
//!   it fails with "That session is running in the background (<short>)".

use serde_json::Value;
use std::path::{Path, PathBuf};

/// A query that is (the start of) a session id: hex digits and dashes,
/// at least 6 characters with a digit. Lowercased, with quotes, backticks
/// and trailing punctuation dropped. None for ordinary words.
pub fn id_query(q: &str) -> Option<String> {
    let t = q
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '(' | ')' | '.' | ',' | ';' | ':'))
        .to_lowercase();
    let ok = t.len() >= 6
        && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && t.chars().any(|c| c.is_ascii_digit())
        && !t.starts_with('-');
    ok.then_some(t)
}

/// `id` is the session `q` (an id_query) names: the whole id or its start.
pub fn id_matches(q: &str, id: &str) -> bool {
    !q.is_empty() && id.to_lowercase().starts_with(q)
}

/// One background run of Claude Code in a config dir.
#[derive(Debug, Clone, PartialEq)]
pub struct BgJob {
    /// The job id (`claude attach <short>`).
    pub short: String,
    /// Its own session id (a fork).
    pub session_id: Option<String>,
    /// The session it continues, when known.
    pub forked_from: Option<String>,
    pub cwd: Option<String>,
    /// Its last state word ("working", "idle"...).
    pub state: Option<String>,
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// The session id in a transcript path (`.../<id>.jsonl`).
fn id_of_path(p: &str) -> Option<String> {
    Path::new(p)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
}

/// The background job `short` of `config_dir`, if there is one.
pub fn bg_job(config_dir: &Path, short: &str) -> Option<BgJob> {
    if short.is_empty() || short.contains(['/', '\\', '.']) {
        return None;
    }
    let st = read_json(&config_dir.join("jobs").join(short).join("state.json"))?;
    let s = |k: &str| st[k].as_str().map(str::to_string);
    let session_id = s("sessionId").or(s("resumeSessionId"));
    // The roster says what it was started from (a resume of a transcript).
    let forked_from = read_json(&config_dir.join("daemon").join("roster.json")).and_then(|r| {
        let l = &r["workers"][short]["dispatch"]["launch"];
        l["sessionId"].as_str().and_then(|p| {
            if p.ends_with(".jsonl") {
                id_of_path(p)
            } else {
                Some(p.to_string())
            }
        })
    });
    Some(BgJob {
        short: short.to_string(),
        session_id,
        forked_from: forked_from.filter(|f| Some(f) != s("sessionId").as_ref()),
        cwd: s("cwd"),
        state: s("state"),
    })
}

/// Every background job of `config_dir`.
pub fn bg_jobs(config_dir: &Path) -> Vec<BgJob> {
    let mut v: Vec<BgJob> = std::fs::read_dir(config_dir.join("jobs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| bg_job(config_dir, &e.file_name().to_string_lossy()))
        .collect();
    v.sort_by(|a, b| a.short.cmp(&b.short));
    v
}

/// `id` is the session of a background run in `config_dir` (not a tab's
/// own conversation).
pub fn is_bg_session(config_dir: &Path, id: &str) -> bool {
    id.len() >= 8
        && bg_job(config_dir, crate::paths::head(id, 8))
            .is_some_and(|j| j.session_id.as_deref() == Some(id))
}

/// A transcript with at least one real message (not only a title).
pub fn has_messages(p: &Path) -> bool {
    use std::io::BufRead;
    let Ok(f) = std::fs::File::open(p) else {
        return false;
    };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .any(|l| {
            l.contains("\"type\":\"user\"")
                || l.contains("\"type\":\"assistant\"")
                || l.contains("\"type\": \"user\"")
                || l.contains("\"type\": \"assistant\"")
        })
}

/// The transcript of session `id` under `config_dir/projects`.
pub fn transcript(config_dir: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\', '.']) {
        return None;
    }
    std::fs::read_dir(config_dir.join("projects"))
        .ok()?
        .flatten()
        .map(|d| d.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())
}

/// The job id in claude's "That session is running in the background
/// (516d52e6). Run `claude attach 516d52e6` ..." (the screen may wrap it).
pub fn parse_bg_lock(screen: &str) -> Option<String> {
    let flat = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    let i = flat.find("running in the background (")?;
    let rest = &flat[i + "running in the background (".len()..];
    let short: String = rest.chars().take_while(|c| *c != ')').collect();
    let short = short.trim().to_string();
    (!short.is_empty()
        && short.len() <= 40
        && short
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then_some(short)
}

/// A tab whose resume hit a background run: what holds it and the choice.
#[derive(Debug, Clone, PartialEq)]
pub struct BgLock {
    /// The background job id.
    pub short: String,
    /// The conversation the tab tried to resume.
    pub session_id: Option<String>,
    /// Asked "stop it?" and waiting for y.
    pub confirm_stop: bool,
    /// `claude stop` is running.
    pub stopping: bool,
}

/// How a tab's next launch differs from a plain resume.
#[derive(Debug, Clone, PartialEq)]
pub enum NextLaunch {
    /// `claude attach <short>`: open the background run here.
    Attach(String),
    /// Resume with `--fork-session`: a copy under a new id.
    Fork,
}

/// What to say for a tab held by a background run.
pub fn lock_choices(short: &str) -> String {
    format!(
        " That session runs in the background ({short}): Enter or a attaches it here, s stops it and resumes here, f forks a copy "
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_whole_prefix_and_truncated() {
        let id = "d34c6258-af8c-417b-8e66-53936c24b49f";
        for q in [
            "d34c6258-af8c-417b-8e66-53936c24b49f",
            "D34C6258",
            "d34c6258",
            "d34c6258-af8c-417b-8e66-53936c24b49",
            " `d34c6258-af8c-417b-8e66-53936c24b49`. ",
        ] {
            let n = id_query(q).unwrap_or_else(|| panic!("{q} is an id"));
            assert!(id_matches(&n, id), "{q}");
        }
        assert!(!id_matches(&id_query("516d52e6").unwrap(), id));
        for words in ["claudego", "fix the bug", "deadbeef cafe", "abc", "facade"] {
            assert_eq!(id_query(words), None, "{words}");
        }
    }

    #[test]
    fn parses_the_lock_message_even_wrapped() {
        let s = "That session is running in the background (516d52e6). Run `claude\nattach 516d52e6` to open it, or `claude stop 516d52e6` first to\nresume it here. Add --fork-session to branch off a copy instead.";
        assert_eq!(parse_bg_lock(s).as_deref(), Some("516d52e6"));
        let wrapped = "That session is running in the\nbackground (516d52e6). Run";
        assert_eq!(parse_bg_lock(wrapped).as_deref(), Some("516d52e6"));
        assert_eq!(parse_bg_lock("No conversation found"), None);
    }

    #[test]
    fn reads_a_background_job_and_its_fork_source() {
        let d = std::env::temp_dir().join(format!("godterm-bgjob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let job = d.join("jobs").join("516d52e6");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(
            job.join("state.json"),
            r#"{"state":"working","sessionId":"516d52e6-15ea-46d1-8318-ffaf31bc91cd","cwd":"/x"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(d.join("daemon")).unwrap();
        std::fs::write(
            d.join("daemon").join("roster.json"),
            r#"{"workers":{"516d52e6":{"dispatch":{"launch":{"mode":"resume","sessionId":"/c/projects/-x/d34c6258-af8c-417b-8e66-53936c24b49f.jsonl","fork":true}}}}}"#,
        )
        .unwrap();
        let j = bg_job(&d, "516d52e6").unwrap();
        assert_eq!(
            j.session_id.as_deref(),
            Some("516d52e6-15ea-46d1-8318-ffaf31bc91cd")
        );
        assert_eq!(
            j.forked_from.as_deref(),
            Some("d34c6258-af8c-417b-8e66-53936c24b49f")
        );
        assert!(is_bg_session(&d, "516d52e6-15ea-46d1-8318-ffaf31bc91cd"));
        assert!(!is_bg_session(&d, "d34c6258-af8c-417b-8e66-53936c24b49f"));
        assert_eq!(bg_jobs(&d).len(), 1);
        assert!(bg_job(&d, "../x").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
