//! Claude Code Remote Control for the assistant itself: one control
//! request to the running brain turns it on (claude.ai/code and the
//! Claude app can then talk to it), another turns it off. It lives and
//! dies with the brain process. Providers declare support
//! (`caps.remote_control`).
//!
//! Turning it on is a flow with an honest end: each attempt waits up to
//! 20 s for claude's answer; a failure is tried again once after a short
//! pause, then once more after restarting the brain (its conversation
//! kept), which covers a claude binary updated or replaced under it. The
//! tool call that asked waits for the outcome (the link on success).
//! With assistant.remote_control on it is turned on whenever the brain
//! starts and again when the bridge drops.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_assistant::{Entry, Who};

/// How long one attempt waits for claude's answer.
pub const ANSWER_WAIT: Duration = Duration::from_secs(20);
/// The pause before the second attempt.
pub const RETRY_DELAY: Duration = Duration::from_secs(2);
/// Kept on: the pause after a flow failed for good.
pub const AUTO_BACKOFF: Duration = Duration::from_secs(300);
/// Kept on: the pause after the bridge dropped.
pub const DROP_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    pub name: String,
    pub url: Option<String>,
    pub account: Option<usize>,
    /// The brain process it belongs to.
    pub gen: u64,
    /// The bridge to claude.ai: "on" until claude reports it, then
    /// "connected", "reconnecting", ...
    pub bridge: String,
}

impl Remote {
    pub fn connected(&self) -> bool {
        matches!(self.bridge.as_str(), "on" | "connected")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemotePending {
    pub request: String,
    pub enable: bool,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlowStage {
    /// The request is out (since).
    Sent(Instant),
    /// The next attempt goes out at this time.
    RetryAt(Instant),
    /// The brain restarts (once it is between turns), then the last try.
    Restart,
}

/// Turning Remote Control on.
#[derive(Debug)]
pub struct RemoteFlow {
    pub name: String,
    /// Started by assistant.remote_control (not by a request).
    pub auto: bool,
    /// Attempts made (1 to 3).
    pub attempt: u8,
    pub stage: FlowStage,
    /// The tool call waiting for the outcome.
    pub reply: Option<Sender<Value>>,
    pub last_error: String,
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

    /// assistant.remote_control is set and can act now (Claude, the
    /// assistant on, not turned off by the user this run).
    pub fn remote_kept_on(&self) -> bool {
        self.cfg.assistant.remote_control
            && self.assistant_on()
            && self.remote_supported().is_ok()
            && !self.assistant.remote_off_by_user
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

    /// Start turning it on: `reply` (a waiting tool call) gets the outcome.
    pub(crate) fn remote_start(
        &mut self,
        name: &str,
        auto: bool,
        reply: Option<Sender<Value>>,
    ) -> Result<(), String> {
        self.remote_supported()?;
        if self.assistant.remote_flow.is_some() {
            return Err(
                "Remote Control is already turning on; its outcome comes as a progress line".into(),
            );
        }
        self.ensure_brain()?;
        self.assistant.remote_off_by_user = false;
        self.assistant.remote_flow = Some(RemoteFlow {
            name: name.to_string(),
            auto,
            attempt: 1,
            stage: FlowStage::Sent(Instant::now()),
            reply,
            last_error: String::new(),
        });
        self.remote_send();
        Ok(())
    }

    /// Send the current attempt's request.
    fn remote_send(&mut self) {
        let Some(name) = self.assistant.remote_flow.as_ref().map(|f| f.name.clone()) else {
            return;
        };
        let sent = match self.ensure_brain() {
            Ok(()) => match self.assistant.brain.as_mut() {
                Some(b) => b
                    .control(json!({"subtype": "remote_control", "enabled": true, "name": name}))
                    .map_err(|e| format!("{e:#}")),
                None => Err("the assistant is not running".into()),
            },
            Err(e) => Err(e),
        };
        match sent {
            Ok(id) => {
                let n = self.assistant.remote_flow.as_ref().map_or(1, |f| f.attempt);
                crate::log::info(&format!(
                    "remote control: enabling as '{name}' ({id}, attempt {n})"
                ));
                self.assistant.remote_pending = Some(RemotePending {
                    request: id,
                    enable: true,
                    name,
                });
                if let Some(f) = self.assistant.remote_flow.as_mut() {
                    f.stage = FlowStage::Sent(Instant::now());
                }
            }
            Err(e) => self.remote_attempt_failed(&e),
        }
    }

    /// Ask the brain to turn Remote Control on (the menu, after its second
    /// pick): the outcome comes as a progress line.
    pub fn enable_remote(&mut self, name: &str) -> Result<String, String> {
        self.remote_start(name, false, None)?;
        Ok(format!(
            "Turning on Remote Control as '{name}'; the link shows in the panel once it is up."
        ))
    }

    /// The tool (after the user's yes): its reply waits for the outcome.
    pub fn enable_remote_tool(&mut self, name: &str) -> Result<Value, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.remote_start(name, false, Some(tx))?;
        self.pending_job = Some(rx);
        Ok(
            json!({"_wait": {"job": true, "deliveries": [], "max_ms": 55000,
            "timeout_error": "Remote Control has not answered yet: it is NOT on. Say it is still turning on; a progress line tells the outcome."}}),
        )
    }

    pub fn disable_remote(&mut self) -> Result<String, String> {
        let Some(r) = self.assistant.remote.clone() else {
            if let Some(f) = self.assistant.remote_flow.take() {
                self.assistant.remote_pending = None;
                self.assistant.remote_off_by_user = true;
                if let Some(tx) = f.reply {
                    let _ =
                        tx.send(json!({"error": "stopped: the user turned Remote Control off"}));
                }
                return Ok("Stopped turning Remote Control on.".into());
            }
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
        self.assistant.remote_off_by_user = true;
        self.admin_record(&format!("Remote Control off ('{}')", r.name));
        Ok(if self.cfg.assistant.remote_control {
            "Remote Control is off until GodTerm restarts; Keep Remote Control on is still set (say keep remote control off to stop it for good).".into()
        } else {
            "Remote Control is off.".into()
        })
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
            Ok(v) => self.remote_on(&p.name, &v),
            Err(e) => {
                crate::log::info(&format!("remote control: refused: {e}"));
                self.remote_attempt_failed(&e);
            }
        }
    }

    fn remote_on(&mut self, name: &str, v: &Value) {
        let flow = self.assistant.remote_flow.take();
        let url = v["session_url"]
            .as_str()
            .or(v["connect_url"].as_str())
            .map(str::to_string);
        let gen = self.assistant.brain.as_ref().map(|b| b.gen).unwrap_or(0);
        self.assistant.remote = Some(Remote {
            name: name.to_string(),
            url: url.clone(),
            account: self.assistant.brain.as_ref().and_then(|b| b.account),
            gen,
            bridge: "on".into(),
        });
        self.assistant.remote_retry_at = None;
        if url.is_some() {
            self.assistant.remote_last_url = url.clone();
        }
        let auto = flow.as_ref().is_some_and(|f| f.auto);
        let attempt = flow.as_ref().map_or(1, |f| f.attempt);
        crate::log::info(&format!(
            "remote control: on as '{name}'{} (attempt {attempt}{})",
            url.as_deref()
                .map(|u| format!(" at {u}"))
                .unwrap_or_default(),
            if auto { ", kept on" } else { "" }
        ));
        self.admin_record(&format!("Remote Control on as '{name}'"));
        self.note(format!(
            "Remote Control: '{name}' · {}",
            url.as_deref().unwrap_or("no link given")
        ));
        let how = match attempt {
            1 => "",
            2 => " (on the second try)",
            _ => " (after restarting my claude process)",
        };
        let say = match &url {
            Some(u) => format!(
                "Remote Control is on{how}. Open {u}, or pick '{name}' in claude.ai/code or the Claude app."
            ),
            None => format!(
                "Remote Control is on{how}. Pick '{name}' in claude.ai/code or the Claude app."
            ),
        };
        let reply = flow.and_then(|f| f.reply);
        match reply {
            Some(tx) => {
                let _ = tx.send(json!({"on": true, "name": name, "session_url": url, "say": say}));
            }
            None if auto => self.flash(say),
            None => self.announce_progress(&say),
        }
    }

    /// An attempt failed (refused, no answer in time, or not sent): try
    /// again, restart the brain, or give up and say so.
    fn remote_attempt_failed(&mut self, e: &str) {
        self.assistant.remote_pending = None;
        let Some(mut f) = self.assistant.remote_flow.take() else {
            return;
        };
        let reason = crate::sessions::snippet(e.trim().trim_end_matches('.'), 100);
        f.last_error = reason.clone();
        crate::log::info(&format!(
            "remote control: attempt {} failed: {reason}",
            f.attempt
        ));
        match f.attempt {
            1 => {
                self.note(format!(
                    "Remote Control did not turn on ({reason}); trying again."
                ));
                f.stage = FlowStage::RetryAt(Instant::now() + RETRY_DELAY);
                self.assistant.remote_flow = Some(f);
            }
            2 => {
                let say = format!(
                    "Remote Control did not turn on: {reason}. I'm restarting my claude process and trying once more; I'll say how it went."
                );
                self.note(say.clone());
                if let Some(tx) = f.reply.take() {
                    let _ = tx.send(json!({
                        "error": format!("Remote Control did not turn on: {reason} (tried twice)"),
                        "pending": true,
                        "say": say,
                        "next": "Say the say text. Remote Control is NOT on: never say it is on, connected or reachable. A progress line after the restart tells the outcome.",
                    }));
                }
                f.stage = FlowStage::Restart;
                self.assistant.remote_flow = Some(f);
            }
            _ => {
                let mut say = format!("Remote Control did not turn on: {reason}.");
                if f.auto || self.remote_kept_on() {
                    self.assistant.remote_retry_at = Some(Instant::now() + AUTO_BACKOFF);
                    say.push_str(" I'll try again in 5 minutes.");
                }
                if let Some(tx) = f.reply.take() {
                    let _ = tx.send(json!({"error": format!("Remote Control did not turn on: {reason}"), "say": say}));
                } else {
                    self.announce_progress(&say);
                }
            }
        }
    }

    /// Restart the brain for the last try, keeping the conversation: its
    /// session is resumed (assistant.resume), or a summary carried.
    fn remote_restart_brain(&mut self) {
        crate::log::info("remote control: restarting the assistant's claude for the last try");
        if !self.cfg.assistant.resume {
            if let Some(c) = self.conversation_carry() {
                self.assistant.carry = Some(c);
            }
        }
        self.assistant.brain = None;
        self.assistant.remote = None;
        if let Some(f) = self.assistant.remote_flow.as_mut() {
            f.attempt = 3;
            f.stage = FlowStage::Sent(Instant::now());
        }
        self.remote_send();
    }

    /// Every tick: attempts that took too long, retries and the restart,
    /// a Remote Control whose brain is gone, and keeping it on.
    pub fn remote_tick(&mut self) {
        let gen = self.assistant.brain.as_ref().map(|b| b.gen);
        if self
            .assistant
            .remote
            .as_ref()
            .is_some_and(|r| Some(r.gen) != gen)
        {
            self.remote_lost("the assistant restarted");
        }
        let now = Instant::now();
        match self.assistant.remote_flow.as_ref().map(|f| f.stage) {
            Some(FlowStage::Sent(at)) if now.duration_since(at) > ANSWER_WAIT => {
                self.remote_attempt_failed(&format!(
                    "claude did not answer in {} s",
                    ANSWER_WAIT.as_secs()
                ));
            }
            Some(FlowStage::RetryAt(t)) if now >= t => {
                if let Some(f) = self.assistant.remote_flow.as_mut() {
                    f.attempt += 1;
                }
                self.remote_send();
            }
            Some(FlowStage::Restart) if !self.assistant.busy => self.remote_restart_brain(),
            _ => {}
        }
        if self.assistant.eager_start && self.assistant.brain.is_none() {
            self.assistant.eager_start = false;
            if let Err(e) = self.ensure_brain() {
                crate::log::info(&format!("assistant: could not start after a crash: {e}"));
            }
        }
        // Kept on: turn it on when it is not (a new brain, a drop).
        if self.remote_kept_on()
            && self.assistant.remote.is_none()
            && self.assistant.remote_flow.is_none()
            && self.assistant.remote_retry_at.is_none_or(|t| now >= t)
            && self
                .assistant_account()
                .is_some_and(|a| self.accounts[a].login.logged_in())
        {
            self.assistant.remote_retry_at = None;
            let name = self.remote_default_name();
            if let Err(e) = self.remote_start(&name, true, None) {
                crate::log::info(&format!("remote control: kept on, but: {e}"));
                self.assistant.remote_retry_at = Some(now + AUTO_BACKOFF);
            }
        }
    }

    /// claude's bridge to claude.ai changed state.
    pub fn on_bridge_state(&mut self, state: &str) {
        crate::log::info(&format!("remote control: bridge {state}"));
        let Some(r) = self.assistant.remote.as_mut() else {
            return;
        };
        match state {
            "disconnected" | "failed" | "error" | "closed" | "stopped" => {
                let name = r.name.clone();
                self.assistant.remote = None;
                if self.remote_kept_on() {
                    self.assistant.remote_retry_at = Some(Instant::now() + DROP_DELAY);
                    self.note(format!(
                        "Remote Control '{name}' dropped ({state}); turning it back on."
                    ));
                } else {
                    let msg = format!(
                        "Remote Control '{name}' dropped ({state}). Say turn on remote control to start it again."
                    );
                    self.note(msg.clone());
                    self.flash(msg);
                }
            }
            s => r.bridge = s.to_string(),
        }
    }

    /// The brain was replaced: Remote Control went with it.
    pub fn remote_lost(&mut self, why: &str) {
        if let Some(r) = self.assistant.remote.take() {
            crate::log::info(&format!("remote control: ended ({why})"));
            let msg = if self.remote_kept_on() {
                format!("Remote Control '{}' ended ({why}); it turns back on by itself, with a new link.", r.name)
            } else {
                format!(
                    "Remote Control '{}' ended ({why}). Say turn on remote control to start it again.",
                    r.name
                )
            };
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

    /// The panel's Remote indicator: open the link (and copy it).
    pub fn open_remote_link(&mut self) {
        let url = self
            .assistant
            .remote
            .as_ref()
            .and_then(|r| r.url.clone())
            .or_else(|| self.assistant.remote_last_url.clone());
        match url {
            Some(u) => {
                crate::admin::open_url(&u);
                if !cfg!(test) {
                    crate::procs::copy_to_clipboard(u.clone());
                }
                self.flash(format!("Remote Control link opened and copied: {u}"));
            }
            None => self.flash("Remote Control has no link yet"),
        }
    }

    /// The indicator in the panel title: "Remote" while connected,
    /// "Remote…" while connecting or reconnecting; None when off.
    pub fn remote_indicator(&self) -> Option<String> {
        match &self.assistant.remote {
            Some(r) if r.connected() => Some("Remote".into()),
            Some(_) => Some("Remote…".into()),
            None if self.assistant.remote_flow.is_some() => Some("Remote…".into()),
            None => None,
        }
    }

    pub fn remote_state_line(&self) -> String {
        if let Some(f) = &self.assistant.remote_flow {
            return format!(
                "remote control: turning on (attempt {}), NOT on yet\n",
                f.attempt
            );
        }
        match &self.assistant.remote {
            Some(r) => format!(
                "remote control: on as '{}'{}{}, bridge {}{}; it ends if you switch account, provider or model (say so first)\n",
                r.name,
                r.account
                    .map(|a| format!(" ({})", self.cfg.accounts[a].display()))
                    .unwrap_or_default(),
                r.url.as_deref().map(|u| format!(" at {u}")).unwrap_or_default(),
                r.bridge,
                if self.cfg.assistant.remote_control { ", kept on" } else { "" }
            ),
            None if self.cfg.assistant.remote_control && self.remote_supported().is_err() => {
                "remote control: kept on, but not on this provider (Claude only)\n".into()
            }
            None if self.cfg.assistant.remote_control => "remote control: off (kept on: it turns on by itself)\n".into(),
            None => String::new(),
        }
    }

    pub fn remote_json(&self) -> Value {
        let keep = self.cfg.assistant.remote_control;
        match &self.assistant.remote {
            Some(r) => {
                json!({"on": true, "name": r.name, "url": r.url, "bridge": r.bridge, "account": r.account.map(|a| a + 1), "kept_on": keep})
            }
            None => json!({
                "on": false,
                "turning_on": self.assistant.remote_flow.is_some(),
                "supported": self.remote_supported().is_ok(),
                "kept_on": keep,
                "last_url": self.assistant.remote_last_url,
            }),
        }
    }
}
