//! Every known project is part of the machine state (its bindings, .env
//! files and conversations) whether or not it has a repo. GodTerm never
//! commits to, pushes or creates a project repo on its own: that is only
//! ever the user's action in the Git section. What the sync can do is
//! say which projects are not backed up, read only: no repo, no GitHub
//! remote, a remote that is only a folder here, unpushed commits,
//! uncommitted changes.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Health {
    /// Portable path.
    pub path: String,
    /// "not backed up: no git repo", "12 unpushed commits", ...
    pub warnings: Vec<String>,
}

fn git_ro(dir: &Path, args: &[&str]) -> Option<String> {
    // Read only commands, with no optional locks (never writes the index).
    let out = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap_or(dir))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The warnings for one project folder (empty: backed up on GitHub).
pub fn check(dir: &Path) -> Vec<String> {
    let mut w = vec![];
    if git_ro(dir, &["rev-parse", "--show-toplevel"]).is_none() {
        w.push("not backed up: no git repo".into());
        return w;
    }
    let remotes = git_ro(dir, &["remote", "-v"]).unwrap_or_default();
    if remotes.trim().is_empty() {
        w.push("not backed up: no GitHub repo (no remote)".into());
    } else if !remotes.contains("github.com") {
        let local = remotes
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1))
            .all(|u| u.starts_with('/') || u.starts_with("file://") || u.starts_with('.'));
        w.push(if local {
            "not backed up: the remote is only a folder on this machine".into()
        } else {
            "no GitHub repo (the remote is on another host)".into()
        });
    }
    let st = git_ro(dir, &["status", "--porcelain=v1", "-b"]).unwrap_or_default();
    let mut lines = st.lines();
    if let Some(head) = lines.next() {
        if let Some(i) = head.find("[ahead ") {
            let n: String = head[i + 7..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            w.push(format!("{n} unpushed commits"));
        } else if !head.contains("...") && !remotes.trim().is_empty() {
            w.push("branch not pushed (no upstream)".into());
        }
    }
    let dirty = lines.count();
    if dirty > 0 {
        w.push(format!("uncommitted changes ({dirty} files)"));
    }
    w
}
