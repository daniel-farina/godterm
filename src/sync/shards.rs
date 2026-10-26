//! Monthly conversation repos (`<prefix>-YYYY-MM`, then `-2` past
//! `shard_gb`). Each is a bare partial clone (`--filter=blob:none`) under
//! `sync/`: chunks are added with plumbing on a temporary index (no
//! checkout, no old blobs fetched), and a chunk is read on demand, which
//! fetches just that blob. A push that someone else beat is rebuilt on
//! the new tip and pushed again, never forced.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::git::{self, Pushed};
use super::Ctx;

/// The repo name for chunks written in `month` ("2026-10"), given the
/// bytes each repo holds.
pub fn pick(ctx: &Ctx, month: &str, used: &BTreeMap<String, u64>) -> String {
    let base = format!("{}-{month}", ctx.sync().convos_prefix);
    let max = ctx.sync().shard_gb.max(1) << 30;
    for n in 1.. {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        if used.get(&name).copied().unwrap_or(0) < max {
            return name;
        }
    }
    unreachable!()
}

pub fn this_month() -> String {
    chrono::Local::now().format("%Y-%m").to_string()
}

pub fn local_dir(ctx: &Ctx, repo: &str) -> PathBuf {
    ctx.sync_dir().join(format!("{repo}.git"))
}

/// The local bare partial clone of a conversation repo, cloned (and on
/// the remote created, when allowed) the first time.
pub fn open(ctx: &Ctx, repo: &str, may_create: bool) -> Result<PathBuf> {
    let d = local_dir(ctx, repo);
    if git::is_sync_repo(&d) {
        return Ok(d);
    }
    let r = ctx.remote();
    let owner = &ctx.sync().owner;
    if !r.exists(owner, repo)? {
        if !may_create {
            bail!("{owner}/{repo} does not exist (and may not be created without a yes)");
        }
        r.create_private(owner, repo)?;
    }
    std::fs::create_dir_all(ctx.sync_dir())?;
    let _ = std::fs::remove_dir_all(&d);
    let url = r.url(owner, repo);
    let ds = d.to_string_lossy().into_owned();
    git::run(
        &ctx.sync_dir(),
        &["clone", "-q", "--bare", "--filter=blob:none", &url, &ds],
        r.cred().as_ref(),
    )?;
    git::mark(&d)?;
    Ok(d)
}

/// Whether new shards may be made without asking: always for a local
/// stand-in, on GitHub only after `sync init`'s yes.
pub fn may_create(ctx: &Ctx) -> bool {
    !ctx.remote().is_github() || ctx.sync().auto_create_shards
}

/// Add files (path, bytes) to a conversation repo and push.
pub fn add(ctx: &Ctx, repo: &str, files: &[(String, Vec<u8>)]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let d = open(ctx, repo, may_create(ctx))?;
    let cred = ctx.remote().cred();
    let idx = d.join(format!("sync-index-{}", std::process::id()));
    let env = [("GIT_INDEX_FILE", idx.as_path())];
    let mut blobs = vec![];
    for (p, b) in files {
        blobs.push((p.clone(), git::hash_object(&d, b)?));
    }
    for _ in 0..5 {
        git::fetch(&d, "main", cred.as_ref())?;
        let base = git::rev(&d, "refs/remotes/origin/main");
        let _ = std::fs::remove_file(&idx);
        if let Some(b) = &base {
            git::run_env(&d, &["read-tree", b], None, &env)?;
        }
        for (p, blob) in &blobs {
            let info = format!("100644,{blob},{p}");
            git::run_env(
                &d,
                &["update-index", "--add", "--cacheinfo", &info],
                None,
                &env,
            )?;
        }
        let tree = git::run_env(&d, &["write-tree"], None, &env)?
            .trim()
            .to_string();
        let msg = format!("{} chunks from {}", files.len(), ctx.machine.name());
        let commit = match &base {
            Some(b) => git::run(&d, &["commit-tree", &tree, "-p", b, "-m", &msg], None)?,
            None => git::run(&d, &["commit-tree", &tree, "-m", &msg], None)?,
        };
        let commit = commit.trim().to_string();
        match git::push(&d, "origin", &commit, "main", cred.as_ref())? {
            Pushed::Done => {
                let _ = std::fs::remove_file(&idx);
                git::run(
                    &d,
                    &["update-ref", "refs/remotes/origin/main", &commit],
                    None,
                )?;
                return Ok(());
            }
            Pushed::Rejected => continue,
        }
    }
    let _ = std::fs::remove_file(&idx);
    bail!("{repo}: other machines kept pushing first; will try again")
}

/// One chunk's bytes (fetched on demand).
pub fn read(ctx: &Ctx, c: &super::chunks::Chunk) -> Result<Vec<u8>> {
    let d = open(ctx, &c.repo, false)?;
    let cred = ctx.remote().cred();
    let spec = format!("refs/remotes/origin/main:{}", c.path);
    if let Ok(b) = git::run_bytes(&d, &["cat-file", "blob", &spec], cred.as_ref()) {
        return Ok(b);
    }
    git::fetch(&d, "main", cred.as_ref())?;
    git::run_bytes(&d, &["cat-file", "blob", &spec], cred.as_ref())
}
