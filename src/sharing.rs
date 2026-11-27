use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::AccountCfg;
use crate::harness::Harness;

pub const SETTINGS_URL: &str = "https://api.anthropic.com/api/oauth/account/settings";
pub const CLAUDE_PAGE: &str = "https://claude.ai/settings/data-privacy-controls";
pub const CURSOR_PAGE: &str = "https://cursor.com/dashboard";

const NETWORK_TTL: Duration = Duration::from_secs(600);
const LOCAL_TTL: Duration = Duration::from_secs(20);
const RETRY_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    On,
    Off,
    Unknown,
}

impl Sharing {
    pub fn word(self) -> &'static str {
        match self {
            Sharing::On => "on",
            Sharing::Off => "off",
            Sharing::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub state: Sharing,
    pub locked: bool,
}

impl Reading {
    fn of(state: Sharing) -> Reading {
        Reading {
            state,
            locked: false,
        }
    }
}

pub fn supported(h: Harness) -> bool {
    matches!(h, Harness::Claude | Harness::Grok | Harness::Cursor)
}

pub fn parse_claude(body: &str) -> Sharing {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Sharing::Unknown;
    };
    match v.get("grove_enabled").and_then(Value::as_bool) {
        Some(true) => Sharing::On,
        Some(false) => Sharing::Off,
        None => Sharing::Unknown,
    }
}

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(o) => o
            .get(key)
            .or_else(|| o.values().find_map(|x| find_key(x, key))),
        _ => None,
    }
}

pub fn parse_grok(auth_json: &str) -> Reading {
    let Ok(v) = serde_json::from_str::<Value>(auth_json) else {
        return Reading::of(Sharing::Unknown);
    };
    if find_key(&v, "team_zero_data_retention").and_then(Value::as_bool) == Some(true) {
        return Reading {
            state: Sharing::Off,
            locked: true,
        };
    }
    match find_key(&v, "coding_data_retention_opt_out").and_then(Value::as_bool) {
        Some(true) => Reading::of(Sharing::Off),
        Some(false) => Reading::of(Sharing::On),
        None => Reading::of(Sharing::Unknown),
    }
}

pub fn parse_cursor(cli_config: &str) -> Sharing {
    let Ok(v) = serde_json::from_str::<Value>(cli_config) else {
        return Sharing::Unknown;
    };
    match v
        .get("privacyCache")
        .and_then(|p| p.get("ghostMode"))
        .and_then(Value::as_bool)
    {
        Some(true) => Sharing::Off,
        Some(false) => Sharing::On,
        None => Sharing::Unknown,
    }
}

fn read_local(h: Harness, dir: &Path) -> Reading {
    match h {
        Harness::Grok => std::fs::read_to_string(dir.join("auth.json"))
            .map(|t| parse_grok(&t))
            .unwrap_or(Reading::of(Sharing::Unknown)),
        Harness::Cursor => {
            let p = crate::config::home_dir().join(".cursor/cli-config.json");
            Reading::of(
                std::fs::read_to_string(p)
                    .map(|t| parse_cursor(&t))
                    .unwrap_or(Sharing::Unknown),
            )
        }
        _ => Reading::of(Sharing::Unknown),
    }
}

fn fixture(name: &str) -> Option<String> {
    let base = crate::config::env_var("SHARING_FIXTURE")?;
    let base = Path::new(&base);
    let path = if base.is_dir() {
        base.join(format!("{name}.json"))
    } else {
        base.to_path_buf()
    };
    Some(std::fs::read_to_string(path).unwrap_or_default())
}

fn fetch_claude(name: &str, dir: &Path) -> Option<Sharing> {
    if let Some(body) = fixture(name) {
        return Some(parse_claude(&body));
    }
    if cfg!(test) {
        return None;
    }
    fetch_live(dir)
}

fn fetch_live(dir: &Path) -> Option<Sharing> {
    let (creds, _) = crate::creds::load_creds(dir)?;
    if creds.is_expired(chrono::Utc::now().timestamp_millis()) {
        return None;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(SETTINGS_URL)
        .header("Authorization", &format!("Bearer {}", creds.access_token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Accept", "application/json")
        .header("User-Agent", concat!("godterm/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?;
    if !(200..300).contains(&resp.status().as_u16()) {
        return None;
    }
    let body = resp.body_mut().read_to_string().ok()?;
    Some(parse_claude(&body))
}

struct Entry {
    at: Instant,
    ttl: Duration,
    reading: Reading,
    busy: bool,
}

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    static C: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(a: &AccountCfg) -> String {
    format!("{}:{}", a.harness().name(), a.name)
}

pub fn forget(a: &AccountCfg) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key(a));
}

pub fn reading(a: &AccountCfg) -> Option<Reading> {
    let h = a.harness();
    if !supported(h) {
        return None;
    }
    let k = key(a);
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = c.get(&k) {
        if e.busy || e.at.elapsed() < e.ttl {
            return Some(e.reading);
        }
    }
    let previous = c
        .get(&k)
        .map(|e| e.reading)
        .unwrap_or(Reading::of(Sharing::Unknown));
    if h != Harness::Claude {
        let r = read_local(h, &a.config_dir());
        c.insert(
            k,
            Entry {
                at: Instant::now(),
                ttl: LOCAL_TTL,
                reading: r,
                busy: false,
            },
        );
        return Some(r);
    }
    c.insert(
        k.clone(),
        Entry {
            at: Instant::now(),
            ttl: NETWORK_TTL,
            reading: previous,
            busy: true,
        },
    );
    drop(c);
    let name = a.name.clone();
    let dir = a.config_dir();
    std::thread::spawn(move || {
        let got = fetch_claude(&name, &dir);
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        let (state, ttl) = match got {
            Some(s) => (s, NETWORK_TTL),
            None => (previous.state, RETRY_TTL),
        };
        c.insert(
            k,
            Entry {
                at: Instant::now(),
                ttl,
                reading: Reading::of(state),
                busy: false,
            },
        );
    });
    Some(previous)
}

pub fn json_for(a: &AccountCfg) -> Value {
    let Some(r) = reading(a) else {
        return Value::Null;
    };
    let h = a.harness();
    let (product, how, url) = match h {
        Harness::Claude => ("Help improve Claude", "link", CLAUDE_PAGE),
        Harness::Grok => ("Help improve Grok", "slash", ""),
        _ => ("Cursor privacy mode", "link", CURSOR_PAGE),
    };
    json!({
        "state": r.state.word(),
        "locked": r.locked,
        "product": product,
        "how": how,
        "url": url,
        "slash": if h == Harness::Grok { "/privacy" } else { "" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_answer_maps_the_account_setting() {
        assert_eq!(parse_claude(r#"{"grove_enabled":true}"#), Sharing::On);
        assert_eq!(parse_claude(r#"{"grove_enabled":false}"#), Sharing::Off);
        assert_eq!(parse_claude(r#"{"grove_enabled":null}"#), Sharing::Unknown);
        assert_eq!(parse_claude("{}"), Sharing::Unknown);
        assert_eq!(parse_claude("not json"), Sharing::Unknown);
        assert_eq!(parse_claude(""), Sharing::Unknown);
    }

    #[test]
    fn grok_auth_file_opt_out_means_sharing_is_off() {
        let off = r#"{"https://auth.x.ai::id":{"email":"a@b.c","team_zero_data_retention":false,"coding_data_retention_opt_out":true}}"#;
        let on = r#"{"https://auth.x.ai::id":{"coding_data_retention_opt_out":false}}"#;
        assert_eq!(parse_grok(off), Reading::of(Sharing::Off));
        assert_eq!(parse_grok(on), Reading::of(Sharing::On));
        assert_eq!(parse_grok("{}"), Reading::of(Sharing::Unknown));
        assert_eq!(parse_grok("junk"), Reading::of(Sharing::Unknown));
    }

    #[test]
    fn grok_zero_data_retention_team_is_off_and_locked() {
        let zdr =
            r#"{"k":{"team_zero_data_retention":true,"coding_data_retention_opt_out":false}}"#;
        assert_eq!(
            parse_grok(zdr),
            Reading {
                state: Sharing::Off,
                locked: true
            }
        );
    }

    #[test]
    fn cursor_ghost_mode_means_privacy_on_so_sharing_off() {
        let ghost = r#"{"privacyCache":{"ghostMode":true,"privacyMode":2,"updatedAt":1}}"#;
        let open = r#"{"privacyCache":{"ghostMode":false,"privacyMode":3,"updatedAt":1}}"#;
        assert_eq!(parse_cursor(ghost), Sharing::Off);
        assert_eq!(parse_cursor(open), Sharing::On);
        assert_eq!(parse_cursor("{}"), Sharing::Unknown);
    }

    #[test]
    fn local_reads_use_the_account_dir_and_missing_files_are_unknown() {
        let dir = std::env::temp_dir().join(format!("gt-sharing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            read_local(Harness::Grok, &dir),
            Reading::of(Sharing::Unknown)
        );
        std::fs::write(
            dir.join("auth.json"),
            r#"{"k":{"coding_data_retention_opt_out":false}}"#,
        )
        .unwrap();
        assert_eq!(read_local(Harness::Grok, &dir), Reading::of(Sharing::On));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn harnesses_without_a_local_flag_show_nothing() {
        for h in [Harness::Codex, Harness::Antigravity, Harness::OpenCode] {
            assert!(!supported(h));
        }
        assert!(
            supported(Harness::Claude) && supported(Harness::Grok) && supported(Harness::Cursor)
        );
    }

    fn acct(name: &str, harness: &str, dir: &Path) -> AccountCfg {
        serde_json::from_value(json!({
            "name": name,
            "harness": harness,
            "config_dir": dir.to_string_lossy(),
        }))
        .unwrap()
    }

    #[test]
    fn web_json_reports_grok_from_its_own_dir_and_hides_other_harnesses() {
        let dir = std::env::temp_dir().join(format!("gt-sharing-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{"k":{"coding_data_retention_opt_out":true}}"#,
        )
        .unwrap();
        let g = acct("sharing-json-grok", "grok", &dir);
        let v = json_for(&g);
        assert_eq!(v["state"], "off");
        assert_eq!(v["how"], "slash");
        assert_eq!(v["slash"], "/privacy");
        assert!(json_for(&acct("sharing-json-codex", "codex", &dir)).is_null());
        assert!(json_for(&acct("sharing-json-opencode", "opencode", &dir)).is_null());

        std::fs::write(
            dir.join("auth.json"),
            r#"{"k":{"coding_data_retention_opt_out":false}}"#,
        )
        .unwrap();
        forget(&g);
        assert_eq!(json_for(&g)["state"], "on");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_without_a_login_stays_unknown_and_never_touches_the_network_in_tests() {
        let dir = std::env::temp_dir().join(format!("gt-sharing-claude-{}", std::process::id()));
        let c = acct("sharing-json-claude", "claude", &dir);
        assert_eq!(json_for(&c)["state"], "unknown");
        assert_eq!(json_for(&c)["url"], CLAUDE_PAGE);
    }
}
