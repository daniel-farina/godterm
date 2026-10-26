//! Conversation ownership (docs/SYNC.md 4): one machine owns a session
//! at a time, through a lease on the state repo's `leases` branch.
//!
//! - `leases/<session-id>.json` = [`Lease`]: the owner's machine id, a
//!   generation bumped on every change of hands, a pending take over
//!   request and, once the owner let go, what it left behind (`released`:
//!   the transcript's length and SHA-256).
//! - `heartbeats/<machine-id>.json` = [`Beat`]: when that machine was last
//!   alive. A lease is live while its owner's heartbeat is younger than
//!   `lease_s` (10 min); heartbeats go out every `heartbeat_s` (2 min).
//!
//! Every change is a compare and swap: fetch `leases`, decide on what is
//! there, commit on top of it, and `git push` without rebase or force. A
//! push that is not a fast forward means another machine wrote first: the
//! loop fetches and decides again. GitHub's atomic ref update is the
//! lock. Time always comes in as an argument (`Ctx::now_ms`), so tests pin
//! the clock.
//!
//! The working repo is a small bare clone of its own
//! (`<app home>/sync/leases.git`, carrying the sync mark), apart from the
//! state working copy, so a lease never waits for a long sync.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::git::{self, Cred, Pushed};
use super::Ctx;

/// `leases/<session-id>.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub owner: String,
    pub gen: u64,
    pub acquired: String,
    #[serde(default)]
    pub takeover: Option<Takeover>,
    #[serde(default)]
    pub released: Option<Released>,
    /// The owner's label when it took the lease (for when its heartbeat
    /// is gone).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Takeover {
    pub by: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// What the owner left when it let go: the transcript as it pushed it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Released {
    pub len: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// The machine that asked for it, if one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// `heartbeats/<machine-id>.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Beat {
    pub at: String,
    #[serde(default)]
    pub label: String,
}

/// The `leases` branch as last read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub leases: BTreeMap<String, Lease>,
    pub beats: BTreeMap<String, Beat>,
    /// The commit it was read from.
    #[serde(default)]
    pub head: Option<String>,
    /// When it was fetched (ms; 0: never).
    #[serde(default)]
    pub fetched_ms: i64,
}

/// A session as seen from one machine.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// No lease: nobody claimed it yet.
    Free,
    /// Ours. `takeover`: another machine asks for it.
    Mine {
        gen: u64,
        takeover: Option<Takeover>,
    },
    /// Its owner let go (`by`, maybe us): free to take, and the transcript
    /// must reach `len` bytes with `sha256` before a resume.
    Released {
        by: String,
        len: u64,
        sha256: String,
        to: Option<String>,
    },
    /// Owned elsewhere, and that machine is alive.
    Live {
        owner: String,
        label: String,
        beat_ms: i64,
        takeover: Option<Takeover>,
    },
    /// Owned elsewhere, but its heartbeat is older than the lease: it
    /// never blocks anyone.
    Stale {
        owner: String,
        label: String,
        beat_ms: i64,
    },
}

impl Status {
    /// May this machine write the session (push its bytes)?
    pub fn writable(&self) -> bool {
        matches!(self, Status::Free | Status::Mine { .. })
    }
}

/// Milliseconds since 1970 of an RFC 3339 time (0 when unreadable).
pub fn ms_of(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// RFC 3339 of milliseconds since 1970.
pub fn rfc(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339()
}

/// A session id that is safe as a file name on the branch.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Board {
    /// When `machine` last beat (ms), if ever.
    pub fn beat_ms(&self, machine: &str) -> Option<i64> {
        self.beats.get(machine).map(|b| ms_of(&b.at))
    }

    pub fn label_of(&self, machine: &str) -> String {
        self.beats
            .get(machine)
            .map(|b| b.label.clone())
            .filter(|l| !l.is_empty())
            .or_else(|| {
                self.leases
                    .values()
                    .find(|l| l.owner == machine && !l.label.is_empty())
                    .map(|l| l.label.clone())
            })
            .unwrap_or_else(|| crate::paths::head(machine, 8).to_string())
    }

    /// Is `machine` alive at `now` (a heartbeat under `lease_ms` old)?
    pub fn alive(&self, machine: &str, now: i64, lease_ms: i64) -> bool {
        self.beat_ms(machine)
            .is_some_and(|b| now.saturating_sub(b) < lease_ms)
    }

    pub fn status(&self, sid: &str, me: &str, now: i64, lease_ms: i64) -> Status {
        let Some(l) = self.leases.get(sid) else {
            return Status::Free;
        };
        if let Some(r) = &l.released {
            return Status::Released {
                by: l.owner.clone(),
                len: r.len,
                sha256: r.sha256.clone(),
                to: r.to.clone(),
            };
        }
        if l.owner == me {
            return Status::Mine {
                gen: l.gen,
                takeover: l.takeover.clone(),
            };
        }
        let label = self.label_of(&l.owner);
        let beat_ms = self.beat_ms(&l.owner).unwrap_or(0);
        if self.alive(&l.owner, now, lease_ms) {
            Status::Live {
                owner: l.owner.clone(),
                label,
                beat_ms,
                takeover: l.takeover.clone(),
            }
        } else {
            Status::Stale {
                owner: l.owner.clone(),
                label,
                beat_ms,
            }
        }
    }
}

/// What a machine wants done with a session's lease.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Take it if nobody live holds it.
    Acquire,
    /// Ask the live owner to let go (or take it when free).
    Request,
    /// Take it whoever holds it (the user said yes).
    TakeOverNow,
    /// Let go of ours, leaving the transcript as pushed.
    Release { len: u64, sha256: String },
    /// Still ours? (a heartbeat's check)
    #[cfg_attr(not(test), allow(dead_code))]
    Keep,
}

/// How an operation went.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Ours now (or still). `prior` is what it was before.
    Acquired { gen: u64, prior: Status },
    /// Someone live holds it; nothing written.
    Held(Status),
    /// The request is on the lease.
    Requested(Status),
    /// Let go.
    Released,
    /// Not ours any more (`by` holds it, or it was released).
    Lost(Status),
    /// Ours, nothing to change (Keep), with any request on it.
    Kept { takeover: Option<Takeover> },
}

/// Decide one operation on the board as read: the lease to write (if
/// any) and the outcome. Pure: the clock and the machine come in.
pub fn decide(
    board: &Board,
    sid: &str,
    op: &Op,
    me: &str,
    label: &str,
    now: i64,
    lease_ms: i64,
) -> (Option<Lease>, Outcome) {
    let st = board.status(sid, me, now, lease_ms);
    let cur = board.leases.get(sid);
    let gen = cur.map(|l| l.gen).unwrap_or(0);
    let take = |prior: Status| {
        let l = Lease {
            owner: me.to_string(),
            gen: gen + 1,
            acquired: rfc(now),
            takeover: None,
            released: None,
            label: label.to_string(),
        };
        let g = l.gen;
        (Some(l), Outcome::Acquired { gen: g, prior })
    };
    match op {
        Op::Acquire | Op::Request => match &st {
            Status::Mine { gen, .. } => (
                None,
                Outcome::Acquired {
                    gen: *gen,
                    prior: st.clone(),
                },
            ),
            Status::Free | Status::Released { .. } | Status::Stale { .. } => take(st),
            Status::Live { takeover, .. } => {
                if *op == Op::Acquire {
                    return (None, Outcome::Held(st.clone()));
                }
                if takeover.as_ref().is_some_and(|t| t.by == me) {
                    return (None, Outcome::Requested(st.clone()));
                }
                let mut l = cur.cloned().unwrap_or_default();
                l.takeover = Some(Takeover {
                    by: me.to_string(),
                    at: rfc(now),
                    label: label.to_string(),
                });
                (Some(l), Outcome::Requested(st))
            }
        },
        Op::TakeOverNow => match &st {
            Status::Mine { gen, .. } => (
                None,
                Outcome::Acquired {
                    gen: *gen,
                    prior: st.clone(),
                },
            ),
            _ => take(st),
        },
        Op::Release { len, sha256 } => match (&st, cur) {
            (Status::Mine { takeover, .. }, Some(l)) => {
                let mut l = l.clone();
                l.released = Some(Released {
                    len: *len,
                    sha256: sha256.clone(),
                    at: rfc(now),
                    to: takeover.as_ref().map(|t| t.by.clone()),
                });
                l.takeover = None;
                (Some(l), Outcome::Released)
            }
            (Status::Released { by, .. }, _) if by == me => (None, Outcome::Released),
            (Status::Free, _) => (None, Outcome::Released),
            _ => (None, Outcome::Lost(st.clone())),
        },
        Op::Keep => match &st {
            Status::Mine { takeover, .. } => (
                None,
                Outcome::Kept {
                    takeover: takeover.clone(),
                },
            ),
            _ => (None, Outcome::Lost(st.clone())),
        },
    }
}

/// The leases repo: a bare clone of the state repo holding `leases` (and
/// `main`, for reading manifests without the state working copy).
pub struct Repo {
    pub dir: PathBuf,
    cred: Option<Cred>,
}

/// One prepared compare and swap, not pushed yet (tests interleave two).
pub struct Prepared {
    pub board: Board,
    pub outcomes: Vec<(String, Outcome)>,
    /// The commit to push, when anything changed.
    pub commit: Option<String>,
}

static INDEX_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Repo {
    /// Open (made the first time) `<app home>/sync/leases.git`.
    pub fn open(ctx: &Ctx) -> Result<Repo> {
        let d = ctx.sync_dir().join("leases.git");
        let r = ctx.remote();
        let url = r.url(&ctx.sync().owner, &ctx.sync().state_repo);
        if !git::is_sync_repo(&d) {
            std::fs::create_dir_all(ctx.sync_dir())?;
            let _ = std::fs::remove_dir_all(&d);
            let ds = d.to_string_lossy().into_owned();
            git::run(&ctx.sync_dir(), &["init", "-q", "--bare", &ds], None)?;
            git::mark(&d)?;
            git::run(&d, &["remote", "add", "origin", &url], None)?;
        } else {
            let cur =
                git::run(&d, &["config", "--get", "remote.origin.url"], None).unwrap_or_default();
            if cur.trim() != url {
                git::run(&d, &["config", "remote.origin.url", &url], None)?;
            }
        }
        Ok(Repo {
            dir: d,
            cred: r.cred(),
        })
    }

    /// Fetch `leases` and read it.
    pub fn fetch(&self, now: i64) -> Result<Board> {
        git::fetch(&self.dir, "leases", self.cred.as_ref())?;
        let mut b = self.read("refs/remotes/origin/leases")?;
        b.fetched_ms = now;
        Ok(b)
    }

    /// Fetch `main` (the manifests) into `refs/remotes/origin/main`.
    pub fn fetch_main(&self) -> Result<()> {
        git::fetch(&self.dir, "main", self.cred.as_ref())
    }

    /// A file of `main` as last fetched.
    pub fn main_file(&self, path: &str) -> Option<Vec<u8>> {
        let spec = format!("refs/remotes/origin/main:{path}");
        git::run_bytes(&self.dir, &["cat-file", "blob", &spec], None).ok()
    }

    /// The board at `rev` (empty when the branch does not exist yet).
    pub fn read(&self, rev: &str) -> Result<Board> {
        let Some(head) = git::rev(&self.dir, rev) else {
            return Ok(Board::default());
        };
        let ls = git::run(&self.dir, &["ls-tree", "-r", &head], None)?;
        let mut want: Vec<(String, String)> = vec![];
        for l in ls.lines() {
            let Some((meta, path)) = l.split_once('\t') else {
                continue;
            };
            let Some(sha) = meta.split_whitespace().nth(2) else {
                continue;
            };
            if path.ends_with(".json")
                && (path.starts_with("leases/") || path.starts_with("heartbeats/"))
            {
                want.push((path.to_string(), sha.to_string()));
            }
        }
        let blobs = self.cat_batch(&want.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>())?;
        let mut b = Board {
            head: Some(head),
            ..Default::default()
        };
        for ((path, _), bytes) in want.iter().zip(blobs) {
            let name = Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if path.starts_with("leases/") {
                if let Ok(l) = serde_json::from_slice::<Lease>(&bytes) {
                    b.leases.insert(name, l);
                }
            } else if let Ok(h) = serde_json::from_slice::<Beat>(&bytes) {
                b.beats.insert(name, h);
            }
        }
        Ok(b)
    }

    /// Many blobs in one `git cat-file --batch`.
    fn cat_batch(&self, shas: &[String]) -> Result<Vec<Vec<u8>>> {
        if shas.is_empty() {
            return Ok(vec![]);
        }
        let mut c = git::git(
            &self.dir,
            None,
            &["cat-file".to_string(), "--batch".to_string()],
        )?;
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn()?;
        let mut input = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let list = shas.join("\n") + "\n";
        let writer = std::thread::spawn(move || {
            let _ = input.write_all(list.as_bytes());
        });
        let out = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let mut r = BufReader::new(out);
        let mut v = vec![];
        for _ in shas {
            let mut head = String::new();
            r.read_line(&mut head)?;
            let parts: Vec<&str> = head.split_whitespace().collect();
            if parts.get(1) == Some(&"missing") || parts.len() < 3 {
                v.push(vec![]);
                continue;
            }
            let n: usize = parts[2].parse().context("cat-file size")?;
            let mut buf = vec![0u8; n + 1];
            r.read_exact(&mut buf)?;
            buf.truncate(n);
            v.push(buf);
        }
        let _ = writer.join();
        let _ = child.wait();
        Ok(v)
    }

    /// Fetch, decide every op, commit on top of what is there. Nothing is
    /// pushed yet.
    pub fn prepare(&self, ctx: &Ctx, ops: &[(String, Op)], beat: bool) -> Result<Prepared> {
        for (sid, _) in ops {
            if !valid_id(sid) {
                bail!("not a session id: {sid:?}");
            }
        }
        let now = ctx.now_ms;
        let lease_ms = ctx.sync().lease_s.max(1) as i64 * 1000;
        let me = ctx.machine.id.clone();
        let label = ctx.machine.name();
        let mut board = self.fetch(now)?;
        let mut writes: Vec<(String, Vec<u8>)> = vec![];
        let mut outcomes = vec![];
        let mut wrote_lease = false;
        for (sid, op) in ops {
            let (w, o) = decide(&board, sid, op, &me, &label, now, lease_ms);
            if let Some(l) = w {
                writes.push((format!("leases/{sid}.json"), serde_json::to_vec_pretty(&l)?));
                board.leases.insert(sid.clone(), l);
                wrote_lease = true;
            }
            outcomes.push((sid.clone(), o));
        }
        // A lease always goes out with a fresh heartbeat: an owner without
        // one would read as stale at once.
        if beat || wrote_lease {
            let hb = Beat {
                at: rfc(now),
                label: label.clone(),
            };
            writes.push((
                format!("heartbeats/{me}.json"),
                serde_json::to_vec_pretty(&hb)?,
            ));
            board.beats.insert(me.clone(), hb);
        }
        if writes.is_empty() {
            return Ok(Prepared {
                board,
                outcomes,
                commit: None,
            });
        }
        let commit = self.commit(board.head.as_deref(), &writes, &message(ops, &label))?;
        Ok(Prepared {
            board,
            outcomes,
            commit: Some(commit),
        })
    }

    fn commit(
        &self,
        base: Option<&str>,
        writes: &[(String, Vec<u8>)],
        msg: &str,
    ) -> Result<String> {
        let n = INDEX_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let idx = self
            .dir
            .join(format!("lease-index-{}-{n}", std::process::id()));
        let env = [("GIT_INDEX_FILE", idx.as_path())];
        let res = (|| -> Result<String> {
            let _ = std::fs::remove_file(&idx);
            if let Some(b) = base {
                git::run_env(&self.dir, &["read-tree", b], None, &env)?;
            }
            for (p, data) in writes {
                let blob = git::hash_object(&self.dir, data)?;
                let info = format!("100644,{blob},{p}");
                git::run_env(
                    &self.dir,
                    &["update-index", "--add", "--cacheinfo", &info],
                    None,
                    &env,
                )?;
            }
            let tree = git::run_env(&self.dir, &["write-tree"], None, &env)?
                .trim()
                .to_string();
            let c = match base {
                Some(b) => git::run(&self.dir, &["commit-tree", &tree, "-p", b, "-m", msg], None)?,
                None => git::run(&self.dir, &["commit-tree", &tree, "-m", msg], None)?,
            };
            Ok(c.trim().to_string())
        })();
        let _ = std::fs::remove_file(&idx);
        res
    }

    /// Push a prepared commit: Done (it is the branch now) or Rejected
    /// (another machine wrote first).
    pub fn push(&self, p: &Prepared) -> Result<Pushed> {
        let Some(c) = &p.commit else {
            return Ok(Pushed::Done);
        };
        let r = git::push(&self.dir, "origin", c, "leases", self.cred.as_ref())?;
        if r == Pushed::Done {
            git::run(
                &self.dir,
                &["update-ref", "refs/remotes/origin/leases", c],
                None,
            )?;
        }
        Ok(r)
    }

    /// The whole compare and swap: prepare and push until a push goes
    /// through (or the branch keeps moving).
    pub fn apply(
        &self,
        ctx: &Ctx,
        ops: &[(String, Op)],
        beat: bool,
    ) -> Result<(Board, Vec<(String, Outcome)>)> {
        for _ in 0..10 {
            let mut p = self.prepare(ctx, ops, beat)?;
            if self.push(&p)? == Pushed::Done {
                if let Some(c) = &p.commit {
                    p.board.head = Some(c.clone());
                }
                save_board(ctx, &p.board);
                return Ok((p.board, p.outcomes));
            }
        }
        bail!("the leases branch kept moving; will try again")
    }
}

fn message(ops: &[(String, Op)], label: &str) -> String {
    match ops {
        [] => format!("heartbeat {label}"),
        [(sid, op)] => {
            let w = match op {
                Op::Acquire => "acquire",
                Op::Request => "request",
                Op::TakeOverNow => "take over",
                Op::Release { .. } => "release",
                Op::Keep => "keep",
            };
            format!("{w} {} by {label}", crate::paths::head(sid, 8))
        }
        _ => format!("{} leases by {label}", ops.len()),
    }
}

/// The board as last fetched, kept for offline starts.
pub fn board_path(ctx: &Ctx) -> PathBuf {
    ctx.sync_dir().join("leases.json")
}

pub fn save_board(ctx: &Ctx, b: &Board) {
    let _ = super::write_file(
        &board_path(ctx),
        &serde_json::to_vec_pretty(b).unwrap_or_default(),
    );
}

pub fn load_board(ctx: &Ctx) -> Option<Board> {
    serde_json::from_slice(&std::fs::read(board_path(ctx)).ok()?).ok()
}

/// Fetch the board (saved for offline use); offline: the last one saved.
pub fn current(ctx: &Ctx) -> (Option<Board>, bool) {
    match Repo::open(ctx).and_then(|r| r.fetch(ctx.now_ms)) {
        Ok(b) => {
            save_board(ctx, &b);
            (Some(b), true)
        }
        Err(_) => (load_board(ctx), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn board_with(owner: &str, beat_at: i64) -> Board {
        let mut b = Board::default();
        b.leases.insert(
            "s1".into(),
            Lease {
                owner: owner.into(),
                gen: 3,
                acquired: rfc(0),
                ..Default::default()
            },
        );
        b.beats.insert(
            owner.into(),
            Beat {
                at: rfc(beat_at),
                label: "MacBook".into(),
            },
        );
        b
    }

    #[test]
    fn a_lease_expires_ten_minutes_after_the_last_heartbeat() {
        let b = board_with("other", 0);
        let lease = 10 * MIN;
        assert!(matches!(
            b.status("s1", "me", 9 * MIN + 59_000, lease),
            Status::Live { .. }
        ));
        assert!(matches!(
            b.status("s1", "me", 10 * MIN, lease),
            Status::Stale { .. }
        ));
        // Live: an acquire is refused; stale: it never blocks.
        let (w, o) = decide(&b, "s1", &Op::Acquire, "me", "Linux", 9 * MIN, lease);
        assert!(w.is_none() && matches!(o, Outcome::Held(Status::Live { .. })));
        let (w, o) = decide(&b, "s1", &Op::Acquire, "me", "Linux", 11 * MIN, lease);
        let l = w.unwrap();
        assert_eq!((l.owner.as_str(), l.gen), ("me", 4));
        assert!(matches!(
            o,
            Outcome::Acquired {
                gen: 4,
                prior: Status::Stale { .. }
            }
        ));
    }

    #[test]
    fn request_release_and_take_over_now() {
        let lease = 10 * MIN;
        let b = board_with("other", 0);
        let (w, o) = decide(&b, "s1", &Op::Request, "me", "Linux", MIN, lease);
        let l = w.unwrap();
        assert_eq!(l.owner, "other", "a request does not change hands");
        assert_eq!(l.takeover.as_ref().unwrap().by, "me");
        assert!(matches!(o, Outcome::Requested(_)));
        // The owner sees it, and lets go.
        let mut b2 = b.clone();
        b2.leases.insert("s1".into(), l);
        let (_, o) = decide(&b2, "s1", &Op::Keep, "other", "Mac", MIN, lease);
        assert!(matches!(o, Outcome::Kept { takeover: Some(_) }));
        let rel = Op::Release {
            len: 10,
            sha256: "ab".into(),
        };
        let (w, o) = decide(&b2, "s1", &rel, "other", "Mac", MIN, lease);
        let l = w.unwrap();
        assert_eq!(o, Outcome::Released);
        let r = l.released.clone().unwrap();
        assert_eq!((r.len, r.to.as_deref()), (10, Some("me")));
        b2.leases.insert("s1".into(), l);
        // Released: the requester takes it, knowing what to wait for.
        let (w, o) = decide(&b2, "s1", &Op::Acquire, "me", "Linux", MIN, lease);
        assert_eq!(w.unwrap().gen, 4);
        assert!(matches!(
            o,
            Outcome::Acquired {
                prior: Status::Released { len: 10, .. },
                ..
            }
        ));
        // Take over now never waits, live or not.
        let (w, _) = decide(&b, "s1", &Op::TakeOverNow, "me", "Linux", MIN, lease);
        assert_eq!(w.unwrap().owner, "me");
        // The old owner's next check finds the loss.
        let mut b3 = b.clone();
        b3.leases.insert(
            "s1".into(),
            decide(&b, "s1", &Op::TakeOverNow, "me", "Linux", MIN, lease)
                .0
                .unwrap(),
        );
        let (_, o) = decide(&b3, "s1", &Op::Keep, "other", "Mac", MIN, lease);
        assert!(matches!(
            o,
            Outcome::Lost(Status::Live { .. }) | Outcome::Lost(Status::Stale { .. })
        ));
    }

    #[test]
    fn ids_are_checked() {
        assert!(valid_id("5dfa-12_ab"));
        assert!(!valid_id("../x") && !valid_id("") && !valid_id("a b"));
    }
}
