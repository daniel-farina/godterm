//! Settings > GitHub accounts and repos: the `[git]` owners and folder
//! rules and the saved per project bindings, as the user edits them in
//! the app (web Settings and the terminal Settings view).
//!
//! The editor works on a [`Draft`]. Nothing is written until the user's
//! own Save, after a confirmation that shows [`diff`]. Then
//! [`write_toml`] puts the owners and rules into config.toml with
//! toml_edit (comments and every other key kept) and the binding edits go
//! through `git_map::Store`. The assistant can read all of this (the
//! git_bindings tool) and suggest changes in words; no tool saves them.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

use crate::git_map::{self, Binding, GitCfg, GitRule, Store};

/// Owners in the shipped starter set that stand for something the user
/// still has to fill in. Shown "placeholder" until renamed. A
/// `github-suggestions.toml` can name more (its `placeholders` list).
pub const PLACEHOLDER_OWNERS: &[&str] = &["your-github-user", "your-org"];

/// The comment written after a placeholder owner in config.toml.
const PLACEHOLDER_NOTE: &str = "# placeholder: set the real name";

pub fn is_placeholder(owner: &str) -> bool {
    let o = owner.trim();
    PLACEHOLDER_OWNERS.iter().any(|p| p.eq_ignore_ascii_case(o))
        || user_suggestions().is_some_and(|s| {
            s.placeholders
                .iter()
                .any(|p| p.trim().eq_ignore_ascii_case(o))
        })
}

/// Every owner name shown as a placeholder: the shipped ones and the
/// ones a `github-suggestions.toml` adds.
pub fn placeholder_owners() -> Vec<String> {
    let mut v: Vec<String> = PLACEHOLDER_OWNERS.iter().map(|s| s.to_string()).collect();
    if let Some(s) = user_suggestions() {
        for p in s.placeholders {
            let p = p.trim().to_string();
            if !p.is_empty() && !v.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
                v.push(p);
            }
        }
    }
    v
}

/// One row of the owners table: GitHub owner to the gh account that
/// pushes for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct OwnerRow {
    pub owner: String,
    pub account: String,
}

/// A change to one saved binding: `binding` None unbinds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BindEdit {
    /// The folder, "~/x" or absolute.
    pub key: String,
    #[serde(default)]
    pub binding: Option<Binding>,
}

/// What the editor holds: the owners, the ordered rules, and binding edits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Draft {
    pub owners: Vec<OwnerRow>,
    pub rules: Vec<GitRule>,
    pub binds: Vec<BindEdit>,
}

impl Draft {
    pub fn from_cfg(cfg: &GitCfg) -> Draft {
        Draft {
            owners: cfg
                .owners
                .iter()
                .map(|(o, a)| OwnerRow {
                    owner: o.clone(),
                    account: a.clone(),
                })
                .collect(),
            rules: cfg.rules.clone(),
            binds: vec![],
        }
    }

    /// The draft's owners and rules on top of `base` (its other keys).
    pub fn to_cfg(&self, base: &GitCfg) -> GitCfg {
        let mut c = base.clone();
        c.owners = self
            .owners
            .iter()
            .map(|r| (r.owner.trim().to_string(), r.account.trim().to_string()))
            .collect();
        c.rules = self.rules.iter().map(clean_rule).collect();
        c
    }

    /// No owners and no rules: the section offers the starter set.
    pub fn is_empty(&self) -> bool {
        self.owners.is_empty() && self.rules.is_empty()
    }
}

fn clean_rule(r: &GitRule) -> GitRule {
    let choose = r.owner.trim().eq_ignore_ascii_case("choose");
    GitRule {
        pattern: r.pattern.trim().to_string(),
        owner: if choose {
            "choose".into()
        } else {
            r.owner.trim().to_string()
        },
        choices: if choose {
            r.choices
                .iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect()
        } else {
            vec![]
        },
        state: r
            .state
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

fn own(owner: &str, account: &str) -> OwnerRow {
    OwnerRow {
        owner: owner.into(),
        account: account.into(),
    }
}

fn rule(pattern: &str, owner: &str) -> GitRule {
    GitRule {
        pattern: pattern.into(),
        owner: owner.into(),
        choices: vec![],
        state: None,
    }
}

/// The shipped starter set: generic examples, every owner a placeholder.
pub fn placeholders() -> Draft {
    Draft {
        owners: vec![
            own("your-github-user", "your-github-user"),
            own("your-org", "your-github-user"),
        ],
        rules: vec![
            rule("~/work/**", "your-org"),
            rule("~/**", "your-github-user"),
        ],
        binds: vec![],
    }
}

/// A fuller starter set for the tests: a user, a lab org, a second
/// account, an org (`org`) to choose for, and a kept out folder.
#[cfg(test)]
pub fn sample(org: &str) -> Draft {
    let mut choose = rule("~/acme/**", "choose");
    choose.choices = vec![org.into(), "alice".into()];
    choose.state = Some("exclude".into());
    Draft {
        owners: vec![
            own("alice", "alice"),
            own("alice-labs", "alice"),
            own("bob", "bob"),
            own(org, "alice_acme"),
        ],
        rules: vec![
            rule("~/labs/**", "alice-labs"),
            choose,
            rule("~/**", "alice"),
        ],
        binds: vec![],
    }
}

/// A starter set built from the gh accounts this machine has: each
/// account owns its own repos, an example org (a placeholder) is pushed
/// by the active one, and everything else under ~ goes to the active one.
/// None when gh lists no account.
pub fn from_accounts(accounts: &[Acct]) -> Option<Draft> {
    let main = accounts
        .iter()
        .find(|a| a.active)
        .or_else(|| accounts.first())?;
    let mut owners: Vec<OwnerRow> = accounts.iter().map(|a| own(&a.name, &a.name)).collect();
    owners.push(own("your-org", &main.name));
    Some(Draft {
        owners,
        rules: vec![rule("~/work/**", "your-org"), rule("~/**", &main.name)],
        binds: vec![],
    })
}

/// The optional per user file that replaces the suggested starter set.
pub fn suggestions_path() -> PathBuf {
    crate::config::app_home().join("github-suggestions.toml")
}

/// `github-suggestions.toml`: the draft's `owners` and `rules`, plus the
/// owner names to mark as placeholders.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct UserSuggestions {
    pub owners: Vec<OwnerRow>,
    pub rules: Vec<GitRule>,
    pub placeholders: Vec<String>,
}

/// The suggestions file parsed, or None when it is missing or unreadable.
pub fn parse_suggestions(text: &str) -> Option<UserSuggestions> {
    toml::from_str::<UserSuggestions>(text).ok()
}

/// The user's suggestions file, read again only when it changes.
pub fn user_suggestions() -> Option<UserSuggestions> {
    use std::sync::Mutex;
    use std::time::SystemTime;
    type Cached = (PathBuf, Option<SystemTime>, Option<UserSuggestions>);
    static CACHE: Mutex<Option<Cached>> = Mutex::new(None);
    let path = suggestions_path();
    let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, t, v)) = c.as_ref() {
        if *p == path && *t == stamp {
            return v.clone();
        }
    }
    let v = stamp
        .and_then(|_| std::fs::read_to_string(&path).ok())
        .and_then(|t| parse_suggestions(&t));
    *c = Some((path, stamp, v.clone()));
    v
}

/// The suggested starter set, shown (marked as placeholders) while `[git]`
/// has no owners and no rules. "Use these" saves it. The user's
/// `github-suggestions.toml` wins; then one built from the gh accounts;
/// then the shipped examples.
pub fn suggested(accounts: Option<&[Acct]>) -> Draft {
    if let Some(s) = user_suggestions().filter(|s| !s.owners.is_empty() || !s.rules.is_empty()) {
        return Draft {
            owners: s.owners,
            rules: s.rules,
            binds: vec![],
        };
    }
    accounts
        .and_then(from_accounts)
        .unwrap_or_else(placeholders)
}

// ---- gh accounts ----

/// A gh account `gh auth status` lists, and whether it is logged in.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Acct {
    pub name: String,
    pub logged_in: bool,
    pub active: bool,
}

/// The accounts in `gh auth status` output. Only names and states are
/// kept: nothing else of the text (gh's masked tokens among it) leaves.
pub fn parse_accounts(text: &str) -> Vec<Acct> {
    let mut v: Vec<Acct> = vec![];
    for l in text.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("- Active account:") {
            if let Some(last) = v.last_mut() {
                last.active = rest.trim() == "true";
            }
            continue;
        }
        let Some(i) = t.find(" account ") else {
            continue;
        };
        let name = t[i + 9..]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
        if !git_map::valid_owner(name) || v.iter().any(|a| a.name == name) {
            continue;
        }
        v.push(Acct {
            name: name.to_string(),
            logged_in: t.contains("Logged in to"),
            active: false,
        });
    }
    v
}

/// The gh accounts on github.com (runs `gh auth status`, read only).
pub fn accounts(gh: &Path) -> Vec<Acct> {
    crate::git_cred::status_text(gh)
        .map(|t| parse_accounts(&t))
        .unwrap_or_default()
}

// ---- checking ----

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Check {
    /// These stop the save.
    pub errors: Vec<String>,
    /// These are shown; the save still goes ahead.
    pub warnings: Vec<String>,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// The literal folder a glob starts from: everything before the first
/// wildcard, cut back to a whole folder.
fn glob_base(full: &str) -> String {
    let lit: String = full
        .chars()
        .take_while(|c| !matches!(c, '*' | '?' | '['))
        .collect();
    if lit.len() == full.len() {
        return lit.trim_end_matches('/').to_string();
    }
    match lit.rfind('/') {
        Some(0) => "/".into(),
        Some(i) => lit[..i].to_string(),
        None => String::new(),
    }
}

/// Why a folder rule's glob is refused, if it is.
pub fn glob_problem(pat: &str) -> Option<String> {
    let p = pat.trim();
    if p.is_empty() {
        return Some("a folder rule needs a glob, like ~/work/**".into());
    }
    let drive = cfg!(windows) && (crate::fspath::is_drive_path(p) || p.starts_with(r"\\"));
    if !(p.starts_with("~/") || p == "~" || p.starts_with('/') || drive) {
        return Some(format!(
            "\"{p}\" is not a folder glob: start it with ~/ or /{}",
            if cfg!(windows) { " or a drive like C:\\" } else { "" }
        ));
    }
    let full = crate::config::expand_tilde(p)
        .to_string_lossy()
        .into_owned();
    if let Err(e) = glob::Pattern::new(&full) {
        return Some(format!("\"{p}\" is not a valid glob: {}", e.msg));
    }
    let base = glob_base(&full);
    let vault = Path::new(crate::guard::VAULT);
    // In the vault, or a folder above it the wildcards could reach into.
    let above = !base.is_empty() && vault.starts_with(&base);
    if crate::guard::in_vault(Path::new(&base)) || above || git_map::glob_matches(p, vault) {
        return Some(format!(
            "\"{p}\" reaches the private vault ({}): GodTerm never maps it",
            crate::guard::VAULT
        ));
    }
    if !base.is_empty() {
        if let Some(why) = crate::guard::never_folder(Path::new(&base)) {
            return Some(format!("\"{p}\" is refused: {why}"));
        }
    }
    None
}

/// Check a draft. `accounts` is what gh lists (None: not known yet, so
/// nothing is said about logins).
pub fn check(d: &Draft, accounts: Option<&[Acct]>) -> Check {
    let mut c = Check::default();
    let mut seen: Vec<String> = vec![];
    for r in &d.owners {
        let (o, a) = (r.owner.trim(), r.account.trim());
        if !git_map::valid_owner(o) {
            c.errors.push(format!(
                "\"{o}\" is not a GitHub owner name (letters, digits, - and _, up to 39)"
            ));
        }
        if !git_map::valid_owner(a) {
            c.errors
                .push(format!("\"{a}\" is not a gh account name (owner {o})"));
        }
        if seen.iter().any(|s| s.eq_ignore_ascii_case(o)) {
            c.errors.push(format!("{o} is in the owners table twice"));
        }
        seen.push(o.to_string());
        if is_placeholder(o) {
            c.warnings
                .push(format!("{o} is a placeholder: set the real name"));
        }
        if let Some(list) = accounts {
            if git_map::valid_owner(a) && !list.iter().any(|x| x.name == a && x.logged_in) {
                c.warnings.push(format!(
                    "{a} (for {o}) is not logged in to gh: run gh auth login for it before pushing"
                ));
            }
        }
    }
    let has_owner = |o: &str| {
        d.owners
            .iter()
            .any(|r| r.owner.trim().eq_ignore_ascii_case(o))
    };
    let mut pats: Vec<String> = vec![];
    for (i, r) in d.rules.iter().enumerate() {
        let r = clean_rule(r);
        let n = i + 1;
        if let Some(why) = glob_problem(&r.pattern) {
            c.errors.push(format!("Rule {n}: {why}"));
        }
        if pats.contains(&r.pattern) {
            c.warnings.push(format!(
                "Rule {n}: {} is already a rule above; the first one wins",
                r.pattern
            ));
        }
        pats.push(r.pattern.clone());
        let owners: Vec<String> = if r.owner == "choose" {
            if r.choices.is_empty() {
                c.errors.push(format!(
                    "Rule {n}: \"choose per project\" needs at least one owner to choose from"
                ));
            }
            r.choices.clone()
        } else {
            vec![r.owner.clone()]
        };
        for o in owners {
            if !git_map::valid_owner(&o) {
                c.errors
                    .push(format!("Rule {n}: \"{o}\" is not a GitHub owner name"));
            } else if !has_owner(&o) {
                c.warnings.push(format!(
                    "Rule {n}: {o} has no gh account in the owners table (it will ask)"
                ));
            }
        }
        if let Some(s) = &r.state {
            if s != "exclude" {
                c.errors.push(format!(
                    "Rule {n}: state can only be \"exclude\", not \"{s}\""
                ));
            }
        }
    }
    for b in &d.binds {
        let p = crate::config::expand_tilde(&b.key);
        if let Some(why) = crate::guard::never_folder(&p) {
            c.errors.push(format!("{}: refused, {why}", b.key));
            continue;
        }
        let Some(x) = &b.binding else { continue };
        if !git_map::valid_owner(&x.owner) {
            c.errors.push(format!(
                "{}: \"{}\" is not a GitHub owner name",
                b.key, x.owner
            ));
        }
        if !git_map::valid_repo(&x.repo) {
            c.errors.push(format!(
                "{}: \"{}\" is not a GitHub repo name",
                b.key, x.repo
            ));
        }
        if !git_map::valid_owner(&x.account) {
            c.errors.push(format!(
                "{}: \"{}\" is not a gh account name",
                b.key, x.account
            ));
        } else if let Some(list) = accounts {
            if !list.iter().any(|a| a.name == x.account && a.logged_in) {
                c.warnings
                    .push(format!("{}: {} is not logged in to gh", b.key, x.account));
            }
        }
    }
    c
}

// ---- preview ----

/// How the draft's rules decide one known project.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreviewRow {
    /// "~/work/team-hub".
    pub key: String,
    /// The deciding rule (an index into the draft's rules), None: undefined.
    pub rule: Option<usize>,
    pub owner: Option<String>,
    pub account: Option<String>,
    pub choices: Vec<String>,
    pub state_exclude: bool,
    /// A saved binding (after the draft's binding edits): owner/repo as account.
    pub bound: Option<String>,
    /// "~/work/team-hub -> your-org (your-github-user)".
    pub line: String,
}

/// How `dir` resolves under the draft's rules (no git is run).
pub fn preview_one(cfg: &GitCfg, dir: &Path) -> PreviewRow {
    let key = git_map::key(dir);
    let mut r = PreviewRow {
        key: key.clone(),
        rule: None,
        owner: None,
        account: None,
        choices: vec![],
        state_exclude: false,
        bound: None,
        line: String::new(),
    };
    let Some(rule) = git_map::rule_for(cfg, dir) else {
        r.line = format!("{key} -> undefined (no rule covers it)");
        return r;
    };
    r.rule = cfg.rules.iter().position(|x| std::ptr::eq(x, rule));
    r.state_exclude = rule.state.as_deref() == Some("exclude");
    let kept = if r.state_exclude {
        ", kept out of the state repo"
    } else {
        ""
    };
    if rule.owner.eq_ignore_ascii_case("choose") {
        r.choices = rule.choices.clone();
        r.line = format!("{key} -> choose: {}{kept}", rule.choices.join(" or "));
        return r;
    }
    r.owner = Some(rule.owner.clone());
    r.account = git_map::account_for(cfg, &rule.owner);
    r.line = format!(
        "{key} -> {} ({}){kept}",
        rule.owner,
        r.account.as_deref().unwrap_or("no account set")
    );
    r
}

/// Every known project under the draft (sorted by folder).
pub fn preview(d: &Draft, base: &GitCfg, projects: &[PathBuf], store: &Store) -> Vec<PreviewRow> {
    let cfg = d.to_cfg(base);
    let store = with_binds(store, d);
    let mut out: Vec<PreviewRow> = projects
        .iter()
        .map(|p| {
            let mut r = preview_one(&cfg, p);
            r.bound = store
                .get(p)
                .map(|b| format!("{}/{} as {}", b.owner, b.repo, b.account));
            r
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// The store as it would be after the draft's binding edits (in memory).
pub fn with_binds(store: &Store, d: &Draft) -> Store {
    let mut s = store.clone();
    for b in &d.binds {
        let k = git_map::key(&crate::config::expand_tilde(&b.key));
        match &b.binding {
            Some(x) => {
                s.projects.insert(k, x.clone());
            }
            None => {
                s.projects.remove(&k);
            }
        }
    }
    s
}

// ---- the diff the confirmation shows ----

fn rule_words(r: &GitRule) -> String {
    let r = clean_rule(r);
    let who = if r.owner == "choose" {
        format!("choose {}", r.choices.join(" or "))
    } else {
        r.owner.clone()
    };
    let st = if r.state.as_deref() == Some("exclude") {
        ", conversations and .env kept out of the state repo"
    } else {
        ""
    };
    format!("{} -> {who}{st}", r.pattern)
}

fn bind_words(b: &Binding) -> String {
    format!("{}/{} as {}", b.owner, b.repo, b.account)
}

/// What saving `new` over `old` changes, line by line ("+", "-", "~").
/// Empty: nothing to save.
pub fn diff(old: &Draft, new: &Draft, store: &Store) -> Vec<String> {
    let mut out = vec![];
    let find = |d: &Draft, o: &str| {
        d.owners
            .iter()
            .find(|r| r.owner.trim() == o)
            .map(|r| r.account.trim().to_string())
    };
    for r in &old.owners {
        if find(new, r.owner.trim()).is_none() {
            out.push(format!("- owner {} -> {}", r.owner, r.account));
        }
    }
    for r in &new.owners {
        let (o, a) = (r.owner.trim(), r.account.trim());
        let mark = if is_placeholder(o) {
            " (placeholder)"
        } else {
            ""
        };
        match find(old, o) {
            None => out.push(format!("+ owner {o} -> {a}{mark}")),
            Some(was) if was != a => out.push(format!("~ owner {o}: {was} -> {a}{mark}")),
            _ => {}
        }
    }
    let olds: Vec<GitRule> = old.rules.iter().map(clean_rule).collect();
    let news: Vec<GitRule> = new.rules.iter().map(clean_rule).collect();
    if olds != news {
        for r in &olds {
            if !news.iter().any(|n| n.pattern == r.pattern) {
                out.push(format!("- rule {}", rule_words(r)));
            }
        }
        for r in &news {
            match olds.iter().find(|o| o.pattern == r.pattern) {
                None => out.push(format!("+ rule {}", rule_words(r))),
                Some(o) if o != r => {
                    out.push(format!("~ rule {} (was {})", rule_words(r), rule_words(o)))
                }
                _ => {}
            }
        }
        let order = |v: &[GitRule]| v.iter().map(|r| r.pattern.clone()).collect::<Vec<_>>();
        let common_old: Vec<String> = order(&olds)
            .into_iter()
            .filter(|p| news.iter().any(|n| &n.pattern == p))
            .collect();
        let common_new: Vec<String> = order(&news)
            .into_iter()
            .filter(|p| olds.iter().any(|o| &o.pattern == p))
            .collect();
        if common_old != common_new || news.len() > 1 {
            out.push("Rules in order (ties go to the higher one):".into());
            for (i, r) in news.iter().enumerate() {
                out.push(format!("  {}. {}", i + 1, rule_words(r)));
            }
        }
    }
    for b in &new.binds {
        let k = git_map::key(&crate::config::expand_tilde(&b.key));
        let was = store.projects.get(&k);
        match (&b.binding, was) {
            (Some(x), None) => out.push(format!("+ bind {k} -> {}", bind_words(x))),
            (Some(x), Some(w))
                if (x.owner.as_str(), x.repo.as_str(), x.account.as_str())
                    != (w.owner.as_str(), w.repo.as_str(), w.account.as_str()) =>
            {
                out.push(format!(
                    "~ bind {k}: {} -> {}",
                    bind_words(w),
                    bind_words(x)
                ))
            }
            (None, Some(w)) => out.push(format!("- unbind {k} (was {})", bind_words(w))),
            _ => {}
        }
    }
    out
}

// ---- writing ----

fn set_str(t: &mut Table, k: &str, v: &str) {
    if t.get(k).and_then(|i| i.as_str()) == Some(v) {
        return;
    }
    let decor = t
        .get(k)
        .and_then(|i| i.as_value())
        .map(|x| x.decor().clone());
    t.insert(k, value(v));
    if let (Some(d), Some(x)) = (decor, t.get_mut(k).and_then(|i| i.as_value_mut())) {
        *x.decor_mut() = d;
    }
}

/// config.toml's text with `[git]` owners and rules set to the draft's.
/// Comments, layout and every other key stay as they were; a rule kept
/// from before keeps its own comments.
pub fn write_toml(text: &str, d: &Draft) -> Result<String, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("config.toml does not parse: {e}"))?;
    if doc.get("git").is_none() {
        doc.insert("git", Item::Table(Table::new()));
    }
    let git = doc
        .get_mut("git")
        .and_then(Item::as_table_mut)
        .ok_or("[git] in config.toml is not a table")?;
    // Owners: drop the ones gone, set the rest in place.
    if git.get("owners").and_then(Item::as_table).is_none() {
        git.insert("owners", Item::Table(Table::new()));
    }
    {
        let owners = git
            .get_mut("owners")
            .and_then(Item::as_table_mut)
            .ok_or("[git.owners] is not a table")?;
        let keep: Vec<String> = d
            .owners
            .iter()
            .map(|r| r.owner.trim().to_string())
            .collect();
        let gone: Vec<String> = owners
            .iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| !keep.contains(k))
            .collect();
        for k in gone {
            owners.remove(&k);
        }
        for r in &d.owners {
            let (o, a) = (r.owner.trim(), r.account.trim());
            let new = owners.get(o).is_none();
            set_str(owners, o, a);
            if new && is_placeholder(o) {
                if let Some(v) = owners.get_mut(o).and_then(|i| i.as_value_mut()) {
                    v.decor_mut().set_suffix(format!("   {PLACEHOLDER_NOTE}"));
                }
            }
        }
    }
    // Rules: in the draft's order, each reusing its old table (and its
    // comments) when the glob was there before.
    let mut old: Vec<Option<Table>> = git
        .get("rules")
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().cloned().map(Some).collect())
        .unwrap_or_default();
    let mut positions: Vec<usize> = old.iter().flatten().filter_map(Table::position).collect();
    positions.sort_unstable();
    let mut arr = ArrayOfTables::new();
    for (i, r) in d.rules.iter().map(clean_rule).enumerate() {
        let reuse = old.iter_mut().find(|t| {
            t.as_ref()
                .and_then(|t| t.get("match"))
                .and_then(|m| m.as_str())
                .map(str::trim)
                == Some(r.pattern.as_str())
        });
        let mut t = reuse.and_then(Option::take).unwrap_or_default();
        set_str(&mut t, "match", &r.pattern);
        set_str(&mut t, "owner", &r.owner);
        if r.owner == "choose" {
            let same = t.get("choices").and_then(Item::as_array).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            }) == Some(r.choices.clone());
            if !same {
                let mut a = Array::new();
                for c in &r.choices {
                    a.push(c.as_str());
                }
                t.insert("choices", value(a));
            }
        } else {
            t.remove("choices");
        }
        match &r.state {
            Some(s) => set_str(&mut t, "state", s),
            None => {
                t.remove("state");
            }
        }
        // Keep the rules where they were in the file, in the new order.
        if let Some(p) = positions.get(i).or(positions.last()) {
            t.set_position(*p);
        }
        arr.push(t);
    }
    if arr.is_empty() {
        git.remove("rules");
    } else {
        git.insert("rules", Item::ArrayOfTables(arr));
    }
    let out = doc.to_string();
    // What was written must read back as the draft.
    let back = crate::config::Config::parse(&out).map_err(|e| format!("{e:#}"))?;
    let want = d.to_cfg(&back.git);
    if back.git.owners != want.owners || back.git.rules != want.rules {
        return Err("the written config did not read back the same; nothing was saved".into());
    }
    Ok(out)
}

/// Apply the draft's binding edits to the store (each checked).
pub fn apply_binds(store: &mut Store, d: &Draft) -> Result<usize, String> {
    let mut n = 0;
    for b in &d.binds {
        let p = crate::config::expand_tilde(&b.key);
        match &b.binding {
            Some(x) => {
                let mut x = x.clone();
                x.source = "manual".into();
                x.defined_at = String::new();
                store.bind(&p, x)?;
            }
            None => {
                if let Some(why) = crate::guard::never_folder(&p) {
                    return Err(why);
                }
                store.projects.remove(&git_map::key(&p));
            }
        }
        n += 1;
    }
    Ok(n)
}

/// Save the draft: config.toml (owners, rules) then the bindings. Refused
/// when the check has errors.
pub fn save(config_path: &Path, d: &Draft, accounts: Option<&[Acct]>) -> Result<String, String> {
    let c = check(d, accounts);
    if !c.ok() {
        return Err(c.errors.join("; "));
    }
    let text = std::fs::read_to_string(config_path).unwrap_or_default();
    let out = write_toml(&text, d)?;
    if out != text {
        crate::config::write_atomic(config_path, out.as_bytes()).map_err(|e| format!("{e:#}"))?;
    }
    let mut n = 0;
    if !d.binds.is_empty() {
        let mut store = Store::load();
        n = apply_binds(&mut store, d)?;
        store.save()?;
    }
    Ok(format!(
        "Saved {} owners and {} folder rules{}",
        d.owners.len(),
        d.rules.len(),
        if n > 0 {
            format!(", {n} binding changes")
        } else {
            String::new()
        }
    ))
}

#[cfg(test)]
#[path = "git_cfg_tests.rs"]
mod tests;
