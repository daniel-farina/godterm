use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;

pub const RING: usize = 50;

const PER_TURN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Spoken,
    Flash,
    Notification,
    Panel,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Channel::Spoken => "spoken",
            Channel::Flash => "flash",
            Channel::Notification => "notification",
            Channel::Panel => "panel",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Source {
    pub tab: Option<u64>,

    pub account: Option<usize>,

    pub fact: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Told {
    pub seq: u64,
    pub at: chrono::DateTime<chrono::Local>,
    pub when: Instant,
    pub kind: String,
    pub text: String,
    pub channels: Vec<Channel>,
    pub src: Source,

    pub low: bool,
}

#[derive(Debug, Default)]
pub struct ToldLog {
    pub ring: VecDeque<Told>,
    seq: u64,

    pub seen: u64,

    pending_seen: Option<u64>,

    pub ctx: Option<(String, Source)>,

    #[cfg(test)]
    pub fake_speaker: bool,
}

const MERGE: Duration = Duration::from_secs(600);

impl ToldLog {
    pub fn record(&mut self, kind: &str, text: &str, ch: Channel, low: bool) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let (kind, src) = match &self.ctx {
            Some((k, s)) => (k.clone(), s.clone()),
            None => (kind.to_string(), Source::default()),
        };
        let seen = self.seen;
        if let Some(e) = self
            .ring
            .iter_mut()
            .rev()
            .take_while(|e| e.seq > seen && e.when.elapsed() < MERGE)
            .find(|e| e.text == text)
        {
            if !e.channels.contains(&ch) {
                e.channels.push(ch);
            }
            e.low &= low;
            if e.src == Source::default() {
                e.src = src;
                e.kind = kind;
            }
            return;
        }
        self.seq += 1;
        self.ring.push_back(Told {
            seq: self.seq,
            at: chrono::Local::now(),
            when: Instant::now(),
            kind,
            text: text.to_string(),
            channels: vec![ch],
            src,
            low,
        });
        while self.ring.len() > RING {
            self.ring.pop_front();
        }
    }

    pub fn unseen(&self) -> impl Iterator<Item = &Told> {
        let seen = self.seen;
        self.ring.iter().filter(move |e| e.seq > seen && !e.low)
    }
}

pub fn is_ui_hint(text: &str) -> bool {
    const STARTS: &[&str] = &[
        "Copied ",
        "Volume ",
        "Page ",
        "Layout: ",
        "Tab lists: ",
        "Tabs sorted",
        "Swapped panes",
        "Moved the pane",
        "Pane number",
        "Panes back to",
        "Everything fits",
        "Nothing selected",
        "Nothing to copy",
        "Opened ",
        "Terminal at ",
        "Preview",
        "Refreshing ",
        "Cancelled",
        "Mouse ",
        "flash ",
    ];
    STARTS.iter().any(|s| text.starts_with(s))
}

fn line(e: &Told) -> String {
    let chans: Vec<&str> = e.channels.iter().map(|c| c.label()).collect();
    let mut about = vec![];
    if let Some(t) = e.src.tab {
        about.push(crate::control::tab_id(t));
    }
    if let Some(a) = e.src.account {
        about.push(format!("a{}", a + 1));
    }
    let fact = e
        .src
        .fact
        .as_ref()
        .map(|f| format!(" fact: {f}"))
        .unwrap_or_default();
    format!(
        "  {} {} {}: \"{}\"{}{}\n",
        e.at.format("%H:%M:%S"),
        chans.join("+"),
        e.kind,
        crate::sessions::snippet(&e.text, 160),
        if about.is_empty() {
            String::new()
        } else {
            format!(" [{}]", about.join(" "))
        },
        fact
    )
}

impl App {
    fn told_log(&self) -> std::cell::RefMut<'_, ToldLog> {
        self.told.borrow_mut()
    }

    pub(crate) fn told_record(&self, kind: &str, text: &str, ch: Channel, low: bool) {
        self.told_log().record(kind, text, ch, low);
    }

    pub(crate) fn speaker_on(&self) -> bool {
        #[cfg(test)]
        if self.told.borrow().fake_speaker {
            return true;
        }
        self.voice.engine.is_some() || self.voice.preview.is_some()
    }

    pub(crate) fn will_speak_assistant(&self) -> bool {
        !self.cfg.voice.speaker_muted && (self.speaker_on() || (self.cfg.voice.tts && !cfg!(test)))
    }

    pub fn tell_user(&mut self, kind: &str, text: &str, channel: Channel) {
        self.tell_user_from(kind, text, &[channel], Source::default());
    }

    pub fn tell_user_from(&mut self, kind: &str, text: &str, channels: &[Channel], src: Source) {
        let prev = self.told_log().ctx.replace((kind.to_string(), src));
        for ch in channels {
            match ch {
                Channel::Flash => self.flash(text.to_string()),
                Channel::Notification => self.notify_user("godterm", text),
                Channel::Panel => self.note(text.to_string()),
                Channel::Spoken => self.say_update(crate::app_quiet::Voice::Announce, text),
            }
        }
        self.told_log().ctx = prev;
    }

    pub(crate) fn told_from<R>(
        &mut self,
        kind: &str,
        src: Source,
        f: impl FnOnce(&mut App) -> R,
    ) -> R {
        let prev = self.told_log().ctx.replace((kind.to_string(), src));
        let r = f(self);
        self.told_log().ctx = prev;
        r
    }

    pub fn notify_user(&self, title: &str, text: &str) {
        self.told_record("notification", text, Channel::Notification, false);
        crate::notify::post(title, text);
    }

    pub fn told_state_lines(&self) -> String {
        let (all, newest): (Vec<Told>, Option<u64>) = {
            let log = self.told.borrow();
            (
                log.unseen().cloned().collect(),
                log.ring.back().map(|e| e.seq),
            )
        };
        self.told_log().pending_seen = newest;
        if all.is_empty() {
            return String::new();
        }
        let skip = all.len().saturating_sub(PER_TURN);
        let mut s = String::from(
            "told_user (GodTerm itself said or showed these since your last turn, not you; never repeat them unasked; when asked, answer from the fact and this state):\n",
        );
        if skip > 0 {
            s.push_str(&format!("  ...{skip} earlier (get_state told_user)\n"));
        }
        for e in &all[skip..] {
            s.push_str(&line(e));
        }
        s
    }

    pub fn told_turn_sent(&mut self) {
        let mut log = self.told_log();
        if let Some(n) = log.pending_seen.take() {
            log.seen = log.seen.max(n);
        }
    }

    pub fn told_json(&self) -> Value {
        let log = self.told.borrow();
        let v: Vec<Value> = log
            .ring
            .iter()
            .filter(|e| !e.low)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .take(20)
            .rev()
            .map(|e| {
                json!({
                    "at": e.at.format("%H:%M:%S").to_string(),
                    "kind": e.kind,
                    "text": e.text,
                    "channels": e.channels.iter().map(|c| c.label()).collect::<Vec<_>>(),
                    "tab": e.src.tab.map(crate::control::tab_id),
                    "account": e.src.account.map(|a| a + 1),
                    "fact": e.src.fact,
                    "since_your_last_turn": e.seq > log.seen,
                })
            })
            .collect();
        json!(v)
    }
}

pub type Shared = RefCell<ToldLog>;
