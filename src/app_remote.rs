//! Claude Code Remote Control for the assistant itself: one control
//! request to the running brain turns it on (claude.ai/code and the
//! Claude app can then talk to it), another turns it off. It lives and
//! dies with the brain process: a switch of account, provider or model
//! ends it. Providers declare support (`caps.remote_control`).

use serde_json::{json, Value};

use crate::app::App;
use crate::app_assistant::{Entry, Who};

#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    pub name: String,
    pub url: Option<String>,
    pub account: Option<usize>,
    /// The brain process it belongs to.
    pub gen: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemotePending {
    pub request: String,
    pub enable: bool,
    pub name: String,
}

impl App {
    pub fn remote_supported(&self) -> Result<(), String> {
        let p = crate::providers::by_id(&self.cfg.assistant.provider);
        if !p.caps.remote_control {
            return Err(format!(
                "Remote Control works only when the assistant runs on Claude ({} has none). Say switch the assistant to Claude first.",
                p.name
            ));
        }
        Ok(())
    }

    /// The default session name: "GodTerm assistant (Account 2)".
    pub fn remote_default_name(&self) -> String {
        let a = self
            .assistant_account()
            .map(|a| self.cfg.accounts[a].display().to_string())
            .unwrap_or_default();
        format!(
            "GodTerm assistant{}",
            if a.is_empty() {
                String::new()
            } else {
                format!(" ({a})")
            }
        )
    }

    /// Ask the brain to turn Remote Control on (after the user's yes).
    pub fn enable_remote(&mut self, name: &str) -> Result<String, String> {
        self.remote_supported()?;
        self.ensure_brain()?;
        let b = self
            .assistant
            .brain
            .as_mut()
            .ok_or("the assistant is not running")?;
        let id = b
            .control(json!({"subtype": "remote_control", "enabled": true, "name": name}))
            .map_err(|e| format!("{e:#}"))?;
        crate::log::info(&format!("remote control: enabling as '{name}' ({id})"));
        self.assistant.remote_pending = Some(RemotePending {
            request: id,
            enable: true,
            name: name.to_string(),
        });
        Ok(format!("Turning on Remote Control as '{name}'."))
    }

    pub fn disable_remote(&mut self) -> Result<String, String> {
        let Some(r) = self.assistant.remote.clone() else {
            return Err("Remote Control is not on".into());
        };
        if let Some(b) = self.assistant.brain.as_mut() {
            if let Ok(id) = b.control(json!({"subtype": "remote_control", "enabled": false})) {
                self.assistant.remote_pending = Some(RemotePending {
                    request: id,
                    enable: false,
                    name: r.name.clone(),
                });
            }
        }
        self.assistant.remote = None;
        self.admin_record(&format!("Remote Control off ('{}')", r.name));
        Ok("Remote Control is off.".into())
    }

    /// A control response from the brain.
    pub fn on_remote_response(&mut self, id: &str, r: Result<Value, String>) {
        let Some(p) = self
            .assistant
            .remote_pending
            .clone()
            .filter(|p| p.request == id)
        else {
            return;
        };
        self.assistant.remote_pending = None;
        if !p.enable {
            crate::log::info("remote control: off");
            return;
        }
        match r {
            Ok(v) => {
                let url = v["session_url"]
                    .as_str()
                    .or(v["connect_url"].as_str())
                    .map(str::to_string);
                let gen = self.assistant.brain.as_ref().map(|b| b.gen).unwrap_or(0);
                self.assistant.remote = Some(Remote {
                    name: p.name.clone(),
                    url: url.clone(),
                    account: self.assistant.brain.as_ref().and_then(|b| b.account),
                    gen,
                });
                self.admin_record(&format!("Remote Control on as '{}'", p.name));
                if let Some(u) = &url {
                    self.assistant.log.push(Entry {
                        who: Who::Note,
                        text: format!("Remote Control: '{}' · {u}", p.name),
                    });
                }
                self.announce_progress(&format!(
                    "Remote Control is on. Open claude.ai/code or the Claude app and pick '{}'.",
                    p.name
                ));
            }
            Err(e) => {
                crate::log::info(&format!("remote control: refused: {e}"));
                self.announce_progress(&format!(
                    "Remote Control did not turn on: {}.",
                    crate::sessions::snippet(&e, 100)
                ));
            }
        }
    }

    /// The brain was replaced: Remote Control went with it.
    pub fn remote_lost(&mut self, why: &str) {
        if let Some(r) = self.assistant.remote.take() {
            crate::log::info(&format!("remote control: ended ({why})"));
            let msg = format!(
                "Remote Control '{}' ended ({why}). Say turn on remote control to start it again.",
                r.name
            );
            self.assistant.log.push(Entry {
                who: Who::Note,
                text: msg.clone(),
            });
            self.flash(msg);
        }
        self.assistant.remote_pending = None;
    }

    /// A user message in the brain's stream: our own echo, or one that
    /// came through Remote Control (it starts a turn of its own).
    pub fn on_user_text(&mut self, text: &str) {
        if let Some(i) = self
            .assistant
            .sent
            .iter()
            .position(|s| s.trim() == text.trim())
        {
            self.assistant.sent.drain(..=i);
            return;
        }
        if self.assistant.remote.is_none() {
            return;
        }
        crate::log::info(&format!(
            "assistant: via Remote Control: {}",
            text.chars().take(200).collect::<String>()
        ));
        self.assistant.log.push(Entry {
            who: Who::User,
            text: format!("{text} · via Remote Control"),
        });
        self.assistant.last_user = text.to_string();
        self.assistant.turn_remote = true;
        self.assistant.turn_reads = 0;
        self.assistant_turn += 1;
        self.assistant_calls = 0;
        self.assistant.busy = true;
        self.assistant.last_event = Some(std::time::Instant::now());
        if let Some(c) = &self.assistant.conv {
            c.write("user", json!({"text": text, "via": "remote_control"}));
        }
    }

    /// The menu item: the first pick explains, a second within 15 s turns
    /// it on.
    pub fn remote_click_armed(&mut self) -> bool {
        let now = std::time::Instant::now();
        let armed = self
            .assistant
            .remote_armed
            .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(15));
        self.assistant.remote_armed = if armed { None } else { Some(now) };
        armed
    }

    pub fn remote_state_line(&self) -> String {
        match &self.assistant.remote {
            Some(r) => format!(
                "remote control: on as '{}'{}; it ends if you switch account, provider or model (say so first)\n",
                r.name,
                r.account
                    .map(|a| format!(" ({})", self.cfg.accounts[a].display()))
                    .unwrap_or_default()
            ),
            None => String::new(),
        }
    }

    pub fn remote_json(&self) -> Value {
        match &self.assistant.remote {
            Some(r) => {
                json!({"on": true, "name": r.name, "url": r.url, "account": r.account.map(|a| a + 1)})
            }
            None => json!({"on": false, "supported": self.remote_supported().is_ok()}),
        }
    }
}
