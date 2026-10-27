use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{App, Modal};
use crate::config::Config;
use crate::git_cfg::{self, Acct, BindEdit, Draft, OwnerRow};
use crate::git_map::{Binding, GitRule, Store};
use crate::hits::UiAction;
use crate::tab_browser::{DlgLine, DlgView, Tone};

#[derive(Default)]
pub struct CfgUi {
    slot: Arc<Mutex<Option<Vec<Acct>>>>,

    pub accounts: Option<Vec<Acct>>,
    asked: Option<Instant>,
    pending: HashMap<String, (Draft, Instant)>,

    relisted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub label: &'static str,
    pub text: String,
    pub options: Vec<String>,

    pub fixed: bool,
}

fn field(label: &'static str, text: &str, options: Vec<String>) -> Field {
    Field {
        label,
        text: text.to_string(),
        options,
        fixed: false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Owner(Option<usize>),
    Rule(Option<usize>),
    Bind(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CfgStep {
    Main,
    Edit {
        target: Target,
        fields: Vec<Field>,
        at: usize,
    },

    Confirm {
        lines: Vec<String>,
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitCfgDlg {
    pub draft: Draft,

    pub saved: Draft,
    pub sel: usize,
    pub step: CfgStep,
    pub note: Option<(String, bool)>,

    pub starter: bool,

    pub warned: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Owner(usize),
    AddOwner,
    Rule(usize),
    AddRule,
    Bound(String),
    Define(String),
}

fn dl(text: impl Into<String>, tone: Tone) -> DlgLine {
    DlgLine {
        text: text.into(),
        tone,
        action: None,
    }
}

fn rand_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    format!("c{:016x}", h.finish())
}

fn rule_text(r: &GitRule) -> String {
    let who = if r.owner.eq_ignore_ascii_case("choose") {
        format!("choose {}", r.choices.join(" or "))
    } else {
        r.owner.clone()
    };
    let st = if r.state.as_deref() == Some("exclude") {
        " · conversations and .env kept out of the state repo"
    } else {
        ""
    };
    format!("{} -> {who}{st}", r.pattern)
}

impl App {
    pub fn gitcfg_ask_accounts(&mut self) {
        let ui = &mut self.git.cfg_ui;
        if cfg!(test)
            || ui
                .asked
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        ui.asked = Some(Instant::now());
        let gh = crate::git_cred::gh_bin(Some(&self.cfg.git));
        let slot = Arc::clone(&ui.slot);
        std::thread::spawn(move || {
            let a = git_cfg::accounts(&gh);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(a);
        });
    }

    pub fn gitcfg_tick(&mut self) {
        let got = self
            .git
            .cfg_ui
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(a) = got {
            let was = git_cfg::suggested(self.gitcfg_accounts());
            self.git.cfg_ui.accounts = Some(a);

            let now = git_cfg::suggested(self.gitcfg_accounts());
            if let Modal::GitCfg(d) = &mut self.modal {
                if d.starter && d.draft == was {
                    d.draft = now;
                }
            }
        }
        self.git
            .cfg_ui
            .pending
            .retain(|_, (_, t)| t.elapsed() < Duration::from_secs(600));
    }

    fn gitcfg_accounts(&self) -> Option<&[Acct]> {
        self.git.cfg_ui.accounts.as_deref()
    }

    pub fn gitcfg_projects(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = vec![];
        for (s, p) in self.panes.iter().enumerate() {
            for t in 0..p.tabs.len() {
                let d = self.tab_dir(s, t);
                v.push(crate::project_git::repo_top(&d).unwrap_or(d));
            }
        }
        {
            let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
            v.extend(ix.git.keys().cloned());
            for r in ix.roots.values() {
                for e in r.entries.iter().filter(|e| e.git) {
                    v.push(if e.rel.is_empty() {
                        r.root.clone()
                    } else {
                        r.root.join(&e.rel)
                    });
                }
            }
        }
        for k in Store::load().projects.keys() {
            v.push(crate::config::expand_tilde(k));
        }
        let home = crate::config::home_dir();
        let tmp = std::env::temp_dir();
        let mut out: Vec<PathBuf> = vec![];
        let mut keys: Vec<String> = vec![];
        for p in v {
            if crate::guard::never_folder(&p).is_some()
                || crate::guard::same(&p, &home)
                || (!cfg!(test) && p.starts_with(&tmp))
            {
                continue;
            }
            let k = crate::git_map::key(&p);
            if !keys.contains(&k) {
                keys.push(k);
                out.push(p);
            }
        }
        out.sort();
        out.truncate(400);
        out
    }

    fn gitcfg_prepare(&mut self, d: &Draft) -> (git_cfg::Check, Vec<String>, Option<String>) {
        let c = git_cfg::check(d, self.gitcfg_accounts());
        let saved = Draft::from_cfg(&self.cfg.git);
        let lines = git_cfg::diff(&saved, d, &Store::load());
        let id = (c.ok() && !lines.is_empty()).then(|| {
            let id = rand_id();
            let p = &mut self.git.cfg_ui.pending;
            if p.len() > 20 {
                p.clear();
            }
            p.insert(id.clone(), (d.clone(), Instant::now()));
            id
        });
        (c, lines, id)
    }

    fn gitcfg_commit(&mut self, id: &str) -> Result<String, String> {
        let Some((d, _)) = self.git.cfg_ui.pending.remove(id) else {
            return Err("that confirmation expired; press Save again".into());
        };
        let said = git_cfg::save(&Config::path(), &d, self.gitcfg_accounts())?;
        self.reload_config_now();
        crate::log::info(&format!("git settings saved by the user: {said}"));
        Ok(said)
    }

    pub fn gitcfg_view(&self) -> Value {
        let cur = Draft::from_cfg(&self.cfg.git);
        let store = Store::load();
        let projects = self.gitcfg_projects();
        let suggested = git_cfg::suggested(self.gitcfg_accounts());
        let shown = if cur.is_empty() {
            suggested.clone()
        } else {
            cur.clone()
        };
        let bindings: Vec<Value> = store
            .projects
            .iter()
            .map(|(k, b)| json!({"key": k, "owner": b.owner, "repo": b.repo, "account": b.account, "source": b.source, "defined_at": b.defined_at}))
            .collect();
        json!({
            "draft": cur,
            "empty": cur.is_empty(),
            "placeholders": suggested,
            "placeholder_owners": git_cfg::placeholder_owners(),
            "accounts": self.gitcfg_accounts(),
            "projects": projects.iter().map(|p| crate::git_map::key(p)).collect::<Vec<_>>(),
            "bindings": bindings,
            "preview": git_cfg::preview(&shown, &self.cfg.git, &projects, &store),
            "check": git_cfg::check(&shown, self.gitcfg_accounts()),
            "config": crate::config::tilde(&Config::path()),
        })
    }

    pub fn gitcfg_frame(&mut self, v: &Value) -> Value {
        let mut r = match v["op"].as_str().unwrap_or("") {
            "cfg_view" => {
                self.gitcfg_ask_accounts();
                json!({"ok": true, "view": self.gitcfg_view()})
            }
            "cfg_check" => {
                match serde_json::from_value::<Draft>(v["draft"].clone()) {
                    Ok(d) => {
                        let (c, lines, id) = self.gitcfg_prepare(&d);
                        let projects = self.gitcfg_projects();
                        let pv = git_cfg::preview(&d, &self.cfg.git, &projects, &Store::load());

                        let test = v["test"]
                        .as_str()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(|t| {
                            let p = crate::config::expand_tilde(t);
                            match crate::guard::never_folder(&p) {
                                Some(why) => json!({"line": format!("{t}: refused, {why}"), "refused": true}),
                                None => json!(git_cfg::preview_one(&d.to_cfg(&self.cfg.git), &p)),
                            }
                        });
                        json!({"ok": true, "check": c, "diff": lines, "plan_id": id, "preview": pv, "test": test})
                    }
                    Err(e) => json!({"ok": false, "error": format!("not a draft: {e}")}),
                }
            }
            "cfg_save" => {
                let id = v["plan_id"].as_str().unwrap_or("");
                match self.gitcfg_commit(id) {
                    Ok(said) => json!({"ok": true, "said": said, "view": self.gitcfg_view()}),
                    Err(e) => json!({"ok": false, "error": e}),
                }
            }

            "cfg_gh_login" => {
                let gh = crate::git_cred::gh_bin(Some(&self.cfg.git));
                json!({"ok": true, "login": crate::gh_login::start(&gh)})
            }
            "cfg_gh_login_poll" => {
                let l = crate::gh_login::poll();
                if l.state == "done" && !self.git.cfg_ui.relisted {
                    self.git.cfg_ui.relisted = true;
                    self.git.cfg_ui.asked = None;
                    self.gitcfg_ask_accounts();
                    crate::log::info(
                        "gh login for a new account finished (from the web Git screen)",
                    );
                } else if l.running() {
                    self.git.cfg_ui.relisted = false;
                }
                json!({"ok": true, "login": l})
            }
            "cfg_gh_login_cancel" => json!({"ok": true, "login": crate::gh_login::cancel()}),
            other => json!({"ok": false, "error": format!("unknown git op {other:?}")}),
        };
        r["t"] = json!("git");
        r["id"] = v["id"].clone();
        r
    }

    pub fn web_gitcfg(&mut self, client: u64, v: &Value) {
        let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) else {
            return;
        };
        let r = self.gitcfg_frame(v);
        c.out.text(r.to_string());
    }

    pub fn open_gitcfg(&mut self) {
        let saved = Draft::from_cfg(&self.cfg.git);
        let starter = saved.is_empty();
        self.modal = Modal::GitCfg(GitCfgDlg {
            draft: if starter {
                git_cfg::suggested(self.gitcfg_accounts())
            } else {
                saved.clone()
            },
            saved,
            sel: 0,
            step: CfgStep::Main,
            note: None,
            starter,
            warned: false,
        });
        self.gitcfg_ask_accounts();
    }

    fn gitcfg_dlg(&self) -> Option<GitCfgDlg> {
        match &self.modal {
            Modal::GitCfg(d) => Some(d.clone()),
            _ => None,
        }
    }

    fn set_gitcfg(&mut self, d: GitCfgDlg) {
        if matches!(self.modal, Modal::GitCfg(_)) {
            self.modal = Modal::GitCfg(d);
        }
    }

    pub fn gitcfg_items(&self, d: &GitCfgDlg) -> Vec<Item> {
        let mut v: Vec<Item> = (0..d.draft.owners.len()).map(Item::Owner).collect();
        v.push(Item::AddOwner);
        v.extend((0..d.draft.rules.len()).map(Item::Rule));
        v.push(Item::AddRule);
        let store = git_cfg::with_binds(&Store::load(), &d.draft);
        v.extend(store.projects.keys().cloned().map(Item::Bound));
        for r in git_cfg::preview(&d.draft, &self.cfg.git, &self.gitcfg_projects(), &store) {
            if r.bound.is_none() && (r.rule.is_none() || !r.choices.is_empty()) {
                v.push(Item::Define(r.key));
            }
        }
        v
    }

    fn acct_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .gitcfg_accounts()
            .map(|a| a.iter().map(|x| x.name.clone()).collect())
            .unwrap_or_default();
        for k in &self.git.known_accounts {
            if !v.contains(k) {
                v.push(k.clone());
            }
        }
        v
    }

    fn owner_names(d: &Draft) -> Vec<String> {
        d.owners.iter().map(|o| o.owner.clone()).collect()
    }

    fn gitcfg_form(&self, d: &GitCfgDlg, target: Target) -> CfgStep {
        let accts = self.acct_names();
        let owners = Self::owner_names(&d.draft);
        let fields = match &target {
            Target::Owner(i) => {
                let r = i
                    .and_then(|i| d.draft.owners.get(i).cloned())
                    .unwrap_or_default();
                vec![
                    field("GitHub owner (user or org)", &r.owner, vec![]),
                    field("gh account that pushes for it", &r.account, accts),
                ]
            }
            Target::Rule(i) => {
                let r = i
                    .and_then(|i| d.draft.rules.get(i).cloned())
                    .unwrap_or_default();
                let folders: Vec<String> = self
                    .gitcfg_projects()
                    .iter()
                    .map(|p| format!("{}/**", crate::git_map::key(p)))
                    .collect();
                let mut who = owners.clone();
                who.push("choose".into());
                vec![
                    field(
                        "Folder glob (Left / Right: known projects)",
                        &r.pattern,
                        folders,
                    ),
                    field("Owner (or choose: per project)", &r.owner, who),
                    field(
                        "Choices when choose, comma separated",
                        &r.choices.join(", "),
                        vec![],
                    ),
                    field(
                        "Keep conversations and .env out of the state repo",
                        if r.state.as_deref() == Some("exclude") {
                            "yes"
                        } else {
                            "no"
                        },
                        vec!["no".into(), "yes".into()],
                    ),
                ]
            }
            Target::Bind(k) => {
                let store = git_cfg::with_binds(&Store::load(), &d.draft);
                let p = crate::config::expand_tilde(k);
                let pv = git_cfg::preview_one(&d.draft.to_cfg(&self.cfg.git), &p);
                let b = store.get(&p).cloned().unwrap_or_else(|| Binding {
                    owner: pv
                        .owner
                        .clone()
                        .or_else(|| pv.choices.first().cloned())
                        .unwrap_or_default(),
                    repo: crate::git_map::repo_name_for(&p),
                    account: pv.account.clone().unwrap_or_default(),
                    ..Default::default()
                });
                let mut opts = owners.clone();
                for c in pv.choices {
                    if !opts.contains(&c) {
                        opts.insert(0, c);
                    }
                }
                vec![
                    Field {
                        fixed: true,
                        ..field("Folder", k, vec![])
                    },
                    field("GitHub owner", &b.owner, opts),
                    field("Repo name", &b.repo, vec![]),
                    field("gh account", &b.account, accts),
                ]
            }
        };
        let at = fields.iter().position(|f| !f.fixed).unwrap_or(0);
        CfgStep::Edit { target, fields, at }
    }

    fn gitcfg_apply_form(d: &mut GitCfgDlg, target: &Target, f: &[Field]) {
        let t = |i: usize| {
            f.get(i)
                .map(|x| x.text.trim().to_string())
                .unwrap_or_default()
        };
        match target {
            Target::Owner(i) => {
                let row = OwnerRow {
                    owner: t(0),
                    account: t(1),
                };
                match i.and_then(|i| d.draft.owners.get_mut(i)) {
                    Some(r) => *r = row,
                    None => d.draft.owners.push(row),
                }
            }
            Target::Rule(i) => {
                let owner = t(1);
                let choose = owner.eq_ignore_ascii_case("choose");
                let rule = GitRule {
                    pattern: t(0),
                    owner: if choose { "choose".into() } else { owner },
                    choices: if choose {
                        t(2).split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    } else {
                        vec![]
                    },
                    state: (t(3) == "yes").then(|| "exclude".to_string()),
                };
                match i.and_then(|i| d.draft.rules.get_mut(i)) {
                    Some(r) => *r = rule,
                    None => d.draft.rules.push(rule),
                }
            }
            Target::Bind(k) => {
                d.draft.binds.retain(|b| &b.key != k);
                d.draft.binds.push(BindEdit {
                    key: k.clone(),
                    binding: Some(Binding {
                        owner: t(1),
                        repo: t(2),
                        account: t(3),
                        source: "manual".into(),
                        defined_at: String::new(),
                    }),
                });
            }
        }
    }

    fn gitcfg_save_step(&mut self, mut d: GitCfgDlg) {
        let (c, mut lines, id) = self.gitcfg_prepare(&d.draft);
        match id {
            Some(id) => {
                lines.extend(c.warnings.iter().map(|w| format!("⚠ {w}")));
                d.step = CfgStep::Confirm { lines, id };
                d.note = None;
            }
            None if !c.ok() => {
                d.note = Some((format!("Not saved: {}", c.errors.join("; ")), false))
            }
            None => d.note = Some(("Nothing to save: no changes".into(), true)),
        }
        self.set_gitcfg(d);
    }

    pub fn gitcfg_click_row(&mut self, i: usize) {
        let Some(mut d) = self.gitcfg_dlg() else {
            return;
        };
        if d.step != CfgStep::Main {
            return;
        }
        d.sel = i;
        self.set_gitcfg(d);
        self.on_gitcfg_key(KeyEvent::from(KeyCode::Enter));
    }

    pub fn on_gitcfg_key(&mut self, k: KeyEvent) {
        let Some(mut d) = self.gitcfg_dlg() else {
            return;
        };
        match d.step.clone() {
            CfgStep::Main => self.gitcfg_main_key(d, k),
            CfgStep::Edit {
                target,
                mut fields,
                mut at,
            } => {
                let n = fields.len();
                let movable = |i: usize, f: &[Field]| !f[i].fixed;
                match k.code {
                    KeyCode::Esc => d.step = CfgStep::Main,
                    KeyCode::Enter => {
                        Self::gitcfg_apply_form(&mut d, &target, &fields);
                        d.step = CfgStep::Main;
                        d.warned = false;
                        let c = git_cfg::check(&d.draft, self.gitcfg_accounts());
                        d.note = c.errors.first().map(|e| (e.clone(), false));
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        let mut j = (at + 1) % n;
                        while !movable(j, &fields) && j != at {
                            j = (j + 1) % n;
                        }
                        at = j;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        let mut j = (at + n - 1) % n;
                        while !movable(j, &fields) && j != at {
                            j = (j + n - 1) % n;
                        }
                        at = j;
                    }
                    KeyCode::Left | KeyCode::Right => {
                        let f = &mut fields[at];
                        if !f.options.is_empty() {
                            let cur = f.options.iter().position(|o| o == &f.text);
                            let m = f.options.len();
                            let next = match (cur, k.code == KeyCode::Right) {
                                (None, _) => 0,
                                (Some(c), true) => (c + 1) % m,
                                (Some(c), false) => (c + m - 1) % m,
                            };
                            f.text = f.options[next].clone();
                        }
                    }
                    KeyCode::Backspace => {
                        fields[at].text.pop();
                    }
                    KeyCode::Char(c)
                        if !k.modifiers.contains(KeyModifiers::CONTROL)
                            && fields[at].text.chars().count() < 200 =>
                    {
                        fields[at].text.push(c);
                    }
                    _ => {}
                }
                if matches!(d.step, CfgStep::Edit { .. }) {
                    d.step = CfgStep::Edit { target, fields, at };
                }
                self.set_gitcfg(d);
            }
            CfgStep::Confirm { id, .. } => match k.code {
                KeyCode::Enter | KeyCode::Char('y') => match self.gitcfg_commit(&id) {
                    Ok(said) => {
                        let saved = Draft::from_cfg(&self.cfg.git);
                        d.draft = saved.clone();
                        d.saved = saved;
                        d.starter = false;
                        d.step = CfgStep::Main;
                        d.note = Some((said, true));
                        self.set_gitcfg(d);
                    }
                    Err(e) => {
                        d.step = CfgStep::Main;
                        d.note = Some((format!("Not saved: {e}"), false));
                        self.set_gitcfg(d);
                    }
                },
                KeyCode::Esc | KeyCode::Char('n') => {
                    d.step = CfgStep::Main;
                    d.note = Some(("Not saved".into(), false));
                    self.set_gitcfg(d);
                }
                _ => {}
            },
        }
    }

    fn gitcfg_main_key(&mut self, mut d: GitCfgDlg, k: KeyEvent) {
        let items = self.gitcfg_items(&d);
        let n = items.len();
        d.sel = d.sel.min(n.saturating_sub(1));
        let item = items.get(d.sel).cloned();
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let dirty = d.draft != d.saved;
        if k.code != KeyCode::Esc {
            d.warned = false;
        }
        match k.code {
            KeyCode::Esc if dirty && !d.warned && !d.starter => {
                d.warned = true;
                d.note = Some((
                    "Unsaved changes: Esc again to drop them, s to save".into(),
                    false,
                ));
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = Modal::None;
                return;
            }
            KeyCode::Up if shift => return self.gitcfg_move(d, item, -1),
            KeyCode::Down if shift => return self.gitcfg_move(d, item, 1),
            KeyCode::Char('K') => return self.gitcfg_move(d, item, -1),
            KeyCode::Char('J') => return self.gitcfg_move(d, item, 1),
            KeyCode::Up | KeyCode::Char('k') => d.sel = d.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => d.sel = (d.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char(' ') => {
                let t = match item {
                    Some(Item::Owner(i)) => Target::Owner(Some(i)),
                    Some(Item::AddOwner) => Target::Owner(None),
                    Some(Item::Rule(i)) => Target::Rule(Some(i)),
                    Some(Item::AddRule) => Target::Rule(None),
                    Some(Item::Bound(k)) | Some(Item::Define(k)) => Target::Bind(k),
                    None => return,
                };
                d.step = self.gitcfg_form(&d, t);
            }
            KeyCode::Char('a') => {
                let t = match item {
                    Some(Item::Owner(_)) | Some(Item::AddOwner) => Target::Owner(None),
                    _ => Target::Rule(None),
                };
                d.step = self.gitcfg_form(&d, t);
            }
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => match item {
                Some(Item::Owner(i)) => {
                    d.draft.owners.remove(i);
                }
                Some(Item::Rule(i)) => {
                    d.draft.rules.remove(i);
                }
                Some(Item::Bound(k)) => {
                    d.draft.binds.retain(|b| b.key != k);
                    if Store::load().projects.contains_key(&k) {
                        d.draft.binds.push(BindEdit {
                            key: k,
                            binding: None,
                        });
                    }
                }
                _ => {}
            },
            KeyCode::Char('x') => {
                if let Some(Item::Rule(i)) = item {
                    let r = &mut d.draft.rules[i];
                    r.state = match r.state {
                        Some(_) => None,
                        None => Some("exclude".into()),
                    };
                }
            }
            KeyCode::Char('u') if d.starter => {
                d.draft = git_cfg::suggested(self.gitcfg_accounts());
                return self.gitcfg_save_step(d);
            }
            KeyCode::Char('s') => return self.gitcfg_save_step(d),
            KeyCode::Char('r') => {
                self.git.cfg_ui.asked = None;
                self.gitcfg_ask_accounts();
                d.note = Some(("Asking gh for the accounts again...".into(), true));
            }
            _ => {}
        }
        let n = self.gitcfg_items(&d).len();
        d.sel = d.sel.min(n.saturating_sub(1));
        self.set_gitcfg(d);
    }

    fn gitcfg_move(&mut self, mut d: GitCfgDlg, item: Option<Item>, by: i64) {
        if let Some(Item::Rule(i)) = item {
            let j = i as i64 + by;
            if j >= 0 && (j as usize) < d.draft.rules.len() {
                d.draft.rules.swap(i, j as usize);
                d.sel = (d.sel as i64 + by) as usize;
            }
        }
        self.set_gitcfg(d);
    }

    pub fn gitcfg_dialog_view(&self, d: &GitCfgDlg) -> DlgView {
        let btn =
            |l: &str, c: char, h: &str| (l.to_string(), UiAction::GitCfgKey(c), h.to_string());
        let title = if d.starter {
            "GitHub accounts and repos · placeholders, not saved yet".to_string()
        } else {
            "GitHub accounts and repos".to_string()
        };
        let mut lines: Vec<DlgLine> = vec![];
        match &d.step {
            CfgStep::Edit { target, fields, at } => {
                lines.push(dl(
                    match target {
                        Target::Owner(None) => "New owner",
                        Target::Owner(Some(_)) => "Edit owner",
                        Target::Rule(None) => "New folder rule",
                        Target::Rule(Some(_)) => "Edit folder rule",
                        Target::Bind(_) => "Project binding",
                    },
                    Tone::Normal,
                ));
                for (i, f) in fields.iter().enumerate() {
                    let mark = if i == *at { "›" } else { " " };
                    let cursor = if i == *at { "▏" } else { "" };
                    let opts = if f.options.is_empty() || i != *at {
                        String::new()
                    } else {
                        format!("   ‹ › {} choices", f.options.len())
                    };
                    lines.push(dl(format!("{mark} {}", f.label), Tone::Dim));
                    lines.push(dl(
                        format!("    {}{cursor}{opts}", f.text),
                        if i == *at { Tone::Normal } else { Tone::Dim },
                    ));
                }
                if let Some(f) = fields.get(*at) {
                    if f.label.starts_with("gh account") {
                        if let Some(a) = self.gitcfg_accounts() {
                            let s: Vec<String> = a
                                .iter()
                                .map(|x| {
                                    format!(
                                        "{} {}",
                                        x.name,
                                        if x.logged_in {
                                            "✓"
                                        } else {
                                            "✗ not logged in"
                                        }
                                    )
                                })
                                .collect();
                            lines.push(dl(format!("gh: {}", s.join(", ")), Tone::Dim));
                        }
                    }
                }
                lines.push(dl(
                    "Type to change · Tab next field · Left / Right pick · Enter done · Esc cancel",
                    Tone::Dim,
                ));
                return DlgView {
                    title,
                    lines,
                    buttons: vec![
                        btn("Done", '\n', "Enter: put it in the list (Save writes it)"),
                        btn("Cancel", '\x1b', "Esc"),
                    ],
                };
            }
            CfgStep::Confirm { lines: diff, .. } => {
                lines.push(dl(
                    format!(
                        "Save these changes to {} and the bindings?",
                        crate::config::tilde(&Config::path())
                    ),
                    Tone::Normal,
                ));
                for l in diff {
                    let tone = if l.starts_with('⚠') {
                        Tone::Warn
                    } else if l.starts_with('+') {
                        Tone::Good
                    } else if l.starts_with('-') || l.starts_with('~') {
                        Tone::Warn
                    } else {
                        Tone::Dim
                    };
                    lines.push(dl(format!("  {l}"), tone));
                }
                lines.push(dl("Enter saves, Esc goes back.", Tone::Dim));
                return DlgView {
                    title,
                    lines,
                    buttons: vec![
                        btn("Save", '\n', "Enter: write config.toml and the bindings"),
                        btn("Back", '\x1b', "Esc: nothing is saved"),
                    ],
                };
            }
            CfgStep::Main => {}
        }
        let items = self.gitcfg_items(d);
        let sel = d.sel.min(items.len().saturating_sub(1));
        let row = |lines: &mut Vec<DlgLine>, it: &Item, text: String, tone: Tone| {
            let i = items.iter().position(|x| x == it).unwrap_or(usize::MAX);
            let mark = if i == sel { "›" } else { " " };
            lines.push(DlgLine {
                text: format!("{mark} {text}"),
                tone,
                action: Some(UiAction::GitCfgRow(i)),
            });
        };
        if d.starter {
            lines.push(dl(
                "[git] in config.toml is empty. These are suggested placeholders: edit them, then Use these (u) or Save (s).",
                Tone::Warn,
            ));
        }
        let accts = self.gitcfg_accounts();
        match accts {
            None => lines.push(dl("gh accounts: asking gh...", Tone::Dim)),
            Some([]) => lines.push(dl(
                "gh accounts: none logged in (gh auth login)",
                Tone::Warn,
            )),
            Some(a) => lines.push(dl(
                format!(
                    "gh accounts: {}",
                    a.iter()
                        .map(|x| format!("{} {}", x.name, if x.logged_in { "✓" } else { "✗" }))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Tone::Dim,
            )),
        }
        lines.push(dl(
            "Owners: GitHub owner -> gh account that pushes",
            Tone::Normal,
        ));
        for (i, o) in d.draft.owners.iter().enumerate() {
            let status = if git_cfg::is_placeholder(&o.owner) {
                ("  placeholder: set the real name".to_string(), Tone::Warn)
            } else {
                match accts.map(|a| a.iter().any(|x| x.name == o.account && x.logged_in)) {
                    Some(true) => ("  ✓ logged in".into(), Tone::Normal),
                    Some(false) => ("  ⚠ not logged in".into(), Tone::Warn),
                    None => (String::new(), Tone::Normal),
                }
            };
            row(
                &mut lines,
                &Item::Owner(i),
                format!("{} -> {}{}", o.owner, o.account, status.0),
                status.1,
            );
        }
        row(&mut lines, &Item::AddOwner, "+ Add owner".into(), Tone::Dim);
        lines.push(dl(
            "Folder rules, in order: the most specific glob wins, ties go to the higher one",
            Tone::Normal,
        ));
        for (i, r) in d.draft.rules.iter().enumerate() {
            row(
                &mut lines,
                &Item::Rule(i),
                format!("{}. {}", i + 1, rule_text(r)),
                Tone::Normal,
            );
        }
        row(&mut lines, &Item::AddRule, "+ Add rule".into(), Tone::Dim);

        let store = git_cfg::with_binds(&Store::load(), &d.draft);
        let pv = git_cfg::preview(&d.draft, &self.cfg.git, &self.gitcfg_projects(), &store);
        lines.push(dl(
            format!("Known projects by rule ({})", pv.len()),
            Tone::Normal,
        ));
        let mut groups: Vec<(Option<usize>, Vec<&git_cfg::PreviewRow>)> = vec![];
        for r in &pv {
            match groups.iter_mut().find(|g| g.0 == r.rule) {
                Some(g) => g.1.push(r),
                None => groups.push((r.rule, vec![r])),
            }
        }
        groups.sort_by_key(|g| g.0.map(|i| i as i64).unwrap_or(i64::MAX));
        for (rule, rows) in groups {
            let head = match rule.and_then(|i| d.draft.rules.get(i)) {
                Some(r) => format!("  {}. {}", rule.unwrap_or(0) + 1, r.pattern),
                None => "  Undefined: no rule covers these".into(),
            };
            let tone = if rule.is_none() {
                Tone::Warn
            } else {
                Tone::Dim
            };
            lines.push(dl(head, tone));
            for r in rows.iter().take(6) {
                lines.push(dl(format!("      {}", r.line), tone));
            }
            if rows.len() > 6 {
                lines.push(dl(format!("      and {} more", rows.len() - 6), Tone::Dim));
            }
        }
        lines.push(dl("Per project bindings (saved)", Tone::Normal));
        if store.projects.is_empty() {
            lines.push(dl("  none yet", Tone::Dim));
        }
        for (k, b) in &store.projects {
            row(
                &mut lines,
                &Item::Bound(k.clone()),
                format!("{k} -> {}/{} as {}", b.owner, b.repo, b.account),
                Tone::Normal,
            );
        }
        for it in items.iter().filter(|i| matches!(i, Item::Define(_))) {
            let Item::Define(k) = it else { continue };
            let why = pv
                .iter()
                .find(|r| &r.key == k)
                .map(|r| {
                    if r.rule.is_none() {
                        "undefined".to_string()
                    } else {
                        format!("choose {}", r.choices.join(" or "))
                    }
                })
                .unwrap_or_default();
            row(&mut lines, it, format!("Define {k}  ({why})"), Tone::Warn);
        }
        let c = git_cfg::check(&d.draft, accts);
        for e in &c.errors {
            lines.push(dl(format!("✗ {e}"), Tone::Warn));
        }
        for w in c.warnings.iter().take(6) {
            lines.push(dl(format!("⚠ {w}"), Tone::Dim));
        }
        if let Some((t, good)) = &d.note {
            lines.push(dl(t.clone(), if *good { Tone::Good } else { Tone::Warn }));
        }
        lines.push(dl(
            "Enter edit · a add · d remove or unbind · Shift-Up/Down (K/J) reorder · x state out · s save · Esc close",
            Tone::Dim,
        ));
        let mut buttons = vec![];
        if d.starter {
            buttons.push(btn("Use these", 'u', "Save the placeholders (asks first)"));
        }
        buttons.extend([
            btn("Save", 's', "Show what changes, then save on your yes (s)"),
            btn("Add", 'a', "Add an owner or a rule (a)"),
            btn("Close", '\x1b', "Esc"),
        ]);
        DlgView {
            title,
            lines,
            buttons,
        }
    }

    pub fn gitcfg_summary(&self) -> Vec<(String, Tone)> {
        let cur = Draft::from_cfg(&self.cfg.git);
        let mut v = vec![];
        if cur.is_empty() {
            v.push((
                "No owners or folder rules yet. Enter opens the editor with suggested placeholders to start from.".to_string(),
                Tone::Warn,
            ));
        }
        v.push((format!("Owners ({})", cur.owners.len()), Tone::Normal));
        for o in &cur.owners {
            let ph = if git_cfg::is_placeholder(&o.owner) {
                "   placeholder"
            } else {
                ""
            };
            v.push((format!("  {} -> {}{ph}", o.owner, o.account), Tone::Dim));
        }
        v.push((format!("Folder rules ({})", cur.rules.len()), Tone::Normal));
        for (i, r) in cur.rules.iter().enumerate() {
            v.push((format!("  {}. {}", i + 1, rule_text(r)), Tone::Dim));
        }
        let pv = git_cfg::preview(&cur, &self.cfg.git, &self.gitcfg_projects(), &Store::load());
        let undefined = pv
            .iter()
            .filter(|r| r.rule.is_none() && r.bound.is_none())
            .count();
        v.push((
            format!(
                "Known projects: {} ({} undefined), saved bindings: {}",
                pv.len(),
                undefined,
                Store::load().projects.len()
            ),
            if undefined > 0 {
                Tone::Warn
            } else {
                Tone::Normal
            },
        ));
        v.push((
            "Enter: edit owners, folder rules and bindings. Saving asks first and keeps config.toml's comments.".into(),
            Tone::Dim,
        ));
        v
    }
}

pub fn is_cfg_frame(v: &Value) -> bool {
    v["op"].as_str().is_some_and(|o| o.starts_with("cfg_"))
}

#[cfg(test)]
#[path = "gitcfg_tests.rs"]
mod tests;
