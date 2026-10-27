use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountCfg {
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,

    #[serde(default)]
    pub label: String,

    #[serde(
        default = "default_harness",
        skip_serializing_if = "is_default_harness"
    )]
    pub harness: String,

    #[serde(default = "default_color")]
    pub color: String,

    #[serde(default)]
    pub cwd: Option<String>,

    #[serde(default)]
    pub args: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_tab_base: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback_lines: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_email: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_trust: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

pub const PERMISSION_MODES: &[&str] = &[
    "bypass",
    "default",
    "auto",
    "accept-edits",
    "plan",
    "manual",
    "dont-ask",
];

pub fn permission_args(mode: &str) -> Vec<String> {
    let m = match mode.trim().to_ascii_lowercase().as_str() {
        "bypass" | "bypasspermissions" | "skip" => {
            return vec!["--dangerously-skip-permissions".into()]
        }
        "auto" => "auto",
        "accept-edits" | "acceptedits" | "edits" => "acceptEdits",
        "plan" => "plan",
        "manual" | "ask" => "manual",
        "dont-ask" | "dontask" => "dontAsk",
        _ => return vec![],
    };
    vec!["--permission-mode".into(), m.into()]
}

pub fn permission_badge(mode: &str) -> &'static str {
    match mode.trim().to_ascii_lowercase().as_str() {
        "bypass" | "bypasspermissions" | "skip" => "bypass",
        "auto" => "auto",
        "accept-edits" | "acceptedits" | "edits" => "edits",
        "plan" => "plan",
        "manual" | "ask" => "manual",
        "dont-ask" | "dontask" => "dont-ask",
        _ => "default",
    }
}

fn default_color() -> String {
    "sage".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub claude_bin: Option<String>,

    #[serde(default)]
    pub grok_bin: Option<String>,
    #[serde(default)]
    pub codex_bin: Option<String>,
    #[serde(default)]
    pub cursor_bin: Option<String>,
    #[serde(default)]
    pub antigravity_bin: Option<String>,
    #[serde(default)]
    pub opencode_bin: Option<String>,

    #[serde(default)]
    pub grok_claude_compat: GrokCompat,

    #[serde(default = "default_true")]
    pub autostart: bool,

    #[serde(default = "default_refresh")]
    pub refresh_secs: u64,

    #[serde(default)]
    pub auto_restart: bool,

    #[serde(default = "default_true")]
    pub notifications: bool,

    #[serde(default)]
    pub pass_env: Vec<String>,

    #[serde(default = "default_tab_position")]
    pub tab_position: String,

    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,

    #[serde(default = "default_confirm_close")]
    pub confirm_close: String,

    #[serde(default = "default_ungrouped")]
    pub ungrouped_tabs: String,

    #[serde(default = "default_true")]
    pub auto_trust: bool,

    #[serde(default)]
    pub trusted_dirs: Vec<String>,

    #[serde(default = "default_base")]
    pub new_tab_base: String,

    #[serde(default = "default_name_pattern")]
    pub new_tab_name_pattern: String,

    #[serde(default = "default_true")]
    pub new_tab_create_folder: bool,

    #[serde(default = "default_recent_limit")]
    pub recent_paths_limit: usize,

    #[serde(default = "default_true")]
    pub remember_window: bool,

    #[serde(default = "default_restore")]
    pub restore: String,

    #[serde(default = "default_true")]
    pub restore_continue_working: bool,

    #[serde(default = "default_true")]
    pub show_email: bool,

    #[serde(default = "default_true")]
    pub show_path: bool,

    #[serde(default)]
    pub privacy: bool,

    #[serde(default)]
    pub setup_dont_show: bool,

    #[serde(default = "default_splash")]
    pub splash: String,

    #[serde(default = "default_splash_motion")]
    pub splash_motion: String,

    #[serde(default = "default_layout")]
    pub layout: String,

    #[serde(default)]
    pub grid: String,

    #[serde(default)]
    pub layout_tree: String,

    #[serde(default)]
    pub closed_accounts: Vec<String>,

    #[serde(default)]
    pub collapsed_accounts: Vec<String>,

    #[serde(default)]
    pub collapsed_sideways: Vec<String>,

    #[serde(default)]
    pub expand_collapsed_on_focus: bool,

    #[serde(default)]
    pub tab_list_columns: u16,

    #[serde(default = "default_true")]
    pub tab_model_bar: bool,

    #[serde(default)]
    pub tab_models: TabModelsCfg,

    #[serde(default = "default_usage_colors")]
    pub usage_colors: String,

    #[serde(default = "default_bg_scrollback")]
    pub background_scrollback_lines: usize,

    #[serde(default)]
    pub memory_saver: bool,

    #[serde(default = "default_suspend")]
    pub suspend_idle_after: String,

    #[serde(default = "default_transcript_cache")]
    pub transcript_cache: usize,

    #[serde(default = "default_usage_history")]
    pub usage_history: usize,

    #[serde(default = "default_writer_queue")]
    pub writer_queue_kb: usize,

    #[serde(default = "default_read_buffer")]
    pub pty_read_buffer_kb: usize,

    #[serde(default)]
    pub voice_idle_stop_min: u32,

    #[serde(default = "default_scrollback")]
    pub scrollback_lines: usize,

    #[serde(default = "default_suggest_move")]
    pub suggest_move_below: f64,
    #[serde(default, rename = "account")]
    pub accounts: Vec<AccountCfg>,
    #[serde(default)]
    pub voice: VoiceCfg,
    #[serde(default)]
    pub assistant: AssistantCfg,

    #[serde(default)]
    pub usage: UsageCfg,

    #[serde(default)]
    pub tabs: TabsCfg,

    #[serde(default)]
    pub browser: BrowserCfg,

    #[serde(default)]
    pub ui: UiCfg,

    #[serde(default)]
    pub viz: crate::livemap::VizCfg,

    #[serde(default)]
    pub update: UpdateCfg,

    #[serde(default)]
    pub supervisor: SupervisorCfg,

    #[serde(default)]
    pub web: WebCfg,

    #[serde(default)]
    pub sync: crate::sync::SyncCfg,

    #[serde(default)]
    pub git: crate::git_map::GitCfg,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WebCfg {
    pub enabled: bool,
    pub port: u16,

    pub hosts: Vec<String>,

    pub tailnet_sign_in: bool,

    pub tailnet_users: Vec<String>,

    pub tailnet_ip: bool,
    pub ip_port: u16,

    pub assets_dir: String,

    pub embedded_assets: bool,
}

impl Default for WebCfg {
    fn default() -> Self {
        WebCfg {
            enabled: false,
            port: 7878,
            hosts: vec![],
            tailnet_sign_in: true,
            tailnet_users: vec![],
            tailnet_ip: true,
            ip_port: 7880,
            assets_dir: String::new(),
            embedded_assets: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SupervisorCfg {
    pub enabled: bool,

    pub hang_restart: bool,
    pub hang_s: u64,

    pub launch: String,
}

impl Default for SupervisorCfg {
    fn default() -> Self {
        SupervisorCfg {
            enabled: true,
            hang_restart: true,
            hang_s: 120,
            launch: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpdateCfg {
    pub enabled: bool,

    pub channel: String,

    pub auto_download: bool,

    pub require_signature: bool,

    pub check_hours: u64,
}

impl Default for UpdateCfg {
    fn default() -> Self {
        UpdateCfg {
            enabled: true,
            channel: "stable".into(),
            auto_download: true,
            require_signature: true,
            check_hours: 6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UsageCfg {
    pub failover_pct: f64,

    pub auto_failover: String,
}

impl Default for UsageCfg {
    fn default() -> Self {
        UsageCfg {
            failover_pct: 5.0,
            auto_failover: "ask".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiCfg {
    pub blink_attention: bool,
}

impl Default for UiCfg {
    fn default() -> Self {
        UiCfg {
            blink_attention: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TabsCfg {
    pub jobs_tools: bool,
}

impl Default for TabsCfg {
    fn default() -> Self {
        TabsCfg { jobs_tools: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BrowserCfg {
    pub default_profile: String,

    pub login_links: bool,

    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub links: std::collections::BTreeMap<String, String>,
}

impl Default for BrowserCfg {
    fn default() -> Self {
        BrowserCfg {
            default_profile: String::new(),
            login_links: true,
            links: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AssistantCfg {
    pub provider: String,

    pub models: std::collections::BTreeMap<String, String>,

    pub mode: String,

    pub account: String,

    pub model: String,

    pub style: String,

    pub confirm: String,

    pub max_tool_calls: usize,

    pub follow_up_s: u64,

    pub effort: String,

    pub max_output_tokens: u32,

    pub reset_after_turns: u32,

    pub endpoint_ms: u32,

    pub prewarm: bool,

    pub history_days: u32,

    pub memory_days: u32,

    pub spoken_sentences: usize,

    pub answer_wait_min: u64,

    pub update_quiet_s: u64,

    pub speak_typed: bool,

    pub panel: String,

    pub panel_width: u16,

    pub input_rows: u16,

    pub remote_control: bool,

    pub resume: bool,

    pub auto_switch: String,

    pub switch_warn_pct: f64,

    pub switch_at_pct: f64,
}

impl Default for AssistantCfg {
    fn default() -> Self {
        AssistantCfg {
            provider: "claude".into(),
            models: Default::default(),
            mode: "always".into(),
            account: "best".into(),
            model: "claude-sonnet-5-5".into(),
            style: "concise".into(),
            confirm: "destructive".into(),
            max_tool_calls: 16,
            follow_up_s: 8,
            effort: "low".into(),
            max_output_tokens: 8192,
            reset_after_turns: 40,
            endpoint_ms: 550,
            prewarm: true,
            history_days: 30,
            memory_days: 2,
            spoken_sentences: 2,
            answer_wait_min: 15,
            update_quiet_s: 4,
            speak_typed: false,
            panel: "docked".into(),
            panel_width: 0,
            input_rows: 8,
            remote_control: false,
            resume: true,
            auto_switch: "auto".into(),
            switch_warn_pct: 15.0,
            switch_at_pct: 5.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VoiceCfg {
    pub enabled: bool,

    pub mode: String,
    pub wake_words: Vec<String>,

    pub instant_commands: bool,

    pub whisper_vad: bool,

    pub engine: String,

    pub mute_on_start: bool,

    pub auto_gain: bool,

    pub beam_size: u32,

    pub debug_save_audio: bool,

    pub instant: Vec<String>,

    pub model: String,

    pub wake_model: String,

    pub whisper_server: String,

    pub whisper_cli: String,
    pub ffmpeg: String,

    pub device: String,
    pub language: String,

    pub vad_threshold: f32,

    pub vad_min_rms: f32,

    pub end_silence_ms: u32,
    pub min_speech_ms: u32,
    pub max_utterance_s: u32,

    pub gain_db: f32,

    pub noise_floor: String,

    pub preroll_ms: u32,

    pub partial_model: String,

    pub whisper_threads: usize,

    pub wake_sensitivity: f32,

    pub open_mic_sleep_min: u32,

    pub pause_default_s: u32,

    pub wake_follow_up_s: u64,

    pub direct_reply: String,

    pub direct_sentences: usize,

    pub pause_max_s: u32,

    pub tts: bool,

    pub speaker_muted: bool,

    pub tts_engine: String,

    pub tts_fallback: Vec<String>,

    pub local: LocalTtsCfg,

    pub grok: GrokTtsCfg,

    pub grok_stt: GrokSttCfg,

    pub tts_voice: Option<String>,

    pub kokoro_voice: String,
    pub kokoro_model: String,
    pub kokoro_voices: String,

    pub espeak: String,

    pub tts_provider: String,

    pub tts_threads: usize,

    pub tts_unload_after_s: u32,

    pub output_device: String,

    pub pronounce: Vec<String>,

    pub tts_speed: f32,

    pub tts_volume: f32,

    pub speak_confirm: bool,

    pub speak_readback: bool,

    pub conversation: bool,

    pub barge_in: bool,
    pub barge_margin_db: f32,

    pub barge_in_min_words: u32,

    pub barge_in_duck: bool,

    pub announce: bool,

    pub announce_every_s: u32,

    pub chime: bool,

    pub headphone_tap: bool,

    pub dictation_fallback: String,

    pub capture: String,

    pub voice_processing: bool,

    pub denoise: String,

    pub speaker_lock: String,

    pub speaker_model: String,

    pub speaker_threshold: f32,

    pub speaker_margin: f32,

    pub near_field: bool,

    pub near_field_db: f32,

    pub connective_hold_ms: u32,

    pub brainstorm_timeout_min: u32,

    pub brainstorm_suggest: bool,

    pub brainstorm_explained: bool,

    pub updates: bool,

    pub updates_gap_s: u32,

    pub updates_every_min: u32,

    pub updates_explained: bool,
}

impl Default for VoiceCfg {
    fn default() -> Self {
        VoiceCfg {
            enabled: false,
            mode: "push".into(),
            wake_words: vec!["hey god".into(), "god term".into()],
            instant_commands: true,
            whisper_vad: false,
            auto_gain: true,
            mute_on_start: false,
            engine: "whisper".into(),
            beam_size: 5,
            debug_save_audio: false,
            instant: crate::instant::DEFAULTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            model: "~/.cache/whisper-models/ggml-large-v3-turbo.bin".into(),
            wake_model: "auto".into(),
            whisper_server: "/opt/homebrew/bin/whisper-server".into(),
            whisper_cli: "/opt/homebrew/bin/whisper-cli".into(),
            ffmpeg: "ffmpeg".into(),
            device: "default".into(),
            language: "en".into(),
            vad_threshold: 3.0,
            vad_min_rms: 300.0,
            end_silence_ms: 700,
            min_speech_ms: 250,
            max_utterance_s: 15,
            gain_db: 0.0,
            open_mic_sleep_min: 10,
            pause_default_s: 120,
            wake_follow_up_s: 7,
            direct_reply: "auto".into(),
            direct_sentences: 3,
            pause_max_s: 3600,
            noise_floor: "auto".into(),
            preroll_ms: 360,
            partial_model: "auto".into(),
            whisper_threads: 0,
            wake_sensitivity: 0.75,
            tts: true,
            speaker_muted: false,
            tts_engine: "kokoro".into(),
            tts_voice: None,
            kokoro_voice: "af_heart".into(),
            kokoro_model: "~/.cache/kokoro-onnx/kokoro-v1.0.onnx".into(),
            kokoro_voices: "~/.cache/kokoro-onnx/voices-v1.0.bin".into(),
            espeak: "/opt/homebrew/bin/espeak-ng".into(),
            tts_provider: "cpu".into(),
            tts_threads: 0,
            tts_unload_after_s: 300,
            output_device: "default".into(),
            pronounce: vec![
                "GodTerm=god term".into(),
                "CLI=C L I".into(),
                "JSON=jason".into(),
            ],
            tts_speed: 1.0,
            tts_volume: 0.8,
            speak_confirm: true,
            speak_readback: true,
            conversation: false,
            barge_in: true,
            headphone_tap: false,
            barge_margin_db: 15.0,
            barge_in_min_words: 2,
            barge_in_duck: true,
            announce: true,
            announce_every_s: 20,
            chime: true,
            dictation_fallback: "confirm".into(),
            capture: "auto".into(),
            voice_processing: true,
            denoise: "auto".into(),
            speaker_lock: "open_mic_only".into(),
            speaker_model: "auto".into(),
            speaker_threshold: 0.0,
            speaker_margin: 0.0,
            near_field: true,
            near_field_db: 12.0,
            connective_hold_ms: 1500,
            brainstorm_timeout_min: 10,
            brainstorm_suggest: true,
            brainstorm_explained: false,
            updates: false,
            updates_gap_s: 60,
            updates_every_min: 4,
            updates_explained: false,
            tts_fallback: vec!["kokoro".into(), "say".into()],
            local: LocalTtsCfg::default(),
            grok: GrokTtsCfg::default(),
            grok_stt: GrokSttCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LocalTtsCfg {
    pub chatterbox_ref: String,

    pub ming_voice: String,

    pub ming_design: String,

    pub venv: String,

    pub python: String,

    pub load_timeout_s: u32,

    pub timeout_ms: u32,
}

impl Default for LocalTtsCfg {
    fn default() -> Self {
        LocalTtsCfg {
            chatterbox_ref: String::new(),
            ming_voice: "US_Emma".into(),
            ming_design: String::new(),
            venv: String::new(),
            python: "auto".into(),
            load_timeout_s: 90,
            timeout_ms: 8000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GrokTtsCfg {
    pub auth: String,

    pub source: String,

    pub voice: String,

    pub speed: f32,

    pub language: String,

    pub timeout_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GrokSttCfg {
    pub timeout_ms: u32,

    pub local_partials: bool,

    pub fallback: String,
}

impl Default for GrokSttCfg {
    fn default() -> Self {
        GrokSttCfg {
            timeout_ms: 6000,
            local_partials: true,
            fallback: "whisper".into(),
        }
    }
}

impl Default for GrokTtsCfg {
    fn default() -> Self {
        GrokTtsCfg {
            auth: "oauth".into(),
            source: "auto".into(),
            voice: "eve".into(),
            speed: 1.0,
            language: "en".into(),
            timeout_ms: 5000,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_base() -> String {
    "~".into()
}
fn default_name_pattern() -> String {
    "%Y-%m-%d".into()
}
fn default_recent_limit() -> usize {
    10
}
fn default_splash() -> String {
    "once".into()
}
fn default_splash_motion() -> String {
    "on".into()
}
fn default_restore() -> String {
    "eager".into()
}
fn default_layout() -> String {
    "auto".into()
}
fn default_suggest_move() -> f64 {
    10.0
}
fn default_usage_colors() -> String {
    "gradient".into()
}
fn default_bg_scrollback() -> usize {
    500
}
fn default_suspend() -> String {
    "10m".into()
}
fn default_transcript_cache() -> usize {
    50
}
fn default_usage_history() -> usize {
    60
}
fn default_writer_queue() -> usize {
    1024
}
fn default_read_buffer() -> usize {
    16
}
fn default_ungrouped() -> String {
    "bottom".into()
}

fn default_confirm_close() -> String {
    "busy".into()
}

fn default_permission_mode() -> String {
    "bypass".into()
}
fn default_tab_position() -> String {
    "left".into()
}
fn default_scrollback() -> usize {
    2000
}
fn default_refresh() -> u64 {
    60
}

impl Default for Config {
    fn default() -> Self {
        let colors = ["sage", "sand", "slate", "clay"];
        Config {
            claude_bin: None,
            grok_bin: None,
            codex_bin: None,
            cursor_bin: None,
            antigravity_bin: None,
            opencode_bin: None,
            grok_claude_compat: GrokCompat::default(),
            autostart: true,
            refresh_secs: 60,
            auto_restart: false,
            notifications: true,
            scrollback_lines: 2000,
            pass_env: vec![],
            tab_position: "left".into(),
            permission_mode: "bypass".into(),
            confirm_close: default_confirm_close(),
            ungrouped_tabs: default_ungrouped(),
            auto_trust: true,
            trusted_dirs: vec![],
            remember_window: true,
            new_tab_base: default_base(),
            new_tab_name_pattern: default_name_pattern(),
            new_tab_create_folder: true,
            recent_paths_limit: default_recent_limit(),
            restore: default_restore(),
            restore_continue_working: true,
            show_email: true,
            show_path: true,
            privacy: false,
            setup_dont_show: false,
            splash: default_splash(),
            splash_motion: default_splash_motion(),
            layout: default_layout(),
            grid: String::new(),
            layout_tree: String::new(),
            closed_accounts: vec![],
            collapsed_accounts: vec![],
            collapsed_sideways: vec![],
            expand_collapsed_on_focus: false,
            tab_list_columns: 0,
            tab_model_bar: true,
            tab_models: TabModelsCfg::default(),
            usage_colors: default_usage_colors(),
            suggest_move_below: default_suggest_move(),
            background_scrollback_lines: default_bg_scrollback(),
            memory_saver: false,
            suspend_idle_after: default_suspend(),
            transcript_cache: default_transcript_cache(),
            usage_history: default_usage_history(),
            writer_queue_kb: default_writer_queue(),
            pty_read_buffer_kb: default_read_buffer(),
            voice_idle_stop_min: 0,
            accounts: (1..=4)
                .map(|i| AccountCfg {
                    name: format!("account{i}"),
                    label: format!("Account {i}"),
                    color: colors[i - 1].into(),
                    cwd: Some("~".into()),
                    args: vec![],
                    permission_mode: None,
                    auto_trust: None,
                    show_email: None,
                    new_tab_base: None,
                    scrollback_lines: None,
                    config_dir: None,
                    harness: crate::config::default_harness(),
                })
                .collect(),
            voice: VoiceCfg::default(),
            viz: Default::default(),
            update: Default::default(),
            supervisor: Default::default(),
            web: Default::default(),
            sync: Default::default(),
            git: Default::default(),
            assistant: AssistantCfg::default(),
            usage: UsageCfg::default(),
            tabs: TabsCfg::default(),
            browser: BrowserCfg::default(),
            ui: UiCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GrokCompat {
    pub hooks: bool,
    pub skills: bool,
    pub rules: bool,
    pub agents: bool,
    pub mcps: bool,
    pub sessions: bool,
}

impl Default for GrokCompat {
    fn default() -> Self {
        GrokCompat {
            hooks: false,
            skills: true,
            rules: true,
            agents: true,
            mcps: false,
            sessions: false,
        }
    }
}

impl GrokCompat {
    pub fn env(&self) -> Vec<(&'static str, &'static str)> {
        let b = |x: bool| if x { "true" } else { "false" };
        vec![
            ("GROK_CLAUDE_HOOKS_ENABLED", b(self.hooks)),
            ("GROK_CLAUDE_SKILLS_ENABLED", b(self.skills)),
            ("GROK_CLAUDE_RULES_ENABLED", b(self.rules)),
            ("GROK_CLAUDE_AGENTS_ENABLED", b(self.agents)),
            ("GROK_CLAUDE_MCPS_ENABLED", b(self.mcps)),
            ("GROK_CLAUDE_SESSIONS_ENABLED", b(self.sessions)),
        ]
    }
}

pub fn default_harness() -> String {
    "claude".into()
}

fn is_default_harness(h: &String) -> bool {
    h == "claude"
}

pub fn env_var(name: &str) -> Option<String> {
    for prefix in ["GODTERM_", "CLAUDEGO_"] {
        if let Ok(v) = std::env::var(format!("{prefix}{name}")) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dirs {
    pub app_home: PathBuf,

    pub main_claude: PathBuf,

    pub main_grok: PathBuf,

    pub explicit_home: bool,

    pub mains: bool,
}

#[cfg(not(test))]
pub fn dirs() -> Dirs {
    let home = env_var("HOME");
    let d = Dirs {
        explicit_home: home.is_some(),
        app_home: home
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".godterm")),
        main_claude: env_var("MAIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".claude")),
        main_grok: env_var("MAIN_GROK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".grok")),
        mains: true,
    };

    if crate::demo::active() {
        crate::demo::assert_safe(&d);
    }
    d
}

fn real_user_dirs() -> Vec<PathBuf> {
    let h = home_dir();
    [".claude", ".grok", ".godterm", ".claudego", ".claude.json"]
        .iter()
        .map(|d| h.join(d))
        .collect()
}

pub fn is_real_user_dir(p: &Path) -> bool {
    p == home_dir() || real_user_dirs().iter().any(|r| p.starts_with(r))
}

#[cfg(test)]
pub fn dirs() -> Dirs {
    let d = testing::current();
    testing::assert_safe(&d);
    d
}

pub fn app_home() -> PathBuf {
    dirs().app_home
}

#[cfg(test)]
pub mod testing {

    use super::{home_dir, Dirs};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    pub static LOCK: Mutex<()> = Mutex::new(());

    static DIRS: Mutex<Option<Dirs>> = Mutex::new(None);

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!("godterm-unit-{}", std::process::id()))
    }

    pub fn current() -> Dirs {
        DIRS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| {
                let s = scratch();
                Dirs {
                    app_home: s.join("home"),
                    main_claude: s.join("no-main-claude"),
                    main_grok: s.join("no-main-grok"),
                    explicit_home: true,
                    mains: false,
                }
            })
    }

    pub fn set_home(home: &Path) {
        let mut d = current();
        d.app_home = home.to_path_buf();
        d.mains = false;
        d.main_claude = home.join("no-main-claude");
        d.main_grok = home.join("no-main-grok");
        assert_safe(&d);
        *DIRS.lock().unwrap_or_else(|e| e.into_inner()) = Some(d);
    }

    pub fn main_dirs(claude: &Path, grok: Option<&Path>) -> MainGuard {
        let prev = current();
        let mut d = prev.clone();
        d.main_claude = claude.to_path_buf();
        if let Some(g) = grok {
            d.main_grok = g.to_path_buf();
        }
        d.mains = true;
        assert_safe(&d);
        *DIRS.lock().unwrap_or_else(|e| e.into_inner()) = Some(d);
        MainGuard { prev }
    }

    pub struct MainGuard {
        prev: Dirs,
    }

    impl Drop for MainGuard {
        fn drop(&mut self) {
            *DIRS.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.prev.clone());
        }
    }

    fn is_real(p: &Path) -> bool {
        super::is_real_user_dir(p)
    }

    pub fn assert_safe(d: &Dirs) {
        for p in [&d.app_home, &d.main_claude, &d.main_grok] {
            assert!(
                !is_real(p),
                "test isolation: {} is a real user dir",
                p.display()
            );
        }
        for v in [
            "GODTERM_HOME",
            "CLAUDEGO_HOME",
            "GODTERM_MAIN_DIR",
            "GODTERM_MAIN_GROK_DIR",
        ] {
            if let Ok(x) = std::env::var(v) {
                assert!(
                    x.is_empty() || !is_real(Path::new(&x)),
                    "test isolation: ${v}={x} points at real user data"
                );
            }
        }
    }

    #[test]
    fn private_files_and_home() {
        use crate::platform::PermissionsExt;
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let h = std::env::temp_dir().join(format!("godterm-modes-{}", std::process::id()));
        std::fs::create_dir_all(&h).unwrap();
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(h.join("godterm.log"), "x").unwrap();
        set_home(&h);
        crate::config::secure_home();

        let mode = |p: &Path| {
            if cfg!(unix) {
                std::fs::metadata(p).unwrap().permissions().mode() & 0o777
            } else {
                assert!(p.exists());
                if p.is_dir() {
                    0o700
                } else {
                    0o600
                }
            }
        };
        assert_eq!(mode(&h), 0o700);
        assert_eq!(mode(&h.join("godterm.log")), 0o600);
        crate::state::AppState::default().save().unwrap();
        assert_eq!(mode(&h.join("state.json")), 0o600);
        crate::config::write_private(&h.join("x.json"), "{}").unwrap();
        assert_eq!(mode(&h.join("x.json")), 0o600);
        crate::log::info("hello");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn real_dirs_are_refused() {
        let h = home_dir();
        assert!(is_real(&h.join(".claude")) && is_real(&h.join(".grok/sessions")) && is_real(&h));
        assert!(!is_real(&std::env::temp_dir().join("x")));
        let bad = Dirs {
            app_home: h.join(".godterm"),
            ..current()
        };
        assert!(std::panic::catch_unwind(|| assert_safe(&bad)).is_err());
        assert_safe(&current());
    }
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn tilde(p: &Path) -> String {
    let home = home_dir();

    match p.strip_prefix(&home) {
        Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        Err(_) => p.display().to_string(),
    }
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        home_dir()
    } else if let Some(rest) = p
        .strip_prefix("~/")
        .or_else(|| p.strip_prefix("~\\").filter(|_| cfg!(windows)))
    {
        home_dir().join(rest)
    } else {
        PathBuf::from(p)
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !name.starts_with('.')
}

impl AccountCfg {
    pub fn display(&self) -> &str {
        if self.label.is_empty() {
            &self.name
        } else {
            &self.label
        }
    }

    pub fn harness(&self) -> crate::harness::Harness {
        crate::harness::Harness::of(&self.harness)
    }

    pub fn config_dir(&self) -> PathBuf {
        match self.config_dir.as_deref().filter(|d| !d.trim().is_empty()) {
            Some(d) => expand_tilde(d),
            None => app_home().join("accounts").join(&self.name),
        }
    }

    pub fn work_dir(&self) -> PathBuf {
        let p = self
            .cwd
            .as_deref()
            .map(expand_tilde)
            .unwrap_or_else(home_dir);
        if p.is_dir() {
            p
        } else {
            home_dir()
        }
    }
}

impl Config {
    pub fn harness_bin(&self, h: crate::harness::Harness) -> String {
        use crate::harness::Harness;
        let configured = match h {
            Harness::Claude => return self.claude_bin(),
            Harness::Grok => &self.grok_bin,
            Harness::Codex => &self.codex_bin,
            Harness::Cursor => &self.cursor_bin,
            Harness::Antigravity => &self.antigravity_bin,
            Harness::OpenCode => &self.opencode_bin,
        };
        h.bin(configured.as_deref())
    }

    pub fn path() -> PathBuf {
        app_home().join("config.toml")
    }

    pub fn load_or_init() -> Result<Config> {
        let path = Self::path();
        if !path.exists() {
            let cfg = Config::default();

            if !crate::migrate::pending() {
                cfg.save()?;
            }
            return Ok(cfg);
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let cfg = Self::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    pub fn load_or_recover() -> Result<Config> {
        let path = Self::path();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,

            Err(_) => return Self::load_or_init(),
        };
        match Self::parse(&text) {
            Ok(c) => Ok(c),
            Err(e) => {
                let why = if text.trim().is_empty() {
                    "empty".to_string()
                } else {
                    format!("{e:#}")
                };
                let Some(bak) = crate::recover::quarantine(&path, &why) else {
                    return Err(e.context(format!("parsing {}", path.display())));
                };

                eprintln!(
                    "GodTerm: config.toml did not parse ({why}); saved as {} and starting clean.",
                    bak.display()
                );
                Ok(Config::default())
            }
        }
    }

    pub fn parse(text: &str) -> Result<Config> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for a in &self.accounts {
            if !valid_name(&a.name) {
                bail!(
                    "invalid account name {:?} (use letters, digits, - _ .)",
                    a.name
                );
            }
            if !seen.insert(a.name.clone()) {
                bail!("duplicate account name {:?}", a.name);
            }
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body = toml::to_string_pretty(self)?;
        let header = "# godterm configuration\n\
# Slots use ~/.godterm/accounts/<name>/ for GodTerm-managed data.\n\
# Claude, Grok and Codex get isolated homes; other CLIs use native stores.\n\
# The first four accounts are shown in the 2x2 grid.\n\n";
        write_atomic(&path, format!("{header}{body}").as_bytes())
    }

    pub fn base_for(&self, i: usize) -> std::path::PathBuf {
        let b = self
            .accounts
            .get(i)
            .and_then(|a| a.new_tab_base.clone())
            .unwrap_or_else(|| self.new_tab_base.clone());
        expand_tilde(if b.trim().is_empty() { "~" } else { b.trim() })
    }

    pub fn trust_for(&self, i: usize) -> bool {
        self.accounts
            .get(i)
            .and_then(|a| a.auto_trust)
            .unwrap_or(self.auto_trust)
    }

    pub fn mode_for(&self, i: usize) -> String {
        let Some(a) = self.accounts.get(i) else {
            return self.permission_mode.clone();
        };
        if let Some(m) = &a.permission_mode {
            return m.clone();
        }
        if !a.harness().integrated() && self.permission_mode == "bypass" {
            return "default".into();
        }
        self.permission_mode.clone()
    }

    pub fn claude_bin(&self) -> String {
        self.claude_bin
            .as_deref()
            .map(|s| expand_tilde(s).to_string_lossy().into_owned())
            .unwrap_or_else(|| crate::harness::Harness::Claude.bin(None))
    }
}

pub fn write_private(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    use crate::platform::{OpenOptionsExt, PermissionsExt};
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;

    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    f.write_all(data.as_ref())
}

pub fn secure_home() {
    use crate::platform::PermissionsExt;
    let h = app_home();
    let _ = fs::create_dir_all(&h);
    let _ = fs::set_permissions(&h, fs::Permissions::from_mode(0o700));
    for f in [
        "godterm.log",
        "godterm.log.1",
        "session-index.json",
        "state.json",
        "config.toml",
        "control.json",
        "assistant-history.jsonl",
    ] {
        let p = h.join(f);
        if p.is_file() {
            let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o600));
        }
    }
}

pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    write_private(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let cfg = Config::parse(
            r##"
[[account]]
name = "work"
label = "Work"
color = "#7a8b99"
cwd = "~/code"
args = ["--model", "opus"]

[[account]]
name = "home"
"##,
        )
        .unwrap();
        assert_eq!(cfg.accounts.len(), 2);
        assert!(cfg.autostart);
        assert_eq!(cfg.refresh_secs, 60);
        assert_eq!(cfg.accounts[0].args, vec!["--model", "opus"]);
        assert_eq!(cfg.accounts[1].display(), "home");
        assert_eq!(cfg.accounts[1].color, "sage");
    }

    #[test]
    fn permission_mode_args() {
        assert_eq!(
            permission_args("bypass"),
            vec!["--dangerously-skip-permissions"]
        );
        assert!(permission_args("default").is_empty());
        assert_eq!(
            permission_args("accept-edits"),
            vec!["--permission-mode", "acceptEdits"]
        );
        assert_eq!(permission_args("plan"), vec!["--permission-mode", "plan"]);
        assert_eq!(permission_args("auto"), vec!["--permission-mode", "auto"]);
        assert_eq!(
            permission_args("manual"),
            vec!["--permission-mode", "manual"]
        );
        assert_eq!(
            permission_args("dont-ask"),
            vec!["--permission-mode", "dontAsk"]
        );
        assert!(permission_args("nonsense").is_empty());
        assert_eq!(permission_badge("Bypass"), "bypass");
        let cfg = Config::parse("permission_mode = \"plan\"\n[[account]]\nname = \"a\"\npermission_mode = \"bypass\"\n[[account]]\nname = \"b\"\n").unwrap();
        assert_eq!(cfg.mode_for(0), "bypass");
        assert_eq!(cfg.mode_for(1), "plan");
        assert_eq!(Config::default().permission_mode, "bypass");
    }

    #[test]
    fn new_harnesses_do_not_inherit_global_bypass() {
        let cfg = Config::parse(
            "[[account]]\nname = \"c\"\nharness = \"codex\"\n\
             [[account]]\nname = \"x\"\nharness = \"cursor\"\npermission_mode = \"bypass\"\n\
             [[account]]\nname = \"g\"\nharness = \"grok\"\n",
        )
        .unwrap();
        assert_eq!(cfg.permission_mode, "bypass");
        assert_eq!(cfg.mode_for(0), "default");
        assert_eq!(cfg.mode_for(1), "bypass");
        assert_eq!(cfg.mode_for(2), "bypass");
        let plan = Config::parse(
            "permission_mode = \"plan\"\n[[account]]\nname = \"c\"\nharness = \"codex\"\n",
        )
        .unwrap();
        assert_eq!(plan.mode_for(0), "plan");
    }

    #[test]
    fn rejects_bad_names() {
        assert!(Config::parse("[[account]]\nname = \"../x\"\n").is_err());
        assert!(Config::parse("[[account]]\nname = \"a\"\n[[account]]\nname = \"a\"\n").is_err());
    }

    #[test]
    fn default_roundtrips() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), cfg);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TabModelsCfg {
    #[serde(default)]
    pub claude: Vec<String>,
    #[serde(default)]
    pub grok: Vec<String>,
}

impl TabModelsCfg {
    pub fn of(&self, h: crate::harness::Harness) -> &[String] {
        match h {
            crate::harness::Harness::Claude => &self.claude,
            crate::harness::Harness::Grok => &self.grok,

            _ => &[],
        }
    }
}
