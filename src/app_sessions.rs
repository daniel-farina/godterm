//! The Sessions view across sources (every account and the main
//! `~/.claude`), with search, marking, and copy / move between them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

use crate::app::{App, AppEvent, Modal, View};
use crate::session_ops::{self, Conflict, Moved};
use crate::sessions::SessionInfo;

/// Where sessions come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Src {
    /// The main Claude Code dir (`~/.claude`), read only unless you move
    /// out of it.
    Main,
    /// The main Grok home (`~/.grok`), read only the same way.
    MainGrok,
    Account(usize),
}

/// Source chip in the Sessions view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceSel {
    All,
    One(Src),
}

/// A pending copy or move, walked through its confirmations.
#[derive(Debug, Clone, PartialEq)]
pub struct SessOp {
    pub items: Vec<(Src, PathBuf, String)>,
    pub mv: bool,
    pub open_after: bool,
    pub target: Option<Src>,
    pub conflict: Option<Conflict>,
    pub confirmed: bool,
}

/// Sentinel account index for the main dir in AppEvent::Sessions.
pub const MAIN_IDX: usize = usize::MAX;
/// And for the main grok home.
pub const MAIN_GROK_IDX: usize = usize::MAX - 1;

impl App {
    pub fn src_dir(&self, s: Src) -> PathBuf {
        match s {
            Src::Main => session_ops::main_dir(),
            Src::MainGrok => crate::harness::Harness::Grok.main_home(),
            Src::Account(a) => self.cfg.accounts[a].config_dir(),
        }
    }

    /// Which harness a source's sessions belong to (copies stay within one).
    pub fn src_harness(&self, s: Src) -> crate::harness::Harness {
        match s {
            Src::Main => crate::harness::Harness::Claude,
            Src::MainGrok => crate::harness::Harness::Grok,
            Src::Account(a) => self
                .cfg
                .accounts
                .get(a)
                .map(|c| c.harness())
                .unwrap_or(crate::harness::Harness::Claude),
        }
    }

    pub fn has_grok(&self) -> bool {
        self.cfg
            .accounts
            .iter()
            .any(|a| a.harness() == crate::harness::Harness::Grok)
    }

    /// Read only scan of the main ~/.grok sessions (summaries only).
    pub fn load_main_grok_sessions(&mut self) {
        let dir = crate::harness::Harness::Grok.main_home();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let list = crate::harness::grok::scan_sessions(&dir);
            let _ = tx.send(AppEvent::Sessions(MAIN_GROK_IDX, list));
        });
    }

    pub fn src_name(&self, s: Src) -> String {
        match s {
            Src::Main => "This Mac (main)".into(),
            Src::MainGrok => "This Mac (grok)".into(),
            Src::Account(a) => self
                .cfg
                .accounts
                .get(a)
                .map(|c| c.display().to_string())
                .unwrap_or_default(),
        }
    }

    /// The source chips: All, This Mac (main), then every account.
    pub fn source_chips(&self) -> Vec<SourceSel> {
        let mut v = vec![SourceSel::All, SourceSel::One(Src::Main)];
        if self.has_grok() {
            v.push(SourceSel::One(Src::MainGrok));
        }
        v.extend((0..self.cfg.accounts.len()).map(|a| SourceSel::One(Src::Account(a))));
        v
    }

    pub fn set_source(&mut self, s: SourceSel) {
        self.sess_source = s;
        self.sel_session = 0;
        if let SourceSel::One(Src::Account(a)) = s {
            self.sel_account = a;
        }
        self.view = View::Sessions;
        self.load_source();
    }

    /// Scan what the current source needs.
    pub fn load_source(&mut self) {
        match self.sess_source {
            SourceSel::All => {
                for a in 0..self.cfg.accounts.len() {
                    self.load_sessions(a);
                }
                self.load_main_sessions();
            }
            SourceSel::One(Src::Main) => self.load_main_sessions(),
            SourceSel::One(Src::MainGrok) => self.load_main_grok_sessions(),
            SourceSel::One(Src::Account(a)) => self.load_sessions(a),
        }
    }

    /// Read only scan of the main ~/.claude transcripts.
    pub fn load_main_sessions(&mut self) {
        if self.main_loading {
            return;
        }
        self.main_loading = true;
        let dir = session_ops::main_dir();
        let cache = std::sync::Arc::clone(&self.main_cache);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let list = cache.lock().unwrap_or_else(|e| e.into_inner()).scan(&dir);
            let _ = tx.send(AppEvent::Sessions(MAIN_IDX, list));
        });
    }

    /// Rows of the current source, newest first, filtered by the search.
    pub fn session_rows(&self) -> Vec<(Src, &SessionInfo)> {
        let mut v: Vec<(Src, &SessionInfo)> = vec![];
        match self.sess_source {
            SourceSel::All => {
                for (a, st) in self.accounts.iter().enumerate() {
                    v.extend(st.sessions.iter().map(|s| (Src::Account(a), s)));
                }
                v.extend(self.main_sessions.iter().map(|s| (Src::Main, s)));
                v.extend(self.main_grok_sessions.iter().map(|s| (Src::MainGrok, s)));
                v.sort_by_key(|(_, s)| std::cmp::Reverse(s.modified));
            }
            SourceSel::One(Src::Main) => {
                v.extend(self.main_sessions.iter().map(|s| (Src::Main, s)))
            }
            SourceSel::One(Src::MainGrok) => {
                v.extend(self.main_grok_sessions.iter().map(|s| (Src::MainGrok, s)))
            }
            SourceSel::One(Src::Account(a)) => {
                if let Some(st) = self.accounts.get(a) {
                    v.extend(st.sessions.iter().map(|s| (Src::Account(a), s)));
                }
            }
        }
        if self.sess_subagents {
            let want = |src: Src| match self.sess_source {
                SourceSel::All => true,
                SourceSel::One(s) => s == src,
            };
            v.extend(
                self.sess_sub_rows
                    .iter()
                    .filter(|(s, _)| want(*s))
                    .map(|(s, i)| (*s, i)),
            );
            v.sort_by_key(|(_, s)| std::cmp::Reverse(s.modified));
        }
        if let Some(h) = self.sess_harness {
            let want = if h == 1 {
                crate::harness::Harness::Grok
            } else {
                crate::harness::Harness::Claude
            };
            v.retain(|(s, _)| self.src_harness(*s) == want);
        }
        if !self.sess_headless {
            v.retain(|(_, s)| !crate::sess_sort::headless(s));
        }
        // The chosen sort, inside the chosen groups.
        let names: std::collections::HashMap<Src, String> =
            v.iter().map(|(s, _)| (*s, self.src_name(*s))).collect();
        let (key, rev, group) = (self.sess_sort, self.sess_sort_rev, self.sess_group);
        v.sort_by(|a, b| {
            let g = match group {
                crate::sess_sort::Group::None => std::cmp::Ordering::Equal,
                crate::sess_sort::Group::Source => {
                    names[&a.0].to_lowercase().cmp(&names[&b.0].to_lowercase())
                }
                crate::sess_sort::Group::Project => {
                    a.1.cwd.to_lowercase().cmp(&b.1.cwd.to_lowercase())
                }
                crate::sess_sort::Group::Harness => (self.src_harness(a.0)
                    != crate::harness::Harness::Claude)
                    .cmp(&(self.src_harness(b.0) != crate::harness::Harness::Claude)),
            };
            g.then_with(|| {
                crate::sess_sort::compare(key, rev, (&names[&a.0], a.1), (&names[&b.0], b.1))
            })
        });
        let q = self.sess_filter.trim().to_lowercase();
        if !q.is_empty() {
            v.retain(|(_, s)| {
                s.summary().to_lowercase().contains(&q)
                    || s.cwd.to_lowercase().contains(&q)
                    || s.id.starts_with(&q)
            });
        }
        v
    }

    /// Sessions the headless filter hides from the current source.
    pub fn headless_hidden(&self) -> usize {
        if self.sess_headless {
            return 0;
        }
        let mut n = 0;
        let mut count = |src: Src, list: &[SessionInfo]| {
            if matches!(self.sess_source, SourceSel::All) || self.sess_source == SourceSel::One(src)
            {
                n += list
                    .iter()
                    .filter(|s| crate::sess_sort::headless(s))
                    .count();
            }
        };
        for (a, st) in self.accounts.iter().enumerate() {
            count(Src::Account(a), &st.sessions);
        }
        count(Src::Main, &self.main_sessions);
        count(Src::MainGrok, &self.main_grok_sessions);
        n
    }

    /// Sort by this column; the same one again reverses it.
    pub fn sort_sessions_by(&mut self, k: crate::sess_sort::SortKey) {
        if self.sess_sort == k {
            self.sess_sort_rev = !self.sess_sort_rev;
        } else {
            self.sess_sort = k;
            self.sess_sort_rev = false;
        }
        self.sel_session = 0;
        self.state_dirty = true;
    }

    /// The session is open in a running tab.
    pub fn session_live(&self, id: &str) -> bool {
        self.panes
            .iter()
            .flat_map(|s| s.tabs.iter())
            .any(|t| t.is_running() && t.session_id.as_deref() == Some(id))
    }

    fn selected_row(&self) -> Option<(Src, PathBuf, String, String)> {
        self.session_rows()
            .get(self.sel_session)
            .map(|(src, s)| (*src, s.path.clone(), s.id.clone(), s.cwd.clone()))
    }

    /// Marked rows, or the selected one when nothing is marked.
    fn op_items(&self) -> Vec<(Src, PathBuf, String)> {
        let rows = self.session_rows();
        let marked: Vec<_> = rows
            .iter()
            .filter(|(_, s)| self.sess_marked.contains(&s.path))
            .map(|(src, s)| (*src, s.path.clone(), s.id.clone()))
            .collect();
        if !marked.is_empty() {
            return marked;
        }
        self.selected_row()
            .map(|(a, p, i, _)| vec![(a, p, i)])
            .unwrap_or_default()
    }

    /// Start a copy (or move) of the marked / selected sessions.
    pub fn start_session_op(&mut self, mv: bool, open_after: bool) {
        let items = self.op_items();
        if items.is_empty() {
            self.flash("No session selected");
            return;
        }
        self.sess_op = Some(SessOp {
            items,
            mv,
            open_after,
            target: None,
            conflict: None,
            confirmed: false,
        });
        self.modal = Modal::SessTarget(0);
    }

    /// Targets for the pending operation: every account, then the main dir.
    pub fn op_targets(&self) -> Vec<Src> {
        let mut v: Vec<Src> = (0..self.cfg.accounts.len()).map(Src::Account).collect();
        v.push(Src::Main);
        if self.has_grok() {
            v.push(Src::MainGrok);
        }
        if let Some(op) = &self.sess_op {
            // Only where the same harness can resume it.
            if let Some((s, _, _)) = op.items.first() {
                let h = self.src_harness(*s);
                v.retain(|t| self.src_harness(*t) == h);
            }
            // Not where every item already lives.
            v.retain(|t| !op.items.iter().all(|(s, _, _)| s == t));
            if op.open_after {
                v.retain(|t| !matches!(t, Src::Main | Src::MainGrok));
            }
        }
        v
    }

    pub fn choose_target(&mut self, i: usize) {
        let targets = self.op_targets();
        let Some(&t) = targets.get(i) else { return };
        if let Some(op) = self.sess_op.as_mut() {
            op.target = Some(t);
        }
        self.advance_op();
    }

    /// Next step: confirmation for moves and for writing into the main
    /// dir, the conflict question, then the work.
    pub fn advance_op(&mut self) {
        let Some(op) = self.sess_op.clone() else {
            self.modal = Modal::None;
            return;
        };
        let Some(target) = op.target else { return };
        if (op.mv || matches!(target, Src::Main | Src::MainGrok)) && !op.confirmed {
            self.modal = Modal::SessConfirm;
            return;
        }
        let tdir = self.src_dir(target);
        let clash = op.items.iter().any(|(s, p, _)| {
            p.strip_prefix(self.src_dir(*s))
                .is_ok_and(|rel| tdir.join(rel).exists())
        });
        if clash && op.conflict.is_none() {
            self.modal = Modal::SessConflict(0);
            return;
        }
        self.modal = Modal::None;
        self.run_session_op(op, target);
    }

    fn run_session_op(&mut self, op: SessOp, target: Src) {
        let tdir = self.src_dir(target);
        let conflict = op.conflict.unwrap_or(Conflict::Skip);
        let trash = crate::config::app_home().join("trash");
        let (mut done, mut skipped, mut failed) = (0, 0, vec![]);
        let mut moves: Vec<Moved> = vec![];
        let mut last_id = None;
        for (src, path, id) in &op.items {
            let sdir = self.src_dir(*src);
            if op.mv && self.session_live(id) {
                failed.push(format!("{id}: open in a tab"));
                continue;
            }
            let res = if self.src_harness(*src) == crate::harness::Harness::Grok {
                crate::harness::grok::copy_session(path, &sdir, &tdir, op.mv, &trash).map(
                    |(c, m)| {
                        if let Some(m) = m {
                            moves.push(m);
                        }
                        c
                    },
                )
            } else if op.mv {
                session_ops::move_session(&sdir, path, id, &tdir, conflict, &trash).map(|m| {
                    let c = m.copied.clone();
                    moves.push(m);
                    c
                })
            } else {
                session_ops::copy_session(&sdir, path, id, &tdir, conflict)
            };
            match res {
                Ok(c) if c.skipped => skipped += 1,
                Ok(c) => {
                    done += 1;
                    last_id = Some(c.id);
                }
                Err(e) => failed.push(format!("{id}: {e:#}")),
            }
        }
        crate::log::info(&format!(
            "sessions: {} {done} to {}, {skipped} skipped, {} failed",
            if op.mv { "moved" } else { "copied" },
            tdir.display(),
            failed.len()
        ));
        self.sess_marked.clear();
        if !moves.is_empty() {
            self.last_moves = moves;
        }
        // Rescan both ends.
        for s in op
            .items
            .iter()
            .map(|(s, _, _)| *s)
            .chain(std::iter::once(target))
        {
            match s {
                Src::Main => {
                    self.main_cache = Default::default();
                    self.main_loading = false;
                    self.load_main_sessions();
                }
                Src::MainGrok => self.load_main_grok_sessions(),
                Src::Account(a) => {
                    if let Some(st) = self.accounts.get_mut(a) {
                        st.sessions_loading = false;
                    }
                    self.load_sessions(a);
                }
            }
        }
        let verb = if op.mv { "Moved" } else { "Copied" };
        let first = op
            .items
            .first()
            .map(|(_, p, _)| {
                p.file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let what = if done == 1 {
            first
        } else {
            format!("{done} sessions")
        };
        let mut msg = format!("{verb} {what} to {}", self.src_name(target));
        if skipped > 0 {
            msg.push_str(&format!(", {skipped} already there (skipped)"));
        }
        if let Some(f) = failed.first() {
            msg.push_str(&format!(", {} failed ({f})", failed.len()));
        }
        if op.mv && done > 0 {
            msg.push_str(". Undo move: u");
        }
        self.flash(msg);
        if op.open_after {
            if let (Some(id), Src::Account(a)) = (last_id, target) {
                // Make sure the target knows the copy before resuming it.
                let path = op.items.first().map(|(s, p, _)| {
                    p.strip_prefix(self.src_dir(*s))
                        .map(|r| tdir.join(r))
                        .unwrap_or_default()
                });
                if let Some(info) = path.and_then(|p| crate::sessions::parse_file(&p)) {
                    if let Some(st) = self.accounts.get_mut(a) {
                        if !st.sessions.iter().any(|s| s.id == id) {
                            st.sessions.insert(0, info);
                        }
                    }
                }
                self.resume(a, id);
                self.flash(format!(
                    "Resumed in {}: it is billed to that account",
                    self.src_name(target)
                ));
            }
        }
    }

    pub fn undo_last_move(&mut self) {
        if self.last_moves.is_empty() {
            self.flash("Nothing to undo");
            return;
        }
        let moves = std::mem::take(&mut self.last_moves);
        let mut ok = 0;
        for m in moves.iter().rev() {
            match session_ops::undo_move(m) {
                Ok(()) => ok += 1,
                Err(e) => self.flash(format!("Undo failed: {e:#}")),
            }
        }
        self.main_cache = Default::default();
        self.main_loading = false;
        for st in &mut self.accounts {
            st.sessions_loading = false;
        }
        self.load_source();
        self.flash(format!(
            "Undid the move of {ok} session{}",
            if ok == 1 { "" } else { "s" }
        ));
    }

    /// Mark every session of the selected one's project (same folder).
    pub fn mark_project(&mut self) {
        let Some((_, _, _, cwd)) = self.selected_row() else {
            return;
        };
        let paths: Vec<PathBuf> = self
            .session_rows()
            .iter()
            .filter(|(_, s)| s.cwd == cwd)
            .map(|(_, s)| s.path.clone())
            .collect();
        let n = paths.len();
        self.sess_marked.extend(paths);
        self.flash(format!(
            "Marked {n} sessions in {}: c copies, m moves them",
            crate::ui::tilde(&cwd)
        ));
    }

    pub fn toggle_mark(&mut self) {
        if let Some((_, p, _, _)) = self.selected_row() {
            if !self.sess_marked.remove(&p) {
                self.sess_marked.insert(p);
            }
            self.move_session(1);
        }
    }

    /// The Bring here choice: 0 take over (wait for its turn), 1 take over
    /// now, 2 copy a snapshot. Taking over from the UI is the confirmation.
    pub fn bring_here(&mut self, id: &str, k: u8) {
        let args = serde_json::json!({});
        let (live, target) = match self.takeover_target_pub(id, &args) {
            Ok(x) => x,
            Err(e) => return self.flash(e),
        };
        let mode = if k == 2 {
            crate::takeover::Mode::Copy
        } else {
            crate::takeover::Mode::Take
        };
        match self.start_takeover(live, target, mode, k == 1, false) {
            Ok(m) => self.flash(format!("Bringing it here: {m}")),
            Err(e) => self.flash(e),
        }
    }

    /// Enter / o on a row: resume an account's session; a main session is
    /// copied to an account first (claude never runs on ~/.claude here).
    pub fn open_selected_session(&mut self) {
        match self.selected_row() {
            Some((Src::Account(a), _, id, _)) => self.resume(a, id),
            Some((Src::Main | Src::MainGrok, ..)) => self.start_session_op(false, true),
            None => {}
        }
    }

    pub fn on_sessions_key2(&mut self, k: KeyEvent) -> bool {
        if self.sess_searching {
            match k.code {
                KeyCode::Esc => {
                    self.sess_searching = false;
                    self.sess_filter.clear();
                }
                KeyCode::Enter => self.sess_searching = false,
                KeyCode::Backspace => {
                    self.sess_filter.pop();
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.sess_filter.clear();
                    self.sel_session = 0;
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.sess_filter.push(c);
                    self.sel_session = 0;
                }
                _ => {}
            }
            return true;
        }
        let chips = self.source_chips();
        let at = chips
            .iter()
            .position(|c| *c == self.sess_source)
            .unwrap_or(0);
        match k.code {
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.set_source(chips[(at + 1) % chips.len()])
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.set_source(chips[(at + chips.len() - 1) % chips.len()])
            }
            KeyCode::Char('/') => {
                self.sess_searching = true;
                self.sess_filter.clear();
            }
            KeyCode::Char('s') => {
                self.sess_sort = self.sess_sort.next();
                self.sess_sort_rev = false;
                self.sel_session = 0;
                self.state_dirty = true;
                self.flash(format!(
                    "Sorted by {}",
                    self.sess_sort.label().to_lowercase()
                ));
            }
            KeyCode::Char('S') => {
                self.sess_sort_rev = !self.sess_sort_rev;
                self.sel_session = 0;
                self.state_dirty = true;
            }
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('p') => self.mark_project(),
            KeyCode::Char('c') => self.start_session_op(false, false),
            KeyCode::Char('m') => self.start_session_op(true, false),
            KeyCode::Char('o') | KeyCode::Enter => self.open_selected_session(),
            KeyCode::Char('u') => self.undo_last_move(),
            KeyCode::Char('B') => {
                // Bring a session running elsewhere here.
                if let Some((_, _, id, _)) = self.selected_row() {
                    if self.sess_live.contains_key(&id) {
                        self.modal = Modal::TakeOver(id);
                    } else if self.session_live(&id) {
                        self.flash("That session is already in a GodTerm tab");
                    } else {
                        self.flash("Not running anywhere else: Enter resumes it here");
                    }
                }
            }
            KeyCode::Char('r') => {
                self.main_loading = false;
                for st in &mut self.accounts {
                    st.sessions_loading = false;
                }
                self.load_source();
            }
            _ => return false,
        }
        true
    }

    /// Keys in the session dialogs.
    pub fn on_sess_modal_key(&mut self, k: KeyEvent) {
        match self.modal.clone() {
            Modal::SessTarget(sel) => {
                let n = self.op_targets().len();
                match k.code {
                    KeyCode::Esc => self.cancel_op(),
                    KeyCode::Up => self.modal = Modal::SessTarget(sel.saturating_sub(1)),
                    KeyCode::Down => {
                        self.modal = Modal::SessTarget((sel + 1).min(n.saturating_sub(1)))
                    }
                    KeyCode::Enter => self.choose_target(sel),
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        let i = (c as u8 - b'1') as usize;
                        self.choose_target(i);
                    }
                    _ => {}
                }
            }
            Modal::SessConfirm => match k.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm_op(),
                _ => self.cancel_op(),
            },
            Modal::SessConflict(sel) => match k.code {
                KeyCode::Esc => self.cancel_op(),
                KeyCode::Up => self.modal = Modal::SessConflict(sel.saturating_sub(1)),
                KeyCode::Down => self.modal = Modal::SessConflict((sel + 1).min(2)),
                KeyCode::Char('s') => self.set_conflict(Conflict::Skip),
                KeyCode::Char('o') => self.set_conflict(Conflict::Overwrite),
                KeyCode::Char('n') => self.set_conflict(Conflict::NewId),
                KeyCode::Enter => self.set_conflict(
                    [Conflict::Skip, Conflict::Overwrite, Conflict::NewId][sel.min(2)],
                ),
                _ => {}
            },
            _ => {}
        }
    }

    pub fn confirm_op(&mut self) {
        if let Some(op) = self.sess_op.as_mut() {
            op.confirmed = true;
        }
        self.advance_op();
    }

    pub fn set_conflict(&mut self, c: Conflict) {
        if let Some(op) = self.sess_op.as_mut() {
            op.conflict = Some(c);
        }
        self.advance_op();
    }

    pub fn cancel_op(&mut self) {
        self.sess_op = None;
        self.modal = Modal::None;
        self.flash("Cancelled");
    }

    /// Lines for the confirmation dialog.
    pub fn op_confirm_lines(&self) -> Vec<String> {
        let Some(op) = &self.sess_op else {
            return vec![];
        };
        let t = op.target.map(|t| self.src_name(t)).unwrap_or_default();
        let mut v = vec![];
        let names: Vec<String> = op
            .items
            .iter()
            .take(3)
            .map(|(s, p, _)| {
                format!(
                    "{} ({})",
                    p.file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    self.src_name(*s)
                )
            })
            .collect();
        v.push(format!(
            "{} {} to {t}?",
            if op.mv { "Move" } else { "Copy" },
            if op.items.len() == 1 {
                names[0].clone()
            } else {
                format!("{} sessions", op.items.len())
            }
        ));
        if op.items.len() > 1 {
            for n in &names {
                v.push(format!("  {n}"));
            }
            if op.items.len() > 3 {
                v.push(format!("  and {} more", op.items.len() - 3));
            }
        }
        if op.mv {
            v.push(
                "The originals go to ~/.godterm/trash after the copy is verified (u undoes)."
                    .into(),
            );
        }
        if op.target == Some(Src::Main) {
            v.push("This writes into your main ~/.claude.".into());
        }
        if op.target == Some(Src::MainGrok) {
            v.push(
                "This writes into your main ~/.grok (sessions only; its login is never touched)."
                    .into(),
            );
        }
        if op.open_after {
            v.push("It then resumes there, billed to that account.".into());
        }
        v
    }
}
