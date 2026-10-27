//! Which GitHub owner and which gh account a project folder belongs to.
//!
//! `[git]` in config.toml maps folders to owners with glob rules (the
//! most specific pattern wins, ties go to the first written) and owners to
//! gh accounts (`[git.owners]`). A folder no rule covers is "undefined":
//! GodTerm asks and never guesses. What the user decided is kept in
//! `~/.godterm/git-bindings.json` with portable `~/...` keys.
//!
//! Resolution of a folder:
//! 1. In the private vault or a private folder (logins, keys): refused.
//! 2. An origin remote on GitHub: owner and repo from its URL; the account
//!    from a saved binding, else `[git.owners]`; an unknown owner asks.
//! 3. A saved binding (defined in the Git section).
//! 4. The first matching rule proposes an owner (or a choice of owners).
//! 5. Nothing: undefined ("No repo defined").
//!
//! Nothing here changes a repo; saving a binding happens only from the
//! user's own button press in the Git section.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `[git]`: project repos (docs/SYNC.md).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GitCfg {
    /// What happens to a folder no rule covers: "ask" (the only value).
    pub undefined: String,
    /// How a new repo is named: "folder" (the folder's name, lowercased,
    /// spaces to '-'). The name is always shown and editable first.
    pub repo_name: String,
    /// GitHub owner (user or org) to the gh account that pushes for it.
    pub owners: BTreeMap<String, String>,
    /// Folder rules: the most specific match wins.
    pub rules: Vec<GitRule>,
    /// Fetch the open tabs' repos every this many minutes (read only:
    /// remote refs only, never a merge). 0, the default: never on its own.
    pub fetch_every_min: u64,
    /// The gh binary (default: `gh` on PATH).
    pub gh_bin: Option<String>,
}

impl Default for GitCfg {
    fn default() -> Self {
        GitCfg {
            undefined: "ask".into(),
            repo_name: "folder".into(),
            owners: BTreeMap::new(),
            rules: vec![],
            fetch_every_min: 0,
            gh_bin: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct GitRule {
    /// A glob over folders: "~/work/**".
    #[serde(rename = "match")]
    pub pattern: String,
    /// An owner, or "choose" (pick one of `choices` per project).
    pub owner: String,
    pub choices: Vec<String>,
    /// "exclude": kept off the personal state repo (Phase 2).
    pub state: Option<String>,
}

/// GitHub user, org and gh account names (EMU accounts have a '_').
pub fn valid_owner(s: &str) -> bool {
    let n = s.chars().count();
    (1..=39).contains(&n)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !s.starts_with('-')
        && !s.starts_with('_')
}

/// A GitHub repo name.
pub fn valid_repo(s: &str) -> bool {
    let n = s.chars().count();
    (1..=100).contains(&n)
        && s != "."
        && s != ".."
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The proposed repo name for a folder: its name, lowercased, spaces to
/// '-', anything GitHub refuses dropped.
pub fn repo_name_for(dir: &Path) -> String {
    let n = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut s: String = n
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    while s.starts_with(['-', '.']) {
        s.remove(0);
    }
    if s.is_empty() {
        "project".into()
    } else {
        s.chars().take(100).collect()
    }
}

/// How specific a pattern is: literal characters before the first
/// wildcard (more is more specific).
fn specificity(pat: &str) -> usize {
    pat.chars()
        .take_while(|c| !matches!(c, '*' | '?' | '['))
        .count()
}

fn expand(pat: &str) -> String {
    crate::config::expand_tilde(pat.trim())
        .to_string_lossy()
        .into_owned()
}

/// Does `pat` cover folder `p`? "x/**" also covers x itself.
pub fn glob_matches(pat: &str, p: &Path) -> bool {
    let full = expand(pat);
    let opts = glob::MatchOptions {
        case_sensitive: !cfg!(any(target_os = "macos", windows)),
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    if let Some(base) = full.strip_suffix("/**") {
        if crate::guard::same(Path::new(base), p) {
            return true;
        }
    }
    glob::Pattern::new(&full)
        .map(|g| g.matches_path_with(p, opts))
        .unwrap_or(false)
}

/// The rule for `p`: the most specific match, ties to the first.
pub fn rule_for<'a>(cfg: &'a GitCfg, p: &Path) -> Option<&'a GitRule> {
    let mut best: Option<(usize, &GitRule)> = None;
    for r in &cfg.rules {
        if r.pattern.trim().is_empty() || !glob_matches(&r.pattern, p) {
            continue;
        }
        let s = specificity(&expand(&r.pattern));
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, r));
        }
    }
    best.map(|(_, r)| r)
}

/// The gh account for a GitHub owner, if `[git.owners]` names one.
pub fn account_for(cfg: &GitCfg, owner: &str) -> Option<String> {
    cfg.owners
        .iter()
        .find(|(o, _)| o.eq_ignore_ascii_case(owner))
        .map(|(_, a)| a.clone())
        .filter(|a| valid_owner(a))
}

// ---- the bindings store ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Binding {
    pub owner: String,
    pub repo: String,
    pub account: String,
    /// remote, rule or manual: how it was decided.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub defined_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Store {
    pub version: u32,
    pub projects: BTreeMap<String, Binding>,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            version: 1,
            projects: BTreeMap::new(),
        }
    }
}

pub fn store_path() -> PathBuf {
    crate::config::app_home().join("git-bindings.json")
}

/// The key for a folder: "~/x/y" under home, else the absolute path.
pub fn key(p: &Path) -> String {
    let home = crate::config::home_dir();
    match p.strip_prefix(&home) {
        Ok(r) if r.as_os_str().is_empty() => "~".into(),
        Ok(r) => format!("~/{}", r.to_string_lossy().replace('\\', "/")),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

impl Store {
    pub fn load() -> Store {
        std::fs::read_to_string(store_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::config::write_private(&store_path(), &text).map_err(|e| format!("{e:#}"))
    }

    pub fn get(&self, p: &Path) -> Option<&Binding> {
        self.projects.get(&key(p))
    }

    /// Save one binding (checked first). Only the Git section calls it,
    /// on the user's own confirmed press.
    pub fn bind(&mut self, p: &Path, mut b: Binding) -> Result<(), String> {
        if let Some(why) = crate::guard::never_folder(p) {
            return Err(why);
        }
        if !valid_owner(&b.owner) {
            return Err(format!("\"{}\" is not a GitHub owner name", b.owner));
        }
        if !valid_repo(&b.repo) {
            return Err(format!("\"{}\" is not a GitHub repo name", b.repo));
        }
        if !valid_owner(&b.account) {
            return Err(format!("\"{}\" is not a gh account name", b.account));
        }
        if b.defined_at.is_empty() {
            b.defined_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        }
        self.projects.insert(key(p), b);
        Ok(())
    }
}

// ---- resolving a folder ----

/// What GodTerm knows about where a folder's repo lives on GitHub.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Resolved {
    /// The repo's top (or the folder, when it is in no repo).
    pub dir: PathBuf,
    /// Refused: why (vault, private folder).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// owner = "choose": the owners to pick from.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// remote, saved, rule or none.
    pub source: String,
    /// The rule's pattern, when a rule decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// The repo exists on GitHub (from an origin remote there).
    pub on_github: bool,
    /// What the user still has to decide, in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask: Option<String>,
    /// Kept off the personal state repo (rule state = "exclude").
    pub state_exclude: bool,
}

impl Resolved {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn undefined(&self) -> bool {
        self.refused.is_none() && self.source == "none"
    }

    /// owner/repo and the account are all known.
    pub fn complete(&self) -> bool {
        self.refused.is_none()
            && self.owner.is_some()
            && self.repo.is_some()
            && self.account.is_some()
    }

    pub fn slug(&self) -> Option<String> {
        Some(format!("{}/{}", self.owner.as_ref()?, self.repo.as_ref()?))
    }
}

/// The GitHub owner/repo of an origin (or first GitHub) remote.
pub fn github_remote(f: &crate::project_git::Facts) -> Option<(String, String, String)> {
    let r = f
        .remotes
        .iter()
        .filter(|r| r.host == "github")
        .min_by_key(|r| r.name != "origin")?;
    let path = r.repo.as_deref()?;
    let (o, n) = path.split_once('/')?;
    let n = n.trim_end_matches(".git");
    (valid_owner(o) && valid_repo(n)).then(|| (o.to_string(), n.to_string(), r.name.clone()))
}

/// Resolve folder `dir` (a repo's top, or a plain folder).
pub fn resolve(
    dir: &Path,
    facts: Option<&crate::project_git::Facts>,
    cfg: &GitCfg,
    store: &Store,
) -> Resolved {
    let mut r = Resolved {
        dir: dir.to_path_buf(),
        source: "none".into(),
        ..Default::default()
    };
    if let Some(why) = crate::guard::never_folder(dir) {
        r.refused = Some(why);
        return r;
    }
    let rule = rule_for(cfg, dir);
    r.state_exclude = rule.and_then(|x| x.state.as_deref()) == Some("exclude");
    let saved = store.get(dir);
    if let Some((owner, repo, _)) = facts.and_then(github_remote) {
        r.source = "remote".into();
        r.on_github = true;
        r.account = saved
            .filter(|b| b.owner.eq_ignore_ascii_case(&owner))
            .map(|b| b.account.clone())
            .or_else(|| account_for(cfg, &owner));
        if r.account.is_none() {
            r.ask = Some(format!(
                "Which gh account pushes for {owner}? It is not in [git.owners]."
            ));
        }
        r.owner = Some(owner);
        r.repo = Some(repo);
        return r;
    }
    if let Some(b) = saved {
        r.source = "saved".into();
        r.owner = Some(b.owner.clone());
        r.repo = Some(b.repo.clone());
        r.account = Some(b.account.clone());
        return r;
    }
    let Some(rule) = rule else {
        r.ask = Some("No repo defined. Define which GitHub owner it belongs to.".into());
        return r;
    };
    r.source = "rule".into();
    r.rule = Some(rule.pattern.clone());
    r.repo = Some(repo_name_for(dir));
    if rule.owner.eq_ignore_ascii_case("choose") {
        r.choices = rule
            .choices
            .iter()
            .filter(|c| valid_owner(c))
            .cloned()
            .collect();
        r.ask = Some(format!(
            "Choose the owner for this project: {}.",
            r.choices.join(" or ")
        ));
        return r;
    }
    if !valid_owner(&rule.owner) {
        r.ask = Some(format!(
            "The rule {} names \"{}\", which is not a GitHub owner.",
            rule.pattern, rule.owner
        ));
        return r;
    }
    r.account = account_for(cfg, &rule.owner);
    if r.account.is_none() {
        r.ask = Some(format!(
            "Which gh account pushes for {}? It is not in [git.owners].",
            rule.owner
        ));
    }
    r.owner = Some(rule.owner.clone());
    r
}

#[cfg(test)]
#[path = "git_map_tests.rs"]
mod tests;
