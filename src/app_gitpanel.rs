//! The Git section of a tab in the terminal UI (tab options "Git..."),
//! the web sheet's `git` frames, the badges in the tab list, and the
//! assistant's git tools: read only (git_log, git_diff, git_branches,
//! git_bindings) or preparing something for the user to confirm
//! (git_draft_commit, git_propose_binding). No tool commits, pushes,
//! pulls, switches, binds or creates a repo: only the user's own press in
//! this section does, after its confirmation.
//!
//! Git work runs on background threads; the UI thread only polls slots.

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{App, Modal};
use crate::git_map::Binding;
use crate::git_panel::{self, Action, Env, Plan, View};
use crate::hits::UiAction;
use crate::tab_browser::{DlgLine, DlgView, Tone};

type Slot<T> = Arc<Mutex<Option<T>>>;

fn take<T>(s: &Slot<T>) -> Option<T> {
    s.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// What the section is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum GitStep {
    Main,
    /// The commit box: message, everything (true) or staged only.
    Commit {
        text: String,
        all: bool,
    },
    /// Owner, repo name and account (field 0, 1, 2); `create` makes the
    /// repo after (else only the binding is saved).
    Define {
        owner: usize,
        repo: String,
        account: usize,
        field: u8,
        create: bool,
    },
    Branches {
        sel: usize,
    },
    NewBranch(String),
    /// Look at what will happen; Enter is the user's yes.
    Confirm(Box<Plan>),
    /// Secret findings: the user types "push anyway".
    Typing(Box<Plan>, String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitDlg {
    pub uid: u64,
    pub dir: PathBuf,
    pub sel: usize,
    pub step: GitStep,
    /// The last result: (text, good).
    pub note: Option<(String, bool)>,
    /// Working on it: what.
    pub busy: Option<String>,
    /// The last failure was a diverged branch (offer push as a branch).
    pub diverged: bool,
}

#[derive(Default)]
pub struct GitState {
    pub view: Option<View>,
    view_slot: Slot<View>,
    plan_slot: Slot<Plan>,
    run_slot: Slot<Result<String, String>>,
    accounts: Slot<Vec<String>>,
    pub known_accounts: Vec<String>,
    asked_accounts: bool,
    last_fetch: Option<Instant>,
    /// When the shown view was asked for (read again every few seconds).
    view_at: Option<Instant>,
    /// Folder badges from the cache: (when looked, badge).
    badges: RefCell<HashMap<PathBuf, (Instant, Option<String>)>>,
    /// Settings > GitHub accounts and repos (app_gitcfg).
    pub cfg_ui: crate::app_gitcfg::CfgUi,
}

fn dl(text: impl Into<String>, tone: Tone) -> DlgLine {
    DlgLine {
        text: text.into(),
        tone,
        action: None,
    }
}

/// A heuristic first message from the change list (the assistant's draft
/// wins when there is one).
fn first_message(v: &View) -> String {
    let names: Vec<String> = v
        .changes
        .files
        .iter()
        .map(|f| f.path.clone())
        .chain(v.changes.untracked.iter().cloned())
        .map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())
        .collect();
    match names.len() {
        0 => String::new(),
        1..=3 => format!("Update {}", names.join(", ")),
        n => format!("Update {}, {} and {} more", names[0], names[1], n - 2),
    }
}

impl App {
    fn git_env(&self) -> Env {
        Env::new(&self.cfg.git)
    }

    /// Open the Git section for tab (s, t).
    pub fn open_git(&mut self, s: usize, t: usize) {
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return;
        };
        let uid = tab.uid;
        let dir = self.tab_dir(s, t);
        self.focus = s;
        self.git.view = None;
        self.modal = Modal::Git(GitDlg {
            uid,
            dir,
            sel: 0,
            step: GitStep::Main,
            note: None,
            busy: Some("Reading git...".into()),
            diverged: false,
        });
        self.git_ask_accounts();
        self.git_refresh_view();
    }

    /// The gh accounts (once per run, in the background; no tokens).
    fn git_ask_accounts(&mut self) {
        if self.git.asked_accounts || cfg!(test) {
            return;
        }
        self.git.asked_accounts = true;
        let gh = crate::git_cred::gh_bin(Some(&self.cfg.git));
        let slot = Arc::clone(&self.git.accounts);
        std::thread::spawn(move || {
            let a = crate::git_cred::accounts(&gh);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(a);
        });
    }

    fn git_dlg(&self) -> Option<GitDlg> {
        match &self.modal {
            Modal::Git(d) => Some(d.clone()),
            _ => None,
        }
    }

    fn set_git(&mut self, d: GitDlg) {
        if matches!(self.modal, Modal::Git(_)) {
            self.modal = Modal::Git(d);
        }
    }

    pub fn git_refresh_view(&mut self) {
        let Some(d) = self.git_dlg() else { return };
        let env = self.git_env();
        let accts = self.git.known_accounts.clone();
        let slot = Arc::clone(&self.git.view_slot);
        self.git.view_at = Some(Instant::now());
        let work = move || {
            let v = git_panel::view(&d.dir, &env, &accts);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
        };
        if cfg!(test) {
            work();
            self.git_tick();
        } else {
            std::thread::spawn(work);
        }
    }

    fn git_prepare(&mut self, action: Action) {
        let Some(mut d) = self.git_dlg() else { return };
        d.busy = Some(format!("Checking: {}...", action.title()));
        d.note = None;
        let dir = d.dir.clone();
        self.set_git(d);
        let env = self.git_env();
        let slot = Arc::clone(&self.git.plan_slot);
        let work = move || {
            let p = git_panel::prepare(&dir, action, &env);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
        };
        if cfg!(test) {
            work();
            self.git_tick();
        } else {
            std::thread::spawn(work);
        }
    }

    /// The user's yes (Enter in Confirm, or the typed override).
    fn git_execute(&mut self, plan: Plan, typed: Option<String>) {
        let Some(mut d) = self.git_dlg() else { return };
        d.busy = Some(format!("{}...", plan.title));
        d.step = GitStep::Main;
        self.set_git(d);
        let env = self.git_env();
        let slot = Arc::clone(&self.git.run_slot);
        let work = move || {
            let r = git_panel::execute(&plan, typed.as_deref(), &env);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        };
        if cfg!(test) {
            work();
            self.git_tick();
        } else {
            std::thread::spawn(work);
        }
    }

    /// Per tick: results of the section's background work; the read only
    /// fetch the user turned on (`[git] fetch_every_min`).
    pub fn git_tick(&mut self) {
        self.gitcfg_tick();
        if let Some(a) = take(&self.git.accounts) {
            self.git.known_accounts = a;
        }
        if let Some(v) = take(&self.git.view_slot) {
            if let Some(mut d) = self.git_dlg() {
                if d.busy.as_deref() == Some("Reading git...") {
                    d.busy = None;
                }
                d.sel = d.sel.min(v.actions.len().saturating_sub(1));
                self.set_git(d);
            }
            self.git.view = Some(v);
        }
        if let Some(p) = take(&self.git.plan_slot) {
            if let Some(mut d) = self.git_dlg() {
                d.busy = None;
                d.step = GitStep::Confirm(Box::new(p));
                self.set_git(d);
            }
        }
        if let Some(r) = take(&self.git.run_slot) {
            if let Some(mut d) = self.git_dlg() {
                d.busy = None;
                d.diverged = matches!(&r, Err(e) if e.starts_with("Diverged"));
                d.note = Some(match r {
                    Ok(s) => (s, true),
                    Err(e) => (e, false),
                });
                self.set_git(d.clone());
                self.git_refresh_view();
                project_index_stale(self, &d.dir);
            }
        }
        // The main list stays true while it is open (a tab may commit).
        if let Some(d) = self.git_dlg() {
            if d.step == GitStep::Main
                && d.busy.is_none()
                && self
                    .git
                    .view_at
                    .is_some_and(|t| t.elapsed() > Duration::from_secs(4))
            {
                self.git_refresh_view();
            }
        }
        self.git_fetch_tick();
    }

    fn git_fetch_tick(&mut self) {
        let every = self.cfg.git.fetch_every_min;
        if every == 0 || cfg!(test) {
            return;
        }
        if self
            .git
            .last_fetch
            .is_some_and(|t| t.elapsed() < Duration::from_secs(every * 60))
        {
            return;
        }
        self.git.last_fetch = Some(Instant::now());
        let mut tops: Vec<PathBuf> = vec![];
        for (s, p) in self.panes.iter().enumerate() {
            for t in 0..p.tabs.len() {
                if let Some(top) = crate::project_git::repo_top(&self.tab_dir(s, t)) {
                    if !tops.contains(&top) {
                        tops.push(top);
                    }
                }
            }
        }
        let env = self.git_env();
        let shared = Arc::clone(&self.project_index);
        std::thread::spawn(move || {
            let store = crate::git_map::Store::load();
            for top in &tops {
                if crate::guard::never_folder(top).is_some() {
                    continue;
                }
                let facts = crate::project_git::locate(top).map(|(t, g, c)| {
                    crate::project_git::compute(&t, &g, &c, Duration::from_secs(5))
                });
                if facts.as_ref().is_none_or(|f| f.remotes.is_empty()) {
                    continue;
                }
                let r = crate::git_map::resolve(top, facts.as_ref(), &env.cfg, &store);
                let ctx = crate::git_ops::Ctx {
                    account: r.account,
                    helper: env.helper.clone(),
                    gh: Some(env.gh.clone()),
                    limit: Duration::from_secs(60),
                    ..crate::git_ops::Ctx::new(top)
                };
                if let Err(e) = crate::git_ops::fetch(&ctx) {
                    crate::log::info(&format!(
                        "git: fetch {} failed: {e}",
                        crate::config::tilde(top)
                    ));
                }
            }
            crate::project_git::update(
                &shared,
                Some(&tops),
                &crate::project_git::Limits::tool(),
                true,
            );
        });
    }

    /// The badge for a folder in the tab list (from the cache, looked at
    /// most every 5 s per folder; never runs git).
    pub fn git_badge(&self, dir: &Path) -> Option<String> {
        if let Some((t, b)) = self.git.badges.borrow().get(dir) {
            if t.elapsed() < Duration::from_secs(5) {
                return b.clone();
            }
        }
        let b = {
            let ix = self.project_index.try_lock().ok()?;
            git_panel::badge(&ix, dir)
        };
        self.git
            .badges
            .borrow_mut()
            .insert(dir.to_path_buf(), (Instant::now(), b.clone()));
        b
    }

    // ---- keys and clicks ----

    fn git_action_rows(&self) -> Vec<crate::git_panel::ActionRow> {
        self.git
            .view
            .as_ref()
            .map(|v| v.actions.clone())
            .unwrap_or_default()
    }

    /// A press of action `id` in the main list.
    pub fn git_activate(&mut self, id: &str) {
        let Some(mut d) = self.git_dlg() else { return };
        let Some(v) = self.git.view.clone() else {
            return;
        };
        if let Some(r) = v.actions.iter().find(|a| a.id == id) {
            if let Some(why) = &r.disabled {
                d.note = Some((format!("{}: {why}", r.label), false));
                self.set_git(d);
                return;
            }
        } else if id != "push_as_branch" {
            return;
        }
        match id {
            "init" => self.git_prepare(Action::Init),
            "commit" => {
                let text = v.draft.clone().unwrap_or_else(|| first_message(&v));
                d.step = GitStep::Commit { text, all: true };
                self.set_git(d);
            }
            "push" => self.git_prepare(Action::Push),
            "pull" => self.git_prepare(Action::Pull),
            "sync" => self.git_prepare(Action::Sync),
            "fetch" => self.git_prepare(Action::Fetch),
            "push_as_branch" => self.git_prepare(Action::PushAsBranch),
            "branches" => {
                let sel = v.branches.iter().position(|b| b.current).unwrap_or(0);
                d.step = GitStep::Branches { sel };
                self.set_git(d);
            }
            "define" | "create" => {
                let r = &v.resolved;
                let prop = v.proposal.clone();
                let want_owner = prop
                    .as_ref()
                    .map(|p| p.owner.clone())
                    .or_else(|| r.owner.clone());
                let owner = want_owner
                    .and_then(|o| v.owners.iter().position(|x| x.eq_ignore_ascii_case(&o)))
                    .unwrap_or(0);
                let want_acct = prop
                    .as_ref()
                    .map(|p| p.account.clone())
                    .or_else(|| r.account.clone());
                let account = want_acct
                    .and_then(|a| v.accounts.iter().position(|x| *x == a))
                    .unwrap_or(0);
                let repo = prop
                    .map(|p| p.repo)
                    .or_else(|| r.repo.clone())
                    .unwrap_or_else(|| crate::git_map::repo_name_for(&d.dir));
                d.step = GitStep::Define {
                    owner,
                    repo,
                    account,
                    field: 1,
                    create: id == "create",
                };
                self.set_git(d);
            }
            "ignores" => self.git_prepare(Action::AddIgnores),
            "untrack" => self.git_prepare(Action::Untrack),
            _ => {}
        }
    }

    /// A click on row `i` (an action, or a branch in Branches).
    pub fn git_click_row(&mut self, i: usize) {
        let Some(mut d) = self.git_dlg() else { return };
        match &d.step {
            GitStep::Branches { .. } => {
                d.step = GitStep::Branches { sel: i };
                self.set_git(d);
                self.on_git_key(KeyEvent::from(KeyCode::Enter));
            }
            GitStep::Main => {
                let rows = self.git_action_rows();
                if let Some(r) = rows.get(i) {
                    d.sel = i;
                    let id = r.id;
                    self.set_git(d);
                    self.git_activate(id);
                }
            }
            _ => {}
        }
    }

    pub fn on_git_key(&mut self, k: KeyEvent) {
        let Some(mut d) = self.git_dlg() else { return };
        if d.busy.is_some() {
            if k.code == KeyCode::Esc {
                self.modal = Modal::None;
            }
            return;
        }
        let v = self.git.view.clone().unwrap_or_default();
        match d.step.clone() {
            GitStep::Main => {
                let n = v.actions.len();
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        d.sel = d.sel.saturating_sub(1);
                        self.set_git(d);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        d.sel = (d.sel + 1).min(n.saturating_sub(1));
                        self.set_git(d);
                    }
                    KeyCode::Enter => {
                        if let Some(r) = v.actions.get(d.sel) {
                            let id = r.id;
                            self.git_activate(id);
                        }
                    }
                    KeyCode::Char('r') => {
                        d.note = None;
                        d.busy = Some("Reading git...".into());
                        self.set_git(d);
                        self.git_refresh_view();
                    }
                    KeyCode::Char('b') if d.diverged => self.git_activate("push_as_branch"),
                    KeyCode::Char(c) => {
                        let id = match c {
                            'c' => "commit",
                            'p' => "push",
                            'l' => "pull",
                            's' => "sync",
                            'f' => "fetch",
                            'B' => "branches",
                            'd' => "define",
                            'n' => "create",
                            'i' => "init",
                            _ => return,
                        };
                        self.git_activate(id);
                    }
                    _ => {}
                }
            }
            GitStep::Commit { mut text, mut all } => {
                match k.code {
                    KeyCode::Esc => d.step = GitStep::Main,
                    KeyCode::Tab => all = !all,
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Enter => {
                        self.git_prepare(Action::Commit { message: text, all });
                        return;
                    }
                    KeyCode::Char(c) if text.chars().count() < 400 => text.push(c),
                    _ => {}
                }
                if matches!(d.step, GitStep::Commit { .. }) {
                    d.step = GitStep::Commit { text, all };
                }
                self.set_git(d);
            }
            GitStep::Define {
                mut owner,
                mut repo,
                mut account,
                mut field,
                create,
            } => {
                let no = v.owners.len().max(1);
                let na = v.accounts.len().max(1);
                match k.code {
                    KeyCode::Esc => {
                        d.step = GitStep::Main;
                        self.set_git(d);
                        return;
                    }
                    KeyCode::Tab | KeyCode::Down => field = (field + 1) % 3,
                    KeyCode::BackTab | KeyCode::Up => field = (field + 2) % 3,
                    KeyCode::Left | KeyCode::Right => {
                        let back = k.code == KeyCode::Left;
                        let step =
                            |x: usize, n: usize| if back { (x + n - 1) % n } else { (x + 1) % n };
                        match field {
                            0 => owner = step(owner, no),
                            2 => account = step(account, na),
                            _ => {}
                        }
                    }
                    KeyCode::Backspace if field == 1 => {
                        repo.pop();
                    }
                    KeyCode::Char(c) if field == 1 && repo.len() < 100 => repo.push(c),
                    KeyCode::Enter => {
                        let o = v.owners.get(owner).cloned().unwrap_or_default();
                        let a = v.accounts.get(account).cloned().unwrap_or_default();
                        let action = if create {
                            Action::CreateRepo {
                                owner: o,
                                name: repo,
                                account: a,
                                push: true,
                            }
                        } else {
                            Action::Bind {
                                owner: o,
                                repo,
                                account: a,
                            }
                        };
                        self.git_prepare(action);
                        return;
                    }
                    _ => {}
                }
                d.step = GitStep::Define {
                    owner,
                    repo,
                    account,
                    field,
                    create,
                };
                self.set_git(d);
            }
            GitStep::Branches { sel } => {
                let n = v.branches.len();
                match k.code {
                    KeyCode::Esc => d.step = GitStep::Main,
                    KeyCode::Up | KeyCode::Char('k') => {
                        d.step = GitStep::Branches {
                            sel: sel.saturating_sub(1),
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        d.step = GitStep::Branches {
                            sel: (sel + 1).min(n.saturating_sub(1)),
                        }
                    }
                    KeyCode::Char('n') => d.step = GitStep::NewBranch(String::new()),
                    KeyCode::Enter | KeyCode::Char('s') => {
                        if let Some(b) = v.branches.get(sel).filter(|b| !b.current) {
                            self.git_prepare(Action::Switch {
                                branch: b.name.clone(),
                                stash: k.code == KeyCode::Char('s'),
                            });
                            return;
                        }
                    }
                    _ => {}
                }
                self.set_git(d);
            }
            GitStep::NewBranch(mut name) => {
                match k.code {
                    KeyCode::Esc => d.step = GitStep::Main,
                    KeyCode::Backspace => {
                        name.pop();
                        d.step = GitStep::NewBranch(name);
                    }
                    KeyCode::Enter => {
                        self.git_prepare(Action::CreateBranch { name });
                        return;
                    }
                    KeyCode::Char(c) if name.len() < 100 => {
                        name.push(c);
                        d.step = GitStep::NewBranch(name);
                    }
                    _ => {}
                }
                self.set_git(d);
            }
            GitStep::Confirm(p) => match k.code {
                KeyCode::Enter | KeyCode::Char('y') if p.can_run() => {
                    if p.needs_typed {
                        d.step = GitStep::Typing(p, String::new());
                        self.set_git(d);
                    } else {
                        self.git_execute(*p, None);
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Enter => {
                    d.step = GitStep::Main;
                    self.set_git(d);
                }
                _ => {}
            },
            GitStep::Typing(p, mut t) => match k.code {
                KeyCode::Esc => {
                    d.step = GitStep::Main;
                    self.set_git(d);
                }
                KeyCode::Enter => self.git_execute(*p, Some(t)),
                KeyCode::Backspace => {
                    t.pop();
                    d.step = GitStep::Typing(p, t);
                    self.set_git(d);
                }
                KeyCode::Char(c) if t.len() < 40 => {
                    t.push(c);
                    d.step = GitStep::Typing(p, t);
                    self.set_git(d);
                }
                _ => {}
            },
        }
    }

    // ---- what the dialog shows ----

    pub fn git_dialog_view(&self, d: &GitDlg) -> DlgView {
        let name = self
            .find_tab(d.uid)
            .map(|(s, t)| self.panes[s].tabs[t].name())
            .unwrap_or_default();
        let title = format!("git · {}", crate::sessions::snippet(&name, 30));
        let mut lines: Vec<DlgLine> = vec![];
        let btn = |l: &str, c: char, h: &str| (l.to_string(), UiAction::GitKey(c), h.to_string());
        let mut buttons = vec![
            btn("Refresh", 'r', "Read git again (r)"),
            btn("Close", '\x1b', "Esc"),
        ];
        let Some(v) = &self.git.view else {
            lines.push(dl(
                d.busy.clone().unwrap_or_else(|| "Reading git...".into()),
                Tone::Dim,
            ));
            return DlgView {
                title,
                lines,
                buttons,
            };
        };
        let r = &v.resolved;
        if let Some(why) = &v.refused {
            lines.push(dl(format!("Refused: {why}"), Tone::Warn));
            return DlgView {
                title,
                lines,
                buttons,
            };
        }
        // Header.
        let top = v.top.clone().unwrap_or_else(|| v.dir.clone());
        if v.is_repo {
            lines.push(dl(
                format!("{top} · {} · {}", v.branch, v.remote),
                Tone::Normal,
            ));
        } else {
            lines.push(dl(format!("{top} · not a git repo"), Tone::Normal));
        }
        let gh = match (r.slug(), &r.account) {
            (Some(s), Some(a)) => format!("GitHub {s} as {a} ({})", source_word(r)),
            (Some(s), None) => format!("GitHub {s}, account not set ({})", source_word(r)),
            _ => match r.owner.as_deref() {
                Some(o) => format!("owner {o} ({})", source_word(r)),
                None => "No repo defined".to_string(),
            },
        };
        lines.push(dl(gh, Tone::Dim));
        if let Some(a) = &r.ask {
            lines.push(dl(a.clone(), Tone::Warn));
        }
        if v.is_repo {
            let mut st = v.state.clone();
            if !v.fetched.is_empty() {
                st.push_str(&format!(" · {}", v.fetched));
            }
            lines.push(dl(st, Tone::Dim));
        }
        for w in &v.warnings {
            lines.push(dl(format!("⚠ {w}"), Tone::Warn));
        }
        if let Some(p) = &v.proposal {
            lines.push(dl(
                format!(
                    "The assistant proposes {}/{} as {} (Define... to use it)",
                    p.owner, p.repo, p.account
                ),
                Tone::Dim,
            ));
        }
        lines.push(dl("", Tone::Dim));
        match &d.step {
            GitStep::Main => {
                for (i, a) in v.actions.iter().enumerate() {
                    let mark = if i == d.sel { "›" } else { " " };
                    let text = match &a.disabled {
                        Some(why) => format!("{mark} {:<24} ({why})", a.label),
                        None => format!("{mark} {:<24} {}", a.label, a.hint),
                    };
                    lines.push(DlgLine {
                        text,
                        tone: if a.disabled.is_some() {
                            Tone::Dim
                        } else {
                            Tone::Normal
                        },
                        action: Some(UiAction::GitRow(i)),
                    });
                }
                if d.diverged {
                    lines.push(DlgLine {
                        text: "  Push as a branch (b): your commits go to godterm/<machine>/<branch>-<date>".into(),
                        tone: Tone::Warn,
                        action: Some(UiAction::GitKey('b')),
                    });
                }
                if !v.commits.is_empty() {
                    lines.push(dl("", Tone::Dim));
                    lines.push(dl("Commits (↑ not pushed)", Tone::Dim));
                    for c in v.commits.iter().take(8) {
                        lines.push(dl(
                            format!(
                                "{} {} {}  {}",
                                if c.unpushed { "↑" } else { " " },
                                c.hash,
                                crate::sessions::snippet(&c.subject, 50),
                                c.when
                            ),
                            if c.unpushed { Tone::Warn } else { Tone::Normal },
                        ));
                    }
                }
                let ch = &v.changes;
                if ch.staged_n + ch.unstaged_n + ch.untracked.len() > 0 {
                    lines.push(dl("", Tone::Dim));
                    lines.push(dl(
                        format!(
                            "Changes: {} staged, {} unstaged, {} untracked (+{} -{})",
                            ch.staged_n,
                            ch.unstaged_n,
                            ch.untracked.len(),
                            ch.added,
                            ch.removed
                        ),
                        Tone::Dim,
                    ));
                    for f in ch.files.iter().take(6) {
                        lines.push(dl(
                            format!(
                                "  {} {}  +{} -{}",
                                if f.staged { "S" } else { "M" },
                                f.path,
                                f.added.unwrap_or(0),
                                f.removed.unwrap_or(0)
                            ),
                            Tone::Normal,
                        ));
                    }
                }
            }
            GitStep::Commit { text, all } => {
                lines.push(dl(
                    format!(
                        "Commit {} · Tab switches",
                        if *all {
                            "everything (new files too)"
                        } else {
                            "staged changes only"
                        }
                    ),
                    Tone::Dim,
                ));
                lines.push(dl(format!("Message: {text}▏"), Tone::Normal));
                if v.draft.is_some() {
                    lines.push(dl(
                        "Drafted by the assistant; edit it before you commit.",
                        Tone::Dim,
                    ));
                }
                buttons = vec![
                    btn("Check and commit", '\n', "Enter"),
                    btn("Back", '\x1b', "Esc"),
                ];
            }
            GitStep::Define {
                owner,
                repo,
                account,
                field,
                create,
            } => {
                let o = v
                    .owners
                    .get(*owner)
                    .cloned()
                    .unwrap_or_else(|| "(no owners: add [git.owners])".into());
                let a = v
                    .accounts
                    .get(*account)
                    .cloned()
                    .unwrap_or_else(|| "(no gh accounts found)".into());
                let f = |i: u8, label: &str, val: &str| {
                    let cur = if *field == i { "›" } else { " " };
                    let edit = if *field == i && i == 1 { "▏" } else { "" };
                    let arrows = if i != 1 { "  ← →" } else { "" };
                    dl(
                        format!("{cur} {label:<9} {val}{edit}{arrows}"),
                        Tone::Normal,
                    )
                };
                lines.push(dl(
                    if *create {
                        "Create a private repo on GitHub (never public). Check the name:"
                    } else {
                        "Which GitHub repo this folder belongs to:"
                    },
                    Tone::Dim,
                ));
                lines.push(f(0, "Owner", &o));
                lines.push(f(1, "Repo", repo));
                lines.push(f(2, "Account", &a));
                buttons = vec![
                    btn(
                        if *create {
                            "Check and create"
                        } else {
                            "Check and save"
                        },
                        '\n',
                        "Enter",
                    ),
                    btn("Back", '\x1b', "Esc"),
                ];
            }
            GitStep::Branches { sel } => {
                lines.push(dl(
                    "Branches: Enter switches (clean tree), s switches with stash, n new",
                    Tone::Dim,
                ));
                for (i, b) in v.branches.iter().enumerate() {
                    lines.push(DlgLine {
                        text: format!(
                            "{} {}{:<30} {}  {}",
                            if i == *sel { "›" } else { " " },
                            if b.current { "* " } else { "  " },
                            b.name,
                            b.upstream.clone().unwrap_or_else(|| "no upstream".into()),
                            b.when
                        ),
                        tone: if b.current { Tone::Good } else { Tone::Normal },
                        action: Some(UiAction::GitRow(i)),
                    });
                }
                buttons = vec![btn("New branch", 'n', "n"), btn("Back", '\x1b', "Esc")];
            }
            GitStep::NewBranch(name) => {
                lines.push(dl("New branch from here, then switch to it:", Tone::Dim));
                lines.push(dl(format!("Name: {name}▏"), Tone::Normal));
                buttons = vec![
                    btn("Check and create", '\n', "Enter"),
                    btn("Back", '\x1b', "Esc"),
                ];
            }
            GitStep::Confirm(p) | GitStep::Typing(p, _) => {
                lines.push(dl(p.title.clone(), Tone::Normal));
                for l in &p.lines {
                    lines.push(dl(format!("  {l}"), Tone::Dim));
                }
                for w in &p.warnings {
                    lines.push(dl(format!("⚠ {w}"), Tone::Warn));
                }
                for b in &p.hard_blocks {
                    lines.push(dl(format!("✕ {b}"), Tone::Warn));
                }
                for b in p.soft_blocks.iter().take(12) {
                    lines.push(dl(format!("✕ {b}"), Tone::Warn));
                }
                if p.soft_blocks.len() > 12 {
                    lines.push(dl(
                        format!("  and {} more", p.soft_blocks.len() - 12),
                        Tone::Warn,
                    ));
                }
                if !p.can_run() {
                    lines.push(dl("Fix these first; nothing was changed.", Tone::Warn));
                    buttons = vec![btn("Back", '\x1b', "Esc")];
                } else if let GitStep::Typing(_, t) = &d.step {
                    lines.push(dl(
                        format!(
                            "Type \"{}\" to push past the findings: {t}▏",
                            crate::git_scan::OVERRIDE
                        ),
                        Tone::Normal,
                    ));
                    buttons = vec![btn("Push", '\n', "Enter"), btn("Cancel", '\x1b', "Esc")];
                } else {
                    if p.needs_typed {
                        lines.push(dl("Possible secrets found: push only if you are sure (you will type to confirm).", Tone::Warn));
                    }
                    buttons = vec![
                        btn("Yes, do it", '\n', "Enter: your yes"),
                        btn("Cancel", '\x1b', "Esc"),
                    ];
                }
            }
        }
        if let Some(b) = &d.busy {
            lines.push(dl("", Tone::Dim));
            lines.push(dl(b.clone(), Tone::Dim));
        } else if let Some((n, ok)) = &d.note {
            lines.push(dl("", Tone::Dim));
            lines.push(dl(n.clone(), if *ok { Tone::Good } else { Tone::Warn }));
        }
        DlgView {
            title,
            lines,
            buttons,
        }
    }

    // ---- the web sheet ----

    /// A `git` frame from a signed in browser: answered from a thread.
    pub fn web_git(&mut self, client: u64, v: Value) {
        if crate::app_gitcfg::is_cfg_frame(&v) {
            return self.web_gitcfg(client, &v);
        }
        let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) else {
            return;
        };
        let uid = v["tab"].as_u64().or_else(|| {
            v["tab"]
                .as_str()
                .and_then(|t| t.trim_start_matches('t').parse().ok())
        });
        let dir = uid
            .and_then(|u| self.find_tab(u))
            .map(|(s, t)| self.tab_dir(s, t));
        let env = self.git_env();
        let accts = self.git.known_accounts.clone();
        self.git_ask_accounts();
        let out = Arc::clone(&c.out);
        let work = move || git_reply(&out, &v, dir, &env, &accts);
        if cfg!(test) {
            work();
        } else {
            std::thread::spawn(work);
        }
    }

    // ---- the assistant's git tools ----

    /// Run a git tool; None when `tool` is not one of them.
    pub fn git_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        if !GIT_TOOLS.contains(&tool) {
            return None;
        }
        Some(self.git_tool_run(tool, args))
    }

    fn git_tool_dir(&mut self, args: &Value) -> Result<PathBuf, String> {
        let q = args["project"].as_str().unwrap_or("").trim().to_string();
        let dir = if q.is_empty() || q == "current" {
            let s = self.focus;
            let p = self.panes.get(s).ok_or("no tab")?;
            self.tab_dir(s, p.active)
        } else {
            self.git_project_dir(&q)?
        };
        if let Some(why) = crate::guard::never_folder(&dir) {
            return Err(format!("refused: {why}"));
        }
        Ok(crate::project_git::repo_top(&dir).unwrap_or(dir))
    }

    fn git_tool_run(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        let dir = self.git_tool_dir(args)?;
        let env = self.git_env();
        let ctx = crate::git_ops::Ctx::new(&dir);
        let is_repo = crate::project_git::repo_top(&dir).is_some();
        let need_repo = || -> Result<(), String> {
            if is_repo {
                Ok(())
            } else {
                Err(format!("{} is not a git repo", crate::config::tilde(&dir)))
            }
        };
        let ok = |v: Value| Ok(json!({"ok": true, "result": v}));
        match tool {
            "git_log" => {
                need_repo()?;
                let n = args["limit"].as_u64().unwrap_or(20).clamp(1, 100) as usize;
                let rows = crate::git_ops::log(&ctx, n);
                let unp = rows.iter().filter(|r| r.unpushed).count();
                ok(
                    json!({"repo": crate::config::tilde(&dir), "branch": crate::git_ops::branch(&ctx), "commits": rows,
                    "say": format!("{} shown, {unp} not pushed.", crate::control::plural(rows.len(), "commit"))}),
                )
            }
            "git_diff" => {
                need_repo()?;
                let staged = args["staged"].as_bool().unwrap_or(false);
                let file = args["file"].as_str().filter(|f| !f.is_empty());
                if let Some(f) = file {
                    if crate::git_scan::secret_file(f) || f.contains("..") {
                        return Err(format!("refused: {f} holds secrets or is outside the repo"));
                    }
                }
                let ch = crate::git_ops::changes(&ctx);
                let mut text = crate::git_ops::diff_text(&ctx, staged, file, 40_000);
                // Secret files' hunks are dropped, values redacted.
                text = drop_secret_hunks(&text);
                text = crate::git_scan::redact_text(&text);
                ok(
                    json!({"repo": crate::config::tilde(&dir), "staged_only": staged, "changes": ch, "diff": text,
                    "then": "To commit, draft a message with git_draft_commit; the user commits from the Git section (tab options > Git...). You cannot commit."}),
                )
            }
            "git_branches" => {
                need_repo()?;
                ok(
                    json!({"repo": crate::config::tilde(&dir), "branches": crate::git_ops::branches(&ctx)}),
                )
            }
            "git_bindings" => {
                let facts = crate::project_git::locate(&dir).map(|(t, g, c)| {
                    crate::project_git::compute(&t, &g, &c, Duration::from_secs(5))
                });
                let store = crate::git_map::Store::load();
                let r = crate::git_map::resolve(&dir, facts.as_ref(), &env.cfg, &store);
                let w = git_panel::warnings(facts.as_ref());
                ok(
                    json!({"repo": crate::config::tilde(&dir), "binding": r, "warnings": w,
                    "rules": env.cfg.rules, "owners": env.cfg.owners,
                    "placeholder_owners": env.cfg.owners.keys().filter(|o| crate::git_cfg::is_placeholder(o)).collect::<Vec<_>>(),
                    "then": "Only the user can bind, create a repo, commit or push, from the Git section; you may propose with git_propose_binding. Owners and folder rules are the user's to change in Settings > GitHub accounts and repos: suggest changes in words, no tool saves them."}),
                )
            }
            "git_draft_commit" => {
                need_repo()?;
                let m = args["message"].as_str().unwrap_or("").trim().to_string();
                crate::git_ops::commit_args(&m)?;
                git_panel::set_draft(&dir, &m);
                ok(
                    json!({"drafted": true, "say": "Drafted. The user can review and commit it in the Git section (tab options > Git...); nothing was committed."}),
                )
            }
            "git_propose_binding" => {
                let s = |k: &str| args[k].as_str().unwrap_or("").trim().to_string();
                let b = Binding {
                    owner: s("owner"),
                    repo: if s("repo").is_empty() {
                        crate::git_map::repo_name_for(&dir)
                    } else {
                        s("repo")
                    },
                    account: s("account"),
                    source: "assistant".into(),
                    defined_at: String::new(),
                };
                if !crate::git_map::valid_owner(&b.owner)
                    || !crate::git_map::valid_repo(&b.repo)
                    || !crate::git_map::valid_owner(&b.account)
                {
                    return Err("owner, repo and account must be valid GitHub names".into());
                }
                git_panel::set_proposal(&dir, b.clone());
                ok(
                    json!({"proposed": b, "say": "Proposed. The user confirms it in the Git section (Define...); nothing was saved."}),
                )
            }
            _ => Err("unknown git tool".into()),
        }
    }

    /// A project name or path to its folder.
    fn git_project_dir(&mut self, q: &str) -> Result<PathBuf, String> {
        let p = crate::config::expand_tilde(q);
        if p.is_absolute() {
            return p
                .is_dir()
                .then_some(p.clone())
                .ok_or_else(|| format!("{} is not a folder", crate::config::tilde(&p)));
        }
        let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
        crate::project_index::search(&ix, q, 5)
            .into_iter()
            .next()
            .map(|h| h.path)
            .ok_or_else(|| format!("no project named like \"{q}\""))
    }
}

/// Answer one `git` frame to its browser: {t: "git", id, tab, ok, ...}.
pub fn git_reply(
    out: &crate::web::tap::Outbox,
    v: &Value,
    dir: Option<PathBuf>,
    env: &Env,
    accts: &[String],
) {
    let mut r = git_panel::web_frame(v, dir, env, accts);
    r["t"] = json!("git");
    r["id"] = v["id"].clone();
    r["tab"] = v["tab"].clone();
    out.text(r.to_string());
}

fn source_word(r: &crate::git_map::Resolved) -> String {
    match r.source.as_str() {
        "remote" => "from its remote".into(),
        "saved" => "saved binding".into(),
        "rule" => format!("rule {}", r.rule.clone().unwrap_or_default()),
        _ => "undefined".into(),
    }
}

/// Drop the hunks of files whose names say they hold secrets.
fn drop_secret_hunks(diff: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for l in diff.split_inclusive('\n') {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            let b = rest.rsplit(" b/").next().unwrap_or("").trim();
            skip = crate::git_scan::secret_file(b);
            if skip {
                out.push_str(&format!("[{b}: not shown, it holds secrets]\n"));
            }
        }
        if !skip {
            out.push_str(l);
        }
    }
    out
}

fn project_index_stale(app: &mut App, dir: &Path) {
    app.git_tab_finished(dir);
}

/// The assistant's git tools: read, or prepare for the user. None of them
/// changes a repo or a binding.
pub const GIT_TOOLS: &[&str] = &[
    "git_log",
    "git_diff",
    "git_branches",
    "git_bindings",
    "git_draft_commit",
    "git_propose_binding",
];

/// Names a model might try that do not exist on purpose: changes happen
/// only in the Git section (tested).
#[cfg_attr(not(test), allow(dead_code))]
pub const NEVER_TOOLS: &[&str] = &[
    "git_commit",
    "git_push",
    "git_pull",
    "git_sync",
    "git_fetch",
    "git_branch",
    "git_switch",
    "git_bind",
    "git_create_repo",
    "git_auto_sync",
    "git_push_as_branch",
    "git_init",
    "git_untrack",
];

#[cfg(test)]
#[path = "gitpanel_tests.rs"]
mod tests;
