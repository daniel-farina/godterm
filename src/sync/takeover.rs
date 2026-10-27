use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::time::Duration;

use super::chunks::{self, Cmp, Manifest};
use super::collect::SessionRef;
use super::lease::{self, Op, Outcome};
use super::state_repo::{self, Lock};
use super::Ctx;

pub const LOCK_WAIT: Duration = Duration::from_secs(90);

pub fn local_session(ctx: &Ctx, acct: &str, sid: &str) -> Option<SessionRef> {
    if !lease::valid_id(sid) {
        return None;
    }
    let a = ctx.accounts().into_iter().find(|a| a.name == acct)?;
    let (root, name) = if a.provider == "claude" {
        ("projects", format!("{sid}.jsonl"))
    } else {
        ("sessions", sid.to_string())
    };
    let path = std::fs::read_dir(a.dir.join(root))
        .ok()?
        .flatten()
        .map(|d| d.path().join(&name))
        .find(|p| p.exists())?;
    let md = std::fs::metadata(&path).ok()?;
    Some(SessionRef {
        provider: a.provider,
        account: a.name.clone(),
        id: sid.to_string(),
        cwd: if a.provider == "claude" {
            super::collect::claude_cwd(&path)
        } else {
            None
        },
        mtime_ms: md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        bytes: md.len(),
        hot: true,
        path,
    })
}

pub fn fingerprint_files(files: &[(String, u64, String)]) -> (u64, String) {
    if let [(_, len, sha)] = files {
        return (*len, sha.clone());
    }
    let mut v: Vec<&(String, u64, String)> = files.iter().collect();
    v.sort();
    let text: String = v.iter().map(|(n, l, s)| format!("{n}:{l}:{s}\n")).collect();
    (v.iter().map(|x| x.1).sum(), super::sha_hex(text.as_bytes()))
}

pub fn fingerprint_local(s: &SessionRef) -> Result<(u64, String)> {
    let mut v = vec![];
    for (name, p) in s.files() {
        let len = std::fs::metadata(&p)?.len();
        let (_, sha) = chunks::scan(&p, &[], len)?;
        v.push((name, len, sha));
    }
    Ok(fingerprint_files(&v))
}

pub fn fingerprint_manifest(m: &Manifest) -> (u64, String) {
    let v: Vec<(String, u64, String)> = m
        .all_files()
        .into_iter()
        .map(|f| (f.name, f.len, f.sha256))
        .collect();
    fingerprint_files(&v)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reconciled {
    pub cmp: Option<Cmp>,

    pub pulled: bool,

    pub forked: Option<String>,

    pub local_only: bool,
}

pub fn remote_manifest(repo: &lease::Repo, acct: &str, sid: &str) -> Option<Manifest> {
    let b = repo.main_file(&format!("sessions/{acct}/{sid}.json"))?;
    serde_json::from_slice(&b).ok()
}

pub fn reconcile(ctx: &Ctx, acct: &str, sid: &str, owner: bool) -> Result<Reconciled> {
    let _lock = Lock::wait(ctx, LOCK_WAIT)?;
    let repo = lease::Repo::open(ctx)?;
    repo.fetch_main()?;
    reconcile_with(ctx, &repo, acct, sid, owner)
}

pub fn reconcile_with(
    ctx: &Ctx,
    repo: &lease::Repo,
    acct: &str,
    sid: &str,
    owner: bool,
) -> Result<Reconciled> {
    let Some(m) = remote_manifest(repo, acct, sid) else {
        return Ok(Reconciled {
            local_only: true,
            ..Default::default()
        });
    };
    let Some(s) = local_session(ctx, acct, sid) else {
        state_repo::place_session(ctx, &m)?;
        return Ok(Reconciled {
            pulled: true,
            ..Default::default()
        });
    };
    let read = |c: &chunks::Chunk| super::shards::read(ctx, c);

    let mut cmp = Cmp::Equal;
    let local_files = s.files();
    let mut per: Vec<(chunks::FileMan, PathBuf, Cmp)> = vec![];
    for f in m.all_files() {
        let p = if s.provider == "claude" {
            s.path.clone()
        } else {
            s.path.join(&f.name)
        };
        let c = if !p.exists() {
            Cmp::RemoteAhead
        } else {
            let len = std::fs::metadata(&p)?.len();
            chunks::compare(&p, len, &f, &read)?
        };
        cmp = match (cmp, c) {
            (Cmp::Equal, x) | (x, Cmp::Equal) => x,
            (a, b) if a == b => a,
            _ => Cmp::Diverged,
        };
        per.push((f, p, c));
    }

    if s.provider != "claude"
        && local_files
            .iter()
            .any(|(n, _)| !per.iter().any(|(f, _, _)| &f.name == n))
    {
        cmp = match cmp {
            Cmp::Equal | Cmp::LocalAhead => Cmp::LocalAhead,
            _ => Cmp::Diverged,
        };
    }
    let mut out = Reconciled {
        cmp: Some(cmp),
        ..Default::default()
    };
    let mut ledger = state_repo::load_ledger(ctx);
    match cmp {
        Cmp::Equal => {}
        Cmp::LocalAhead if owner => {}
        Cmp::RemoteAhead => {
            for (f, p, c) in &per {
                match c {
                    Cmp::Equal => {}
                    _ if !p.exists() || !f.name.ends_with(".jsonl") => {
                        chunks::place(p, &chunks::remote_bytes(f, 0, &read)?)?;
                    }
                    _ => {
                        let len = std::fs::metadata(p)?.len();
                        chunks::extend(p, len, &chunks::remote_bytes(f, len, &read)?)?;
                    }
                }
                ledger.insert(
                    state_repo::led_key(acct, sid, &f.name),
                    state_repo::led_from(f, p),
                );
            }
            out.pulled = true;
        }
        Cmp::LocalAhead | Cmp::Diverged => {
            let date = chrono::Local::now().format("%Y-%m-%d").to_string();
            let (nid, _) = if s.provider == "claude" {
                let all = std::fs::read(&s.path).unwrap_or_default();
                let head = &all[..all.len().min(256 << 10)];
                let title = chunks::claude_title(head, chunks::last_title(&all));
                chunks::fork_claude(&s.path, sid, &title, &ctx.machine.name(), &date)?
            } else {
                chunks::fork_grok(&s.path, &ctx.machine.name(), &date)?
            };

            if s.provider != "claude" {
                for (n, p) in &local_files {
                    if !per.iter().any(|(f, _, _)| &f.name == n) {
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
            for (f, p, _) in &per {
                chunks::place(p, &chunks::remote_bytes(f, 0, &read)?)?;
                ledger.insert(
                    state_repo::led_key(acct, sid, &f.name),
                    state_repo::led_from(f, p),
                );
            }
            state_repo::note_fork(ctx, &nid, sid);
            crate::log::info(&format!(
                "sync: {sid}: bytes this machine never pushed are now session {nid}"
            ));
            out.forked = Some(nid);
            out.pulled = true;
        }
    }
    state_repo::save_ledger(ctx, &ledger);
    Ok(out)
}

pub fn matches_release(ctx: &Ctx, acct: &str, sid: &str, len: u64, sha: &str) -> bool {
    local_session(ctx, acct, sid)
        .and_then(|s| fingerprint_local(&s).ok())
        .is_some_and(|(l, h)| l == len && h == sha)
}

pub fn release(ctx: &Ctx, acct: &str, sid: &str) -> Result<Outcome> {
    let s = local_session(ctx, acct, sid).context("the transcript is not here")?;
    let want = fingerprint_local(&s)?;
    let mut last = None;
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let rep = loop {
            match state_repo::sync_now(ctx) {
                Ok(r)
                    if r.error
                        .as_deref()
                        .is_some_and(|e| e.contains("already running")) =>
                {
                    if t.elapsed() > LOCK_WAIT {
                        break r;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Ok(r) => break r,
                Err(e) => return Err(e),
            }
        };
        let wc = ctx.state_dir();
        let pushed = state_repo::load_manifest(&wc, acct, sid).map(|m| fingerprint_manifest(&m));
        if rep.ok && pushed.as_ref() == Some(&want) {
            let repo = lease::Repo::open(ctx)?;
            let (_, out) = repo.apply(
                ctx,
                &[(
                    sid.to_string(),
                    Op::Release {
                        len: want.0,
                        sha256: want.1.clone(),
                    },
                )],
                false,
            )?;
            return Ok(out
                .into_iter()
                .next()
                .map(|(_, o)| o)
                .unwrap_or(Outcome::Released));
        }
        last = Some(
            rep.error
                .unwrap_or_else(|| "not all of it reached the repo".into()),
        );
    }
    bail!(
        "could not push the last bytes: {}",
        last.unwrap_or_default()
    )
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Acquire {
        sid: String,
        acct: String,
        pull: bool,
    },
    Request {
        sid: String,
        acct: String,
    },

    TakeOverNow {
        sid: String,
        acct: String,
    },

    Release {
        sid: String,
        acct: String,
    },

    Fork {
        sid: String,
        acct: String,
        owner: bool,
        label: String,
    },

    Offline {
        sid: String,
        acct: String,
    },

    Poll,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Acquired {
        sid: String,
        offline: bool,
        pulled: bool,
        forked: Option<String>,
        warn: Option<String>,
    },

    Held {
        sid: String,
        status: lease::Status,
        offline: bool,
    },
    Requested {
        sid: String,
        status: lease::Status,
    },

    Lost {
        sid: String,
        label: String,
        conflict: bool,
    },

    Asked {
        sid: String,
        label: String,
    },
    Released {
        sid: String,
        to: Option<String>,
    },
    Forked {
        sid: String,
        fork: Option<String>,
        label: String,
        owner: bool,
    },
    Board {
        board: lease::Board,
        online: bool,
    },
    Failed {
        sid: String,
        why: String,
    },
}

#[derive(Default)]
pub struct Engine {
    pub held: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,

    pub wanted: std::collections::BTreeMap<String, String>,

    pub offline: std::collections::BTreeSet<String>,
    asked: std::collections::BTreeSet<String>,
    lost: std::collections::BTreeSet<String>,
    pub board: Option<lease::Board>,
    pub online: bool,
    last_beat_ms: i64,
}

impl Engine {
    pub fn new(
        held: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
    ) -> Engine {
        Engine {
            held,
            online: true,
            ..Default::default()
        }
    }

    fn held(&self) -> std::collections::BTreeMap<String, String> {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn interval_s(&self, ctx: &Ctx) -> u64 {
        let hb = ctx.sync().heartbeat_s.clamp(10, 3600);
        if !self.online {
            30.min(hb)
        } else if !self.wanted.is_empty() || !self.asked.is_empty() {
            20.min(hb)
        } else {
            hb
        }
    }

    pub fn beat_due(&self, ctx: &Ctx) -> bool {
        ctx.now_ms - self.last_beat_ms >= ctx.sync().heartbeat_s.max(1) as i64 * 1000 - 1000
    }

    fn acquire(
        &mut self,
        ctx: &Ctx,
        repo: &lease::Repo,
        list: &[(String, String, bool)],
        op: Op,
    ) -> Vec<Event> {
        let ops: Vec<(String, Op)> = list
            .iter()
            .map(|(s, _, _)| (s.clone(), op.clone()))
            .collect();
        let mut ev = vec![];
        match repo.apply(ctx, &ops, false) {
            Ok((board, outs)) => {
                self.online = true;
                self.last_beat_ms = ctx.now_ms;
                for ((sid, acct, pull), (_, o)) in list.iter().zip(outs) {
                    ev.push(self.after_outcome(ctx, repo, sid, acct, *pull, o));
                }
                self.board = Some(board);
            }
            Err(e) => {
                self.online = false;
                let b = self.board.clone().or_else(|| lease::load_board(ctx));
                for (sid, acct, _) in list {
                    let st = b
                        .as_ref()
                        .map(|b| b.status(sid, &ctx.machine.id, ctx.now_ms, ctx.lease_ms()))
                        .unwrap_or(lease::Status::Free);
                    let ours = matches!(st, lease::Status::Mine { .. } | lease::Status::Free);
                    if op == Op::Acquire && ours {
                        self.offline.insert(sid.clone());
                        self.hold(sid, acct);
                        ev.push(Event::Acquired {
                            sid: sid.clone(),
                            offline: true,
                            pulled: false,
                            forked: None,
                            warn: Some(format!(
                                "offline ({}): will fork if someone else took over",
                                short_err(&e)
                            )),
                        });
                    } else {
                        ev.push(Event::Held {
                            sid: sid.clone(),
                            status: st,
                            offline: true,
                        });
                    }
                }
            }
        }
        ev
    }

    fn hold(&self, sid: &str, acct: &str) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(sid.to_string(), acct.to_string());
    }

    fn after_outcome(
        &mut self,
        ctx: &Ctx,
        repo: &lease::Repo,
        sid: &str,
        acct: &str,
        pull: bool,
        o: Outcome,
    ) -> Event {
        match o {
            Outcome::Acquired { prior, .. } => {
                self.wanted.remove(sid);
                self.lost.remove(sid);
                self.hold(sid, acct);
                if !pull {
                    return Event::Acquired {
                        sid: sid.into(),
                        offline: false,
                        pulled: false,
                        forked: None,
                        warn: None,
                    };
                }

                let r = (|| -> Result<Reconciled> {
                    let _lock = Lock::wait(ctx, LOCK_WAIT)?;
                    let mut last = Reconciled::default();
                    for i in 0..3 {
                        repo.fetch_main()?;
                        last = reconcile_with(ctx, repo, acct, sid, true)?;
                        match &prior {
                            lease::Status::Released { len, sha256, .. }
                                if !matches_release(ctx, acct, sid, *len, sha256) && i < 2 =>
                            {
                                std::thread::sleep(Duration::from_millis(500));
                            }
                            _ => break,
                        }
                    }
                    Ok(last)
                })();
                match r {
                    Ok(r) => Event::Acquired {
                        sid: sid.into(),
                        offline: false,
                        pulled: r.pulled,
                        forked: r.forked,
                        warn: None,
                    },
                    Err(e) => Event::Acquired {
                        sid: sid.into(),
                        offline: false,
                        pulled: false,
                        forked: None,
                        warn: Some(format!(
                            "could not pull the latest copy first: {}",
                            short_err(&e)
                        )),
                    },
                }
            }
            Outcome::Held(status) => Event::Held {
                sid: sid.into(),
                status,
                offline: false,
            },
            Outcome::Requested(status) => {
                self.wanted.insert(sid.into(), acct.into());
                Event::Requested {
                    sid: sid.into(),
                    status,
                }
            }
            Outcome::Released => Event::Released {
                sid: sid.into(),
                to: None,
            },
            Outcome::Lost(st) => Event::Lost {
                sid: sid.into(),
                label: label_of(&st),
                conflict: false,
            },
            Outcome::Kept { .. } => Event::Acquired {
                sid: sid.into(),
                offline: false,
                pulled: false,
                forked: None,
                warn: None,
            },
        }
    }

    pub fn handle(&mut self, ctx: &Ctx, cmds: Vec<Cmd>) -> Vec<Event> {
        let repo = match lease::Repo::open(ctx) {
            Ok(r) => r,
            Err(e) => {
                return cmds
                    .iter()
                    .filter_map(|c| match c {
                        Cmd::Acquire { sid, .. }
                        | Cmd::Request { sid, .. }
                        | Cmd::TakeOverNow { sid, .. }
                        | Cmd::Release { sid, .. }
                        | Cmd::Fork { sid, .. }
                        | Cmd::Offline { sid, .. } => Some(Event::Failed {
                            sid: sid.clone(),
                            why: short_err(&e),
                        }),
                        Cmd::Poll => None,
                    })
                    .collect()
            }
        };
        let mut ev = vec![];
        let acquires: Vec<(String, String, bool)> = cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Acquire { sid, acct, pull } => Some((sid.clone(), acct.clone(), *pull)),
                _ => None,
            })
            .collect();
        if !acquires.is_empty() {
            ev.extend(self.acquire(ctx, &repo, &acquires, Op::Acquire));
        }
        for c in cmds {
            match c {
                Cmd::Acquire { .. } => {}
                Cmd::Poll => ev.extend(self.poll_with(ctx, &repo, false)),
                Cmd::Request { sid, acct } => {
                    ev.extend(self.acquire(ctx, &repo, &[(sid, acct, true)], Op::Request))
                }
                Cmd::TakeOverNow { sid, acct } => {
                    ev.extend(self.acquire(ctx, &repo, &[(sid, acct, true)], Op::TakeOverNow))
                }
                Cmd::Offline { sid, acct } => {
                    self.offline.insert(sid.clone());
                    self.hold(&sid, &acct);
                    ev.push(Event::Acquired {
                        sid,
                        offline: true,
                        pulled: false,
                        forked: None,
                        warn: Some("working offline: this part becomes a fork on reconnect if someone else has it".into()),
                    });
                }
                Cmd::Release { sid, acct } => {
                    let e = match release(ctx, &acct, &sid) {
                        Ok(Outcome::Released) => {
                            self.unhold(&sid);
                            let to = self
                                .board
                                .as_ref()
                                .and_then(|b| b.leases.get(&sid))
                                .and_then(|l| l.takeover.as_ref())
                                .map(|t| t.label.clone());
                            Event::Released { sid, to }
                        }
                        Ok(o) => self.after_outcome(ctx, &repo, &sid, &acct, false, o),
                        Err(e) => Event::Failed {
                            sid,
                            why: format!("could not release: {}", short_err(&e)),
                        },
                    };
                    ev.push(e);
                }
                Cmd::Fork {
                    sid,
                    acct,
                    owner,
                    label,
                } => {
                    let e = match reconcile(ctx, &acct, &sid, owner) {
                        Ok(r) => {
                            if !owner {
                                self.unhold(&sid);
                            }
                            self.offline.remove(&sid);
                            Event::Forked {
                                sid,
                                fork: r.forked,
                                label,
                                owner,
                            }
                        }
                        Err(e) => Event::Failed {
                            sid,
                            why: format!("could not save the unsent part: {}", short_err(&e)),
                        },
                    };
                    ev.push(e);
                }
            }
        }
        ev
    }

    fn unhold(&self, sid: &str) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(sid);
    }

    pub fn poll(&mut self, ctx: &Ctx) -> Vec<Event> {
        match lease::Repo::open(ctx) {
            Ok(repo) => {
                let beat = self.beat_due(ctx);
                self.poll_with(ctx, &repo, beat)
            }
            Err(_) => vec![],
        }
    }

    fn poll_with(&mut self, ctx: &Ctx, repo: &lease::Repo, beat: bool) -> Vec<Event> {
        let got = if beat {
            repo.apply(ctx, &[], true).map(|(b, _)| b)
        } else {
            repo.fetch(ctx.now_ms)
                .inspect(|b| lease::save_board(ctx, b))
        };
        let board = match got {
            Ok(b) => b,
            Err(_) => {
                self.online = false;
                let b = self.board.clone().or_else(|| lease::load_board(ctx));
                return b
                    .map(|board| {
                        vec![Event::Board {
                            board,
                            online: false,
                        }]
                    })
                    .unwrap_or_default();
            }
        };
        if beat {
            self.last_beat_ms = ctx.now_ms;
        }
        self.online = true;
        self.board = Some(board.clone());
        let me = ctx.machine.id.clone();
        let (now, lease_ms) = (ctx.now_ms, ctx.lease_ms());
        let mut ev = vec![];
        let mut take: Vec<(String, String, bool)> = vec![];
        let mut check: Vec<(String, String)> = vec![];
        for (sid, acct) in self.held() {
            let st = board.status(&sid, &me, now, lease_ms);
            let was_offline = self.offline.contains(&sid);
            match &st {
                lease::Status::Mine { takeover, .. } => {
                    if was_offline {
                        self.offline.remove(&sid);
                        check.push((sid.clone(), acct.clone()));
                    }
                    match takeover {
                        Some(t) if t.by != me && !self.asked.contains(&sid) => {
                            self.asked.insert(sid.clone());
                            ev.push(Event::Asked {
                                sid: sid.clone(),
                                label: if t.label.is_empty() {
                                    board.label_of(&t.by)
                                } else {
                                    t.label.clone()
                                },
                            });
                        }
                        None => {
                            self.asked.remove(&sid);
                        }
                        _ => {}
                    }
                }

                lease::Status::Free => take.push((sid.clone(), acct.clone(), false)),
                lease::Status::Released { .. } | lease::Status::Stale { .. } if was_offline => {
                    take.push((sid.clone(), acct.clone(), false))
                }
                _ => {
                    if self.lost.insert(sid.clone()) {
                        self.offline.remove(&sid);
                        ev.push(Event::Lost {
                            sid: sid.clone(),
                            label: label_of(&st),
                            conflict: false,
                        });
                    }
                }
            }
        }
        if !take.is_empty() {
            let offline_ones: Vec<(String, String)> = take
                .iter()
                .filter(|(s, _, _)| self.offline.contains(s))
                .map(|(s, a, _)| (s.clone(), a.clone()))
                .collect();
            for e in self.acquire(ctx, repo, &take, Op::Acquire) {
                match e {
                    Event::Held { sid, status, .. } => {
                        if self.lost.insert(sid.clone()) {
                            ev.push(Event::Lost {
                                sid,
                                label: label_of(&status),
                                conflict: false,
                            });
                        }
                    }

                    Event::Acquired { .. } => {}
                    other => ev.push(other),
                }
            }
            for (s, a) in offline_ones {
                self.offline.remove(&s);
                check.push((s, a));
            }
        }

        if !check.is_empty() && repo.fetch_main().is_ok() {
            for (sid, acct) in check {
                if self.diverged(ctx, repo, &acct, &sid) && self.lost.insert(sid.clone()) {
                    ev.push(Event::Lost {
                        sid,
                        label: "the synced copy".into(),
                        conflict: true,
                    });
                }
            }
        }

        let wanted: Vec<(String, String)> = self
            .wanted
            .iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        let ready: Vec<(String, String, bool)> = wanted
            .into_iter()
            .filter(|(sid, _)| {
                !matches!(
                    board.status(sid, &me, now, lease_ms),
                    lease::Status::Live { .. }
                )
            })
            .map(|(s, a)| (s, a, true))
            .collect();
        if !ready.is_empty() {
            ev.extend(self.acquire(ctx, repo, &ready, Op::Acquire));
        }
        ev.push(Event::Board {
            board: self.board.clone().unwrap_or(board),
            online: true,
        });
        ev
    }

    fn diverged(&self, ctx: &Ctx, repo: &lease::Repo, acct: &str, sid: &str) -> bool {
        let (Some(m), Some(s)) = (
            remote_manifest(repo, acct, sid),
            local_session(ctx, acct, sid),
        ) else {
            return false;
        };
        let read = |c: &chunks::Chunk| super::shards::read(ctx, c);
        m.all_files().iter().any(|f| {
            let p = if s.provider == "claude" {
                s.path.clone()
            } else {
                s.path.join(&f.name)
            };
            let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            matches!(
                chunks::compare(&p, len, f, &read),
                Ok(Cmp::Diverged) | Ok(Cmp::RemoteAhead)
            )
        })
    }
}

pub fn label_of(st: &lease::Status) -> String {
    match st {
        lease::Status::Live { label, .. } | lease::Status::Stale { label, .. } => label.clone(),
        lease::Status::Released { to: Some(t), .. } => crate::paths::head(t, 8).to_string(),
        lease::Status::Released { .. } => "nobody (released)".into(),
        _ => "this machine".into(),
    }
}

fn short_err(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    crate::paths::head(s.lines().next().unwrap_or(""), 160).to_string()
}
