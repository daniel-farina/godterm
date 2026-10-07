//! Follow-through for questions the assistant asks a tab's agent: the
//! question is watched, the tab's answer is taken from its transcript
//! once its turn is over, and a turn of our own has the brain tell the
//! user in a sentence or two. Answers wait while the user is talking or
//! listening is paused, several are told together, and a question nobody
//! answers is dropped after a while (Settings > Assistant).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_assistant::{Entry, Who};
use crate::pane::Activity;

#[derive(Debug, Clone, PartialEq)]
pub enum FuState {
    Waiting,
    /// The answer is in; it is told once the user can hear it.
    Answered {
        text: String,
        at: chrono::DateTime<chrono::Local>,
    },
    Reported {
        text: String,
        at: chrono::DateTime<chrono::Local>,
    },
    TimedOut,
    Cancelled,
    /// The tab was closed before it answered.
    Gone,
}

#[derive(Debug, Clone)]
pub struct FollowUp {
    pub id: u64,
    pub uid: u64,
    pub tab: String,
    /// The prompt sent to the tab.
    pub question: String,
    /// What the user said that turn.
    pub asked: String,
    pub sent: Instant,
    pub sent_at: chrono::DateTime<chrono::Local>,
    /// The tab was seen working on it.
    pub seen_working: bool,
    pub checked: Option<Instant>,
    pub state: FuState,
    /// Mentioned in the "While you were away" summary already.
    pub held: bool,
    /// When it left Waiting (finished entries are kept a while for the
    /// snapshot).
    pub ended: Option<Instant>,
}

/// Finished entries stay in the snapshot this long.
const KEEP_DONE: Duration = Duration::from_secs(3600);

/// Whether what the user said asks for information (so a prompt sent for
/// it is a question to follow up on).
pub fn asks_something(user: &str) -> bool {
    let c = user.to_lowercase();
    if c.contains('?') {
        return true;
    }
    let c: String = c
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '\'' {
                ch
            } else {
                ' '
            }
        })
        .collect();
    let c = format!(" {} ", c.split_whitespace().collect::<Vec<_>>().join(" "));
    [
        " ask ",
        " ask it",
        " check with ",
        " follow up ",
        " find out ",
        " what's the latest",
        " whats the latest",
        " what is the latest",
        " the status ",
        " any update",
        " did it ",
        " did he ",
        " did she ",
        " did they ",
        " has it ",
        " is it done",
        " is he still",
        " is she still",
        " is it still",
    ]
    .iter()
    .any(|k| c.contains(k))
}

/// A short key of a prompt, to find it again in a transcript (pasting
/// can change whitespace).
fn key_of(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(60)
        .collect()
}

fn trimmed(text: &str, max: usize) -> String {
    crate::sessions::first_sentences(text.trim(), max)
}

impl App {
    /// Watch for the answer to `question`, just sent to tab `uid`.
    pub fn register_follow_up(&mut self, uid: u64, question: &str) {
        let tab = self
            .find_tab(uid)
            .map(|(s, t)| self.panes[s].tabs[t].name())
            .unwrap_or_else(|| format!("t{uid}"));
        // A newer question to the same tab replaces the waiting one.
        for f in &mut self.assistant.follow_ups {
            if f.uid == uid && f.state == FuState::Waiting {
                f.state = FuState::Cancelled;
                f.ended = Some(Instant::now());
            }
        }
        self.assistant.follow_seq += 1;
        crate::log::info(&format!(
            "follow-up: watching t{uid} ({tab}) for the answer to: {}",
            question.chars().take(120).collect::<String>()
        ));
        self.assistant.follow_ups.push(FollowUp {
            id: self.assistant.follow_seq,
            uid,
            tab,
            question: question.to_string(),
            asked: self.assistant.last_user.clone(),
            sent: Instant::now(),
            sent_at: chrono::Local::now(),
            seen_working: false,
            checked: None,
            state: FuState::Waiting,
            held: false,
            ended: None,
        });
    }

    /// After a brain send_prompt: follow up on the deliveries that ask.
    pub fn follow_up_sends(&mut self, args: &Value, v: &Value) {
        let explicit = args.get("expect_reply").and_then(Value::as_bool);
        let ids: Vec<u64> = v["_wait"]["deliveries"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        for id in ids {
            let Some((uid, text)) = self
                .deliveries
                .iter()
                .find(|d| d.id == id)
                .map(|d| (d.uid, d.text.clone()))
            else {
                continue;
            };
            let asks = explicit.unwrap_or_else(|| {
                text.trim_end().ends_with('?') || asks_something(&self.assistant.last_user)
            });
            if asks {
                self.register_follow_up(uid, &text);
            }
        }
    }

    /// Stop waiting (all, or one tab's). Returns how many were dropped.
    pub fn cancel_follow_ups(&mut self, uid: Option<u64>) -> usize {
        let mut n = 0;
        for f in &mut self.assistant.follow_ups {
            if uid.is_none_or(|u| u == f.uid)
                && matches!(f.state, FuState::Waiting | FuState::Answered { .. })
            {
                f.state = FuState::Cancelled;
                f.ended = Some(Instant::now());
                n += 1;
            }
        }
        if n > 0 {
            crate::log::info(&format!("follow-up: cancelled {n}"));
        }
        n
    }

    /// "never mind" while waiting for a tab's answer: stop waiting.
    pub fn cancel_follow_ups_said(&mut self, text: &str) -> bool {
        let c: String = text
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '\'')
            .collect();
        let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
        let phrases = [
            "never mind",
            "nevermind",
            "never mind that",
            "forget it",
            "forget about it",
            "stop waiting",
            "don't wait",
            "dont wait",
            "cancel that",
            "never mind the question",
        ];
        let pending = self
            .assistant
            .follow_ups
            .iter()
            .any(|f| matches!(f.state, FuState::Waiting | FuState::Answered { .. }));
        if !pending || self.assistant.busy || !phrases.contains(&c.as_str()) {
            return false;
        }
        let tabs: Vec<String> = self
            .assistant
            .follow_ups
            .iter()
            .filter(|f| matches!(f.state, FuState::Waiting | FuState::Answered { .. }))
            .map(|f| f.tab.clone())
            .collect();
        self.cancel_follow_ups(None);
        let say = match tabs.as_slice() {
            [one] => format!("Okay, I won't wait for {one}'s answer."),
            _ => "Okay, I won't wait for those answers.".to_string(),
        };
        self.assistant.log.push(Entry {
            who: Who::User,
            text: text.to_string(),
        });
        self.assistant.log.push(Entry {
            who: Who::Reply,
            text: say.clone(),
        });
        self.assistant.unsaid = None;
        self.assistant.muted = false;
        self.assistant.spoken.push(say.clone());
        self.speak_assistant(&say);
        true
    }

    /// Every second or so: look for answers, drop what timed out, and
    /// tell the user what came in once they can hear it.
    pub fn follow_ups_tick(&mut self) {
        let wait = Duration::from_secs(self.cfg.assistant.answer_wait_min.max(1) * 60);
        let mut fus = std::mem::take(&mut self.assistant.follow_ups);
        for f in &mut fus {
            if f.state != FuState::Waiting {
                continue;
            }
            let uid = self.current_uid(f.uid);
            f.uid = uid;
            let Some((s, t)) = self.find_tab(uid) else {
                f.state = FuState::Gone;
                f.ended = Some(Instant::now());
                crate::log::info(&format!("follow-up: t{uid} closed before it answered"));
                continue;
            };
            if f.sent.elapsed() > wait {
                f.state = FuState::TimedOut;
                f.ended = Some(Instant::now());
                crate::log::info(&format!(
                    "follow-up: t{uid} did not answer in {} min; stopped waiting",
                    wait.as_secs() / 60
                ));
                continue;
            }
            match self.panes[s].tabs[t].activity {
                Activity::Working | Activity::Permission | Activity::Starting => {
                    f.seen_working = true;
                    continue;
                }
                Activity::Ready => {}
                Activity::Idle | Activity::Exited => continue,
            }
            // The prompt is still on its way, or the tab has not shown it
            // started (a ready screen right after the paste is not the end).
            if self.deliveries.iter().any(|d| d.uid == uid && !d.done())
                || (!f.seen_working && f.sent.elapsed() < Duration::from_secs(8))
            {
                continue;
            }
            if f.checked
                .is_some_and(|c| c.elapsed() < Duration::from_millis(1500))
            {
                continue;
            }
            f.checked = Some(Instant::now());
            let Some(path) = self.transcript_path(s, t) else {
                continue;
            };
            let key = key_of(&f.question);
            let turns = crate::sessions::recent_turns(&path, 6);
            let Some(pos) = turns
                .iter()
                .rposition(|x| key_of(&x.user).starts_with(&key))
            else {
                continue;
            };
            // Its turn is over: the tab is ready, and it wrote something.
            let done = turns[pos].finished;
            if let Some(text) = turns[pos]
                .reply
                .clone()
                .filter(|r| done && !r.trim().is_empty())
            {
                crate::log::info(&format!(
                    "follow-up: t{uid} answered ({} chars)",
                    text.len()
                ));
                f.state = FuState::Answered {
                    text,
                    at: chrono::Local::now(),
                };
            }
        }
        fus.retain(|f| f.ended.is_none_or(|e| e.elapsed() < KEEP_DONE));
        self.assistant.follow_ups = fus;
        self.report_answers();
    }

    /// Whether the user can take a report now.
    fn can_report(&self) -> bool {
        !self.assistant.busy
            && self.assistant.stale == 0
            && !self.paused()
            && self.voice.partial.is_none()
            && self.voice.holding.is_none()
    }

    /// Tell the user the answers that came in: one turn for all of them.
    fn report_answers(&mut self) {
        let answered: Vec<usize> = (0..self.assistant.follow_ups.len())
            .filter(|&i| matches!(self.assistant.follow_ups[i].state, FuState::Answered { .. }))
            .collect();
        if answered.is_empty() {
            return;
        }
        if self.paused() {
            // Named in the "While you were away" summary; told on resume.
            for &i in &answered {
                if !self.assistant.follow_ups[i].held {
                    self.assistant.follow_ups[i].held = true;
                    let msg = format!(
                        "{} answered your question",
                        self.assistant.follow_ups[i].tab
                    );
                    self.hold_announcement(&msg);
                }
            }
            return;
        }
        if !self.can_report()
            || self
                .assistant
                .report_retry
                .is_some_and(|t| Instant::now() < t)
        {
            return;
        }
        let mut parts = vec![];
        let mut names = vec![];
        for &i in &answered {
            let f = &self.assistant.follow_ups[i];
            let FuState::Answered { text, .. } = &f.state else {
                continue;
            };
            parts.push(format!(
                "(The agent in t{} ({}) answered your earlier question \"{}\":) {}",
                f.uid,
                f.tab,
                trimmed(&f.question, 300),
                trimmed(text, 1500)
            ));
            names.push(format!("t{} ({})", f.uid, f.tab));
        }
        let how = if parts.len() > 1 {
            "Summarize each for the user in one or two spoken sentences, then ask if they want the details. Call no tools."
        } else {
            "Summarize it for the user in one or two spoken sentences, then ask if they want the details. Call no tools."
        };
        let msg = format!("{}\n{how}", parts.join("\n"));
        let note = format!("({} answered)", names.join(", "));
        crate::log::info(&format!("follow-up: reporting {}", names.join(", ")));
        self.ask_assistant_system(&note, &msg);
        if !self.assistant.busy {
            // The assistant could not start (no account, no claude): the
            // answers stay and are told once it can.
            crate::log::info("follow-up: the assistant did not start; the report waits");
            self.assistant.report_retry = Some(Instant::now() + Duration::from_secs(30));
            return;
        }
        self.assistant.report_retry = None;
        for &i in &answered {
            let f = &mut self.assistant.follow_ups[i];
            if let FuState::Answered { text, at } = f.state.clone() {
                f.state = FuState::Reported { text, at };
                f.ended = Some(Instant::now());
            }
        }
    }

    /// The questions out to tabs, for the snapshot and get_state.
    pub fn pending_answers_json(&self) -> Value {
        json!(self
            .assistant
            .follow_ups
            .iter()
            .map(|f| {
                let (state, answer, at) = match &f.state {
                    FuState::Waiting => ("waiting", None, None),
                    FuState::Answered { text, at } => {
                        ("answered, not told yet", Some(text), Some(at))
                    }
                    FuState::Reported { text, at } => ("answered and told", Some(text), Some(at)),
                    FuState::TimedOut => ("no answer in time, stopped waiting", None, None),
                    FuState::Cancelled => ("cancelled", None, None),
                    FuState::Gone => ("tab closed", None, None),
                };
                json!({
                    "id": f.id,
                    "tab": format!("t{}", f.uid),
                    "user_asked": crate::sessions::snippet(&f.asked, 120),
                    "name": f.tab,
                    "question": trimmed(&f.question, 200),
                    "asked_at": f.sent_at.format("%H:%M").to_string(),
                    "state": state,
                    "answered_at": at.map(|t| t.format("%H:%M").to_string()),
                    "answer": answer.map(|a| trimmed(a, 400)),
                })
            })
            .collect::<Vec<_>>())
    }

    /// One line per question out to a tab, for the state block.
    pub fn pending_answers_line(&self) -> String {
        let items: Vec<String> = self
            .assistant
            .follow_ups
            .iter()
            .map(|f| {
                let state = match &f.state {
                    FuState::Waiting => format!("waiting {} min", f.sent.elapsed().as_secs() / 60),
                    FuState::Answered { at, .. } => format!("answered {}", at.format("%H:%M")),
                    FuState::Reported { at, .. } => {
                        format!("answered {} (told; read_tab has it)", at.format("%H:%M"))
                    }
                    FuState::TimedOut => "no answer in time".into(),
                    FuState::Cancelled => "cancelled".into(),
                    FuState::Gone => "tab closed".into(),
                };
                format!(
                    "t{} \"{}\" asked {}: {state}",
                    f.uid,
                    crate::sessions::snippet(&f.question, 60),
                    f.sent_at.format("%H:%M")
                )
            })
            .collect();
        if items.is_empty() {
            String::new()
        } else {
            format!("pending_answers: {}\n", items.join("; "))
        }
    }
}
