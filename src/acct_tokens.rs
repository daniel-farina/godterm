//! Long-lived account tokens (`claude setup-token`): kept ONLY in the
//! macOS keychain (generic password, service `godterm-token`, account =
//! the GodTerm account's name), never in config, state, logs, the sync or
//! a WebSocket frame.
//!
//! - The token is caught from the setup-token tab's output by `Scrub`
//!   before the terminal parser sees it: the screen (the terminal UI, the
//!   web tap and every snapshot) only ever gets `sk-ant-` and dots.
//! - It is written with `security -i` (the command goes on stdin, so the
//!   token is never in a process's arguments) and read back only to hand
//!   it to the account's own headless processes (CLAUDE_CODE_OAUTH_TOKEN)
//!   or to the Mac's clipboard on the user's confirmed request.
//! - What may be shown: that one exists, when it was made and its last
//!   four characters (kept in the item's comment).
//!
//! Demo mode never touches the keychain: tokens there are a file in the
//! demo home (fake tokens of the fake CLI). Tests use their own service
//! name (GODTERM_TOKEN_SERVICE) and remove what they made.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

pub const SERVICE: &str = "godterm-token";
/// What a Claude long-lived token starts with.
pub const PREFIX: &str = "sk-ant-";
/// Shortest run after the prefix that counts as a token.
const MIN_BODY: usize = 30;

/// A token's public facts.
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    /// When it was stored (local time, "2026-10-26 14:03").
    pub created: Option<String>,
    pub last4: String,
}

/// The token shown masked: its kind and the last four characters.
pub fn mask(token: &str) -> String {
    format!("sk-ant-…{}", last4(token))
}

pub fn last4(token: &str) -> String {
    let c: Vec<char> = token.chars().collect();
    c[c.len().saturating_sub(4)..].iter().collect()
}

fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// A whole string is a plausible token (what is stored).
pub fn plausible(t: &str) -> bool {
    t.starts_with(PREFIX)
        && t.len() >= PREFIX.len() + MIN_BODY
        && t.len() <= 400
        && t.bytes().all(token_char)
}

/// Every `sk-ant-` run in some text masked (logs, a screen).
pub fn mask_all(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(at) = text[i..].find(PREFIX) {
        let start = i + at;
        out.push_str(&text[i..start]);
        let mut end = start + PREFIX.len();
        while end < b.len() && token_char(b[end]) {
            end += 1;
        }
        if end - start - PREFIX.len() >= 8 {
            out.push_str(&mask(&text[start..end]));
        } else {
            out.push_str(&text[start..end]);
        }
        i = end;
    }
    out.push_str(&text[i..]);
    out
}

// ---------- the output filter ----------

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum Esc {
    #[default]
    None,
    /// Just saw ESC.
    Start,
    Csi,
    Osc,
    /// ESC inside an OSC (its ST is ESC \).
    OscSt,
}

/// Takes long-lived tokens out of a PTY stream as it is read: each token
/// character becomes a dot, and the token is kept for `take`. A token the
/// CLI wrapped onto the next lines (newlines and colors between its
/// pieces) is followed across them; a blank line, a space, a cursor move
/// or anything that is not a token character ends it.
#[derive(Debug, Default)]
pub struct Scrub {
    /// Bytes that may begin the prefix, held to the next read.
    hold: Vec<u8>,
    capturing: bool,
    cur: String,
    newlines: u8,
    esc: Esc,
    caught: Vec<String>,
}

const DOT: &[u8] = "•".as_bytes();

impl Scrub {
    pub fn new() -> Scrub {
        Scrub::default()
    }

    /// Filter one read into `out`.
    pub fn filter(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            self.byte(b, out);
        }
    }

    /// The stream ended: what was held goes out, a token in progress ends.
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.hold);
        self.end();
    }

    /// A token caught since the last call (each once).
    pub fn take(&mut self) -> Option<String> {
        if self.caught.is_empty() {
            None
        } else {
            Some(self.caught.remove(0))
        }
    }

    fn end(&mut self) {
        if self.capturing {
            self.capturing = false;
            let t = std::mem::take(&mut self.cur);
            if plausible(&t) && !self.caught.contains(&t) {
                self.caught.push(t);
            }
        }
        self.newlines = 0;
    }

    fn byte(&mut self, b: u8, out: &mut Vec<u8>) {
        if !self.capturing {
            let p = PREFIX.as_bytes();
            if p.get(self.hold.len()) == Some(&b) {
                self.hold.push(b);
                if self.hold.len() == p.len() {
                    out.append(&mut self.hold);
                    self.capturing = true;
                    self.cur = PREFIX.to_string();
                    self.newlines = 0;
                    self.esc = Esc::None;
                }
                return;
            }
            out.append(&mut self.hold);
            if b == p[0] {
                self.hold.push(b);
            } else {
                out.push(b);
            }
            return;
        }
        // Inside an escape sequence: passed on whole.
        match self.esc {
            Esc::Start => {
                self.esc = match b {
                    b'[' => Esc::Csi,
                    b']' => Esc::Osc,
                    _ => Esc::None,
                };
                out.push(b);
                return;
            }
            Esc::Csi => {
                out.push(b);
                if (0x40..=0x7e).contains(&b) {
                    self.esc = Esc::None;
                    // Colors keep the token going; a cursor move ends it.
                    if b != b'm' {
                        self.end();
                    }
                }
                return;
            }
            Esc::Osc => {
                out.push(b);
                if b == 0x07 {
                    self.esc = Esc::None;
                } else if b == 0x1b {
                    self.esc = Esc::OscSt;
                }
                return;
            }
            Esc::OscSt => {
                out.push(b);
                self.esc = Esc::None;
                return;
            }
            Esc::None => {}
        }
        match b {
            0x1b => {
                self.esc = Esc::Start;
                out.push(b);
            }
            b'\r' => out.push(b),
            b'\n' => {
                self.newlines = self.newlines.saturating_add(1);
                out.push(b);
                if self.newlines >= 2 {
                    self.end();
                }
            }
            b' ' if self.newlines > 0 => out.push(b),
            _ if token_char(b) => {
                self.newlines = 0;
                if self.cur.len() < 400 {
                    self.cur.push(b as char);
                }
                out.extend_from_slice(DOT);
            }
            _ => {
                self.end();
                // Not part of the token: read again as plain output.
                self.byte(b, out);
            }
        }
    }
}

// ---------- the store ----------

/// Where tokens live.
#[derive(Debug, Clone)]
pub enum Store {
    /// The login keychain, through `security`.
    Keychain { service: String },
    /// Demo mode: a 0600 file of fake tokens in the demo home.
    DemoFile(PathBuf),
}

/// The store for this run: the keychain (service `godterm-token`, or
/// GODTERM_TOKEN_SERVICE for a test), or the demo's file.
pub fn store() -> Store {
    if crate::demo::active() {
        return Store::DemoFile(crate::config::app_home().join("demo-tokens.json"));
    }
    Store::Keychain {
        service: crate::config::env_var("TOKEN_SERVICE").unwrap_or_else(|| SERVICE.to_string()),
    }
}

/// An account name as a keychain account (GodTerm's names are letters,
/// digits, - _ . already; anything else is refused).
fn safe_name(account: &str) -> Result<&str, String> {
    if !account.is_empty()
        && account.len() <= 64
        && account
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        Ok(account)
    } else {
        Err("not a valid account name".into())
    }
}

fn run_security(args: &[&str], stdin: Option<&str>) -> Result<std::process::Output, String> {
    let mut c = Command::new("security");
    c.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("security: {e}"))?;
    if let (Some(s), Some(mut w)) = (stdin, child.stdin.take()) {
        use std::io::Write;
        let _ = w.write_all(s.as_bytes());
    }
    // `security` can wait on a locked keychain's password dialog.
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < Duration::from_secs(20) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the keychain did not answer (locked?)".into());
            }
        }
    }
    child
        .wait_with_output()
        .map_err(|e| format!("security: {e}"))
}

fn demo_read(p: &PathBuf) -> serde_json::Value {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// "20261026043743Z" (a keychain timedate) as local "2026-10-26 04:37".
fn keychain_date(s: &str) -> Option<String> {
    let t = chrono::NaiveDateTime::parse_from_str(s.trim_end_matches('Z'), "%Y%m%d%H%M%S").ok()?;
    let utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(t, chrono::Utc);
    Some(
        utc.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
    )
}

/// The attributes `security find-generic-password` prints: (cdat, icmt).
pub fn parse_attrs(text: &str) -> (Option<String>, Option<String>) {
    let mut cdat = None;
    let mut icmt = None;
    for l in text.lines() {
        let l = l.trim();
        if let Some(r) = l.strip_prefix("\"cdat\"<timedate>=") {
            // 0x3230...  "20261026043743Z\000"
            if let Some(q) = r.split('"').nth(1) {
                cdat = Some(q.trim_end_matches("\\000").to_string());
            }
        } else if let Some(r) = l.strip_prefix("\"icmt\"<blob>=\"") {
            icmt = Some(r.trim_end_matches('"').to_string());
        }
    }
    (cdat, icmt)
}

impl Store {
    /// Save (or replace) `account`'s token.
    pub fn put(&self, account: &str, token: &str) -> Result<(), String> {
        let account = safe_name(account)?;
        if !plausible(token) {
            return Err("that is not a long-lived token".into());
        }
        match self {
            Store::Keychain { service } => {
                let cmd = format!(
                    "add-generic-password -U -s {service} -a {account} -l \"GodTerm token ({account})\" -j \"last4={}\" -w {token}\n",
                    last4(token)
                );
                let o = run_security(&["-i"], Some(&cmd))?;
                if !o.status.success() {
                    return Err("the keychain refused it".into());
                }
                // `security -i` says nothing on success; check it is there.
                if self.meta(account).is_none() {
                    return Err("the keychain did not keep it".into());
                }
                Ok(())
            }
            Store::DemoFile(p) => {
                if let Some(d) = p.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let mut v = demo_read(p);
                v[account] = serde_json::json!({
                    "token": token,
                    "created": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
                });
                crate::config::write_private(p, v.to_string()).map_err(|e| e.to_string())
            }
        }
    }

    /// The token itself, for the account's own processes or the clipboard.
    pub fn get(&self, account: &str) -> Option<String> {
        let account = safe_name(account).ok()?;
        match self {
            Store::Keychain { service } => {
                let o = run_security(
                    &["find-generic-password", "-s", service, "-a", account, "-w"],
                    None,
                )
                .ok()?;
                if !o.status.success() {
                    return None;
                }
                let t = String::from_utf8(o.stdout).ok()?.trim().to_string();
                plausible(&t).then_some(t)
            }
            Store::DemoFile(p) => demo_read(p)[account]["token"].as_str().map(str::to_string),
        }
    }

    /// When it was made and its last four characters (no secret is read).
    pub fn meta(&self, account: &str) -> Option<Meta> {
        let account = safe_name(account).ok()?;
        match self {
            Store::Keychain { service } => {
                let o = run_security(
                    &["find-generic-password", "-s", service, "-a", account],
                    None,
                )
                .ok()?;
                if !o.status.success() {
                    return None;
                }
                let (cdat, icmt) = parse_attrs(&String::from_utf8_lossy(&o.stdout));
                Some(Meta {
                    created: cdat.as_deref().and_then(keychain_date),
                    last4: icmt
                        .as_deref()
                        .and_then(|c| c.strip_prefix("last4="))
                        .unwrap_or("????")
                        .chars()
                        .take(4)
                        .collect(),
                })
            }
            Store::DemoFile(p) => {
                let v = demo_read(p);
                let t = v[account]["token"].as_str()?;
                Some(Meta {
                    created: v[account]["created"].as_str().map(str::to_string),
                    last4: last4(t),
                })
            }
        }
    }

    /// Remove it. Ok(false): there was none.
    pub fn delete(&self, account: &str) -> Result<bool, String> {
        let account = safe_name(account)?;
        match self {
            Store::Keychain { service } => {
                if self.meta(account).is_none() {
                    return Ok(false);
                }
                let o = run_security(
                    &["delete-generic-password", "-s", service, "-a", account],
                    None,
                )?;
                if o.status.success() {
                    Ok(true)
                } else {
                    Err("the keychain did not remove it".into())
                }
            }
            Store::DemoFile(p) => {
                let mut v = demo_read(p);
                let had = v.as_object_mut().and_then(|o| o.remove(account)).is_some();
                crate::config::write_private(p, v.to_string()).map_err(|e| e.to_string())?;
                Ok(had)
            }
        }
    }
}

// ---------- what each token is set to do ----------

/// `<app home>/account-tokens.json`: per account choices about its token
/// (use it for the assistant, sync it), when it was saved, a short hash
/// to tell tokens apart, where a synced one came from, and tombstones of
/// synced tokens deleted here. Never the token. Never synced itself
/// (sync::collect::NEVER_FILES).
pub const USE_FILE: &str = "account-tokens.json";

/// A synced token deleted here: deleted on every machine at the next sync.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tomb {
    pub account: String,
    pub sha: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Prefs {
    /// Accounts whose assistant runs with its token.
    #[serde(default, rename = "use")]
    pub use_for: Vec<String>,
    /// "Sync tokens to my other machines" (off by default): without it no
    /// token goes to the state repo or comes from it.
    #[serde(default)]
    pub sync_all: bool,
    /// Accounts whose token syncs (with `sync_all`).
    #[serde(default)]
    pub sync: Vec<String>,
    /// When each token was saved here (ms).
    #[serde(default)]
    pub at: std::collections::BTreeMap<String, i64>,
    /// sha256 of each token, first 16 hex: which token it is.
    #[serde(default)]
    pub sha: std::collections::BTreeMap<String, String>,
    /// A token restored from the sync: the machine it came from.
    #[serde(default)]
    pub from: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub tombstones: Vec<Tomb>,
}

pub fn sha16(token: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Prefs {
    pub fn path(app_home: &std::path::Path) -> PathBuf {
        app_home.join(USE_FILE)
    }

    pub fn load(app_home: &std::path::Path) -> Prefs {
        std::fs::read_to_string(Self::path(app_home))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, app_home: &std::path::Path) -> Result<(), String> {
        let _ = std::fs::create_dir_all(app_home);
        crate::config::write_private(
            &Self::path(app_home),
            serde_json::to_string_pretty(self).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())
    }

    /// A token was saved for `account` (made here or restored).
    pub fn saved(&mut self, account: &str, token: &str, at_ms: i64, from: Option<&str>) {
        self.at.insert(account.into(), at_ms);
        self.sha.insert(account.into(), sha16(token));
        match from {
            Some(m) => {
                self.from.insert(account.into(), m.into());
            }
            None => {
                self.from.remove(account);
            }
        }
        self.tombstones.retain(|t| t.account != account);
    }

    /// The token was deleted: a synced one leaves a tombstone.
    pub fn deleted(&mut self, account: &str, now_ms: i64) {
        let synced = self.sync.iter().any(|a| a == account);
        if let (true, Some(sha)) = (synced, self.sha.get(account).cloned()) {
            self.tombstones.retain(|t| t.account != account);
            self.tombstones.push(Tomb {
                account: account.into(),
                sha,
                at_ms: now_ms,
            });
            self.tombstones.truncate(50);
        }
        self.forget(account);
    }

    /// Everything about `account`'s token but its tombstone.
    pub fn forget(&mut self, account: &str) {
        self.use_for.retain(|a| a != account);
        self.sync.retain(|a| a != account);
        self.at.remove(account);
        self.sha.remove(account);
        self.from.remove(account);
    }

    pub fn syncs(&self, account: &str) -> bool {
        self.sync_all && self.sync.iter().any(|a| a == account)
    }
}

fn here() -> PathBuf {
    crate::config::app_home()
}

pub fn prefs() -> Prefs {
    Prefs::load(&here())
}

pub fn edit_prefs(f: impl FnOnce(&mut Prefs)) -> Result<(), String> {
    let mut p = prefs();
    f(&mut p);
    p.save(&here())
}

pub fn uses_token(account: &str) -> bool {
    prefs().use_for.iter().any(|a| a == account)
}

pub fn set_uses_token(account: &str, on: bool) -> Result<(), String> {
    edit_prefs(|p| {
        p.use_for.retain(|a| a != account);
        if on {
            p.use_for.push(account.to_string());
        }
    })
}

/// The environment for `account`'s headless processes: its token when it
/// has one and is set to use it.
pub fn env_for(account: &str) -> Option<(String, String)> {
    if !uses_token(account) {
        return None;
    }
    store()
        .get(account)
        .map(|t| ("CLAUDE_CODE_OAUTH_TOKEN".to_string(), t))
}

/// Put the token on the Mac's clipboard (the user confirmed it).
pub fn to_clipboard(token: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        *TEST_CLIP.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.to_string());
        Ok(())
    }
    #[cfg(not(test))]
    pbcopy(token)
}

#[cfg(not(test))]
fn pbcopy(token: &str) -> Result<(), String> {
    let mut c = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pbcopy: {e}"))?;
    if let Some(mut w) = c.stdin.take() {
        use std::io::Write;
        w.write_all(token.as_bytes()).map_err(|e| e.to_string())?;
    }
    c.wait().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
pub static TEST_CLIP: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
#[cfg_attr(not(unix), allow(unused_imports))]
pub use tests::FAKE as TEST_TOKEN;

#[cfg(test)]
mod tests {
    use super::*;

    pub const FAKE: &str = "sk-ant-oat01-FAKEtoken_for_tests_only-0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUV-wxyzAA";

    fn run(s: &mut Scrub, chunks: &[&[u8]]) -> Vec<u8> {
        let mut out = vec![];
        for c in chunks {
            s.filter(c, &mut out);
        }
        s.flush(&mut out);
        out
    }

    #[test]
    fn masks() {
        assert_eq!(mask(FAKE), "sk-ant-…yzAA");
        assert_eq!(last4(FAKE), "yzAA");
        assert!(plausible(FAKE));
        assert!(!plausible("sk-ant-short"));
        assert!(!plausible(&format!("{FAKE} x")));
        let m = mask_all(&format!("token {FAKE} done and sk-ant- alone"));
        assert!(!m.contains("FAKEtoken"), "{m}");
        assert!(
            m.contains("sk-ant-…yzAA") && m.ends_with("sk-ant- alone"),
            "{m}"
        );
    }

    #[test]
    fn scrub_catches_a_token_on_one_line() {
        let mut s = Scrub::new();
        let text = format!(
            "\x1b[32m✓ Long-lived authentication token created successfully!\x1b[39m\r\n\r\nYour OAuth token (valid for 1 year):\r\n\r\n\x1b[33m{FAKE}\x1b[39m\r\n\r\nStore this token securely."
        );
        let out = run(&mut s, &[text.as_bytes()]);
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains("FAKEtoken"), "{out}");
        assert!(out.contains("sk-ant-•••"), "{out}");
        assert!(out.contains("Store this token securely."), "{out}");
        assert_eq!(s.take().as_deref(), Some(FAKE));
        assert_eq!(s.take(), None);
    }

    #[test]
    fn scrub_follows_a_wrapped_token_split_across_reads() {
        // Wrapped at 40 columns the way Ink wraps (color off, CRLF, on).
        let mut text = String::from("\x1b[33m");
        for (i, ch) in FAKE.chars().enumerate() {
            if i > 0 && i % 40 == 0 {
                text.push_str("\x1b[39m\r\r\n\x1b[33m");
            }
            text.push(ch);
        }
        text.push_str("\x1b[39m\r\r\n\r\r\nStore this token securely.");
        // Every split point: one byte at a time.
        let bytes = text.as_bytes();
        let chunks: Vec<&[u8]> = bytes.chunks(1).collect();
        let mut s = Scrub::new();
        let out = String::from_utf8(run(&mut s, &chunks)).unwrap();
        assert!(
            !out.contains("FAKE") && !out.contains("0123456789"),
            "{out}"
        );
        assert!(out.contains("Store this token securely."));
        assert_eq!(s.take().as_deref(), Some(FAKE));
    }

    #[test]
    fn scrub_leaves_other_output_alone() {
        let raw = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/login/claude_setup_token_raw.bin"),
        )
        .unwrap();
        let mut s = Scrub::new();
        let out = run(&mut s, &raw.chunks(7).collect::<Vec<_>>());
        assert_eq!(out, raw);
        assert_eq!(s.take(), None);
        // A cursor move ends a token: the text after it is not eaten.
        let mut s = Scrub::new();
        let out = run(&mut s, &[format!("{FAKE}\x1b[2BStore").as_bytes()]);
        assert!(String::from_utf8(out).unwrap().ends_with("Store"));
        assert_eq!(s.take().as_deref(), Some(FAKE));
        // Words that only start like it pass.
        let mut s = Scrub::new();
        let out = run(&mut s, &[b"tasks skills sk-an sk-x"]);
        assert_eq!(out, b"tasks skills sk-an sk-x");
    }

    #[test]
    fn keychain_attrs_parse() {
        let t = "attributes:\n    \"acct\"<blob>=\"probe\"\n    \"cdat\"<timedate>=0x3230  \"20261026043743Z\\000\"\n    \"icmt\"<blob>=\"last4=wxyz\"\n";
        let (c, i) = parse_attrs(t);
        assert_eq!(c.as_deref(), Some("20261026043743Z"));
        assert_eq!(i.as_deref(), Some("last4=wxyz"));
        assert!(keychain_date("20261026043743Z")
            .unwrap()
            .starts_with("2026-10-2"));
    }

    /// A real keychain round trip under a test service of its own,
    /// removed at the end (macOS only; never the real service).
    #[test]
    #[cfg(target_os = "macos")]
    fn keychain_round_trip_with_a_test_service() {
        let service = format!("godterm-token-test-{}", std::process::id());
        assert_ne!(service, SERVICE);
        let st = Store::Keychain {
            service: service.clone(),
        };
        let name = "tok-test";
        let _ = st.delete(name);
        assert!(st.meta(name).is_none());
        if let Err(e) = st.put(name, FAKE) {
            // A locked keychain on a build machine: nothing was written.
            eprintln!("keychain unavailable: {e}");
            return;
        }
        let m = st.meta(name).unwrap();
        assert_eq!(m.last4, "yzAA");
        assert!(m.created.is_some());
        assert_eq!(st.get(name).as_deref(), Some(FAKE));
        assert_eq!(st.delete(name), Ok(true));
        assert!(st.meta(name).is_none() && st.get(name).is_none());
        assert_eq!(st.delete(name), Ok(false));
        // Bad names never reach `security`.
        assert!(st.put("a b", FAKE).is_err());
        assert!(st.put("x;rm", FAKE).is_err());
    }

    #[test]
    fn demo_file_store_and_use_flags() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let h = std::env::temp_dir().join(format!("godterm-tok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        crate::config::testing::set_home(&h);
        let p = crate::config::app_home().join("demo-tokens.json");
        let st = Store::DemoFile(p.clone());
        st.put("work", FAKE).unwrap();
        assert_eq!(st.meta("work").unwrap().last4, "yzAA");
        assert_eq!(st.get("work").as_deref(), Some(FAKE));
        assert_eq!(st.delete("work"), Ok(true));
        assert!(st.get("work").is_none());
        assert!(!uses_token("work"));
        set_uses_token("work", true).unwrap();
        assert!(uses_token("work") && !uses_token("other"));
        let f = std::fs::read_to_string(Prefs::path(&crate::config::app_home())).unwrap();
        assert!(!f.contains("sk-ant"), "names only: {f}");
        set_uses_token("work", false).unwrap();
        assert!(!uses_token("work"));
        let _ = std::fs::remove_dir_all(&h);
    }
}
