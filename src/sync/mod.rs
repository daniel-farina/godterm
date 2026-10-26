//! GodTerm Sync, Phase 2: this machine's state backed up to a private
//! state repo (`<owner>/godterm-state`) plus monthly conversation repos
//! (`<owner>/godterm-convos-YYYY-MM`), and restored on another machine
//! (docs/SYNC.md sections 2, 3 and 5).
//!
//! The home is the source of truth: every sync fetches the state repo,
//! applies what other machines changed (keyed merges, append-only
//! transcripts, .env files), projects the home into the working copy
//! through an allowlist and pushes, never with force. A rejected push
//! fetches and runs again; nothing in the repos is ever rewritten.
//!
//! Everything takes a [`Ctx`] (home, GodTerm's home, config, machine), so
//! tests run against fixture homes and local bare repos (file://) that
//! stand in for GitHub.

pub mod chunks;
pub mod cli;
pub mod collect;
pub mod env;
pub mod files;
pub mod git;
pub mod lease;
pub mod merge;
pub mod paths;
pub mod plan;
pub mod portable;
pub mod projects;
pub mod restore;
pub mod secrets;
pub mod shards;
pub mod state_repo;
pub mod takeover;
pub mod tokens;
pub mod worker;

#[cfg(test)]
mod lease_tests;
#[cfg(test)]
mod setup_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ui_tests;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// `[sync]` in config.toml. The whole table is machine local: it is
/// never pushed and never applied from another machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SyncCfg {
    /// The background worker runs (set by `godterm sync init` / `clone`).
    pub enabled: bool,
    /// Sync is on but the background sync waits (Settings > Sync > Pause,
    /// the assistant's sync_pause). A sync the user asks for still runs;
    /// conversation leases keep working.
    pub paused: bool,
    /// GitHub owner of the state and conversation repos.
    pub owner: String,
    /// The gh account that reads and writes them.
    pub account: String,
    /// The state repo's name.
    pub state_repo: String,
    /// Conversation repos are `<convos_prefix>-YYYY-MM`.
    pub convos_prefix: String,
    /// "github", or a `file:///folder` whose `<owner>/<name>.git` bare
    /// repos stand in for GitHub (tests, a local mirror).
    pub remote: String,
    /// Sessions written in the last N days are synced (besides the hot
    /// set: open and closed tabs, the archive, jobs).
    pub window_days: u32,
    /// A new machine places sessions of the last N days at once.
    pub hot_days: u32,
    /// Seconds of quiet after a change before a sync.
    pub debounce_s: u64,
    /// Largest transcript chunk (MB).
    pub chunk_mb: u64,
    /// A conversation repo over this many GB starts a `-2` repo.
    pub shard_gb: u64,
    /// Phase 3 (leases): heartbeat and lease expiry, seconds.
    pub heartbeat_s: u64,
    pub lease_s: u64,
    /// New monthly conversation repos may be created without asking
    /// (the yes given at `sync init` covers them; always private).
    pub auto_create_shards: bool,
    /// Sessions synced although a rule would leave them out (vault
    /// mentions), by id.
    pub include_sessions: Vec<String>,
    /// Sessions never synced, by id.
    pub exclude_sessions: Vec<String>,
    /// More config keys kept on this machine only ("voice.tts_speed",
    /// "ui.*").
    pub machine_local: Vec<String>,
    /// Absolute paths outside the home, by name: a path under one is
    /// stored as `@name/rest` and placed under this machine's folder.
    pub paths: BTreeMap<String, String>,
}

impl Default for SyncCfg {
    fn default() -> Self {
        SyncCfg {
            enabled: false,
            paused: false,
            owner: String::new(),
            account: String::new(),
            state_repo: "godterm-state".into(),
            convos_prefix: "godterm-convos".into(),
            remote: "github".into(),
            window_days: 30,
            hot_days: 7,
            debounce_s: 5,
            chunk_mb: 25,
            shard_gb: 3,
            heartbeat_s: 120,
            lease_s: 600,
            auto_create_shards: false,
            include_sessions: vec![],
            exclude_sessions: vec![],
            machine_local: vec![],
            paths: BTreeMap::new(),
        }
    }
}

/// The vault: never read, never synced, also through symlinks.
pub const VAULT: &str = "/Volumes/Private";

/// This machine: a random id kept in `<app home>/machine.json`, never
/// synced and never copied by a restore.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Machine {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub first_seen: String,
}

impl Machine {
    pub fn path(app_home: &Path) -> PathBuf {
        app_home.join("machine.json")
    }

    /// This machine's id, made (and saved) the first time.
    pub fn load_or_new(app_home: &Path) -> Machine {
        if let Some(m) = std::fs::read(Self::path(app_home))
            .ok()
            .and_then(|b| serde_json::from_slice::<Machine>(&b).ok())
            .filter(|m| !m.id.is_empty())
        {
            return m;
        }
        let m = Machine {
            id: crate::session_ops::new_uuid(),
            label: host_label(),
            first_seen: chrono::Utc::now().to_rfc3339(),
        };
        let _ = std::fs::create_dir_all(app_home);
        let _ = crate::config::write_private(
            &Self::path(app_home),
            serde_json::to_vec_pretty(&m).unwrap_or_default(),
        );
        m
    }

    pub fn save(&self, app_home: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(app_home)?;
        crate::config::write_private(
            &Self::path(app_home),
            serde_json::to_vec_pretty(self).unwrap_or_default(),
        )
    }

    pub fn short(&self) -> &str {
        crate::paths::head(&self.id, 8)
    }

    pub fn name(&self) -> String {
        if self.label.is_empty() {
            self.short().to_string()
        } else {
            self.label.clone()
        }
    }
}

/// The host name, without ".local".
pub fn host_label() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname writes
        // a NUL terminated name into it.
        let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if r == 0 {
            let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let s = String::from_utf8_lossy(&buf[..n]).to_string();
            let s = s.trim_end_matches(".local").to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "this machine".into())
}

/// Changes pulled for a store the running app holds in memory (archive,
/// jobs): the app applies them to its own copy and saves, so a sync never
/// writes such a file under it. Keys to set (entry in the file's local
/// form) or remove (None).
#[derive(Debug, Clone, PartialEq)]
pub struct StoreApply {
    pub store: &'static str,
    pub set: BTreeMap<String, Option<serde_json::Value>>,
}

impl StoreApply {
    /// A store's file contents with these changes in place: changed
    /// entries where they were, new ones at the end, removed ones gone
    /// (every other field of the file kept).
    pub fn apply_to(&self, whole: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        let Some(s) = merge::STORES.iter().find(|s| s.name == self.store) else {
            return whole.clone();
        };
        let mut out = if whole.is_object() {
            whole.clone()
        } else {
            serde_json::json!({})
        };
        let mut done = BTreeSet::new();
        let mut list = vec![];
        for e in whole[s.list].as_array().into_iter().flatten() {
            match (s.key)(e) {
                Some(k) if self.set.contains_key(&k) => {
                    if let Some(Some(v)) = self.set.get(&k) {
                        list.push(v.clone());
                    }
                    done.insert(k);
                }
                _ => list.push(e.clone()),
            }
        }
        for (k, v) in &self.set {
            if let (false, Some(v)) = (done.contains(k), v) {
                list.push(v.clone());
            }
        }
        out[s.list] = Value::Array(list);
        out
    }
}

/// The running app, as a sync sees it.
pub struct AppLink {
    /// Sessions whose CLI runs in a tab here now (never extended,
    /// replaced or forked under it).
    pub running: Mutex<BTreeSet<String>>,
    /// Hand pulled store changes to the app; true once it saved them.
    #[allow(clippy::type_complexity)]
    pub apply: Box<dyn Fn(StoreApply) -> bool + Send + Sync>,
}

/// The stores the running app saves itself.
pub const APP_STORES: &[&str] = &["archive", "jobs"];

#[derive(Clone)]
pub struct Link(pub Arc<AppLink>);

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Link")
    }
}

impl Link {
    pub fn running(&self) -> BTreeSet<String> {
        self.0
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Everything a sync step needs, passed explicitly.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// The user's home (what `~` means in portable paths).
    pub home: PathBuf,
    /// GodTerm's home (`~/.godterm` or $GODTERM_HOME).
    pub app_home: PathBuf,
    pub cfg: crate::config::Config,
    pub machine: Machine,
    /// Folders nothing is collected from or restored into (the vault, keys,
    /// other private folders), canonical.
    pub never: Vec<PathBuf>,
    /// This program, for the git credential helper.
    pub exe: PathBuf,
    /// Milliseconds since 1970 (tests pin it).
    pub now_ms: i64,
    /// The running app, when the sync runs inside it.
    pub link: Option<Link>,
}

impl Ctx {
    /// The real one: the user's home and GodTerm's, the config on disk.
    pub fn from_env() -> anyhow::Result<Ctx> {
        let app_home = crate::config::app_home();
        let cfg = crate::config::Config::load_or_init()?;
        Ok(Ctx::new(crate::config::home_dir(), app_home, cfg))
    }

    pub fn new(home: PathBuf, app_home: PathBuf, cfg: crate::config::Config) -> Ctx {
        let machine = Machine::load_or_new(&app_home);
        let mut c = Ctx {
            never: vec![],
            home,
            app_home,
            cfg,
            machine,
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("godterm")),
            now_ms: chrono::Utc::now().timestamp_millis(),
            link: None,
        };
        c.never = never_dirs(&c.home, &c.app_home);
        c
    }

    pub fn sync(&self) -> &SyncCfg {
        &self.cfg.sync
    }

    /// The lease length in ms.
    pub fn lease_ms(&self) -> i64 {
        self.cfg.sync.lease_s.max(1) as i64 * 1000
    }

    /// `<app home>/sync`: working copies, bases, the ledger.
    pub fn sync_dir(&self) -> PathBuf {
        self.app_home.join("sync")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.sync_dir().join("state")
    }

    pub fn paths(&self) -> portable::PathMap {
        portable::PathMap::new(&self.home, &self.cfg.sync.paths)
    }

    pub fn remote(&self) -> git::Remote {
        git::Remote::from_cfg(&self.cfg.sync, &self.exe)
    }

    /// Each account's harness and config dir (Claude and Grok only: the
    /// harnesses whose sessions Phase 2 syncs).
    pub fn accounts(&self) -> Vec<Account> {
        self.cfg
            .accounts
            .iter()
            .filter_map(|a| {
                let provider = match a.harness() {
                    crate::harness::Harness::Claude => "claude",
                    crate::harness::Harness::Grok => "grok",
                    _ => return None,
                };
                let dir = match &a.config_dir {
                    Some(d) => self.expand(d),
                    None => self.app_home.join("accounts").join(&a.name),
                };
                Some(Account {
                    name: a.name.clone(),
                    provider,
                    dir,
                })
            })
            .collect()
    }

    /// `~/x` under this context's home.
    pub fn expand(&self, p: &str) -> PathBuf {
        if p == "~" {
            self.home.clone()
        } else if let Some(r) = p.strip_prefix("~/") {
            self.home.join(r)
        } else {
            PathBuf::from(p)
        }
    }

    /// Why `p` may not be synced (inside a never folder, also through a
    /// symlink), if so.
    pub fn refused(&self, p: &Path) -> Option<String> {
        paths::refused(p, &self.never)
    }
}

/// One account whose sessions are synced.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub name: String,
    /// "claude" or "grok".
    pub provider: &'static str,
    pub dir: PathBuf,
}

/// Folders never collected from and never restored into: the vault, the
/// key stores, GodTerm's own home (only the allowlist reads there) and
/// the main Claude Code and Grok installs.
pub fn never_dirs(home: &Path, app_home: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [
        ".ssh",
        ".aws",
        ".gnupg",
        ".config/gh",
        ".config/gcloud",
        ".kube",
        ".docker",
        ".azure",
        ".password-store",
        ".claude",
        ".grok",
        ".claudego",
        "Library/Keychains",
        "Library/Cookies",
    ]
    .iter()
    .map(|d| home.join(d))
    .collect();
    v.push(app_home.to_path_buf());
    v.push(PathBuf::from(VAULT));
    v.push(PathBuf::from("/Library/Keychains"));
    v.push(PathBuf::from("/private/var/db"));
    v.into_iter().map(|p| paths::real(&p)).collect()
}

/// When the sync was last seen (for status lines): "3s ago".
pub fn ago(ms: i64, now_ms: i64) -> String {
    let s = ((now_ms - ms).max(0) / 1000) as u64;
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

/// Write a file through a private temp file and a rename (parents made).
pub fn write_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.sync-tmp",
        path.file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    crate::config::write_private(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Hex SHA-256 of some bytes.
pub fn sha_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(b))
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
