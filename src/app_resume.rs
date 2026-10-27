use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::app::App;
use crate::providers::{Provider, SessionPlan};

pub const MAX_RESUME_BYTES: u64 = 3 * 1024 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Kept {
    pub id: String,
    pub provider: String,

    pub account: String,

    #[serde(default)]
    pub live: bool,
}

pub fn path() -> PathBuf {
    crate::assistant::dir().join("session.json")
}

pub fn load() -> Option<Kept> {
    serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()
}

pub fn save(k: &Kept) {
    let _ = std::fs::create_dir_all(crate::assistant::dir());
    if let Ok(s) = serde_json::to_string_pretty(k) {
        let _ = crate::config::write_private(&path(), s);
    }
}

pub fn mark_clean_exit() {
    if let Some(mut k) = load() {
        if k.live {
            k.live = false;
            save(&k);
        }
    }
}

pub fn project_slug(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn is_assistant_project(dir_name: &str) -> bool {
    let d = crate::assistant::dir();
    if project_slug(&d) == dir_name {
        return true;
    }
    d.canonicalize().is_ok_and(|c| project_slug(&c) == dir_name)
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

pub fn session_path(provider: &Provider, home: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\', '.']) {
        return None;
    }
    let (root, name) = if provider.id == "claude" {
        ("projects", format!("{id}.jsonl"))
    } else {
        ("sessions", id.to_string())
    };
    std::fs::read_dir(home.join(root))
        .ok()?
        .flatten()
        .map(|d| d.path().join(&name))
        .find(|p| p.exists())
}

pub fn session_size(p: &Path) -> u64 {
    if p.is_dir() {
        dir_size(p)
    } else {
        p.metadata().map(|m| m.len()).unwrap_or(0)
    }
}

impl App {
    pub(crate) fn session_plan(
        &mut self,
        provider: &Provider,
        account: Option<usize>,
        home: &Path,
    ) -> Option<SessionPlan> {
        let fresh = std::mem::take(&mut self.assistant.fresh_session);
        if !self.cfg.assistant.resume {
            return None;
        }
        let acct = account
            .map(|a| self.cfg.accounts[a].name.clone())
            .unwrap_or_default();
        if let Some(k) = load().filter(|k| !k.id.is_empty()) {
            let k = self.adopt_kept_session(k, provider, &acct, home);
            let same = k.provider == provider.id && k.account == acct;
            let found = same.then(|| session_path(provider, home, &k.id)).flatten();
            let size = found.as_deref().map(session_size).unwrap_or(0);
            let why = if fresh {
                Some("a new conversation".to_string())
            } else if k.provider != provider.id {
                Some(format!("the kept one is a {} session", k.provider))
            } else if !same {
                Some("the kept one belongs to another account".into())
            } else if found.is_none() {
                Some("the kept one is gone".into())
            } else if size > MAX_RESUME_BYTES {
                Some(format!(
                    "the kept one is large ({:.1} MB)",
                    size as f64 / 1_048_576.0
                ))
            } else {
                None
            };
            match why {
                None => {
                    crate::log::info(&format!(
                        "assistant: resuming its session {} ({} KB)",
                        k.id,
                        size / 1024
                    ));
                    save(&Kept {
                        live: true,
                        ..k.clone()
                    });
                    return Some(SessionPlan {
                        id: k.id,
                        resume: true,
                    });
                }
                Some(w) => {
                    crate::log::info(&format!("assistant: a fresh session ({w})"));

                    if let Some(p) = found.filter(|_| provider.id == "claude") {
                        let _ = std::fs::remove_file(&p);
                        let _ = std::fs::remove_dir_all(p.with_extension(""));
                    }
                }
            }
        }
        let id = crate::session_ops::new_uuid();
        crate::log::info(&format!("assistant: new session {id}"));
        save(&Kept {
            id: id.clone(),
            provider: provider.id.to_string(),
            account: acct,
            live: true,
        });
        Some(SessionPlan { id, resume: false })
    }

    pub(crate) fn resume_failed(&mut self, why: &str, was_busy: bool) {
        if let Some(f) = self.take_switch_fallback() {
            self.assistant.carry = Some(f);
        }
        self.assistant.resumed = None;
        self.assistant.fresh_session = true;
        if let Some(mut k) = load() {
            k.id.clear();
            save(&k);
        }
        crate::log::info(&format!(
            "assistant: could not resume its session ({why}); starting fresh"
        ));
        self.note("Could not continue the assistant's session; it starts fresh with a summary.");
        if self.assistant.carry.is_none() {
            self.assistant.carry = self.conversation_carry();
        }
        self.assistant.brain = None;
        self.assistant.busy = false;
        if !was_busy {
            return;
        }
        let Some((text, heard, system)) = self.assistant.last_turn.clone() else {
            return;
        };
        let n = self.assistant.log.len();
        self.start_turn(&text, heard.as_deref(), system.as_deref());

        if self.assistant.log.len() > n {
            self.assistant.log.remove(n);
        }
    }

    pub(crate) fn crash_check(&mut self) {
        if self.assistant.crash_checked {
            return;
        }
        self.assistant.crash_checked = true;
        if !self.cfg.assistant.resume || !self.assistant_on() || self.assistant.started.is_some() {
            return;
        }
        if load().is_some_and(|k| k.live && !k.id.is_empty()) {
            crate::log::info(
                "assistant: the last run did not exit cleanly; resuming its session now",
            );
            self.note(
                "GodTerm did not exit cleanly last time: the assistant continues its session.",
            );
            self.assistant.eager_start = true;
        }
    }
}
