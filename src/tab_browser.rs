//! A browser per tab. The Browser button on a pane's bottom bar (also
//! Ctrl-a i and the command palette) opens a small dialog for the tab: it
//! lists the Chrome profiles (from `Local State`) with the BrowserSkill
//! extension's state in each. Choosing one
//!
//!  - binds that profile to the tab (saved with the tab, it follows a
//!    move) and makes it the default profile for links (`[browser]
//!    default_profile`);
//!  - if the extension is missing, offers to open its Chrome Web Store page
//!    in THAT profile; Chrome makes the user press Add;
//!  - if it is installed but off, offers to open chrome://extensions there;
//!  - then looks every few seconds, only while the dialog or a pending
//!    enable is open, and marks the tab ready once the extension is on.
//!
//! GodTerm only reads Chrome's profile files and only launches Chrome, after
//! a yes in the dialog. It never writes into a profile and never forces an
//! install by policy.
//!
//! Awareness and scope: the tab's own `godterm mcp` (the tab token) answers
//! list_browsers and browser_status for the tab's bound profile only, and
//! the assistant sees the binding in list_tabs. Another tab never gets it.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};

use crate::app::{App, Modal};
use crate::browser::{self, Browser, ChromeProfile, Env, ProfileExt, EXTENSION_ID};
use crate::hits::UiAction;

/// The extension's page in the Chrome Web Store.
pub const STORE_URL: &str =
    "https://chromewebstore.google.com/detail/hhcmgoofomhgciiibhipgmgkgnoenaoi";
/// Chrome's extensions page.
pub const EXTENSIONS_URL: &str = "chrome://extensions";

/// How often the profiles are looked at while a dialog or a pending enable
/// is open.
const POLL_EVERY: Duration = Duration::from_secs(3);
/// A pending enable is given up after this long.
const PENDING_FOR: Duration = Duration::from_secs(600);

// ---------- states ----------

/// The extension in one profile, as the button and the dialog see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtState {
    Missing,
    Disabled,
    Enabled,
}

pub fn ext_state(p: &ProfileExt) -> ExtState {
    match (p.installed, p.enabled) {
        (_, Some(true)) => ExtState::Enabled,
        (_, Some(false)) | (true, None) => ExtState::Disabled,
        (false, None) => ExtState::Missing,
    }
}

/// The state of a tab's Browser button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtnState {
    /// No profile chosen for the tab.
    None,
    /// A profile is chosen, the extension is not in it.
    Chosen,
    /// The extension is in the profile but turned off.
    NeedsEnable,
    /// The extension is on in the tab's profile.
    Ready,
}

impl BtnState {
    pub fn word(self) -> &'static str {
        match self {
            BtnState::None => "none",
            BtnState::Chosen => "needs_install",
            BtnState::NeedsEnable => "needs_enable",
            BtnState::Ready => "ready",
        }
    }
}

/// The button state from a tab's bound profile folder and the profiles.
pub fn button_state(bound: Option<&str>, profiles: &[ProfileExt]) -> BtnState {
    let Some(dir) = bound else {
        return BtnState::None;
    };
    match profiles.iter().find(|p| p.profile.dir == dir) {
        None => BtnState::Chosen,
        Some(p) => match ext_state(p) {
            ExtState::Enabled => BtnState::Ready,
            ExtState::Disabled => BtnState::NeedsEnable,
            ExtState::Missing => BtnState::Chosen,
        },
    }
}

/// The button's text for a state and the profile's name.
pub fn button_label(state: BtnState, name: &str) -> String {
    let n = crate::sessions::snippet(name, 14);
    match state {
        BtnState::None => " browser ".into(),
        BtnState::Chosen => format!(" browser: {n} (add) "),
        BtnState::NeedsEnable => format!(" browser: {n} (turn on) "),
        BtnState::Ready => format!(" ✓ browser: {n} "),
    }
}

/// The same, shorter, for rows with little room: the full text, then
/// without the profile name, then just the word (its color keeps the state).
pub fn button_labels(state: BtnState, name: &str) -> Vec<String> {
    let mid = match state {
        BtnState::None => " browser ",
        BtnState::Chosen => " browser (add) ",
        BtnState::NeedsEnable => " browser (turn on) ",
        BtnState::Ready => " ✓ browser ",
    };
    let tiny = if state == BtnState::Ready {
        " ✓ browser "
    } else {
        " browser "
    };
    let mut v = vec![button_label(state, name), mid.to_string(), tiny.to_string()];
    v.dedup();
    v
}

/// What opening Chrome in the dialog would show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPlan {
    Store,
    Extensions,
}

impl OpenPlan {
    pub fn url(self) -> &'static str {
        match self {
            OpenPlan::Store => STORE_URL,
            OpenPlan::Extensions => EXTENSIONS_URL,
        }
    }
}

/// What to open for a profile whose extension is in this state.
pub fn plan_for(s: ExtState) -> Option<OpenPlan> {
    match s {
        ExtState::Missing => Some(OpenPlan::Store),
        ExtState::Disabled => Some(OpenPlan::Extensions),
        ExtState::Enabled => None,
    }
}

/// Which connected `bsk` browsers are this profile's. `bsk` does not say
/// which profile an instance is: the label the user typed in the extension
/// popup is the only hint, so a label equal to the profile's name, folder
/// or email counts. A lone connected browser counts when this profile is
/// the only one with the extension on.
pub fn match_browsers(
    all: &[Browser],
    p: &ChromeProfile,
    enabled_profiles: usize,
    this_enabled: bool,
) -> (Vec<Browser>, &'static str) {
    let same = |a: &str, b: &str| !b.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim());
    let by_label: Vec<Browser> = all
        .iter()
        .filter(|b| {
            [&p.name, &p.dir, &p.user_name, &p.gaia_name]
                .iter()
                .any(|f| same(&b.label, f))
        })
        .cloned()
        .collect();
    if !by_label.is_empty() {
        return (by_label, "label");
    }
    if all.len() == 1 && enabled_profiles == 1 && this_enabled {
        return (all.to_vec(), "only browser");
    }
    (vec![], "unknown")
}

// ---------- opening Chrome ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// A command to run: program and arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub prog: String,
    pub args: Vec<String>,
}

/// A Chrome profile folder name that is safe to pass on: "Default",
/// "Profile 3", a name Chrome made. Never a path.
pub fn valid_profile_dir(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 64
        && d != "."
        && d != ".."
        && d.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

/// The command that opens `url` in Chrome, in `profile` if given. None
/// when it cannot be done safely (a bad profile folder, or characters
/// `cmd` would read as syntax on Windows).
pub fn chrome_cmd(os: Os, url: &str, profile: Option<&str>) -> Option<Cmd> {
    if profile.is_some_and(|p| !valid_profile_dir(p)) || url.trim().is_empty() {
        return None;
    }
    let flag = profile.map(|p| format!("--profile-directory={p}"));
    let cmd = match os {
        // -n: a new instance, so the arguments reach a running Chrome too.
        Os::Mac => {
            let mut args = vec!["-na".into(), "Google Chrome".into(), "--args".into()];
            args.extend(flag);
            args.push(url.into());
            Cmd {
                prog: "open".into(),
                args,
            }
        }
        Os::Windows => {
            if url.contains(['&', '^', '|', '<', '>', '"', '%', '\n', '\r']) {
                return None;
            }
            let mut args: Vec<String> =
                vec!["/C".into(), "start".into(), "".into(), "chrome".into()];
            args.extend(flag);
            args.push(url.into());
            Cmd {
                prog: "cmd".into(),
                args,
            }
        }
        Os::Linux => {
            let mut args: Vec<String> = flag.into_iter().collect();
            args.push(url.into());
            Cmd {
                prog: "google-chrome".into(),
                args,
            }
        }
    };
    Some(cmd)
}

/// Programs tried for Chrome on Linux, in order.
#[allow(dead_code)]
const LINUX_CHROME: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
];

/// Commands run in tests instead of launching Chrome.
#[cfg(test)]
pub static LAUNCHED: std::sync::Mutex<Vec<Cmd>> = std::sync::Mutex::new(Vec::new());

/// Open `url` in Chrome (in `profile` if given). True when a command was
/// started. Tests record the command instead; `GODTERM_NO_OPEN` skips it.
pub fn open_chrome(url: &str, profile: Option<&str>) -> bool {
    let Some(cmd) = chrome_cmd(Os::current(), url, profile) else {
        return false;
    };
    #[cfg(test)]
    {
        LAUNCHED.lock().unwrap_or_else(|e| e.into_inner()).push(cmd);
        return true;
    }
    #[cfg(not(test))]
    {
        if std::env::var_os("GODTERM_NO_OPEN").is_some() {
            return true;
        }
        crate::log::info(&format!("browser: {} {}", cmd.prog, cmd.args.join(" ")));
        let run = |prog: &str| {
            std::process::Command::new(prog)
                .args(&cmd.args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
        };
        if Os::current() == Os::Linux {
            LINUX_CHROME.iter().any(|p| run(p))
        } else {
            run(&cmd.prog)
        }
    }
}

// ---------- the quick look ----------

/// What the dialog polls: the profiles' extension state (files only) and,
/// once some extension is on, `bsk browsers`.
#[derive(Debug, Clone)]
pub struct Quick {
    pub profiles: Vec<ProfileExt>,
    /// None: `bsk` is missing, not running or did not answer.
    pub browsers: Option<Vec<Browser>>,
}

pub fn quick(env: &Env, want_bsk: bool) -> Quick {
    let profiles = browser::chrome_extensions(&env.chrome_root);
    let browsers = if want_bsk {
        browser::find_bsk(&env.bin_dirs).and_then(|bin| {
            match browser::run_bsk(&bin, &["browsers", "--json"], env.timeout) {
                Ok((0, out)) => Some(browser::parse_browsers(&out)),
                _ => None,
            }
        })
    } else {
        None
    };
    Quick { profiles, browsers }
}

// ---------- the dialog ----------

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Pick,
    /// Open Chrome with this? Waits for the user's yes.
    Confirm(OpenPlan),
}

/// The Browser dialog of one tab.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserDlg {
    pub uid: u64,
    pub sel: usize,
    pub step: Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Dim,
    Good,
    Warn,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DlgLine {
    pub text: String,
    pub tone: Tone,
    pub action: Option<UiAction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DlgView {
    pub title: String,
    pub lines: Vec<DlgLine>,
    pub buttons: Vec<(String, UiAction, String)>,
}

fn line(text: impl Into<String>, tone: Tone) -> DlgLine {
    DlgLine {
        text: text.into(),
        tone,
        action: None,
    }
}

/// Only the folder name of a stored profile, so a hand edited config or
/// state cannot smuggle a path into a command.
fn clean(dir: &str) -> Option<String> {
    valid_profile_dir(dir).then(|| dir.to_string())
}

impl App {
    // ----- what is known -----

    fn tab_env(&self) -> Option<Env> {
        self.browser_env_pub()
    }

    /// Chrome profiles with the extension's state: the newest quick look,
    /// else the last full look.
    pub fn browser_profiles(&self) -> Vec<ProfileExt> {
        if let Some(q) = self.browser.quick_last_result() {
            return q.profiles;
        }
        self.browser
            .snapshot()
            .map(|s| s.profiles)
            .unwrap_or_default()
    }

    /// The browsers `bsk` reports connected (None: not known).
    pub fn browser_connected(&self) -> Option<Vec<Browser>> {
        if let Some(b) = self.browser.quick_last_result().and_then(|q| q.browsers) {
            return Some(b);
        }
        self.browser
            .snapshot()
            .and_then(|s| matches!(s.daemon, browser::Daemon::Running(_)).then_some(s.browsers))
    }

    pub fn tab_browser_dir(&self, uid: u64) -> Option<String> {
        let (s, t) = self.find_tab(uid)?;
        self.panes[s].tabs[t].browser.clone()
    }

    /// The tab's profile (from the last look), or just its folder name.
    pub fn tab_browser_profile(&self, uid: u64) -> Option<ChromeProfile> {
        let dir = self.tab_browser_dir(uid)?;
        Some(
            self.browser_profiles()
                .into_iter()
                .find(|p| p.profile.dir == dir)
                .map(|p| p.profile)
                .unwrap_or(ChromeProfile {
                    dir: dir.clone(),
                    name: dir,
                    ..Default::default()
                }),
        )
    }

    pub fn tab_browser_state(&self, uid: u64) -> BtnState {
        button_state(
            self.tab_browser_dir(uid).as_deref(),
            &self.browser_profiles(),
        )
    }

    /// The button's states and labels (longest first) for pane `s`'s
    /// current tab.
    pub fn browser_button(&self, s: usize) -> (BtnState, Vec<String>) {
        let uid = self.panes[s].cur().uid;
        let st = self.tab_browser_state(uid);
        let name = self
            .tab_browser_profile(uid)
            .map(|p| if p.name.is_empty() { p.dir } else { p.name })
            .unwrap_or_default();
        (st, button_labels(st, &name))
    }

    /// What `list_tabs` says about the tab's browser.
    pub fn tab_browser_json(&self, s: usize, t: usize) -> Option<Value> {
        let tab = &self.panes[s].tabs[t];
        tab.browser.as_ref()?;
        let p = self.tab_browser_profile(tab.uid)?;
        Some(json!({
            "profile_dir": p.dir, "profile": p.name, "email": p.user_name,
            "state": self.tab_browser_state(tab.uid).word(),
        }))
    }

    // ----- binding -----

    /// Bind (or clear) the tab's profile. Returns whether it changed.
    pub fn bind_tab_browser(&mut self, uid: u64, dir: Option<String>) -> bool {
        let dir = dir.and_then(|d| clean(&d));
        let Some((s, t)) = self.find_tab(uid) else {
            return false;
        };
        if self.panes[s].tabs[t].browser == dir {
            return false;
        }
        self.panes[s].tabs[t].browser = dir;
        self.state_dirty = true;
        true
    }

    /// Write `[browser] default_profile` (empty clears it).
    pub fn set_default_profile(&mut self, dir: &str) {
        let dir = clean(dir).unwrap_or_default();
        if self.cfg.browser.default_profile == dir {
            return;
        }
        self.cfg.browser.default_profile = dir.clone();
        let item = (!dir.is_empty()).then(|| toml_edit::value(dir.as_str()));
        if let Err(e) = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("browser.default_profile"),
            item,
        ) {
            crate::log::error(&format!("browser.default_profile: {e:#}"));
        }
        self.config_mtime = crate::app::config_mtime();
    }

    /// Settings > Browser: the next profile as the default for links (and
    /// none after the last).
    pub fn browser_default_cycle(&mut self) {
        let mut dirs: Vec<String> = vec![String::new()];
        dirs.extend(self.browser_profiles().into_iter().map(|p| p.profile.dir));
        let cur = &self.cfg.browser.default_profile;
        let i = dirs.iter().position(|d| d == cur).unwrap_or(0);
        let next = dirs[(i + 1) % dirs.len()].clone();
        self.set_default_profile(&next);
        let say = match self.profile_name(&next) {
            Some(n) => format!("Links open in Chrome profile {n}"),
            None => "Links open in the system browser".to_string(),
        };
        self.flash(say);
    }

    /// A profile's display name from its folder.
    pub fn profile_name(&self, dir: &str) -> Option<String> {
        if dir.is_empty() {
            return None;
        }
        Some(
            self.browser_profiles()
                .into_iter()
                .find(|p| p.profile.dir == dir)
                .map(|p| p.profile.display())
                .unwrap_or_else(|| dir.to_string()),
        )
    }

    // ----- links -----

    /// The Chrome profile a link opens in: the tab's own, else the
    /// default. Only a profile Chrome still has (when Chrome's folder can
    /// be read).
    pub fn link_profile(&self, tab_uid: Option<u64>) -> Option<String> {
        let own = tab_uid.and_then(|u| self.tab_browser_dir(u));
        let dir = own
            .or_else(|| Some(self.cfg.browser.default_profile.clone()))
            .and_then(|d| clean(&d))?;
        match self.tab_env() {
            Some(env) => {
                let text = std::fs::read_to_string(env.chrome_root.join("Local State"))
                    .unwrap_or_default();
                browser::parse_local_state(&text)
                    .iter()
                    .any(|p| p.dir == dir)
                    .then_some(dir)
            }
            None => Some(dir),
        }
    }

    /// Open a link in Chrome in the right profile. False: no profile is
    /// set (or it is gone), so the caller opens it the usual way.
    pub fn open_link_in_chrome(&self, url: &str, tab_uid: Option<u64>) -> bool {
        match self.link_profile(tab_uid) {
            Some(p) => open_chrome(url, Some(&p)),
            None => false,
        }
    }

    // ----- the dialog: opening, polling -----

    /// Open the Browser dialog for the tab shown in pane `s`.
    pub fn open_browser_dialog(&mut self, s: usize) {
        let Some(slot) = self.panes.get(s) else {
            return;
        };
        if let Some(why) = self.pane_harness(s).and_then(|h| h.no_tab_tools_reason()) {
            self.flash(why);
            return;
        }
        let uid = slot.cur().uid;
        self.focus = s;
        self.browser.note = None;
        let profiles = self.browser_profiles();
        let want = self
            .tab_browser_dir(uid)
            .or_else(|| Some(self.cfg.browser.default_profile.clone()));
        let sel = want
            .and_then(|d| profiles.iter().position(|p| p.profile.dir == d))
            .unwrap_or(0);
        self.modal = Modal::Browser(BrowserDlg {
            uid,
            sel,
            step: Step::Pick,
        });
        self.browser_quick(true);
        self.browser_refresh(false);
    }

    /// Start a quick look in the background (at most one at a time, and
    /// not more often than every few seconds unless `force`).
    pub fn browser_quick(&mut self, force: bool) {
        let Some(env) = self.tab_env() else {
            return;
        };
        if self.browser.quick_busy.load(Ordering::SeqCst) {
            return;
        }
        if !force
            && self
                .browser
                .quick_at
                .is_some_and(|t| t.elapsed() < POLL_EVERY)
        {
            return;
        }
        self.browser.quick_at = Some(Instant::now());
        self.browser.quick_busy.store(true, Ordering::SeqCst);
        // `bsk` is asked only once some profile has the extension on.
        let want_bsk = self
            .browser_profiles()
            .iter()
            .any(|p| ext_state(p) == ExtState::Enabled);
        let (slot, flag) = (self.browser.quick.clone(), self.browser.quick_busy.clone());
        std::thread::spawn(move || {
            let q = quick(&env, want_bsk);
            if let Ok(mut g) = slot.lock() {
                *g = Some(q);
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    fn dialog_open(&self) -> bool {
        matches!(self.modal, Modal::Browser(_))
    }

    /// Per tick: while the dialog is open or an enable is pending, look
    /// every few seconds; say when a pending tab is ready.
    pub fn browser_poll_tick(&mut self) {
        let waiting = !self.browser.pending.is_empty();
        // A tab with a browser keeps its button true: a look at start and
        // then about once a minute (files only, on a background thread).
        let bound = self
            .panes
            .iter()
            .any(|p| p.tabs.iter().any(|t| t.browser.is_some()));
        if bound
            && !waiting
            && !self.dialog_open()
            && self
                .browser
                .quick_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(60))
        {
            self.browser_quick(true);
        }
        if !self.dialog_open() && !waiting {
            return;
        }
        self.browser_quick(false);
        let pending = std::mem::take(&mut self.browser.pending);
        for (uid, since) in pending {
            let Some((s, t)) = self.find_tab(uid) else {
                continue;
            };
            if self.panes[s].tabs[t].browser.is_none() || since.elapsed() > PENDING_FOR {
                continue;
            }
            if self.tab_browser_state(uid) != BtnState::Ready {
                self.browser.pending.push((uid, since));
                continue;
            }
            let name = self.panes[s].tabs[t].name();
            let who = self
                .tab_browser_profile(uid)
                .map(|p| p.display())
                .unwrap_or_default();
            let more = match self.browser_connected() {
                Some(b) if !b.is_empty() => "",
                _ => " Open the extension popup and turn on the local connection.",
            };
            self.browser.note = Some(format!("Ready: the extension is on in {who}.{more}"));
            self.flash(format!(
                "Browser ready for {name}: Chrome profile {who}.{more}"
            ));
        }
    }

    // ----- the dialog: keys and clicks -----

    pub fn on_browser_key(&mut self, k: KeyEvent) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        match d.step {
            Step::Confirm(plan) => match k.code {
                KeyCode::Enter | KeyCode::Char('y') => self.browser_open_run(plan),
                _ => self.modal = Modal::None,
            },
            Step::Pick => {
                let n = self.browser_profiles().len();
                let set = |app: &mut App, sel: usize| {
                    app.modal = Modal::Browser(BrowserDlg { sel, ..d.clone() })
                };
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Up | KeyCode::Char('k') => set(self, d.sel.saturating_sub(1)),
                    KeyCode::Down | KeyCode::Char('j') => {
                        set(self, (d.sel + 1).min(n.saturating_sub(1)))
                    }
                    KeyCode::Enter => self.browser_choose(d.sel),
                    KeyCode::Char('c') => self.browser_clear(),
                    KeyCode::Char('r') => self.browser_quick(true),
                    _ => {}
                }
            }
        }
    }

    /// The user picks profile row `i`: bind it, make it the default for
    /// links, then say or offer what is still missing.
    pub fn browser_choose(&mut self, i: usize) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        let profiles = self.browser_profiles();
        let Some(p) = profiles.get(i) else {
            return;
        };
        let dir = p.profile.dir.clone();
        let who = p.profile.display();
        self.bind_tab_browser(d.uid, Some(dir.clone()));
        self.set_default_profile(&dir);
        self.browser.note = None;
        match plan_for(ext_state(p)) {
            None => {
                self.modal = Modal::Browser(BrowserDlg {
                    sel: i,
                    step: Step::Pick,
                    ..d
                });
                self.browser.note = Some(match self.browser_connected() {
                    Some(b) if !b.is_empty() => format!("Ready: this tab uses {who}."),
                    _ => format!(
                        "This tab uses {who}. Open the extension popup there and turn on the local connection."
                    ),
                });
                self.browser_quick(true);
                self.flash(format!("Browser for this tab: Chrome profile {who}"));
            }
            Some(plan) => {
                self.modal = Modal::Browser(BrowserDlg {
                    sel: i,
                    step: Step::Confirm(plan),
                    ..d
                });
            }
        }
    }

    pub fn browser_clear(&mut self) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        if self.bind_tab_browser(d.uid, None) {
            self.browser.pending.retain(|(u, _)| *u != d.uid);
            self.browser.note = Some("This tab has no browser now.".into());
            self.flash("Browser cleared for this tab");
        }
    }

    /// The user said yes: open Chrome in the tab's profile.
    pub fn browser_open_run(&mut self, plan: OpenPlan) {
        let Modal::Browser(d) = self.modal.clone() else {
            return;
        };
        let Some(dir) = self.tab_browser_dir(d.uid) else {
            self.modal = Modal::None;
            return;
        };
        let who = self.profile_name(&dir).unwrap_or_else(|| dir.clone());
        let ok = open_chrome(plan.url(), Some(&dir));
        self.modal = Modal::Browser(BrowserDlg {
            step: Step::Pick,
            ..d.clone()
        });
        if !ok {
            self.browser.note = Some("Could not open Chrome.".into());
            return;
        }
        self.browser.pending.retain(|(u, _)| *u != d.uid);
        self.browser.pending.push((d.uid, Instant::now()));
        self.browser.note = Some(match plan {
            OpenPlan::Store => format!(
                "Opened the Chrome Web Store in {who}. Press Add to Chrome there (Chrome needs your click), then turn the extension on. GodTerm looks every few seconds."
            ),
            OpenPlan::Extensions => format!(
                "Opened chrome://extensions in {who}. Turn BrowserSkill on there. GodTerm looks every few seconds."
            ),
        });
        self.browser_quick(true);
    }

    // ----- the dialog: what it shows -----

    pub fn browser_dialog_view(&self, d: &BrowserDlg) -> Option<DlgView> {
        let (s, t) = self.find_tab(d.uid)?;
        let tab = self.panes[s].tabs[t].name();
        let title = format!("browser for {}", crate::sessions::snippet(&tab, 30));
        let who = self
            .tab_browser_dir(d.uid)
            .and_then(|dir| self.profile_name(&dir));
        if let Step::Confirm(plan) = d.step {
            let who = who.unwrap_or_default();
            let mut lines = match plan {
                OpenPlan::Store => vec![
                    line(format!("Open the Chrome Web Store page of BrowserSkill in {who}?"), Tone::Normal),
                    line("Chrome makes you press Add to Chrome yourself; GodTerm cannot do that for you.", Tone::Warn),
                ],
                OpenPlan::Extensions => vec![
                    line(format!("Open chrome://extensions in {who}?"), Tone::Normal),
                    line("Turn BrowserSkill on there; GodTerm cannot do that for you.", Tone::Warn),
                ],
            };
            lines.push(line(
                "Then open the extension's popup and turn on its local connection. GodTerm looks every few seconds and marks this tab ready.",
                Tone::Dim,
            ));
            return Some(DlgView {
                title,
                lines,
                buttons: vec![
                    (
                        "Open Chrome".into(),
                        UiAction::ModalOk,
                        "Open it in that profile (Enter)".into(),
                    ),
                    (
                        "Not now".into(),
                        UiAction::ModalCancel,
                        "The profile stays chosen (Esc)".into(),
                    ),
                ],
            });
        }
        let profiles = self.browser_profiles();
        let mut lines = vec![line(
            "Chrome profile for this tab. Links open there too.",
            Tone::Dim,
        )];
        if profiles.is_empty() {
            lines.push(line(
                "No Chrome profiles found. Is Chrome installed and used once?",
                Tone::Warn,
            ));
        }
        let bound = self.tab_browser_dir(d.uid);
        for (i, p) in profiles.iter().enumerate() {
            let this = bound.as_deref() == Some(p.profile.dir.as_str());
            let text = format!(
                "{} {:<24} {}{}",
                if i == d.sel { "›" } else { " " },
                crate::sessions::snippet(&p.profile.display(), 24),
                p.word(),
                if this { "  (this tab)" } else { "" }
            );
            lines.push(DlgLine {
                text,
                tone: match ext_state(p) {
                    ExtState::Enabled => Tone::Good,
                    ExtState::Disabled => Tone::Warn,
                    ExtState::Missing => Tone::Normal,
                },
                action: Some(UiAction::BrowserRow(i)),
            });
        }
        let st = self.tab_browser_state(d.uid);
        let w = who.unwrap_or_default();
        let (msg, tone) = match st {
            BtnState::None => ("Pick a profile.".to_string(), Tone::Dim),
            BtnState::Chosen => (format!("BrowserSkill is not in {w} yet."), Tone::Warn),
            BtnState::NeedsEnable => (
                format!("BrowserSkill is in {w} but turned off."),
                Tone::Warn,
            ),
            BtnState::Ready => (format!("Ready: this tab's agent can use {w}."), Tone::Good),
        };
        lines.push(line("", Tone::Dim));
        lines.push(line(msg, tone));
        if st == BtnState::Ready {
            let conn = match self.browser_connected() {
                Some(b) if !b.is_empty() => format!("bsk sees {} connected.", crate::control::plural(b.len(), "browser")),
                Some(_) => "bsk sees no connected browser: turn on the local connection in the extension popup.".to_string(),
                None => "bsk is not running or not installed (Settings > Browser).".to_string(),
            };
            lines.push(line(conn, Tone::Dim));
            lines.push(line(
                "Tip: set the extension's Browser name to the profile name so agents can tell browsers apart.",
                Tone::Dim,
            ));
        }
        if let Some(n) = &self.browser.note {
            lines.push(line(n.clone(), Tone::Normal));
        }
        Some(DlgView {
            title,
            lines,
            buttons: vec![
                (
                    "Use profile".into(),
                    UiAction::ModalOk,
                    "Choose the selected profile for this tab (Enter)".into(),
                ),
                (
                    "No browser".into(),
                    UiAction::BrowserClear,
                    "Clear this tab's browser (c)".into(),
                ),
                ("Close".into(), UiAction::ModalCancel, "Esc".into()),
            ],
        })
    }

    // ----- the tab's own tools -----

    /// `list_browsers` and `browser_status` for the agent of tab `uid`:
    /// the tab's bound profile and nothing of any other tab's.
    pub(crate) fn browser_tool_for(&mut self, uid: u64, tool: &str) -> Result<Value, String> {
        let base = self.browser_tool(tool)?;
        let Some(dir) = self.tab_browser_dir(uid) else {
            let mut v = base;
            if v["result"].is_object() {
                v["result"]["bound_profile"] = Value::Null;
                v["result"]["say"] = json!("No browser is set for this tab. Ask the user to click Browser on this tab's bottom bar (or Ctrl-a i) to choose a Chrome profile; do not pick one yourself.");
            }
            return Ok(v);
        };
        let Some(snap) = self.browser.snapshot() else {
            return Ok(base);
        };
        let profiles = self.browser_profiles();
        let profiles = if profiles.is_empty() {
            snap.profiles.clone()
        } else {
            profiles
        };
        let mine = profiles.iter().find(|p| p.profile.dir == dir);
        let profile = mine.map(|p| p.profile.clone()).unwrap_or(ChromeProfile {
            dir: dir.clone(),
            name: dir.clone(),
            ..Default::default()
        });
        let state = button_state(Some(&dir), &profiles);
        let enabled = profiles
            .iter()
            .filter(|p| ext_state(p) == ExtState::Enabled)
            .count();
        let all = self
            .browser_connected()
            .unwrap_or_else(|| snap.browsers.clone());
        let (matched, how) = match_browsers(&all, &profile, enabled, state == BtnState::Ready);
        let bound = json!({
            "profile_dir": profile.dir, "name": profile.name, "email": profile.user_name,
            "extension": mine.map(ProfileExt::word).unwrap_or("not found"),
            "state": state.word(),
        });
        let say = match (state, matched.is_empty()) {
            (BtnState::Ready, false) => format!(
                "Use only this browser: `bsk session start --browser {}`.",
                matched[0].instance_id
            ),
            (BtnState::Ready, true) if all.is_empty() => "The extension is on in this tab's profile but bsk sees no connected browser. Ask the user to turn on the local connection in the extension popup.".to_string(),
            (BtnState::Ready, true) => format!(
                "bsk cannot tell which connected browser is this tab's profile. Ask the user to set the extension's Browser name to \"{}\" and call again. Do not use another browser.",
                profile.name
            ),
            _ => "This tab's profile is not ready: the extension is missing or off there. Ask the user to finish in the Browser dialog of this tab. Do not use another browser.".to_string(),
        };
        let mut result = if tool == "list_browsers" {
            json!({
                "daemon": snap.daemon.word(),
                "bound_profile": bound,
                "browsers": matched.iter().map(Browser::to_json).collect::<Vec<_>>(),
                "count": matched.len(),
                "match": how,
                "checked_secs_ago": snap.age_secs(),
            })
        } else {
            let mut v = snap.to_json();
            v["chrome_profiles"] = json!(mine.map(|p| vec![p.to_json()]).unwrap_or_default());
            v["browsers"] = json!(matched.iter().map(Browser::to_json).collect::<Vec<_>>());
            v["bound_profile"] = bound;
            v["match"] = json!(how);
            v
        };
        result["say"] = json!(say);
        result["extension_id"] = json!(EXTENSION_ID);
        Ok(json!({"ok": true, "result": result}))
    }
}
