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
    UserTurn(String),
    Input(Event),
    PaneOutput,

    PaneExited(u64, u64),
    Status(usize, Box<StatusSnapshot>),

    MainGrokUsage(Box<Result<crate::usage::Usage, String>>),

    KeyRead(crossterm::event::KeyEvent, bool),

    Wheel(crossterm::event::MouseEvent, u16),

    OwnUsage(&'static str, Box<Result<crate::usage::Usage, String>>),
    Sessions(usize, Vec<SessionInfo>),
    Voice(crate::voice::VoiceEvent),

    Window(Option<crate::window::SavedWindow>),

    Mem(crate::app_settings::MemStats),

    InputClosed,

    Loops(Vec<crate::app_loops::LoopRow>),

    Control(
        String,
        serde_json::Value,
        std::sync::mpsc::Sender<serde_json::Value>,
    ),

    Assistant(u64, crate::assistant::BrainEvent),

    Web(crate::web::ToApp),

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    MediaKey(crate::media_keys::Press),

    Lease(crate::sync::takeover::Event),

    SyncApply(crate::sync::StoreApply, std::sync::mpsc::Sender<bool>),
}

#[derive(Debug, Clone, Default)]
pub struct LoginInfo {
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

    pub usage: Option<Result<Usage, UsageError>>,
}

#[derive(Default)]
pub struct AccountState {
    pub profile: Profile,
    pub login: LoginInfo,

    pub usage: Option<Usage>,

    pub usage_good_at: Option<DateTime<Utc>>,

    pub usage_err: Option<UsageError>,

    pub usage_at: Option<DateTime<Utc>>,

    pub refreshing: bool,

    inflight: bool,
    last_fetch: Option<Instant>,
    next_fetch: Option<Instant>,
    backoff: u32,
    pub sessions: Vec<SessionInfo>,
    pub sessions_loading: bool,
    pub sessions_loaded: bool,
    pub cache: Arc<Mutex<SessionCache>>,

    pub grok_screen: Option<(String, f64, DateTime<Utc>)>,

    pub usage_note: Option<String>,

    pub usage_hist: std::collections::VecDeque<(DateTime<Utc>, f64)>,
}

pub const MIN_FETCH_INTERVAL: Duration = Duration::from_secs(60);

pub fn min_fetch_interval() -> Duration {
    if crate::demo::active() {
        Duration::from_secs(2)
    } else {
        MIN_FETCH_INTERVAL
    }
}

impl AccountState {
    pub fn five_hour_left(&self) -> Option<f64> {
        self.usage
            .as_ref()
            .and_then(|u| u.get("five_hour"))
            .map(|w| w.left_now())
    }

    pub fn five_hour_reset_passed(&self) -> bool {
        self.usage
            .as_ref()
            .and_then(|u| u.get("five_hour"))
            .is_some_and(|w| w.reset_passed())
    }

    pub fn binding(&self) -> Option<(f64, &'static str, Option<chrono::DateTime<chrono::Utc>>)> {
        let u = self.usage.as_ref()?;

        let left = |w: &crate::usage::Window| w.left_now();
        let f = u.get("five_hour").map(|w| (left(w), "5h", w.resets_at));
        let w = u.get("seven_day").map(|w| (left(w), "weekly", w.resets_at));
        match (f, w) {
            (Some(a), Some(b)) => Some(if b.0 < a.0 { b } else { a }),
            (a, b) => a.or(b),
        }
    }

    pub fn usage_problem(&self, interval: Duration, now: DateTime<Utc>) -> Option<String> {
        self.usage_problem_as(interval, now, false)
    }

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

    pub fn effective_left(&self) -> Option<f64> {
        self.binding().map(|b| b.0)
    }

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

    TabHistory,

    Learned,

    LiveMap,

    Prompt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewAccount {
    pub label: String,

    pub harness: String,
    pub color: String,
    pub cwd: String,
    pub permission_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    GrokLogins(usize),

    GroupMenu(usize, usize, usize),

    GroupPick(usize, usize, usize),

    GroupName {
        slot: usize,
        group: Option<usize>,
        tabs: Vec<u64>,
        buf: String,
    },

    ConfirmCloseGroup(usize, usize),

    GroupMove(usize, usize, usize),

    ConfirmStopLoops,

    ConfirmTabJob(String),

    Browser(crate::tab_browser::BrowserDlg),

    Git(crate::app_gitpanel::GitDlg),

    GitCfg(crate::app_gitcfg::GitCfgDlg),

    SchedCard(String),

    MenuOverflow,
    None,
    Help,

    Updates(u16),

    UpdateRestart,
    ConfirmQuit,
    ConfirmClose,

    ConfirmArchive(u64),

    ArchMenu(String, usize),
    AddAccount(Box<crate::add_account::AddForm>),

    ConfirmCloseWindow(usize),

    EmptySlotMenu(usize),

    MicMenu(usize),

    ColorPick(crate::color_pick::Target, usize),
    NewTab(Picker),
    Palette(crate::palette::Palette),

    FindAll(crate::find_all::FindAll),

    Broadcast(String),

    Approvals(usize),

    ConfirmBypass(usize, usize),

    OfferRestart(usize),

    EditSetting(usize, String),

    ConfirmRemoveAccount(usize),

    ConfirmLocalVoice(&'static str, bool),

    Rename(usize, usize, String),

    AccountMenu(usize, usize),

    Tour(usize),

    WakeTrain,

    MoveTab(crate::app_tabmove::MovePicker),

    MoveBusy(crate::app_tabmove::QueuedMove),

    TabMenu(usize, usize, usize),

    PathMenu(usize),

    Menu(crate::menus::Open),

    TakeOver(String),

    SessTarget(usize),

    SessConfirm,

    SessConflict(usize),
}

pub fn unknown_key_line(c: char) -> String {
    format!("Ctrl-a {c} does nothing. Ctrl-a ? lists keys, Ctrl-a : finds actions.")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Onboard {
    pub queue: Vec<usize>,
    pub pos: usize,

    pub skipped: Vec<usize>,

    pub first: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attention {
    pub slot: usize,
    pub tab: usize,
    pub what: Activity,

    pub uid: u64,
    pub from: Activity,
}

pub const FINISHED_MIN_WORK: Duration = Duration::from_millis(1500);

pub fn attention_worthy(old: Activity, new: Activity, held: Duration) -> bool {
    match new {
        Activity::Permission => matches!(old, Activity::Working | Activity::Ready),
        Activity::Ready => old == Activity::Working && held >= FINISHED_MIN_WORK,
        _ => false,
    }
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

    pub help_scroll: usize,

    pub help_max: std::cell::Cell<usize>,

    pub sel_account: usize,
    pub sel_session: usize,

    pub sess_source: crate::app_sessions::SourceSel,
    pub sess_filter: String,
    pub sess_searching: bool,
    pub sess_marked: std::collections::HashSet<std::path::PathBuf>,
    pub sess_op: Option<crate::app_sessions::SessOp>,
    pub last_moves: Vec<crate::session_ops::Moved>,
    pub main_sessions: Vec<SessionInfo>,
    pub main_grok_sessions: Vec<SessionInfo>,

    pub sess_harness: Option<u8>,
    pub sess_subagents: bool,

    pub sess_sort: crate::sess_sort::SortKey,
    pub sess_sort_rev: bool,
    pub sess_group: crate::sess_sort::Group,
    pub sess_headless: bool,
    pub sess_sub_rows: Vec<(crate::app_sessions::Src, SessionInfo)>,
    pub main_cache: Arc<std::sync::Mutex<crate::sessions::SessionCache>>,
    pub main_loading: bool,

    pub queued_moves: Vec<crate::app_tabmove::QueuedMove>,

    pub loop_cache: Arc<std::sync::Mutex<crate::loops::LoopCache>>,

    pub assistant: crate::app_assistant::AssistantState,
    pub assistant_turn: u64,
    pub assistant_calls: usize,
    pub pending_confirms: Vec<crate::control::Pending>,

    pub tab_jobs: crate::tab_jobs::TabJobs,

    pub superseded: Vec<(String, String)>,

    pub pending_job: Option<std::sync::mpsc::Receiver<serde_json::Value>>,

    pub deliveries: Vec<crate::control::Delivery>,

    pub pending_enters: Vec<crate::control::PendingEnter>,
    pub delivery_seq: u64,

    pub last_target: Option<u64>,
    pub ctl_waits: Vec<crate::control::Waiting>,

    pub exit_clean: bool,
    pub loops: Vec<crate::app_loops::LoopRow>,
    pub loops_scanning: bool,
    pub loops_scanned: Instant,
    pub sel_loop: usize,
    pub stop_reqs: Vec<crate::app_loops::StopReq>,
    pub pending_moves: Vec<crate::app_tabmove::PendingMove>,

    pub session_index: crate::session_index::Shared,

    pub project_index: crate::project_index::Shared,

    pub takeovers: Vec<crate::takeover::TakeOver>,

    pub lease: crate::app_lease::LeaseApp,

    pub sync_ui: crate::app_sync::SyncUi,

    pub sess_live: std::collections::HashMap<String, crate::takeover::Live>,
    pub sess_live_at: Option<Instant>,

    pub opened_paths: Vec<String>,

    pub moved_ids: std::collections::HashMap<u64, u64>,

    pub bg_stops: Vec<crate::app_bglock::Stopping>,

    pub busy: crate::app_busy::State,
    pub last_tab_move: Option<crate::app_tabmove::DoneMove>,
    pub sel_overview: usize,

    pub attention: Vec<Attention>,

    pub told: crate::told::Shared,

    pub rt_show_email: Option<bool>,

    pub rt_mem_saver: Option<bool>,

    pub rt_voice: Option<String>,

    pub voice_at_quit: Option<String>,
    pub mem_ticked: Instant,

    pub rt_layout: Option<String>,
    pub ratios: crate::layout::RatioMap,
    pub last_arranged: Option<crate::layout::Arranged>,
    pub main_area: ratatui::layout::Rect,

    pub dragging: Option<crate::layout::Border>,

    pub sidebar_drag: Option<usize>,

    pub pane_drag: Option<(usize, bool)>,

    pub output_at: std::collections::VecDeque<Instant>,

    pub closed: Vec<crate::closed::ClosedTab>,
    pub closed_seq: u64,

    pub archive: crate::archive::Store,
    pub archive_collapsed: bool,
    pub archive_sel: Option<String>,
    pub archive_clock: Option<i64>,
    pub freed_kb: u64,

    pub open_home_ok: bool,

    pub corrections: std::cell::RefCell<Vec<(String, String)>>,

    pub th: crate::app_tabhist::TabHistUi,

    pub learned_ui: crate::app_learned::LearnedUi,

    pub livemap: crate::livemap::LiveMap,
    pub prompt_ui: crate::app_sysprompt::PromptUi,

    pub forced_stops: Vec<String>,

    pub main_grok_usage: Option<Result<crate::usage::Usage, String>>,

    pub own_usage: std::collections::HashMap<&'static str, Result<crate::usage::Usage, String>>,
    pub own_usage_at: Option<Instant>,

    pub bg_cache: std::cell::RefCell<std::collections::HashMap<u64, (Instant, Option<String>)>>,

    pub native_titles:
        std::cell::RefCell<std::collections::HashMap<String, (Instant, Option<String>)>>,

    pub tab_search: Option<usize>,

    pub tab_drag: Option<(usize, usize, bool)>,
    pub undo_toast: Option<(Instant, String)>,

    pub tab_list_width: Option<String>,

    pub num_entry: Option<String>,

    pub model_entry: Option<usize>,

    pub model_switches: Vec<crate::app_tabmodel::ModelSwitch>,

    pub loops_drawer: std::collections::HashMap<usize, u16>,

    pub loops_drawer_focus: Option<usize>,
    pub loops_drawer_sel: usize,

    pub loops_drawer_drag: Option<usize>,

    pub loops_drawer_geo: std::cell::RefCell<std::collections::HashMap<usize, Rect>>,

    pub loops_confirm: Option<(u64, String, Instant)>,

    pub jobs: crate::jobs::Store,
    pub job_runs: Vec<crate::app_jobs::JobRun>,
    pub job_clock: Option<i64>,
    pub jobs_boot: Instant,

    pub attn_clock_ms: std::cell::Cell<Option<u64>>,
    pub jobs_ticked: Instant,

    pub sched_open: bool,
    pub sched_confirm: Option<(String, Instant)>,

    pub sched_ui: crate::app_sched::SchedUi,
    pub rt_privacy: Option<bool>,

    pub restore_queue: std::collections::VecDeque<(usize, u64)>,
    pub(crate) restore_next: Instant,

    restore_done: (usize, std::collections::BTreeSet<usize>),

    pub marked: std::collections::HashSet<u64>,

    pub window: Option<crate::window::SavedWindow>,
    last_window_check: Instant,

    pub recents: Vec<crate::picker::RecentDir>,
    pub flash: Option<(String, Instant)>,
    pub quit: bool,

    pub want_cursor: std::cell::Cell<Option<(u16, u16)>>,

    pub update: crate::app_update::UpdateUi,

    pub admin: crate::app_admin::AdminState,

    pub acct: crate::app_accounts::AcctState,

    pub setup: crate::app_setup::SetupState,

    pub browser: crate::app_browser::BrowserState,

    pub git: crate::app_gitpanel::GitState,

    pub force_eager: bool,

    pub web: Option<crate::web::app_web::WebSide>,

    pub web_caller: bool,

    pub web_retry_at: Option<std::time::Instant>,
    pub pane_rects: Vec<Rect>,

    pub term_rects: Vec<Rect>,

    pub panel_overlay: std::cell::Cell<bool>,

    pub updates_max_scroll: std::cell::Cell<u16>,

    pub menu_shown: std::cell::Cell<bool>,

    pub failover: Vec<crate::app_failover::Offer>,

    pub failover_declined: Vec<(u64, usize)>,

    pub limit_hit: std::collections::HashSet<u64>,

    pub key_burst: Option<bool>,

    pub hits: std::cell::RefCell<crate::hits::Hits>,

    pub modal_from: std::cell::Cell<Option<usize>>,

    pub inline_rename: std::cell::Cell<bool>,

    pub menu_overflow: std::cell::RefCell<Vec<(String, crate::hits::UiAction)>>,

    pub last_hits: crate::hits::Hits,
    pub last_modal_from: Option<usize>,

    pub mouse_pos: Option<(u16, u16)>,

    pub wheel_n: u16,

    pub wheel_last: Option<(Instant, bool)>,
    pub last_click: Option<(Instant, crate::hits::UiAction)>,

    pub selection: Option<crate::select::Selection>,

    pub sel_press: Option<crate::select::Press>,

    pub assistant_text: std::cell::RefCell<Option<(Rect, Vec<String>)>>,

    pub last_copied: Option<String>,

    pub mouse_capture: bool,
    pub tx: Sender<AppEvent>,
    pub voice: crate::app_voice::VoiceState,

    pub direct: crate::app_direct::DirectState,

    pub brainstorm: crate::app_brainstorm::BrainstormState,

    pub updates: crate::app_updates::UpdatesState,

    pub onb: crate::app_onboarding::OnbState,

    pub media: crate::media_keys::MediaKeys,

    pub(crate) config_mtime: Option<std::time::SystemTime>,
    last_config_check: Instant,

    pub config_error: Option<String>,

    pub settings_section: usize,
    pub settings_sel: usize,

    pub settings_query: Option<String>,

    pub settings_collapsed: std::collections::HashSet<&'static str>,

    pub mem: crate::app_settings::MemStats,
    last_mem: Instant,

    pub onboard: Option<Onboard>,
    last_status: Instant,
    last_session_scan: Instant,
    pub(crate) state_dirty: bool,
    last_state_save: Instant,

    pub cont: crate::app_continue::ContinueState,
}

impl App {
    pub fn new(cfg: Config, tx: Sender<AppEvent>) -> App {
        if !cfg!(test) {
            crate::theme::set_usage_colors(&cfg.usage_colors);
        }
        let accounts = cfg
            .accounts
            .iter()
            .map(|_| AccountState::default())
            .collect();

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
            help_scroll: 0,
            help_max: std::cell::Cell::new(0),
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
            tab_jobs: Default::default(),
            superseded: vec![],
            pending_job: None,
            deliveries: vec![],
            pending_enters: vec![],
            delivery_seq: 0,
            last_target: None,
            ctl_waits: vec![],
            exit_clean: false,
            loops: vec![],
            loops_scanning: false,
            loops_scanned: crate::clock::ago(std::time::Duration::from_secs(60)),
            sel_loop: 0,
            stop_reqs: vec![],
            pending_moves: vec![],
            moved_ids: Default::default(),
            bg_stops: vec![],
            busy: Default::default(),
            opened_paths: vec![],
            takeovers: vec![],
            lease: Default::default(),
            sync_ui: Default::default(),
            sess_live: Default::default(),
            sess_live_at: None,
            session_index: Default::default(),
            project_index: Default::default(),
            last_tab_move: None,
            sel_overview: 0,
            attention: vec![],
            told: Default::default(),
            rt_show_email: None,
            rt_mem_saver: None,
            rt_voice: None,
            voice_at_quit: None,
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
            archive: crate::archive::Store::load(),
            archive_collapsed: true,
            archive_sel: None,
            archive_clock: None,
            freed_kb: 0,
            open_home_ok: false,
            corrections: Default::default(),
            th: Default::default(),
            learned_ui: Default::default(),
            livemap: Default::default(),
            prompt_ui: Default::default(),
            tab_drag: None,
            bg_cache: Default::default(),
            native_titles: Default::default(),
            tab_search: None,
            main_grok_usage: None,
            own_usage: Default::default(),
            own_usage_at: None,
            forced_stops: vec![],
            undo_toast: None,
            tab_list_width: None,
            num_entry: None,
            model_entry: None,
            model_switches: vec![],
            loops_drawer: Default::default(),
            loops_drawer_focus: None,
            loops_drawer_sel: 0,
            loops_drawer_drag: None,
            loops_drawer_geo: Default::default(),
            loops_confirm: None,
            jobs: crate::jobs::Store::load(),
            job_runs: vec![],
            job_clock: None,
            jobs_boot: Instant::now(),
            attn_clock_ms: std::cell::Cell::new(None),
            jobs_ticked: Instant::now(),
            sched_open: true,
            sched_confirm: None,
            sched_ui: Default::default(),
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
            acct: Default::default(),
            setup: Default::default(),
            browser: Default::default(),
            git: Default::default(),
            force_eager: false,
            web: None,
            web_caller: false,
            web_retry_at: None,
            pane_rects: vec![],
            term_rects: vec![],
            panel_overlay: Default::default(),
            key_burst: None,
            updates_max_scroll: Default::default(),
            menu_shown: Default::default(),
            failover: vec![],
            failover_declined: vec![],
            limit_hit: Default::default(),
            hits: Default::default(),
            modal_from: Default::default(),
            inline_rename: Default::default(),
            menu_overflow: Default::default(),
            last_hits: Default::default(),
            last_modal_from: None,
            mouse_pos: None,
            wheel_n: 1,
            wheel_last: None,
            last_click: None,
            selection: None,
            sel_press: None,
            assistant_text: Default::default(),
            last_copied: None,
            mouse_capture: true,
            tx,
            voice: Default::default(),
            direct: Default::default(),
            brainstorm: Default::default(),
            updates: Default::default(),
            onb: Default::default(),
            media: Default::default(),
            onboard: None,
            settings_section: 0,
            settings_sel: 0,
            settings_query: None,
            settings_collapsed: crate::settings::COLLAPSED_BY_DEFAULT
                .iter()
                .copied()
                .collect(),
            mem: Default::default(),
            last_mem: crate::clock::ago(Duration::from_secs(60)),
            config_mtime: config_mtime(),
            last_config_check: Instant::now(),
            config_error: None,
            last_status: Instant::now(),
            last_session_scan: Instant::now(),
            state_dirty: false,
            last_state_save: Instant::now(),
            cont: Default::default(),
        };
        for a in &app.cfg.accounts {
            ensure_dir(a);
        }

        for i in 0..app.accounts.len() {
            let snap = snapshot_of(&app.cfg.accounts[i], false);
            app.apply_status(i, snap);
        }

        if !cfg!(test) && !crate::demo::active() {
            for (i, st) in app.accounts.iter_mut().enumerate().skip(1) {
                st.next_fetch = Some(Instant::now() + Duration::from_secs(3 * i as u64));
            }
        }
        app
    }

    pub fn autostart(&mut self) {
        if let Some(st) = AppState::load() {
            self.restore(st);
        }
        if !self.cfg.autostart {
            return;
        }
        self.plan_continue();
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

        self.start_tabs_with_requests();
    }

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

    pub fn restore(&mut self, st: AppState) {
        let mut panes = vec![];
        let mut focus = None;

        let saved_max = st
            .slots
            .iter()
            .flat_map(|s| s.tabs.iter())
            .filter_map(|t| t.uid)
            .max()
            .unwrap_or(0);

        crate::pane::reserve_uids_below(
            st.next_uid
                .unwrap_or(0)
                .max(saved_max + 1)
                .max(crate::pane::reserved_on_disk()),
        );

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
                tab.browser = t.browser.clone();
                tab.browser_also =
                    crate::tab_browser::clean_also(t.browser.as_deref(), &t.browser_also);
                tab.finished = t.finished;
                if let Some(o) = t.opened {
                    tab.opened = std::time::UNIX_EPOCH + std::time::Duration::from_secs(o);
                }
                if let Some(u) = t.uid.filter(|u| taken.insert(*u)) {
                    tab.uid = u;
                }
                self.cont.read_mark(tab.uid, t);
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
        self.rt_voice = st.voice.clone();
        self.rt_privacy = st.privacy;
        self.restore_deliveries(&st.deliveries);
        self.sched_open = !st.sched_collapsed;
    }

    pub fn snapshot_state(&self) -> AppState {
        AppState {
            show_email: self.rt_show_email,
            memory_saver: self.rt_mem_saver,
            voice: Some(
                self.voice_at_quit
                    .clone()
                    .unwrap_or_else(|| self.voice_mode_now().into()),
            ),
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
            deliveries: self.saved_deliveries(),
            sched_collapsed: !self.sched_open,
            window: self.window.clone(),
            recents: self.recents.clone(),
            focus: self.focus,
            next_uid: Some(crate::pane::next_uid()),
            slots: self
                .panes
                .iter()
                .map(|s| SlotState {
                    account: s.account.map(|a| self.cfg.accounts[a].name.clone()),

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
                            browser: t.browser.clone(),
                            browser_also: t.browser_also.clone(),
                            finished: t.finished,
                            opened: t
                                .opened
                                .duration_since(std::time::UNIX_EPOCH)
                                .ok()
                                .map(|d| d.as_secs()),
                            ..self.working_mark(t)
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
                    .chain(self.busy.fork_ids.iter().cloned())
                    .collect();
                if let Some(id) = state::detect_session(&dir, &t.cwd, t.started, &taken) {
                    if t.session_id.as_deref() != Some(&id) {
                        self.panes[si].tabs[ti].session_id = Some(id.clone());
                        self.state_dirty = true;

                        self.lease_claim(si, ti, &id);
                    }
                }
            }
        }
    }

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
        let msg = msg.into();
        self.told_record(
            "flash",
            &msg,
            crate::told::Channel::Flash,
            crate::told::is_ui_hint(&msg),
        );
        self.flash = Some((msg, Instant::now()));
    }

    pub fn account_cfg(&self, idx: usize) -> &AccountCfg {
        &self.cfg.accounts[idx]
    }

    pub fn account_color(&self, idx: Option<usize>) -> ratatui::style::Color {
        idx.map(|i| theme::parse_color(&self.cfg.accounts[i].color))
            .unwrap_or(theme::STONE)
    }

    pub fn launch(&mut self, pane: usize, kind: LaunchKind) {
        let Some(acct) = self.panes[pane].account else {
            self.flash("No account assigned to this pane (Ctrl-a a to assign)");
            return;
        };

        if self.lease_gate(pane, &kind) {
            return;
        }
        let a = self.cfg.accounts[acct].clone();
        let dir = a.config_dir();
        if creds::is_default_dir(&dir) {
            self.flash("Refusing to use the default ~/.claude config dir");
            return;
        }
        ensure_dir(&a);
        let h = a.harness();

        let mut args = h.base_args(&dir);
        args.extend(h.permission_args(&self.cfg.mode_for(acct)));
        args.extend(a.args.iter().cloned());

        let mut tab_env = vec![];
        if kind != LaunchKind::Login {
            let uid = self.panes[pane].cur().uid;
            let plan = self.cfg.mode_for(acct) == "plan";
            tab_env = self.tab_jobs_launch(h, uid, &dir, &mut args, plan);
        }
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
                if creds::load_profile(&dir).onboarded {
                    type_when_idle = Some(b"/login\r".to_vec());
                }
            }
            LaunchKind::Login => {}
            LaunchKind::Resume(id)
                if h == crate::harness::Harness::Claude
                    && crate::sessions::claude_session_is_empty(&dir, id) =>
            {
                crate::log::info(&format!(
                    "launch: session {id} has no messages, starting a new conversation"
                ));
                self.flash("That conversation was empty: started a new one");
            }
            LaunchKind::Resume(id) => args.extend(h.resume_args(id)),
        }

        let next = self.panes[pane].cur_mut().next_launch.take();
        self.panes[pane].cur_mut().bg_lock = None;
        match (&next, &kind, h) {
            (
                Some(crate::session_ids::NextLaunch::Attach(short)),
                _,
                crate::harness::Harness::Claude,
            ) => {
                args = vec!["attach".into(), short.clone()];
            }
            (
                Some(crate::session_ids::NextLaunch::Fork),
                LaunchKind::Resume(_),
                crate::harness::Harness::Claude,
            ) => args.push("--fork-session".into()),
            _ => {}
        }
        if let LaunchKind::Resume(id) = &kind {
            if let Some(s) = self.accounts[acct].sessions.iter().find(|s| &s.id == id) {
                let p = std::path::PathBuf::from(&s.cwd);
                if p.is_dir() {
                    cwd = p;
                }
            }
        }
        let mut extra_env: Vec<(String, String)> = if h == crate::harness::Harness::Grok {
            crate::harness::grok::write_slot_compat(&dir, &self.cfg.grok_claude_compat);
            self.cfg
                .grok_claude_compat
                .env()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        } else {
            vec![]
        };
        extra_env.extend(tab_env);
        let spec = LaunchSpec {
            program: self.cfg.harness_bin(h),
            home_env: h.home_env(),
            extra_env,
            args,
            cwd,
            config_dir: dir,
            kind: kind.clone(),
            pass_env: self.cfg.pass_env.clone(),
            type_when_idle,
            harness: h,
        };
        self.state_dirty = true;

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
            "launch pane {} tab {} account {} {:?}{} in {}",
            pane + 1,
            self.panes[pane].active + 1,
            a.name,
            kind,
            match &next {
                Some(n) => format!(" ({n:?})"),
                None => String::new(),
            },
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
            harness: crate::harness::Harness::Claude,
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

    pub(crate) fn pane_for_account(&self, acct: usize) -> usize {
        self.panes
            .iter()
            .position(|p| p.account == Some(acct))
            .unwrap_or(self.focus)
    }

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

        let cur = self.panes[pane].cur();
        let other = cur.session_id.as_deref().is_some_and(|s| s != session_id);
        if cur.is_running() || other {
            self.panes[pane].add_tab(cwd);
        } else {
            self.panes[pane].cur_mut().cwd = cwd;
        }
        self.launch(pane, LaunchKind::Resume(session_id));
        self.focus = pane;
        self.view = View::Grid;
    }

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

    pub(crate) fn add_account(&mut self, spec: NewAccount) {
        let taken: Vec<String> = self.cfg.accounts.iter().map(|a| a.name.clone()).collect();

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

        self.config_mtime = config_mtime();
        let idx = self.cfg.accounts.len() - 1;
        self.sel_account = idx;

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

    pub fn refresh_all(&mut self, with_usage: bool) {
        for i in 0..self.accounts.len() {
            self.refresh_account(i, with_usage);
        }
        self.last_status = Instant::now();
    }

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

    pub fn tick(&mut self) {
        self.voice_mode_tick();
        let now = self.voice_mode_now();
        if self.voice_at_quit.is_none() && self.rt_voice.as_deref() != Some(now) {
            self.rt_voice = Some(now.into());
            self.state_dirty = true;
        }
        self.selection_tick();
        self.headphone_tap_tick();
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
        self.continue_tick();
        self.tabmove_tick();
        self.bglock_tick();
        self.refresh_loops(false);
        self.stop_tick();
        self.memory_tick();
        self.assistant_tick();
        self.update_tick();
        self.admin_tick();
        self.acct_tick();
        self.onboarding_tick();
        self.browser_tick();
        self.git_tick();
        self.control_tick();
        self.jobs_tick();
        self.tab_jobs_tick();
        self.sync_tick();
        self.lease_tick();
        self.local_voices_tick();
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
                let held = t.activity_since.elapsed();
                let visible = si == self.focus && ti == active && self.view == View::Grid;
                if let Some((old, new)) = t.update_activity() {
                    let bg = new == Activity::Ready
                        && attention_worthy(old, new, held)
                        && crate::app_continue::background_busy(&t.screen_text());
                    if bg {
                        t.bg_wait = true;
                        t.bg_wait_unseen = !visible;
                        crate::log::info(&format!(
                            "attention: t{} turn ended with background work still running: not finished yet",
                            t.uid
                        ));
                    }
                    if new == Activity::Working {
                        t.bg_wait = false;
                    }

                    if !bg && new == Activity::Ready && attention_worthy(old, new, held) && !visible
                    {
                        t.finished = true;
                        self.state_dirty = true;
                    }

                    if !bg && attention_worthy(old, new, held) {
                        let uid = t.uid;
                        let dup = self.attention.iter().any(|a| a.uid == uid && a.what == new);
                        if !visible && !dup {
                            self.attention.push(Attention {
                                slot: si,
                                tab: ti,
                                what: new,
                                uid,
                                from: old,
                            });
                        }
                    }
                }

                if t.bg_wait
                    && t.activity == Activity::Ready
                    && !crate::app_continue::background_busy(&t.screen_text())
                {
                    t.bg_wait = false;
                    crate::log::info(&format!(
                        "attention: t{} background work done: finished",
                        t.uid
                    ));
                    let uid = t.uid;
                    if t.bg_wait_unseen || !visible {
                        t.finished = !visible;
                        self.state_dirty = true;
                        let dup = self
                            .attention
                            .iter()
                            .any(|a| a.uid == uid && a.what == Activity::Ready);
                        if !visible && !dup {
                            self.attention.push(Attention {
                                slot: si,
                                tab: ti,
                                what: Activity::Ready,
                                uid,
                                from: Activity::Working,
                            });
                        }
                    }
                }

                if t.finished && (visible || t.activity == Activity::Working) {
                    t.finished = false;
                    self.state_dirty = true;
                }
            }
        }

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
        self.direct_tick();
        self.voice_tick();
        self.onboard_tick();

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

                if k.kind != KeyEventKind::Release && self.onboarding_key(&k) {
                    return;
                }
                if k.kind != KeyEventKind::Release {
                    let before = self.focus;
                    self.on_key(k);
                    self.expand_on_focus(before);
                }
            }
            AppEvent::Input(Event::Paste(text)) => {
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
            AppEvent::Input(Event::Mouse(m)) if self.onboarding_mouse(&m) => {}
            AppEvent::Input(Event::Mouse(m)) => self.on_mouse(m),
            AppEvent::Wheel(..) if self.onb_full_screen() => {}
            AppEvent::Wheel(m, n) => self.on_wheel(m, n),
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
                        let who = self.spoken_tab_name(uid);
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
                                self.flash(format!("{who} exited with code {code}; restarting"));
                            } else {
                                self.flash(format!(
                                    "{who} exited with code {code}; Enter restarts it"
                                ));
                            }
                        }
                        if was_running && !was_login {
                            self.note_bg_lock(si, ti);
                            self.learn_exit_session(si, ti);
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
            AppEvent::MediaKey(p) => self.headphone_tap(p),
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
                        timeout_error: w["timeout_error"].as_str().map(str::to_string),
                    });
                    self.control_tick();
                } else {
                    let _ = reply.send(v);
                }
            }
            AppEvent::Assistant(gen, ev) => {
                if self.assistant.brain.as_ref().is_some_and(|b| b.gen == gen) {
                    self.on_brain(ev);
                }
            }
            AppEvent::Web(ev) => self.web_event(ev),
            AppEvent::Lease(e) => self.lease_event(e),
            AppEvent::SyncApply(a, reply) => self.sync_apply(a, reply),
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

        if self.model_entry.is_some() && self.model_entry_key(&k) {
            return;
        }

        if self.modal == Modal::None
            && self.view == View::Grid
            && self.loops_drawer_focus.is_some()
            && self.loops_drawer_key(&k)
        {
            return;
        }

        if self.modal == Modal::None && self.sched_ui.focus && self.sched_key(&k) {
            return;
        }

        match &mut self.modal {
            Modal::None => {}
            Modal::SchedCard(_) => {
                self.on_sched_card_key(k);
                return;
            }
            Modal::FindAll(_) => {
                self.on_find_all_key(k);
                return;
            }
            Modal::Help => {
                let n = self.help_max.get();
                match k.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.help_scroll = (self.help_scroll + 1).min(n)
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.help_scroll = self.help_scroll.saturating_sub(1)
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        self.help_scroll = (self.help_scroll + 15).min(n)
                    }
                    KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(15),
                    KeyCode::Home => self.help_scroll = 0,
                    _ => {
                        self.help_scroll = 0;
                        self.modal = Modal::None;
                    }
                }
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
            Modal::ConfirmArchive(uid) => {
                let uid = *uid;
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.confirm_archive(uid);
                } else {
                    self.modal = Modal::None;
                }
                return;
            }
            Modal::ArchMenu(id, sel) => {
                let (id, sel) = (id.clone(), *sel);
                let rows = App::archive_menu_rows();
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Up => self.modal = Modal::ArchMenu(id, sel.saturating_sub(1)),
                    KeyCode::Down => {
                        self.modal = Modal::ArchMenu(id, (sel + 1).min(rows.len() - 1))
                    }
                    KeyCode::Enter => self.archive_menu_run(&id, sel),
                    KeyCode::Char(c) => {
                        if let Some(i) = rows.iter().position(|r| r.1 == c) {
                            self.archive_menu_run(&id, i);
                        }
                    }
                    _ => {}
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
                        let snap = pl.clone();
                        let n = crate::palette::items(self, &snap).len();
                        if let Modal::Palette(pl) = &mut self.modal {
                            if n > 0 {
                                pl.sel = (pl.sel + 1) % n;
                            }
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
                    self.quit_by_user();
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
            Modal::MicMenu(_) => {
                self.modal = Modal::None;
                match k.code {
                    KeyCode::Char('0') | KeyCode::Char('a') => {
                        self.end_direct("mic menu", true);
                    }
                    KeyCode::Char(c @ '1'..='9') => {
                        if let Some((uid, _)) = self.mic_targets().get((c as u8 - b'1') as usize) {
                            let uid = *uid;
                            self.mic_move_to(uid);
                        }
                    }
                    _ => {}
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
            Modal::ConfirmTabJob(_) => {
                let Modal::ConfirmTabJob(tok) = std::mem::replace(&mut self.modal, Modal::None)
                else {
                    return;
                };
                let yes = matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                );
                self.tab_ask_answer(&tok, yes);
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
            Modal::Browser(_) => {
                self.on_browser_key(k);
                return;
            }
            Modal::Git(_) => {
                self.on_git_key(k);
                return;
            }
            Modal::GitCfg(_) => {
                self.on_gitcfg_key(k);
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
            Modal::ConfirmLocalVoice(id, remove) => {
                let (id, remove) = (*id, *remove);
                self.modal = Modal::None;
                if matches!(
                    k.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                ) {
                    self.local_voice_yes(id, remove);
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

        if self.on_tab_search_key(k) {
            return;
        }

        if self.selection_key(&k) {
            return;
        }

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

        if matches!(k.code, KeyCode::Char('I') | KeyCode::Char('i'))
            && self.panes[self.focus].cur().state != PaneState::Running
        {
            if let Some(id) = self.agent_missing(account) {
                self.open_setup();
                self.setup_click(id);
                return;
            }
        }

        let (fp, ft) = (self.focus, self.panes[self.focus].active);
        if self.panes[fp].tabs[ft]
            .lease
            .as_ref()
            .is_some_and(|l| l.blocks_input())
            && self.lease_key(fp, ft, k)
        {
            return;
        }
        let pane = self.panes[self.focus].cur_mut();
        match pane.state {
            PaneState::Running => {
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

                if pane.web_size.is_some() {
                    let uid = pane.uid;
                    self.web_size_back(uid);
                }
            }
            _ if pane.bg_lock.is_some() => {
                let (p, t) = (self.focus, self.panes[self.focus].active);
                self.bg_lock_key(p, t, k);
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

    #[allow(clippy::collapsible_match)]
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
            (_, Some('#')) => self.start_model_entry(),
            (_, Some(';')) => {
                let p = self.focus;
                self.toggle_loops_drawer(p);
            }
            (_, Some('i')) => {
                let p = self.focus;
                self.open_browser_dialog(p);
            }
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
            (_, Some('f')) => {
                let (s, t) = (self.focus, self.panes[self.focus].active);
                self.request_archive(s, t);
            }
            (_, Some('W')) => {
                if let Err(e) = self.reopen_closed() {
                    self.flash(format!("Nothing to reopen: {e}"));
                }
            }
            (_, Some('n')) => {
                self.view = View::Grid;
                let top = self.ungrouped_top();
                let only = self.tab_filter(self.focus);
                self.panes[self.focus].step_tab(top, 1, only.as_deref());
            }
            (_, Some('p')) => {
                self.view = View::Grid;
                let top = self.ungrouped_top();
                let only = self.tab_filter(self.focus);
                self.panes[self.focus].step_tab(top, -1, only.as_deref());
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
            (_, Some('*')) => self.open_find_all(),
            (_, Some('Z')) => self.toggle_memory_saver(),
            (_, Some('X')) => self.toggle_mute(),
            (_, Some('O')) => self.toggle_speaker(),
            (_, Some('K')) => self.toggle_direct(),
            (_, Some('B')) => self.toggle_brainstorm(),
            (_, Some('D')) => self.open_updates(),
            (_, Some('F')) => {
                if self.accept_failover(None, false).is_none() {
                    self.flash("No move on offer");
                }
            }
            (_, Some('+')) | (_, Some('=')) => self.nudge_volume(0.1),
            (_, Some('-')) => self.nudge_volume(-0.1),
            (_, Some('.')) => self.toggle_assistant_panel(),
            (_, Some('{')) => {
                self.nudge_panel_width(crate::app_panelwin::STEP as i32);
            }
            (_, Some('}')) => {
                self.nudge_panel_width(-(crate::app_panelwin::STEP as i32));
            }
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
            (_, Some('Q')) => self.quit_by_user(),
            (_, Some('N')) => self.restart_to_update(false),
            (_, Some('<')) | (_, Some('>')) => {
                self.view = View::Grid;
                let p = self.focus;
                self.nudge_sidebar(p, if ch == Some('>') { 4 } else { -4 });
            }
            (_, Some('/')) => {
                let p = self.focus;
                self.focus_tab_search(p);
            }
            (_, Some('_')) => {
                self.view = View::Grid;
                let p = self.focus;
                self.toggle_fold_pane(p);
            }
            (_, Some('\\')) => {
                self.view = View::Grid;
                let p = self.focus;
                self.toggle_fold_sideways(p);
            }
            (_, Some('J')) => self.onboarding_ctrl_j(),
            (_, Some('C')) => {
                if let Some(a) = self.panes.get(self.focus).and_then(|p| p.account) {
                    let say = self.close_accounts(&[a]);
                    self.flash(say);
                }
            }
            (_, Some(c)) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.flash(unknown_key_line(c));
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

    pub fn quit_by_user(&mut self) {
        self.quit = true;
        self.exit_clean = true;
    }

    pub fn shutdown(&mut self) {
        if self.cfg.remember_window && crate::window::terminal_app().is_some() {
            if let Some(w) = crate::window::capture() {
                self.window = Some(w);
            }
        }

        let tx = self.tx.clone();
        self.media.set(false, &tx);

        self.voice_at_quit = Some(self.voice_mode_now().into());
        self.stop_voice();
        crate::voice::kill_helpers();
        self.detect_sessions();
        self.save_state();
        crate::app_resume::mark_clean_exit();
        for p in &mut self.panes {
            p.kill_all();
        }
        crate::pane::wait_for_reapers(Duration::from_millis(800));
    }

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
                    Status::Invalid(_) => {}
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

    pub fn tab_pos(&self, p: usize) -> crate::slot::TabPos {
        self.panes
            .get(p)
            .and_then(|s| s.tab_pos)
            .or_else(|| crate::slot::TabPos::parse(&self.cfg.tab_position))
            .unwrap_or(crate::slot::TabPos::Left)
    }

    pub fn set_tab_pos(&mut self, p: usize, pos: Option<crate::slot::TabPos>) {
        if let Some(s) = self.panes.get_mut(p) {
            s.tab_pos = pos;
            self.state_dirty = true;
        }
        let shown = self.tab_pos(p);
        self.flash(format!("Pane {} tabs: {}", p + 1, shown.label()));
    }

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

    pub fn toggle_sidebar(&mut self, p: usize) {
        if let Some(s) = self.panes.get_mut(p) {
            s.sidebar_collapsed = !s.sidebar_collapsed;
            self.state_dirty = true;
        }
    }

    pub fn start_rename(&mut self, p: usize, t: usize) {
        if let Some(tab) = self.panes.get(p).and_then(|s| s.tabs.get(t)) {
            self.view = View::Grid;
            self.modal = Modal::Rename(p, t, tab.name());
        }
    }

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

    pub fn apply_config(&mut self, new: Config) -> String {
        let mut same = new.assistant.clone();
        same.remote_control = self.cfg.assistant.remote_control;
        same.resume = self.cfg.assistant.resume;
        if self.cfg.assistant != same {
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
        self.cfg.restore_continue_working = new.restore_continue_working;
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

        let old_web = self.cfg.web.clone();
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
                let always = self.voice.always_on;
                self.stop_voice();
                self.start_voice(always);
            }
        }
        if self.cfg.git != new.git {
            self.cfg.git = new.git;
            changes.push("GitHub owners and folder rules".into());
        }
        if let Some(c) = self.web_config_changed(&old_web) {
            changes.push(c);
        }
        self.apply_buffers();

        self.ensure_panes();
        if changes.is_empty() {
            "no changes".into()
        } else {
            changes.join(", ")
        }
    }

    pub fn start_onboarding(&mut self) {
        let queue: Vec<usize> = (0..self.cfg.accounts.len()).collect();
        self.onboard = Some(Onboard {
            queue,
            pos: 0,
            skipped: vec![],
            first: false,
        });
        self.onboard_step();
    }

    pub fn onboard_step(&mut self) {
        let Some(ob) = self.onboard.clone() else {
            return;
        };
        let next = (ob.pos..ob.queue.len()).find(|&i| {
            let a = ob.queue[i];
            !self.accounts[a].login.can_start() && !ob.skipped.contains(&a)
        });
        match next {
            None if ob.first => {
                self.onboard = None;
                self.view = View::Grid;
                self.refresh_all(true);
                let label = ob
                    .queue
                    .first()
                    .and_then(|&a| self.cfg.accounts.get(a))
                    .map(|a| a.display().to_string())
                    .unwrap_or_default();
                self.maybe_start_tour();
                self.flash(crate::first_run::ready_line(&label, ob.skipped.is_empty()));
                crate::log::info("first account set up");
            }
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

    pub(crate) fn run_palette(&mut self, pl: crate::palette::Palette) {
        use crate::palette::Action;
        let matches = crate::palette::items(self, &pl);

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
            Action::UpdateMode => self.toggle_update_mode(),
            Action::VoiceOnboarding => self.start_voice_onboarding(false),
            Action::VoicePhrases => self.onb.card = true,
            Action::Setup => self.open_setup(),
            Action::UpdatesNow => {
                self.assistant.show = true;
                self.updates_roundup_now();
            }
            Action::PaneWidth(0) => self.equal_pane_widths(),
            Action::PaneWidth(d) => {
                let p = self.focus;
                self.resize_pane_width(p, d > 0);
            }
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
            Action::FindAll => self.open_find_all(),
            Action::GoTab(uid) => {
                if let Some((s, t)) = self.find_tab(uid) {
                    self.panes[s].hidden = false;
                    self.jump_to(s, t);
                }
            }
            Action::BrowserOpenTab => {
                if let Some(p) = self.panes.get(self.focus) {
                    let uid = p.cur().uid;
                    self.open_tab_chrome(uid);
                }
            }
            Action::AssistantNew => self.reset_assistant(),
            Action::OpenMic => self.start_open_mic(),
            Action::HeadphoneTap => self.toggle_headphone_tap(),
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
            Action::CreateToken => self.palette_create_token(),
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
            self.mark_seen(slot, tab);
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
