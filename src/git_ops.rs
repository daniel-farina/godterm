use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Ctx {
    pub top: PathBuf,

    pub account: Option<String>,

    pub helper: Option<PathBuf>,

    pub gh: Option<PathBuf>,
    pub limit: Duration,
}

impl Ctx {
    pub fn new(top: &Path) -> Ctx {
        Ctx {
            top: top.to_path_buf(),
            account: None,
            helper: None,
            gh: None,
            limit: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Fail {
    Msg(String),

    Diverged { ahead: u32, behind: u32 },
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Msg(m) => write!(f, "{m}"),
            Fail::Diverged { ahead, behind } => write!(
                f,
                "Diverged: {ahead} local, {behind} remote. Nothing was rewritten; push the local commits as a branch of their own, or pull and merge in a tab."
            ),
        }
    }
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Msg(s)
    }
}

pub type Res<T> = Result<T, Fail>;

const ALLOWED: &[&str] = &[
    "status",
    "log",
    "diff",
    "rev-list",
    "rev-parse",
    "for-each-ref",
    "config",
    "add",
    "commit",
    "push",
    "pull",
    "fetch",
    "switch",
    "stash",
    "init",
    "check-ref-format",
    "check-ignore",
    "ls-files",
    "cat-file",
    "remote",
    "symbolic-ref",
    "rm",
    "merge-base",
    "branch",
];

const WITH_VALUE: &[&str] = &["-m", "--message", "--format", "-n", "-b", "-C", "-c"];

#[allow(clippy::collapsible_match)]
pub fn forbidden(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-c" || a == "-C" {
            if let Some(v) = args.get(i + 1) {
                let low = v.to_ascii_lowercase();

                if a == "-c" && (low.starts_with("alias.") || low.contains("force")) {
                    return Some(format!("config {v} is not allowed"));
                }
            }
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        break;
    }
    let Some(sub) = args.get(i).map(|s| s.as_str()) else {
        return Some("no git subcommand".into());
    };
    if !ALLOWED.contains(&sub) {
        return Some(format!("git {sub} is never run by GodTerm"));
    }
    let rest: Vec<&str> = args[i + 1..].iter().map(|s| s.as_str()).collect();
    let mut opts: Vec<&str> = vec![];
    let mut pos: Vec<&str> = vec![];
    let mut k = 0;
    let mut dashdash = false;
    while k < rest.len() {
        let a = rest[k];
        if dashdash {
            pos.push(a);
        } else if a == "--" {
            dashdash = true;
        } else if a.starts_with('-') {
            opts.push(a);
            if WITH_VALUE.contains(&a) {
                k += 1;
            }
        } else {
            pos.push(a);
        }
        k += 1;
    }
    for o in &opts {
        let low = o.to_ascii_lowercase();
        if low.starts_with("--force") || low == "--mirror" || low.starts_with("--delete") {
            return Some(format!("{o} is never used: history is never rewritten"));
        }

        if !o.starts_with("--") && o.len() > 1 && o[1..].contains('f') {
            return Some(format!("{o} is never used (force)"));
        }
    }
    let has = |x: &str| opts.contains(&x);
    match sub {
        "push" => {
            for o in &opts {
                if matches!(*o, "-d" | "--prune" | "--all" | "--tags" | "--follow-tags") {
                    return Some(format!("push {o} is never used"));
                }
            }
            for p in &pos {
                if p.starts_with('+') || p.starts_with(':') || p.contains(":+") {
                    return Some(format!("refspec {p} would force or delete"));
                }
            }
        }
        "fetch" => {
            if pos.iter().any(|p| p.starts_with('+')) {
                return Some("a forced fetch refspec".into());
            }
        }
        "pull" => {
            if !has("--ff-only") {
                return Some("pull runs with --ff-only only".into());
            }
            if opts.iter().any(|o| o.starts_with("--rebase") || *o == "-r") {
                return Some("pull never rebases".into());
            }
        }
        "commit" => {
            if opts
                .iter()
                .any(|o| *o == "--amend" || o.starts_with("--fixup") || o.starts_with("--squash"))
            {
                return Some("commits are never amended".into());
            }
        }
        "switch" => {
            if opts.iter().any(|o| {
                matches!(
                    *o,
                    "--discard-changes" | "-C" | "--force-create" | "--orphan"
                )
            }) {
                return Some("switch never discards or recreates".into());
            }
        }
        "branch" => {
            if opts
                .iter()
                .any(|o| !(o.starts_with("--format") || *o == "--list" || *o == "-a"))
                || !pos.is_empty() && !has("--list")
            {
                return Some("branch is only listed".into());
            }
        }
        "stash" => {
            if pos.first().is_some_and(|p| *p != "push" && *p != "list") {
                return Some("stash only pushes or lists".into());
            }
        }
        "config" => {
            if !opts
                .iter()
                .any(|o| matches!(*o, "--get" | "--get-regexp" | "--get-all" | "--list"))
            {
                return Some("config is only read".into());
            }
        }
        "remote" => {
            let add_github = pos.first() == Some(&"add")
                && pos.len() == 3
                && opts.is_empty()
                && pos[2].starts_with("https://github.com/");
            if !(add_github || pos.first() == Some(&"get-url") || (pos.is_empty() && has("-v"))) {
                return Some("remote is only read (or a GitHub remote added)".into());
            }
        }
        "rm" => {
            if !has("--cached") {
                return Some("rm only untracks (--cached)".into());
            }
        }
        "symbolic-ref" => {
            if pos.len() > 1 || opts.iter().any(|o| *o == "-d" || *o == "--delete") {
                return Some("symbolic-ref is only read".into());
            }
        }
        _ => {}
    }
    None
}

pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 200
        && !b.starts_with(['-', '/', '.', '+'])
        && !b.ends_with(['/', '.'])
        && !b.ends_with(".lock")
        && !b.contains("..")
        && !b.contains("@{")
        && !b.contains("//")
        && b != "@"
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
}

pub fn valid_remote(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 100
        && !r.starts_with(['-', '.'])
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub fn machine_name() -> String {
    machine_slug(&host_name())
}

#[cfg(unix)]
fn host_name() -> String {
    let mut buf = [0u8; 256];

    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    if ok {
        let n = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..n]).into_owned()
    } else {
        String::new()
    }
}

#[cfg(not(unix))]
fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

pub fn machine_slug(raw: &str) -> String {
    let first = raw.split('.').next().unwrap_or("");
    let s: String = first
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "machine".into()
    } else {
        s.chars().take(40).collect()
    }
}

pub fn diverged_branch(machine: &str, branch: &str, date: &str) -> String {
    format!("godterm/{}/{branch}-{date}", machine_slug(machine))
}

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

pub fn push_args(remote: &str, branch: &str, set_upstream: bool) -> Result<Vec<String>, String> {
    if !valid_remote(remote) || !valid_branch(branch) {
        return Err("bad remote or branch name".into());
    }
    let mut a = v(&["push", "--porcelain"]);
    if set_upstream {
        a.push("-u".into());
    }
    a.push(remote.into());
    a.push(format!("refs/heads/{branch}:refs/heads/{branch}"));
    Ok(a)
}

pub fn push_as_args(remote: &str, local: &str, target: &str) -> Result<Vec<String>, String> {
    if !valid_remote(remote) || !valid_branch(local) || !valid_branch(target) {
        return Err("bad remote or branch name".into());
    }
    Ok(vec![
        "push".into(),
        "--porcelain".into(),
        remote.into(),
        format!("refs/heads/{local}:refs/heads/{target}"),
    ])
}

pub fn pull_args() -> Vec<String> {
    v(&["pull", "--ff-only", "--no-edit", "--no-rebase"])
}

pub fn fetch_args(remote: &str) -> Result<Vec<String>, String> {
    if !valid_remote(remote) {
        return Err("bad remote name".into());
    }
    Ok(vec![
        "fetch".into(),
        "--quiet".into(),
        "--no-recurse-submodules".into(),
        remote.into(),
    ])
}

pub fn commit_args(message: &str) -> Result<Vec<String>, String> {
    let m = message.trim();
    if m.is_empty() {
        return Err("the commit message is empty".into());
    }
    if m.len() > 20_000 || m.contains('\0') {
        return Err("the commit message is too long".into());
    }
    Ok(vec!["commit".into(), "-q".into(), "-m".into(), m.into()])
}

pub fn add_all_args() -> Vec<String> {
    v(&["add", "-A", "--", "."])
}

pub fn switch_args(branch: &str) -> Result<Vec<String>, String> {
    if !valid_branch(branch) {
        return Err("bad branch name".into());
    }
    Ok(vec!["switch".into(), "-q".into(), branch.into()])
}

pub fn create_branch_args(name: &str) -> Result<Vec<String>, String> {
    if !valid_branch(name) {
        return Err(format!("\"{name}\" is not a usable branch name"));
    }
    Ok(vec!["switch".into(), "-q".into(), "-c".into(), name.into()])
}

pub fn stash_args(note: &str) -> Vec<String> {
    vec![
        "stash".into(),
        "push".into(),
        "-q".into(),
        "-m".into(),
        note.chars().filter(|c| !c.is_control()).take(200).collect(),
    ]
}

pub fn untrack_args(files: &[String]) -> Result<Vec<String>, String> {
    if files.is_empty() {
        return Err("no files".into());
    }
    let mut a = v(&["rm", "--cached", "-q", "--"]);
    for f in files {
        if f.starts_with('/') || f.split('/').any(|p| p == "..") {
            return Err(format!("{f} is not inside the repo"));
        }
        a.push(f.clone());
    }
    Ok(a)
}

pub fn remote_add_args(name: &str, owner: &str, repo: &str) -> Result<Vec<String>, String> {
    if !valid_remote(name)
        || !crate::git_map::valid_owner(owner)
        || !crate::git_map::valid_repo(repo)
    {
        return Err("bad remote, owner or repo name".into());
    }
    Ok(vec![
        "remote".into(),
        "add".into(),
        name.into(),
        format!("https://github.com/{owner}/{repo}.git"),
    ])
}

pub fn init_args() -> Vec<String> {
    v(&["init", "-q", "-b", "main"])
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn full_args(ctx: &Ctx, args: &[String]) -> Vec<String> {
    let mut a: Vec<String> = vec![];
    if cfg!(test) {
        for kv in [
            "user.name=GodTerm Test",
            "user.email=test@example.invalid",
            "commit.gpgsign=false",
            "init.defaultBranch=main",
        ] {
            a.push("-c".into());
            a.push(kv.into());
        }
    }
    if let (Some(acct), Some(bin)) = (&ctx.account, &ctx.helper) {
        if crate::git_map::valid_owner(acct) {
            a.extend(["-c".to_string(), "credential.helper=".to_string()]);
            a.push("-c".into());
            a.push(format!(
                "credential.https://github.com.helper=!{} git-credential --account {acct}",
                sh_quote(&bin.to_string_lossy())
            ));
            a.extend(["-c".to_string(), "credential.useHttpPath=true".to_string()]);
        }
    }
    a.extend(["-c".to_string(), "core.quotePath=false".to_string()]);
    a.extend([
        "-C".to_string(),
        crate::fspath::plain(&ctx.top)
            .to_string_lossy()
            .into_owned(),
    ]);
    a.extend(args.iter().cloned());
    a
}

pub fn run(ctx: &Ctx, args: &[String]) -> Res<String> {
    run_in(ctx, args, None).map(|(o, _)| o)
}

pub fn run_in(ctx: &Ctx, args: &[String], input: Option<&[u8]>) -> Res<(String, String)> {
    let (ok, out, err) = run_any(ctx, args, input)?;
    if ok {
        Ok((out, err))
    } else {
        Err(Fail::Msg(git_error(&err, &out)))
    }
}

pub fn run_any(ctx: &Ctx, args: &[String], input: Option<&[u8]>) -> Res<(bool, String, String)> {
    let full = full_args(ctx, args);
    if let Some(why) = forbidden(&full) {
        crate::log::error(&format!("git_ops: refused {args:?}: {why}"));
        return Err(Fail::Msg(format!("refused: {why}")));
    }
    let mut c = Command::new("git");
    c.args(&full);
    crate::project_git::quiet_env(&mut c);
    if let Some(gh) = &ctx.gh {
        c.env("GODTERM_GH_BIN", gh);
    }

    c.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN");
    crate::project_git::exec(c, ctx.limit, input).map_err(Fail::Msg)
}

fn git_error(err: &str, out: &str) -> String {
    let lines: Vec<&str> = err
        .lines()
        .chain(out.lines())
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("hint:"))
        .collect();
    let s = if lines.is_empty() {
        "git failed".to_string()
    } else {
        lines
            .iter()
            .take(6)
            .copied()
            .collect::<Vec<_>>()
            .join(" / ")
    };
    crate::sessions::snippet(&s, 400)
}

fn read(ctx: &Ctx, a: &[&str]) -> Option<String> {
    run(ctx, &v(a)).ok().map(|s| s.trim().to_string())
}

pub fn head(ctx: &Ctx) -> Option<String> {
    read(ctx, &["rev-parse", "-q", "--verify", "HEAD"]).filter(|s| !s.is_empty())
}

pub fn branch(ctx: &Ctx) -> Option<String> {
    read(ctx, &["symbolic-ref", "--short", "-q", "HEAD"]).filter(|s| !s.is_empty())
}

pub fn upstream(ctx: &Ctx) -> Option<String> {
    read(
        ctx,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .filter(|s| !s.is_empty())
}

pub fn remotes(ctx: &Ctx) -> Vec<String> {
    read(ctx, &["remote", "-v"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .fold(vec![], |mut acc, r| {
            if !acc.contains(&r) {
                acc.push(r);
            }
            acc
        })
}

pub fn push_remote(ctx: &Ctx) -> Option<String> {
    if let Some(u) = upstream(ctx) {
        if let Some((r, _)) = u.split_once('/') {
            return Some(r.to_string());
        }
    }
    let rs = remotes(ctx);
    if rs.iter().any(|r| r == "origin") {
        return Some("origin".into());
    }
    (rs.len() == 1).then(|| rs[0].clone())
}

pub fn counts(ctx: &Ctx) -> Option<(u32, u32)> {
    let t = read(ctx, &["rev-list", "--left-right", "--count", "HEAD...@{u}"])?;
    let mut it = t.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

pub fn tracked_dirty(ctx: &Ctx) -> bool {
    read(ctx, &["status", "--porcelain=v1", "--untracked-files=no"]).is_some_and(|s| !s.is_empty())
}

pub fn commit(ctx: &Ctx, message: &str, all: bool) -> Res<String> {
    let args = commit_args(message)?;
    if all {
        run(ctx, &add_all_args())?;
    }
    let staged = read(ctx, &["diff", "--cached", "--name-only"]).unwrap_or_default();
    if staged.is_empty() {
        return Err(Fail::Msg("nothing to commit".into()));
    }
    run(ctx, &args)?;
    let h = read(ctx, &["log", "-1", "--format=%h"]).unwrap_or_default();
    let n = staged.lines().count();
    Ok(format!(
        "Committed {h}: {} ({}).",
        crate::sessions::snippet(message.lines().next().unwrap_or(""), 60),
        crate::control::plural(n, "file")
    ))
}

fn rejected(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("non-fast-forward")
        || l.contains("fetch first")
        || l.contains("[rejected]")
        || l.contains("rejected")
}

pub fn push(ctx: &Ctx) -> Res<String> {
    let b = branch(ctx).ok_or_else(|| Fail::Msg("not on a branch (detached)".into()))?;
    let remote = push_remote(ctx).ok_or_else(|| Fail::Msg("no remote to push to".into()))?;
    let first = upstream(ctx).is_none();
    let args = push_args(&remote, &b, first)?;
    match run(ctx, &args) {
        Ok(_) => Ok(format!("Pushed {b} to {remote}.")),
        Err(Fail::Msg(e)) if rejected(&e) => {
            let _ = fetch(ctx);
            let (a, bh) = counts(ctx).unwrap_or((0, 0));
            Err(Fail::Diverged {
                ahead: a,
                behind: bh,
            })
        }
        Err(e) => Err(e),
    }
}

pub fn pull(ctx: &Ctx) -> Res<String> {
    if upstream(ctx).is_none() {
        return Err(Fail::Msg(
            "the branch has no upstream yet: push it first".into(),
        ));
    }
    let _ = fetch(ctx);
    if let Some((a, b)) = counts(ctx) {
        if a > 0 && b > 0 {
            return Err(Fail::Diverged {
                ahead: a,
                behind: b,
            });
        }
        if b == 0 {
            return Ok("Already up to date.".into());
        }
    }
    run(ctx, &pull_args())?;
    Ok("Pulled (fast forward).".into())
}

pub fn fetch(ctx: &Ctx) -> Res<String> {
    let remote = push_remote(ctx).ok_or_else(|| Fail::Msg("no remote to fetch".into()))?;
    run(ctx, &fetch_args(&remote)?)?;
    Ok(format!("Fetched {remote}."))
}

pub fn sync(ctx: &Ctx) -> Res<String> {
    fetch(ctx)?;
    let Some((a, b)) = counts(ctx) else {
        return push(ctx);
    };
    if a > 0 && b > 0 {
        return Err(Fail::Diverged {
            ahead: a,
            behind: b,
        });
    }
    let mut said = vec![];
    if b > 0 {
        run(ctx, &pull_args())?;
        said.push(format!("pulled {b}"));
    }
    if a > 0 {
        push(ctx)?;
        said.push(format!("pushed {a}"));
    }
    if said.is_empty() {
        Ok("In sync.".into())
    } else {
        Ok(format!("Synced: {}.", said.join(", ")))
    }
}

pub fn push_as_branch(ctx: &Ctx, machine: &str, date: &str) -> Res<String> {
    let b = branch(ctx).ok_or_else(|| Fail::Msg("not on a branch (detached)".into()))?;
    let remote = push_remote(ctx).ok_or_else(|| Fail::Msg("no remote to push to".into()))?;
    let base = diverged_branch(machine, &b, date);
    let mut last = String::new();
    for k in 1..=9 {
        let target = if k == 1 {
            base.clone()
        } else {
            format!("{base}-{k}")
        };
        match run(ctx, &push_as_args(&remote, &b, &target)?) {
            Ok(_) => {
                return Ok(format!(
                    "Pushed your {b} commits to {remote} as branch {target}. Nothing was rewritten."
                ))
            }
            Err(Fail::Msg(e)) if rejected(&e) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(Fail::Msg(format!("every branch name was taken: {last}")))
}

pub fn switch(ctx: &Ctx, to: &str, stash: bool) -> Res<String> {
    let args = switch_args(to)?;
    let mut note = String::new();
    if tracked_dirty(ctx) {
        if !stash {
            return Err(Fail::Msg(
                "uncommitted changes: commit them, or switch with stash".into(),
            ));
        }
        let from = branch(ctx).unwrap_or_else(|| "HEAD".into());
        run(
            ctx,
            &stash_args(&format!("godterm: before switching from {from} to {to}")),
        )?;
        note = " Your changes are in the stash.".into();
    }
    run(ctx, &args)?;
    Ok(format!("Switched to {to}.{note}"))
}

pub fn create_branch(ctx: &Ctx, name: &str) -> Res<String> {
    let args = create_branch_args(name)?;
    run(ctx, &args)?;
    Ok(format!("Created and switched to {name}."))
}

pub fn init(dir: &Path) -> Res<String> {
    let ctx = Ctx::new(dir);
    run(&ctx, &init_args())?;
    Ok("Made it a git repo (branch main).".into())
}

pub fn untrack(ctx: &Ctx, files: &[String]) -> Res<String> {
    run(ctx, &untrack_args(files)?)?;
    Ok(format!(
        "Untracked {} (the files stay on disk).",
        crate::control::plural(files.len(), "file")
    ))
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct LogRow {
    pub hash: String,
    pub subject: String,
    pub when: String,
    pub author: String,
    pub unpushed: bool,
}

pub fn log(ctx: &Ctx, n: usize) -> Vec<LogRow> {
    let cap = format!("-n{}", n.clamp(1, 200));
    let Some(t) = read(
        ctx,
        &["log", &cap, "--format=%h%x1f%s%x1f%cr%x1f%an", "HEAD", "--"],
    ) else {
        return vec![];
    };
    let has_remote = !remotes(ctx).is_empty();
    let unp: Vec<String> = if has_remote {
        read(
            ctx,
            &[
                "rev-list",
                "--abbrev-commit",
                &cap,
                "HEAD",
                "--not",
                "--remotes",
            ],
        )
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
    } else {
        vec![]
    };
    t.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('\x1f').collect();
            (p.len() == 4).then(|| LogRow {
                hash: p[0].into(),
                subject: crate::sessions::snippet(p[1], 100),
                when: p[2].into(),
                author: p[3].into(),
                unpushed: unp
                    .iter()
                    .any(|u| u.starts_with(p[0]) || p[0].starts_with(u.as_str())),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct FileChange {
    pub path: String,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    pub staged: bool,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Changes {
    pub files: Vec<FileChange>,
    pub untracked: Vec<String>,
    pub staged_n: usize,
    pub unstaged_n: usize,
    pub added: u32,
    pub removed: u32,
}

fn numstat(t: &str, staged: bool) -> Vec<FileChange> {
    t.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let a = it.next()?;
            let r = it.next()?;
            let p = it.next()?;
            Some(FileChange {
                path: p.to_string(),
                added: a.parse().ok(),
                removed: r.parse().ok(),
                staged,
            })
        })
        .collect()
}

pub fn changes(ctx: &Ctx) -> Changes {
    let st = numstat(
        &read(ctx, &["diff", "--cached", "--numstat", "--no-renames"]).unwrap_or_default(),
        true,
    );
    let un = numstat(
        &read(ctx, &["diff", "--numstat", "--no-renames"]).unwrap_or_default(),
        false,
    );
    let untracked: Vec<String> = read(ctx, &["ls-files", "--others", "--exclude-standard"])
        .unwrap_or_default()
        .lines()
        .take(500)
        .map(str::to_string)
        .collect();
    let mut c = Changes {
        staged_n: st.len(),
        unstaged_n: un.len(),
        untracked,
        ..Default::default()
    };
    for f in st.iter().chain(un.iter()) {
        c.added += f.added.unwrap_or(0);
        c.removed += f.removed.unwrap_or(0);
    }
    c.files = st.into_iter().chain(un).collect();
    c
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BranchRow {
    pub name: String,
    pub upstream: Option<String>,
    pub current: bool,
    pub when: String,
}

pub fn branches(ctx: &Ctx) -> Vec<BranchRow> {
    read(
        ctx,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(upstream:short)%09%(HEAD)%09%(committerdate:relative)",
            "refs/heads",
        ],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| {
        let p: Vec<&str> = l.split('\t').collect();
        (p.len() == 4).then(|| BranchRow {
            name: p[0].into(),
            upstream: (!p[1].is_empty()).then(|| p[1].to_string()),
            current: p[2] == "*",
            when: p[3].into(),
        })
    })
    .collect()
}

pub fn outgoing_range(ctx: &Ctx) -> Vec<String> {
    match upstream(ctx) {
        Some(u) => vec![format!("{u}..HEAD")],
        None => vec!["HEAD".into(), "--not".into(), "--remotes".into()],
    }
}

pub fn diff_text(ctx: &Ctx, staged_only: bool, file: Option<&str>, cap: usize) -> String {
    let mut a = vec!["diff", "--no-color", "--no-ext-diff", "-U3"];
    let head_ok = head(ctx).is_some();
    if staged_only {
        a.push("--cached");
    } else if head_ok {
        a.push("HEAD");
    }
    a.push("--");
    if let Some(f) = file {
        a.push(f);
    }
    let t = read(ctx, &a).unwrap_or_default();
    if t.len() > cap {
        let mut end = cap;
        while !t.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n[... cut at {cap} bytes]", &t[..end])
    } else {
        t
    }
}

#[cfg(test)]
#[path = "git_ops_tests.rs"]
pub(crate) mod tests;
