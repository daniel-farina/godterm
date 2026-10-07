//! App configuration stored at `~/.godterm/config.toml`.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// One account slot. Each slot owns an isolated Claude Code config dir.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountCfg {
    /// Short identifier, used as the directory name under `accounts/`.
    pub name: String,
    /// Where this account's claude config lives (CLAUDE_CONFIG_DIR), when
    /// not ~/.godterm/accounts/<name>. Never change it by hand: claude's
    /// keychain item name depends on this exact path (use `godterm
    /// migrate-accounts`, which carries the login over).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// Human friendly label shown in pane titles and the status bar.
    #[serde(default)]
    pub label: String,
    /// The coding agent this account runs: "claude" (default) or "grok"
    /// (Grok Build, isolated with GROK_HOME).
    #[serde(
        default = "default_harness",
        skip_serializing_if = "is_default_harness"
    )]
    pub harness: String,
    /// Accent color: a palette name (sage, sand, slate, clay, mauve, stone)
    /// or a hex string like "#8a9a7b".
    #[serde(default = "default_color")]
    pub color: String,
    /// Working directory for new sessions. `~` is expanded.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Extra arguments appended to every `claude` invocation for this slot.
    #[serde(default)]
    pub args: Vec<String>,
    /// Base folder for new tabs of this account (overrides `new_tab_base`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_tab_base: Option<String>,
    /// History lines for this account's tabs (overrides scrollback_lines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback_lines: Option<usize>,
    /// Show this account's email in pane headers (overrides `show_email`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_email: Option<bool>,
    /// Answer claude's folder trust dialog automatically for this account
    /// (overrides the global `auto_trust`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_trust: Option<bool>,
    /// Permission mode for this account's tabs (overrides the global one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

/// Permission modes, as written in config.toml.
pub const PERMISSION_MODES: &[&str] = &[
    "bypass",
    "default",
    "auto",
    "accept-edits",
    "plan",
    "manual",
    "dont-ask",
];

/// claude arguments for a permission mode. "bypass" uses
/// --dangerously-skip-permissions, "default" passes nothing (claude's own
/// default), the rest map to --permission-mode <x> as named in
/// `claude --help` (2.1.29x). Unknown values pass nothing.
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

/// Short badge text for the pane header.
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
    /// Path to the claude binary. Defaults to `claude` on PATH.
    #[serde(default)]
    pub claude_bin: Option<String>,
    /// Path to the grok binary (Grok Build) for grok accounts. Defaults to
    /// ~/.local/bin/grok, else `grok` on PATH.
    #[serde(default)]
    pub grok_bin: Option<String>,
    /// What grok in a GodTerm slot may pick up from the user's Claude Code
    /// setup (~/.claude): grok scans it whatever GROK_HOME says.
    #[serde(default)]
    pub grok_claude_compat: GrokCompat,
    /// Start `claude` automatically in panes whose account is logged in.
    #[serde(default = "default_true")]
    pub autostart: bool,
    /// Usage refresh interval in seconds.
    #[serde(default = "default_refresh")]
    pub refresh_secs: u64,
    /// Restart a tab automatically (resuming its session) when claude
    /// crashes. Off by default: the tab offers a restart on Enter instead.
    #[serde(default)]
    pub auto_restart: bool,
    /// macOS notifications when a background tab needs approval or
    /// finishes, while the terminal is not in front.
    #[serde(default = "default_true")]
    pub notifications: bool,
    /// Environment variables to pass to claude even though they match the
    /// Claude Code markers godterm strips (CLAUDECODE, CLAUDE_CODE_*, ...).
    #[serde(default)]
    pub pass_env: Vec<String>,
    /// Where each pane lists its tabs: "left" (default), "right" or "top".
    /// A pane can override it (Ctrl-a S, the account menu, or voice).
    #[serde(default = "default_tab_position")]
    pub tab_position: String,
    /// Permission mode for new tabs: "bypass" (default, skips every
    /// permission check), "default" (claude decides), "auto",
    /// "accept-edits", "plan", "manual" or "dont-ask". Accounts can override.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// Ask before closing a tab: "busy" (default: when it is working,
    /// waiting for an answer or has loops), "always" (whenever it runs)
    /// or "never".
    #[serde(default = "default_confirm_close")]
    pub confirm_close: String,
    /// Where tabs outside any group sit in a tab list: "bottom" or "top".
    #[serde(default = "default_ungrouped")]
    pub ungrouped_tabs: String,
    /// Trust each tab's folder automatically: seed claude's trust flag before
    /// starting it and answer the "Yes, I trust this folder" dialog.
    #[serde(default = "default_true")]
    pub auto_trust: bool,
    /// Limit auto trust to folders inside these (empty: any folder).
    #[serde(default)]
    pub trusted_dirs: Vec<String>,
    /// Folder new tabs are created in (a dated subfolder by default).
    #[serde(default = "default_base")]
    pub new_tab_base: String,
    /// strftime pattern for new tab folder names; -1, -2... is appended.
    #[serde(default = "default_name_pattern")]
    pub new_tab_name_pattern: String,
    /// Create the folder for a new tab (otherwise it must exist).
    #[serde(default = "default_true")]
    pub new_tab_create_folder: bool,
    /// Recent folders kept per account.
    #[serde(default = "default_recent_limit")]
    pub recent_paths_limit: usize,
    /// Save the terminal window's size and position and restore it from the
    /// app launcher (moving it to the main screen if its monitor is gone).
    #[serde(default = "default_true")]
    pub remember_window: bool,
    /// "eager" starts every restored tab at launch; "lazy" when first shown.
    #[serde(default = "default_restore")]
    pub restore: String,
    /// Show the account email in pane headers.
    #[serde(default = "default_true")]
    pub show_email: bool,
    /// Show the active tab's folder in each pane header (and tab rows).
    #[serde(default = "default_true")]
    pub show_path: bool,
    /// Hide emails everywhere (screen sharing).
    #[serde(default)]
    pub privacy: bool,
    /// Pane layout: auto, grid, columns, rows, focus.
    #[serde(default = "default_layout")]
    pub layout: String,
    /// Explicit grid such as "3x2" (used with layout = "grid").
    #[serde(default)]
    pub grid: String,
    /// Usage colors: gradient, bands or mono.
    #[serde(default = "default_usage_colors")]
    pub usage_colors: String,
    /// Scrollback kept for tabs not on screen.
    #[serde(default = "default_bg_scrollback")]
    pub background_scrollback_lines: usize,
    /// Low memory profile.
    #[serde(default)]
    pub memory_saver: bool,
    /// Stop idle background tabs after this long (memory saver), e.g. "10m".
    #[serde(default = "default_suspend")]
    pub suspend_idle_after: String,
    /// Parsed sessions kept in memory for the sessions list and read back.
    #[serde(default = "default_transcript_cache")]
    pub transcript_cache: usize,
    /// Usage samples kept per account.
    #[serde(default = "default_usage_history")]
    pub usage_history: usize,
    /// Per tab input queue size in KB.
    #[serde(default = "default_writer_queue")]
    pub writer_queue_kb: usize,
    /// PTY read buffer per tab in KB.
    #[serde(default = "default_read_buffer")]
    pub pty_read_buffer_kb: usize,
    /// Stop whisper-server (and Kokoro) after this many idle minutes in
    /// push to talk mode (0: keep them; the memory saver uses 5).
    #[serde(default)]
    pub voice_idle_stop_min: u32,
    /// Lines of scrollback kept per tab (bounds memory use).
    #[serde(default = "default_scrollback")]
    pub scrollback_lines: usize,
    /// A tab whose account has less than this % of its 5 hour window left
    /// offers a one click move to the account with the most left (0: off).
    #[serde(default = "default_suggest_move")]
    pub suggest_move_below: f64,
    #[serde(default, rename = "account")]
    pub accounts: Vec<AccountCfg>,
    #[serde(default)]
    pub voice: VoiceCfg,
    #[serde(default)]
    pub assistant: AssistantCfg,
    /// `[viz]`: the live map.
    #[serde(default)]
    pub viz: crate::livemap::VizCfg,
    /// `[update]`: checking for and installing new versions.
    #[serde(default)]
    pub update: UpdateCfg,
}

/// `[update]`: GodTerm checks GitHub Releases at start and every few
/// hours, downloads and verifies a new version in the background, and
/// restarts into it when you say so (Ctrl-a N).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpdateCfg {
    /// Check for new versions.
    pub enabled: bool,
    /// "stable", or "prerelease" to include pre-releases.
    pub channel: String,
    /// Download and verify a new version in the background.
    pub auto_download: bool,
    /// Refuse a release whose SHA256SUMS has no valid minisign signature
    /// (on by default: every release from 0.2.2 is signed).
    pub require_signature: bool,
    /// Hours between checks.
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

/// `[assistant]`: the natural language voice assistant, a headless claude
/// on one of your accounts that drives godterm through its tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AssistantCfg {
    /// "always" (default): the assistant interprets everything said or
    /// typed, apart from the instant one word commands; "off". The old
    /// "fallback" reads as "always".
    pub mode: String,
    /// "best" (the logged in account with the most 5 hour quota left) or
    /// an account name. The assistant spends that account's quota.
    pub account: String,
    /// claude model alias or name.
    pub model: String,
    /// "concise" or "chatty".
    pub style: String,
    /// When it must get a spoken yes: "destructive" (closing tabs,
    /// stopping loops, broadcast, deny, approving rm / push / force...),
    /// "always" (every approval too), "never" (not recommended).
    pub confirm: String,
    /// Most actions in one turn.
    pub max_tool_calls: usize,
    /// After it speaks, listen this many seconds for a follow up without
    /// the wake word (wake mode).
    pub follow_up_s: u64,
    /// claude --effort: low (default, quickest), medium, high. Thinking is
    /// off at low.
    pub effort: String,
    /// Upper limit in tokens for one reply including its tool calls (a
    /// long send_prompt or summary needs room); speech is kept short by
    /// spoken_sentences instead.
    pub max_output_tokens: u32,
    /// Start a fresh conversation after this many turns (keeps context
    /// small and answers quick); a one line summary carries over.
    pub reset_after_turns: u32,
    /// Silence that ends an utterance while the assistant or open mic is on
    /// (shorter than voice.end_silence_ms, so it answers sooner).
    pub endpoint_ms: u32,
    /// Start the assistant's process at launch and keep it warm.
    pub prewarm: bool,
    /// Keep saved conversations this many days (0: forever).
    pub history_days: u32,
    /// A new assistant process remembers the conversations of this many
    /// days (a short summary of each; 0: none).
    pub memory_days: u32,
    /// Speak at most this many sentences of a reply (0: all); the rest
    /// stays in the panel, and "more" says it.
    pub spoken_sentences: usize,
    /// Watch for the answer to a question delegated to a tab this many
    /// minutes, then stop waiting.
    pub answer_wait_min: u64,
}

impl Default for AssistantCfg {
    fn default() -> Self {
        AssistantCfg {
            mode: "always".into(),
            account: "best".into(),
            model: "claude-haiku-4-5".into(),
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
        }
    }
}

/// `[voice]` section. Everything runs locally (ffmpeg + whisper.cpp + say).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VoiceCfg {
    /// Start the voice engine at launch. Push-to-talk (Ctrl-a space) also
    /// starts it on demand.
    pub enabled: bool,
    /// "push" (Ctrl-a space only) or "wake" (always listening for a wake word).
    pub mode: String,
    pub wake_words: Vec<String>,
    /// Instant one word commands: handled at once, without the assistant.
    pub instant_commands: bool,
    /// whisper.cpp's silero voice activity detection inside the server
    /// (the model, about 1 MB, is fetched once when missing).
    pub whisper_vad: bool,
    /// Recognizer: "auto" (Apple on-device when available, else whisper),
    /// "apple" or "whisper".
    pub engine: String,
    /// Start with the mic muted.
    pub mute_on_start: bool,
    /// Level each utterance to about -20 dBFS before whisper.
    pub auto_gain: bool,
    /// Beam size for final transcripts (1: greedy).
    pub beam_size: u32,
    /// Save the last 20 utterances (the exact WAV sent to whisper, the
    /// partials, the final and timings) to ~/.godterm/voice/debug.
    pub debug_save_audio: bool,
    /// The instant set: "phrase" (a built in one) or "phrase=action"
    /// with action one of stop, approve, deny, sleep, wake, stop_talking,
    /// next_tab, previous_tab. Matched against the whole utterance only.
    pub instant: Vec<String>,
    /// whisper.cpp ggml model. `~` is expanded.
    pub model: String,
    /// Small model used only to spot the wake word in always listening mode:
    /// "auto" (a ggml tiny or base model next to `model`, if present), a
    /// path, or "off".
    pub wake_model: String,
    /// whisper-server binary (kept running so the model loads once).
    pub whisper_server: String,
    /// whisper-cli binary, used if the server cannot start.
    pub whisper_cli: String,
    pub ffmpeg: String,
    /// avfoundation audio device: "default", an index like "1", or a name.
    pub device: String,
    pub language: String,
    /// Speech starts when frame energy exceeds noise floor times this.
    pub vad_threshold: f32,
    /// Absolute minimum RMS (0..32768) that counts as speech.
    pub vad_min_rms: f32,
    /// Silence that ends an utterance.
    pub end_silence_ms: u32,
    pub min_speech_ms: u32,
    pub max_utterance_s: u32,
    /// Input gain in dB applied to the microphone.
    pub gain_db: f32,
    /// "auto" (calibrate, then follow the room) or a fixed level in dBFS
    /// such as "-55".
    pub noise_floor: String,
    /// Audio kept from just before speech starts.
    pub preroll_ms: u32,
    /// Model for live partial transcripts: "auto" (the small wake model if
    /// there is one, else the main model), "main", or "off".
    pub partial_model: String,
    /// whisper threads (0: about half the idle cores).
    pub whisper_threads: usize,
    /// How close a heard word must be to a wake word or a learned alias
    /// (0.5 loose to 1.0 exact).
    pub wake_sensitivity: f32,
    /// Open mic: after this many silent minutes it goes back to waiting
    /// for the wake word (0: never).
    pub open_mic_sleep_min: u32,
    /// "Hold on" without a duration pauses listening this long (seconds).
    pub pause_default_s: u32,
    /// After a bare wake word ("hey god" alone), the next utterance within
    /// this many seconds is the command (no wake word needed).
    pub wake_follow_up_s: u64,
    /// The longest pause (seconds).
    pub pause_max_s: u32,
    /// Talk back at all (confirmations, read backs, announcements).
    pub tts: bool,
    /// "kokoro" (neural, in process), "say", "grok" (xAI cloud), "off".
    /// When it fails or times out, `tts_fallback` is tried in order.
    pub tts_engine: String,
    /// Engines tried after `tts_engine`, in order ("kokoro", "say", "grok").
    pub tts_fallback: Vec<String>,
    /// Grok (xAI cloud) talk back, and the xAI login Grok speech
    /// recognition shares.
    pub grok: GrokTtsCfg,
    /// Grok (xAI cloud) speech recognition (engine = grok).
    pub grok_stt: GrokSttCfg,
    /// macOS `say` voice (None: the system voice).
    pub tts_voice: Option<String>,
    /// Kokoro voice, e.g. af_heart, am_michael, bf_emma.
    pub kokoro_voice: String,
    pub kokoro_model: String,
    pub kokoro_voices: String,
    /// espeak-ng binary, for Kokoro's phonemes.
    pub espeak: String,
    /// ONNX Runtime provider for Kokoro: "cpu" or "coreml".
    pub tts_provider: String,
    /// Kokoro inference threads (0: half the cores, at most 8).
    pub tts_threads: usize,
    /// Unload Kokoro (about 800 MB) after this many idle seconds; it
    /// reloads in a fraction of a second when needed. 0 keeps it loaded.
    pub tts_unload_after_s: u32,
    /// Output device name for talk back ("default").
    pub output_device: String,
    /// Pronunciation fixes, "word=spoken form", applied before speaking.
    pub pronounce: Vec<String>,
    /// Speaking rate, 1.0 is normal (0.5 to 2.0).
    pub tts_speed: f32,
    /// Playback volume, 0.0 to 1.0.
    pub tts_volume: f32,
    /// Speak short confirmations ("sent", "new tab").
    pub speak_confirm: bool,
    /// Read replies and requests back when asked.
    pub speak_readback: bool,
    /// After talking back, listen for a reply without the wake word.
    pub conversation: bool,
    /// Speaking over the talk back stops it. The mic stays muted while we
    /// speak; only speech this many dB over the threshold cuts through
    /// (headphones recommended).
    pub barge_in: bool,
    pub barge_margin_db: f32,
    /// Speak when a background tab needs approval or finishes.
    pub announce: bool,
    /// Minimum seconds between announcements.
    pub announce_every_s: u32,
    /// Play a short sound when the wake word is heard.
    pub chime: bool,
    /// Unrecognized speech after the wake word: "confirm" holds it until you
    /// say "send", "send" types it into the focused session right away,
    /// "off" ignores it.
    pub dictation_fallback: String,
    /// Mic capture: "auto" (Apple voice processing through the
    /// godterm-speech helper on macOS when it is available, else ffmpeg),
    /// "apple" or "ffmpeg".
    pub capture: String,
    /// Apple capture: voice processing (echo cancellation, noise
    /// suppression, gain control; honors the Voice Isolation mic mode).
    pub voice_processing: bool,
    /// Neural noise suppression (RNNoise) before the endpointer: "auto"
    /// (on unless Apple voice processing is on), "on" or "off".
    pub denoise: String,
    /// Only accept the enrolled voice: "off", "open_mic_only" or "always"
    /// (every hands free utterance). Push to talk is never checked.
    pub speaker_lock: String,
    /// Speaker model: "auto" (WeSpeaker ResNet34 in ~/.cache/godterm-models),
    /// a known model name, or a path to an ONNX file.
    pub speaker_model: String,
    /// Accept threshold for the speaker score (0: the calibrated one).
    pub speaker_threshold: f32,
    /// Added to the calibrated threshold (stricter when positive).
    pub speaker_margin: f32,
    /// Drop utterances far quieter and duller than the user (a phone on
    /// speaker or a TV across the room).
    pub near_field: bool,
    /// How far under the user's usual level counts as far away (dB).
    pub near_field_db: f32,
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
            pause_max_s: 3600,
            noise_floor: "auto".into(),
            preroll_ms: 360,
            partial_model: "auto".into(),
            whisper_threads: 0,
            wake_sensitivity: 0.75,
            tts: true,
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
            barge_margin_db: 15.0,
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
            tts_fallback: vec!["kokoro".into(), "say".into()],
            grok: GrokTtsCfg::default(),
            grok_stt: GrokSttCfg::default(),
        }
    }
}

/// `[voice.grok]`: the xAI text to speech API. The spoken text (the
/// assistant's replies) goes to xAI. An API key lives in the Keychain,
/// never here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GrokTtsCfg {
    /// "oauth" (a grok login) or "api_key" (an xAI API key in the Keychain).
    pub auth: String,
    /// For oauth: "auto" (the first logged in grok account, then ~/.grok),
    /// "main" (~/.grok) or a grok account's name.
    pub source: String,
    /// A voice id from /v1/tts/voices: eve, ara, leo, rex, sal, ...
    pub voice: String,
    /// 0.7 to 1.5, 1.0 is normal.
    pub speed: f32,
    /// BCP-47 code or "auto".
    pub language: String,
    /// No audio within this long: the sentence goes to the fallback.
    pub timeout_ms: u32,
}

/// `[voice.grok_stt]`: Grok speech recognition. Microphone audio (each
/// utterance, after the speaker lock) goes to xAI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GrokSttCfg {
    /// No text within this long: the utterance goes to the fallback.
    pub timeout_ms: u32,
    /// Live partials from whisper or Apple while you talk.
    pub local_partials: bool,
    /// When Grok fails: "whisper" (then apple) or "apple" (then whisper).
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
            show_email: true,
            show_path: true,
            privacy: false,
            layout: default_layout(),
            grid: String::new(),
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
            assistant: AssistantCfg::default(),
        }
    }
}

/// `GODTERM_<name>`, or the old `CLAUDEGO_<name>` (deprecated, read for
/// one more version). Empty counts as unset.
/// grok's [compat.claude] switches for GodTerm's grok slots. Hooks are
/// off by default: grok runs the user's Claude plugin hooks and fails on
/// them ("command not found: .../hooks/node"); MCP servers and sessions
/// are off too (they would start the user's Claude MCP servers).
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
    /// (environment variable, value) for each switch.
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

/// Where GodTerm keeps its state and where the user's own (non GodTerm)
/// Claude Code and Grok installs are. One place resolves these, so unit
/// tests can point them at fixtures without touching the environment.
#[derive(Debug, Clone, PartialEq)]
pub struct Dirs {
    /// `~/.godterm`, or `$GODTERM_HOME`.
    pub app_home: PathBuf,
    /// `~/.claude`, or `$GODTERM_MAIN_DIR`.
    pub main_claude: PathBuf,
    /// `~/.grok`, or `$GODTERM_MAIN_GROK_DIR`.
    pub main_grok: PathBuf,
    /// The home was given explicitly (no migration from ~/.claudego).
    pub explicit_home: bool,
    /// Whether to look at the main installs at all (tests: only when a
    /// fixture is set).
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
    // Demo mode: nothing may point at the user's real data.
    if crate::demo::active() {
        crate::demo::assert_safe(&d);
    }
    d
}

/// The user's real data dirs (`~/.claude`, `~/.grok`, `~/.godterm`, ...).
fn real_user_dirs() -> Vec<PathBuf> {
    let h = home_dir();
    [".claude", ".grok", ".godterm", ".claudego", ".claude.json"]
        .iter()
        .map(|d| h.join(d))
        .collect()
}

/// `p` is the home folder itself or inside one of the user's real data
/// dirs. Tests and demo mode refuse such paths.
pub fn is_real_user_dir(p: &Path) -> bool {
    p == home_dir() || real_user_dirs().iter().any(|r| p.starts_with(r))
}

/// Unit tests never read the environment for these: they use
/// `testing::set_dirs`, and until then a scratch dir that is not the
/// user's. Every lookup checks nothing points at a real user dir.
#[cfg(test)]
pub fn dirs() -> Dirs {
    let d = testing::current();
    testing::assert_safe(&d);
    d
}

/// `~/.godterm`, or `$GODTERM_HOME` when set.
pub fn app_home() -> PathBuf {
    dirs().app_home
}

#[cfg(test)]
pub mod testing {
    //! Test isolation: one lock for tests that change the shared dirs,
    //! the dirs themselves (never the process environment), and a check
    //! that fails when anything would point at the user's real data.
    use super::{home_dir, Dirs};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Held by every test that changes the dirs or builds an App.
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

    /// Point GodTerm's home at `home` (main installs off).
    pub fn set_home(home: &Path) {
        let mut d = current();
        d.app_home = home.to_path_buf();
        d.mains = false;
        d.main_claude = home.join("no-main-claude");
        d.main_grok = home.join("no-main-grok");
        assert_safe(&d);
        *DIRS.lock().unwrap_or_else(|e| e.into_inner()) = Some(d);
    }

    /// Fixture main installs until the guard drops (also on panic).
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

    /// Panics if the dirs, or GodTerm's own env vars, point at the user's
    /// real data. (CLAUDE_CONFIG_DIR and GROK_HOME may come from the shell
    /// running the tests; every tab is given its own slot dir instead.)
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
        // (Windows has no mode bits: the calls run, the modes are Unix.)
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

/// A path with the home folder written as `~`.
pub fn tilde(p: &Path) -> String {
    let home = home_dir();
    // One separator throughout: "~\AppData\Local" on Windows, not
    // "~/AppData\Local".
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

    /// The isolated CLAUDE_CONFIG_DIR for this slot: `config_dir` when set
    /// (kept byte for byte, since claude names the slot's keychain item
    /// after this exact string), else `<home>/accounts/<name>`.
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
    pub fn path() -> PathBuf {
        app_home().join("config.toml")
    }

    /// Load the config, writing a default one on first run.
    pub fn load_or_init() -> Result<Config> {
        let path = Self::path();
        if !path.exists() {
            let cfg = Config::default();
            // Never write a fresh config while a claudego install still
            // waits to be migrated: it would stand in for the real one.
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
# Each [[account]] gets an isolated Claude Code config dir at\n\
# ~/.godterm/accounts/<name>/ (passed to claude as CLAUDE_CONFIG_DIR).\n\
# The first four accounts are shown in the 2x2 grid.\n\n";
        write_atomic(&path, format!("{header}{body}").as_bytes())
    }

    /// Base folder for new tabs of account `i`.
    pub fn base_for(&self, i: usize) -> std::path::PathBuf {
        let b = self
            .accounts
            .get(i)
            .and_then(|a| a.new_tab_base.clone())
            .unwrap_or_else(|| self.new_tab_base.clone());
        expand_tilde(if b.trim().is_empty() { "~" } else { b.trim() })
    }

    /// Whether account `i` trusts folders automatically.
    pub fn trust_for(&self, i: usize) -> bool {
        self.accounts
            .get(i)
            .and_then(|a| a.auto_trust)
            .unwrap_or(self.auto_trust)
    }

    /// Effective permission mode of account `i`.
    pub fn mode_for(&self, i: usize) -> String {
        self.accounts
            .get(i)
            .and_then(|a| a.permission_mode.clone())
            .unwrap_or_else(|| self.permission_mode.clone())
    }

    pub fn claude_bin(&self) -> String {
        self.claude_bin
            .as_deref()
            .map(|s| expand_tilde(s).to_string_lossy().into_owned())
            .unwrap_or_else(|| "claude".into())
    }
}

/// Write a file only its owner can read (GodTerm's state, logs, config).
pub fn write_private(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    use crate::platform::{OpenOptionsExt, PermissionsExt};
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // An existing file keeps its mode on open: tighten it.
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    f.write_all(data.as_ref())
}

/// GodTerm's home is owner only (it holds logins, prompts and the
/// control token), and so are the files in it, also ones made by older
/// versions.
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

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
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
