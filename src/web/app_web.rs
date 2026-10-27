use super::{Hub, ToApp};
use crate::app::App;
use crate::app_assistant::{Entry, Who};
use crate::pane::Activity;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const PUSH_EVERY: Duration = Duration::from_millis(100);

pub const WEB_RETRY: Duration = Duration::from_secs(5);

pub struct WebSide {
    pub hub: Arc<Hub>,

    pub changed: bool,
    last_push: Instant,
    state: String,

    log: Vec<(u64, Entry, bool)>,

    in_system: bool,
    next_id: u64,

    live: String,
    live_at: i64,
    approvals: Vec<String>,
    update: String,
    flash: Option<String>,

    taken: HashMap<u64, u64>,

    taken_by: HashMap<u64, String>,

    left_sizes: Vec<(u64, Instant)>,

    pub voice: Option<super::voice::WebVoice>,

    pub push: super::push::Watch,
}

impl WebSide {
    pub fn new(hub: Arc<Hub>) -> WebSide {
        WebSide {
            hub,
            changed: true,
            last_push: Instant::now() - PUSH_EVERY,
            state: String::new(),
            log: vec![],
            in_system: false,
            next_id: 1,
            live: String::new(),
            live_at: 0,
            approvals: vec![],
            update: String::new(),
            flash: None,
            taken: HashMap::new(),
            taken_by: HashMap::new(),
            left_sizes: vec![],
            voice: None,
            push: Default::default(),
        }
    }
}

fn role(w: &Who) -> &'static str {
    match w {
        Who::User => "user",
        Who::Reply => "assistant",
        Who::Tool => "tool",
        Who::Note => "note",
        Who::Preamble => "preamble",
    }
}

fn system_note(text: &str) -> bool {
    text.starts_with('(')
        && text.ends_with(')')
        && (text.starts_with("(events: ")
            || text.ends_with(" answered)")
            || text.ends_with(" replied; summing it up)"))
}

pub(super) fn hidden(e: &Entry, in_system: &mut bool) -> bool {
    match e.who {
        Who::User | Who::Reply => {
            *in_system = false;
            false
        }
        Who::Note if system_note(&e.text) => {
            *in_system = true;
            true
        }
        _ => *in_system,
    }
}

pub(super) fn message(id: u64, e: &Entry, hidden: bool) -> Value {
    let mut m = json!({"t": "assistant", "id": id, "role": role(&e.who), "text": e.text, "done": true, "at": e.at});
    if hidden {
        m["hidden"] = json!(true);
    }
    m
}

fn hex(c: ratatui::style::Color) -> String {
    match c {
        ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => String::new(),
    }
}

pub fn theme_json() -> Value {
    use crate::theme::*;
    json!({
        "bg": "#1c1e21", "fg": hex(FG), "dim": hex(DIM), "faint": hex(FAINT),
        "bar": hex(BAR_BG), "selection": hex(SEL_BG), "select": hex(SELECT_BG),
        "sage": hex(SAGE), "sand": hex(SAND), "slate": hex(SLATE), "clay": hex(CLAY),
        "mauve": hex(MAUVE), "stone": hex(STONE), "mist": hex(MIST), "rust": hex(RUST),
        "active": hex(ACTIVE), "attention": hex(ATTN_RED), "red": hex(MUTED_RED),
    })
}

const SIZE_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

impl App {
    pub fn web_size_back(&mut self, uid: u64) {
        if let Some(w) = self.web.as_mut() {
            w.taken.remove(&uid);
            w.taken_by.remove(&uid);
            w.changed = true;
        }
        if let Some((s, t)) = self.find_tab(uid) {
            let area = self.panes[s].size;
            let tab = &mut self.panes[s].tabs[t];
            tab.web_size = None;
            tab.web_sizer = None;
            tab.resize(area.0, area.1);
        }
    }

    pub fn start_web(&mut self) {
        if super::wanted(&self.cfg) {
            self.try_start_web(true);
        }
    }

    pub(crate) fn try_start_web(&mut self, first: bool) {
        let port = super::port(&self.cfg);
        let web = self.cfg.web.clone();
        match super::start_with(port, || super::Opts::from_config(&web), self.tx.clone()) {
            Ok(hub) => {
                crate::log::info(&format!("web: the web app is on port {}", hub.port));
                if !first {
                    self.flash(format!("The web app is on port {} now", hub.port));
                }
                self.web = Some(WebSide::new(hub));
                self.web_retry_at = None;
                self.state_dirty = true;
            }
            Err(e) => {
                if first {
                    crate::log::error(&format!(
                        "web: not started: {e:#}; trying again every {} s",
                        WEB_RETRY.as_secs()
                    ));
                    self.flash(format!(
                        "The web app could not start ({e:#}): trying again every {} s",
                        WEB_RETRY.as_secs()
                    ));
                }
                self.web_retry_at = Some(Instant::now() + WEB_RETRY);
            }
        }
    }

    pub fn web_retry_tick(&mut self) -> Option<Duration> {
        let at = self.web_retry_at?;
        if Instant::now() < at {
            return Some(at - Instant::now());
        }
        self.try_start_web(false);
        self.web_retry_at
            .map(|a| a.saturating_duration_since(Instant::now()))
    }

    pub fn stop_web(&mut self) {
        self.web_retry_at = None;
        let ids: Vec<u64> = match &self.web {
            Some(w) => w.hub.clients().iter().map(|c| c.id).collect(),
            None => return,
        };
        for id in ids {
            self.web_event(ToApp::Gone(id));
        }
        if let Some(w) = self.web.take() {
            w.hub.stop();
        }
    }

    pub fn web_config_changed(&mut self, old: &crate::config::WebCfg) -> Option<String> {
        let new = self.cfg.web.clone();
        let running = self.web.is_some() || self.web_retry_at.is_some();
        if new.enabled != old.enabled {
            if new.enabled && !running {
                self.try_start_web(true);
                return Some("web app on".into());
            }
            if !new.enabled && running {
                self.stop_web();
                return Some("web app off".into());
            }
        }
        let tailnet_changed = new.tailnet_sign_in != old.tailnet_sign_in
            || new.tailnet_users != old.tailnet_users
            || new.tailnet_ip != old.tailnet_ip
            || new.ip_port != old.ip_port
            || new.hosts != old.hosts
            || new.assets_dir != old.assets_dir
            || new.embedded_assets != old.embedded_assets;
        if (new.port != old.port || tailnet_changed) && running {
            self.stop_web();
            self.try_start_web(true);
            return Some(format!("web app on port {}", super::port(&self.cfg)));
        }
        None
    }

    fn web_tabs(&self, state: &Value) -> Value {
        let tabs = state["tabs"].as_array().cloned().unwrap_or_default();
        let all = self.all_tabs();
        let out: Vec<Value> = tabs
            .into_iter()
            .zip(all)
            .map(|(mut v, (s, t))| {
                let tab = &self.panes[s].tabs[t];
                v["uid"] = json!(tab.uid);
                let (rows, cols) = tab.web.size();
                v["rows"] = json!(rows);
                v["cols"] = json!(cols);
                v["pane"] = json!(s + 1);
                v["active"] = json!(self.panes[s].active == t);
                v["running"] = json!(tab.is_running());

                let idle = tab.last_activity().elapsed().as_secs();
                let mut at = crate::app_assistant::now_ms() / 1000 - idle as i64;

                if !matches!(tab.activity, Activity::Working | Activity::Permission) {
                    let m = self
                        .transcript_path(s, t)
                        .and_then(|p| std::fs::metadata(p).ok())
                        .and_then(|m| m.modified().ok())
                        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok());
                    if let Some(m) = m {
                        at = m.as_secs() as i64;
                    }
                }
                v["active_at"] = json!(at - at.rem_euclid(30));

                if let Some(id) = &tab.session_id {
                    v["session_id"] = json!(id);
                }
                if let Some(c) = self.web.as_ref().and_then(|w| w.taken.get(&tab.uid)) {
                    v["size_taken_by"] = json!(c);
                    if let Some(d) = self.web.as_ref().and_then(|w| w.taken_by.get(&tab.uid)) {
                        v["size_device"] = json!(d);
                    }
                }

                if let Some(g) = self.web_git_facts(&tab.cwd) {
                    v["git_facts"] = g;
                }

                if let Some(l) = self.lease_json(s, t) {
                    v["lease"] = l;
                }
                v
            })
            .collect();
        json!(out)
    }

    pub fn web_git_facts(&self, dir: &std::path::Path) -> Option<Value> {
        let top = crate::project_git::repo_top(dir)?;
        let ix = self.project_index.try_lock().ok()?;
        let f = ix.git.get(&top)?;
        Some(json!({
            "branch": f.branch_word(),
            "dirty": f.staged + f.modified + f.untracked + f.conflicts,
            "unpushed": f.unpushed.unwrap_or(0).max(f.ahead.unwrap_or(0)),
            "behind": f.behind.unwrap_or(0),
            "error": f.error.is_some(),
        }))
    }

    fn web_account_models(&self, accounts: Value) -> Value {
        let mut v = accounts.as_array().cloned().unwrap_or_default();
        for (i, a) in v.iter_mut().enumerate() {
            let Some(acc) = self.cfg.accounts.get(i) else {
                continue;
            };
            let models: Vec<Value> = crate::app_tabmodel::models_for(acc.harness(), &self.cfg)
                .into_iter()
                .map(|m| json!({"arg": m.arg, "label": m.label, "desc": m.desc}))
                .collect();
            a["models"] = json!(models);

            if let Some(e) = self.email_label(i, 64) {
                a["email"] = json!(e);
            }
        }
        json!(v)
    }

    pub fn web_approvals(&self) -> Vec<Value> {
        let mut v = vec![];
        for p in self.pending_confirms.iter().filter(|p| p.answerable()) {
            v.push(json!({
                "id": format!("confirm:{}", p.token), "kind": "confirm", "tab": Value::Null,
                "question": p.summary, "choices": ["yes", "no"],
            }));
        }
        if let Some(a) = &self.assistant.selfswitch.asked {
            v.push(json!({
                "id": "switch", "kind": "switch", "tab": Value::Null,
                "question": a.question, "choices": ["yes", "no"],
            }));
        }
        for (s, t) in self.all_tabs() {
            let tab = &self.panes[s].tabs[t];
            if tab.activity != Activity::Permission {
                continue;
            }
            let q = self
                .request_of(s, t)
                .map(|r| {
                    if r.question.is_empty() {
                        r.summary()
                    } else {
                        format!("{} ({})", r.summary(), r.question)
                    }
                })
                .unwrap_or_else(|| "waiting for approval".into());

            use crate::prompt::Choice;
            let all = [
                ("yes", Choice::Approve),
                ("always", Choice::Always),
                ("no", Choice::Deny),
            ];
            let choices: Vec<&str> = match self.pane_harness(s).filter(|h| !h.integrated()) {
                None => all.iter().map(|c| c.0).collect(),
                Some(h) => {
                    let screen = tab
                        .parser
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .screen()
                        .contents();
                    let p = crate::prompt_cli::parse_prompt_for(h, &screen);
                    all.iter()
                        .filter(|c| p.as_ref().and_then(|p| p.index_for(c.1)).is_some())
                        .map(|c| c.0)
                        .collect()
                }
            };
            v.push(json!({
                "id": format!("tab:{}", tab.uid), "kind": "tab", "tab": tab.uid,
                "name": tab.name(), "question": q, "choices": choices,
            }));
        }
        v
    }

    pub fn web_state(&self) -> Value {
        let s = self.state_json();
        let pick = |k: &str| s.get(k).cloned().unwrap_or(Value::Null);
        json!({
            "modes": pick("modes"),
            "mode_flags": {
                "memory_saver": self.memory_saver_on(),
                "privacy": self.privacy(),
                "emails_shown": self.show_email_global(),
                "direct": self.direct.on.as_ref().map(|d| d.name.clone()),
                "brainstorm": self.brainstorm.on.is_some(),
                "updates": self.updates_on(),
            },
            "accounts": self.web_account_models(pick("accounts")),
            "tabs": self.web_tabs(&s),
            "focused": self.panes.get(self.focus).map(|p| p.cur().uid),

            "moved": self
                .moved_ids
                .iter()
                .map(|(o, n)| (o.to_string(), json!(n)))
                .collect::<serde_json::Map<String, Value>>(),
            "loops": pick("loops"),
            "jobs": pick("jobs"),
            "archived": pick("archived"),
            "closed": pick("closed"),
            "remote_control": pick("remote_control"),
            "listening_paused": pick("listening_paused"),
            "approvals": self.web_approvals(),
            "assistant": {
                "on": self.assistant_on(),
                "busy": self.assistant.busy,
                "provider": self.cfg.assistant.provider,
                "account": pick("assistant_account"),
            },
            "voice": {
                "mode": self.voice_mode_now(),
                "muted": self.voice.muted,
                "heard": self.voice.heard,
                "action": self.voice.action,
                "web": self.web_voice_client(),
                "web_muted": self.web_mic_is_muted(),

                "speaking": self.voice.engine.as_ref().is_some_and(|v| v.speaking()),
                "awake": self.voice.wake_until.is_some_and(|t| t > std::time::Instant::now()),
                "wake_word": self.cfg.voice.wake_words.first(),
                "barge_in": self.cfg.voice.barge_in,
                "typed_spoken": self.cfg.assistant.speak_typed,
                "status": match &self.voice.status {
                    crate::voice::VoiceStatus::Starting => "starting",
                    crate::voice::VoiceStatus::Idle => "idle",
                    crate::voice::VoiceStatus::Listening => "listening",
                    crate::voice::VoiceStatus::Transcribing => "transcribing",
                    crate::voice::VoiceStatus::Off(_) => "off",
                },
                "error": match &self.voice.status {
                    crate::voice::VoiceStatus::Off(Some(e)) => Some(e.clone()),
                    _ => None,
                },
            },
        })
    }

    fn web_update(&self) -> Value {
        let u = self.update_info_json();
        let first = &u["versions"][0];
        json!({
            "t": "update",
            "available": u["newer"].as_bool().unwrap_or(false),
            "version": u["latest"],
            "current": u["current"],
            "state": u["state"],
            "notes": first["notes"],
            "page": first["page"],
            "update_with": u["update_with"],
        })
    }

    pub fn web_event(&mut self, ev: ToApp) {
        match ev {
            ToApp::Hello(id) => self.web_hello(id),
            ToApp::Say {
                client,
                text,
                images,
            } => {
                crate::log::info(&format!(
                    "web: a turn typed in browser {client} ({} images)",
                    images.len()
                ));

                self.assistant.pending_web = Some(client);
                self.web_say(&text, &images);
                self.assistant.pending_web = None;
            }
            ToApp::Answer { client, id, choice } => {
                let r = self.web_answer(&id, &choice);
                if let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) {
                    let frame = match r {
                        Ok(msg) => json!({"t": "answered", "id": id, "ok": true, "result": msg}),
                        Err(e) => json!({"t": "answered", "id": id, "ok": false, "error": e}),
                    };
                    c.out.text(frame.to_string());
                }
            }
            ToApp::Typed(uid) => {
                if let Some((s, t)) = self.find_tab(uid) {
                    let tab = &mut self.panes[s].tabs[t];
                    tab.reset_scroll();
                    tab.last_input = Instant::now();
                }
            }
            ToApp::Resize {
                client,
                uid,
                size,
                by,
            } => {
                let Some((s, t)) = self.find_tab(uid) else {
                    return;
                };
                if let Some(w) = self.web.as_mut() {
                    w.left_sizes.retain(|(u, _)| *u != uid);
                    match size {
                        Some(_) => {
                            w.taken.insert(uid, client);
                            match &by {
                                Some(b) => {
                                    w.taken_by.insert(uid, b.clone());
                                }
                                None => {
                                    w.taken_by.remove(&uid);
                                }
                            }
                        }
                        None => {
                            w.taken.remove(&uid);
                            w.taken_by.remove(&uid);
                        }
                    }
                }
                let area = self.panes[s].size;
                let tab = &mut self.panes[s].tabs[t];
                tab.web_size = size;
                tab.web_sizer = size.map(|_| by.unwrap_or_else(|| "a browser".into()));
                tab.resize(area.0, area.1);
                if let Some(w) = self.web.as_mut() {
                    w.changed = true;
                }
            }
            ToApp::Voice {
                client,
                on,
                mode,
                talk,
            } => self.web_voice(client, on, &mode, talk),
            ToApp::VoiceCtl { client, v } => self.web_voice_ctl(client, &v),
            ToApp::Git { client, v } => self.web_git(client, v),
            ToApp::Accounts { client, v } => self.web_accounts(client, v),
            ToApp::BrowserView { client, v } => self.web_bview(client, v),
            ToApp::Gone(client) => {
                crate::browser_view::unwatch(client, None);
                if self.web_voice_client() == Some(client) {
                    self.web_voice_off("the browser left");
                }
                let Some(w) = self.web.as_mut() else {
                    return;
                };
                let mine: Vec<u64> = w
                    .taken
                    .iter()
                    .filter(|(_, c)| **c == client)
                    .map(|(u, _)| *u)
                    .collect();
                let due = Instant::now() + SIZE_GRACE;
                for u in mine {
                    w.taken.remove(&u);
                    w.taken_by.remove(&u);
                    w.left_sizes.push((u, due));
                }
                w.changed = true;
            }
        }
    }

    fn web_say(&mut self, text: &str, images: &[Vec<u8>]) {
        let mut text = text.to_string();
        let mut atts = vec![];
        for b in images {
            match crate::assistant_images::save_bytes(b) {
                Ok((path, w, h)) => {
                    self.assistant.image_seq += 1;
                    let a = crate::assistant_images::Attachment {
                        id: self.assistant.image_seq,
                        path,
                        w,
                        h,
                        at: Instant::now(),
                    };
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(&a.chip());
                    crate::log::info(&format!("web: attached {}", a.label()));
                    atts.push(a);
                }
                Err(e) => self.flash(format!("A pasted image could not be attached: {e:#}")),
            }
        }
        if text.trim().is_empty() {
            return;
        }
        self.assistant.next_images = atts;
        self.ask_assistant(&text);

        self.assistant.next_images.clear();
    }

    pub fn web_answer(&mut self, id: &str, choice: &str) -> Result<String, String> {
        let c = choice.trim().to_lowercase();
        let yes = matches!(c.as_str(), "yes" | "approve" | "always");
        let no = matches!(c.as_str(), "no" | "deny");
        if !yes && !no {
            return Err("choice must be yes, always or no".into());
        }
        if let Some(uid) = id.strip_prefix("tab:").and_then(|u| u.parse::<u64>().ok()) {
            let (s, t) = self.find_tab(uid).ok_or("that tab is gone")?;
            let pick = match c.as_str() {
                "always" => crate::prompt::Choice::Always,
                _ if yes => crate::prompt::Choice::Approve,
                _ => crate::prompt::Choice::Deny,
            };

            self.tab_answerable(s, t, pick)?;
            crate::log::info(&format!("web: the user answered t{uid}'s prompt: {c}"));
            return Ok(self.answer_tab(s, t, pick));
        }

        if id == "switch" || id.starts_with("confirm:") {
            if let Some(tok) = id.strip_prefix("confirm:") {
                if !self.pending_confirms.iter().any(|p| p.token == tok) {
                    return Err("that question is no longer open".into());
                }
            } else if self.assistant.selfswitch.asked.is_none() {
                return Err("that question is no longer open".into());
            }
            crate::log::info(&format!("web: the user answered {id}: {c}"));
            if self.pending_confirms.is_empty() && self.answer_self_switch(yes) {
                return Ok("answered".into());
            }
            self.assistant.input = if yes { "yes" } else { "no" }.into();
            self.send_assistant_input();
            return Ok("answered".into());
        }
        Err("unknown approval".into())
    }

    fn web_hello(&mut self, id: u64) {
        let Some(w) = self.web.as_ref() else {
            return;
        };
        let Some(c) = w.hub.client(id) else {
            return;
        };
        let messages: Vec<Value> = w.log.iter().map(|(i, e, h)| message(*i, e, *h)).collect();
        let hello = json!({
            "t": "hello",
            "proto": super::PROTO,
            "version": env!("CARGO_PKG_VERSION"),
            "build": w.hub.assets.build(),
            "client": id,
            "theme": theme_json(),
            "state": self.web_state(),
            "messages": messages,
            "live": (!self.assistant.current.is_empty()).then(|| self.assistant.current.clone()),
            "live_at": (!self.assistant.current.is_empty()).then(|| w.live_at),
            "update": self.web_update(),
        });

        c.out.text(hello.to_string());
        c.ready.store(true, Ordering::Relaxed);
    }

    pub fn web_due(&self) -> Option<Duration> {
        let w = self.web.as_ref()?;
        if !w.changed {
            return None;
        }
        Some(PUSH_EVERY.saturating_sub(w.last_push.elapsed()))
    }

    pub fn web_pump(&mut self) {
        let Some(w) = self.web.as_mut() else {
            return;
        };

        if !w.left_sizes.is_empty() {
            let now = Instant::now();
            let (gone, keep): (Vec<_>, Vec<_>) =
                w.left_sizes.drain(..).partition(|(_, at)| *at <= now);
            w.left_sizes = keep;
            let back: Vec<u64> = gone
                .into_iter()
                .map(|(u, _)| u)
                .filter(|u| !w.taken.contains_key(u))
                .collect();
            for u in back {
                self.web_size_back(u);
            }
        }
        let Some(w) = self.web.as_mut() else {
            return;
        };
        if !w.changed || w.last_push.elapsed() < PUSH_EVERY {
            return;
        }
        w.changed = false;
        w.last_push = Instant::now();

        self.web_sync_log();

        self.web_push_tick();
        let Some(w) = self.web.as_ref() else {
            return;
        };
        if !w.hub.any_ready() {
            return;
        }
        let state = json!({"t": "state", "state": self.web_state()}).to_string();
        let approvals = self.web_approvals();
        let update = self.web_update().to_string();
        let flash = self.flash.as_ref().map(|f| f.0.clone());
        let Some(w) = self.web.as_mut() else {
            return;
        };
        if state != w.state {
            w.hub.broadcast(&state);
            w.state = state;
        }

        let ids: Vec<String> = approvals
            .iter()
            .filter_map(|a| a["id"].as_str().map(str::to_string))
            .collect();
        for a in &approvals {
            let id = a["id"].as_str().unwrap_or("").to_string();
            if !w.approvals.contains(&id) {
                let mut f = a.clone();
                f["t"] = json!("approval");
                w.hub.broadcast(&f.to_string());
            }
        }
        w.approvals = ids;
        if update != w.update {
            w.hub.broadcast(&update);
            w.update = update;
        }
        if flash != w.flash {
            if let Some(t) = &flash {
                w.hub
                    .broadcast(&json!({"t": "toast", "text": t, "level": "info"}).to_string());
            }
            w.flash = flash;
        }
    }

    fn web_sync_log(&mut self) {
        let log = &self.assistant.log;
        let live = self.assistant.current.clone();
        let Some(w) = self.web.as_mut() else {
            return;
        };
        let ready = w.hub.any_ready();
        let sent: Vec<&Entry> = w.log.iter().map(|(_, e, _)| e).collect();
        let same = sent.len() == log.len() && sent.iter().zip(log.iter()).all(|(a, b)| *a == b);
        if !same {
            let start = if sent.is_empty() {
                Some(0)
            } else {
                (0..sent.len()).find(|&d| {
                    let rest = &sent[d..];
                    rest.len() <= log.len() && rest.iter().zip(log.iter()).all(|(a, b)| *a == b)
                })
            };
            match start {
                Some(d) => {
                    let kept = sent.len() - d;
                    w.log.drain(..d);
                    for e in &log[kept..] {
                        let id = w.next_id;
                        w.next_id += 1;
                        let h = hidden(e, &mut w.in_system);
                        if ready {
                            w.hub.broadcast(&message(id, e, h).to_string());
                        }
                        w.log.push((id, e.clone(), h));
                    }
                }
                None => {
                    w.log.clear();
                    w.in_system = false;
                    for e in log {
                        let id = w.next_id;
                        w.next_id += 1;
                        let h = hidden(e, &mut w.in_system);
                        w.log.push((id, e.clone(), h));
                    }
                    if ready {
                        let msgs: Vec<Value> =
                            w.log.iter().map(|(i, e, h)| message(*i, e, *h)).collect();
                        w.hub.broadcast(
                            &json!({"t": "assistant_reset", "messages": msgs}).to_string(),
                        );
                    }
                }
            }
        }
        if live != w.live {
            if w.live.is_empty() && !live.is_empty() {
                w.live_at = crate::app_assistant::now_ms();
            }
            if ready {
                w.hub.broadcast(
                    &json!({"t": "assistant", "id": "live", "role": "assistant", "text": live, "done": live.is_empty(), "at": w.live_at})
                        .to_string(),
                );
            }
            w.live = live;
        }
    }
}
