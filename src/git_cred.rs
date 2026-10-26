//! Credentials for project repos, per gh account, without ever switching
//! gh's active account or storing a token.
//!
//! - `godterm git-credential --account <acct> get` speaks git's credential
//!   protocol: it reads the request on stdin and, for https://github.com
//!   only, answers with the token `gh auth token --hostname github.com
//!   --user <acct>` prints. The token goes from gh's stdout to git's stdin
//!   and nowhere else: never logged, never an argument, never on disk.
//! - `gh` API calls (repo view, repo create) get the same token as
//!   GH_TOKEN in their own environment, never as an argument.
//! - Never `gh auth switch`, logout, a public or internal repo, or a repo
//!   delete: `gh_args_ok` refuses them. The one login is the user's "Add
//!   account..." (gh_login.rs), its exact argument list only.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The gh binary: `[git] gh_bin`, the helper's GODTERM_GH_BIN, else gh.
pub fn gh_bin(cfg: Option<&crate::git_map::GitCfg>) -> PathBuf {
    if let Some(b) = cfg.and_then(|c| c.gh_bin.clone()).filter(|b| !b.is_empty()) {
        return crate::config::expand_tilde(&b);
    }
    if cfg!(test) {
        // Tests never reach the real gh (and its keyring).
        return PathBuf::from("/nonexistent/gh");
    }
    if let Some(b) = std::env::var_os("GODTERM_GH_BIN").filter(|b| !b.is_empty()) {
        return PathBuf::from(b);
    }
    for c in ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"] {
        if Path::new(c).exists() {
            return PathBuf::from(c);
        }
    }
    PathBuf::from("gh")
}

/// gh argument lists GodTerm may run.
pub fn gh_args_ok(args: &[&str]) -> Result<(), String> {
    let low: Vec<String> = args.iter().map(|a| a.to_ascii_lowercase()).collect();
    let has = |x: &str| low.iter().any(|a| a == x);
    let sub = (
        low.first().map(String::as_str).unwrap_or(""),
        low.get(1).map(String::as_str).unwrap_or(""),
    );
    if has("--public") || has("--internal") || low.iter().any(|a| a.starts_with("--visibility")) {
        return Err("GodTerm never makes a repo public or internal".into());
    }
    match sub {
        ("auth", "token") | ("auth", "status") => Ok(()),
        // The user's own "Add account..." (gh_login): exactly this list.
        ("auth", "login") if args == crate::gh_login::ARGS => Ok(()),
        ("repo", "view") => Ok(()),
        ("repo", "create") if has("--private") && !has("--push") => Ok(()),
        ("repo", "create") => Err("repos are created --private, without --push".into()),
        ("auth", x) => Err(format!("gh auth {x} is never run (no account switching)")),
        (a, b) => Err(format!("gh {a} {b} is never run by GodTerm")),
    }
}

fn gh_env(c: &mut Command) {
    c.env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .env("GH_SPINNER_DISABLED", "1");
}

/// The token of gh account `account` (from gh's own store).
pub fn gh_token(gh: &Path, account: &str) -> Result<String, String> {
    if !crate::git_map::valid_owner(account) {
        return Err("bad account name".into());
    }
    let args = [
        "auth",
        "token",
        "--hostname",
        "github.com",
        "--user",
        account,
    ];
    gh_args_ok(&args)?;
    let mut c = Command::new(gh);
    c.args(args);
    gh_env(&mut c);
    let (ok, out, err) = crate::project_git::exec(c, Duration::from_secs(20), None)?;
    let tok = out.trim().to_string();
    if !ok || tok.is_empty() || tok.contains(char::is_whitespace) {
        // gh's message names the account, never a token.
        let e = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!(
            "gh has no token for {account}{}",
            if e.is_empty() {
                String::new()
            } else {
                format!(": {}", crate::sessions::snippet(e.trim(), 120))
            }
        ));
    }
    Ok(tok)
}

/// A gh command for `account` with its token in GH_TOKEN (never argv).
pub fn gh_command(gh: &Path, account: &str, args: &[&str]) -> Result<Command, String> {
    gh_args_ok(args)?;
    let tok = gh_token(gh, account)?;
    let mut c = Command::new(gh);
    c.args(args);
    gh_env(&mut c);
    c.env("GH_TOKEN", tok);
    Ok(c)
}

/// git's credential request, parsed.
fn parse_request(input: &str) -> Vec<(String, String)> {
    input
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// The helper's answer to `op` with request `input` (empty: say nothing,
/// so git tries no other helper and fails cleanly).
pub fn respond(op: &str, input: &str, account: &str, gh: &Path) -> Result<String, String> {
    if op != "get" {
        // store and erase: GodTerm keeps nothing.
        return Ok(String::new());
    }
    let req = parse_request(input);
    let get = |k: &str| req.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
    if get("protocol") != Some("https")
        || get("host").map(str::to_ascii_lowercase).as_deref() != Some("github.com")
    {
        return Ok(String::new());
    }
    let tok = gh_token(gh, account)?;
    Ok(format!("username={account}\npassword={tok}\n"))
}

/// `godterm git-credential --account <acct> <get|store|erase>`.
pub fn helper_main(args: &[String]) -> anyhow::Result<()> {
    let account = args
        .iter()
        .position(|a| a == "--account")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let op = args
        .iter()
        .rev()
        .find(|a| matches!(a.as_str(), "get" | "store" | "erase"))
        .cloned()
        .unwrap_or_default();
    let mut input = String::new();
    let stdin = std::io::stdin();
    for l in stdin.lock().lines() {
        let l = l?;
        if l.trim().is_empty() {
            break;
        }
        input.push_str(&l);
        input.push('\n');
        if input.len() > 64 * 1024 {
            break;
        }
    }
    match respond(&op, &input, &account, &gh_bin(None)) {
        Ok(out) => {
            let mut so = std::io::stdout().lock();
            so.write_all(out.as_bytes())?;
            so.flush()?;
            Ok(())
        }
        Err(e) => {
            // To git's stderr only (shown to the user as git's message).
            eprintln!("godterm git-credential: {e}");
            std::process::exit(1);
        }
    }
}

/// The gh accounts logged in on github.com (`gh auth status`), never
/// with tokens.
pub fn accounts(gh: &Path) -> Vec<String> {
    status_text(gh)
        .map(|t| parse_status(&t))
        .unwrap_or_default()
}

/// What `gh auth status --hostname github.com` printed (gh masks the
/// tokens; callers keep only the account names from it).
pub fn status_text(gh: &Path) -> Option<String> {
    let args = ["auth", "status", "--hostname", "github.com"];
    gh_args_ok(&args).ok()?;
    let mut c = Command::new(gh);
    c.args(args);
    gh_env(&mut c);
    let (_, out, err) = crate::project_git::exec(c, Duration::from_secs(15), None).ok()?;
    Some(format!("{out}\n{err}"))
}

pub fn parse_status(text: &str) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for l in text.lines() {
        let Some(i) = l.find(" account ") else {
            continue;
        };
        let name = l[i + 9..]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
        if crate::git_map::valid_owner(name) && !v.iter().any(|x| x == name) {
            v.push(name.to_string());
        }
    }
    v
}

// ---- creating a private repo ----

/// `gh repo create owner/name --private --source dir --remote R`. Never
/// `--public`, never `--push` (the first push is its own step).
pub fn create_args(
    owner: &str,
    name: &str,
    dir: &Path,
    remote: &str,
) -> Result<Vec<String>, String> {
    if !crate::git_map::valid_owner(owner) || !crate::git_map::valid_repo(name) {
        return Err("bad owner or repo name".into());
    }
    if !crate::git_ops::valid_remote(remote) {
        return Err("bad remote name".into());
    }
    Ok(vec![
        "repo".into(),
        "create".into(),
        format!("{owner}/{name}"),
        "--private".into(),
        "--source".into(),
        dir.to_string_lossy().into_owned(),
        "--remote".into(),
        remote.into(),
    ])
}

/// The visibility gh reports for owner/name: Ok(None) when it does not
/// exist (or this account cannot see it).
pub fn visibility(
    gh: &Path,
    account: &str,
    owner: &str,
    name: &str,
) -> Result<Option<String>, String> {
    let slug = format!("{owner}/{name}");
    let args = [
        "repo",
        "view",
        slug.as_str(),
        "--json",
        "visibility,nameWithOwner",
    ];
    let c = gh_command(gh, account, &args)?;
    let (ok, out, err) = crate::project_git::exec(c, Duration::from_secs(30), None)?;
    if !ok {
        let e = err.to_ascii_lowercase();
        if e.contains("could not resolve") || e.contains("not found") || e.contains("404") {
            return Ok(None);
        }
        return Err(crate::sessions::snippet(
            err.lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("gh failed")
                .trim(),
            200,
        ));
    }
    let v: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| e.to_string())?;
    Ok(v["visibility"].as_str().map(|s| s.to_ascii_uppercase()))
}

/// Create the private repo and check gh says PRIVATE after. Err with why;
/// a repo that is not private afterwards is reported loudly.
pub fn create_private(
    gh: &Path,
    account: &str,
    owner: &str,
    name: &str,
    dir: &Path,
    remote: &str,
) -> Result<String, String> {
    let args = create_args(owner, name, dir, remote)?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut c = gh_command(gh, account, &refs)?;
    c.current_dir(dir);
    let (ok, _out, err) = crate::project_git::exec(c, Duration::from_secs(60), None)?;
    if !ok {
        return Err(crate::sessions::snippet(
            err.lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("gh repo create failed")
                .trim(),
            200,
        ));
    }
    match visibility(gh, account, owner, name)? {
        Some(v) if v == "PRIVATE" => Ok(format!("Created {owner}/{name} on GitHub (private, checked).")),
        Some(v) => Err(format!(
            "{owner}/{name} was created but GitHub says it is {v}, not PRIVATE: make it private on github.com now"
        )),
        None => Err(format!("{owner}/{name} was created but cannot be seen yet; check it on github.com")),
    }
}

#[cfg(test)]
#[path = "git_cred_tests.rs"]
mod tests;
