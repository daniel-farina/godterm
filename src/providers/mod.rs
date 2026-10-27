use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::Result;

use crate::app::AppEvent;
use crate::harness::Harness;

pub mod claude;
pub mod grok;

pub trait Backend: Send {
    fn send(&mut self, text: &str) -> Result<()>;

    fn takes_images(&self) -> bool {
        false
    }

    fn send_images(
        &mut self,
        text: &str,
        _images: &[crate::assistant_images::ModelImage],
    ) -> Result<()> {
        self.send(text)
    }

    fn interrupt(&mut self) -> Result<()>;
    fn pid(&self) -> u32;

    fn running(&mut self) -> bool;

    fn control(&mut self, _request: serde_json::Value) -> Result<String> {
        anyhow::bail!("this provider takes no control requests")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub persistent: bool,

    pub partial: bool,
    pub efforts: &'static [&'static str],

    pub remote_control: bool,
}

pub struct StartCtx<'a> {
    pub bin: String,

    pub home: PathBuf,
    pub cfg: &'a crate::config::AssistantCfg,
    pub model: String,
    pub pass_env: &'a [String],
    pub events: Sender<AppEvent>,
    pub gen: u64,

    pub system_prompt: String,

    pub session: Option<SessionPlan>,

    pub extra_env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPlan {
    pub id: String,
    pub resume: bool,
}

pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,

    pub harness: Option<Harness>,
    pub caps: Caps,

    pub models: &'static [(&'static str, &'static str, &'static str)],

    pub tab_models: &'static [TabModel],

    pub usage: fn(&crate::usage::Usage) -> Vec<Bucket>,

    pub bin: fn(&crate::config::Config) -> String,

    pub own_home: Option<fn() -> PathBuf>,
    pub start: fn(StartCtx) -> Result<Box<dyn Backend>>,
}

pub const PROVIDERS: &[Provider] = &[claude::PROVIDER, grok::PROVIDER];

#[derive(Debug, Clone, Copy)]
pub struct TabModel {
    pub arg: &'static str,

    pub label: &'static str,

    pub desc: &'static str,

    pub matches: &'static [&'static str],
}

pub fn tab_models(h: Harness) -> &'static [TabModel] {
    for_harness(h).map(|p| p.tab_models).unwrap_or(&[])
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub label: String,

    pub long: String,
    pub left_pct: f64,
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,

    pub binding: bool,
}

pub const LOW_PCT: f64 = 20.0;

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

pub fn for_harness(h: Harness) -> Option<&'static Provider> {
    PROVIDERS
        .iter()
        .find(|p| p.harness == Some(h))
        .or_else(|| PROVIDERS.iter().find(|p| p.id == h.name()))
}

pub fn binding(b: &[Bucket]) -> Option<&Bucket> {
    b.iter().find(|b| b.binding)
}

pub fn bucket_text(b: &Bucket) -> String {
    format!("{} {:.0}%", b.label, b.left_pct.floor())
}

pub fn by_id(id: &str) -> &'static Provider {
    PROVIDERS
        .iter()
        .find(|p| p.id == id)
        .unwrap_or(&PROVIDERS[0])
}

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
