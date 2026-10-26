//! The git state of every known project, so the assistant can answer
//! "are all my projects on GitHub?", "which ones are only local?",
//! "anything unpushed or uncommitted?" without asking a tab.
//!
//! For each project root (and each subproject that is a repo of its own)
//! the repo it is in gets a set of facts: branch, upstream, ahead and
//! behind, uncommitted counts, stashes, the last commit, the remotes
//! (credentials stripped, with their host kind), whether the branch is on
//! a remote, unpushed commits and when it was last fetched. Everything
//! comes from local refs ("as of the last fetch"); only an explicit
//! refresh the user asked for runs `git fetch`, in the background and
//! never prompting for credentials.
//!
//! Read only: every git call runs with --no-optional-locks (no index
//! refresh written), fsmonitor off and a short timeout, from a small
//! pool. Facts are cached in the project index with the mtimes of the
//! repo's HEAD, index and refs, and recomputed when one changes, when
//! they are older than the caller allows, or when a tab working in that
//! repo finishes a turn.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// One remote: its name, its URL with any credentials stripped, the kind
/// of host and the repo path on it ("acme/platform").
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    /// github, gitlab, bitbucket, other or local.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
    pub date: String,
    pub author: String,
}

/// What git says about one repo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Facts {
    /// The work tree's top level.
    pub top: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub branch: Option<String>,
    #[serde(default)]
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    #[serde(default)]
    pub staged: u32,
    #[serde(default)]
    pub modified: u32,
    #[serde(default)]
    pub untracked: u32,
    #[serde(default)]
    pub conflicts: u32,
    #[serde(default)]
    pub stash: u32,
    pub last: Option<Commit>,
    #[serde(default)]
    pub remotes: Vec<Remote>,
    /// The remote's default branch (from refs/remotes/<remote>/HEAD).
    pub default_branch: Option<String>,
    /// The current branch has a remote tracking ref of the same name.
    pub branch_on_remote: Option<bool>,
    /// The default branch exists locally and on the remote.
    pub default_on_remote: Option<bool>,
    /// Commits on HEAD that no remote ref has (capped at UNPUSHED_CAP).
    pub unpushed: Option<u32>,
    /// FETCH_HEAD's mtime, unix seconds: when it was last fetched.
    pub last_fetch: Option<i64>,
    /// The newest remote tracking ref file, unix seconds: what a clone
    /// or a push left when there was never a fetch.
    #[serde(default)]
    pub remote_refs_at: Option<i64>,
    pub error: Option<String>,
    /// Files whose (mtime ns, size) a change of shows up in.
    #[serde(default)]
    pub stamps: Vec<(PathBuf, u128, u64)>,
    /// When these facts were computed, unix seconds.
    #[serde(default)]
    pub computed: i64,
    #[serde(default)]
    pub took_ms: u64,
    /// Marked for a recompute (a tab in it finished a turn).
    #[serde(skip)]
    pub stale: bool,
}

/// rev-list counts stop here: "1000+" unpushed commits.
pub const UNPUSHED_CAP: u32 = 1000;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Facts older than this are recomputed even when no stamp changed
    /// (edits in the work tree change no git file).
    pub max_age: Duration,
    /// Per git command.
    pub per_cmd: Duration,
    /// For the whole update; repos left over keep their old facts.
    pub total: Duration,
    pub threads: usize,
}

impl Limits {
    /// The index schedule's background pass.
    pub fn background() -> Self {
        Limits {
            max_age: Duration::from_secs(600),
            per_cmd: Duration::from_secs(3),
            total: Duration::from_secs(20),
            threads: pool_size(),
        }
    }

    /// A tool call: fresh enough to answer about uncommitted work.
    pub fn tool() -> Self {
        Limits {
            max_age: Duration::from_secs(120),
            per_cmd: Duration::from_secs(3),
            total: Duration::from_secs(8),
            threads: pool_size(),
        }
    }
}

/// A small pool: half the cores, 2 to 6.
pub fn pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(2)
        .clamp(2, 6)
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn stamp(p: &Path) -> Option<(u128, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((t, m.len()))
}

// ---- finding the repo ----

/// The repo `p` is in: (top level, git dir, common dir), found on disk
/// (nearest ancestor with .git, never the home folder or above it).
pub fn locate(p: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let home = crate::config::home_dir();
    let mut cur = Some(p);
    while let Some(c) = cur {
        if home.starts_with(c) {
            return None;
        }
        let dot = c.join(".git");
        if let Ok(md) = std::fs::symlink_metadata(&dot) {
            let git_dir = if md.is_dir() {
                Some(dot)
            } else if md.is_file() {
                // A worktree or submodule: "gitdir: <path>".
                std::fs::read_to_string(&dot).ok().and_then(|t| {
                    let g = t
                        .lines()
                        .next()?
                        .strip_prefix("gitdir:")?
                        .trim()
                        .to_string();
                    let g = PathBuf::from(g);
                    Some(if g.is_absolute() { g } else { c.join(g) })
                })
            } else {
                None
            };
            if let Some(g) = git_dir.filter(|g| g.join("HEAD").exists()) {
                let common = std::fs::read_to_string(g.join("commondir"))
                    .ok()
                    .map(|t| {
                        let r = PathBuf::from(t.trim());
                        if r.is_absolute() {
                            r
                        } else {
                            g.join(r)
                        }
                    })
                    .map(|r| std::fs::canonicalize(&r).unwrap_or(r))
                    .unwrap_or_else(|| g.clone());
                return Some((c.to_path_buf(), g, common));
            }
        }
        cur = c.parent();
    }
    None
}

/// The repo's top level, if `p` is in one.
pub fn repo_top(p: &Path) -> Option<PathBuf> {
    locate(p).map(|(t, _, _)| t)
}

/// Files whose change means the facts may have changed.
fn stamp_paths(git_dir: &Path, common: &Path, branch: Option<&str>) -> Vec<PathBuf> {
    let mut v = vec![
        git_dir.join("HEAD"),
        git_dir.join("index"),
        git_dir.join("FETCH_HEAD"),
        common.join("FETCH_HEAD"),
        common.join("packed-refs"),
        common.join("config"),
        common.join("refs/heads"),
        common.join("refs/remotes"),
        common.join("refs/stash"),
        common.join("logs/refs/stash"),
    ];
    if let Some(b) = branch {
        v.push(common.join("refs/heads").join(b));
    }
    if let Ok(rd) = std::fs::read_dir(common.join("refs/remotes")) {
        let mut r: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        r.sort();
        v.extend(r.into_iter().take(20));
    }
    v
}

fn take_stamps(paths: &[PathBuf]) -> Vec<(PathBuf, u128, u64)> {
    paths
        .iter()
        .map(|p| {
            let (m, l) = stamp(p).unwrap_or((0, 0));
            (p.clone(), m, l)
        })
        .collect()
}

/// Nothing git keeps for this repo has changed since the facts.
pub fn unchanged(f: &Facts) -> bool {
    !f.stamps.is_empty()
        && f.stamps
            .iter()
            .all(|(p, m, l)| stamp(p).unwrap_or((0, 0)) == (*m, *l))
}

// ---- running git ----

/// The environment every GodTerm git call runs with: never prompting,
/// ssh in batch mode, plain C messages, no repo above home, none of the
/// caller's GIT_DIR. Tests never read the user's own git settings.
pub(crate) fn quiet_env(c: &mut Command) {
    c.env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("SSH_ASKPASS", "echo")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=10",
        )
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C")
        .env("GIT_CEILING_DIRECTORIES", crate::config::home_dir())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    if cfg!(test) {
        c.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
    }
}

/// A git command that can never write into the repo or prompt.
fn git(top: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("--no-optional-locks")
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.untrackedCache=false"])
        .args(["-c", "gc.auto=0"])
        .args(["-c", "maintenance.auto=false"])
        .arg("-C")
        .arg(top);
    quiet_env(&mut c);
    c
}

/// Run a prepared command: Ok((success, stdout, stderr)), killed at
/// `limit`. Polled from 1 ms up (git answers in a few ms), so a repo's
/// handful of calls costs little more than the calls themselves.
pub(crate) fn exec(
    mut c: Command,
    limit: Duration,
    input: Option<&[u8]>,
) -> Result<(bool, String, String), String> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    c.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    if let (Some(data), Some(mut si)) = (input, child.stdin.take()) {
        let _ = si.write_all(data);
    }
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    // Both pipes on their own threads so a full pipe cannot stall git.
    let reader = std::thread::spawn(move || {
        let mut v = vec![];
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut v);
        }
        v
    });
    let ereader = std::thread::spawn(move || {
        let mut v = vec![];
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut v);
        }
        v
    });
    let t0 = Instant::now();
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if t0.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                let _ = ereader.join();
                return Err(format!("timed out after {} ms", limit.as_millis()));
            }
            Ok(None) => {
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(15));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let stdout = reader.join().unwrap_or_default();
    let stderr = ereader.join().unwrap_or_default();
    Ok((
        status.success(),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

/// Run git; Ok(stdout) on success, Err(why) otherwise. Killed at `limit`.
fn run(top: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut c = git(top);
    c.args(args);
    let (ok, out, err) = exec(c, limit, None)?;
    if ok {
        Ok(out)
    } else {
        let e = err
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed");
        Err(crate::sessions::snippet(e.trim(), 160))
    }
}

// ---- remotes ----

/// A remote URL without credentials: "https://user:token@host/x" and
/// "https://token@host/x" become "https://host/x"; ssh keeps a plain user
/// name ("git@github.com:acme/x.git").
pub fn clean_url(url: &str) -> String {
    let url = url.trim();
    if let Some((scheme, rest)) = url.split_once("://") {
        let (auth, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if let Some((user, host)) = auth.rsplit_once('@') {
            let ssh = scheme.starts_with("ssh") || scheme.contains("+ssh");
            let keep = ssh && !user.contains(':');
            let mut out = format!("{scheme}://");
            if keep {
                out.push_str(user);
                out.push('@');
            }
            out.push_str(host);
            out.push_str(path);
            return strip_query(&out);
        }
        return strip_query(url);
    }
    // scp form: user@host:path (a password cannot be written here).
    if let Some((auth, rest)) = url.split_once(':') {
        if let Some((user, host)) = auth.rsplit_once('@') {
            if !host.contains('/') && !user.contains('/') {
                return format!("{user}@{host}:{rest}");
            }
        }
    }
    url.to_string()
}

fn strip_query(u: &str) -> String {
    u.split(['?', '#']).next().unwrap_or(u).to_string()
}

/// A local path remote (a folder or file:// URL, no host).
pub fn is_local(url: &str) -> bool {
    let u = url.trim();
    if u.starts_with("file://") || u.starts_with('/') || u.starts_with('.') || u.starts_with('~') {
        return true;
    }
    if u.contains("://") {
        return false;
    }
    // scp form needs a colon before any slash.
    match (u.find(':'), u.find('/')) {
        (Some(c), Some(s)) => s < c,
        (Some(_), None) => false,
        _ => true,
    }
}

/// (host kind, repo path) of a remote URL.
pub fn host_kind(url: &str) -> (&'static str, Option<String>) {
    if is_local(url) {
        return ("local", None);
    }
    let u = clean_url(url);
    let (host, path) = if let Some((_, rest)) = u.split_once("://") {
        let rest = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
        let rest = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        (rest.0.split(':').next().unwrap_or("").to_string(), rest.1)
    } else {
        let (auth, path) = u.split_once(':').unwrap_or((&u, ""));
        let host = auth.rsplit_once('@').map(|(_, h)| h).unwrap_or(auth);
        (host.to_string(), path)
    };
    let host = host.to_ascii_lowercase();
    let repo = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .trim_start_matches('~')
        .to_string();
    let repo = (!repo.is_empty()).then_some(repo);
    // Host aliases from ~/.ssh/config ("github.com-work") still count.
    let kind = if host.contains("github") {
        "github"
    } else if host.contains("gitlab") {
        "gitlab"
    } else if host.contains("bitbucket") {
        "bitbucket"
    } else {
        "other"
    };
    (kind, repo)
}

fn parse_remotes(cfg: &str) -> Vec<Remote> {
    let mut v: Vec<Remote> = vec![];
    for l in cfg.lines() {
        let Some((k, url)) = l.split_once(' ') else {
            continue;
        };
        let Some(name) = k
            .strip_prefix("remote.")
            .and_then(|r| r.strip_suffix(".url"))
        else {
            continue;
        };
        if v.iter().any(|r| r.name == name) {
            continue;
        }
        let (host, repo) = host_kind(url);
        v.push(Remote {
            name: name.to_string(),
            url: clean_url(url),
            host: host.to_string(),
            repo,
        });
    }
    // origin first.
    v.sort_by_key(|r| r.name != "origin");
    v
}

// ---- computing ----

/// Parse `git status --porcelain=v2 --branch --show-stash`.
fn parse_status(text: &str, f: &mut Facts) {
    for l in text.lines() {
        if let Some(h) = l.strip_prefix("# branch.head ") {
            if h == "(detached)" {
                f.detached = true;
            } else {
                f.branch = Some(h.to_string());
            }
        } else if let Some(u) = l.strip_prefix("# branch.upstream ") {
            f.upstream = Some(u.to_string());
        } else if let Some(ab) = l.strip_prefix("# branch.ab ") {
            let mut it = ab.split_whitespace();
            f.ahead = it
                .next()
                .and_then(|a| a.trim_start_matches('+').parse().ok());
            f.behind = it
                .next()
                .and_then(|b| b.trim_start_matches('-').parse().ok());
        } else if let Some(n) = l.strip_prefix("# stash ") {
            f.stash = n.trim().parse().unwrap_or(0);
        } else if l.starts_with("1 ") || l.starts_with("2 ") {
            let xy: Vec<char> = l[2..].chars().take(2).collect();
            if xy.first().is_some_and(|c| *c != '.') {
                f.staged += 1;
            }
            if xy.get(1).is_some_and(|c| *c != '.') {
                f.modified += 1;
            }
        } else if l.starts_with("u ") {
            f.conflicts += 1;
        } else if l.starts_with("? ") {
            f.untracked += 1;
        }
    }
}

/// Compute one repo's facts (read only, local refs only).
pub fn compute(top: &Path, git_dir: &Path, common: &Path, per_cmd: Duration) -> Facts {
    let t0 = Instant::now();
    let mut f = Facts {
        top: top.to_path_buf(),
        git_dir: git_dir.to_path_buf(),
        common_dir: common.to_path_buf(),
        computed: now_unix(),
        ..Default::default()
    };
    // Stamps first: a change during the run means a recompute next time.
    let pre = take_stamps(&stamp_paths(git_dir, common, None));
    match run(
        top,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--show-stash",
            "--untracked-files=normal",
        ],
        per_cmd,
    ) {
        Ok(t) => parse_status(&t, &mut f),
        Err(e) => {
            f.error = Some(e);
            // A broken repo still says where it pushes to.
            if let Ok(t) = run(
                top,
                &["config", "--get-regexp", r"^remote\..*\.url$"],
                per_cmd,
            ) {
                f.remotes = parse_remotes(&t);
            }
            f.stamps = pre;
            f.took_ms = t0.elapsed().as_millis() as u64;
            return f;
        }
    }
    if let Ok(t) = run(
        top,
        &["config", "--get-regexp", r"^remote\..*\.url$"],
        per_cmd,
    ) {
        f.remotes = parse_remotes(&t);
    }
    if let Ok(t) = run(
        top,
        &["log", "-1", "--format=%h%x1f%s%x1f%cI%x1f%an", "HEAD", "--"],
        per_cmd,
    ) {
        let p: Vec<&str> = t.trim_end_matches('\n').split('\x1f').collect();
        if p.len() == 4 {
            f.last = Some(Commit {
                hash: p[0].to_string(),
                subject: crate::sessions::snippet(p[1], 100),
                date: p[2].to_string(),
                author: p[3].to_string(),
            });
        }
    }
    let born = f.last.is_some();
    if !f.remotes.is_empty() {
        if let Ok(t) = run(
            top,
            &[
                "for-each-ref",
                "--format=%(refname)%09%(symref)",
                "refs/heads",
                "refs/remotes",
            ],
            per_cmd,
        ) {
            let mut heads: HashSet<String> = HashSet::new();
            let mut on_remote: HashSet<String> = HashSet::new();
            let mut defaults: Vec<(String, String)> = vec![];
            for l in t.lines() {
                let (r, sym) = l.split_once('\t').unwrap_or((l, ""));
                if let Some(h) = r.strip_prefix("refs/heads/") {
                    heads.insert(h.to_string());
                } else if let Some(rr) = r.strip_prefix("refs/remotes/") {
                    let Some((remote, b)) = rr.split_once('/') else {
                        continue;
                    };
                    if b == "HEAD" {
                        let target = sym
                            .strip_prefix(&format!("refs/remotes/{remote}/"))
                            .unwrap_or("")
                            .to_string();
                        if !target.is_empty() {
                            defaults.push((remote.to_string(), target));
                        }
                    } else {
                        on_remote.insert(b.to_string());
                    }
                }
            }
            defaults.sort_by_key(|(r, _)| r != "origin");
            f.default_branch = defaults.first().map(|(_, b)| b.clone()).or_else(|| {
                ["main", "master"]
                    .iter()
                    .find(|b| on_remote.contains(**b))
                    .map(|b| b.to_string())
            });
            if let Some(b) = &f.branch {
                f.branch_on_remote = Some(on_remote.contains(b));
            }
            if let Some(d) = &f.default_branch {
                f.default_on_remote = Some(heads.contains(d) && on_remote.contains(d));
            }
        }
        if born {
            let cap = format!("--max-count={UNPUSHED_CAP}");
            if let Ok(t) = run(
                top,
                &["rev-list", "--count", &cap, "HEAD", "--not", "--remotes"],
                per_cmd,
            ) {
                f.unpushed = t.trim().parse().ok();
            }
        }
    }
    f.last_fetch = [git_dir.join("FETCH_HEAD"), common.join("FETCH_HEAD")]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
        .max()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    if f.last_fetch.is_none() && !f.remotes.is_empty() {
        f.remote_refs_at = newest_mtime(&common.join("logs/refs/remotes"), 3, 200)
            .max(newest_mtime(&common.join("refs/remotes"), 3, 200));
    }
    // Re-stamp with the branch's own ref, keeping the earlier values for
    // the rest so a change mid run is noticed.
    let all = stamp_paths(git_dir, common, f.branch.as_deref());
    let mut st = pre;
    for p in all {
        if !st.iter().any(|(q, _, _)| *q == p) {
            let (m, l) = stamp(&p).unwrap_or((0, 0));
            st.push((p, m, l));
        }
    }
    f.stamps = st;
    f.took_ms = t0.elapsed().as_millis() as u64;
    f
}

/// The newest file mtime below `dir` (unix seconds), bounded.
fn newest_mtime(dir: &Path, depth: usize, cap: usize) -> Option<i64> {
    let mut best: Option<i64> = None;
    let mut todo = vec![(dir.to_path_buf(), 0)];
    let mut n = 0;
    while let Some((d, k)) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            n += 1;
            if n > cap {
                return best;
            }
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if k < depth {
                    todo.push((e.path(), k + 1));
                }
            } else if let Some(s) = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            {
                best = best.max(Some(s.as_secs() as i64));
            }
        }
    }
    best
}

// ---- the index ----

/// The folders whose repos matter: every root (and the repo it is in)
/// and every subproject found to be a repo of its own.
pub fn folders(ix: &crate::project_index::Index) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![];
    let mut roots: Vec<&PathBuf> = ix.roots.keys().collect();
    roots.sort();
    for r in roots {
        v.push(r.clone());
        if let Some(m) = ix.roots.get(r) {
            for e in m.entries.iter().filter(|e| e.git && !e.rel.is_empty()) {
                v.push(r.join(&e.rel));
            }
        }
    }
    v
}

#[derive(Debug, Clone, Default)]
pub struct UpdateStats {
    pub repos: usize,
    pub computed: usize,
    pub reused: usize,
    pub waiting: usize,
    pub took_ms: u128,
}

/// Bring the git facts up to date for `folders` (None: every folder the
/// index knows): recompute repos that changed, are stale or older than
/// `lim.max_age`; keep the rest. `force` recomputes all.
pub fn update(
    shared: &crate::project_index::Shared,
    only: Option<&[PathBuf]>,
    lim: &Limits,
    force: bool,
) -> UpdateStats {
    let t0 = Instant::now();
    let (dirs, old) = {
        let ix = shared.lock().unwrap_or_else(|e| e.into_inner());
        let dirs = match only {
            Some(o) => o.to_vec(),
            None => folders(&ix),
        };
        (dirs, ix.git.clone())
    };
    // Each folder to its repo, once per repo.
    let mut repos: Vec<(PathBuf, PathBuf, PathBuf)> = vec![];
    let mut seen = HashSet::new();
    for d in &dirs {
        if let Some(l) = locate(d) {
            if seen.insert(l.0.clone()) {
                repos.push(l);
            }
        }
    }
    let now = now_unix();
    let mut st = UpdateStats {
        repos: repos.len(),
        ..Default::default()
    };
    let todo: Vec<(PathBuf, PathBuf, PathBuf)> = repos
        .iter()
        .filter(|(top, _, _)| {
            let due = match old.get(top) {
                None => true,
                Some(f) => {
                    force
                        || f.stale
                        || now - f.computed >= lim.max_age.as_secs() as i64
                        || !unchanged(f)
                }
            };
            if !due {
                st.reused += 1;
            }
            due
        })
        .cloned()
        .collect();
    let queue = std::sync::Mutex::new(todo.into_iter());
    let done: std::sync::Mutex<Vec<Facts>> = std::sync::Mutex::new(vec![]);
    let threads = lim.threads.max(1);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                if t0.elapsed() > lim.total {
                    break;
                }
                let next = queue.lock().unwrap_or_else(|e| e.into_inner()).next();
                let Some((top, g, c)) = next else { break };
                let f = compute(&top, &g, &c, lim.per_cmd);
                done.lock().unwrap_or_else(|e| e.into_inner()).push(f);
            });
        }
    });
    st.waiting = queue.lock().unwrap_or_else(|e| e.into_inner()).len();
    let done = done.into_inner().unwrap_or_default();
    st.computed = done.len();
    {
        let mut ix = shared.lock().unwrap_or_else(|e| e.into_inner());
        for f in done {
            ix.git.insert(f.top.clone(), f);
        }
        if only.is_none() {
            // Repos no longer known go.
            let keep: HashSet<&PathBuf> = repos.iter().map(|(t, _, _)| t).collect();
            ix.git.retain(|k, _| keep.contains(k));
        }
        ix.git_checked = Some(Instant::now());
    }
    st.took_ms = t0.elapsed().as_millis();
    if st.computed > 0 {
        crate::project_index::save(shared);
        crate::log::info(&format!(
            "project git: {} repos ({} computed, {} unchanged, {} waiting) in {} ms",
            st.repos, st.computed, st.reused, st.waiting, st.took_ms
        ));
    }
    st
}

/// A tab worked in `dir`: its repo's facts are recomputed next time.
pub fn mark_stale(shared: &crate::project_index::Shared, dir: &Path) -> Option<PathBuf> {
    let top = repo_top(dir)?;
    let mut ix = shared.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = ix.git.get_mut(&top) {
        f.stale = true;
    }
    Some(top)
}

/// `git fetch` every repo with a remote (the user asked to check the
/// remotes now): in parallel, each with a timeout, never prompting.
/// Returns (fetched, failed with why).
pub fn fetch_all(tops: &[PathBuf], per_repo: Duration) -> (usize, Vec<(PathBuf, String)>) {
    let queue = std::sync::Mutex::new(tops.iter().cloned());
    let res: std::sync::Mutex<(usize, Vec<(PathBuf, String)>)> = Default::default();
    std::thread::scope(|s| {
        for _ in 0..pool_size().min(4) {
            s.spawn(|| loop {
                let next = queue.lock().unwrap_or_else(|e| e.into_inner()).next();
                let Some(top) = next else { break };
                let r = run(
                    &top,
                    &["fetch", "--quiet", "--all", "--no-recurse-submodules"],
                    per_repo,
                );
                let mut g = res.lock().unwrap_or_else(|e| e.into_inner());
                match r {
                    Ok(_) => g.0 += 1,
                    Err(e) => g.1.push((top, e)),
                }
            });
        }
    });
    res.into_inner().unwrap_or_default()
}

// ---- what to say ----

impl Facts {
    pub fn dirty(&self) -> bool {
        self.staged + self.modified + self.untracked + self.conflicts > 0
    }

    pub fn on_github(&self) -> bool {
        self.remotes.iter().any(|r| r.host == "github")
    }

    /// No remote, or only remotes that are folders on this machine.
    pub fn local_only(&self) -> bool {
        self.remotes.iter().all(|r| r.host == "local")
    }

    /// Work a remote does not have: commits ahead or unpushed, or a
    /// branch the remote has never seen.
    pub fn has_unpushed(&self) -> bool {
        self.ahead.unwrap_or(0) > 0
            || self.unpushed.unwrap_or(0) > 0
            || (!self.local_only() && self.last.is_some() && self.branch_on_remote == Some(false))
    }

    pub fn is_behind(&self) -> bool {
        self.behind.unwrap_or(0) > 0
    }

    /// "github acme/platform" or "no remote".
    pub fn remote_word(&self) -> String {
        match self.remotes.first() {
            None => "no remote".into(),
            Some(r) => {
                let mut s = match (&r.repo, r.host.as_str()) {
                    (_, "local") => format!(
                        "local folder {}",
                        Path::new(r.url.trim_end_matches('/'))
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| r.url.clone())
                    ),
                    (Some(p), h) => format!("{h} {p}"),
                    (None, h) => format!("{h} {}", r.url),
                };
                if r.name != "origin" {
                    s = format!("{s} ({})", r.name);
                }
                if self.remotes.len() > 1 {
                    s.push_str(&format!(" +{} more", self.remotes.len() - 1));
                }
                s
            }
        }
    }

    /// "2 unpushed, 3 modified, 1 untracked" ("clean" when nothing).
    pub fn state_word(&self) -> String {
        let mut v: Vec<String> = vec![];
        if let Some(e) = &self.error {
            return format!("git error: {e}");
        }
        if self.last.is_none() {
            v.push("no commits yet".into());
        }
        let ahead = self.ahead.unwrap_or(0);
        let unp = self.unpushed.unwrap_or(0).max(ahead);
        if unp > 0 {
            let plus = if unp >= UNPUSHED_CAP { "+" } else { "" };
            v.push(format!("{unp}{plus} unpushed"));
        }
        if !self.local_only()
            && self.last.is_some()
            && self.branch_on_remote == Some(false)
            && self.branch.is_some()
        {
            v.push("branch not on remote".into());
        }
        if let Some(b) = self.behind.filter(|b| *b > 0) {
            v.push(format!("{b} behind"));
        }
        for (n, w) in [
            (self.staged, "staged"),
            (self.modified, "modified"),
            (self.untracked, "untracked"),
            (self.conflicts, "conflicted"),
        ] {
            if n > 0 {
                v.push(format!("{n} {w}"));
            }
        }
        if self.stash > 0 {
            v.push(crate::control::plural(self.stash as usize, "stash"));
        }
        if v.is_empty() {
            "clean".into()
        } else {
            v.join(", ")
        }
    }

    pub fn branch_word(&self) -> String {
        match (&self.branch, self.detached) {
            (_, true) => format!(
                "detached at {}",
                self.last.as_ref().map(|c| c.hash.as_str()).unwrap_or("?")
            ),
            (Some(b), _) => b.clone(),
            (None, _) => "?".into(),
        }
    }

    /// "fetched 3 days ago" / "never fetched".
    pub fn fetch_word(&self) -> String {
        if self.remotes.is_empty() {
            return String::new();
        }
        match (self.last_fetch, self.remote_refs_at) {
            (Some(t), _) => format!("fetched {}", ago(now_unix() - t)),
            (None, Some(t)) => format!("remote refs from {}, never fetched", ago(now_unix() - t)),
            (None, None) => "never fetched".into(),
        }
    }

    /// One line: "master, github acme/x, 2 unpushed, 3 modified (as of
    /// the fetch 2 h ago)".
    pub fn line(&self) -> String {
        let mut s = format!(
            "{}, {}, {}",
            self.branch_word(),
            self.remote_word(),
            self.state_word()
        );
        let f = self.fetch_word();
        if !f.is_empty() {
            s.push_str(&format!(" ({f})"));
        }
        s
    }

    pub fn json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "repo": crate::config::tilde(&self.top),
            "branch": self.branch_word(),
            "upstream": self.upstream,
            "ahead": self.ahead,
            "behind": self.behind,
            "staged": self.staged,
            "modified": self.modified,
            "untracked": self.untracked,
            "stash": self.stash,
            "unpushed": self.unpushed,
            "branch_on_remote": self.branch_on_remote,
            "default_branch": self.default_branch,
            "default_on_remote": self.default_on_remote,
            "remotes": self.remotes,
            "last_commit": self.last,
            "last_fetch": self.last_fetch.map(|t| ago(now_unix() - t)),
            "remote_refs_from": self.remote_refs_at.filter(|_| self.last_fetch.is_none()).map(|t| ago(now_unix() - t)),
            "on_github": self.on_github(),
            "local_only": self.local_only(),
            "summary": self.line(),
        });
        if self.conflicts > 0 {
            v["conflicts"] = serde_json::json!(self.conflicts);
        }
        if let Some(e) = &self.error {
            v["error"] = serde_json::json!(e);
        }
        v
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else if s < 86400 {
        format!("{} h ago", s / 3600)
    } else {
        let d = s / 86400;
        format!("{d} day{} ago", if d == 1 { "" } else { "s" })
    }
}

/// The one line for `dir` from the cache ("not a git repo" when it is
/// in none; None when its repo has no facts yet). Never runs git.
pub fn line_for(ix: &crate::project_index::Index, dir: &Path) -> Option<String> {
    match repo_top(dir) {
        None => Some("not a git repo".into()),
        Some(top) => {
            let f = ix.git.get(&top)?;
            if top == dir {
                Some(f.line())
            } else {
                Some(format!(
                    "in repo {}: {}",
                    crate::config::tilde(&top),
                    f.line()
                ))
            }
        }
    }
}

/// One project row: a repo (with the known folders in it) or a folder
/// that is in no repo.
#[derive(Debug, Clone)]
pub struct Row {
    pub path: PathBuf,
    pub facts: Option<Facts>,
    /// Known folders inside this repo besides its top.
    pub inside: Vec<PathBuf>,
    /// For a non-repo folder: how many repos are below it.
    pub holds: usize,
}

impl Row {
    pub fn matches(&self, filter: &str) -> bool {
        match (filter, &self.facts) {
            ("all", _) | ("", _) => true,
            ("no_remote", None) => self.holds == 0,
            ("no_remote", Some(f)) => f.error.is_none() && f.local_only(),
            ("not_github", None) => self.holds == 0,
            ("not_github", Some(f)) => !f.on_github(),
            ("dirty", Some(f)) => f.dirty(),
            ("unpushed", Some(f)) => f.has_unpushed(),
            ("behind", Some(f)) => f.is_behind(),
            _ => false,
        }
    }
}

/// Every project as rows: repos once each, then the folders in no repo.
pub fn rows(ix: &crate::project_index::Index) -> Vec<Row> {
    let mut by_top: HashMap<PathBuf, Row> = HashMap::new();
    let mut plain: Vec<PathBuf> = vec![];
    for d in folders(ix) {
        match repo_top(&d) {
            Some(top) => {
                let r = by_top.entry(top.clone()).or_insert_with(|| Row {
                    path: top.clone(),
                    facts: ix.git.get(&top).cloned(),
                    inside: vec![],
                    holds: 0,
                });
                if d != top && !r.inside.contains(&d) {
                    r.inside.push(d);
                }
            }
            None => plain.push(d),
        }
    }
    let mut v: Vec<Row> = by_top.into_values().collect();
    for p in plain {
        // A folder of repos (~/code): say so instead of "not a repo".
        let holds = v.iter().filter(|r| r.path.starts_with(&p)).count();
        // A plain folder inside another plain folder is the same project.
        v.push(Row {
            path: p,
            facts: None,
            inside: vec![],
            holds,
        });
    }
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

/// Counts over every row, for the summary.
pub fn summary(rows: &[Row]) -> serde_json::Value {
    let repos: Vec<&Facts> = rows.iter().filter_map(|r| r.facts.as_ref()).collect();
    let pending = rows
        .iter()
        .filter(|r| r.facts.is_none() && repo_top(&r.path).is_some())
        .count();
    let not_repo = rows
        .iter()
        .filter(|r| r.facts.is_none() && r.holds == 0 && repo_top(&r.path).is_none())
        .count();
    let c = |p: &dyn Fn(&Facts) -> bool| repos.iter().filter(|f| p(f)).count();
    serde_json::json!({
        "projects": rows.len(),
        "git_repos": repos.len() + pending,
        "on_github": c(&|f| f.on_github()),
        "other_host_only": c(&|f| !f.on_github() && !f.local_only()),
        "no_remote": c(&|f| f.error.is_none() && f.remotes.is_empty()),
        "local_path_remote_only": c(&|f| f.error.is_none() && !f.remotes.is_empty() && f.local_only()),
        "not_a_git_repo": not_repo,
        "folders_of_repos": rows.iter().filter(|r| r.facts.is_none() && r.holds > 0).count(),
        "dirty": c(&|f| f.dirty()),
        "unpushed": c(&|f| f.has_unpushed()),
        "behind": c(&|f| f.is_behind()),
        "errors": c(&|f| f.error.is_some()),
        "not_checked_yet": pending,
    })
}

/// Rows as a compact aligned table.
pub fn table(rows: &[&Row]) -> String {
    let cells: Vec<[String; 4]> = rows
        .iter()
        .map(|r| {
            let path = crate::config::tilde(&r.path);
            match &r.facts {
                Some(f) => {
                    let mut st = f.state_word();
                    let fw = f.fetch_word();
                    if !fw.is_empty() {
                        st.push_str(&format!("; {fw}"));
                    }
                    [path, f.branch_word(), f.remote_word(), st]
                }
                None if r.holds > 0 => [
                    path,
                    "-".into(),
                    "-".into(),
                    format!(
                        "not a repo, holds {}",
                        crate::control::plural(r.holds, "repo")
                    ),
                ],
                None if repo_top(&r.path).is_some() => {
                    [path, "-".into(), "-".into(), "not checked yet".into()]
                }
                None => [path, "-".into(), "-".into(), "not a git repo".into()],
            }
        })
        .collect();
    let head = ["project", "branch", "remote", "state"].map(String::from);
    let mut w = [0usize; 3];
    for c in std::iter::once(&head).chain(cells.iter()) {
        for i in 0..3 {
            w[i] = w[i].max(c[i].chars().count()).min(WIDTH);
        }
    }
    let mut out = String::new();
    for c in std::iter::once(&head).chain(cells.iter()) {
        let mut line = String::new();
        for i in 0..3 {
            let s = tail(&c[i], WIDTH);
            line.push_str(&format!("{s:<width$}  ", width = w[i]));
        }
        line.push_str(&c[3]);
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// A table cell's widest.
const WIDTH: usize = 44;

/// The end of a long cell ("...code/alpha"): paths differ at the end.
fn tail(s: &str, n: usize) -> String {
    let k = s.chars().count();
    if k <= n {
        return s.to_string();
    }
    let t: String = s.chars().skip(k - (n - 3)).collect();
    format!("...{t}")
}

pub const FILTERS: &[&str] = &[
    "all",
    "no_remote",
    "not_github",
    "dirty",
    "unpushed",
    "behind",
];

#[cfg(test)]
#[path = "project_git_tests.rs"]
mod tests;
