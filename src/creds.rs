//! Locating and reading per-account Claude Code OAuth credentials.
//!
//! Claude Code (2.1.x) picks its secure storage name like this (from the
//! shipped bundle):
//!
//! ```text
//! service = "Claude Code" + OAUTH_FILE_SUFFIX + "-credentials" + hash
//! hash    = ""                                   when CLAUDE_CONFIG_DIR is unset
//!         = "-" + sha256(configDir NFC)[..8 hex]  when it is set
//! account = $USER
//! ```
//!
//! OAUTH_FILE_SUFFIX is empty for the production OAuth app. When the keychain
//! is unavailable it falls back to `<configDir>/.credentials.json`, which has
//! the same JSON shape: `{"claudeAiOauth": {"accessToken", "refreshToken",
//! "expiresAt", "scopes", "subscriptionType", ...}}`.
//!
//! This module only ever reads items for godterm's own config dirs. It never
//! touches the default `Claude Code-credentials` item or `~/.claude`.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::home_dir;

pub const SERVICE_BASE: &str = "Claude Code-credentials";

/// The keychain service name Claude Code uses for a given CLAUDE_CONFIG_DIR.
pub fn keychain_service(config_dir: &str) -> String {
    let digest = Sha256::digest(config_dir.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{SERVICE_BASE}-{}", &hex[..8])
}

/// True if `dir` is the user's default Claude config dir. godterm refuses to
/// operate on it so the default login is never disturbed.
pub fn is_default_dir(dir: &Path) -> bool {
    let default = home_dir().join(".claude");
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    dir == default || canon(dir) == canon(&default)
}

fn keychain_account() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "claude-code-user".into())
}

#[derive(Debug, Clone, Default)]
pub struct OAuthCreds {
    pub access_token: String,
    pub has_refresh: bool,
    /// Milliseconds since epoch.
    pub expires_at: Option<i64>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
}

impl OAuthCreds {
    pub fn is_expired(&self, now_ms: i64) -> bool {
        matches!(self.expires_at, Some(t) if t <= now_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredSource {
    Keychain,
    File,
}

impl CredSource {
    pub fn label(self) -> &'static str {
        match self {
            CredSource::Keychain => "keychain",
            CredSource::File => ".credentials.json",
        }
    }
}

/// Parse the stored credentials blob. Returns None if there is no OAuth token.
pub fn parse_creds(json: &str) -> Option<OAuthCreds> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let o = v.get("claudeAiOauth")?;
    let access_token = o.get("accessToken")?.as_str()?.to_string();
    if access_token.is_empty() {
        return None;
    }
    Some(OAuthCreds {
        access_token,
        has_refresh: o
            .get("refreshToken")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        expires_at: o.get("expiresAt").and_then(Value::as_i64),
        subscription_type: o
            .get("subscriptionType")
            .and_then(Value::as_str)
            .map(str::to_string),
        rate_limit_tier: o
            .get("rateLimitTier")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Candidate strings Claude Code may have hashed for this dir (as given, and
/// with symlinks resolved), deduplicated.
fn dir_candidates(config_dir: &Path) -> Vec<String> {
    let mut out = vec![config_dir.to_string_lossy().into_owned()];
    if let Ok(c) = fs::canonicalize(config_dir) {
        let c = c.to_string_lossy().into_owned();
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// Run a command, giving up (and killing it) after `limit`. `security` can
/// hang when the keychain is locked and waiting for a password dialog.
pub fn output_with_timeout(mut cmd: Command, limit: Duration) -> Option<std::process::Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = reader.join().unwrap_or_default();
                return Some(std::process::Output {
                    status,
                    stdout,
                    stderr: vec![],
                });
            }
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn keychain_read(service: &str) -> Option<String> {
    if !crate::demo::keychain_allowed() {
        return None;
    }
    let mut cmd = Command::new("security");
    cmd.args([
        "find-generic-password",
        "-a",
        &keychain_account(),
        "-w",
        "-s",
        service,
    ]);
    let out = output_with_timeout(cmd, Duration::from_secs(5))?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    // `security -w` prints binary data as hex; Claude stores plain JSON.
    if s.starts_with('{') {
        Some(s.to_string())
    } else {
        decode_hex(s).and_then(|b| String::from_utf8(b).ok())
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Read the OAuth credentials for an isolated config dir. Never returns the
/// default account's credentials.
pub fn load_creds(config_dir: &Path) -> Option<(OAuthCreds, CredSource)> {
    if is_default_dir(config_dir) {
        return None;
    }
    // Demo mode logins are files; the keychain is never read.
    if cfg!(target_os = "macos") && !crate::demo::active() {
        for cand in dir_candidates(config_dir) {
            let svc = keychain_service(&cand);
            if svc == SERVICE_BASE {
                continue;
            }
            if let Some(c) = keychain_read(&svc).as_deref().and_then(parse_creds) {
                return Some((c, CredSource::Keychain));
            }
        }
    }
    let file = config_dir.join(".credentials.json");
    let text = fs::read_to_string(file).ok()?;
    parse_creds(&text).map(|c| (c, CredSource::File))
}

/// Profile fields cached by Claude Code in `<dir>/.claude.json`.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub org_name: Option<String>,
    pub org_role: Option<String>,
    pub billing_type: Option<String>,
    pub org_tier: Option<String>,
    pub onboarded: bool,
}

pub fn global_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(".claude.json")
}

pub fn load_profile(config_dir: &Path) -> Profile {
    let Ok(text) = fs::read_to_string(global_config_path(config_dir)) else {
        return Profile::default();
    };
    parse_profile(&text)
}

pub fn parse_profile(text: &str) -> Profile {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Profile::default();
    };
    let s = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
    let mut p = Profile {
        onboarded: v
            .get("hasCompletedOnboarding")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    };
    if let Some(o) = v.get("oauthAccount") {
        p.email = s(o, "emailAddress");
        p.display_name = s(o, "displayName");
        p.org_name = s(o, "organizationName");
        p.org_role = s(o, "organizationRole");
        p.billing_type = s(o, "billingType");
        p.org_tier = s(o, "organizationRateLimitTier");
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_matches_claude_scheme() {
        // sha256("/Users/alex/.claude") starts with dbec7fc2.
        assert_eq!(
            keychain_service("/Users/alex/.claude"),
            "Claude Code-credentials-dbec7fc2"
        );
    }

    #[test]
    fn parses_creds_blob() {
        let c = parse_creds(
            r#"{"claudeAiOauth":{"accessToken":"tok","refreshToken":"r","expiresAt":1700000000000,"scopes":["user:inference"],"subscriptionType":"max"}}"#,
        )
        .unwrap();
        assert_eq!(c.access_token, "tok");
        assert!(c.has_refresh);
        assert_eq!(c.subscription_type.as_deref(), Some("max"));
        assert!(c.is_expired(1_800_000_000_000));
        assert!(!c.is_expired(1_600_000_000_000));
        assert!(parse_creds(r#"{"other":1}"#).is_none());
        assert!(parse_creds("garbage").is_none());
    }

    #[test]
    fn hex_decoding() {
        assert_eq!(decode_hex("7b7d").unwrap(), b"{}");
        assert!(decode_hex("xyz").is_none());
    }

    #[test]
    fn parses_profile() {
        let p = parse_profile(
            r#"{"hasCompletedOnboarding":true,"oauthAccount":{"emailAddress":"a@b.c","organizationName":"Org","billingType":"stripe_subscription"}}"#,
        );
        assert!(p.onboarded);
        assert_eq!(p.email.as_deref(), Some("a@b.c"));
        assert_eq!(p.org_name.as_deref(), Some("Org"));
        assert!(parse_profile("{}").email.is_none());
    }

    #[test]
    fn refuses_default_dir() {
        assert!(is_default_dir(&home_dir().join(".claude")));
        assert!(!is_default_dir(&home_dir().join(".godterm/accounts/x")));
    }
}
