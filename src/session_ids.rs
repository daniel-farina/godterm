use serde_json::Value;
use std::path::{Path, PathBuf};

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

pub fn id_matches(q: &str, id: &str) -> bool {
    !q.is_empty() && id.to_lowercase().starts_with(q)
}

#[derive(Debug, Clone, PartialEq)]
pub struct BgJob {
    pub short: String,

    pub session_id: Option<String>,

    pub forked_from: Option<String>,
    pub cwd: Option<String>,

    pub state: Option<String>,
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn id_of_path(p: &str) -> Option<String> {
    Path::new(p)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
}

pub fn bg_job(config_dir: &Path, short: &str) -> Option<BgJob> {
    if short.is_empty() || short.contains(['/', '\\', '.']) {
        return None;
    }
    let st = read_json(&config_dir.join("jobs").join(short).join("state.json"))?;
    let s = |k: &str| st[k].as_str().map(str::to_string);
    let session_id = s("sessionId").or(s("resumeSessionId"));

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

pub fn is_bg_session(config_dir: &Path, id: &str) -> bool {
    id.len() >= 8
        && bg_job(config_dir, crate::paths::head(id, 8))
            .is_some_and(|j| j.session_id.as_deref() == Some(id))
}

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

#[derive(Debug, Clone, PartialEq)]
pub struct BgLock {
    pub short: String,

    pub session_id: Option<String>,

    pub confirm_stop: bool,

    pub stopping: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NextLaunch {
    Attach(String),

    Fork,
}

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
