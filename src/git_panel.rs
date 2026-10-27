//! The Git section of a tab, for both the terminal UI (ui_git.rs) and the
//! web sheet (web/js/git.js): what it shows (`view`), what a button would
//! do (`prepare`: checks run, nothing changed) and doing it (`execute`,
//! only after the user confirmed in the dialog, with "push anyway" typed
//! when a secret finding stands in the way).
//!
//! The assistant can read all of this (git_status, git_log, git_diff,
//! git_branches, git_bindings) and prepare a commit message or a binding
//! for the user to look at, but it can never call `execute`: no control
//! tool leads here. Only a key press or click in the TUI Git panel, or a
//! `git` frame from the signed in web app's sheet, does.
//!
//! auto_sync and wip_backup from the design are not offered: GodTerm never
//! commits, pushes, pulls or creates a repo on its own.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::git_map::{self, Binding, GitCfg, Resolved, Store};
use crate::git_ops::{self, Ctx, Fail};
use crate::git_scan::{self, PushCheck};
use crate::project_git::{self, Facts};

/// What a section needs from GodTerm.
#[derive(Debug, Clone)]
pub struct Env {
    pub cfg: GitCfg,
    /// The godterm binary (credential helper).
    pub helper: Option<PathBuf>,
    pub gh: PathBuf,
    pub machine: String,
}

impl Env {
    pub fn new(cfg: &GitCfg) -> Env {
        Env {
            cfg: cfg.clone(),
            helper: std::env::current_exe().ok().filter(|_| !cfg!(test)),
            gh: crate::git_cred::gh_bin(Some(cfg)),
            machine: git_ops::machine_name(),
        }
    }
}

// ---- what the assistant prepared (never acted on by itself) ----

static DRAFTS: Mutex<Option<HashMap<PathBuf, String>>> = Mutex::new(None);
static PROPOSALS: Mutex<Option<HashMap<PathBuf, Binding>>> = Mutex::new(None);

/// The assistant drafted a commit message for `top` (shown in the commit
/// box; the user edits and confirms).
pub fn set_draft(top: &Path, msg: &str) {
    let mut g = DRAFTS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .insert(top.to_path_buf(), msg.trim().chars().take(4000).collect());
}

pub fn draft(top: &Path) -> Option<String> {
    DRAFTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .get(top)
        .cloned()
}

fn clear_draft(top: &Path) {
    if let Some(m) = DRAFTS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(top);
    }
}

/// The assistant proposed a binding (shown with a Use button).
pub fn set_proposal(dir: &Path, b: Binding) {
    let mut g = PROPOSALS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .insert(dir.to_path_buf(), b);
}

pub fn proposal(dir: &Path) -> Option<Binding> {
    PROPOSALS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .get(dir)
        .cloned()
}

// ---- warnings ----

/// What is not safe on GitHub yet, in short words: "Not backed up: no
/// GitHub repo", "12 unpushed", "uncommitted changes".
pub fn warnings(facts: Option<&Facts>) -> Vec<String> {
    let Some(f) = facts else {
        return vec!["Not backed up: not in git".into()];
    };
    let mut v = vec![];
    if f.error.is_some() {
        v.push("git error".into());
        return v;
    }
    if f.local_only() {
        v.push("Not backed up: no GitHub repo".into());
    } else if !f.on_github() {
        let h = f
            .remotes
            .iter()
            .find(|r| r.host != "local")
            .map(|r| r.host.clone())
            .unwrap_or_default();
        v.push(format!("No GitHub repo ({h} only)"));
    }
    if f.last.is_none() {
        v.push("no commits yet".into());
    }
    let unp = f.unpushed.unwrap_or(0).max(f.ahead.unwrap_or(0));
    if !f.remotes.is_empty() && unp > 0 {
        let plus = if unp >= project_git::UNPUSHED_CAP {
            "+"
        } else {
            ""
        };
        v.push(format!("{unp}{plus} unpushed"));
    } else if !f.local_only()
        && f.last.is_some()
        && f.branch_on_remote == Some(false)
        && f.branch.is_some()
    {
        v.push("branch not pushed".into());
    }
    if let Some(b) = f.behind.filter(|b| *b > 0) {
        v.push(format!("{b} behind"));
    }
    if f.dirty() {
        v.push("uncommitted changes".into());
    }
    v
}

/// The badge for a folder from the cached facts (never runs git): None
/// when all is on GitHub, or nothing is known yet.
pub fn badge(ix: &crate::project_index::Index, dir: &Path) -> Option<String> {
    if crate::guard::never_folder(dir).is_some() {
        return None;
    }
    // The home folder itself and temp folders are no project.
    let home = crate::config::home_dir();
    let tmp = std::env::temp_dir();
    // (Tests build their fixtures in the temp folder, which on Linux is
    // /tmp itself: the temp rule does not apply to them.)
    let in_tmp =
        dir.starts_with("/private/tmp") || dir.starts_with("/tmp") || dir.starts_with(&tmp);
    if crate::guard::same(dir, &home) || (in_tmp && !cfg!(test)) {
        return None;
    }
    let w = match project_git::repo_top(dir) {
        None => warnings(None),
        Some(top) => warnings(Some(ix.git.get(&top)?)),
    };
    (!w.is_empty()).then(|| w.join(" · "))
}

// ---- the view ----

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActionRow {
    pub id: &'static str,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<String>,
    pub hint: &'static str,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct View {
    pub dir: String,
    pub is_repo: bool,
    pub top: Option<String>,
    pub branch: String,
    pub remote: String,
    pub state: String,
    pub fetched: String,
    pub ahead: u32,
    pub behind: u32,
    pub warnings: Vec<String>,
    pub resolved: Resolved,
    pub commits: Vec<git_ops::LogRow>,
    pub changes: git_ops::Changes,
    pub branches: Vec<git_ops::BranchRow>,
    pub missing_ignores: Vec<String>,
    pub secret_files: Vec<String>,
    pub draft: Option<String>,
    pub proposal: Option<Binding>,
    /// Owners to pick from when defining (rules' choices, [git.owners]).
    pub owners: Vec<String>,
    /// gh accounts known for binding.
    pub accounts: Vec<String>,
    pub actions: Vec<ActionRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

fn ctx_for(top: &Path, r: &Resolved, env: &Env) -> Ctx {
    Ctx {
        account: r.account.clone(),
        helper: env.helper.clone(),
        gh: Some(env.gh.clone()),
        ..Ctx::new(top)
    }
}

/// The folder's repo top and facts (computed now; read only).
fn facts_for(dir: &Path) -> Option<Facts> {
    let (t, g, c) = project_git::locate(dir)?;
    Some(project_git::compute(
        &t,
        &g,
        &c,
        std::time::Duration::from_secs(5),
    ))
}

/// Everything the section shows for `dir` (runs read-only git; call off
/// the UI thread).
pub fn view(dir: &Path, env: &Env, accounts: &[String]) -> View {
    let mut v = View {
        dir: crate::config::tilde(dir),
        ..Default::default()
    };
    if let Some(why) = crate::guard::never_folder(dir) {
        v.refused = Some(why.clone());
        v.resolved.refused = Some(why);
        v.warnings = vec!["Refused: private folder".into()];
        return v;
    }
    let facts = facts_for(dir);
    let top = facts
        .as_ref()
        .map(|f| f.top.clone())
        .unwrap_or_else(|| dir.to_path_buf());
    let store = Store::load();
    let r = git_map::resolve(&top, facts.as_ref(), &env.cfg, &store);
    v.warnings = warnings(facts.as_ref());
    v.is_repo = facts.is_some();
    let ctx = ctx_for(&top, &r, env);
    if let Some(f) = &facts {
        v.top = Some(crate::config::tilde(&f.top));
        v.branch = f.branch_word();
        v.remote = f.remote_word();
        v.state = f.state_word();
        v.fetched = f.fetch_word();
        v.ahead = f.ahead.unwrap_or(0);
        v.behind = f.behind.unwrap_or(0);
        v.commits = git_ops::log(&ctx, 20);
        let mut ch = git_ops::changes(&ctx);
        ch.files.truncate(10);
        v.changes = ch;
        v.branches = git_ops::branches(&ctx);
        v.branches.truncate(30);
        v.missing_ignores = git_scan::missing_ignores(&ctx);
        v.secret_files = git_scan::tracked_secret_files(&ctx);
        v.draft = draft(&f.top);
    }
    v.proposal = proposal(&top);
    let mut owners: Vec<String> = r.choices.clone();
    for o in env.cfg.owners.keys().chain(accounts.iter()) {
        if !owners.iter().any(|x| x.eq_ignore_ascii_case(o)) {
            owners.push(o.clone());
        }
    }
    v.owners = owners;
    let mut accts: Vec<String> = accounts.to_vec();
    for a in env.cfg.owners.values() {
        if !accts.contains(a) {
            accts.push(a.clone());
        }
    }
    v.accounts = accts;
    v.resolved = r;
    v.actions = actions(&v);
    v
}

fn actions(v: &View) -> Vec<ActionRow> {
    let row = |id, label: &str, disabled: Option<&str>, hint| ActionRow {
        id,
        label: label.into(),
        disabled: disabled.map(str::to_string),
        hint,
    };
    let r = &v.resolved;
    let mut a = vec![];
    if !v.is_repo {
        a.push(row(
            "init",
            "Make it a git repo",
            None,
            "git init -b main here",
        ));
    }
    let dirty = v.changes.staged_n + v.changes.unstaged_n + v.changes.untracked.len() > 0;
    let has_remote = v.is_repo && v.remote != "no remote";
    let no_remote = (!has_remote).then_some("no remote yet");
    if v.is_repo {
        a.push(row(
            "commit",
            "Commit...",
            (!dirty).then_some("nothing to commit"),
            "Write or edit the message (the assistant can draft it), then confirm",
        ));
        a.push(row(
            "push",
            "Push",
            no_remote,
            "Checks for secrets and large files first",
        ));
        a.push(row(
            "pull",
            "Pull",
            no_remote,
            "Fast forward only; never merges or rebases",
        ));
        a.push(row(
            "sync",
            "Sync",
            no_remote,
            "Fetch, fast forward if behind, push if ahead",
        ));
        a.push(row("fetch", "Fetch", no_remote, "Remote refs only"));
        a.push(row(
            "branches",
            "Branches...",
            None,
            "Switch or create a branch",
        ));
    }
    if r.refused.is_none() {
        let label = if r.source == "none" {
            "Define repo..."
        } else {
            "Change binding..."
        };
        a.push(row(
            "define",
            label,
            None,
            "Which GitHub owner, repo name and gh account",
        ));
        if !r.on_github {
            let why = if !r.complete() {
                Some("define the owner, name and account first")
            } else {
                None
            };
            a.push(row(
                "create",
                "Create private repo...",
                why,
                "gh repo create --private, checked PRIVATE after, then the first push",
            ));
        }
    }
    if !v.missing_ignores.is_empty() {
        a.push(row(
            "ignores",
            "Add .gitignore rules...",
            None,
            "Secrets and build output stay out of git",
        ));
    }
    if !v.secret_files.is_empty() {
        a.push(row(
            "untrack",
            "Untrack secret files...",
            None,
            "git rm --cached; the files stay on disk",
        ));
    }
    a
}

impl View {
    pub fn json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

// ---- actions ----

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Init,
    Commit {
        message: String,
        all: bool,
    },
    Push,
    Pull,
    Sync,
    Fetch,
    PushAsBranch,
    Switch {
        branch: String,
        stash: bool,
    },
    CreateBranch {
        name: String,
    },
    Bind {
        owner: String,
        repo: String,
        account: String,
    },
    CreateRepo {
        owner: String,
        name: String,
        account: String,
        push: bool,
    },
    Link {
        owner: String,
        name: String,
        account: String,
    },
    AddIgnores,
    Untrack,
}

impl Action {
    pub fn from_json(v: &Value) -> Result<Action, String> {
        let s = |k: &str| v[k].as_str().unwrap_or("").trim().to_string();
        Ok(match s("action").as_str() {
            "init" => Action::Init,
            "commit" => Action::Commit {
                message: v["message"].as_str().unwrap_or("").to_string(),
                all: v["all"].as_bool().unwrap_or(true),
            },
            "push" => Action::Push,
            "pull" => Action::Pull,
            "sync" => Action::Sync,
            "fetch" => Action::Fetch,
            "push_as_branch" => Action::PushAsBranch,
            "switch" => Action::Switch {
                branch: s("branch"),
                stash: v["stash"].as_bool().unwrap_or(false),
            },
            "create_branch" => Action::CreateBranch { name: s("name") },
            "bind" => Action::Bind {
                owner: s("owner"),
                repo: s("repo"),
                account: s("account"),
            },
            "create" => Action::CreateRepo {
                owner: s("owner"),
                name: s("name"),
                account: s("account"),
                push: v["push"].as_bool().unwrap_or(true),
            },
            "link" => Action::Link {
                owner: s("owner"),
                name: s("name"),
                account: s("account"),
            },
            "ignores" => Action::AddIgnores,
            "untrack" => Action::Untrack,
            other => return Err(format!("unknown git action {other:?}")),
        })
    }

    fn pushes(&self) -> bool {
        matches!(
            self,
            Action::Push
                | Action::Sync
                | Action::PushAsBranch
                | Action::CreateRepo { push: true, .. }
        )
    }

    pub fn title(&self) -> String {
        match self {
            Action::Init => "Make it a git repo".into(),
            Action::Commit { all, .. } => {
                if *all {
                    "Commit all changes".into()
                } else {
                    "Commit staged changes".into()
                }
            }
            Action::Push => "Push".into(),
            Action::Pull => "Pull (fast forward)".into(),
            Action::Sync => "Sync".into(),
            Action::Fetch => "Fetch".into(),
            Action::PushAsBranch => "Push as a branch".into(),
            Action::Switch { branch, .. } => format!("Switch to {branch}"),
            Action::CreateBranch { name } => format!("Create branch {name}"),
            Action::Bind { owner, repo, .. } => format!("Bind to {owner}/{repo}"),
            Action::CreateRepo {
                owner, name, push, ..
            } => {
                if *push {
                    format!("Create {owner}/{name} (private) and push")
                } else {
                    format!("Create {owner}/{name} (private)")
                }
            }
            Action::Link { owner, name, .. } => format!("Use existing {owner}/{name}"),
            Action::AddIgnores => "Add .gitignore rules".into(),
            Action::Untrack => "Untrack secret files".into(),
        }
    }
}

/// What a confirmed action will do, with the checks it ran.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Plan {
    #[serde(skip)]
    pub action: Option<Action>,
    #[serde(skip)]
    pub top: PathBuf,
    #[serde(skip)]
    pub account: Option<String>,
    pub title: String,
    /// What will happen, line by line.
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
    /// Secret findings: typing "push anyway" passes them.
    pub soft_blocks: Vec<String>,
    /// Nothing passes these.
    pub hard_blocks: Vec<String>,
    pub needs_typed: bool,
    /// The repo already exists on GitHub (offer Link instead).
    pub exists: bool,
    #[serde(skip)]
    pub check: Option<PushCheck>,
}

impl Plan {
    pub fn json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn can_run(&self) -> bool {
        self.hard_blocks.is_empty()
    }
}

/// Look before doing: names checked, push checks run, the GitHub repo
/// looked up for a create. Changes nothing (call off the UI thread).
pub fn prepare(dir: &Path, action: Action, env: &Env) -> Plan {
    let facts = facts_for(dir);
    let top = facts
        .as_ref()
        .map(|f| f.top.clone())
        .unwrap_or_else(|| dir.to_path_buf());
    let store = Store::load();
    let r = git_map::resolve(&top, facts.as_ref(), &env.cfg, &store);
    let mut p = Plan {
        title: action.title(),
        top: top.clone(),
        account: r.account.clone(),
        action: None,
        lines: vec![],
        warnings: vec![],
        soft_blocks: vec![],
        hard_blocks: vec![],
        needs_typed: false,
        exists: false,
        check: None,
    };
    if let Some(why) = &r.refused {
        p.hard_blocks.push(why.clone());
        p.action = Some(action);
        return p;
    }
    let ctx = ctx_for(&top, &r, env);
    let branch = git_ops::branch(&ctx).unwrap_or_else(|| "?".into());
    let remote = git_ops::push_remote(&ctx).unwrap_or_else(|| "origin".into());
    match &action {
        Action::Init => {
            if facts.is_some() {
                p.hard_blocks.push("it is a git repo already".into());
            }
            p.lines.push(format!(
                "git init -b main in {}",
                crate::config::tilde(&top)
            ));
        }
        Action::Commit { message, all } => {
            if let Err(e) = git_ops::commit_args(message) {
                p.hard_blocks.push(e);
            }
            let ch = git_ops::changes(&ctx);
            let mut paths: Vec<String> = ch
                .files
                .iter()
                .filter(|f| *all || f.staged)
                .map(|f| f.path.clone())
                .collect();
            if *all {
                paths.extend(ch.untracked.iter().cloned());
            }
            paths.sort();
            paths.dedup();
            if paths.is_empty() {
                p.hard_blocks.push("nothing to commit".into());
            }
            let secret: Vec<&String> = paths.iter().filter(|x| git_scan::secret_file(x)).collect();
            if !secret.is_empty() {
                p.hard_blocks.push(format!(
                    "these look like secrets and would be committed: {} (add .gitignore rules or untrack them first)",
                    secret.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ));
            }
            for (f, n) in git_scan::big_worktree_files(&ctx, &paths) {
                let w = format!("{f} is {}", git_scan::mb(n));
                if n > git_scan::BLOCK_BYTES {
                    p.hard_blocks
                        .push(format!("{w}: GitHub refuses files over 100 MB"));
                } else {
                    p.warnings.push(format!("{w} (over 50 MB)"));
                }
            }
            p.lines.push(format!(
                "Commit {} on {branch}:",
                crate::control::plural(paths.len(), "file")
            ));
            for x in paths.iter().take(10) {
                p.lines.push(format!("  {x}"));
            }
            if paths.len() > 10 {
                p.lines.push(format!("  and {} more", paths.len() - 10));
            }
            p.lines.push(format!(
                "Message: {}",
                crate::sessions::snippet(message.lines().next().unwrap_or(""), 80)
            ));
        }
        Action::Push | Action::Sync => {
            let (a, b) = git_ops::counts(&ctx).unwrap_or((0, 0));
            p.lines.push(match &action {
                Action::Push => format!(
                    "git push {remote} {branch} ({} to send)",
                    a.max(if git_ops::upstream(&ctx).is_none() {
                        1
                    } else {
                        0
                    })
                ),
                _ => format!(
                    "fetch {remote}, then fast forward {b} and push {a} (as of the last fetch)"
                ),
            });
            if a > 0 && b > 0 {
                p.warnings.push(format!(
                    "Diverged: {a} local, {b} remote. Push as a branch instead."
                ));
            }
        }
        Action::PushAsBranch => {
            let date = chrono::Local::now().format("%Y%m%d").to_string();
            p.lines.push(format!(
                "Push your {branch} commits to {remote} as {}",
                git_ops::diverged_branch(&env.machine, &branch, &date)
            ));
            p.lines
                .push("Nothing is rewritten; merge it later in a tab.".into());
        }
        Action::Pull => p.lines.push(format!("git pull --ff-only on {branch}")),
        Action::Fetch => p.lines.push(format!("git fetch {remote}")),
        Action::Switch { branch: to, stash } => {
            if let Err(e) = git_ops::switch_args(to) {
                p.hard_blocks.push(e);
            }
            if git_ops::tracked_dirty(&ctx) {
                if *stash {
                    p.lines
                        .push("Your uncommitted changes go to the stash first.".into());
                } else {
                    p.hard_blocks
                        .push("uncommitted changes: commit them, or switch with stash".into());
                }
            }
            p.lines.push(format!("git switch {to}"));
        }
        Action::CreateBranch { name } => {
            if let Err(e) = git_ops::create_branch_args(name) {
                p.hard_blocks.push(e);
            }
            p.lines
                .push(format!("git switch -c {name} (from {branch})"));
        }
        Action::Bind {
            owner,
            repo,
            account,
        } => {
            check_names(&mut p, owner, repo, account);
            p.lines.push(format!(
                "{} belongs to {owner}/{repo}, pushed as gh account {account}",
                crate::config::tilde(&top)
            ));
            p.lines
                .push("Saved in ~/.godterm/git-bindings.json; nothing in the repo changes.".into());
        }
        Action::CreateRepo {
            owner,
            name,
            account,
            push,
        } => {
            check_names(&mut p, owner, name, account);
            if facts.is_none() {
                p.hard_blocks.push("make it a git repo first".into());
            }
            if r.on_github {
                p.hard_blocks.push("it already has a GitHub remote".into());
            }
            if p.hard_blocks.is_empty() {
                match crate::git_cred::visibility(&env.gh, account, owner, name) {
                    Ok(Some(vis)) => {
                        p.exists = true;
                        p.hard_blocks.push(format!(
                            "{owner}/{name} exists already ({}): use it instead",
                            vis.to_lowercase()
                        ));
                    }
                    Ok(None) => {}
                    Err(e) => p.hard_blocks.push(format!("could not ask GitHub: {e}")),
                }
            }
            let rname = new_remote_name(&ctx);
            p.lines.push(format!(
                "gh repo create {owner}/{name} --private --source {} --remote {rname} (as {account})",
                crate::config::tilde(&top)
            ));
            p.lines.push("Then check GitHub says PRIVATE.".into());
            if *push {
                p.lines.push(format!(
                    "Then push {branch} (checked for secrets and large files)."
                ));
            }
        }
        Action::Link {
            owner,
            name,
            account,
        } => {
            check_names(&mut p, owner, name, account);
            if r.on_github {
                p.hard_blocks.push("it already has a GitHub remote".into());
            }
            let rname = new_remote_name(&ctx);
            p.lines.push(format!(
                "git remote add {rname} https://github.com/{owner}/{name}.git"
            ));
        }
        Action::AddIgnores => {
            let m = git_scan::missing_ignores(&ctx);
            if m.is_empty() {
                p.hard_blocks.push("nothing missing".into());
            }
            p.lines
                .push(format!("Append to .gitignore: {}", m.join(" ")));
        }
        Action::Untrack => {
            let s = git_scan::tracked_secret_files(&ctx);
            if s.is_empty() {
                p.hard_blocks.push("no secret files are tracked".into());
            }
            p.lines.push(format!(
                "git rm --cached {} (they stay on disk)",
                s.join(" ")
            ));
            p.lines
                .push("Then Add .gitignore rules so they stay out.".into());
        }
    }
    if action.pushes() && facts.is_some() {
        let c = git_scan::check_push(&ctx);
        p.hard_blocks.extend(c.hard_blocks());
        p.soft_blocks = c.soft_blocks();
        p.warnings.extend(c.warnings());
        p.lines.push(format!(
            "Checked: secret scan ({}), {} outgoing in {}",
            c.secrets.tool,
            git_scan::mb(c.sizes.total),
            crate::control::plural(c.sizes.blobs, "file")
        ));
        p.needs_typed = !p.soft_blocks.is_empty();
        p.check = Some(c);
    }
    p.action = Some(action);
    p
}

fn check_names(p: &mut Plan, owner: &str, repo: &str, account: &str) {
    if !git_map::valid_owner(owner) {
        p.hard_blocks
            .push(format!("\"{owner}\" is not a GitHub owner name"));
    }
    if !git_map::valid_repo(repo) {
        p.hard_blocks
            .push(format!("\"{repo}\" is not a GitHub repo name"));
    }
    if !git_map::valid_owner(account) {
        p.hard_blocks
            .push(format!("\"{account}\" is not a gh account"));
    }
}

/// origin, unless the repo has one already (then github).
fn new_remote_name(ctx: &Ctx) -> String {
    if git_ops::remotes(ctx).iter().any(|r| r == "origin") {
        "github".into()
    } else {
        "origin".into()
    }
}

fn fail(e: Fail) -> String {
    e.to_string()
}

/// Do a prepared action: only from the user's confirmation in the Git
/// section. `typed` is what they typed when the plan needs "push anyway".
pub fn execute(plan: &Plan, typed: Option<&str>, env: &Env) -> Result<String, String> {
    let Some(action) = plan.action.clone() else {
        return Err("nothing to do".into());
    };
    if !plan.hard_blocks.is_empty() {
        return Err(format!("not done: {}", plan.hard_blocks.join("; ")));
    }
    if plan.needs_typed
        && typed.map(|t| t.trim().to_ascii_lowercase()).as_deref() != Some(git_scan::OVERRIDE)
    {
        return Err(format!(
            "not pushed: type \"{}\" to push past the findings",
            git_scan::OVERRIDE
        ));
    }
    let top = plan.top.clone();
    if let Some(why) = crate::guard::never_folder(&top) {
        return Err(why);
    }
    let ctx = Ctx {
        account: plan.account.clone(),
        helper: env.helper.clone(),
        gh: Some(env.gh.clone()),
        ..Ctx::new(&top)
    };
    // What was checked is what goes: newer commits are checked again.
    if let Some(c) = &plan.check {
        if c.head != git_ops::head(&ctx) {
            return Err("the repo changed since the check; press it again".into());
        }
    }
    let said = match action {
        Action::Init => git_ops::init(&top).map_err(fail)?,
        Action::Commit { message, all } => {
            let s = git_ops::commit(&ctx, &message, all).map_err(fail)?;
            clear_draft(&top);
            s
        }
        Action::Push => git_ops::push(&ctx).map_err(fail)?,
        Action::Pull => git_ops::pull(&ctx).map_err(fail)?,
        Action::Sync => git_ops::sync(&ctx).map_err(fail)?,
        Action::Fetch => git_ops::fetch(&ctx).map_err(fail)?,
        Action::PushAsBranch => {
            let date = chrono::Local::now().format("%Y%m%d").to_string();
            git_ops::push_as_branch(&ctx, &env.machine, &date).map_err(fail)?
        }
        Action::Switch { branch, stash } => git_ops::switch(&ctx, &branch, stash).map_err(fail)?,
        Action::CreateBranch { name } => git_ops::create_branch(&ctx, &name).map_err(fail)?,
        Action::Bind {
            owner,
            repo,
            account,
        } => {
            bind(&top, &owner, &repo, &account, "manual")?;
            format!("Bound to {owner}/{repo} (gh account {account}).")
        }
        Action::CreateRepo {
            owner,
            name,
            account,
            push,
        } => {
            let rname = new_remote_name(&ctx);
            let made =
                crate::git_cred::create_private(&env.gh, &account, &owner, &name, &top, &rname)?;
            bind(&top, &owner, &name, &account, "manual")?;
            let ctx = Ctx {
                account: Some(account.clone()),
                ..ctx
            };
            if push && git_ops::head(&ctx).is_some() {
                match git_ops::push(&ctx) {
                    Ok(s) => format!("{made} {s}"),
                    Err(e) => format!("{made} The first push did not go: {e}"),
                }
            } else {
                made
            }
        }
        Action::Link {
            owner,
            name,
            account,
        } => {
            let rname = new_remote_name(&ctx);
            git_ops::run(&ctx, &git_ops::remote_add_args(&rname, &owner, &name)?).map_err(fail)?;
            bind(&top, &owner, &name, &account, "manual")?;
            format!("Linked {owner}/{name} as remote {rname}.")
        }
        Action::AddIgnores => {
            let m = git_scan::missing_ignores(&ctx);
            git_scan::add_ignores(&top, &m)?
        }
        Action::Untrack => {
            let s = git_scan::tracked_secret_files(&ctx);
            git_ops::untrack(&ctx, &s).map_err(fail)?
        }
    };
    crate::log::info(&format!(
        "git section: {} in {}: {said}",
        plan.title,
        crate::config::tilde(&top)
    ));
    Ok(said)
}

fn bind(top: &Path, owner: &str, repo: &str, account: &str, source: &str) -> Result<(), String> {
    let mut s = Store::load();
    s.bind(
        top,
        Binding {
            owner: owner.into(),
            repo: repo.into(),
            account: account.into(),
            source: source.into(),
            defined_at: String::new(),
        },
    )?;
    s.save()
}

// ---- the web sheet's frames ----

/// Pending plans the web sheet may run, by id (made by `prepare`, used once).
type Kept = HashMap<String, (std::time::Instant, PathBuf, Plan)>;
static PLANS: Mutex<Option<Kept>> = Mutex::new(None);

pub fn keep_plan(dir: &Path, p: Plan) -> String {
    let id = format!("g{:016x}", rand_u64());
    let mut g = PLANS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    m.retain(|_, (t, _, _)| t.elapsed() < std::time::Duration::from_secs(600));
    m.insert(
        id.clone(),
        (std::time::Instant::now(), dir.to_path_buf(), p),
    );
    id
}

pub fn take_plan(id: &str) -> Option<(PathBuf, Plan)> {
    let mut g = PLANS.lock().unwrap_or_else(|e| e.into_inner());
    let (t, d, p) = g.as_mut()?.remove(id)?;
    (t.elapsed() < std::time::Duration::from_secs(600)).then_some((d, p))
}

fn rand_u64() -> u64 {
    let mut b = [0u8; 8];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    u64::from_le_bytes(b)
}

/// One `git` frame from the web sheet, answered off the UI thread:
/// {op: view|prepare|run, dir, action..., plan, typed}.
pub fn web_frame(v: &Value, dir: Option<PathBuf>, env: &Env, accounts: &[String]) -> Value {
    let op = v["op"].as_str().unwrap_or("view");
    match op {
        "view" => match dir {
            Some(d) => json!({"ok": true, "view": view(&d, env, accounts).json()}),
            None => json!({"ok": false, "error": "no such tab"}),
        },
        "prepare" => {
            let Some(d) = dir else {
                return json!({"ok": false, "error": "no such tab"});
            };
            match Action::from_json(v) {
                Ok(a) => {
                    let p = prepare(&d, a, env);
                    let j = p.json();
                    let id = keep_plan(&d, p);
                    json!({"ok": true, "plan": j, "plan_id": id, "override": git_scan::OVERRIDE})
                }
                Err(e) => json!({"ok": false, "error": e}),
            }
        }
        "run" => {
            let Some((d, p)) = v["plan_id"].as_str().and_then(take_plan) else {
                return json!({"ok": false, "error": "that confirmation expired; press the button again"});
            };
            match execute(&p, v["typed"].as_str(), env) {
                Ok(said) => {
                    json!({"ok": true, "said": said, "view": view(&d, env, accounts).json()})
                }
                Err(e) => json!({"ok": false, "error": e, "view": view(&d, env, accounts).json()}),
            }
        }
        other => json!({"ok": false, "error": format!("unknown git op {other:?}")}),
    }
}

#[cfg(test)]
#[path = "git_panel_tests.rs"]
mod tests;
