//! git and gh for the sync repos. Every call is non-interactive (the
//! settings of `project_git::git()`), https remotes on github.com get
//! GodTerm's own credential helper for one gh account (the token comes
//! from `gh auth token --user <acct>` at the moment git asks: never on a
//! command line, never logged, never written to disk; `gh auth switch`
//! is never run), and no push can ever force: [`check_args`] refuses
//! every argument list that would, and a test runs it over every call.
//!
//! Phase 1's git_cred will own the credential helper for project repos;
//! this one is the minimal version the sync needs, to be unified then.

use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the sync repos live: GitHub through gh, or a folder of bare
/// repos standing in for it (`file:///...`, tests and local mirrors).
#[derive(Debug, Clone, PartialEq)]
pub enum Remote {
    GitHub { account: String, exe: PathBuf },
    Local { base: PathBuf },
}

impl Remote {
    pub fn from_cfg(c: &super::SyncCfg, exe: &Path) -> Remote {
        match c.remote.strip_prefix("file://") {
            Some(p) => Remote::Local {
                base: PathBuf::from(p),
            },
            None => Remote::GitHub {
                account: c.account.clone(),
                exe: exe.to_path_buf(),
            },
        }
    }

    pub fn url(&self, owner: &str, name: &str) -> String {
        match self {
            Remote::GitHub { .. } => format!("https://github.com/{owner}/{name}.git"),
            Remote::Local { base } => {
                format!(
                    "file://{}",
                    base.join(owner).join(format!("{name}.git")).display()
                )
            }
        }
    }

    pub fn is_github(&self) -> bool {
        matches!(self, Remote::GitHub { .. })
    }

    /// The credential helper for git calls on this remote.
    pub fn cred(&self) -> Option<Cred> {
        match self {
            Remote::GitHub { account, exe } => Some(Cred {
                account: account.clone(),
                exe: exe.clone(),
            }),
            Remote::Local { .. } => None,
        }
    }

    /// Whether the repo exists.
    pub fn exists(&self, owner: &str, name: &str) -> Result<bool> {
        match self {
            Remote::Local { base } => Ok(base
                .join(owner)
                .join(format!("{name}.git"))
                .join("HEAD")
                .is_file()),
            Remote::GitHub { account, .. } => {
                let out = gh(
                    account,
                    &[
                        "repo",
                        "view",
                        &format!("{owner}/{name}"),
                        "--json",
                        "visibility",
                    ],
                )?;
                Ok(out.status.success())
            }
        }
    }

    /// Create a private repo (GitHub: `gh repo create --private`, then
    /// checked private; there is no public path). Callers ask first.
    pub fn create_private(&self, owner: &str, name: &str) -> Result<()> {
        match self {
            Remote::Local { base } => {
                let d = base.join(owner).join(format!("{name}.git"));
                std::fs::create_dir_all(&d)?;
                run(&d, &["init", "-q", "--bare", "-b", "main"], None)?;
                // Partial clones of it work like GitHub's.
                run(&d, &["config", "uploadpack.allowFilter", "true"], None)?;
                run(
                    &d,
                    &["config", "uploadpack.allowAnySHA1InWant", "true"],
                    None,
                )?;
                Ok(())
            }
            Remote::GitHub { account, .. } => {
                let full = format!("{owner}/{name}");
                let args = create_args(&full);
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                let out = gh(account, &a)?;
                if !out.status.success() {
                    bail!(
                        "gh repo create {full} failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                let v = gh(account, &["repo", "view", &full, "--json", "visibility"])?;
                let text = String::from_utf8_lossy(&v.stdout);
                if !text.contains("PRIVATE") {
                    bail!(
                        "{full} was created but does not read as private: check it on GitHub now"
                    );
                }
                Ok(())
            }
        }
    }
}

impl Remote {
    /// A repo's visibility ("PRIVATE", "PUBLIC", "INTERNAL"); None when it
    /// cannot be told. A folder remote reads a `visibility` file in the
    /// bare repo (tests, local mirrors): no file is unknown.
    pub fn visibility(&self, owner: &str, name: &str) -> Option<String> {
        match self {
            Remote::Local { base } => {
                let f = base
                    .join(owner)
                    .join(format!("{name}.git"))
                    .join("visibility");
                Some(std::fs::read_to_string(f).ok()?.trim().to_uppercase())
            }
            Remote::GitHub { account, .. } => visibility(account, &format!("{owner}/{name}")),
        }
    }
}

/// `gh repo create` arguments: always private, never pushed by gh.
pub fn create_args(full: &str) -> Vec<String> {
    vec![
        "repo".into(),
        "create".into(),
        full.into(),
        "--private".into(),
        "--description".into(),
        "GodTerm sync (private: never make public)".into(),
    ]
}

/// The credential helper for one gh account.
#[derive(Debug, Clone, PartialEq)]
pub struct Cred {
    pub account: String,
    pub exe: PathBuf,
}

impl Cred {
    /// `-c` settings that make git ask this program, and only for
    /// https://github.com.
    pub fn config_args(&self) -> Vec<String> {
        let exe = self.exe.to_string_lossy().replace('\'', "'\\''");
        vec![
            "-c".into(),
            "credential.helper=".into(),
            "-c".into(),
            format!(
                "credential.https://github.com.helper=!'{exe}' sync git-credential --account {}",
                self.account
            ),
            "-c".into(),
            "credential.useHttpPath=true".into(),
        ]
    }
}

/// A gh account name as GitHub allows them.
pub fn valid_account(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 39
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Arguments that would force a push, or rewrite a remote ref.
pub fn check_args(args: &[String]) -> Result<()> {
    let is_push = args.iter().any(|a| a == "push");
    for a in args {
        let bad = matches!(
            a.as_str(),
            "--force" | "-f" | "--mirror" | "--delete" | "-d" | "--prune"
        ) || a.starts_with("--force")
            || (is_push && (a.starts_with('+') || a.starts_with(':')))
            || (is_push && a.contains(":+"));
        if bad && is_push {
            bail!("refused: a sync push never forces or deletes ({a})");
        }
        if a.starts_with("--force") {
            bail!("refused: {a}");
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod record {
    //! Every argument list the sync built, for the no-force test.
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    /// (the repo folder, the arguments).
    pub static CALLS: Mutex<Vec<(PathBuf, Vec<String>)>> = Mutex::new(Vec::new());
    pub fn push(dir: &Path, a: &[String]) {
        CALLS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((dir.to_path_buf(), a.to_vec()));
    }
    pub fn all() -> Vec<(PathBuf, Vec<String>)> {
        CALLS.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The mark of GodTerm's own sync repos (the state repo's working copy,
/// the conversation repos). Only these are ever committed to or pushed:
/// project repos are touched only by the user from the Git section.
pub const MARK: &str = "godterm-sync";

/// Mark a sync repo (its git dir gets the mark file).
pub fn mark(dir: &Path) -> Result<()> {
    let gd = if dir.join(".git").is_dir() {
        dir.join(".git")
    } else {
        dir.to_path_buf()
    };
    std::fs::write(gd.join(MARK), "GodTerm Sync's own repo\n")?;
    Ok(())
}

pub fn is_sync_repo(dir: &Path) -> bool {
    dir.join(".git").join(MARK).is_file() || dir.join(MARK).is_file()
}

/// Subcommands that write history or remote refs.
const WRITES: &[&str] = &[
    "commit",
    "commit-tree",
    "push",
    "merge",
    "rebase",
    "update-ref",
    "tag",
];

/// A non-interactive git in `dir`.
pub fn git(dir: &Path, cred: Option<&Cred>, args: &[String]) -> Result<Command> {
    check_args(args)?;
    if let Some(sub) = args.iter().find(|a| !a.starts_with('-')) {
        if WRITES.contains(&sub.as_str()) && !is_sync_repo(dir) {
            bail!(
                "refused: git {sub} in {}: GodTerm Sync only writes its own repos",
                dir.display()
            );
        }
    }
    #[cfg(test)]
    record::push(dir, args);
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    if let Some(cr) = cred {
        if !valid_account(&cr.account) {
            bail!("bad gh account name {:?}", cr.account);
        }
        c.args(cr.config_args());
    }
    c.args(["-c", "core.fsmonitor=false"])
        .args(["-c", "gc.auto=0"])
        .args(["-c", "maintenance.auto=false"])
        .args(["-c", "core.autocrlf=false"])
        .args(["-c", "protocol.file.allow=always"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("SSH_ASKPASS", "echo")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=10",
        )
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "GodTerm Sync")
        .env("GIT_AUTHOR_EMAIL", "sync@godterm.invalid")
        .env("GIT_COMMITTER_NAME", "GodTerm Sync")
        .env("GIT_COMMITTER_EMAIL", "sync@godterm.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null());
    if cfg!(test) {
        // Tests never read the user's own git settings.
        c.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
    }
    Ok(c)
}

fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// Run git; stdout on success, stderr in the error.
pub fn run(dir: &Path, args: &[&str], cred: Option<&Cred>) -> Result<String> {
    run_env(dir, args, cred, &[])
}

pub fn run_env(
    dir: &Path,
    args: &[&str],
    cred: Option<&Cred>,
    env: &[(&str, &Path)],
) -> Result<String> {
    let mut c = git(dir, cred, &owned(args))?;
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().context("running git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Bytes on stdout (cat-file).
pub fn run_bytes(dir: &Path, args: &[&str], cred: Option<&Cred>) -> Result<Vec<u8>> {
    let out = git(dir, cred, &owned(args))?.output()?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Write bytes into the object store, returning the blob id.
pub fn hash_object(dir: &Path, data: &[u8]) -> Result<String> {
    let mut c = git(dir, None, &owned(&["hash-object", "-w", "--stdin"]))?;
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn()?;
    {
        let mut i = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        i.write_all(data)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "git hash-object: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// How a push went.
#[derive(Debug, Clone, PartialEq)]
pub enum Pushed {
    Done,
    /// Someone else pushed first (not a fast forward): fetch and redo.
    Rejected,
}

/// `git push <remote> <src>:refs/heads/<branch>`, never forced.
pub fn push(
    dir: &Path,
    remote: &str,
    src: &str,
    branch: &str,
    cred: Option<&Cred>,
) -> Result<Pushed> {
    let spec = format!("{src}:refs/heads/{branch}");
    let args = owned(&["push", "--porcelain", remote, &spec]);
    let out = git(dir, cred, &args)?.output()?;
    if out.status.success() {
        return Ok(Pushed::Done);
    }
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if all.contains("[rejected]")
        || all.contains("non-fast-forward")
        || all.contains("fetch first")
        || all.contains("stale info")
        // The ref moved between our read and our write (two pushes at
        // once): the atomic update refused ours.
        || all.contains("incorrect old value")
        || all.contains("cannot lock ref")
        || all.contains("failed to update ref")
    {
        return Ok(Pushed::Rejected);
    }
    bail!("git push: {}", all.trim())
}

/// The commit a ref names, if it exists.
pub fn rev(dir: &Path, r: &str) -> Option<String> {
    run(
        dir,
        &["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")],
        None,
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// Fetch `branch` from origin into `refs/remotes/origin/<branch>`. A
/// missing branch (a new empty repo) is not an error.
pub fn fetch(dir: &Path, branch: &str, cred: Option<&Cred>) -> Result<()> {
    let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    match run(dir, &["fetch", "-q", "--no-tags", "origin", &spec], cred) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("couldn't find remote ref") => Ok(()),
        Err(e) => Err(e),
    }
}

/// `godterm sync git-credential --account <acct> get|store|erase`: git's
/// credential protocol. `get` for https://github.com answers with the
/// account's gh token; everything else answers nothing.
pub fn credential_command(args: &[String]) -> Result<()> {
    let acct = args
        .iter()
        .position(|a| a == "--account")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let op = args.last().cloned().unwrap_or_default();
    let stdin = std::io::stdin();
    let mut input = String::new();
    for l in stdin.lock().lines() {
        let l = l?;
        if l.is_empty() {
            break;
        }
        input.push_str(&l);
        input.push('\n');
    }
    let out = credential_answer(&op, &acct, &input, "gh")?;
    std::io::stdout().write_all(out.as_bytes())?;
    Ok(())
}

/// The answer to one credential request (`gh` is the program to ask,
/// a stub in tests).
pub fn credential_answer(op: &str, acct: &str, input: &str, gh_prog: &str) -> Result<String> {
    if op != "get" || !valid_account(acct) {
        return Ok(String::new());
    }
    let field = |k: &str| {
        input
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or("")
            .to_string()
    };
    if field("protocol") != "https" || field("host") != "github.com" {
        return Ok(String::new());
    }
    let token = gh_token_with(gh_prog, acct)?;
    Ok(format!("username=x-access-token\npassword={token}\n"))
}

/// One account's gh token (from the keyring through gh). Never logged.
#[cfg_attr(test, allow(dead_code))]
pub fn gh_token(acct: &str) -> Result<String> {
    gh_token_with("gh", acct)
}

fn gh_token_with(prog: &str, acct: &str) -> Result<String> {
    if !valid_account(acct) {
        bail!("bad gh account name");
    }
    #[cfg(test)]
    if prog == "gh" {
        bail!("tests never ask the real gh");
    }
    let out = Command::new(prog)
        .args(["auth", "token", "--hostname", "github.com", "--user", acct])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {prog}"))?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || t.is_empty() {
        bail!("gh has no github.com login for {acct} (run: gh auth login --hostname github.com)");
    }
    Ok(t)
}

/// Run gh as one account: its token in GH_TOKEN (the child's environment,
/// never its arguments), prompts off.
fn gh(acct: &str, args: &[&str]) -> Result<std::process::Output> {
    #[cfg(test)]
    {
        let _ = (acct, args);
        bail!("tests never reach GitHub");
    }
    #[cfg(not(test))]
    {
        let token = gh_token(acct)?;
        Command::new("gh")
            .args(args)
            .env("GH_TOKEN", token)
            .env("GH_PROMPT_DISABLED", "1")
            .env_remove("GITHUB_TOKEN")
            .stdin(Stdio::null())
            .output()
            .context("running gh")
    }
}

/// A repo's visibility on GitHub ("PRIVATE", "PUBLIC", "INTERNAL") as
/// `account` sees it; None when gh cannot say.
pub fn visibility(account: &str, full: &str) -> Option<String> {
    let out = gh(account, &["repo", "view", full, "--json", "visibility"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v["visibility"].as_str().map(str::to_uppercase)
}

/// Which gh accounts are logged in on github.com (`gh auth status`).
pub fn gh_accounts() -> Vec<String> {
    if cfg!(test) {
        return vec![];
    }
    let Ok(out) = Command::new("gh")
        .args(["auth", "status", "--hostname", "github.com"])
        .stdin(Stdio::null())
        .output()
    else {
        return vec![];
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines()
        .filter_map(|l| {
            let i = l.find("account ")?;
            let rest = &l[i + 8..];
            Some(rest.split_whitespace().next()?.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_is_refused_everywhere() {
        for bad in [
            vec!["push", "--force", "origin", "main"],
            vec!["push", "-f", "origin", "main"],
            vec!["push", "--force-with-lease", "origin", "main"],
            vec!["push", "origin", "+main:refs/heads/main"],
            vec!["push", "origin", "HEAD:+refs/heads/main"],
            vec!["push", "origin", ":refs/heads/main"],
            vec!["push", "--delete", "origin", "main"],
            vec!["push", "--mirror", "origin"],
        ] {
            assert!(check_args(&owned(&bad)).is_err(), "{bad:?}");
        }
        assert!(check_args(&owned(&[
            "push",
            "--porcelain",
            "origin",
            "abc:refs/heads/main"
        ]))
        .is_ok());
        // Fetches may update their own tracking refs.
        assert!(check_args(&owned(&[
            "fetch",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main"
        ]))
        .is_ok());
    }

    #[test]
    fn credential_helper_speaks_the_protocol() {
        let d = std::env::temp_dir().join(format!("gt-sync-cred-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let stub = d.join("gh");
        // The stub prints a token only for the right account and records
        // its arguments (never the token's).
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\n[ \"$6\" = alice ] && echo tok_123 && exit 0\nexit 1\n",
                d.join("args").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
            let p = stub.to_string_lossy().into_owned();
            let req = "protocol=https\nhost=github.com\npath=alice/godterm-state.git\n";
            assert_eq!(
                credential_answer("get", "alice", req, &p).unwrap(),
                "username=x-access-token\npassword=tok_123\n"
            );
            assert!(credential_answer("get", "bob", req, &p).is_err());
            assert_eq!(credential_answer("store", "alice", req, &p).unwrap(), "");
            assert_eq!(
                credential_answer("get", "alice", "protocol=https\nhost=gitlab.com\n", &p).unwrap(),
                ""
            );
            assert_eq!(credential_answer("get", "a;rm", req, &p).unwrap(), "");
            let args = std::fs::read_to_string(d.join("args")).unwrap();
            assert!(args.contains("auth token --hostname github.com --user alice"));
            assert!(!args.contains("switch"));
        }
        let c = Cred {
            account: "alice".into(),
            exe: PathBuf::from("/x/god term"),
        };
        let a = c.config_args().join(" ");
        assert!(a.contains("credential.helper= "));
        assert!(a.contains("credential.https://github.com.helper=!'/x/god term' sync git-credential --account alice"));
        assert!(!a.contains("tok_"));
        let ca = create_args("o/n");
        assert!(
            ca.contains(&"--private".to_string())
                && !ca.iter().any(|x| x == "--public" || x == "--push")
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
