use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_assistant::{Entry, Who};
use crate::app_provider::reset_word;
use crate::app_resume::Kept;
use crate::providers::{self, Bucket, Provider};

pub const RETRY_EVERY: Duration = Duration::from_secs(300);

const REMOTE_FOLLOW: Duration = Duration::from_secs(120);

pub const FAKE_ENV: &str = "GODTERM_FAKE_LEFT";

#[derive(Debug, Clone, Copy, PartialEq)]
enum SayHow {
    Now,

    Quiet,

    Urgent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub from: usize,
    pub to: usize,
    pub key: String,
    pub question: String,
}

#[derive(Debug, Default)]
pub struct SelfSwitch {
    pub warned: Option<String>,
    pub asked: Option<Ask>,

    pub declined: Option<String>,

    pub all_out: Option<String>,
    pub all_out_blocked: Option<String>,

    pub held: Option<(String, Option<String>, Option<String>)>,

    pub blocked: bool,

    pub retry_at: Option<Instant>,

    pub retry_every: Option<Duration>,

    pub remote_follow: Option<(Instant, Option<String>)>,

    pub fallback: Option<(u64, String)>,

    pub last: Option<String>,
    pub last_remote: Option<String>,
}

pub fn limit_said(text: &str) -> bool {
    let t = text.to_lowercase();
    crate::app_failover::limit_error(text)
        || [
            "rate_limit",
            "rate limit",
            "api error: 429",
            "status 429",
            "usage limit",
            "hit your limit",
            "you've hit your",
            "you have hit your",
            "session limit",
            "weekly limit",
            "5-hour limit",
        ]
        .iter()
        .any(|s| t.contains(s))
}

fn fake_left(name: &str) -> Option<(f64, String)> {
    let mut v = std::env::var(FAKE_ENV).ok()?;
    if let Some(p) = v.strip_prefix('@') {
        v = std::fs::read_to_string(p).ok()?;
    }
    v.split(',').find_map(|e| {
        let mut p = e.trim().split(':');
        (p.next()? == name).then_some(())?;
        let left = p.next()?.trim().parse().ok()?;
        let long = match p.next().map(str::trim) {
            Some("weekly") | Some("week") => "weekly",
            _ => "5 hour",
        };
        Some((left, long.to_string()))
    })
}

fn window_key(a: usize, k: &Bucket) -> String {
    format!(
        "{a}:{}:{}",
        k.long,
        k.resets_at.map(|t| t.timestamp() / 600).unwrap_or(0)
    )
}

fn window_words(k: &Bucket) -> String {
    match k.long.as_str() {
        "weekly" => "this week".into(),
        "5 hour" => "in its 5 hour window".into(),
        l => format!("on its {l} limit"),
    }
}

impl App {
    fn ss_every(&self) -> Duration {
        self.assistant.selfswitch.retry_every.unwrap_or(RETRY_EVERY)
    }

    pub fn self_binding(&self, a: usize) -> Option<Bucket> {
        let real = self
            .account_buckets(a)
            .and_then(|b| providers::binding(&b).cloned());
        match fake_left(&self.cfg.accounts.get(a)?.name) {
            Some((left, long)) => Some(Bucket {
                label: if long == "weekly" { "wk" } else { "5h" }.into(),
                long,
                left_pct: left,
                resets_at: real.and_then(|k| k.resets_at),
                binding: true,
            }),
            None => real,
        }
    }

    pub fn self_switch_target(&self, from: usize) -> Option<(usize, f64)> {
        use crate::usage::UsageError;
        let h = self.cfg.accounts.get(from)?.harness();
        let min = self.cfg.assistant.switch_at_pct.max(2.0);
        let now = chrono::Utc::now().timestamp_millis();
        (0..self.cfg.accounts.len())
            .filter(|&a| a != from && self.cfg.accounts[a].harness() == h)
            .filter(|&a| {
                let st = &self.accounts[a];
                let token_ok = st.login.has_refresh || st.login.expires_at.is_none_or(|e| e >= now);
                st.login.logged_in()
                    && token_ok
                    && !matches!(
                        st.usage_err,
                        Some(UsageError::NotLoggedIn) | Some(UsageError::Unauthorized)
                    )
            })
            .filter_map(|a| {
                let l = self.self_binding(a)?.left_pct;
                (l > min).then_some((a, l))
            })
            .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    fn left_words(&self, a: usize, k: &Bucket) -> String {
        let resets = k
            .resets_at
            .map(|t| format!(" (resets {})", reset_word(t)))
            .unwrap_or_default();
        format!(
            "{}, has {:.0}% left {}{resets}",
            self.cfg.accounts[a].display(),
            k.left_pct.max(0.0).floor(),
            window_words(k)
        )
    }

    fn ss_say(&mut self, msg: &str, notify: bool, how: SayHow) {
        crate::log::info(&format!("assistant account: {msg}"));
        let src = crate::told::Source {
            account: self.brain_account(),
            fact: self.brain_account().and_then(|a| {
                self.self_binding(a).map(|k| {
                    format!(
                        "you run on a{}: {:.0}% left ({})",
                        a + 1,
                        k.left_pct.floor(),
                        k.long
                    )
                })
            }),
            tab: None,
        };
        self.told_from("assistant account", src, |app| {
            app.note(msg.to_string());
            app.flash(msg.to_string());
            match how {
                SayHow::Now => app.speak_as(crate::voice::tts::Kind::Announce, msg),
                SayHow::Quiet => app.say_update(crate::app_quiet::Voice::Announce, msg),
                SayHow::Urgent => app.say_urgent(crate::app_quiet::Voice::Announce, msg),
            }
            if notify && app.cfg.notifications {
                app.notify_user("GodTerm assistant", msg);
            }
        });
    }

    pub fn self_switch_tick(&mut self) {
        self.remote_follow_tick();
        let mode = self.cfg.assistant.auto_switch.clone();
        if mode == "off" || !self.assistant_on() {
            self.assistant.selfswitch.asked = None;
            return;
        }

        if providers::by_id(&self.cfg.assistant.provider)
            .harness
            .is_none()
        {
            return;
        }
        let Some(a) = self.brain_account() else {
            return;
        };
        let at = self.cfg.assistant.switch_at_pct;
        let warn = self.cfg.assistant.switch_warn_pct.max(at);
        let k = self.self_binding(a);
        let key = k
            .as_ref()
            .map(|k| window_key(a, k))
            .unwrap_or_else(|| format!("{a}:limit"));
        let blocked = self.assistant.selfswitch.blocked;
        let low = blocked || k.as_ref().is_some_and(|k| k.left_pct <= at);
        if !low {
            if self
                .assistant
                .selfswitch
                .asked
                .as_ref()
                .is_some_and(|q| q.from == a)
            {
                self.assistant.selfswitch.asked = None;
            }
            if let Some(k) = k.filter(|k| k.left_pct < warn) {
                if self.assistant.selfswitch.warned.as_deref() != Some(&key) {
                    self.assistant.selfswitch.warned = Some(key);
                    self.self_warn(a, &k, &mode);
                }
            }
            return;
        }

        if self.assistant.busy || self.assistant.stale > 0 {
            return;
        }
        match self.self_switch_target(a) {
            Some((to, _)) => {
                self.assistant.selfswitch.all_out = None;
                let why = match (&k, blocked) {
                    (_, true) => format!(
                        "My account, {}, hit its usage limit",
                        self.cfg.accounts[a].display()
                    ),
                    (Some(k), _) => format!("My account, {}", self.left_words(a, k)),
                    (None, _) => format!("My account, {}, is out", self.cfg.accounts[a].display()),
                };
                if mode == "auto" {
                    self.move_assistant(to, &why);
                } else if self.assistant.selfswitch.declined.as_deref() == Some(&key) {
                    self.retry_blocked();
                } else if self.assistant.selfswitch.asked.is_none() {
                    self.ask_self_switch(a, to, key, &why);
                }
            }
            None => {
                let ss = &mut self.assistant.selfswitch;
                let said = if blocked {
                    &mut ss.all_out_blocked
                } else {
                    &mut ss.all_out
                };
                if said.as_deref() != Some(&key) {
                    *said = Some(key.clone());

                    ss.all_out = Some(key);
                    self.say_all_out(a, k.as_ref());
                }
                self.retry_blocked();
            }
        }
    }

    fn self_warn(&mut self, a: usize, k: &Bucket, mode: &str) {
        let at = self.cfg.assistant.switch_at_pct;
        let plan = match self.self_switch_target(a) {
            Some((to, l)) => format!(
                "; I'll {} to {} ({:.0}% left) at {at:.0}%",
                if mode == "ask" {
                    "offer to move"
                } else {
                    "move"
                },
                self.cfg.accounts[to].display(),
                l.floor()
            ),
            None => "; no other account has more room right now".into(),
        };
        let msg = format!("My account, {}{plan}.", self.left_words(a, k));
        self.ss_say(&msg, false, SayHow::Quiet);
    }

    fn ask_self_switch(&mut self, from: usize, to: usize, key: String, why: &str) {
        let left = self
            .self_binding(to)
            .map(|k| format!(" ({:.0}% left)", k.left_pct.floor()))
            .unwrap_or_default();
        let question = format!(
            "{why}. Move me to {}{left}? Our conversation comes along.",
            self.cfg.accounts[to].display()
        );
        self.assistant.selfswitch.asked = Some(Ask {
            from,
            to,
            key,
            question: question.clone(),
        });
        self.ss_say(&question, true, SayHow::Quiet);
    }

    pub fn answer_self_switch(&mut self, yes: bool) -> bool {
        let Some(q) = self.assistant.selfswitch.asked.take() else {
            return false;
        };
        if yes {
            self.move_assistant(q.to, "You said yes");
        } else {
            self.assistant.selfswitch.declined = Some(q.key);
            let msg = format!("Staying on {}.", self.cfg.accounts[q.from].display());
            self.ss_say(&msg, false, SayHow::Now);
        }
        true
    }

    pub fn self_switch_heard(&mut self, text: &str) -> bool {
        if self.assistant.selfswitch.asked.is_none() {
            return false;
        }
        let t = text.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
        let yes = [
            "yes",
            "yeah",
            "yep",
            "sure",
            "ok",
            "okay",
            "do it",
            "yes please",
            "move",
            "move it",
            "go ahead",
        ];
        let no = ["no", "nope", "not now", "stay", "no thanks", "don't"];
        let answer = if yes.contains(&t.as_str()) {
            true
        } else if no.contains(&t.as_str()) {
            false
        } else {
            return false;
        };
        self.assistant
            .log
            .push(Entry::new(Who::User, text.trim().to_string()));
        self.answer_self_switch(answer)
    }

    fn say_all_out(&mut self, a: usize, k: Option<&Bucket>) {
        let now = self.cfg.accounts[a].display().to_string();
        let mut msg = match k {
            Some(k) if !self.assistant.selfswitch.blocked => {
                format!(
                    "My account, {}, and no other account has room.",
                    self.left_words(a, k)
                )
            }
            _ => format!("My account, {now}, hit its usage limit, and no other account has room."),
        };
        match self.soonest_room(a) {
            Some((b, t, long)) => msg.push_str(&format!(
                " The soonest reset is {}'s {long} limit at {}; I'll {} then.",
                self.cfg.accounts[b].display(),
                reset_word(t),
                if b == a {
                    "carry on here".to_string()
                } else {
                    format!("move to {}", self.cfg.accounts[b].display())
                }
            )),
            None => msg.push_str(" I'll keep trying every few minutes."),
        }

        let how = if self.assistant.selfswitch.blocked {
            SayHow::Urgent
        } else {
            msg.push_str(&format!(" I keep running on {now} until it stops me."));
            SayHow::Quiet
        };
        self.ss_say(&msg, true, how);
    }

    fn soonest_room(&self, a: usize) -> Option<(usize, chrono::DateTime<chrono::Utc>, String)> {
        let h = self.cfg.accounts[a].harness();
        let at = self.cfg.assistant.switch_at_pct.max(2.0);
        (0..self.cfg.accounts.len())
            .filter(|&b| self.cfg.accounts[b].harness() == h && self.accounts[b].login.logged_in())
            .filter_map(|b| {
                let bs = self.account_buckets(b)?;

                let out = bs
                    .iter()
                    .filter(|k| k.left_pct <= at)
                    .filter_map(|k| k.resets_at.map(|t| (t, k.long.clone())))
                    .max_by_key(|(t, _)| *t)
                    .or_else(|| {
                        self.self_binding(b)
                            .filter(|k| k.left_pct <= at)
                            .and_then(|k| k.resets_at.map(|t| (t, k.long)))
                    })?;
                Some((b, out.0, out.1))
            })
            .min_by_key(|x| x.1)
    }

    fn retry_blocked(&mut self) {
        let ss = &mut self.assistant.selfswitch;
        if !ss.blocked {
            return;
        }
        if ss.retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        ss.blocked = false;
        ss.retry_at = None;
        let held = ss.held.take();
        crate::log::info("assistant account: trying the current account again");
        if let Some((text, heard, system)) = held {
            self.resend(&text, heard.as_deref(), system.as_deref());
        }
    }

    fn resend(&mut self, text: &str, heard: Option<&str>, system: Option<&str>) {
        let n = self.assistant.log.len();
        self.start_turn(text, heard, system);

        if self.assistant.log.len() > n && self.assistant.log[n].who == Who::User {
            self.assistant.log.remove(n);
        }
    }

    pub fn self_switch_on_limit(&mut self, text: &str) -> bool {
        if self.cfg.assistant.auto_switch == "off" || !limit_said(text) {
            return false;
        }
        if providers::by_id(&self.cfg.assistant.provider)
            .harness
            .is_none()
        {
            return false;
        }
        crate::log::info(&format!(
            "assistant account: a turn hit a limit: {}",
            crate::sessions::snippet(text, 200)
        ));
        let turn = if self.assistant.turn_remote {
            Some((self.assistant.last_user.clone(), None, None))
        } else {
            self.assistant.last_turn.clone()
        };
        self.assistant.busy = false;
        self.assistant.system_turn = false;
        self.assistant.turn_remote = false;
        let every = self.ss_every();
        let reset = self
            .brain_account()
            .and_then(|a| self.self_binding(a))
            .and_then(|k| k.resets_at)
            .and_then(|t| (t - chrono::Utc::now()).to_std().ok());
        let ss = &mut self.assistant.selfswitch;
        ss.blocked = true;
        ss.held = turn;
        ss.retry_at = Some(Instant::now() + reset.map_or(every, |r| r.min(every)));
        self.self_switch_tick();
        true
    }

    fn fuller_carry(&self) -> Option<String> {
        let lines: Vec<String> = match &self.assistant.conv {
            Some(c) => crate::assistant_history::read(&c.path)
                .iter()
                .filter_map(|e| {
                    let t = e["text"].as_str()?;
                    match e["kind"].as_str() {
                        Some("user") => Some(format!("user: {t}")),
                        Some("reply") => Some(format!("you: {t}")),
                        _ => None,
                    }
                })
                .collect(),
            None => self
                .assistant
                .log
                .iter()
                .filter(|e| matches!(e.who, Who::User | Who::Reply))
                .map(|e| {
                    let who = if e.who == Who::User { "user" } else { "you" };
                    format!("{who}: {}", e.text)
                })
                .collect(),
        };
        let mut out = String::new();
        for l in lines.iter().rev() {
            let l: String = l.chars().take(600).collect();
            if out.len() + l.len() > 4000 {
                break;
            }
            out = if out.is_empty() {
                l
            } else {
                format!("{l} / {out}")
            };
        }
        (!out.is_empty()).then_some(out)
    }

    pub(crate) fn take_switch_fallback(&mut self) -> Option<String> {
        let gen = self.assistant.resumed?;
        match self.assistant.selfswitch.fallback.take() {
            Some((g, s)) if g == gen => Some(s),
            _ => None,
        }
    }

    pub fn move_assistant(&mut self, to: usize, why: &str) -> String {
        let from = self.brain_account();

        let how = if why.starts_with("You") {
            SayHow::Now
        } else if self.assistant.selfswitch.blocked {
            SayHow::Urgent
        } else {
            SayHow::Quiet
        };
        let to_name = self.cfg.accounts[to].display().to_string();
        let left = self
            .self_binding(to)
            .map(|k| format!(" ({:.0}% left)", k.left_pct.floor()))
            .unwrap_or_default();

        let rc = self.assistant.remote.take();
        let flow = self.assistant.remote_flow.take();
        if let Some(tx) = flow.as_ref().and_then(|f| f.reply.as_ref()) {
            let _ = tx.send(json!({"error": "the assistant moved to another account while Remote Control was turning on; it turns on again there", "pending": true}));
        }
        self.assistant.remote_pending = None;
        let follow = rc.is_some() || flow.is_some() || self.remote_kept_on();
        let old_url = rc
            .as_ref()
            .and_then(|r| r.url.clone())
            .or_else(|| self.assistant.remote_last_url.clone());
        let fuller = self.fuller_carry();
        if !self.cfg.assistant.resume {
            self.assistant.carry = fuller.clone();
        }

        self.assistant.brain = None;
        if self.cfg.assistant.account != "best" {
            let name = self.cfg.accounts[to].name.clone();
            let _ = crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global("assistant.account"),
                Some(toml_edit::value(name.clone())),
            );
            self.cfg.assistant.account = name;
            self.config_mtime = crate::app::config_mtime();
        }
        self.assistant.account_after = None;
        let ss = &mut self.assistant.selfswitch;
        ss.blocked = false;
        ss.retry_at = None;
        ss.asked = None;
        ss.warned = None;
        ss.all_out = None;
        ss.all_out_blocked = None;
        ss.declined = None;
        let held = ss.held.take();
        crate::log::info(&format!(
            "assistant account: moving from {} to {}",
            from.map(|a| self.cfg.accounts[a].name.clone())
                .unwrap_or_default(),
            self.cfg.accounts[to].name
        ));
        let mut msg = format!(
            "{why}, so I moved myself to {to_name}{left}; our conversation continues there."
        );
        let started = if self.cfg.assistant.account == "best" {
            self.ensure_brain_on(to)
        } else {
            self.ensure_brain()
        };
        match started {
            Ok(()) => {
                let gen = self.assistant.brain.as_ref().map(|b| b.gen).unwrap_or(0);
                self.assistant.selfswitch.fallback = fuller.map(|f| (gen, f));
            }
            Err(e) => {
                msg = format!("{why}. I tried to move to {to_name}, but it did not start: {e}.");
            }
        }
        if follow && self.remote_supported().is_ok() {
            self.assistant.selfswitch.remote_follow = Some((Instant::now(), old_url));
            self.assistant.remote_retry_at = None;
            if !self.remote_kept_on() {
                let name = self.remote_default_name();
                if let Err(e) = self.remote_start(&name, true, None) {
                    crate::log::info(&format!("assistant account: remote control: {e}"));
                }
            }
            msg.push_str(" Remote Control is coming back there; its link may change.");
        }
        self.assistant.selfswitch.last = Some(msg.clone());
        self.ss_say(&msg, true, how);
        if let Some((text, heard, system)) = held {
            if self.assistant.brain.is_some() {
                self.resend(&text, heard.as_deref(), system.as_deref());
            }
        }
        msg
    }

    fn ensure_brain_on(&mut self, to: usize) -> Result<(), String> {
        let keep = std::mem::replace(
            &mut self.cfg.assistant.account,
            self.cfg.accounts[to].name.clone(),
        );
        let r = self.ensure_brain();
        self.cfg.assistant.account = keep;
        r
    }

    fn remote_follow_tick(&mut self) {
        let Some((since, old)) = self.assistant.selfswitch.remote_follow.clone() else {
            return;
        };
        let gen = self.assistant.brain.as_ref().map(|b| b.gen);
        let up = self
            .assistant
            .remote
            .as_ref()
            .filter(|r| Some(r.gen) == gen && r.connected())
            .cloned();
        if let Some(r) = up {
            self.assistant.selfswitch.remote_follow = None;
            let acct = r
                .account
                .map(|a| self.cfg.accounts[a].display().to_string())
                .unwrap_or_default();
            let link = r.url.clone().unwrap_or_else(|| "no link given".into());
            let changed = r.url.is_some() && r.url != old;
            let msg = format!(
                "Remote Control moved to {acct}: {link}{}",
                if changed { " (a new link)" } else { "" }
            );
            self.assistant.selfswitch.last_remote = Some(msg.clone());
            self.ss_say(&msg, true, SayHow::Quiet);
        } else if since.elapsed() > REMOTE_FOLLOW && self.assistant.remote_flow.is_none() {
            self.assistant.selfswitch.remote_follow = None;
            crate::log::info("assistant account: Remote Control did not come back after the move");
        }
    }

    pub(crate) fn adopt_kept_session(
        &self,
        k: Kept,
        provider: &Provider,
        acct: &str,
        home: &Path,
    ) -> Kept {
        if k.id.is_empty()
            || k.provider != provider.id
            || k.account == acct
            || acct.is_empty()
            || provider.id != "claude"
        {
            return k;
        }
        let Some(from) = self.cfg.accounts.iter().position(|a| a.name == k.account) else {
            return k;
        };
        let sdir = self.cfg.accounts[from].config_dir();
        if sdir == home {
            return k;
        }
        let Some(jsonl) = crate::app_resume::session_path(provider, &sdir, &k.id) else {
            return k;
        };

        match crate::session_ops::copy_session(
            &sdir,
            &jsonl,
            &k.id,
            home,
            crate::session_ops::Conflict::Overwrite,
        ) {
            Ok(c) => {
                crate::log::info(&format!(
                    "assistant: its session {} copied from {} to {acct} ({} KB)",
                    c.id,
                    k.account,
                    c.bytes / 1024
                ));
                let nk = Kept {
                    id: c.id,
                    account: acct.to_string(),
                    ..k
                };
                crate::app_resume::save(&nk);
                nk
            }
            Err(e) => {
                crate::log::info(&format!("assistant: could not copy its session: {e:#}"));
                k
            }
        }
    }

    pub fn self_switch_json(&self) -> Value {
        let p = providers::by_id(&self.cfg.assistant.provider);
        let a = self.brain_account().filter(|_| p.harness.is_some());
        let k = a.and_then(|a| self.self_binding(a));
        let ss = &self.assistant.selfswitch;
        json!({
            "account": a.map(|a| a + 1),
            "label": a.map(|a| self.cfg.accounts[a].display().to_string()),
            "left_pct": k.as_ref().map(|k| k.left_pct.floor()),
            "limited_by": k.as_ref().map(|k| k.long.clone()),
            "resets": k.as_ref().and_then(|k| k.resets_at).map(reset_word),
            "auto_switch": self.cfg.assistant.auto_switch,
            "warn_pct": self.cfg.assistant.switch_warn_pct,
            "switch_at_pct": self.cfg.assistant.switch_at_pct,
            "move_to": a.and_then(|a| self.self_switch_target(a)).map(|(t, l)| json!({"account": t + 1, "left_pct": l.floor()})),
            "blocked_by_limit": ss.blocked,
            "asking": ss.asked.as_ref().map(|q| q.question.clone()),
            "last_move": ss.last,
            "remote_control": ss.last_remote,
        })
    }

    pub fn self_left_words(&self) -> String {
        let Some(a) = self.brain_account() else {
            return String::new();
        };
        match self.self_binding(a) {
            Some(k) => format!(", {:.0}% left ({} binds)", k.left_pct.floor(), k.long),
            None => String::new(),
        }
    }
}
