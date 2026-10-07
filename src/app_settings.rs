//! Settings screen actions: change, reset and edit values, account
//! management, live memory figures.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::process::{Command, Stdio};

use crate::app::{App, Modal, View};
use crate::config::Config;
use crate::settings::{self, Kind, Section, Setting, SECTIONS};

/// Resident memory of godterm and of each tab's process, in KB.
#[derive(Debug, Clone, Default)]
pub struct MemStats {
    pub own_kb: u64,
    /// (pane, tab, label, kb)
    pub tabs: Vec<(usize, usize, String, u64)>,
}

impl MemStats {
    pub fn total_kb(&self) -> u64 {
        self.own_kb + self.tabs.iter().map(|t| t.3).sum::<u64>()
    }
}

/// RSS in KB for these pids, from one `ps` call.
pub fn rss_of(pids: &[u32]) -> std::collections::HashMap<u32, u64> {
    let mut out = std::collections::HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut cmd = Command::new("ps");
    cmd.args(["-o", "pid=,rss=", "-p", &list]);
    if let Some(o) = crate::creds::output_with_timeout(cmd, std::time::Duration::from_secs(2)) {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let mut it = line.split_whitespace();
            if let (Some(p), Some(r)) = (it.next(), it.next()) {
                if let (Ok(p), Ok(r)) = (p.parse(), r.parse()) {
                    out.insert(p, r);
                }
            }
        }
    }
    out
}

/// A line of the settings list.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsLine {
    Header {
        title: String,
        group: &'static str,
        collapsed: bool,
    },
    Row(usize),
}

impl App {
    pub fn open_settings(&mut self) {
        self.view = if self.view == View::Settings {
            View::Grid
        } else {
            View::Settings
        };
        if self.view == View::Settings {
            // Devices, models and voices for the pickers, in the background.
            // Devices, voices and models, in the background, once (a hung
            // ffmpeg or audio system never blocks the screen).
            settings::start_audio_lists(&self.cfg.voice, false);
            if !cfg!(test) {
                self.want_grok_usage();
            }
        }
    }

    pub fn settings_section_kind(&self) -> Section {
        SECTIONS[self.settings_section.min(SECTIONS.len() - 1)].0
    }

    /// The rows that can be selected: a search's matches, or the section's
    /// rows that apply now, minus those under a collapsed header.
    pub fn settings_rows(&self) -> Vec<Setting> {
        self.settings_view().0
    }

    /// The rows, and the lines that show them (headers, then rows).
    pub fn settings_view(&self) -> (Vec<Setting>, Vec<SettingsLine>) {
        let mut rows = vec![];
        let mut lines = vec![];
        if let Some(q) = self
            .settings_query
            .as_deref()
            .filter(|q| !q.trim().is_empty())
        {
            let mut last = String::new();
            for (sec, s) in settings::search(&self.cfg, q) {
                let name = SECTIONS
                    .iter()
                    .find(|x| x.0 == sec)
                    .map(|x| x.1)
                    .unwrap_or("");
                let head = if s.group.is_empty() {
                    name.to_string()
                } else {
                    format!("{name} › {}", s.group)
                };
                if head != last {
                    lines.push(SettingsLine::Header {
                        title: head.clone(),
                        group: "",
                        collapsed: false,
                    });
                    last = head;
                }
                lines.push(SettingsLine::Row(rows.len()));
                rows.push(s);
            }
            return (rows, lines);
        }
        let mut last: Option<&'static str> = None;
        for s in settings::settings_for(self.settings_section_kind(), &self.cfg) {
            let collapsed = self.settings_collapsed.contains(s.group);
            if !s.group.is_empty() && last != Some(s.group) {
                lines.push(SettingsLine::Header {
                    title: s.group.to_string(),
                    group: s.group,
                    collapsed,
                });
                last = Some(s.group);
            }
            if collapsed {
                continue;
            }
            lines.push(SettingsLine::Row(rows.len()));
            rows.push(s);
        }
        (rows, lines)
    }

    /// Fold or unfold a header of the section.
    pub fn toggle_settings_group(&mut self, g: &'static str) {
        if !self.settings_collapsed.remove(g) {
            self.settings_collapsed.insert(g);
        }
        let n = self.settings_rows().len();
        self.settings_sel = self.settings_sel.min(n.saturating_sub(1));
    }

    /// Write a setting and reload so it applies at once.
    pub fn setting_write(&mut self, row: usize, item: Option<toml_edit::Item>) {
        let Some(s) = self.settings_rows().get(row).cloned() else {
            return;
        };
        match settings::write(&Config::path(), &s.key, item) {
            Ok(()) => {
                // Settings win over the runtime toggles from now on.
                match &s.key {
                    settings::Key::Global("privacy") => self.rt_privacy = None,
                    settings::Key::Global("layout") | settings::Key::Global("grid") => {
                        self.rt_layout = None
                    }
                    settings::Key::Global("show_email")
                    | settings::Key::Account(_, "show_email") => self.rt_show_email = None,
                    _ => {}
                }
                self.state_dirty = true;
                self.reload_config_now();
                let now = settings::display(&self.cfg, &s);
                let shown = if now.is_empty() {
                    "default".to_string()
                } else {
                    now
                };
                self.flash(format!("{}: {shown}", s.label));
            }
            Err(e) => self.flash(format!("Not saved: {e:#}")),
        }
    }

    pub fn setting_step(&mut self, row: usize, dir: i64) {
        let Some(s) = self.settings_rows().get(row).cloned() else {
            return;
        };
        self.settings_sel = row;
        if s.key == settings::Key::Global("voice.grok.source") {
            return self.open_grok_logins();
        }
        match s.kind {
            Kind::Text | Kind::List | Kind::Secret => self.setting_edit(row),
            _ => {
                if let Some(item) = settings::stepped(&self.cfg, &s, dir) {
                    // Switching to bypass from the settings screen is explicit
                    // enough; it still logs.
                    self.setting_write(row, Some(item));
                }
            }
        }
    }

    pub fn setting_edit(&mut self, row: usize) {
        let Some(s) = self.settings_rows().get(row).cloned() else {
            return;
        };
        if s.key == settings::Key::Global("voice.grok.source") {
            self.settings_sel = row;
            return self.open_grok_logins();
        }
        if s.kind == Kind::Secret {
            // Typed fresh, never shown.
            self.settings_sel = row;
            self.modal = Modal::EditSetting(row, String::new());
        } else if matches!(s.kind, Kind::Text | Kind::List) {
            self.settings_sel = row;
            self.modal = Modal::EditSetting(row, settings::display(&self.cfg, &s));
        } else {
            self.setting_step(row, 1);
        }
    }

    pub fn setting_set_text(&mut self, row: usize, text: &str) {
        let Some(s) = self.settings_rows().get(row).cloned() else {
            return;
        };
        if s.kind == Kind::Secret {
            if text.trim().is_empty() {
                return;
            }
            match crate::voice::grok_tts::save_key(text) {
                Ok(place) => {
                    self.flash(format!("xAI API key saved in the {place}; testing it..."));
                    crate::voice::grok_tts::check_in_background(self.cfg.voice.grok.clone());
                }
                Err(e) => self.flash(format!("Not saved: {e}")),
            }
            return;
        }
        self.setting_write(row, Some(settings::from_text(&s, text)));
    }

    pub fn setting_reset(&mut self, row: usize) {
        if self
            .settings_rows()
            .get(row)
            .is_some_and(|s| s.kind == Kind::Secret)
        {
            match crate::voice::grok_tts::remove_key() {
                Ok(()) => self.flash("xAI API key removed"),
                Err(e) => self.flash(format!("Could not remove it: {e}")),
            }
            return;
        }
        self.setting_write(row, None);
    }

    /// Remove account `a` from config.toml (its config dir stays on disk).
    pub fn remove_account(&mut self, a: usize) {
        let path = Config::path();
        let res = (|| -> anyhow::Result<()> {
            let text = std::fs::read_to_string(&path)?;
            let mut doc: toml_edit::DocumentMut = text.parse()?;
            if let Some(arr) = doc
                .get_mut("account")
                .and_then(toml_edit::Item::as_array_of_tables_mut)
            {
                arr.remove(a);
            }
            let out = doc.to_string();
            Config::parse(&out)?;
            std::fs::write(&path, out)?;
            Ok(())
        })();
        match res {
            Ok(()) => {
                let name = self
                    .cfg
                    .accounts
                    .get(a)
                    .map(|x| x.display().to_string())
                    .unwrap_or_default();
                self.reload_config_now();
                self.flash(format!("Removed {name} from config.toml; it goes away on the next start (its folder is kept)"));
            }
            Err(e) => self.flash(format!("Could not remove: {e:#}")),
        }
    }

    /// Open config.toml in $EDITOR in a new tab, or the system text editor.
    pub fn open_config_in_editor(&mut self) {
        let path = Config::path();
        match std::env::var("EDITOR")
            .ok()
            .filter(|e| !e.trim().is_empty())
        {
            Some(ed) => {
                let mut parts = ed.split_whitespace().map(str::to_string);
                let prog = parts.next().unwrap_or_default();
                let mut args: Vec<String> = parts.collect();
                args.push(path.to_string_lossy().into_owned());
                let p = self.focus;
                self.run_in_new_tab(p, &prog, args);
                self.view = View::Grid;
            }
            None => {
                let _ = Command::new("open")
                    .arg("-t")
                    .arg(&path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                self.flash(format!(
                    "Opened {} (changes apply when saved)",
                    path.display()
                ));
            }
        }
    }

    /// `godterm doctor` in a new tab of the focused pane.
    pub fn run_doctor_tab(&mut self) {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "godterm".into());
        let p = self.focus;
        let mut args = vec!["doctor".to_string()];
        if self.privacy() {
            args.push("--privacy".into());
        }
        self.run_in_new_tab(p, &exe, args);
        self.view = View::Grid;
    }

    /// Refresh memory figures in the background.
    pub fn refresh_mem(&mut self) {
        let mut pids = vec![(std::process::id(), usize::MAX, usize::MAX, String::new())];
        for (si, s) in self.panes.iter().enumerate() {
            for (ti, t) in s.tabs.iter().enumerate() {
                if let Some(pid) = t.pid() {
                    let label = format!(
                        "{} tab {}",
                        s.account
                            .map(|a| self.cfg.accounts[a].display().to_string())
                            .unwrap_or_default(),
                        ti + 1
                    );
                    pids.push((pid, si, ti, label));
                }
            }
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let only: Vec<u32> = pids.iter().map(|p| p.0).collect();
            let rss = rss_of(&only);
            let mut m = MemStats::default();
            for (pid, si, ti, label) in pids {
                let kb = rss.get(&pid).copied().unwrap_or(0);
                if si == usize::MAX {
                    m.own_kb = kb;
                } else {
                    m.tabs.push((si, ti, label, kb));
                }
            }
            let _ = tx.send(crate::app::AppEvent::Mem(m));
        });
    }

    pub fn on_settings_key(&mut self, k: KeyEvent) {
        let n = self.settings_rows().len();
        // Searching: typing goes to the filter; arrows, Enter and Left /
        // Right still work on the rows.
        if let Some(q) = self.settings_query.as_mut() {
            match k.code {
                KeyCode::Esc => {
                    self.settings_query = None;
                    self.settings_sel = 0;
                    return;
                }
                KeyCode::Backspace => {
                    if q.pop().is_none() {
                        self.settings_query = None;
                    }
                    self.settings_sel = 0;
                    return;
                }
                KeyCode::Char(c)
                    if !k.modifiers.contains(KeyModifiers::CONTROL)
                        && !(c == ' ' && q.is_empty()) =>
                {
                    q.push(c);
                    self.settings_sel = 0;
                    return;
                }
                _ => {}
            }
        }
        match k.code {
            KeyCode::Char('/') if self.settings_query.is_none() => {
                self.settings_query = Some(String::new());
                self.settings_sel = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => self.view = View::Grid,
            KeyCode::Tab => {
                self.settings_section = (self.settings_section + 1) % SECTIONS.len();
                self.settings_sel = 0;
            }
            KeyCode::BackTab => {
                self.settings_section =
                    (self.settings_section + SECTIONS.len() - 1) % SECTIONS.len();
                self.settings_sel = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings_sel = self.settings_sel.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.settings_sel = (self.settings_sel + 1).min(n.saturating_sub(1))
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') => {
                self.setting_step(self.settings_sel, 1)
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => {
                self.setting_step(self.settings_sel, -1)
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.setting_edit(self.settings_sel),
            KeyCode::Backspace | KeyCode::Delete => self.setting_reset(self.settings_sel),
            KeyCode::Char('e') if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_config_in_editor()
            }
            _ => {}
        }
    }
}

impl App {
    /// The usage of a Grok login: an account's last reply, or for the main
    /// ~/.grok the one the picker fetched.
    pub fn grok_usage(&self, id: &str) -> Option<&crate::usage::Usage> {
        match self.cfg.accounts.iter().position(|c| c.name == id) {
            Some(a) => self.accounts.get(a)?.usage.as_ref(),
            None if id == "main" => self.main_grok_usage.as_ref()?.as_ref().ok(),
            None => None,
        }
    }

    /// Why a login's usage is not there: its last error.
    pub fn grok_usage_error(&self, id: &str) -> Option<String> {
        match self.cfg.accounts.iter().position(|c| c.name == id) {
            Some(a) => self.accounts.get(a)?.usage_err.as_ref().map(|e| e.short()),
            None if id == "main" => self.main_grok_usage.as_ref()?.as_ref().err().cloned(),
            None => None,
        }
    }

    /// What a login has left for voice: (percent, whether it is the Voice
    /// figure (else the weekly credits), when it resets).
    pub fn grok_left(
        &self,
        id: &str,
    ) -> Option<(f64, bool, Option<chrono::DateTime<chrono::Utc>>)> {
        let u = self.grok_usage(id)?;
        let voice = u
            .windows
            .iter()
            .find(|w| w.label.to_lowercase().contains("voice"));
        let weekly = u.windows.iter().find(|w| w.key == "seven_day");
        match (voice, weekly) {
            (Some(v), _) => Some((
                v.left(),
                true,
                v.resets_at.or(weekly.and_then(|w| w.resets_at)),
            )),
            (None, Some(w)) => Some((w.left(), false, w.resets_at)),
            _ => None,
        }
    }

    /// The login with the most voice (or weekly credits) left; never one
    /// with nothing left or one xAI refused.
    pub fn grok_best(&self) -> Option<String> {
        let refused = crate::voice::grok_tts::refused_ids();
        let mut best: Option<(String, f64)> = None;
        for s in crate::voice::grok_tts::sources(&self.cfg) {
            if !s.logged_in || refused.contains(&s.id) {
                continue;
            }
            // Effective: the voice figure, and never more than the weekly.
            let Some((left, _, _)) = self.grok_left(&s.id) else {
                continue;
            };
            let weekly = self
                .grok_usage(&s.id)
                .and_then(|u| u.windows.iter().find(|w| w.key == "seven_day"))
                .map(|w| w.left())
                .unwrap_or(100.0);
            let eff = left.min(weekly);
            if eff <= 0.0 {
                continue;
            }
            if best.as_ref().is_none_or(|b| eff > b.1) {
                best = Some((s.id, eff));
            }
        }
        best.map(|b| b.0)
    }

    /// One login, compact: "demo8 (d…@…) · Voice 82% · weekly 37%", or
    /// why it cannot be used, or its usage error.
    pub fn grok_source_line(&self, id: &str) -> String {
        use crate::voice::grok_tts;
        let all = grok_tts::sources(&self.cfg);
        let refused = grok_tts::refused_ids();
        if id == "auto" || id.is_empty() {
            return match grok_tts::auto_choice(&all, &refused) {
                Some(s) => format!("auto → {}", self.grok_source_line(&s.id)),
                None => "auto → none available".into(),
            };
        }
        let Some(s) = all.iter().find(|s| s.id == id) else {
            return format!("{id} (no such grok login)");
        };
        let a = self.cfg.accounts.iter().position(|c| c.name == s.id);
        let mut out = s.label.clone();
        if let Some(e) = a.and_then(|a| self.email_label(a, 24)) {
            out.push_str(&format!(" ({e})"));
        }
        if !s.logged_in {
            out.push_str(" · not logged in");
            return out;
        }
        if let Some(at) = grok_tts::refused_at(&s.id) {
            out.push_str(&format!(" · out of credits at {}", at.format("%H:%M")));
            return out;
        }
        match self.grok_left(&s.id) {
            Some((left, voice, _)) => {
                let weekly = self
                    .grok_usage(&s.id)
                    .and_then(|u| u.windows.iter().find(|w| w.key == "seven_day"))
                    .map(|w| w.left());
                if voice {
                    out.push_str(&format!(" · Voice {left:.0}%"));
                    if let Some(w) = weekly {
                        out.push_str(&format!(" · weekly {w:.0}%"));
                    }
                } else {
                    out.push_str(&format!(" · weekly {left:.0}% (voice not reported)"));
                }
                if self.grok_best().as_deref() == Some(s.id.as_str()) {
                    out.push_str(" · most left");
                }
            }
            None => match self.grok_usage_error(&s.id) {
                Some(e) => out.push_str(&format!(" · usage unavailable: {e}")),
                None if a.is_none() && s.id != "main" => {}
                None => out.push_str(" · usage loading…"),
            },
        }
        if self.privacy() {
            out = crate::app_privacy::mask_emails(&out);
        }
        out
    }

    /// Every login of the picker on one line.
    pub fn grok_sources_hint(&self) -> String {
        let all = crate::voice::grok_tts::sources(&self.cfg);
        let lines: Vec<String> = all.iter().map(|s| self.grok_source_line(&s.id)).collect();
        format!("Change… picks a login. {}", lines.join("  |  "))
    }

    /// Fetch the usage of every Grok login that has none (Settings, the
    /// picker): the accounts, and the main ~/.grok when it is logged in
    /// (its token is read, nothing written).
    pub fn want_grok_usage(&mut self) {
        let ids: Vec<usize> = self
            .cfg
            .accounts
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                c.harness() == crate::harness::Harness::Grok
                    && self.accounts.get(*i).is_some_and(|a| a.usage.is_none())
            })
            .map(|(i, _)| i)
            .collect();
        for i in ids {
            self.request_usage(i);
        }
        self.fetch_main_grok_usage(false);
    }

    /// The main ~/.grok's usage, in the background.
    pub fn fetch_main_grok_usage(&mut self, again: bool) {
        if cfg!(test) || (self.main_grok_usage.is_some() && !again) {
            return;
        }
        let main = crate::config::dirs().main_grok;
        if !main.join("auth.json").is_file() {
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = crate::harness::grok::fetch_usage(&main).map_err(|e| e.short());
            let _ = tx.send(crate::app::AppEvent::MainGrokUsage(Box::new(r)));
        });
    }
}
