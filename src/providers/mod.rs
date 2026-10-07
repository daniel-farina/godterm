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
}

/// What a provider can do.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// One process for the whole conversation (else one per turn).
    pub persistent: bool,
    /// Text streams as it is written.
    pub partial: bool,
    pub efforts: &'static [&'static str],
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
    /// Models offered in the menu (the first is the default).
    pub models: &'static [(&'static str, &'static str)],
    /// The program to run.
    pub bin: fn(&crate::config::Config) -> String,
    /// Its own home and whether it is logged in (providers with no
    /// account of their own: `harness` None).
    pub own_home: Option<fn() -> PathBuf>,
    pub start: fn(StartCtx) -> Result<Box<dyn Backend>>,
}

pub const PROVIDERS: &[Provider] = &[claude::PROVIDER, grok::PROVIDER];

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
