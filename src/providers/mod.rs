//! The assistant's brain providers. Each one starts a backend that takes
//! turns and streams `BrainEvent`s back (the Claude stream-json shape),
//! so the app, the prompt, the tools and the confirmations are the same
//! for every provider. Adding one is a module here plus an entry in
//! `PROVIDERS` (docs/ASSISTANT_PROVIDERS.md).

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::Result;

use crate::app::AppEvent;
use crate::harness::Harness;

pub mod claude;
pub mod grok;

/// A running brain.
pub trait Backend: Send {
    /// Send one user turn.
    fn send(&mut self, text: &str) -> Result<()>;
    /// Stop the current turn (it still ends with a Done).
    fn interrupt(&mut self) -> Result<()>;
    fn pid(&self) -> u32;
    /// Still usable for the next turn.
    fn running(&mut self) -> bool;
    /// A control request (Remote Control); returns its request id.
    fn control(&mut self, _request: serde_json::Value) -> Result<String> {
        anyhow::bail!("this provider takes no control requests")
    }
}

/// What a provider can do.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// One process for the whole conversation (else one per turn).
    pub persistent: bool,
    /// Text streams as it is written.
    pub partial: bool,
    pub efforts: &'static [&'static str],
    /// Claude Code Remote Control (claude.ai/code and the app).
    pub remote_control: bool,
}

/// Everything a backend needs to start.
pub struct StartCtx<'a> {
    pub bin: String,
    /// Where its login and settings live (CLAUDE_CONFIG_DIR / GROK_HOME).
    pub home: PathBuf,
    pub cfg: &'a crate::config::AssistantCfg,
    pub model: String,
    pub pass_env: &'a [String],
    pub events: Sender<AppEvent>,
    pub gen: u64,
    /// The assembled system prompt (sections and learned rules).
    pub system_prompt: String,
}

pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    /// Accounts of this harness can run it (None: it has its own login).
    pub harness: Option<Harness>,
    pub caps: Caps,
    /// Models offered in the menu (the first is the default): id, name,
    /// a two word description ("quickest").
    pub models: &'static [(&'static str, &'static str, &'static str)],
    /// Its usage limits from a fetched usage reply, as buckets (none:
    /// it reports no usage). The binding one is marked.
    pub usage: fn(&crate::usage::Usage) -> Vec<Bucket>,
    /// The program to run.
    pub bin: fn(&crate::config::Config) -> String,
    /// Its own home and whether it is logged in (providers with no
    /// account of their own: `harness` None).
    pub own_home: Option<fn() -> PathBuf>,
    pub start: fn(StartCtx) -> Result<Box<dyn Backend>>,
}

pub const PROVIDERS: &[Provider] = &[claude::PROVIDER, grok::PROVIDER];

/// One usage limit of an account, whatever the provider calls it.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    /// Short: "5h", "wk", "Build".
    pub label: String,
    /// A word for speech and hints: "5 hour", "weekly".
    pub long: String,
    pub left_pct: f64,
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The lowest of the ones that limit it: what it can really do.
    pub binding: bool,
}

/// Low enough to warn about (percent left of the binding bucket).
pub const LOW_PCT: f64 = 20.0;

/// The buckets for `keys` (window key, short, long) that the reply has,
/// in that order, the lowest marked binding (a reset that passed reads
/// as full, like AccountState::binding).
pub fn buckets_of(u: &crate::usage::Usage, keys: &[(&str, &str, &str)]) -> Vec<Bucket> {
    let mut v: Vec<Bucket> = vec![];
    for (key, short, long) in keys {
        if let Some(prefix) = key.strip_suffix('*') {
            for w in u.windows.iter().filter(|w| w.key.starts_with(prefix)) {
                v.push(Bucket {
                    label: w.short.clone(),
                    long: w.label.clone(),
                    left_pct: w.left_now(),
                    resets_at: w.resets_at,
                    binding: false,
                });
            }
        } else if let Some(w) = u.get(key) {
            v.push(Bucket {
                label: short.to_string(),
                long: long.to_string(),
                left_pct: w.left_now(),
                resets_at: w.resets_at,
                binding: false,
            });
        }
    }
    let low = v
        .iter()
        .enumerate()
        .min_by(|a, b| {
            a.1.left_pct
                .partial_cmp(&b.1.left_pct)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i);
    if let Some(i) = low {
        v[i].binding = true;
    }
    v
}

/// The provider that reads the usage of accounts of harness `h` (Grok
/// accounts are read like the assistant's own Grok login).
pub fn for_harness(h: Harness) -> Option<&'static Provider> {
    PROVIDERS
        .iter()
        .find(|p| p.harness == Some(h))
        .or_else(|| PROVIDERS.iter().find(|p| p.id == h.name()))
}

/// The binding bucket.
pub fn binding(b: &[Bucket]) -> Option<&Bucket> {
    b.iter().find(|b| b.binding)
}

/// "5h 89%".
pub fn bucket_text(b: &Bucket) -> String {
    format!("{} {:.0}%", b.label, b.left_pct.floor())
}

pub fn by_id(id: &str) -> &'static Provider {
    PROVIDERS
        .iter()
        .find(|p| p.id == id)
        .unwrap_or(&PROVIDERS[0])
}

/// The model to use for `p` from the config (its own key per provider).
pub fn model_for(p: &Provider, cfg: &crate::config::AssistantCfg) -> String {
    let m = match p.id {
        "claude" => cfg.model.clone(),
        _ => cfg.models.get(p.id).cloned().unwrap_or_default(),
    };
    if m.trim().is_empty() {
        p.models[0].0.to_string()
    } else {
        m
    }
}
