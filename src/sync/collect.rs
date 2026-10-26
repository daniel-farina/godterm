//! What the sync reads from this machine: an ALLOWLIST (docs/SYNC.md
//! 2.2). Only the files named here are ever read; everything else in
//! GodTerm's home and the account folders (logins, `.claude.json`, shell
//! snapshots, session-env, tokens, logs, caches) is never looked at. Every
//! project path is canonicalized (symlinks too) and refused inside the
//! vault or another never folder.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

use super::{Account, Ctx};

/// Files under GodTerm's home that are read, and what they become.
pub const HOME_FILES: &[(&str, &str)] = &[
    ("config.toml", "config/portable.toml"),
    ("archive.json", "archive.json"),
    ("jobs.json", "jobs.json"),
    ("projects.json", "projects.json"),
    ("assistant/learned.json", "learned.json"),
    ("state.json", "machines/<id>/state.json"),
    ("git-bindings.json", "git/bindings.json"),
];

/// Files under GodTerm's home that are never synced, whatever is added
/// above: secrets and what names them (the web token, the account
/// token list; the tokens themselves live only in the keychain).
pub const NEVER_FILES: &[&str] = &[
    "web-token",
    crate::acct_tokens::USE_FILE,
    "demo-tokens.json",
    "web-push-vapid.pk8",
    "web-push-subs.json",
];

/// Files and folders of a Claude account dir that are read.
pub const CLAUDE_FILES: &[&str] = &["settings.json", "CLAUDE.md"];
pub const CLAUDE_DIRS: &[&str] = &["skills", "commands", "agents"];

/// Biggest file copied as is (skills, memory, assistant conversations).
const MAX_FILE: u64 = 20 << 20;

/// One file of a file set: `area/key` in the state repo, `path` here.
#[derive(Debug, Clone, PartialEq)]
pub struct FileItem {
    pub area: String,
    pub key: String,
    pub path: PathBuf,
}

/// One conversation to sync.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRef {
    /// "claude" or "grok".
    pub provider: &'static str,
    pub account: String,
    pub id: String,
    /// Claude: the `<id>.jsonl`; Grok: the session folder.
    pub path: PathBuf,
    pub cwd: Option<PathBuf>,
    pub mtime_ms: i64,
    pub bytes: u64,
    /// In a tab, a closed tab, the archive or a job.
    pub hot: bool,
}

impl SessionRef {
    /// The files of the session, as (name, path): Claude has one, Grok
    /// its folder's files.
    pub fn files(&self) -> Vec<(String, PathBuf)> {
        if self.provider == "claude" {
            return vec![(format!("{}.jsonl", self.id), self.path.clone())];
        }
        let mut v = vec![];
        walk_files(&self.path, &self.path, 4, &mut v);
        v
    }
}

#[derive(Debug, Clone, Default)]
pub struct Collected {
    /// Present home files of HOME_FILES, by their name there.
    pub home_files: BTreeMap<String, PathBuf>,
    pub files: Vec<FileItem>,
    /// (account, history.jsonl).
    pub histories: Vec<(String, PathBuf)>,
    /// Memory folders: area -> portable cwd.
    pub memory_index: BTreeMap<String, String>,
    pub sessions: Vec<SessionRef>,
    /// What was left out, and why.
    pub skipped: Vec<(String, String)>,
}

impl Collected {
    /// Every local file this collection would read (the never-list test
    /// walks these).
    #[cfg(test)]
    pub fn sources(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.home_files.values().cloned().collect();
        v.extend(self.files.iter().map(|f| f.path.clone()));
        v.extend(self.histories.iter().map(|(_, p)| p.clone()));
        for s in &self.sessions {
            v.extend(s.files().into_iter().map(|(_, p)| p));
        }
        v.sort();
        v
    }

    /// The state repo paths it becomes (chunks and generated files
    /// aside).
    #[cfg(test)]
    pub fn repo_paths(&self) -> Vec<String> {
        let mut v: BTreeSet<String> = BTreeSet::new();
        for k in self.home_files.keys() {
            if let Some((_, r)) = HOME_FILES.iter().find(|(h, _)| h == k) {
                v.insert(r.to_string());
            }
        }
        for f in &self.files {
            v.insert(format!("{}/{}", f.area, f.key));
        }
        for (a, _) in &self.histories {
            v.insert(format!("claude/{a}/history.jsonl"));
        }
        for s in &self.sessions {
            v.insert(format!("sessions/{}/{}.json", s.account, s.id));
        }
        v.into_iter().collect()
    }
}

/// Regular files under `dir` (no symlinks, no dot folders, no secret
/// names), as (path relative to `root`, path).
fn walk_files(root: &Path, dir: &Path, depth: u32, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut ents: Vec<_> = rd.flatten().collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if md.file_type().is_symlink()
            || crate::guard::secret_name(&name)
            || name.ends_with(".sync-tmp")
        {
            continue;
        }
        if md.is_dir() {
            if depth > 0 && !name.starts_with('.') && name != "node_modules" {
                walk_files(root, &p, depth - 1, out);
            }
        } else if md.is_file() && md.len() <= MAX_FILE && name != ".DS_Store" {
            if let Ok(rel) = p.strip_prefix(root) {
                out.push((rel.to_string_lossy().replace('\\', "/"), p));
            }
        }
    }
}

fn mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The first `cwd` a Claude transcript names (its first 256 KB).
pub fn claude_cwd(p: &Path) -> Option<PathBuf> {
    let f = std::fs::File::open(p).ok()?;
    let r = std::io::BufReader::new(f.take(256 << 10));
    for l in r.split(b'\n').map_while(Result::ok) {
        if !l.windows(6).any(|w| w == b"\"cwd\":") {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&l) {
            if let Some(c) = v["cwd"].as_str() {
                return Some(PathBuf::from(c));
            }
        }
    }
    None
}

/// grok's session folder name back to the cwd (percent decoding).
pub fn grok_cwd(enc: &str) -> PathBuf {
    let b = enc.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&enc[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

/// Whether some bytes of a file name the vault.
pub fn mentions_vault(p: &Path, from: u64) -> bool {
    use std::io::Seek;
    let Ok(mut f) = std::fs::File::open(p) else {
        return false;
    };
    if f.seek(std::io::SeekFrom::Start(from)).is_err() {
        return false;
    }
    let needle = super::VAULT.as_bytes();
    let mut buf = vec![0u8; 1 << 20];
    let mut carry: Vec<u8> = vec![];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        let mut hay = std::mem::take(&mut carry);
        hay.extend_from_slice(&buf[..n]);
        if hay.windows(needle.len()).any(|w| w == needle) {
            return true;
        }
        let keep = needle.len().min(hay.len());
        carry = hay[hay.len() - keep..].to_vec();
    }
}

/// (account, session id) pairs in the hot set: open and closed tabs, the
/// archive, jobs.
pub fn hot_set(app_home: &Path) -> BTreeSet<(String, String)> {
    let mut v = BTreeSet::new();
    let read = |f: &str| -> serde_json::Value {
        std::fs::read(app_home.join(f))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    };
    let st = read("state.json");
    for s in st["slots"].as_array().into_iter().flatten() {
        let acct = s["account"].as_str().unwrap_or("").to_string();
        for t in s["tabs"].as_array().into_iter().flatten() {
            if let Some(id) = t["session_id"].as_str() {
                v.insert((acct.clone(), id.to_string()));
            }
        }
    }
    for c in st["closed_tabs"].as_array().into_iter().flatten() {
        if let (Some(a), Some(id)) = (c["account"].as_str(), c["session_id"].as_str()) {
            v.insert((a.to_string(), id.to_string()));
        }
    }
    for e in read("archive.json")["entries"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let (Some(a), Some(id)) = (e["account"].as_str(), e["session_id"].as_str()) {
            v.insert((a.to_string(), id.to_string()));
        }
    }
    for j in read("jobs.json")["jobs"].as_array().into_iter().flatten() {
        let t = &j["target"];
        if let (Some(a), Some(id)) = (t["account"].as_str(), t["session_id"].as_str()) {
            v.insert((a.to_string(), id.to_string()));
        }
    }
    v
}

/// Sessions of one account (no content read beyond the cwd).
pub fn account_sessions(
    ctx: &Ctx,
    a: &Account,
    hot: &BTreeSet<(String, String)>,
) -> (Vec<SessionRef>, Vec<(String, String)>) {
    let mut out = vec![];
    let mut skipped = vec![];
    let window_ms = ctx.sync().window_days as i64 * 86_400_000;
    let keep = |id: &str, mtime: i64| -> Option<bool> {
        if ctx.sync().exclude_sessions.iter().any(|x| x == id) {
            return None;
        }
        let is_hot = hot.contains(&(a.name.clone(), id.to_string()));
        (is_hot || ctx.now_ms - mtime <= window_ms).then_some(is_hot)
    };
    let check_cwd =
        |id: &str, cwd: &Option<PathBuf>, skipped: &mut Vec<(String, String)>| -> bool {
            let Some(c) = cwd else { return true };
            if super::paths::is_temp(c) {
                skipped.push((id.to_string(), "cwd in a temp folder".into()));
                return false;
            }
            if let Some(why) = ctx.refused(c) {
                skipped.push((id.to_string(), format!("cwd refused: {why}")));
                return false;
            }
            true
        };
    if a.provider == "claude" {
        let Ok(rd) = std::fs::read_dir(a.dir.join("projects")) else {
            return (out, skipped);
        };
        let mut dirs: Vec<_> = rd.flatten().collect();
        dirs.sort_by_key(|e| e.file_name());
        for d in dirs {
            let Ok(files) = std::fs::read_dir(d.path()) else {
                continue;
            };
            let mut files: Vec<_> = files.flatten().collect();
            files.sort_by_key(|e| e.file_name());
            for f in files {
                let p = f.path();
                if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(md) = std::fs::symlink_metadata(&p) else {
                    continue;
                };
                if !md.is_file() {
                    continue;
                }
                let id = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let mt = mtime_ms(&md);
                let Some(is_hot) = keep(&id, mt) else {
                    continue;
                };
                if crate::session_ids::is_bg_session(&a.dir, &id) {
                    continue;
                }
                let cwd = claude_cwd(&p);
                if !check_cwd(&id, &cwd, &mut skipped) {
                    continue;
                }
                out.push(SessionRef {
                    provider: "claude",
                    account: a.name.clone(),
                    id,
                    path: p,
                    cwd,
                    mtime_ms: mt,
                    bytes: md.len(),
                    hot: is_hot,
                });
            }
        }
    } else {
        let Ok(rd) = std::fs::read_dir(a.dir.join("sessions")) else {
            return (out, skipped);
        };
        let mut dirs: Vec<_> = rd.flatten().collect();
        dirs.sort_by_key(|e| e.file_name());
        for d in dirs {
            let enc = d.file_name().to_string_lossy().into_owned();
            let cwd = Some(grok_cwd(&enc));
            let Ok(ss) = std::fs::read_dir(d.path()) else {
                continue;
            };
            for s in ss.flatten() {
                let p = s.path();
                let Ok(md) = std::fs::symlink_metadata(&p) else {
                    continue;
                };
                if !md.is_dir() || !p.join("summary.json").is_file() {
                    continue;
                }
                let id = s.file_name().to_string_lossy().into_owned();
                let r = SessionRef {
                    provider: "grok",
                    account: a.name.clone(),
                    id: id.clone(),
                    path: p.clone(),
                    cwd: cwd.clone(),
                    mtime_ms: 0,
                    bytes: 0,
                    hot: false,
                };
                let files = r.files();
                let mt = files
                    .iter()
                    .filter_map(|(_, f)| std::fs::metadata(f).ok())
                    .map(|m| mtime_ms(&m))
                    .max()
                    .unwrap_or(0);
                let Some(is_hot) = keep(&id, mt) else {
                    continue;
                };
                if !check_cwd(&id, &cwd, &mut skipped) {
                    continue;
                }
                let bytes = files
                    .iter()
                    .filter_map(|(_, f)| std::fs::metadata(f).ok())
                    .map(|m| m.len())
                    .sum();
                out.push(SessionRef {
                    mtime_ms: mt,
                    bytes,
                    hot: is_hot,
                    ..r
                });
            }
        }
    }
    (out, skipped)
}

/// The whole allowlisted collection of this machine.
pub fn collect(ctx: &Ctx) -> Collected {
    let mut c = Collected::default();
    for (f, _) in HOME_FILES {
        if NEVER_FILES.contains(f) {
            continue;
        }
        let p = ctx.app_home.join(f);
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            c.home_files.insert(f.to_string(), p);
        }
    }
    // The assistant's saved conversations.
    let mut conv = vec![];
    walk_files(
        &ctx.app_home.join("assistant/conversations"),
        &ctx.app_home.join("assistant/conversations"),
        0,
        &mut conv,
    );
    for (rel, p) in conv {
        if rel.ends_with(".jsonl") {
            c.files.push(FileItem {
                area: "assistant/conversations".into(),
                key: rel,
                path: p,
            });
        }
    }
    let hot = hot_set(&ctx.app_home);
    let map = ctx.paths();
    for a in ctx.accounts() {
        if a.provider == "claude" {
            for f in CLAUDE_FILES {
                let p = a.dir.join(f);
                if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
                    c.files.push(FileItem {
                        area: format!("claude/{}", a.name),
                        key: f.to_string(),
                        path: p,
                    });
                }
            }
            for d in CLAUDE_DIRS {
                let mut v = vec![];
                walk_files(&a.dir, &a.dir.join(d), 6, &mut v);
                for (rel, p) in v {
                    c.files.push(FileItem {
                        area: format!("claude/{}", a.name),
                        key: rel,
                        path: p,
                    });
                }
            }
            let h = a.dir.join("history.jsonl");
            if std::fs::symlink_metadata(&h).is_ok_and(|m| m.is_file()) {
                c.histories.push((a.name.clone(), h));
            }
            // Claude's own memory per project folder.
            if let Ok(rd) = std::fs::read_dir(a.dir.join("projects")) {
                let mut dirs: Vec<_> = rd.flatten().collect();
                dirs.sort_by_key(|e| e.file_name());
                for d in dirs {
                    let mem = d.path().join("memory");
                    if !mem.is_dir() {
                        continue;
                    }
                    let cwd = std::fs::read_dir(d.path())
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
                        .find_map(|e| claude_cwd(&e.path()));
                    let Some(cwd) = cwd else {
                        c.skipped.push((
                            d.file_name().to_string_lossy().into_owned(),
                            "memory folder with no transcript naming its folder".into(),
                        ));
                        continue;
                    };
                    if let Some(why) = ctx.refused(&cwd) {
                        c.skipped.push((cwd.display().to_string(), why));
                        continue;
                    }
                    let port = map.to_portable(&cwd);
                    let area = format!("memory/{}/{}", a.name, super::portable::path_key(&port));
                    let mut v = vec![];
                    walk_files(&mem, &mem, 4, &mut v);
                    for (rel, p) in v {
                        c.files.push(FileItem {
                            area: area.clone(),
                            key: rel,
                            path: p,
                        });
                    }
                    c.memory_index.insert(area, port);
                }
            }
        }
        let (s, sk) = account_sessions(ctx, &a, &hot);
        c.sessions.extend(s);
        c.skipped.extend(sk);
    }
    c
}

/// Lines of a history.jsonl (bytes), for the line union merge.
pub fn read_lines(p: &Path) -> Vec<Vec<u8>> {
    let Ok(f) = std::fs::File::open(p) else {
        return vec![];
    };
    std::io::BufReader::new(f)
        .split(b'\n')
        .map_while(Result::ok)
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(test)]
mod never_files_tests {
    #[test]
    fn token_files_are_never_collected() {
        for (f, _) in super::HOME_FILES {
            assert!(!super::NEVER_FILES.contains(f), "{f} is on the never list");
            assert!(!f.contains("token"), "{f}");
        }
        assert!(super::NEVER_FILES.contains(&crate::acct_tokens::USE_FILE));
    }
}
