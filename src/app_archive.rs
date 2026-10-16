//! Archiving tabs in the app: stop a tab's session and what it owns,
//! close its pane tab and keep it in the Archived group (the tab list's
//! last rows) to come back with a double click; restoring resumes the
//! very session in its folder on its account. The assistant has the same
//! through archive_tabs, list_archived and restore_archived.

use serde_json::{json, Value};

use crate::app::{App, Modal};
use crate::archive::{unsent_input, Archived};
use crate::control::{plural, tab_id};
use crate::pane::Activity;

/// Most restored by one call (each starts a claude process).
pub const RESTORE_MAX: usize = 10;

/// Why a tab cannot be archived right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// A hard reason `force` does not override.
    pub hard: bool,
    pub why: String,
}

impl App {
    /// Unix seconds now (a test may fix it).
    pub fn archive_now(&self) -> i64 {
        self.archive_clock
            .unwrap_or_else(|| chrono::Utc::now().timestamp())
    }

    /// When a tab last did anything, Unix seconds: its conversation file's
    /// last write when it has one (so a tab resumed at startup counts from
    /// its real last turn), else the last output, input or state change.
    pub fn tab_last_activity(&self, s: usize, t: usize) -> i64 {
        let now = self.archive_now();
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return now;
        };
        if tab.is_running() && matches!(tab.activity, Activity::Working | Activity::Permission) {
            return now;
        }
        let from_file = self
            .transcript_path(s, t)
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        // (The real clock: only `now` can be fixed by a test.)
        let live = chrono::Utc::now().timestamp()
            - crate::clock::age(tab.last_activity()).as_secs() as i64;
        match from_file {
            // (A prompt sent just now is in the file already.)
            Some(f) => f,
            None => live,
        }
    }

    /// What stops tab (s, t) from being archived now.
    pub fn archive_blockers(&self, s: usize, t: usize) -> Vec<Block> {
        let mut v = vec![];
        let Some(tab) = self.panes.get(s).and_then(|p| p.tabs.get(t)) else {
            return vec![Block {
                hard: true,
                why: "no such tab".into(),
            }];
        };
        let hard = |why: &str| Block {
            hard: true,
            why: why.into(),
        };
        let soft = |why: String| Block { hard: false, why };
        let uid = tab.uid;
        if self.panes[s].account.is_none() {
            v.push(hard("the pane has no account"));
        }
        if !tab.is_running() && tab.session_id.is_none() {
            v.push(hard("it never ran, so there is nothing to resume"));
        }
        if self.direct_with(uid) {
            v.push(hard("you are talking to it directly right now"));
        }
        if self.moving_away(uid) {
            v.push(hard("it is being moved to another account"));
        }
        if tab.is_running() {
            match tab.activity {
                Activity::Working => v.push(soft("it is working".into())),
                Activity::Permission => v.push(soft("it is waiting for your approval".into())),
                _ => {}
            }
            if unsent_input(&tab.screen_text()).is_some() {
                v.push(soft("it has unsent text in its input box".into()));
            }
            let q = self.queued_for(uid);
            if q > 0 {
                v.push(soft(format!("{} on the way to it", plural(q, "prompt"))));
            }
            if let Some(bg) = crate::ui_chrome::background_of(self, s, t) {
                v.push(soft(format!("it has {bg}")));
            }
        }
        v
    }

    fn block_text(b: &[Block]) -> String {
        b.iter()
            .map(|x| x.why.clone())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Archive tab (s, t) now: stop its session and what it owns, record
    /// it, close the pane tab. Refused for hard reasons, and for soft ones
    /// (busy, unsent text...) unless `force`. Returns the entry.
    pub fn archive_tab(
        &mut self,
        s: usize,
        t: usize,
        by: &str,
        force: bool,
    ) -> Result<Archived, String> {
        let blocks = self.archive_blockers(s, t);
        if blocks.iter().any(|b| b.hard || !force) {
            return Err(Self::block_text(&blocks));
        }
        let c = self
            .snapshot_tab(s, t, 0)
            .ok_or("it never ran, so there is nothing to resume")?;
        let a = self.panes[s].account.ok_or("the pane has no account")?;
        let tab = &self.panes[s].tabs[t];
        let uid = tab.uid;
        let model = tab.model.clone();
        let bg_lock = tab.bg_lock.clone();
        let rss = tab
            .pid()
            .map(|p| crate::app_settings::rss_of(&[p]))
            .unwrap_or_default();
        let freed_kb: u64 = rss.values().sum();
        let entry = Archived {
            id: String::new(),
            provider: self.cfg.accounts[a].harness().name().to_string(),
            account: c.account.clone(),
            cwd: c.cwd.clone(),
            session_id: c.session_id.clone(),
            name: c.name.clone(),
            label: c.label.clone(),
            accent: c.accent.clone(),
            pinned: c.pinned,
            tab_group: c.tab_group.clone(),
            model,
            loops: c.loops.clone(),
            last_activity: self.tab_last_activity(s, t),
            archived_at: self.archive_now(),
            by: by.to_string(),
        };
        // A background run holding the session: stop it properly, so
        // nothing keeps working (or holding the session) in the dark.
        if let Some(lock) = bg_lock {
            let dir = self.cfg.accounts[a].config_dir();
            let (bin, pass) = (self.cfg.claude_bin(), self.cfg.pass_env.clone());
            let short = lock.short.clone();
            crate::log::info(&format!("t{uid}: archive stops the background run {short}"));
            if !cfg!(test) {
                std::thread::spawn(move || {
                    let _ = crate::app_bglock::run_stop(&bin, &short, &dir, &pass);
                });
            }
        }
        self.tab_event(s, t, "close", Some(by), Some("archived".into()));
        let id = self.archive.add(entry);
        if let Err(e) = self.archive.save() {
            crate::log::info(&format!("archive: could not save: {e}"));
            self.flash(format!("Could not save the archive: {e}"));
        }
        let fallback = self.cfg.accounts[a].work_dir();
        self.panes[s].close_tab(t, fallback);
        self.deliveries.retain(|d| d.uid != uid);
        self.freed_kb += freed_kb;
        self.state_dirty = true;
        self.archive.find(&id).cloned().ok_or("not saved".into())
    }

    /// Ctrl-a f, the menus and the tab options: archive a tab, asking
    /// first when it is busy or has unsent text (a yes forces it).
    pub fn request_archive(&mut self, s: usize, t: usize) {
        let Some(uid) = self.panes.get(s).and_then(|p| p.tabs.get(t)).map(|x| x.uid) else {
            return;
        };
        self.jump_to(s, t);
        let blocks = self.archive_blockers(s, t);
        if let Some(b) = blocks.iter().find(|b| b.hard) {
            self.flash(format!("Can't archive it: {}", b.why));
        } else if blocks.is_empty() {
            self.archive_now_flash(s, t, false);
        } else {
            self.modal = Modal::ConfirmArchive(uid);
        }
    }

    /// Archive from the UI and say so.
    pub fn archive_now_flash(&mut self, s: usize, t: usize, force: bool) {
        let jobs = self.jobs_wake_note(self.panes[s].tabs[t].uid);
        match self.archive_tab(s, t, "user", force) {
            Ok(e) => {
                let n = self.archive.entries.len();
                self.flash(format!(
                    "Archived '{}' ({n} in the archive): its session stopped, double click it under Archived to resume{}",
                    e.label,
                    jobs.map(|j| format!(". {j}")).unwrap_or_default()
                ));
            }
            Err(e) => self.flash(format!("Can't archive it: {e}")),
        }
    }

    /// The dialog's yes.
    pub fn confirm_archive(&mut self, uid: u64) {
        self.modal = Modal::None;
        if let Some((s, t)) = self.find_tab(uid) {
            self.archive_now_flash(s, t, true);
        }
    }

    /// Lines for the archive dialog.
    pub fn archive_lines(&self, uid: u64) -> Vec<String> {
        let Some((s, t)) = self.find_tab(uid) else {
            return vec!["That tab is gone.".into()];
        };
        let b = self.archive_blockers(s, t);
        vec![
            format!("Archive '{}' anyway?", self.panes[s].tabs[t].name()),
            format!("Right now: {}.", Self::block_text(&b)),
            "Its session stops; Archived keeps it to resume (a double click).".into(),
        ]
        .into_iter()
        .chain(
            self.jobs_wake_note(uid)
                .map(|j| format!("Its jobs stay: {j}.")),
        )
        .collect()
    }

    /// Restore archived entry `id`: resume it in its account's pane (or
    /// on `onto`), and drop the entry. Returns what to say.
    pub fn restore_archived(&mut self, id: &str, onto: Option<usize>) -> Result<String, String> {
        let e = self
            .archive
            .find(id)
            .cloned()
            .ok_or("that conversation is not in the archive")?;
        let mut c = e.to_closed();
        if let Some(a) = onto {
            let acct = self.cfg.accounts.get(a).ok_or("no such account")?;
            if acct.harness().name() != e.provider {
                return Err(format!(
                    "'{}' is a {} conversation; {} runs {}",
                    e.label,
                    e.provider,
                    acct.display(),
                    acct.harness().name()
                ));
            }
            c.account = acct.name.clone();
        }
        let msg = self.reopen_closed_tab(c)?;
        self.archive.remove(id);
        if self.archive_sel.as_deref() == Some(id) {
            self.archive_sel = None;
        }
        if let Err(err) = self.archive.save() {
            crate::log::info(&format!("archive: could not save: {err}"));
        }
        self.state_dirty = true;
        Ok(msg)
    }

    /// Forget an archive entry (never the session's own files).
    pub fn forget_archived(&mut self, id: &str) -> Option<String> {
        let e = self.archive.remove(id)?;
        if self.archive_sel.as_deref() == Some(id) {
            self.archive_sel = None;
        }
        if let Err(err) = self.archive.save() {
            crate::log::info(&format!("archive: could not save: {err}"));
        }
        Some(e.label)
    }

    // ---- the Archived group in a tab list ----

    /// The archived conversations pane `p`'s tab list shows (its account's,
    /// narrowed by its search), newest archived first.
    pub fn archive_for_pane(&self, p: usize) -> Vec<Archived> {
        let Some(a) = self.panes.get(p).and_then(|s| s.account) else {
            return vec![];
        };
        let name = &self.cfg.accounts[a].name;
        let q = self.panes[p].filter.trim().to_string();
        self.archive
            .newest_first()
            .into_iter()
            .filter(|e| &e.account == name)
            .filter(|e| q.is_empty() || crate::app_tabgroups::filter_match(&q, &e.search_fields()))
            .cloned()
            .collect()
    }

    /// The Archived rows at the end of pane `p`'s list: the header, and the
    /// entries when it is open (or a search is typed).
    pub fn archive_rows(&self, p: usize) -> Vec<crate::tab_groups::Row> {
        use crate::tab_groups::Row;
        let list = self.archive_for_pane(p);
        if list.is_empty() {
            return vec![];
        }
        let searching = !self.panes[p].filter.trim().is_empty();
        let mut v = vec![Row::ArchHead];
        if searching || !self.archive_collapsed {
            v.extend((0..list.len()).map(Row::Arch));
        }
        v
    }

    pub fn toggle_archive_group(&mut self) {
        self.archive_collapsed = !self.archive_collapsed;
    }

    pub fn open_archive_menu(&mut self, id: String) {
        self.archive_sel = Some(id.clone());
        self.modal = Modal::ArchMenu(id, 0);
    }

    /// Rows of an archived entry's menu: (label, key, hint).
    pub fn archive_menu_rows() -> [(&'static str, char, &'static str); 2] {
        [
            (
                "Restore",
                'r',
                "Resume the conversation in a tab (a double click does the same)",
            ),
            (
                "Delete from archive",
                'x',
                "Forget this entry; the session's own files stay and can still be resumed from the Sessions list",
            ),
        ]
    }

    pub fn archive_menu_run(&mut self, id: &str, i: usize) {
        self.modal = Modal::None;
        match i {
            0 => match self.restore_archived(id, None) {
                Ok(m) => self.flash(m),
                Err(e) => self.flash(format!("Could not restore it: {e}")),
            },
            _ => {
                if let Some(l) = self.forget_archived(id) {
                    self.flash(format!(
                        "Removed '{l}' from the archive; its session files are untouched"
                    ));
                }
            }
        }
    }

    // ---- the assistant's tools ----

    fn archived_json(&self, e: &Archived) -> Value {
        let now = self.archive_now();
        let label = self
            .cfg
            .accounts
            .iter()
            .find(|a| a.name == e.account)
            .map(|a| a.display().to_string())
            .unwrap_or_else(|| e.account.clone());
        json!({
            "id": e.id,
            "name": e.label,
            "account": self.cfg.accounts.iter().position(|a| a.name == e.account).map(|i| i + 1),
            "account_label": label,
            "provider": e.provider,
            "folder": crate::config::tilde(std::path::Path::new(&e.cwd)),
            "session": e.session_id,
            "model": e.model,
            "pinned": e.pinned,
            "group": e.tab_group,
            "loops": e.loops.len(),
            "idle_hours_when_archived": ((e.archived_at - e.last_activity).max(0) / 3600),
            "last_active": crate::ui::ago_secs(now - e.last_activity),
            "archived": crate::ui::ago_secs(now - e.archived_at),
            "by": e.by,
        })
    }

    /// The archived conversations matching `args` (account, provider,
    /// name contains, older_than / idle_for_hours since archived).
    fn archived_matching(&self, args: &Value) -> Result<Vec<&Archived>, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let acct = match args.get("account") {
            Some(v) if !v.is_null() => Some(self.account_arg(v)?),
            _ => None,
        };
        let name = acct.map(|a| self.cfg.accounts[a].name.clone());
        let provider = s("provider").map(|p| p.trim().to_lowercase());
        let text = s("name").or(s("name_contains")).or(s("text"));
        let now = self.archive_now();
        let older = match s("older_than") {
            Some(w) => Some(self.cutoff(&w)?),
            None => None,
        };
        let mut v: Vec<&Archived> = self
            .archive
            .newest_first()
            .into_iter()
            .filter(|e| name.as_ref().is_none_or(|n| &e.account == n))
            .filter(|e| provider.as_ref().is_none_or(|p| &e.provider == p))
            .filter(|e| {
                text.as_ref()
                    .is_none_or(|q| crate::app_tabgroups::filter_match(q, &e.search_fields()))
            })
            .filter(|e| older.is_none_or(|c| e.archived_at <= c))
            .collect();
        if let Some(h) = args.get("archived_for_hours").and_then(Value::as_f64) {
            v.retain(|e| (now - e.archived_at) as f64 >= h * 3600.0);
        }
        Ok(v)
    }

    /// "2 days", "36 hours", "yesterday" as the Unix time that long ago.
    fn cutoff(&self, w: &str) -> Result<i64, String> {
        let now = chrono::DateTime::from_timestamp(self.archive_now(), 0)
            .ok_or("bad clock")?
            .with_timezone(&chrono::Local);
        crate::session_index::parse_when(w, now)
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .ok_or_else(|| {
                format!("I could not read \"{w}\" as a time (try \"2 days\" or \"36 hours\")")
            })
    }

    /// list_archived.
    pub fn list_archived_json(&self, args: &Value) -> Result<Value, String> {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 100) as usize;
        let all = self.archived_matching(args)?;
        let total = all.len();
        let rows: Vec<Value> = all
            .into_iter()
            .take(limit)
            .map(|e| self.archived_json(e))
            .collect();
        let say = if total == 0 {
            "Nothing matching is archived.".to_string()
        } else {
            format!("{} archived.", plural(total, "conversation"))
        };
        Ok(json!({"ok": true, "result": {"archived": rows, "total": total, "say": say}}))
    }

    /// restore_archived: by id (one, a list, "all") and/or filters; onto
    /// `onto` (an account) when asked.
    pub fn restore_archived_tool(&mut self, args: &Value) -> Result<Value, String> {
        let onto = match args.get("onto").or(args.get("open_on")) {
            Some(v) if !v.is_null() => Some(self.account_arg(v)?),
            _ => None,
        };
        let mut ids: Vec<String> = match args.get("id").or(args.get("ids")) {
            Some(Value::String(x)) if x != "all" => vec![x.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            _ => vec![],
        };
        let all = matches!(args.get("id"), Some(Value::String(x)) if x == "all");
        if ids.is_empty() {
            if !all && args.get("name").is_none() && args.get("older_than").is_none() {
                return Err(
                    "say which: id (from list_archived), a list of ids, \"all\", or name".into(),
                );
            }
            // The account filter selects what to restore here; `onto` is
            // where it opens.
            ids = self
                .archived_matching(args)?
                .iter()
                .map(|e| e.id.clone())
                .collect();
        }
        // An id may be a unique prefix, or a tab name.
        let mut resolved = vec![];
        for i in &ids {
            let hit = self
                .archive
                .entries
                .iter()
                .filter(|e| e.id == *i || e.id.starts_with(i.as_str()))
                .map(|e| e.id.clone())
                .collect::<Vec<_>>();
            match hit.as_slice() {
                [one] => resolved.push(one.clone()),
                [] => {
                    return Err(format!(
                        "{i} is not in the archive (list_archived shows it)"
                    ))
                }
                _ => return Err(format!("{i} matches several archived conversations")),
            }
        }
        resolved.dedup();
        if resolved.is_empty() {
            return Err("nothing matching is archived".into());
        }
        let extra = resolved.len().saturating_sub(RESTORE_MAX);
        resolved.truncate(RESTORE_MAX);
        let mut done = vec![];
        let mut failed = vec![];
        for id in &resolved {
            let label = self
                .archive
                .find(id)
                .map(|e| e.label.clone())
                .unwrap_or_default();
            match self.restore_archived(id, onto) {
                Ok(m) => {
                    let tab = self.panes.get(self.focus).map(|p| tab_id(p.cur().uid));
                    done.push(json!({"id": id, "name": label, "tab": tab, "result": m}));
                }
                Err(e) => failed.push(json!({"id": id, "name": label, "reason": e})),
            }
        }
        let mut say = format!("Restored {}.", plural(done.len(), "conversation"));
        if !failed.is_empty() {
            say.push_str(&format!(" {} could not be restored.", failed.len()));
        }
        if extra > 0 {
            say.push_str(&format!(
                " {extra} more are still archived (at most {RESTORE_MAX} at a time)."
            ));
        }
        Ok(
            json!({"ok": true, "result": {"restored": done.len(), "tabs": done, "failed": failed, "still_archived": self.archive.entries.len(), "say": say}}),
        )
    }

    /// The tabs archive_tabs selects, and those it leaves with why.
    /// Selectors: tab (ids, "all"), group, account, provider, name,
    /// idle_for_hours / older_than (no activity that long).
    #[allow(clippy::type_complexity)]
    pub fn archive_select(
        &self,
        args: &Value,
    ) -> Result<(Vec<(usize, usize)>, Vec<(usize, usize, String)>), String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let now = self.archive_now();
        let idle_hours = match (
            args.get("idle_for_hours").and_then(Value::as_f64),
            s("older_than"),
        ) {
            (Some(h), _) => Some(h),
            (None, Some(w)) => Some((now - self.cutoff(&w)?) as f64 / 3600.0),
            _ => None,
        };
        let provider = s("provider").map(|p| p.trim().to_lowercase());
        let text = s("name").or(s("name_contains"));
        let named = args.get("tab").is_some_and(
            |t| !matches!(t, Value::String(x) if matches!(x.as_str(), "all" | "idle" | "ready")),
        );
        let selector = named
            || args.get("tab").is_some()
            || args.get("group").is_some()
            || args.get("account").is_some()
            || idle_hours.is_some()
            || provider.is_some()
            || text.is_some();
        if !selector {
            return Err("say which tabs: tab ids, or a filter (idle_for_hours, older_than, account, provider, name, group)".into());
        }
        let base: Vec<(usize, usize)> = if args.get("tab").is_some()
            || args.get("group").is_some()
            || args.get("account").is_some()
        {
            // `tab` "all" with other filters means every tab, filtered.
            let mut a = args.clone();
            if let Some(o) = a.as_object_mut() {
                if matches!(o.get("tab"), Some(Value::String(x)) if x == "all" || x == "idle" || x == "ready")
                {
                    o.remove("tab");
                }
            }
            if a.get("tab").is_none() && a.get("group").is_none() && a.get("account").is_none() {
                self.all_tabs()
            } else {
                self.tab_targets(&a, true)?
            }
        } else {
            self.all_tabs()
        };
        let with_pinned = args
            .get("include_pinned")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
        let mut ok = vec![];
        let mut skipped = vec![];
        for (sl, t) in base {
            let tab = &self.panes[sl].tabs[t];
            if self.panes[sl].account.is_none() {
                continue;
            }
            if let Some(p) = &provider {
                let h = self.panes[sl]
                    .account
                    .map(|a| self.cfg.accounts[a].harness().name())
                    .unwrap_or("");
                if h != p {
                    continue;
                }
            }
            if let Some(q) = &text {
                if !crate::app_tabgroups::filter_match(q, &self.tab_search_fields(sl, t)) {
                    continue;
                }
            }
            if let Some(h) = idle_hours {
                if !crate::archive::idle_at_least(self.tab_last_activity(sl, t), now, h) {
                    continue;
                }
            }
            let blocks = self.archive_blockers(sl, t);
            // A tab that never ran is not worth a line in a sweep.
            if !named && !tab.is_running() && tab.session_id.is_none() {
                continue;
            }
            if tab.pinned && !with_pinned && !named {
                skipped.push((
                    sl,
                    t,
                    "it is pinned (include_pinned archives pinned tabs too)".to_string(),
                ));
                continue;
            }
            if blocks.iter().any(|b| b.hard || !force) {
                skipped.push((sl, t, Self::block_text(&blocks)));
                continue;
            }
            ok.push((sl, t));
        }
        Ok((ok, skipped))
    }

    fn skipped_json(&self, skipped: &[(usize, usize, String)]) -> Vec<Value> {
        skipped
            .iter()
            .map(|(s, t, why)| {
                json!({"tab": tab_id(self.panes[*s].tabs[*t].uid), "name": self.panes[*s].tabs[*t].name(), "reason": why})
            })
            .collect()
    }

    /// (plan, question, needs a yes) for archive_tabs.
    pub(crate) fn archive_plan_for(
        &mut self,
        args: &Value,
    ) -> Result<(Value, String, bool), String> {
        let (ok, skipped) = self.archive_select(args)?;
        if ok.is_empty() {
            return Err(if skipped.is_empty() {
                "no tab matches (nothing was archived)".into()
            } else {
                format!(
                    "none of the {} matching can be archived: {}. Nothing was archived; force=true archives busy ones too.",
                    skipped.len(),
                    skipped
                        .iter()
                        .take(5)
                        .map(|(s, t, w)| format!("{} ({w})", self.panes[*s].tabs[*t].name()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        }
        let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
        let mut q = format!(
            "Archive {} (their sessions stop and free memory; each comes back with a double click)?",
            self.describe_set(&ok)
        );
        if !skipped.is_empty() {
            q.push_str(&format!(
                " Leaving {} alone: {}.",
                skipped.len(),
                skipped
                    .iter()
                    .take(4)
                    .map(|(s, t, w)| format!("{} ({w})", self.panes[*s].tabs[*t].name()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        // One tab named by id, nothing in the way: no question.
        let single = ok.len() == 1
            && matches!(args.get("tab"), Some(Value::String(x)) if x != "all")
            && self.archive_blockers(ok[0].0, ok[0].1).is_empty()
            && skipped.is_empty();
        Ok((json!({"uids": self.uids(&ok), "force": force}), q, !single))
    }

    /// dry_run: what archive_tabs would do, without asking or doing it.
    pub fn archive_dry_run(&mut self, args: &Value) -> Result<Value, String> {
        let (ok, skipped) = self.archive_select(args)?;
        let now = self.archive_now();
        let would: Vec<Value> = ok
            .iter()
            .map(|&(s, t)| {
                let tab = &self.panes[s].tabs[t];
                json!({
                    "tab": tab_id(tab.uid), "name": tab.name(),
                    "account": self.describe_tab(s),
                    "idle_hours": (now - self.tab_last_activity(s, t)).max(0) / 3600,
                })
            })
            .collect();
        let say = format!(
            "Would archive {}{}.",
            plural(would.len(), "tab"),
            if skipped.is_empty() {
                String::new()
            } else {
                format!(" and leave {} alone", skipped.len())
            }
        );
        Ok(
            json!({"ok": true, "result": {"dry_run": true, "would_archive": would, "skipped": self.skipped_json(&skipped), "say": say, "next": "Nothing was done. Call archive_tabs with the same arguments (without dry_run) to ask the user."}}),
        )
    }

    /// Run a confirmed archive plan.
    pub(crate) fn archive_run(&mut self, plan: &Value) -> Result<Value, String> {
        let force = plan["force"].as_bool().unwrap_or(false);
        let uids: Vec<u64> = plan["uids"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        let kb0 = self.freed_kb;
        let mut done = vec![];
        let mut skipped = vec![];
        let mut loops = 0;
        let mut waking: Vec<String> = vec![];
        for u in uids {
            if let Some(w) = self.jobs_wake_note(u) {
                waking.push(format!("{}: {w}", self.spoken_tab_name(u)));
            }
            let Some((s, t)) = self.find_tab(u) else {
                skipped.push(json!({"tab": tab_id(u), "reason": "it was closed meanwhile"}));
                continue;
            };
            let name = self.panes[s].tabs[t].name();
            match self.archive_tab(s, t, "assistant", force) {
                Ok(e) => {
                    loops += e.loops.len();
                    done.push(json!({"id": e.id, "name": e.label, "account": self.describe_tab_of(&e.account), "folder": crate::config::tilde(std::path::Path::new(&e.cwd))}));
                }
                Err(why) => skipped.push(json!({"tab": tab_id(u), "name": name, "reason": why})),
            }
        }
        let freed_mb = (self.freed_kb - kb0) / 1024;
        let n = done.len();
        let mut say = format!(
            "Archived {}. Their sessions are stopped, which frees memory",
            plural(n, "tab")
        );
        if freed_mb > 0 {
            say.push_str(&format!(" (about {freed_mb} MB)"));
        }
        say.push_str("; each comes back with a double click under Archived.");
        if !waking.is_empty() {
            say.push_str(&format!(
                " Their GodTerm jobs were kept ({}).",
                waking.join("; ")
            ));
        }
        if !skipped.is_empty() {
            say.push_str(&format!(" {} were left alone.", skipped.len()));
        }
        self.flash(format!("Archived {}", plural(n, "tab")));
        Ok(json!({"ok": true, "result": {
            "archived": n, "tabs": done, "skipped": skipped,
            "archive_total": self.archive.entries.len(),
            "memory_freed_mb": freed_mb,
            "loops_stopped": loops,
            "jobs_kept": waking,
            "note": "Sessions stopped and their pane tabs closed (each pane keeps one empty tab). Memory is freed; background runs holding a session were stopped. Durable scheduled jobs on disk keep their schedule.",
            "undo": "restore_archived",
            "say": say,
        }}))
    }

    fn describe_tab_of(&self, account: &str) -> String {
        self.cfg
            .accounts
            .iter()
            .find(|a| a.name == account)
            .map(|a| a.display().to_string())
            .unwrap_or_else(|| account.to_string())
    }

    /// get_state: how many are archived, and the latest.
    pub fn archive_state_json(&self) -> Value {
        let list = self.archive.newest_first();
        json!({
            "count": list.len(),
            "recent": list.iter().take(8).map(|e| self.archived_json(e)).collect::<Vec<_>>(),
            "note": "Archived conversations are stopped (no process, no memory) and listed under Archived in the tab list. list_archived searches them, restore_archived brings one back resumed, archive_tabs archives tabs.",
        })
    }
}
