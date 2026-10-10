//! Application state, event handling and background workers.

use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;
use std::fs;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{valid_name, AccountCfg, Config};
use crate::creds::{self, CredSource, Profile};
use crate::keys;
use crate::pane::{Activity, LaunchKind, LaunchSpec, PaneState};
use crate::picker::{PickAction, Picker};
use crate::sessions::{SessionCache, SessionInfo};
use crate::slot::Slot;
use crate::state::{self, AppState, SlotState, TabState};
use crate::theme;
use crate::usage::{self, Usage, UsageError};

pub enum AppEvent {
    /// A user turn from an external control client (the socket's
    /// user_says): what the user said, so a confirmation can follow.
    UserTurn(String),
    Input(Event),
    PaneOutput,
    /// (tab uid, generation)
    PaneExited(u64, u64),
    Status(usize, Box<StatusSnapshot>),
    /// The main ~/.grok's usage (the Grok login picker).
    MainGrokUsage(Box<Result<crate::usage::Usage, String>>),
    /// A key as the input thread read it: `burst` when it came right
    /// after the previous key (a paste the terminal typed out), timed
    /// when read, not when handled (a busy frame bunches keys up).
    KeyRead(crossterm::event::KeyEvent, bool),
    /// A provider's own login's usage (the assistant's Grok).
    OwnUsage(&'static str, Box<Result<crate::usage::Usage, String>>),
    Sessions(usize, Vec<SessionInfo>),
    Voice(crate::voice::VoiceEvent),
    /// The window's current bounds (from the terminal app).
    Window(Option<crate::window::SavedWindow>),
    /// Fresh memory figures for the Settings screen.
    Mem(crate::app_settings::MemStats),
    /// The terminal closed (stdin ended).
    InputClosed,
    /// Loops found in running tabs.
    Loops(Vec<crate::app_loops::LoopRow>),
    /// A control API call (tool, arguments) and where the reply goes.
    Control(
        String,
        serde_json::Value,
        std::sync::mpsc::Sender<serde_json::Value>,
    ),
    /// The assistant's process said something.
    Assistant(u64, crate::assistant::BrainEvent),
}

/// Login state as derived from stored credentials (no secrets kept here).
#[derive(Debug, Clone, Default)]
pub struct LoginInfo {
    /// Login is handled by the CLI; GodTerm has no credential adapter.
    pub cli_managed: bool,
    pub source: Option<CredSource>,
    pub expires_at: Option<i64>,
    pub has_refresh: bool,
    pub subscription: Option<String>,
    pub rate_tier: Option<String>,
}

impl LoginInfo {
    pub fn can_start(&self) -> bool {
        self.cli_managed || self.logged_in()
    }
    pub fn logged_in(&self) -> bool {
        self.source.is_some()
    }
    pub fn expired(&self) -> bool {
        matches!(self.expires_at, Some(t) if t <= Utc::now().timestamp_millis())
    }
}

#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub profile: Profile,
    pub login: LoginInfo,
    /// None when usage was not requested in this refresh.
    pub usage: Option<Result<Usage, UsageError>>,
}

#[derive(Default)]
pub struct AccountState {
    pub profile: Profile,
    pub login: LoginInfo,
    /// Last good usage reply, kept across errors.
    pub usage: Option<Usage>,
    /// When `usage` was fetched.
    pub usage_good_at: Option<DateTime<Utc>>,
    /// Error from the latest attempt, cleared by the next success.
    pub usage_err: Option<UsageError>,
    /// When the latest attempt (good or bad) finished.
    pub usage_at: Option<DateTime<Utc>>,
    /// A usage fetch is in flight.
    pub refreshing: bool,
    /// Any status snapshot is in flight.
    inflight: bool,
    last_fetch: Option<Instant>,
    next_fetch: Option<Instant>,
    backoff: u32,
    pub sessions: Vec<SessionInfo>,
    pub sessions_loading: bool,
    pub sessions_loaded: bool,
    pub cache: Arc<Mutex<SessionCache>>,
    /// grok's own on screen "Weekly limit left: N%": (label, left, when).
    pub grok_screen: Option<(String, f64, DateTime<Utc>)>,
    /// Where the usage shown came from, when not the API ("grok status").
    pub usage_note: Option<String>,
    /// Recent 5 hour % left samples (capped at `usage_history`).
    pub usage_hist: std::collections::VecDeque<(DateTime<Utc>, f64)>,
}

/// Never ask the usage API about one account more often than this.
pub const MIN_FETCH_INTERVAL: Duration = Duration::from_secs(60);

/// The interval in use: demo mode reads fixtures, every 2 s.
pub fn min_fetch_interval() -> Duration {
    if crate::demo::active() {
        Duration::from_secs(2)
    } else {
        MIN_FETCH_INTERVAL
    }
}

impl AccountState {
    /// Percent left in the 5 hour window, from the last good reply.
    pub fn five_hour_left(&self) -> Option<f64> {
        self.usage
            .as_ref()
            .and_then(|u| u.get("five_hour"))
            .map(|w| w.left_now())
    }

    /// The 5 hour window reset since the numbers were fetched.
    pub fn five_hour_reset_passed(&self) -> bool {
        self.usage
            .as_ref()
            .and_then(|u| u.get("five_hour"))
            .is_some_and(|w| w.reset_passed())
    }

    /// What is really left: the lower of the 5 hour and weekly windows,
    /// which one binds ("5h" or "weekly"), and when that one resets.
    pub fn binding(&self) -> Option<(f64, &'static str, Option<chrono::DateTime<chrono::Utc>>)> {
        let u = self.usage.as_ref()?;
        // A window whose reset time has passed is full again, even while
        // the fetch that would say so fails (kept numbers would call the
        // account out after it reset).
        let left = |w: &crate::usage::Window| w.left_now();
        let f = u.get("five_hour").map(|w| (left(w), "5h", w.resets_at));
        let w = u.get("seven_day").map(|w| (left(w), "weekly", w.resets_at));
        match (f, w) {
            (Some(a), Some(b)) => Some(if b.0 < a.0 { b } else { a }),
            (a, b) => a.or(b),
        }
    }

    /// What to say about the last fetch failing, if anything: a 429 is
    /// only worth a word once the numbers shown are old (twice the fetch
    /// interval), and then calmly.
    pub fn usage_problem(&self, interval: Duration, now: DateTime<Utc>) -> Option<String> {
        self.usage_problem_as(interval, now, false)
    }

    /// `short`: for a pane's footer ("usage from 3 min ago").
    pub fn usage_problem_as(
        &self,
        interval: Duration,
        now: DateTime<Utc>,
        short: bool,
    ) -> Option<String> {
        let e = self.usage_err.as_ref()?;
        let ago = self.usage_good_at.map(|t| crate::usage::ago_words(t, now));
        match e {
            UsageError::RateLimited(_) => {
                let t = self.usage_good_at?;
                let stale = (now - t).to_std().unwrap_or_default() > interval * 2;
                let ago = ago.unwrap_or_default();
                stale.then(|| match short {
                    true => format!("usage from {ago} ago"),
                    false => format!("usage from {ago} ago ({e})"),
                })
            }
            _ if short => Some(match ago {
                Some(a) => format!("{}, values from {a} ago", e.short()),
                None => e.short(),
            }),
            _ => Some(match ago {
                Some(a) => format!("{e}, showing data from {a} ago"),
                None => e.to_string(),
            }),
        }
    }

    /// Percent really left (see `binding`).
    pub fn effective_left(&self) -> Option<f64> {
        self.binding().map(|b| b.0)
    }

    /// Time until a usage fetch is allowed again (zero if allowed now).
    pub fn fetch_wait(&self) -> Duration {
        let now = Instant::now();
        let min = self
            .last_fetch
            .map(|t| (t + min_fetch_interval()).saturating_duration_since(now))
            .unwrap_or_default();
        let next = self
            .next_fetch
            .map(|t| t.saturating_duration_since(now))
            .unwrap_or_default();
        min.max(next)
    }

    /// Record the outcome of a fetch and schedule the next one.
    pub fn record_usage(&mut self, res: Result<Usage, UsageError>, refresh_secs: u64) {
        let now = Instant::now();
        let base = Duration::from_secs(refresh_secs).max(min_fetch_interval());
        self.refreshing = false;
        self.usage_at = Some(Utc::now());
        let delay = match res {
            Ok(u) => {
                if let Some(l) = u.get("five_hour").map(|w| w.left()) {
                    self.usage_hist.push_back((Utc::now(), l));
                    if self.usage_hist.len() > 1000 {
                        self.usage_hist.pop_front();
                    }
                }
                self.usage = Some(u);
                self.usage_good_at = Some(Utc::now());
                self.usage_err = None;
                self.backoff = 0;
                crate::usage_share::jitter(base)
            }
            Err(e) => {
                let d = match &e {
                    UsageError::RateLimited(retry) => {
                        self.backoff = (self.backoff + 1).min(8);
                        crate::usage_share::jitter(crate::usage_share::backoff(
                            self.backoff,
                            *retry,
                        ))
                    }
                    _ => crate::usage_share::jitter(base),
                };
                self.usage_err = Some(e);
                d
            }
        };
        self.next_fetch = Some(now + delay);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Grid,
    Dashboard,
    Sessions,
    Overview,
    Settings,
    Loops,
    /// Every tab opened and closed (View ▾ > Tab history).
    TabHistory,
    /// The assistant's learned rules and their history.
    Learned,
    /// Every session as an animated diagram (View ▾ > Live map).
    LiveMap,
    /// The assistant's system prompt: sections, edits, history.
    Prompt,
}

/// What the Add account dialog asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAccount {
    pub label: String,
    /// "claude" or "grok".
    pub harness: String,
    pub color: String,
    pub cwd: String,
    pub permission_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// The Grok login picker: the selected card.
    GrokLogins(usize),
    /// A tab group header's menu: (pane, group, selected item).
    GroupMenu(usize, usize, usize),
    /// Tab menu > Move to group: (pane, tab, selected choice).
    GroupPick(usize, usize, usize),
    /// Naming a new group (with these tabs) or renaming one.
    GroupName {
        slot: usize,
        group: Option<usize>,
        tabs: Vec<u64>,
        buf: String,
    },
    /// Close every tab of a group? (pane, group)
    ConfirmCloseGroup(usize, usize),
    /// Move a group to which account: (pane, group, selected account).
    GroupMove(usize, usize, usize),
    /// Stop every loop?
    ConfirmStopLoops,
    /// Menu bar buttons that did not fit.
    MenuOverflow,
    None,
    Help,
    /// What is new since this version (scrolled this far), and the update.
    Updates(u16),
    /// "Restart to update?": what comes back, what is interrupted.
    UpdateRestart,
    ConfirmQuit,
    ConfirmClose,
    AddAccount(Box<crate::add_account::AddForm>),
    /// Close this pane's window (all its tabs)?
    ConfirmCloseWindow(usize),
    /// "New tab on… ▾" of an empty grid cell.
    EmptySlotMenu(usize),
    /// Picking a color for an account or a tab: the selected choice.
    ColorPick(crate::color_pick::Target, usize),
    NewTab(Picker),
    Palette(crate::palette::Palette),
    /// Prompt text to send to the marked tabs (or every running tab).
    Broadcast(String),
    /// Every tab waiting for approval; the selected row.
    Approvals(usize),
    /// Confirm switching account `.1` (shown in pane `.0`) to bypass mode.
    ConfirmBypass(usize, usize),
    /// Offer to restart pane `.0`'s tab so a new permission mode applies.
    OfferRestart(usize),
    /// Edit a text setting: (row in the current section, text).
    EditSetting(usize, String),
    /// Confirm removing account `.0` from config.toml.
    ConfirmRemoveAccount(usize),
    /// Rename a tab: (pane, tab, text typed so far).
    Rename(usize, usize, String),
    /// Account menu of a pane: (pane, selected row).
    AccountMenu(usize, usize),
    /// First launch walkthrough, at this step.
    Tour(usize),
    /// Wake word training (state in `voice.training`).
    WakeTrain,
    /// "Move tab to..." picker.
    MoveTab(crate::app_tabmove::MovePicker),
    /// The tab to move is busy: wait, interrupt, or cancel.
    MoveBusy(crate::app_tabmove::QueuedMove),
    /// Right click menu of a tab: (pane, tab, selected row).
    TabMenu(usize, usize, usize),
    /// A pane header folder's menu: Finder, copy, terminal.
    PathMenu(usize),
    /// A menu bar dropdown.
    Menu(crate::menus::Open),
    /// Bring a session running elsewhere here: take over or copy.
    TakeOver(String),
    /// Sessions: pick where to copy / move to (selected row).
    SessTarget(usize),
    /// Sessions: confirm a move, or a write into the main ~/.claude.
    SessConfirm,
    /// Sessions: the target has the session already: skip, overwrite, new id.
    SessConflict(usize),
}

/// Progress of the first run login walk through.
#[derive(Debug, Clone, PartialEq)]
pub struct Onboard {
    /// Accounts to log in, in order.
    pub queue: Vec<usize>,
    pub pos: usize,
    /// Accounts the user skipped.
    pub skipped: Vec<usize>,
}

/// A state change worth telling the user about (used by voice alerts).
#[derive(Debug, Clone, PartialEq)]
pub struct Attention {
    pub slot: usize,
    pub tab: usize,
    pub what: Activity,
}

pub struct App {
    pub cfg: Config,
    pub accounts: Vec<AccountState>,
    pub panes: Vec<Slot>,
    pub focus: usize,
    pub zoom: bool,
    pub view: View,
    pub modal: Modal,
    pub prefix: bool,
    pub prefix_at: Option<Instant>,
    /// Selected account in dashboard and sessions views.
    pub sel_account: usize,
    pub sel_session: usize,
    /// Sessions view: source chip, search, marks, pending copy / move,
    /// the last move (for undo), and the main ~/.claude listing.
    pub sess_source: crate::app_sessions::SourceSel,
    pub sess_filter: String,
    pub sess_searching: bool,
    pub sess_marked: std::collections::HashSet<std::path::PathBuf>,
    pub sess_op: Option<crate::app_sessions::SessOp>,
    pub last_moves: Vec<crate::session_ops::Moved>,
    pub main_sessions: Vec<SessionInfo>,
    pub main_grok_sessions: Vec<SessionInfo>,
    /// Sessions view filters: one harness only; subagent rows too.
    pub sess_harness: Option<u8>,
    pub sess_subagents: bool,
    /// Sessions view: sort column, reversed, grouping, headless shown.
    pub sess_sort: crate::sess_sort::SortKey,
    pub sess_sort_rev: bool,
    pub sess_group: crate::sess_sort::Group,
    pub sess_headless: bool,
    pub sess_sub_rows: Vec<(crate::app_sessions::Src, SessionInfo)>,
    pub main_cache: Arc<std::sync::Mutex<crate::sessions::SessionCache>>,
    pub main_loading: bool,
    /// Tab moves: waiting for idle, started (until the target is up), and
    /// the last one for undo.
    pub queued_moves: Vec<crate::app_tabmove::QueuedMove>,
    /// Loops: scanner cache, what is active, selection, stop requests.
    pub loop_cache: Arc<std::sync::Mutex<crate::loops::LoopCache>>,
    /// The assistant, its turn counter (confirmation tokens must predate
    /// the current turn), actions this turn, and pending confirmations.
    pub assistant: crate::app_assistant::AssistantState,
    pub assistant_turn: u64,
    pub assistant_calls: usize,
    pub pending_confirms: Vec<crate::control::Pending>,
    /// Confirmation tokens a newer question replaced: (old, new).
    pub superseded: Vec<(String, String)>,
    /// A system_check running off the UI thread, for the reply that waits.
    pub pending_job: Option<std::sync::mpsc::Receiver<serde_json::Value>>,
    /// Prompts waiting for their tab to be ready, and tool replies waiting
    /// for a new tab.
    /// Prompts on their way into tabs (followed until claude takes them).
    pub deliveries: Vec<crate::control::Delivery>,
    /// Pastes whose Enter waits for the tab to show them (voice "send",
    /// a login code).
    pub pending_enters: Vec<crate::control::PendingEnter>,
    pub delivery_seq: u64,
    /// The tab the assistant last used ("there", "that tab").
    pub last_target: Option<u64>,
    pub ctl_waits: Vec<crate::control::Waiting>,
    pub loops: Vec<crate::app_loops::LoopRow>,
    pub loops_scanning: bool,
    pub loops_scanned: Instant,
    pub sel_loop: usize,
    pub stop_reqs: Vec<crate::app_loops::StopReq>,
    pub pending_moves: Vec<crate::app_tabmove::PendingMove>,
    /// Every session on the Mac, for the assistant's sessions tool.
    pub session_index: crate::session_index::Shared,
    /// Sessions being brought here from other terminals.
    pub takeovers: Vec<crate::takeover::TakeOver>,
    /// Sessions running in other terminals (for the Sessions view), and when looked.
    pub sess_live: std::collections::HashMap<String, crate::takeover::Live>,
    pub sess_live_at: Option<Instant>,
    /// What open_path opened (for tests and the log).
    pub opened_paths: Vec<String>,
    /// Tab ids that moved, old to new (so plans and queued prompts follow).
    pub moved_ids: std::collections::HashMap<u64, u64>,
    pub last_tab_move: Option<crate::app_tabmove::DoneMove>,
    pub sel_overview: usize,
    /// Background tabs that just started waiting or finished.
    pub attention: Vec<Attention>,
    /// Runtime email toggles (persisted in state.json).
    pub rt_show_email: Option<bool>,
    /// Memory saver toggled at runtime (persisted), and its last tick.
    pub rt_mem_saver: Option<bool>,
    pub mem_ticked: Instant,
    /// Layout picked at runtime (Ctrl-a L), dragged sizes, and the last
    /// frame's arrangement (pages, borders) and main area.
    pub rt_layout: Option<String>,
    pub ratios: crate::layout::RatioMap,
    pub last_arranged: Option<crate::layout::Arranged>,
    pub main_area: ratatui::layout::Rect,
    /// A pane border being dragged.
    pub dragging: Option<crate::layout::Border>,
    /// Dragging the edge of this pane's tab list.
    pub sidebar_drag: Option<usize>,
    /// Dragging pane `.0`'s header (moved: past the first cell).
    pub pane_drag: Option<(usize, bool)>,
    /// When output last arrived (the latest 40 wake ups), to tell a steady
    /// stream from an occasional update.
    pub output_at: std::collections::VecDeque<Instant>,
    /// Recently closed tabs (newest last) and the Undo toast.
    pub closed: Vec<crate::closed::ClosedTab>,
    pub closed_seq: u64,
    /// The user said yes to opening a tab in their home folder (set only
    /// while that confirmed plan runs).
    pub open_home_ok: bool,
    /// Names the tools matched for a misheard one this call: (said, used).
    pub corrections: std::cell::RefCell<Vec<(String, String)>>,
    /// The Tab history view.
    pub th: crate::app_tabhist::TabHistUi,
    /// The Learned rules view.
    pub learned_ui: crate::app_learned::LearnedUi,
    /// The Live map view.
    pub livemap: crate::livemap::LiveMap,
    pub prompt_ui: crate::app_sysprompt::PromptUi,
    /// Sessions a take-over had to stop by force (Windows TerminateProcess).
    pub forced_stops: Vec<String>,
    /// The main ~/.grok's usage, fetched for the Grok login picker.
    pub main_grok_usage: Option<Result<crate::usage::Usage, String>>,
    /// Usage of the providers with a login of their own, by provider id.
    pub own_usage: std::collections::HashMap<&'static str, Result<crate::usage::Usage, String>>,
    pub own_usage_at: Option<Instant>,
    /// Background work per tab (uid): when looked, what runs (2 s cache).
    pub bg_cache: std::cell::RefCell<std::collections::HashMap<u64, (Instant, Option<String>)>>,
    /// A tab being dragged in its list: (pane, tab, moved yet).
    pub tab_drag: Option<(usize, usize, bool)>,
    pub undo_toast: Option<(Instant, String)>,
    /// Tab list width preset (Tabs ▾ > Tab list width): narrow, normal, wide.
    pub tab_list_width: Option<String>,
    /// Ctrl-a ' then digits: a pane number being typed.
    pub num_entry: Option<String>,
    pub rt_privacy: Option<bool>,
    /// Eager restore: tabs still to start, (pane, tab uid), one at a time.
    pub restore_queue: std::collections::VecDeque<(usize, u64)>,
    restore_next: Instant,
    /// (tabs, accounts) started by the eager restore so far.
    restore_done: (usize, std::collections::BTreeSet<usize>),
    /// Tabs marked in the overview for broadcast (by uid).
    pub marked: std::collections::HashSet<u64>,
    /// Last known terminal window bounds (saved in state.json).
    pub window: Option<crate::window::SavedWindow>,
    last_window_check: Instant,
    /// Recently used folders (New Tab dialog), saved in state.json.
    pub recents: Vec<crate::picker::RecentDir>,
    pub flash: Option<(String, Instant)>,
    pub quit: bool,
    /// Where the terminal cursor goes this frame (one place at most: the
    /// focused text input); set while drawing, applied at the end.
    pub want_cursor: std::cell::Cell<Option<(u16, u16)>>,
    /// Self update: the checker, the chip, restart to update.
    pub update: crate::app_update::UpdateUi,
    /// The assistant's admin flows (installs, logins) and their log.
    pub admin: crate::app_admin::AdminState,
    /// What is installed and the installs running (Settings > Setup).
    pub setup: crate::app_setup::SetupState,
    /// Restore every tab at once this start (after a restart to update).
    pub force_eager: bool,
    pub pane_rects: Vec<Rect>,
    /// Terminal area of each pane (inside the border, above the footer).
    pub term_rects: Vec<Rect>,
    /// The assistant panel floats over the panes this frame.
    pub panel_overlay: std::cell::Cell<bool>,
    /// How far the Updates window can scroll (set when drawn).
    pub updates_max_scroll: std::cell::Cell<u16>,
    /// The menu bar is on screen (the update chip lives there, else in the
    /// status bar).
    pub menu_shown: std::cell::Cell<bool>,
    /// Moves on offer for tabs whose account runs out (app_failover).
    pub failover: Vec<crate::app_failover::Offer>,
    /// (tab uid, account) the user said no to.
    pub failover_declined: Vec<(u64, usize)>,
    /// The key being handled came in a burst (see AppEvent::KeyRead).
    pub key_burst: Option<bool>,
    /// Clickable regions being registered by the frame being drawn.
    pub hits: std::cell::RefCell<crate::hits::Hits>,
    /// Index in `hits` where the open modal's regions begin.
    pub modal_from: std::cell::Cell<Option<usize>>,
    /// The tab being renamed was drawn in its sidebar row this frame (so
    /// no popup is needed).
    pub inline_rename: std::cell::Cell<bool>,
    /// Menu bar buttons that did not fit this frame.
    pub menu_overflow: std::cell::RefCell<Vec<(String, crate::hits::UiAction)>>,
    /// Regions of the last drawn frame: what the user can click.
    pub last_hits: crate::hits::Hits,
    pub last_modal_from: Option<usize>,
    /// Last mouse position (for hover highlights and hints).
    pub mouse_pos: Option<(u16, u16)>,
    pub last_click: Option<(Instant, crate::hits::UiAction)>,
    /// Text selected by dragging in a pane or the assistant (app_select).
    pub selection: Option<crate::select::Selection>,
    /// The last press in selectable text, for double and triple clicks.
    pub sel_press: Option<crate::select::Press>,
    /// The assistant's conversation as drawn: its area and lines.
    pub assistant_text: std::cell::RefCell<Option<(Rect, Vec<String>)>>,
    /// What the last copy put on the clipboard.
    pub last_copied: Option<String>,
    /// Mouse capture on (clickable UI) or released for text selection.
    pub mouse_capture: bool,
    pub tx: Sender<AppEvent>,
    pub voice: crate::app_voice::VoiceState,
    /// config.toml mtime last loaded, for hot reload.
    pub(crate) config_mtime: Option<std::time::SystemTime>,
    last_config_check: Instant,
    /// Parse error in config.toml (the old config stays in use).
    pub config_error: Option<String>,
    /// Settings screen: selected section and row.
    pub settings_section: usize,
    pub settings_sel: usize,
    /// Settings search (`/`): matches across every section.
    pub settings_query: Option<String>,
    /// Settings headers folded (Advanced and debugging starts so).
    pub settings_collapsed: std::collections::HashSet<&'static str>,
    /// Live memory use: godterm RSS and each tab's process RSS (KB).
    pub mem: crate::app_settings::MemStats,
    last_mem: Instant,
    /// Guided first run: log each account in, one at a time.
    pub onboard: Option<Onboard>,
    last_status: Instant,
    last_session_scan: Instant,
    pub(crate) state_dirty: bool,
    last_state_save: Instant,
}

impl App {
    pub fn new(cfg: Config, tx: Sender<AppEvent>) -> App {
        // (Tests share the global color mode; they set it themselves.)
        if !cfg!(test) {
            crate::theme::set_usage_colors(&cfg.usage_colors);
        }
        let accounts = cfg
            .accounts
            .iter()
            .map(|_| AccountState::default())
            .collect();
        // One pane per account (at least one pane).
        let panes = (0..cfg.accounts.len().max(1))
            .map(|i| {
                let acct = (i < cfg.accounts.len()).then_some(i);
                let cwd = acct
                    .map(|a| cfg.accounts[a].work_dir())
                    .unwrap_or_else(crate::config::home_dir);
                Slot::new(acct, cwd)
            })
            .collect();
        let mut app = App {
            cfg,
            accounts,
            panes,
            focus: 0,
            zoom: false,
            view: View::Grid,
            modal: Modal::None,
            prefix: false,
            prefix_at: None,
            sel_account: 0,
            sel_session: 0,
            sess_source: crate::app_sessions::SourceSel::All,
            sess_filter: String::new(),
            sess_searching: false,
            sess_marked: Default::default(),
            sess_op: None,
            last_moves: vec![],
            main_sessions: vec![],
            main_grok_sessions: vec![],
            sess_harness: None,
            sess_subagents: false,
            sess_sort: Default::default(),
            sess_sort_rev: false,
            sess_group: Default::default(),
            sess_headless: false,
            sess_sub_rows: vec![],
            main_cache: Default::default(),
            main_loading: false,
            queued_moves: vec![],
            loop_cache: Default::default(),
            assistant: Default::default(),
            assistant_turn: 0,
            assistant_calls: 0,
            pending_confirms: vec![],
            superseded: vec![],
            pending_job: None,
            deliveries: vec![],
            pending_enters: vec![],
            delivery_seq: 0,
            last_target: None,
            ctl_waits: vec![],
            loops: vec![],
            loops_scanning: false,
            loops_scanned: Instant::now() - std::time::Duration::from_secs(60),
            sel_loop: 0,
            stop_reqs: vec![],
            pending_moves: vec![],
            moved_ids: Default::default(),
            opened_paths: vec![],
            takeovers: vec![],
            sess_live: Default::default(),
            sess_live_at: None,
            session_index: Default::default(),
            last_tab_move: None,
            sel_overview: 0,
            attention: vec![],
            rt_show_email: None,
            rt_mem_saver: None,
            mem_ticked: Instant::now(),
            rt_layout: None,
            ratios: Default::default(),
            last_arranged: None,
            main_area: Default::default(),
            dragging: None,
            sidebar_drag: None,
            pane_drag: None,
            output_at: Default::default(),
            closed: vec![],
            closed_seq: 0,
            open_home_ok: false,
            corrections: Default::default(),
            th: Default::default(),
            learned_ui: Default::default(),
            livemap: Default::default(),
            prompt_ui: Default::default(),
            tab_drag: None,
            bg_cache: Default::default(),
            main_grok_usage: None,
            own_usage: Default::default(),
            own_usage_at: None,
            forced_stops: vec![],
            undo_toast: None,
            tab_list_width: None,
            num_entry: None,
            rt_privacy: None,
            restore_queue: Default::default(),
            restore_next: Instant::now(),
            restore_done: Default::default(),
            marked: Default::default(),
            recents: vec![],
            window: None,
            last_window_check: Instant::now(),
            flash: None,
            quit: false,
            want_cursor: Default::default(),
            update: Default::default(),
            admin: Default::default(),
            setup: Default::default(),
            force_eager: false,
            pane_rects: vec![],
            term_rects: vec![],
            panel_overlay: Default::default(),
            key_burst: None,
            updates_max_scroll: Default::default(),
            menu_shown: Default::default(),
            failover: vec![],
            failover_declined: vec![],
            hits: Default::default(),
            modal_from: Default::default(),
            inline_rename: Default::default(),
            menu_overflow: Default::default(),
            last_hits: Default::default(),
            last_modal_from: None,
            mouse_pos: None,
            last_click: None,
            selection: None,
            sel_press: None,
            assistant_text: Default::default(),
            last_copied: None,
            mouse_capture: true,
            tx,
            voice: Default::default(),
            onboard: None,
            settings_section: 0,
            settings_sel: 0,
            settings_query: None,
            settings_collapsed: crate::settings::COLLAPSED_BY_DEFAULT
                .iter()
                .copied()
                .collect(),
            mem: Default::default(),
            last_mem: Instant::now() - Duration::from_secs(60),
            config_mtime: config_mtime(),
            last_config_check: Instant::now(),
            config_error: None,
            last_status: Instant::now(),
            last_session_scan: Instant::now(),
            state_dirty: false,
            last_state_save: Instant::now(),
        };
        for a in &app.cfg.accounts {
            ensure_dir(a);
        }
        // Synchronous first look so autostart knows who is logged in.
        for i in 0..app.accounts.len() {
            let snap = snapshot_of(&app.cfg.accounts[i], false);
            app.apply_status(i, snap);
        }
        // The first usage requests a few seconds apart, not all at once.
        if !cfg!(test) && !crate::demo::active() {
            for (i, st) in app.accounts.iter_mut().enumerate().skip(1) {
                st.next_fetch = Some(Instant::now() + Duration::from_secs(3 * i as u64));
            }
        }
        app
    }

    /// Restore saved tabs (if any) and start the visible ones.
    pub fn autostart(&mut self) {
        if let Some(st) = AppState::load() {
            self.restore(st);
        }
        if !self.cfg.autostart {
            return;
        }
        if self.restore_mode() == "eager" {
            self.queue_eager_restore();
        }
        for p in 0..self.panes.len() {
            if let Some(a) = self.panes[p].account {
                let t = self.panes[p].cur();
                if self.accounts[a].login.can_start() && t.state == PaneState::Idle {
                    let kind = t.pending.clone().unwrap_or(LaunchKind::Normal);
                    self.launch(p, kind);
                }
            }
        }
    }

    /// Every restored tab of a logged in account, visible tabs first. They
    /// start ~300 ms apart (`claude --resume <id>`, or fresh without an id)
    /// so the machine and the accounts are not hit all at once.
    pub fn queue_eager_restore(&mut self) {
        let mut visible = vec![];
        let mut rest = vec![];
        for (p, slot) in self.panes.iter().enumerate() {
            let Some(a) = slot.account else { continue };
            if !self.accounts.get(a).is_some_and(|x| x.login.can_start()) {
                continue;
            }
            for (ti, t) in slot.tabs.iter().enumerate() {
                if t.pending.is_none() || t.is_running() {
                    continue;
                }
                if ti == slot.active {
                    visible.push((p, t.uid));
                } else {
                    rest.push((p, t.uid));
                }
            }
        }
        // Visible tabs are started by autostart right away; count them.
        for (p, _) in &visible {
            self.restore_done.0 += 1;
            if let Some(a) = self.panes[*p].account {
                self.restore_done.1.insert(a);
            }
        }
        self.restore_queue = rest.into();
        self.restore_next = Instant::now() + std::time::Duration::from_millis(300);
        if self.restore_queue.is_empty() {
            self.restore_finished();
        }
    }

    fn restore_finished(&mut self) {
        let (n, accts) = (self.restore_done.0, self.restore_done.1.len());
        if n > 0 {
            self.flash(format!(
                "Restored {n} tab{} across {accts} account{}",
                if n == 1 { "" } else { "s" },
                if accts == 1 { "" } else { "s" }
            ));
            crate::log::info(&format!("restored {n} tabs across {accts} accounts"));
        }
        self.restore_done = Default::default();
    }

    /// Start the next queued tab when its turn comes (called from tick).
    pub fn restore_step(&mut self) {
        if self.restore_queue.is_empty() || Instant::now() < self.restore_next {
            return;
        }
        while let Some((p, uid)) = self.restore_queue.pop_front() {
            let Some(ti) = self
                .panes
                .get(p)
                .and_then(|s| s.tabs.iter().position(|t| t.uid == uid))
            else {
                continue;
            };
            let t = &self.panes[p].tabs[ti];
            // Already started (shown, or restarted by hand) in the meantime.
            if t.is_running() || t.pending.is_none() {
                continue;
            }
            let kind = t.pending.clone().unwrap_or(LaunchKind::Normal);
            self.launch_tab(p, ti, kind);
            self.restore_done.0 += 1;
            if let Some(a) = self.panes[p].account {
                self.restore_done.1.insert(a);
            }
            break;
        }
        self.restore_next = Instant::now() + std::time::Duration::from_millis(300);
        if self.restore_queue.is_empty() {
            self.restore_finished();
        }
    }

    /// Rebuild slots and tabs from saved state. Tabs start lazily: each one
    /// launches (with --resume when its session id is known) when first shown.
    pub fn restore(&mut self, st: AppState) {
        let mut panes = vec![];
        let mut focus = None;
        // Tabs keep their ids (t12) across restarts, and ids used before
        // are never handed out again: a remembered id is that tab or none.
        let saved_max = st
            .slots
            .iter()
            .flat_map(|s| s.tabs.iter())
            .filter_map(|t| t.uid)
            .max()
            .unwrap_or(0);
        // Also past ids a run that crashed handed out after its last save.
        crate::pane::reserve_uids_below(
            st.next_uid
                .unwrap_or(0)
                .max(saved_max + 1)
                .max(crate::pane::reserved_on_disk()),
        );
        // (The panes made before restoring are replaced, ids and all.)
        let mut taken = std::collections::HashSet::new();
        for (i, ss) in st.slots.iter().enumerate() {
            let acct = ss
                .account
                .as_ref()
                .and_then(|n| self.cfg.accounts.iter().position(|a| &a.name == n));
            if acct.is_none() || ss.tabs.is_empty() {
                continue;
            }
            let fallback = self.cfg.accounts[acct.unwrap()].work_dir();
            let mut slot = Slot::new(acct, fallback.clone());
            slot.tabs.clear();
            for t in &ss.tabs {
                let cwd = std::path::PathBuf::from(&t.cwd);
                let cwd = if cwd.is_dir() { cwd } else { fallback.clone() };
                slot.add_tab(cwd);
                let tab = slot.tabs.last_mut().unwrap();
                tab.session_id = t.session_id.clone();
                tab.custom_name = t.name.clone();
                tab.accent = t.accent.clone();
                tab.group = t.group.clone();
                tab.pinned = t.pinned;
                if let Some(o) = t.opened {
                    tab.opened = std::time::UNIX_EPOCH + std::time::Duration::from_secs(o);
                }
                if let Some(u) = t.uid.filter(|u| taken.insert(*u)) {
                    tab.uid = u;
                }
                tab.pending = Some(match &t.session_id {
                    Some(id) => LaunchKind::Resume(id.clone()),
                    None => LaunchKind::Normal,
                });
            }
            slot.active = ss.active.min(slot.tabs.len() - 1);
            slot.sidebar_collapsed = ss.sidebar_collapsed;
            slot.tab_pos = ss.tab_pos;
            slot.sidebar_w = ss.sidebar_w;
            slot.hidden = ss.hidden;
            slot.groups = ss.groups.clone();
            slot.sort = ss.sort;
            slot.repair_groups();
            if i == st.focus {
                focus = Some(panes.len());
            }
            panes.push(slot);
        }
        if !panes.is_empty() {
            // Saved panes in their order, then any account without one.
            self.panes = panes;
            self.focus = focus.unwrap_or(0);
        }
        self.ensure_panes();
        self.ensure_focus_visible();
        self.rt_layout = st.layout.clone();
        self.tab_list_width = st.tab_list_width.clone();
        self.closed = st.closed_tabs.clone();
        if let Some(v) = &st.sessions_view {
            self.sess_sort = crate::sess_sort::SortKey::parse(&v.sort).unwrap_or_default();
            self.sess_sort_rev = v.reverse;
            self.sess_group = crate::sess_sort::Group::parse(&v.group).unwrap_or_default();
            self.sess_headless = v.headless;
        }
        self.ratios = st.ratios.clone();
        self.recents = st.recents.clone();
        self.window = st.window.clone();
        self.rt_show_email = st.show_email;
        self.rt_mem_saver = st.memory_saver;
        self.rt_privacy = st.privacy;
    }

    pub fn snapshot_state(&self) -> AppState {
        AppState {
            show_email: self.rt_show_email,
            memory_saver: self.rt_mem_saver,
            privacy: self.rt_privacy,
            layout: self.rt_layout.clone(),
            tab_list_width: self.tab_list_width.clone(),
            closed_tabs: self.closed.clone(),
            sessions_view: {
                let v = crate::state::SessionsView {
                    sort: self.sess_sort.name().into(),
                    reverse: self.sess_sort_rev,
                    group: self.sess_group.name().into(),
                    headless: self.sess_headless,
                };
                (v != crate::state::SessionsView {
                    sort: "modified".into(),
                    group: "none".into(),
                    ..Default::default()
                })
                .then_some(v)
            },
            ratios: self.ratios.clone(),
            window: self.window.clone(),
            recents: self.recents.clone(),
            focus: self.focus,
            next_uid: Some(crate::pane::next_uid()),
            slots: self
                .panes
                .iter()
                .map(|s| SlotState {
                    account: s.account.map(|a| self.cfg.accounts[a].name.clone()),
                    // Index among the saved (non transient) tabs.
                    active: s.tabs[..s.active.min(s.tabs.len())]
                        .iter()
                        .filter(|t| !t.transient)
                        .count()
                        .min(
                            s.tabs
                                .iter()
                                .filter(|t| !t.transient)
                                .count()
                                .saturating_sub(1),
                        ),
                    tabs: s
                        .tabs
                        .iter()
                        .filter(|t| !t.transient)
                        .map(|t| TabState {
                            cwd: t.cwd.to_string_lossy().into_owned(),
                            session_id: t.session_id.clone(),
                            name: t.custom_name.clone(),
                            accent: t.accent.clone(),
                            uid: Some(t.uid),
                            group: t.group.clone(),
                            pinned: t.pinned,
                            opened: t
                                .opened
                                .duration_since(std::time::UNIX_EPOCH)
                                .ok()
                                .map(|d| d.as_secs()),
                        })
                        .collect(),
                    sidebar_collapsed: s.sidebar_collapsed,
                    tab_pos: s.tab_pos,
                    sidebar_w: s.sidebar_w,
                    hidden: s.hidden,
                    groups: s.groups.clone(),
                    sort: s.sort,
                })
                .collect(),
        }
    }

    pub fn save_state(&mut self) {
        let st = self.snapshot_state();
        if let Err(e) = st.save() {
            self.flash(format!("Could not save tabs: {e}"));
        }
        self.state_dirty = false;
        self.last_state_save = Instant::now();
    }

    /// Learn session ids of freshly started tabs from new transcripts.
    pub(crate) fn detect_sessions(&mut self) {
        for si in 0..self.panes.len() {
            let Some(a) = self.panes[si].account else {
                continue;
            };
            if !self.cfg.accounts[a].harness().integrated() {
                continue;
            }
            let dir = self.cfg.accounts[a].config_dir();
            for ti in 0..self.panes[si].tabs.len() {
                let t = &self.panes[si].tabs[ti];
                if !t.is_running() || matches!(t.kind, LaunchKind::Login) {
                    continue;
                }
                let taken: Vec<String> = self
                    .panes
                    .iter()
                    .flat_map(|s| s.tabs.iter())
                    .filter(|o| o.uid != t.uid)
                    .filter_map(|o| o.session_id.clone())
                    .collect();
                if let Some(id) = state::detect_session(&dir, &t.cwd, t.started, &taken) {
                    if t.session_id.as_deref() != Some(&id) {
                        self.panes[si].tabs[ti].session_id = Some(id);
                        self.state_dirty = true;
                    }
                }
            }
        }
    }

    /// Output has been arriving without a break for the last second: a
    /// tab is streaming, so redraws can slow to 30 a second.
    pub fn streaming(&self) -> bool {
        self.output_at.len() >= 30
            && self
                .output_at
                .front()
                .is_some_and(|t| t.elapsed() < Duration::from_millis(1100))
            && self
                .output_at
                .back()
                .is_some_and(|t| t.elapsed() < Duration::from_millis(100))
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now()));
    }

    pub fn account_cfg(&self, idx: usize) -> &AccountCfg {
        &self.cfg.accounts[idx]
    }

    pub fn account_color(&self, idx: Option<usize>) -> ratatui::style::Color {
        idx.map(|i| theme::parse_color(&self.cfg.accounts[i].color))
            .unwrap_or(theme::STONE)
    }

    // ---------- launching ----------

    /// Launch claude in the active tab of slot `pane`.
    pub fn launch(&mut self, pane: usize, kind: LaunchKind) {
        let Some(acct) = self.panes[pane].account else {
            self.flash("No account assigned to this pane (Ctrl-a a to assign)");
            return;
        };
        let a = self.cfg.accounts[acct].clone();
        let dir = a.config_dir();
        if creds::is_default_dir(&dir) {
            self.flash("Refusing to use the default ~/.claude config dir");
            return;
        }
        ensure_dir(&a);
        let h = a.harness();
        // Harness flags (grok: its own leader), the permission mode, then
        // the account's own extra args.
        let mut args = h.base_args(&dir);
        args.extend(h.permission_args(&self.cfg.mode_for(acct)));
        args.extend(a.args.iter().cloned());
        let tab_cwd = self.panes[pane].cur().cwd.clone();
        let mut cwd = if tab_cwd.is_dir() {
            tab_cwd
        } else {
            a.work_dir()
        };
        let mut type_when_idle = None;
        match &kind {
            LaunchKind::Normal => {}
            LaunchKind::Login if h.login_args().is_some() => {
                args = h.login_args().unwrap_or_default();
            }
            LaunchKind::Login if h == crate::harness::Harness::Claude => {
                // A fresh dir runs onboarding, which includes the OAuth login.
                // An onboarded but logged out dir needs /login typed in.
                if creds::load_profile(&dir).onboarded {
                    type_when_idle = Some(b"/login\r".to_vec());
                }
            }
            LaunchKind::Login => {} // native onboarding / login picker
            LaunchKind::Resume(id) => args.extend(h.resume_args(id)),
        }
        if let LaunchKind::Resume(id) = &kind {
            if let Some(s) = self.accounts[acct].sessions.iter().find(|s| &s.id == id) {
                let p = std::path::PathBuf::from(&s.cwd);
                if p.is_dir() {
                    cwd = p;
                }
            }
        }
        let spec = LaunchSpec {
            program: self.cfg.harness_bin(h),
            home_env: h.home_env(),
            extra_env: if h == crate::harness::Harness::Grok {
                crate::harness::grok::write_slot_compat(&dir, &self.cfg.grok_claude_compat);
                self.cfg
                    .grok_claude_compat
                    .env()
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            } else {
                vec![]
            },
            args,
            cwd,
            config_dir: dir,
            kind: kind.clone(),
            pass_env: self.cfg.pass_env.clone(),
            type_when_idle,
        };
        self.state_dirty = true;
        // Trust the folder up front, unless another claude of this account is
        // running and could be rewriting .claude.json at the same moment.
        if h == crate::harness::Harness::Claude
            && self.cfg.trust_for(acct)
            && crate::trust::dir_allowed(&spec.cwd, &self.cfg.trusted_dirs)
        {
            let busy = self
                .panes
                .iter()
                .filter(|s| s.account == Some(acct))
                .any(|s| s.any_running());
            if !busy {
                match crate::trust::seed_trust(&spec.config_dir, &spec.cwd) {
                    Ok(true) => crate::log::info(&format!(
                        "seeded trust for {} in {}",
                        spec.cwd.display(),
                        a.name
                    )),
                    Ok(false) => {}
                    Err(e) => crate::log::error(&format!("seeding trust: {e}")),
                }
            }
        }
        crate::log::info(&format!(
            "launch pane {} tab {} account {} {:?} in {}",
            pane + 1,
            self.panes[pane].active + 1,
            a.name,
            kind,
            spec.cwd.display()
        ));
        let ran_before = self.panes[pane].cur().generation > 0;
        match self.panes[pane].cur_mut().spawn(spec, self.tx.clone()) {
            Ok(()) => {
                if kind != LaunchKind::Login {
                    let t = self.panes[pane].active;
                    self.tab_event(
                        pane,
                        t,
                        if ran_before { "restart" } else { "open" },
                        None,
                        None,
                    );
                }
                let what = match kind {
                    LaunchKind::Normal => "Started",
                    LaunchKind::Login => "Login started for",
                    LaunchKind::Resume(_) => "Resumed session in",
                };
                self.flash(format!("{what} {}", a.display()));
            }
            Err(e) => self.flash(format!("Failed to start claude: {e:#}")),
        }
    }

    /// Run any program in a new tab of `pane` (an editor, doctor, ...).
    pub fn run_in_new_tab(&mut self, pane: usize, program: &str, args: Vec<String>) {
        let cwd = crate::config::app_home();
        let config_dir = self.panes[pane]
            .account
            .map(|a| self.cfg.accounts[a].config_dir())
            .unwrap_or_else(crate::config::app_home);
        self.panes[pane].add_tab(cwd.clone());
        let spec = LaunchSpec {
            program: program.to_string(),
            home_env: "CLAUDE_CONFIG_DIR",
            extra_env: vec![],
            args,
            cwd,
            config_dir,
            kind: LaunchKind::Normal,
            pass_env: self.cfg.pass_env.clone(),
            type_when_idle: None,
        };
        self.focus = pane;
        if let Err(e) = self.panes[pane].cur_mut().spawn(spec, self.tx.clone()) {
            self.flash(format!("Could not start {program}: {e:#}"));
        } else {
            self.panes[pane].cur_mut().transient = true;
            self.panes[pane].cur_mut().custom_name = Some(
                std::path::Path::new(program)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| program.to_string()),
            );
        }
    }

    /// Launch in a specific tab (which need not be the active one).
    pub fn launch_tab(&mut self, pane: usize, tab: usize, kind: LaunchKind) {
        let active = self.panes[pane].active;
        if !self.panes[pane].select(tab) {
            return;
        }
        self.launch(pane, kind);
        self.panes[pane].active = active;
    }

    pub(crate) fn login_for_account(&mut self, acct: usize) {
        let pane = self.pane_for_account(acct);
        self.assign(pane, acct);
        self.launch(pane, LaunchKind::Login);
        self.focus = pane;
        self.view = View::Grid;
    }

    /// The pane showing `acct`, or the focused pane (reassigned) if none.
    pub(crate) fn pane_for_account(&self, acct: usize) -> usize {
        self.panes
            .iter()
            .position(|p| p.account == Some(acct))
            .unwrap_or(self.focus)
    }

    /// Resume a session in the account's slot, in a new tab when the active
    /// tab is busy. If the session is already open in a tab, jump there.
    pub fn resume(&mut self, acct: usize, session_id: String) {
        for (si, s) in self.panes.iter().enumerate() {
            if let Some(ti) = s
                .tabs
                .iter()
                .position(|t| t.session_id.as_deref() == Some(&session_id) && t.is_running())
            {
                self.focus = si;
                self.panes[si].active = ti;
                self.view = View::Grid;
                self.flash("Session already open in this tab");
                return;
            }
        }
        let pane = self.pane_for_account(acct);
        self.assign(pane, acct);
        let cwd = self.accounts[acct]
            .sessions
            .iter()
            .find(|s| s.id == session_id)
            .map(|s| std::path::PathBuf::from(&s.cwd))
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| self.cfg.accounts[acct].work_dir());
        if self.panes[pane].cur().is_running() {
            self.panes[pane].add_tab(cwd);
        } else {
            self.panes[pane].cur_mut().cwd = cwd;
        }
        self.launch(pane, LaunchKind::Resume(session_id));
        self.focus = pane;
        self.view = View::Grid;
    }

    /// Point slot `pane` at `acct`. Switching accounts closes its tabs.
    pub(crate) fn assign(&mut self, pane: usize, acct: usize) {
        if self.panes[pane].account == Some(acct) {
            return;
        }
        self.panes[pane].kill_all();
        self.panes[pane] = Slot::new(Some(acct), self.cfg.accounts[acct].work_dir());
        self.state_dirty = true;
    }

    pub(crate) fn cycle_account(&mut self) {
        let n = self.cfg.accounts.len();
        if n == 0 {
            self.flash("No accounts configured (Ctrl-a n to add one)");
            return;
        }
        let next = self.panes[self.focus]
            .account
            .map(|a| (a + 1) % n)
            .unwrap_or(0);
        self.assign(self.focus, next);
        let label = self.cfg.accounts[next].display().to_string();
        self.flash(format!(
            "Pane {} now uses {label}; press Enter to start",
            self.focus + 1
        ));
    }

    /// Add an account from the Add account dialog, open its pane focused
    /// and start the agent's login there.
    pub(crate) fn add_account(&mut self, spec: NewAccount) {
        let taken: Vec<String> = self.cfg.accounts.iter().map(|a| a.name.clone()).collect();
        // The folder name comes from the label; "Account 3" -> account-3.
        let name = crate::setup::slug(&spec.label, &taken, self.cfg.accounts.len() + 1);
        if !valid_name(&name) {
            self.flash("Invalid name: use letters, digits, - _ .");
            return;
        }
        let harness = spec.harness.as_str();
        let acct = AccountCfg {
            name: name.clone(),
            label: spec.label.clone(),
            color: spec.color.clone(),
            cwd: Some(spec.cwd.clone()),
            args: vec![],
            permission_mode: spec.permission_mode.clone(),
            auto_trust: None,
            show_email: None,
            new_tab_base: None,
            scrollback_lines: None,
            config_dir: None,
            harness: crate::harness::Harness::of(harness).name().to_string(),
        };
        ensure_dir(&acct);
        // Append to config.toml with toml_edit so the user's comments stay.
        let saved = if Config::path().exists() {
            crate::settings::append_account(&Config::path(), &acct)
        } else {
            let mut c = self.cfg.clone();
            c.accounts.push(acct.clone());
            c.save()
        };
        self.cfg.accounts.push(acct);
        self.accounts.push(AccountState::default());
        if let Err(e) = saved {
            self.flash(format!("Could not save config: {e:#}"));
        }
        // Our own write is not an outside edit to reload.
        self.config_mtime = config_mtime();
        let idx = self.cfg.accounts.len() - 1;
        self.sel_account = idx;
        // Prefer an empty pane, then the focused pane if nothing runs there.
        // An empty pane takes it, otherwise it gets a pane of its own.
        let target = match self.panes.iter().position(|p| p.account.is_none()) {
            Some(p) => Some(p),
            None => {
                self.panes
                    .push(Slot::new(Some(idx), self.cfg.accounts[idx].work_dir()));
                Some(self.panes.len() - 1)
            }
        };
        match target {
            Some(p) => {
                self.assign(p, idx);
                self.focus = p;
                self.view = View::Grid;
                self.launch(p, LaunchKind::Login);
            }
            None => self.flash(format!(
                "Added {name}. Use Ctrl-a a on a pane to switch it to this account, then Ctrl-a I"
            )),
        }
    }

    // ---------- background refresh ----------

    /// Status for every account, plus usage where a fetch is allowed.
    pub fn refresh_all(&mut self, with_usage: bool) {
        for i in 0..self.accounts.len() {
            self.refresh_account(i, with_usage);
        }
        self.last_status = Instant::now();
    }

    /// Manual refresh: fetch usage if allowed, otherwise say when it will be.
    pub fn request_usage(&mut self, idx: usize) {
        let Some(st) = self.accounts.get(idx) else {
            return;
        };
        let wait = st.fetch_wait();
        let name = self.cfg.accounts[idx].display().to_string();
        if !st.login.logged_in() {
            self.flash(format!("{name} is not logged in"));
        } else if wait.is_zero() {
            self.refresh_account(idx, true);
            self.flash(format!("Refreshing usage for {name}"));
        } else {
            self.flash(format!(
                "{name}: next usage fetch in {}s (max one per minute)",
                wait.as_secs().max(1)
            ));
        }
    }

    /// Re-read login state; fetch usage too when asked and allowed.
    pub fn refresh_account(&mut self, idx: usize, want_usage: bool) {
        let st = &mut self.accounts[idx];
        if st.inflight {
            return;
        }
        let with_usage = want_usage && st.login.logged_in() && st.fetch_wait().is_zero();
        st.inflight = true;
        if with_usage {
            st.refreshing = true;
            st.last_fetch = Some(Instant::now());
        }
        let a = self.cfg.accounts[idx].clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let snap = snapshot_of(&a, with_usage);
            let _ = tx.send(AppEvent::Status(idx, Box::new(snap)));
        });
    }

    pub(crate) fn apply_status(&mut self, idx: usize, snap: StatusSnapshot) {
        let refresh_secs = self.cfg.refresh_secs;
        let Some(st) = self.accounts.get_mut(idx) else {
            return;
        };
        st.inflight = false;
        let was_logged_in = st.login.logged_in();
        let old_expiry = st.login.expires_at;
        st.profile = snap.profile;
        st.login = snap.login;
        if let Some(res) = snap.usage {
            let api_ok = res.is_ok();
            st.record_usage(res, refresh_secs);
            if api_ok {
                // A fresh reply wins over what grok showed before it.
                if let (Some((_, left, _)), Some(w)) = (
                    st.grok_screen.clone(),
                    st.usage.as_ref().and_then(|u| u.get("seven_day")),
                ) {
                    if (w.left() - left).abs() >= 1.0 {
                        crate::log::info(&format!("grok usage: the API says {:.0}% left, grok's screen said {left:.0}%; using the API (newer)", w.left()));
                    }
                }
                st.usage_note = None;
            }
        }
        let just_logged_in = !was_logged_in && st.login.logged_in();
        let token_renewed = was_logged_in
            && st.login.expires_at != old_expiry
            && st.usage_err == Some(UsageError::Unauthorized);
        if just_logged_in || token_renewed {
            // Fetch as soon as the one per minute limit allows.
            st.next_fetch = None;
            st.backoff = 0;
        }
    }

    pub fn load_sessions(&mut self, idx: usize) {
        let Some(st) = self.accounts.get_mut(idx) else {
            return;
        };
        if st.sessions_loading {
            return;
        }
        st.sessions_loading = true;
        let dir = self.cfg.accounts[idx].config_dir();
        let h = self.cfg.accounts[idx].harness();
        let cache = Arc::clone(&st.cache);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let list = match h {
                crate::harness::Harness::Grok => crate::harness::grok::scan_sessions(&dir),
                crate::harness::Harness::Claude => {
                    cache.lock().unwrap_or_else(|e| e.into_inner()).scan(&dir)
                }
                _ => vec![],
            };
            let _ = tx.send(AppEvent::Sessions(idx, list));
        });
    }

    /// grok shows its limit in its status line ("Weekly limit left: 0%"):
    /// mirror it, so the footer is never empty while grok knows the number
    /// and stays the same as grok's own when the two disagree.
    pub fn grok_screen_tick(&mut self) {
        let now = chrono::Utc::now();
        for a in 0..self.cfg.accounts.len() {
            if self.cfg.accounts[a].harness() != crate::harness::Harness::Grok {
                continue;
            }
            let seen = self
                .panes
                .iter()
                .filter(|p| p.account == Some(a))
                .flat_map(|p| p.tabs.iter())
                .filter(|t| t.is_running())
                .find_map(|t| {
                    let text = t
                        .parser
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .screen()
                        .contents();
                    crate::harness::grok::status_line_left(&text)
                });
            let Some((label, left)) = seen else { continue };
            let st = &mut self.accounts[a];
            if st
                .grok_screen
                .as_ref()
                .is_some_and(|(l, v, _)| *l == label && (*v - left).abs() < 0.5)
            {
                continue;
            }
            st.grok_screen = Some((label.clone(), left, now));
            let u = st.usage.get_or_insert_with(|| crate::usage::Usage {
                windows: vec![],
                absent: vec![],
                extra: None,
            });
            match u.windows.iter_mut().find(|w| w.key == "seven_day") {
                Some(w) => {
                    if (w.left() - left).abs() >= 1.0 {
                        crate::log::info(&format!("grok usage {}: grok's screen says {label} {left:.0}% left, the API said {:.0}%; using the screen (newer)", self.cfg.accounts[a].name, w.left()));
                    }
                    w.utilization = 100.0 - left;
                    w.label = label.clone();
                    w.short = label;
                }
                None => u.windows.insert(
                    0,
                    crate::usage::Window {
                        key: "seven_day".into(),
                        label: label.clone(),
                        short: label,
                        utilization: 100.0 - left,
                        resets_at: None,
                    },
                ),
            }
            st.usage_note = Some("from grok status".into());
            if st.usage_good_at.is_none() {
                st.usage_err = None;
            }
        }
    }

    /// Periodic work, called a few times per second.
    pub fn tick(&mut self) {
        self.selection_tick();
        self.grok_screen_tick();
        self.takeover_tick();
        if self.view == View::Sessions
            && self
                .sess_live_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(3))
        {
            self.sess_live = self
                .live_sessions()
                .into_iter()
                .map(|l| (l.session_id.clone(), l))
                .collect();
            self.sess_live_at = Some(Instant::now());
        }
        self.restore_step();
        self.tabmove_tick();
        self.refresh_loops(false);
        self.stop_tick();
        self.memory_tick();
        self.assistant_tick();
        self.update_tick();
        self.admin_tick();
        self.control_tick();
        self.refresh_paths();
        for st in &mut self.accounts {
            while st.usage_hist.len() > self.cfg.usage_history {
                st.usage_hist.pop_front();
            }
        }
        for (si, slot) in self.panes.iter_mut().enumerate() {
            let active = slot.active;
            for (ti, t) in slot.tabs.iter_mut().enumerate() {
                t.tick();
                if let Some((old, new)) = t.update_activity() {
                    // Waiting for approval, or finished a turn.
                    let finished = old == Activity::Working && new == Activity::Ready;
                    if new == Activity::Permission || finished {
                        let visible = si == self.focus && ti == active && self.view == View::Grid;
                        if !visible {
                            self.attention.push(Attention {
                                slot: si,
                                tab: ti,
                                what: new,
                            });
                        }
                    }
                }
            }
        }
        // Scheduled crash restarts.
        for p in 0..self.panes.len() {
            for t in 0..self.panes[p].tabs.len() {
                let tab = &mut self.panes[p].tabs[t];
                if tab.restart_at.is_some_and(|at| Instant::now() >= at) {
                    tab.restart_at = None;
                    let kind = tab.relaunch_kind();
                    self.launch_tab(p, t, kind);
                }
            }
        }
        // Folder trust dialogs answered automatically where allowed.
        let mut to_trust = vec![];
        for (si, slot) in self.panes.iter().enumerate() {
            let Some(a) = slot.account else { continue };
            if !self.cfg.trust_for(a) {
                continue;
            }
            for (ti, t) in slot.tabs.iter().enumerate() {
                if t.activity == Activity::Permission
                    && !t.trust_answered
                    && crate::trust::dir_allowed(&t.cwd, &self.cfg.trusted_dirs)
                {
                    let screen = t
                        .parser
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .screen()
                        .contents();
                    if self.cfg.accounts[a].harness().integrated()
                        && crate::prompt::parse_prompt(&screen)
                            .is_some_and(|p| p.kind == crate::prompt::PromptKind::Trust)
                    {
                        to_trust.push((si, ti));
                    }
                }
            }
        }
        for (si, ti) in to_trust {
            let (keys, _) = self.answer_keys(si, ti, crate::prompt::Choice::Approve);
            let t = &mut self.panes[si].tabs[ti];
            t.trust_answered = true;
            t.write(&keys);
            let dir = t.cwd.to_string_lossy().into_owned();
            crate::log::info(&format!(
                "auto trusted {dir} in pane {} tab {}",
                si + 1,
                ti + 1
            ));
            self.flash(format!("trusted {dir}"));
        }
        // Restored tabs launch when they become the visible tab of a shown
        // slot (hidden panes wait like lazy tabs).
        for p in 0..self.panes.len() {
            if self.panes[p].hidden {
                continue;
            }
            let Some(a) = self.panes[p].account else {
                continue;
            };
            if self.accounts[a].login.can_start() {
                if let Some(kind) = self.panes[p].cur().pending.clone() {
                    if self.panes[p].cur().state == PaneState::Idle {
                        self.launch(p, kind);
                    }
                }
            }
        }
        if self.last_session_scan.elapsed() >= Duration::from_secs(10) {
            self.last_session_scan = Instant::now();
            self.detect_sessions();
        }
        self.voice_tick();
        self.onboard_tick();
        // Window bounds, every 5 s, off the UI thread.
        if self.cfg.remember_window
            && self.last_window_check.elapsed() >= Duration::from_secs(5)
            && crate::window::terminal_app().is_some()
        {
            self.last_window_check = Instant::now();
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(AppEvent::Window(crate::window::capture()));
            });
        }
        if self.view == View::Settings && self.last_mem.elapsed() >= Duration::from_secs(2) {
            self.last_mem = Instant::now();
            self.refresh_mem();
        }
        if self.last_config_check.elapsed() >= Duration::from_secs(2) {
            self.last_config_check = Instant::now();
            self.check_config_reload();
        }
        if self.state_dirty && self.last_state_save.elapsed() >= Duration::from_secs(3) {
            self.save_state();
        }
        // A login in progress gets polled quickly so the dashboard flips fast.
        let logging_in = self
            .panes
            .iter()
            .flat_map(|s| s.tabs.iter())
            .any(|p| p.kind == LaunchKind::Login && p.is_running());
        let status_every = Duration::from_secs(if logging_in { 3 } else { 15 });
        let status_due = self.last_status.elapsed() >= status_every;
        for i in 0..self.accounts.len() {
            let st = &self.accounts[i];
            let usage_due = st.login.logged_in() && st.fetch_wait().is_zero();
            if usage_due || status_due {
                self.refresh_account(i, usage_due);
            }
        }
        if status_due {
            self.last_status = Instant::now();
            self.maybe_fetch_own_usage();
            self.failover_scan();
        }
        if let Some((_, t)) = &self.flash {
            if t.elapsed() > Duration::from_secs(5) {
                self.flash = None;
            }
        }
        if self.prefix {
            if let Some(t) = self.prefix_at {
                if t.elapsed() > Duration::from_secs(3) {
                    self.prefix = false;
                }
            }
        }
    }

    // ---------- events ----------

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::KeyRead(k, burst) => {
                self.key_burst = Some(burst);
                self.handle(AppEvent::Input(Event::Key(k)));
                self.key_burst = None;
            }
            AppEvent::Input(Event::Key(k)) => {
                if self.hold_to_talk_key(&k) {
                    return;
                }
                if k.kind != KeyEventKind::Release {
                    self.on_key(k);
                }
            }
            AppEvent::Input(Event::Paste(text)) => {
                // The assistant panel's input, while it has the keys.
                if self.assistant_has_focus()
                    && self.assistant.history.is_none()
                    && self.modal == Modal::None
                {
                    self.assistant_paste(&text);
                } else if self.view == View::Grid && self.modal == Modal::None {
                    let p = self.panes[self.focus].cur_mut();
                    let b = keys::encode_paste(&text, p.bracketed_paste());
                    p.reset_scroll();
                    p.write(&b);
                } else if let Modal::AddAccount(form) = &mut self.modal {
                    form.paste(&text);
                } else if let Modal::EditSetting(_, b) = &mut self.modal {
                    b.push_str(text.trim());
                } else if let Modal::Rename(_, _, b) = &mut self.modal {
                    b.push_str(text.trim());
                } else if let Modal::Broadcast(b) = &mut self.modal {
                    b.push_str(&text.replace(['\r', '\n'], " "));
                } else if let Modal::NewTab(pk) = &mut self.modal {
                    for c in text.trim().chars() {
                        pk.type_char(c);
                    }
                }
            }
            AppEvent::Input(Event::Mouse(m)) => self.on_mouse(m),
            AppEvent::Input(Event::FocusGained) => self.voice.term_focused = Some(true),
            AppEvent::Input(Event::FocusLost) => self.voice.term_focused = Some(false),
            AppEvent::Input(_) => {}
            AppEvent::PaneOutput => {
                crate::pane::OUTPUT_PENDING.store(false, std::sync::atomic::Ordering::Release);
                self.output_at.push_back(Instant::now());
                while self.output_at.len() > 40 {
                    self.output_at.pop_front();
                }
            }
            AppEvent::PaneExited(uid, gen) => {
                for si in 0..self.panes.len() {
                    if let Some(ti) = self.panes[si].find_uid(uid) {
                        let auto = self.cfg.auto_restart;
                        let t = &mut self.panes[si].tabs[ti];
                        let was_login = t.kind == LaunchKind::Login;
                        let was_running = t.is_running();
                        t.on_exit(gen);
                        if was_running && !was_login && t.crashed() {
                            let code = match t.state {
                                PaneState::Exited(Some(c)) => c,
                                _ => 0,
                            };
                            t.crashes.retain(|c| c.elapsed() < Duration::from_secs(60));
                            t.crashes.push(Instant::now());
                            let msg = format!(
                                "claude in pane {} tab {} exited with code {code}",
                                si + 1,
                                ti + 1
                            );
                            crate::log::error(&msg);
                            let crashes = t.crashes.len();
                            self.tab_event(
                                si,
                                ti,
                                "crash",
                                Some("crash"),
                                Some(format!("exit code {code}")),
                            );
                            let t = &mut self.panes[si].tabs[ti];
                            if auto && crashes <= 3 {
                                t.restart_at = Some(Instant::now() + Duration::from_secs(2));
                                self.flash(format!("{msg}; restarting"));
                            } else {
                                self.flash(format!("{msg}; Enter restarts it"));
                            }
                        }
                        if was_login {
                            if let Some(a) = self.panes[si].account {
                                self.refresh_account(a, true);
                            }
                        }
                    }
                }
            }
            AppEvent::Status(idx, snap) => self.apply_status(idx, *snap),
            AppEvent::MainGrokUsage(r) => self.main_grok_usage = Some(*r),
            AppEvent::OwnUsage(id, r) => {
                self.own_usage.insert(id, *r);
            }
            AppEvent::Voice(ev) => self.on_voice(ev),
            AppEvent::Mem(m) => self.mem = m,
            AppEvent::Window(w) => {
                if let Some(w) = w {
                    if self.window.as_ref() != Some(&w) {
                        self.window = Some(w);
                        self.state_dirty = true;
                    }
                }
            }
            AppEvent::Loops(rows) => self.on_loops(rows),
            AppEvent::UserTurn(text) => {
                crate::log::info(&format!("control: user turn from a control client: {text}"));
                self.assistant_turn += 1;
                self.assistant_calls = 0;
                self.assistant.last_user = text;
            }
            AppEvent::Control(tool, args, reply) => {
                let mut v = self.control_call(&tool, &args);
                // Replies that wait for a tab to come up or a prompt to be
                // taken are finished by control_tick.
                if let Some(w) = v.as_object_mut().and_then(|o| o.remove("_wait")) {
                    let ids: Vec<u64> = w["deliveries"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|x| x.as_u64())
                        .collect();
                    self.ctl_waits.push(crate::control::Waiting {
                        reply: v,
                        tx: reply,
                        since: Instant::now(),
                        ready_uids: w["ready_uids"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|x| x.as_u64())
                            .chain(w["ready_uid"].as_u64())
                            .collect(),
                        deliveries: ids,
                        wait_queued: w["wait_queued"].as_bool().unwrap_or(false),
                        max: std::time::Duration::from_millis(w["max_ms"].as_u64().unwrap_or(6000)),
                        takeovers: w["takeovers"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect(),
                        job: if w["job"].as_bool() == Some(true) {
                            self.pending_job.take()
                        } else {
                            None
                        },
                    });
                    self.control_tick();
                } else {
                    let _ = reply.send(v);
                }
            }
            AppEvent::Assistant(gen, ev) => {
                // Only the current process speaks; an old one's exit is not news.
                if self.assistant.brain.as_ref().is_some_and(|b| b.gen == gen) {
                    self.on_brain(ev);
                }
            }
            AppEvent::InputClosed => {
                crate::log::info("terminal closed, shutting down");
                self.quit = true;
            }
            AppEvent::Sessions(crate::app_sessions::MAIN_GROK_IDX, list) => {
                self.main_grok_sessions = list
            }
            AppEvent::Sessions(crate::app_sessions::MAIN_IDX, list) => {
                self.main_sessions = list;
                self.main_loading = false;
            }
            AppEvent::Sessions(idx, list) => {
                if let Some(st) = self.accounts.get_mut(idx) {
                    st.sessions = list;
                    st.sessions_loading = false;
                    st.sessions_loaded = true;
                }
                if idx == self.sel_account {
                    let n = self.accounts[idx].sessions.len();
                    self.sel_session = self.sel_session.min(n.saturating_sub(1));
                }
                if let Modal::NewTab(pk) = &self.modal {
                    if pk.account == idx {
                        let rows = self.recent_rows_for(idx, pk.all_accounts);
                        let sessions = self.accounts[idx].sessions.clone();
                        if let Modal::NewTab(pk) = &mut self.modal {
                            pk.recents = rows;
                            pk.set_sessions(&sessions);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn on_mouse(&mut self, m: MouseEvent) {
        self.on_mouse_event(m);
    }

    pub(crate) fn on_key(&mut self, k: KeyEvent) {
        // Ctrl-a ' then a pane number.
        if let Some(buf) = self.num_entry.as_mut() {
            match k.code {
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    buf.push(c);
                    let b = buf.clone();
                    self.flash(format!("Pane {b}... (Enter)"));
                    return;
                }
                KeyCode::Enter => {
                    let n = buf.parse().unwrap_or(0);
                    self.num_entry = None;
                    self.focus_number(n);
                    return;
                }
                KeyCode::Backspace => {
                    buf.pop();
                    return;
                }
                _ => {
                    self.num_entry = None;
                    if k.code == KeyCode::Esc {
                        return;
                    }
                }
            }
        }
        // Modals swallow input first.
        match &mut self.modal {
            Modal::None => {}
            Modal::Help => {
                self.modal = Modal::None;
                return;
            }
            Modal::ConfirmCloseWindow(s) => {
                let s = *s;
                self.modal = Modal::None;
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.close_window(s);
                }
                return;
            }
            Modal::ConfirmClose => {
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.modal = Modal::None;
                    self.close_tab_now();
                } else {
                    self.modal = Modal::None;
                }
                return;
            }
            Modal::NewTab(pk) => {
                use crate::picker::Mode;
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                match (k.code, pk.mode.clone()) {
                    (KeyCode::Esc, Mode::Name) => self.modal = Modal::None,
                    (KeyCode::Esc, _) => pk.mode = Mode::Name,
                    (KeyCode::Tab, Mode::OpenExisting(t)) => {
                        pk.mode = Mode::OpenExisting(crate::picker::complete_dir(&t))
                    }
                    (KeyCode::Up, _) => pk.move_sel(-1),
                    (KeyCode::Down | KeyCode::Tab, _) => pk.move_sel(1),
                    (KeyCode::Backspace, _) => pk.backspace(),
                    (KeyCode::Char('f'), _) if ctrl => pk.create_folder = !pk.create_folder,
                    (KeyCode::Char('g'), _) if ctrl => pk.git_init = !pk.git_init,
                    (KeyCode::Char('o'), _) if ctrl => {
                        pk.mode = Mode::OpenExisting(
                            crate::picker::tilde(&pk.base.to_string_lossy()) + "/",
                        )
                    }
                    (KeyCode::Char('b'), _) if ctrl => {
                        pk.mode = Mode::Base(crate::picker::tilde(&pk.base.to_string_lossy()))
                    }
                    (KeyCode::Char('u'), _) if ctrl => pk.input_mut().clear(),
                    (KeyCode::Enter, _) => self.new_tab_enter(),
                    (KeyCode::Char(c), _) if !ctrl => pk.type_char(c),
                    _ => {}
                }
                return;
            }
            Modal::Palette(pl) => {
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Up => pl.sel = pl.sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Tab => {
                        let n = pl.matches().len();
                        if n > 0 {
                            pl.sel = (pl.sel + 1) % n;
                        }
                    }
                    KeyCode::Backspace => {
                        pl.input.pop();
                        pl.sel = 0;
                    }
                    KeyCode::Enter => {
                        let pl = pl.clone();
                        self.modal = Modal::None;
                        self.run_palette(pl);
                    }
                    KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                        pl.input.push(c);
                        pl.sel = 0;
                    }
                    _ => {}
                }
                return;
            }
            Modal::AccountMenu(slot, sel) => {
                let (slot, sel_v) = (*slot, *sel);
                let n = self.account_menu_entries(slot).len();
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.modal = Modal::None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.modal = Modal::AccountMenu(slot, sel_v.saturating_sub(1))
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.modal = Modal::AccountMenu(slot, (sel_v + 1).min(n.saturating_sub(1)))
                    }
                    KeyCode::Enter => self.modal_ok(),
                    _ => {}
                }
                return;
            }
            Modal::Tour(n) => {
                let n = *n;
                match k.code {
                    KeyCode::Esc | KeyCode::Char('s') | KeyCode::Char('q') => self.finish_tour(),
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Char('n') => {
                        if n + 1 < crate::app_mouse::TOUR.len() {
                            self.modal = Modal::Tour(n + 1);
                        } else {
                            self.finish_tour();
                        }
                    }
                    KeyCode::Left | KeyCode::Char('p') => {
                        self.modal = Modal::Tour(n.saturating_sub(1))
                    }
                    _ => {}
                }
                return;
            }
            Modal::Approvals(sel) => {
                let sel_v = *sel;
                self.on_approvals_key(k, sel_v);
                return;
            }
            Modal::Broadcast(buf) => {
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Enter => {
                        let text = buf.clone();
                        self.modal = Modal::None;
                        self.broadcast(&text);
                    }
                    KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => buf.push(c),
                    _ => {}
                }
                return;
            }
            Modal::Updates(top) => {
                let top = *top;
                let max = self.updates_max_scroll.get();
                let page = 10u16;
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.modal = Modal::None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.modal = Modal::Updates(top.saturating_sub(1))
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.modal = Modal::Updates((top + 1).min(max))
                    }
                    KeyCode::PageUp => self.modal = Modal::Updates(top.saturating_sub(page)),
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        self.modal = Modal::Updates((top + page).min(max))
                    }
                    KeyCode::Home => self.modal = Modal::Updates(0),
                    KeyCode::End => self.modal = Modal::Updates(max),
                    KeyCode::Char('l') => self.updates_later(),
                    KeyCode::Char('p') => self.updates_release_page(),
                    KeyCode::Char('d') => self.updates_download(),
                    KeyCode::Char('r') | KeyCode::Enter => {
                        if self.update.ready().is_some() {
                            self.modal = Modal::UpdateRestart;
                        } else if k.code == KeyCode::Enter {
                            self.updates_download();
                        }
                    }
                    _ => {}
                }
                return;
            }
            Modal::UpdateRestart => {
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.modal = Modal::None;
                    self.restart_to_update(true);
                } else {
                    self.modal = Modal::Updates(0);
                }
                return;
            }
            Modal::ConfirmQuit => {
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.quit = true;
                }
                self.modal = Modal::None;
                return;
            }
            Modal::ConfirmBypass(p, a) => {
                let (p, a) = (*p, *a);
                self.modal = Modal::None;
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.set_permission_mode(p, a, "bypass", true);
                }
                return;
            }
            Modal::OfferRestart(p) => {
                let p = *p;
                self.modal = Modal::None;
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    let kind = self.panes[p].cur().relaunch_kind();
                    self.launch(p, kind);
                }
                return;
            }
            Modal::EditSetting(row, buf) => {
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Enter => {
                        let (row, text) = (*row, buf.clone());
                        self.modal = Modal::None;
                        self.setting_set_text(row, &text);
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => buf.push(c),
                    _ => {}
                }
                return;
            }
            Modal::WakeTrain => {
                self.on_train_key(k);
                return;
            }
            Modal::Menu(_) => {
                self.on_menu_key(k);
                return;
            }
            Modal::TakeOver(id) => {
                let id = id.clone();
                match k.code {
                    KeyCode::Char(c @ '1'..='3') => {
                        self.modal = Modal::None;
                        self.bring_here(&id, c as u8 - b'1');
                    }
                    _ => self.modal = Modal::None,
                }
                return;
            }
            Modal::EmptySlotMenu(_) => {
                self.modal = Modal::None;
                if let KeyCode::Char(c @ '1'..='9') = k.code {
                    if let Some(&(a, _)) = self.accounts_by_left().get((c as u8 - b'1') as usize) {
                        self.new_pane_for(a);
                    }
                }
                return;
            }
            Modal::MenuOverflow | Modal::PathMenu(_) => {
                self.modal = Modal::None;
                return;
            }
            Modal::ConfirmStopLoops => {
                self.modal = Modal::None;
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.stop_all_loops();
                }
                return;
            }
            Modal::SessTarget(_) | Modal::SessConfirm | Modal::SessConflict(_) => {
                self.on_sess_modal_key(k);
                return;
            }
            Modal::MoveTab(_) | Modal::MoveBusy(_) | Modal::TabMenu(..) => {
                self.on_move_modal_key(k);
                return;
            }
            Modal::GroupMenu(..)
            | Modal::GroupPick(..)
            | Modal::GroupName { .. }
            | Modal::ConfirmCloseGroup(..)
            | Modal::GroupMove(..) => {
                self.on_group_modal_key(k);
                return;
            }
            Modal::GrokLogins(_) => {
                self.on_grok_logins_key(k);
                return;
            }
            Modal::ConfirmRemoveAccount(a) => {
                let a = *a;
                self.modal = Modal::None;
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.remove_account(a);
                }
                return;
            }
            Modal::Rename(slot, tab, buf) => {
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Enter => {
                        let (s, t, name) = (*slot, *tab, buf.clone());
                        self.modal = Modal::None;
                        self.rename_tab(s, t, &name);
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c)
                        if !k.modifiers.contains(KeyModifiers::CONTROL)
                            && buf.chars().count() < 40 =>
                    {
                        buf.push(c)
                    }
                    _ => {}
                }
                return;
            }
            Modal::AddAccount(_) => {
                self.on_add_account_key(k);
                return;
            }
            Modal::ColorPick(..) => {
                self.on_color_pick_key(k);
                return;
            }
        }

        let is_prefix_key =
            k.code == KeyCode::Char('a') && k.modifiers.contains(KeyModifiers::CONTROL);
        if self.prefix {
            self.prefix = false;
            if is_prefix_key {
                // Ctrl-a Ctrl-a sends a literal Ctrl-a.
                if self.view == View::Grid {
                    self.panes[self.focus].cur_mut().write(&[0x01]);
                }
                return;
            }
            self.on_command(k);
            return;
        }
        if is_prefix_key {
            self.prefix = true;
            self.prefix_at = Some(Instant::now());
            return;
        }
        // A key into a pane (or the assistant) drops the selection there;
        // Esc does only that.
        if self.selection_key(&k) {
            return;
        }
        // The assistant panel's input takes typing while it is open.
        if self.on_assistant_key(k) {
            return;
        }
        match self.view {
            View::Grid => self.on_grid_key(k),
            View::Dashboard => self.on_dashboard_key(k),
            View::Sessions => self.on_sessions_key(k),
            View::Overview => self.on_overview_key(k),
            View::Settings => self.on_settings_key(k),
            View::Loops => self.on_loops_key(k),
            View::TabHistory => self.on_tab_history_key(k),
            View::Learned => self.on_learned_key(k),
            View::LiveMap => self.on_livemap_key(k),
            View::Prompt => self.on_prompt_key(k),
        }
    }

    pub(crate) fn on_grid_key(&mut self, k: KeyEvent) {
        let account = self.panes[self.focus].account;
        // Its program is not installed: I installs it.
        if matches!(k.code, KeyCode::Char('I') | KeyCode::Char('i'))
            && self.panes[self.focus].cur().state != PaneState::Running
        {
            if let Some(id) = self.agent_missing(account) {
                self.open_setup();
                self.setup_click(id);
                return;
            }
        }
        let pane = self.panes[self.focus].cur_mut();
        match pane.state {
            PaneState::Running => {
                // Shift+PageUp/PageDown scroll locally; everything else goes in.
                if k.modifiers.contains(KeyModifiers::SHIFT) {
                    match k.code {
                        KeyCode::PageUp => return pane.scroll_by(10),
                        KeyCode::PageDown => return pane.scroll_by(-10),
                        _ => {}
                    }
                }
                let bytes = keys::encode_key(&k, pane.app_cursor());
                pane.reset_scroll();
                pane.write(&bytes);
            }
            _ => {
                if k.code == KeyCode::Enter {
                    let p = self.focus;
                    let logged_in = account
                        .map(|a| self.accounts[a].login.can_start())
                        .unwrap_or(false);
                    let kind = match pane.pending.clone() {
                        Some(k) if logged_in => k,
                        _ if logged_in => pane.relaunch_kind(),
                        _ => LaunchKind::Login,
                    };
                    self.launch(p, kind);
                }
            }
        }
    }

    pub(crate) fn on_command(&mut self, k: KeyEvent) {
        let ch = match k.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        };
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let dir = match k.code {
            KeyCode::Left => Some((-1, 0)),
            KeyCode::Right => Some((1, 0)),
            KeyCode::Up => Some((0, -1)),
            KeyCode::Down => Some((0, 1)),
            _ => None,
        };
        if let (true, Some((dx, dy))) = (shift, dir) {
            // Ctrl-a Shift+arrow: move the focused pane that way.
            self.view = View::Grid;
            match self.move_pane(self.focus, dx, dy) {
                Ok(to) => self.flash(format!("Moved the pane to slot {}", self.slot_number(to))),
                Err(e) => self.flash(format!("Can't move it: {e}")),
            }
            return;
        }
        match (k.code, ch) {
            (KeyCode::Left, _) | (_, Some('h')) => self.move_focus(-1, 0),
            (KeyCode::Right, _) | (_, Some('l')) => self.move_focus(1, 0),
            (KeyCode::Up, _) | (_, Some('k')) => self.move_focus(0, -1),
            (KeyCode::Down, _) | (_, Some('j')) => self.move_focus(0, 1),
            (_, Some(c @ '1'..='9')) => self.focus_number((c as u8 - b'0') as usize),
            (_, Some('\'')) => {
                self.num_entry = Some(String::new());
                self.flash("Pane number: type it, then Enter (Esc cancels)");
            }
            (KeyCode::Tab, _) => self.focus_next_visible(),
            (_, Some('[')) => self.page_by(-1),
            (_, Some(']')) => self.page_by(1),
            (_, Some('L')) => self.cycle_layout(),
            (_, Some('|')) => {
                let p = self.focus;
                self.split_pane(p);
            }
            (_, Some('o')) => self.open_overview(),
            (_, Some('t')) => self.new_tab_prompt(self.focus),
            (_, Some('w')) => {
                let (s, t) = (self.focus, self.panes[self.focus].active);
                self.request_close(s, t);
            }
            (_, Some('W')) => {
                if let Err(e) = self.reopen_closed() {
                    self.flash(format!("Nothing to reopen: {e}"));
                }
            }
            (_, Some('n')) => {
                self.view = View::Grid;
                let top = self.ungrouped_top();
                self.panes[self.focus].step_tab(top, 1);
            }
            (_, Some('p')) => {
                self.view = View::Grid;
                let top = self.ungrouped_top();
                self.panes[self.focus].step_tab(top, -1);
            }
            (_, Some('P')) => {
                let (s, t) = (self.focus, self.panes[self.focus].active);
                self.toggle_pin(s, t);
            }
            (_, Some('z')) => {
                self.zoom = !self.zoom;
                self.view = View::Grid;
            }
            (_, Some('r')) => {
                let p = self.focus;
                self.view = View::Grid;
                let kind = self.panes[p].cur().relaunch_kind();
                self.launch(p, kind);
            }
            (_, Some('x')) => {
                self.panes[self.focus].cur_mut().kill();
                self.flash(format!("Stopped pane {}", self.focus + 1));
            }
            (_, Some('I')) => {
                if let Some(a) = self.panes[self.focus].account {
                    self.login_for_account(a);
                } else {
                    self.flash("No account on this pane (Ctrl-a a)");
                }
            }
            (_, Some('a')) => {
                self.view = View::Grid;
                self.cycle_account();
            }
            (_, Some('A')) => self.open_add_account(),
            (_, Some('d')) => {
                self.view = if self.view == View::Dashboard {
                    View::Grid
                } else {
                    View::Dashboard
                };
                if self.view == View::Dashboard {
                    self.sel_account = self.panes[self.focus].account.unwrap_or(0);
                }
            }
            (_, Some('s')) => {
                let p = self.focus;
                self.toggle_sidebar(p);
            }
            (_, Some(',')) => self.open_settings(),
            (_, Some('R')) => {
                let (p, t) = (self.focus, self.panes[self.focus].active);
                self.start_rename(p, t);
            }
            (_, Some('H')) => {
                if self.view == View::Sessions {
                    self.view = View::Grid;
                } else {
                    self.open_sessions(self.panes[self.focus].account.unwrap_or(self.sel_account));
                }
            }
            (_, Some('u')) => {
                let a = self.panes[self.focus].account.unwrap_or(self.sel_account);
                self.request_usage(a);
            }
            (_, Some('g')) | (KeyCode::Home, _) => self.go_home(),
            (KeyCode::Char(' '), _) => self.push_to_talk(),
            (_, Some(':')) => self.modal = Modal::Palette(Default::default()),
            (_, Some('S')) if self.onboard.is_some() => self.onboard_skip(),
            (_, Some('S')) => {
                let p = self.focus;
                let next = self.tab_pos(p).next();
                self.set_tab_pos(p, Some(next));
            }
            (_, Some('b')) => self.modal = Modal::Broadcast(String::new()),
            (_, Some('y')) => self.modal = Modal::Approvals(0),
            (_, Some('c')) => self.copy_selection(),
            (_, Some('m')) => {
                let (p, t) = (self.focus, self.panes[self.focus].active);
                self.open_move_picker(p, t, false);
            }
            (_, Some('U')) => self.undo_tab_move(),
            (_, Some('@')) => self.open_loops(),
            (_, Some('G')) => self.open_livemap(),
            (_, Some('Z')) => self.toggle_memory_saver(),
            (_, Some('X')) => self.toggle_mute(),
            (_, Some('O')) => self.toggle_speaker(),
            (_, Some('D')) => self.open_updates(),
            (_, Some('F')) => {
                if self.accept_failover(None, false).is_none() {
                    self.flash("No move on offer");
                }
            }
            (_, Some('+')) | (_, Some('=')) => self.nudge_volume(0.1),
            (_, Some('-')) => self.nudge_volume(-0.1),
            (_, Some('.')) => self.toggle_assistant_panel(),
            (_, Some('M')) => {
                self.mouse_capture = !self.mouse_capture;
                self.flash(if self.mouse_capture {
                    "Mouse on: everything is clickable, and a drag in a pane selects and copies its text"
                } else {
                    "Mouse released to the terminal (its selection spans every pane; drag to select works without this). Ctrl-a M takes it back"
                });
            }
            (_, Some('T')) => {
                self.view = View::Grid;
                self.modal = Modal::Tour(0);
            }
            (_, Some('v')) => self.toggle_wake_mode(),
            (_, Some('V')) => self.voice.show_log = !self.voice.show_log,
            (_, Some('e')) => self.toggle_show_email(),
            (_, Some('E')) => self.toggle_privacy(),
            (_, Some('?')) => self.modal = Modal::Help,
            (_, Some('q')) => self.modal = Modal::ConfirmQuit,
            (_, Some('Q')) => self.quit = true,
            (_, Some('N')) => self.restart_to_update(false),
            (_, Some('C')) => {
                if let Some(a) = self.panes.get(self.focus).and_then(|p| p.account) {
                    let say = self.close_accounts(&[a]);
                    self.flash(say);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn open_sessions(&mut self, acct: usize) {
        if self.cfg.accounts.is_empty() {
            self.flash("No accounts configured");
            return;
        }
        self.sel_account = acct.min(self.cfg.accounts.len() - 1);
        self.set_source(crate::app_sessions::SourceSel::One(
            crate::app_sessions::Src::Account(self.sel_account),
        ));
    }

    pub(crate) fn move_account(&mut self, d: isize) {
        let n = self.cfg.accounts.len();
        if n == 0 {
            return;
        }
        self.sel_account = (self.sel_account as isize + d).rem_euclid(n as isize) as usize;
    }

    pub(crate) fn move_session(&mut self, d: isize) {
        let n = self.session_rows().len();
        if n == 0 {
            self.sel_session = 0;
            return;
        }
        self.sel_session = (self.sel_session as isize + d).clamp(0, n as isize - 1) as usize;
    }

    pub(crate) fn on_dashboard_key(&mut self, k: KeyEvent) {
        let n = self.cfg.accounts.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('d') => self.view = View::Grid,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Left | KeyCode::Char('h') => {
                self.move_account(-1)
            }
            KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Right
            | KeyCode::Char('l')
            | KeyCode::Tab => self.move_account(1),
            KeyCode::Char('r') | KeyCode::Char('u') if n > 0 => {
                self.refresh_account(self.sel_account, true);
                self.flash("Refreshing usage");
            }
            KeyCode::Char('R') => {
                self.refresh_all(true);
                self.flash("Refreshing accounts not fetched in the last minute");
            }
            KeyCode::Char('L') | KeyCode::Char('i') if n > 0 => {
                self.login_for_account(self.sel_account);
            }
            KeyCode::Char('s') => self.open_sessions(self.sel_account),
            KeyCode::Char('n') => self.open_add_account(),
            KeyCode::Enter if n > 0 => {
                let a = self.sel_account;
                let p = self.pane_for_account(a);
                self.focus = p;
                self.assign(p, a);
                self.view = View::Grid;
            }
            _ => {}
        }
    }

    pub(crate) fn on_sessions_key(&mut self, k: KeyEvent) {
        if self.on_sessions_key2(k) {
            return;
        }
        let n = self.cfg.accounts.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.view = View::Grid,
            KeyCode::Up | KeyCode::Char('k') => self.move_session(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_session(1),
            KeyCode::PageUp => self.move_session(-15),
            KeyCode::PageDown => self.move_session(15),
            KeyCode::Home | KeyCode::Char('g') => self.sel_session = 0,
            KeyCode::End | KeyCode::Char('G') => self.move_session(isize::MAX / 2),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab if n > 0 => {
                let a = (self.sel_account + n - 1) % n;
                self.open_sessions(a);
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab if n > 0 => {
                let a = (self.sel_account + 1) % n;
                self.open_sessions(a);
            }
            KeyCode::Char('r') => self.load_sessions(self.sel_account),
            KeyCode::Char('d') => self.view = View::Dashboard,
            KeyCode::Enter => {
                let a = self.sel_account;
                if let Some(s) = self
                    .accounts
                    .get(a)
                    .and_then(|st| st.sessions.get(self.sel_session))
                {
                    let id = s.id.clone();
                    self.resume(a, id);
                }
            }
            _ => {}
        }
    }

    pub fn shutdown(&mut self) {
        if self.cfg.remember_window && crate::window::terminal_app().is_some() {
            if let Some(w) = crate::window::capture() {
                self.window = Some(w);
            }
        }
        self.stop_voice();
        crate::voice::kill_helpers();
        self.detect_sessions();
        self.save_state();
        for p in &mut self.panes {
            p.kill_all();
        }
        crate::pane::wait_for_reapers(Duration::from_millis(800));
    }

    // ---------- tabs ----------

    pub fn new_tab_prompt(&mut self, slot: usize) {
        let Some(a) = self.panes[slot].account else {
            self.flash("No account on this pane (Ctrl-a a)");
            return;
        };
        self.view = View::Grid;
        self.focus = slot;
        self.load_sessions(a);
        let rows = self.recent_rows_for(a, false);
        let base = self.cfg.base_for(a);
        self.modal = Modal::NewTab(Picker::new(
            slot,
            a,
            base,
            &self.cfg.new_tab_name_pattern,
            self.cfg.new_tab_create_folder,
            rows,
            &self.accounts[a].sessions,
        ));
    }

    pub fn recent_rows_for(&self, a: usize, all: bool) -> Vec<crate::picker::RecentRow> {
        crate::picker::recent_rows(
            &self.recents,
            &self.cfg.accounts[a].name,
            all,
            &self.accounts[a].sessions,
            self.cfg.recent_paths_limit,
        )
    }

    /// Enter in the New Tab dialog: depends on what is being edited.
    pub fn new_tab_enter(&mut self) {
        use crate::picker::{Mode, Status};
        let Modal::NewTab(pk) = self.modal.clone() else {
            return;
        };
        match pk.mode.clone() {
            Mode::Base(text) => {
                let a = pk.account;
                let path = crate::config::expand_tilde(text.trim());
                if !path.is_dir() {
                    self.flash(format!("{} is not a folder", text.trim()));
                    return;
                }
                if let Err(e) =
                    self.save_account_key(a, "new_tab_base", toml_edit::value(text.trim()))
                {
                    self.flash(format!("Could not save: {e:#}"));
                }
                self.reload_config_now();
                if let Modal::NewTab(p) = &mut self.modal {
                    p.base = path;
                    p.name = crate::picker::dated_name(
                        &self.cfg.new_tab_name_pattern,
                        &p.base,
                        chrono::Local::now(),
                    );
                    p.mode = Mode::Name;
                }
                self.flash(format!(
                    "New tabs of {} start in {}",
                    self.cfg.accounts[a].display(),
                    text.trim()
                ));
            }
            Mode::OpenExisting(text) => {
                let path = crate::config::expand_tilde(text.trim());
                if path.is_dir() {
                    self.modal = Modal::None;
                    self.open_tab(pk.slot, PickAction::NewIn(path));
                } else {
                    self.flash(format!("{} is not a folder", text.trim()));
                }
            }
            Mode::Name => {
                if let Some(c) = pk.chosen() {
                    self.modal = Modal::None;
                    self.open_tab(pk.slot, c);
                    return;
                }
                match pk.status() {
                    Status::Invalid(_) => {} // shown inline; nothing happens
                    st => {
                        let dir = pk.target();
                        if st == Status::New {
                            if let Err(e) = fs::create_dir_all(&dir) {
                                self.flash(format!("Could not create {}: {e}", dir.display()));
                                return;
                            }
                            crate::log::info(&format!("created {}", dir.display()));
                            if pk.git_init {
                                let d = dir.clone();
                                std::thread::spawn(move || {
                                    let _ = std::process::Command::new("git")
                                        .arg("init")
                                        .current_dir(d)
                                        .stdin(std::process::Stdio::null())
                                        .stdout(std::process::Stdio::null())
                                        .stderr(std::process::Stdio::null())
                                        .status();
                                });
                            }
                        }
                        self.modal = Modal::None;
                        self.open_tab(pk.slot, PickAction::NewIn(dir));
                    }
                }
            }
        }
    }

    pub fn open_tab(&mut self, slot: usize, choice: PickAction) {
        let (cwd, kind) = match choice {
            PickAction::NewIn(d) => (d, LaunchKind::Normal),
            PickAction::Resume { id, cwd } => (cwd, LaunchKind::Resume(id)),
        };
        if let LaunchKind::Resume(id) = &kind {
            if let Some(a) = self.panes[slot].account {
                return self.resume_in_new_tab(slot, a, id.clone(), cwd);
            }
        }
        let fallback = self.panes[slot]
            .account
            .map(|a| self.cfg.accounts[a].work_dir())
            .unwrap_or_else(crate::config::home_dir);
        let cwd = if cwd.is_dir() { cwd } else { fallback };
        if let Some(a) = self.panes[slot].account {
            let name = self.cfg.accounts[a].name.clone();
            let limit = self.cfg.recent_paths_limit;
            crate::picker::remember(
                &mut self.recents,
                &cwd,
                &name,
                chrono::Utc::now().timestamp(),
                limit,
            );
            self.state_dirty = true;
        }
        let s = &mut self.panes[slot];
        // Reuse an unused first tab instead of stacking an empty one.
        if s.tabs.len() == 1 && s.cur().state == PaneState::Idle {
            s.cur_mut().cwd = cwd;
            s.cur_mut().pending = None;
        } else {
            s.add_tab(cwd);
        }
        self.focus = slot;
        self.launch(slot, kind);
    }

    pub(crate) fn resume_in_new_tab(
        &mut self,
        slot: usize,
        acct: usize,
        id: String,
        cwd: std::path::PathBuf,
    ) {
        let cwd = if cwd.is_dir() {
            cwd
        } else {
            self.cfg.accounts[acct].work_dir()
        };
        let s = &mut self.panes[slot];
        if s.tabs.len() == 1 && s.cur().state == PaneState::Idle {
            s.cur_mut().cwd = cwd;
        } else {
            s.add_tab(cwd);
        }
        self.focus = slot;
        self.launch(slot, LaunchKind::Resume(id));
    }

    /// Where pane `p` shows its tabs: its own setting or the global one.
    pub fn tab_pos(&self, p: usize) -> crate::slot::TabPos {
        self.panes
            .get(p)
            .and_then(|s| s.tab_pos)
            .or_else(|| crate::slot::TabPos::parse(&self.cfg.tab_position))
            .unwrap_or(crate::slot::TabPos::Left)
    }

    /// Set (or with None, clear) pane `p`'s tab list position.
    pub fn set_tab_pos(&mut self, p: usize, pos: Option<crate::slot::TabPos>) {
        if let Some(s) = self.panes.get_mut(p) {
            s.tab_pos = pos;
            self.state_dirty = true;
        }
        let shown = self.tab_pos(p);
        self.flash(format!("Pane {} tabs: {}", p + 1, shown.label()));
    }

    /// Set account `a`'s permission mode (shown in pane `p`). Switching to
    /// bypass asks first unless `confirmed`. Saved to config.toml.
    pub fn set_permission_mode(&mut self, p: usize, a: usize, mode: &str, confirmed: bool) {
        let mode = crate::config::permission_badge(mode);
        let mode = if mode == "edits" {
            "accept-edits"
        } else {
            mode
        };
        if mode == "bypass" && !confirmed && self.cfg.mode_for(a) != "bypass" {
            self.modal = Modal::ConfirmBypass(p, a);
            return;
        }
        if a >= self.cfg.accounts.len() {
            return;
        }
        self.cfg.accounts[a].permission_mode = Some(mode.to_string());
        if let Err(e) = self.save_account_key(a, "permission_mode", toml_edit::value(mode)) {
            self.flash(format!("Could not save config: {e:#}"));
        }
        self.config_mtime = config_mtime();
        crate::log::info(&format!(
            "permission mode of {} set to {mode}",
            self.cfg.accounts[a].name
        ));
        let running = self
            .panes
            .get(p)
            .is_some_and(|s| s.account == Some(a) && s.cur().is_running());
        self.flash(format!(
            "{}: permissions {mode} for new tabs",
            self.cfg.accounts[a].display()
        ));
        if running {
            self.modal = Modal::OfferRestart(p);
        }
    }

    /// Flip auto trust for the account shown in pane `p` (saved per account).
    pub fn toggle_trust(&mut self, p: usize) {
        let Some(a) = self.panes.get(p).and_then(|s| s.account) else {
            return;
        };
        let on = !self.cfg.trust_for(a);
        self.cfg.accounts[a].auto_trust = Some(on);
        if let Err(e) = self.save_account_key(a, "auto_trust", toml_edit::value(on)) {
            self.flash(format!("Could not save config: {e:#}"));
        }
        self.config_mtime = config_mtime();
        self.flash(format!(
            "{}: auto trust folders {}",
            self.cfg.accounts[a].display(),
            if on { "on" } else { "off" }
        ));
    }

    /// Collapse or expand the tab list of pane `p` (Ctrl-a s).
    pub fn toggle_sidebar(&mut self, p: usize) {
        if let Some(s) = self.panes.get_mut(p) {
            s.sidebar_collapsed = !s.sidebar_collapsed;
            self.state_dirty = true;
        }
    }

    /// Open the rename box for (pane, tab), prefilled with its name.
    pub fn start_rename(&mut self, p: usize, t: usize) {
        if let Some(tab) = self.panes.get(p).and_then(|s| s.tabs.get(t)) {
            self.view = View::Grid;
            self.modal = Modal::Rename(p, t, tab.name());
        }
    }

    /// Give a tab a custom name; an empty name goes back to the folder name.
    pub fn rename_tab(&mut self, p: usize, t: usize, name: &str) -> String {
        let Some(tab) = self.panes.get_mut(p).and_then(|s| s.tabs.get_mut(t)) else {
            return "no such tab".into();
        };
        let clean: String = name.chars().filter(|c| !c.is_control()).take(40).collect();
        let clean = clean.trim().to_string();
        let was = tab.name();
        tab.custom_name = (!clean.is_empty()).then(|| clean.clone());
        self.state_dirty = true;
        self.tab_event(p, t, "rename", None, Some(format!("was {was}")));
        let shown = self.panes[p].tabs[t].name();
        let msg = format!("Tab {} of pane {} is now \"{shown}\"", t + 1, p + 1);
        self.flash(msg.clone());
        msg
    }

    pub(crate) fn close_tab_now(&mut self) {
        let slot = self.focus;
        let i = self.panes[slot].active;
        self.close_tab_at(slot, i);
    }

    pub(crate) fn on_approvals_key(&mut self, k: KeyEvent, sel: usize) {
        let rows = self.waiting_tabs();
        let n = rows.len();
        let sel_i = sel.min(n.saturating_sub(1));
        let cur = rows.get(sel_i).copied();
        let answer = |app: &mut App, c: crate::prompt::Choice| {
            if let Some((s, t)) = cur {
                let msg = app.answer_tab(s, t, c);
                app.flash(msg);
            }
        };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.modal = Modal::None,
            KeyCode::Up | KeyCode::Char('k') => {
                self.modal = Modal::Approvals(sel_i.saturating_sub(1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.modal = Modal::Approvals((sel_i + 1).min(n.saturating_sub(1)))
            }
            KeyCode::Char('y') | KeyCode::Char('a') => answer(self, crate::prompt::Choice::Approve),
            KeyCode::Char('A') => answer(self, crate::prompt::Choice::Always),
            KeyCode::Char('n') | KeyCode::Char('d') => answer(self, crate::prompt::Choice::Deny),
            KeyCode::Enter | KeyCode::Char('g') => {
                if let Some((s, t)) = cur {
                    self.modal = Modal::None;
                    self.jump_to(s, t);
                }
            }
            _ => {}
        }
    }

    // ---------- config hot reload ----------

    /// Save one account key into config.toml (comments kept); when there is
    /// no usable file yet, write the whole config.
    pub fn save_account_key(
        &mut self,
        a: usize,
        key: &'static str,
        item: toml_edit::Item,
    ) -> anyhow::Result<()> {
        let path = Config::path();
        if path.exists()
            && crate::settings::write(&path, &crate::settings::Key::Account(a, key), Some(item))
                .is_ok()
        {
            return Ok(());
        }
        self.cfg.save()
    }

    /// Reload config.toml right now (after the Settings screen wrote it).
    pub fn reload_config_now(&mut self) {
        self.config_mtime = None;
        self.check_config_reload();
    }

    pub fn check_config_reload(&mut self) {
        let m = config_mtime();
        if m.is_none() || m == self.config_mtime {
            return;
        }
        self.config_mtime = m;
        let text = match fs::read_to_string(Config::path()) {
            Ok(t) => t,
            Err(_) => return,
        };
        match Config::parse(&text) {
            Ok(new) => {
                self.config_error = None;
                let msg = self.apply_config(new);
                crate::log::info(&format!("config reloaded: {msg}"));
                self.flash(format!("config.toml reloaded: {msg}"));
            }
            Err(e) => {
                let e = format!("{e:#}").replace('\n', " ");
                crate::log::error(&format!("config.toml: {e}"));
                self.config_error = Some(format!("config.toml error, keeping the old config: {e}"));
            }
        }
    }

    /// Apply a newly parsed config in place. Accounts are matched by name:
    /// existing ones get new labels, colors, dirs and args; new ones are
    /// added. Returns a short summary.
    pub fn apply_config(&mut self, new: Config) -> String {
        if self.cfg.assistant != new.assistant {
            // Model, style or account changed: the next request starts fresh.
            self.assistant.brain = None;
        }
        if !cfg!(test) {
            crate::theme::set_usage_colors(&new.usage_colors);
        }
        let full = new.clone();
        let mut changes = vec![];
        for na in &new.accounts {
            match self.cfg.accounts.iter_mut().find(|a| a.name == na.name) {
                Some(a) => {
                    if a != na {
                        *a = na.clone();
                        changes.push(format!("{} updated", na.name));
                    }
                }
                None => {
                    ensure_dir(na);
                    self.cfg.accounts.push(na.clone());
                    self.accounts.push(AccountState::default());
                    changes.push(format!("{} added", na.name));
                }
            }
        }
        let removed: Vec<String> = self
            .cfg
            .accounts
            .iter()
            .filter(|a| !new.accounts.iter().any(|n| n.name == a.name))
            .map(|a| a.name.clone())
            .collect();
        if !removed.is_empty() {
            changes.push(format!("{} removed on next start", removed.join(", ")));
        }
        let voice_changed = self.cfg.voice != new.voice;
        if self.cfg.notifications != new.notifications {
            changes.push(format!(
                "notifications {}",
                if new.notifications { "on" } else { "off" }
            ));
        }
        self.cfg.notifications = new.notifications;
        self.cfg.refresh_secs = new.refresh_secs;
        self.cfg.auto_restart = new.auto_restart;
        self.cfg.autostart = new.autostart;
        self.cfg.claude_bin = new.claude_bin;
        self.cfg.grok_bin = new.grok_bin;
        self.cfg.codex_bin = new.codex_bin;
        self.cfg.cursor_bin = new.cursor_bin;
        self.cfg.antigravity_bin = new.antigravity_bin;
        self.cfg.opencode_bin = new.opencode_bin;
        self.cfg.pass_env = new.pass_env;
        self.cfg.auto_trust = new.auto_trust;
        self.cfg.trusted_dirs = new.trusted_dirs;
        if self.cfg.scrollback_lines != new.scrollback_lines {
            crate::pane::set_scrollback(new.scrollback_lines);
            changes.push("scrollback for new tabs".into());
        }
        self.cfg.scrollback_lines = new.scrollback_lines;
        if self.cfg.tab_position != new.tab_position {
            changes.push(format!("tabs {}", new.tab_position));
            self.cfg.tab_position = new.tab_position.clone();
        }
        if self.cfg.permission_mode != new.permission_mode {
            changes.push(format!("permissions {} for new tabs", new.permission_mode));
        }
        // Everything else at the top level is taken as is; accounts were
        // merged above so their indices stay stable.
        {
            let accounts = std::mem::take(&mut self.cfg.accounts);
            let voice = self.cfg.voice.clone();
            self.cfg = Config {
                accounts,
                voice,
                ..full
            };
        }
        if voice_changed {
            let tts_only = crate::voice::tts::only_tts_changed(&self.cfg.voice, &new.voice);
            self.cfg.voice = new.voice;
            changes.push("voice settings".into());
            if let Some(p) = &self.voice.preview {
                p.configure(&self.cfg.voice);
            }
            if tts_only {
                if let Some(v) = self.voice.engine.as_mut() {
                    v.configure_tts(&self.cfg.voice);
                }
            } else if self.voice.engine.is_some() {
                // Restart the engine so device, model and thresholds apply.
                let always = self.voice.always_on;
                self.stop_voice();
                self.start_voice(always);
            }
        }
        self.apply_buffers();
        // New accounts get a pane.
        self.ensure_panes();
        if changes.is_empty() {
            "no changes".into()
        } else {
            changes.join(", ")
        }
    }

    // ---------- first run ----------

    /// Walk through logging in every configured account.
    pub fn start_onboarding(&mut self) {
        let queue: Vec<usize> = (0..self.cfg.accounts.len()).collect();
        self.onboard = Some(Onboard {
            queue,
            pos: 0,
            skipped: vec![],
        });
        self.onboard_step();
    }

    /// Move to the next account that still needs a login, or finish.
    pub fn onboard_step(&mut self) {
        let Some(ob) = self.onboard.clone() else {
            return;
        };
        let next = (ob.pos..ob.queue.len()).find(|&i| {
            let a = ob.queue[i];
            !self.accounts[a].login.can_start() && !ob.skipped.contains(&a)
        });
        match next {
            None => {
                self.onboard = None;
                self.view = View::Dashboard;
                self.refresh_all(true);
                let skipped: Vec<String> = ob
                    .skipped
                    .iter()
                    .map(|&a| self.cfg.accounts[a].display().to_string())
                    .collect();
                self.maybe_start_tour();
                if skipped.is_empty() {
                    self.flash("Setup complete: every account is ready");
                } else {
                    self.flash(format!(
                        "Setup finished. Skipped {}: log in later with Ctrl-a I",
                        skipped.join(", ")
                    ));
                }
                crate::log::info("onboarding finished");
            }
            Some(i) => {
                let a = ob.queue[i];
                if let Some(o) = &mut self.onboard {
                    o.pos = i;
                }
                self.ensure_panes();
                let pane = self.pane_for_account(a);
                self.panes[pane].hidden = false;
                self.focus = pane;
                self.view = View::Grid;
                if !self.panes[pane].cur().is_running() {
                    self.launch(pane, LaunchKind::Login);
                }
            }
        }
    }

    /// Ctrl-a S during setup: skip the current account.
    pub fn onboard_skip(&mut self) {
        if let Some(o) = &mut self.onboard {
            if let Some(&a) = o.queue.get(o.pos) {
                o.skipped.push(a);
            }
        }
        self.onboard_step();
    }

    pub(crate) fn onboard_tick(&mut self) {
        let Some(ob) = &self.onboard else { return };
        let Some(&a) = ob.queue.get(ob.pos) else {
            return;
        };
        if self.accounts[a].login.logged_in() {
            let label = self.cfg.accounts[a].display().to_string();
            self.flash(format!("{label} is logged in"));
            self.onboard_step();
        }
    }

    // ---------- palette and broadcast ----------

    pub(crate) fn run_palette(&mut self, pl: crate::palette::Palette) {
        use crate::palette::Action;
        let matches = pl.matches();
        // An action whose name matches runs; anything else typed is a
        // request in plain words, for the assistant to interpret.
        let Some(&(_, action)) = matches.get(pl.sel.min(matches.len().saturating_sub(1))) else {
            let text = pl.input.trim().to_string();
            if text.is_empty() {
                return;
            }
            if self.assistant_on() {
                self.assistant.show = true;
                self.assistant.focused = true;
                self.ask_assistant(&text);
            } else {
                self.flash("No action matches, and the assistant is off (Settings > Assistant)");
            }
            return;
        };
        match action {
            Action::Key(c) => {
                self.on_command(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            Action::Broadcast => self.modal = Modal::Broadcast(String::new()),
            Action::Perm(m) => {
                let p = self.focus;
                if let Some(a) = self.panes[p].account {
                    self.set_permission_mode(p, a, m, false);
                }
            }
            Action::ToggleTrust => {
                let p = self.focus;
                self.toggle_trust(p);
            }
            Action::AccountMenu => {
                let s = self.focus;
                self.modal = Modal::AccountMenu(s, 0);
            }
            Action::VoicePushToTalk => self.push_to_talk(),
            Action::VoiceTrain => self.start_wake_training(),
            Action::Layout(l) => self.set_layout(l),
            Action::HidePane => {
                let p = self.focus;
                self.hide_pane(p);
            }
            Action::ShowHidden => self.show_hidden(),
            Action::FreeNow => self.free_now(),
            Action::AssistantNew => self.reset_assistant(),
            Action::OpenMic => self.start_open_mic(),
            Action::StopAllLoops => {
                if !self.loops.is_empty() {
                    self.modal = Modal::ConfirmStopLoops;
                }
            }
            Action::DuplicateTab => {
                let (p, t) = (self.focus, self.panes[self.focus].active);
                self.open_move_picker(p, t, true);
            }
            Action::VoicePreview => self.preview_voice(),
            Action::StopTalking => {
                if let Some(v) = &self.voice.engine {
                    v.stop_speaking();
                }
                if let Some(p) = &self.voice.preview {
                    p.stop();
                }
            }
            Action::VoiceToggleWake => self.toggle_wake_mode(),
            Action::VoiceStop => {
                self.stop_voice();
                self.voice.info = None;
                self.flash("Voice off");
            }
        }
    }

    /// Tabs a broadcast goes to: the marked running tabs, or all running.
    pub fn broadcast_targets(&self) -> Vec<(usize, usize)> {
        let running: Vec<(usize, usize)> = self
            .overview_rows()
            .into_iter()
            .filter(|&(s, t)| self.panes[s].tabs[t].is_running())
            .collect();
        let marked: Vec<(usize, usize)> = running
            .iter()
            .copied()
            .filter(|&(s, t)| self.marked.contains(&self.panes[s].tabs[t].uid))
            .collect();
        if marked.is_empty() {
            running
        } else {
            marked
        }
    }

    pub fn broadcast(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let targets = self.broadcast_targets();
        for &(s, t) in &targets {
            let uid = self.panes[s].tabs[t].uid;
            self.deliver(uid, text);
        }
        crate::log::info(&format!("broadcast to {} tab(s): {text}", targets.len()));
        self.flash(format!("Sent to {} session(s)", targets.len()));
    }

    // ---------- overview ----------

    /// Every (slot, tab) in display order.
    pub fn overview_rows(&self) -> Vec<(usize, usize)> {
        self.panes
            .iter()
            .enumerate()
            .flat_map(|(si, s)| (0..s.tabs.len()).map(move |ti| (si, ti)))
            .collect()
    }

    pub(crate) fn open_overview(&mut self) {
        let rows = self.overview_rows();
        self.sel_overview = rows
            .iter()
            .position(|&(s, t)| s == self.focus && t == self.panes[s].active)
            .unwrap_or(0);
        self.view = if self.view == View::Overview {
            View::Grid
        } else {
            View::Overview
        };
    }

    pub(crate) fn move_overview(&mut self, d: isize) {
        let n = self.overview_rows().len();
        if n > 0 {
            self.sel_overview = (self.sel_overview as isize + d).rem_euclid(n as isize) as usize;
        }
    }

    pub fn jump_to(&mut self, slot: usize, tab: usize) {
        if slot < self.panes.len() && self.panes[slot].select(tab) {
            self.focus = slot;
            self.view = View::Grid;
            self.attention.retain(|a| !(a.slot == slot && a.tab == tab));
        }
    }

    pub(crate) fn on_overview_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('o') => self.view = View::Grid,
            KeyCode::Up | KeyCode::Char('k') => self.move_overview(-1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.move_overview(1),
            KeyCode::Enter => {
                if let Some(&(s, t)) = self.overview_rows().get(self.sel_overview) {
                    self.jump_to(s, t);
                }
            }
            KeyCode::Char('t') => {
                if let Some(&(s, _)) = self.overview_rows().get(self.sel_overview) {
                    self.new_tab_prompt(s);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(&(s, t)) = self.overview_rows().get(self.sel_overview) {
                    let uid = self.panes[s].tabs[t].uid;
                    if !self.marked.remove(&uid) {
                        self.marked.insert(uid);
                    }
                    self.move_overview(1);
                }
            }
            KeyCode::Char('b') => self.modal = Modal::Broadcast(String::new()),
            _ => {}
        }
    }
}

pub(crate) fn config_mtime() -> Option<std::time::SystemTime> {
    fs::metadata(Config::path()).and_then(|m| m.modified()).ok()
}

fn ensure_dir(a: &AccountCfg) {
    let dir = a.config_dir();
    if !dir.exists() && fs::create_dir_all(&dir).is_ok() {
        #[cfg(unix)]
        {
            use crate::platform::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
    }
}

/// Login state, profile and usage of an account, for its harness.
pub fn snapshot_of(a: &AccountCfg, with_usage: bool) -> StatusSnapshot {
    let dir = a.config_dir();
    match a.harness() {
        crate::harness::Harness::Claude => snapshot(&dir, with_usage),
        crate::harness::Harness::Grok => {
            let logged = crate::harness::grok::logged_in(&dir);
            let login = LoginInfo {
                source: logged.then_some(creds::CredSource::File),
                subscription: logged.then(|| "grok".to_string()),
                ..Default::default()
            };
            let profile = creds::Profile {
                email: crate::harness::grok::email(&dir),
                onboarded: logged,
                ..Default::default()
            };
            let usage = with_usage.then(|| {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match (logged, usage::fixture_for(&name)) {
                    (false, _) => Err(UsageError::NotLoggedIn),
                    // Fixtures are read as grok's billing reply; anything
                    // else stands for the API being unavailable.
                    (true, Some(_)) => {
                        let base = crate::config::env_var("USAGE_FIXTURE").unwrap_or_default();
                        let p = std::path::Path::new(&base);
                        let p = if p.is_dir() {
                            p.join(format!("{name}.json"))
                        } else {
                            p.to_path_buf()
                        };
                        let u = std::fs::read_to_string(p)
                            .ok()
                            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                            .map(|v| crate::harness::grok::parse_billing(&v));
                        match u {
                            Some(u) if !u.windows.is_empty() => Ok(u),
                            _ => Err(UsageError::Network("billing unavailable (fixture)".into())),
                        }
                    }
                    (true, None) => crate::harness::grok::fetch_usage(&dir),
                }
            });
            StatusSnapshot {
                profile,
                login,
                usage,
            }
        }
        _ => StatusSnapshot {
            profile: Profile::default(),
            login: LoginInfo {
                cli_managed: true,
                ..Default::default()
            },
            usage: None,
        },
    }
}

/// Gather login state, profile and (optionally) usage for one config dir.
/// The access token never leaves this function.
pub fn snapshot(dir: &std::path::Path, with_usage: bool) -> StatusSnapshot {
    let profile = creds::load_profile(dir);
    let found = creds::load_creds(dir);
    let login = found
        .as_ref()
        .map(|(c, src)| LoginInfo {
            cli_managed: false,
            source: Some(*src),
            expires_at: c.expires_at,
            has_refresh: c.has_refresh,
            subscription: c.subscription_type.clone(),
            rate_tier: c.rate_limit_tier.clone(),
        })
        .unwrap_or_default();
    let usage = if !with_usage {
        None
    } else {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Some(match (&found, usage::fixture_for(&name)) {
            (None, _) => Err(UsageError::NotLoggedIn),
            (Some(_), Some(fixture)) => fixture,
            (Some((c, _)), None) if c.is_expired(Utc::now().timestamp_millis()) => {
                Err(UsageError::Unauthorized)
            }
            // One request per account across every GodTerm process.
            (Some((c, _)), None) => crate::usage_share::fetch(&name, min_fetch_interval(), || {
                usage::fetch_usage_raw(usage::USAGE_URL, &c.access_token)
            }),
        })
    };
    StatusSnapshot {
        profile,
        login,
        usage,
    }
}
