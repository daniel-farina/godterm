//! Follow-through for questions the assistant asks a tab's agent: the
//! question is watched, the tab's answer is taken from its transcript
//! once its turn is over, and a turn of our own has the brain tell the
//! user in a sentence or two.
//!
//! A question sent to a busy tab is often taken into its running turn (a
//! queued command attachment) instead of starting a turn of its own, so
//! it is looked for there too; failing that, the reply written after it
//! was sent counts once the tab has worked and is ready again. Several
//! questions one turn answered are told together. Reports wait for a
//! quiet moment (app_quiet). At the wait limit a tab still working is
//! waited for (the user is told once), up to a hard cap; a tab that is
//! idle with no answer found is said plainly. Nothing is dropped
//! silently.

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
    /// The user was told it is still being worked on (past the wait).
    pub told_working: bool,
}

/// Finished entries stay in the snapshot this long.
const KEEP_DONE: Duration = Duration::from_secs(3600);
/// A tab still working past the wait limit is waited for up to this long.
pub const HARD_CAP: Duration = Duration::from_secs(2 * 3600);
/// Turns of a transcript searched for the question.
const SEARCH_TURNS: usize = 40;
/// The send time is taken a little before the paste lands.
const CLOCK_SLACK_S: i64 = 2;

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
pub fn key_of(text: &str) -> String {
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

/// Whether a turn holds the question: as its own prompt, or taken into it
/// while it ran.
fn holds(turn: &crate::sessions::Turn, key: &str) -> bool {
    std::iter::once(&turn.user)
        .chain(turn.queued.iter())
        .any(|t| !t.is_empty() && key_of(t).contains(key))
}

/// Seconds since the epoch of a transcript time.
fn ts(t: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(t)
        .ok()
        .map(|d| d.timestamp())
}

/// The answer to the question keyed `key`, sent at `sent` (epoch
/// seconds): the reply of the turn that holds it, once that turn is over;
/// else, with `fallback`, the reply of the last turn when it is over and
/// was written after the send. None while there is no answer yet.
pub fn find_answer(
    turns: &[crate::sessions::Turn],
    key: &str,
    sent: i64,
    fallback: bool,
) -> Option<String> {
    let reply = |t: &crate::sessions::Turn| {
        t.reply
            .clone()
            .filter(|r| t.finished && !r.trim().is_empty())
    };
    if let Some(pos) = turns.iter().rposition(|t| holds(t, key)) {
        return reply(&turns[pos]);
    }
    if !fallback {
        return None;
    }
    let last = turns.last()?;
    let after = last
        .reply_at
        .as_deref()
        .and_then(ts)
        .is_some_and(|at| at + CLOCK_SLACK_S >= sent);
    if after {
        reply(last)
    } else {
        None
    }
}

impl App {
    /// Watch for the answer to `question`, just sent to tab `uid`.
    pub fn register_follow_up(&mut self, uid: u64, question: &str) {
        let tab = self
            .find_tab(uid)
            .map(|(s, t)| self.panes[s].tabs[t].name())
            .unwrap_or_else(|| format!("t{uid}"));
        // Several questions to one tab are all followed (a busy tab may
        // answer them in one turn); the same question again replaces it.
        let key = key_of(question);
        for f in &mut self.assistant.follow_ups {
            if f.uid == uid && f.state == FuState::Waiting && key_of(&f.question) == key {
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
            told_working: false,
        });
    }

    /// After a brain send_prompt: follow up on the deliveries that ask,
    /// and say in the result whether the answer will be reported
    /// (`will_report`), so nothing is promised that will not come.
    pub fn follow_up_sends(&mut self, args: &Value, v: &mut Value) {
        let explicit = args.get("expect_reply").and_then(Value::as_bool);
        let mut watched: Vec<String> = vec![];
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
                asks_something(&text) || asks_something(&self.assistant.last_user)
            });
            if asks {
                self.register_follow_up(uid, &text);
                if let Some(f) = self.assistant.follow_ups.last() {
                    if !watched.contains(&f.tab) {
                        watched.push(f.tab.clone());
                    }
                }
            }
        }
        let will = !watched.is_empty();
        v["will_report"] = json!(will);
        v["report_say"] = json!(if will {
            format!("I'll tell you when {} answers.", watched.join(" and "))
        } else {
            "Its answer is not reported back on its own: say it was sent and that the user can ask later."
                .to_string()
        });
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

    /// Every second or so: look for answers, keep waiting on a tab still
    /// working past the wait, say what never came, and tell the user what
    /// came in once it is quiet.
    pub fn follow_ups_tick(&mut self) {
        let wait = Duration::from_secs(self.cfg.assistant.answer_wait_min.max(1) * 60);
        let cap = HARD_CAP.max(wait);
        let mut fus = std::mem::take(&mut self.assistant.follow_ups);
        let mut notices: Vec<String> = vec![];
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
                notices.push(format!(
                    "{} was closed before it answered your question.",
                    f.tab
                ));
                continue;
            };
            let past = f.sent.elapsed() > wait;
            let not_found = format!(
                "{} finished but I couldn't find its answer; read_tab has its last reply.",
                f.tab
            );
            match self.panes[s].tabs[t].activity {
                Activity::Working | Activity::Permission | Activity::Starting => {
                    f.seen_working = true;
                    if f.sent.elapsed() > cap {
                        f.state = FuState::TimedOut;
                        f.ended = Some(Instant::now());
                        crate::log::info(&format!(
                            "follow-up: t{uid} still working after {} min; stopped waiting",
                            f.sent.elapsed().as_secs() / 60
                        ));
                        notices.push(format!(
                            "{} is still working after {} hours, so I stopped waiting for its answer; read_tab has its last reply.",
                            f.tab,
                            cap.as_secs() / 3600
                        ));
                    } else if past && !f.told_working {
                        // Busy on it: keep waiting, and say so once.
                        f.told_working = true;
                        crate::log::info(&format!(
                            "follow-up: t{uid} still working past the wait; waiting on"
                        ));
                        notices.push(format!(
                            "{} is still working on your question; I'll tell you when it's done.",
                            f.tab
                        ));
                    }
                    continue;
                }
                Activity::Ready => {}
                Activity::Idle | Activity::Exited => {
                    if past {
                        f.state = FuState::TimedOut;
                        f.ended = Some(Instant::now());
                        crate::log::info(&format!("follow-up: t{uid} idle with no answer"));
                        notices.push(not_found);
                    }
                    continue;
                }
            }
            // The prompt is still on its way, or the tab has not shown it
            // started (a ready screen right after the paste is not the end).
            if !past
                && (self.deliveries.iter().any(|d| d.uid == uid && !d.done())
                    || (!f.seen_working && f.sent.elapsed() < Duration::from_secs(8)))
            {
                continue;
            }
            if !past
                && f.checked
                    .is_some_and(|c| c.elapsed() < Duration::from_millis(1500))
            {
                continue;
            }
            f.checked = Some(Instant::now());
            let answer = self.transcript_path(s, t).and_then(|path| {
                let turns = crate::sessions::follow_turns(&path, SEARCH_TURNS);
                find_answer(
                    &turns,
                    &key_of(&f.question),
                    f.sent_at.timestamp(),
                    f.seen_working,
                )
            });
            match answer {
                Some(text) => {
                    crate::log::info(&format!(
                        "follow-up: t{uid} answered ({} chars)",
                        text.len()
                    ));
                    f.state = FuState::Answered {
                        text,
                        at: chrono::Local::now(),
                    };
                }
                None if past => {
                    f.state = FuState::TimedOut;
                    f.ended = Some(Instant::now());
                    crate::log::info(&format!(
                        "follow-up: t{uid} ready but no answer found; telling the user"
                    ));
                    notices.push(not_found);
                }
                None => {}
            }
        }
        fus.retain(|f| f.ended.is_none_or(|e| e.elapsed() < KEEP_DONE));
        self.assistant.follow_ups = fus;
        for n in notices {
            self.say_update(crate::app_quiet::Voice::Reply, &n);
        }
        self.report_answers();
    }

    /// Whether the user can take a report now: the one quiet gate.
    fn can_report(&self) -> bool {
        self.quiet_moment()
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
        // Questions one tab turn answered are told together.
        let mut groups: Vec<(u64, String, String, Vec<String>)> = vec![];
        for &i in &answered {
            let f = &self.assistant.follow_ups[i];
            let FuState::Answered { text, .. } = &f.state else {
                continue;
            };
            let q = trimmed(&f.question, 300);
            match groups.iter_mut().find(|g| g.0 == f.uid && g.2 == *text) {
                Some(g) => g.3.push(q),
                None => groups.push((f.uid, f.tab.clone(), text.clone(), vec![q])),
            }
        }
        let mut parts = vec![];
        let mut names = vec![];
        for (uid, tab, text, qs) in &groups {
            let asked = match qs.as_slice() {
                [one] => format!("your earlier question \"{one}\""),
                many => format!(
                    "your earlier questions {}",
                    many.iter()
                        .map(|q| format!("\"{q}\""))
                        .collect::<Vec<_>>()
                        .join(" and ")
                ),
            };
            parts.push(format!(
                "(The agent in t{uid} ({tab}) answered {asked}:) {}",
                trimmed(text, 1500)
            ));
            let name = format!("t{uid} ({tab})");
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let how = if parts.len() > 1 || groups.iter().any(|g| g.3.len() > 1) {
            "Summarize each for the user in one or two spoken sentences. Call no tools."
        } else {
            "Summarize it for the user in one or two spoken sentences. Call no tools."
        };
        // Other updates that waited for this quiet moment go in the same
        // short turn.
        let also: Vec<String> = self
            .assistant
            .quiet
            .queue
            .iter()
            .map(|u| u.text.clone())
            .collect();
        let also = if also.is_empty() {
            String::new()
        } else {
            format!(
                "\nAlso tell the user, briefly, in the same reply: {}",
                also.join(" ")
            )
        };
        let msg = format!("{}\n{how}{also}", parts.join("\n"));
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
        self.take_updates();
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
                    FuState::TimedOut => (
                        "stopped waiting, no answer found (the user was told)",
                        None,
                        None,
                    ),
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
                    FuState::Waiting if f.told_working => format!(
                        "waiting {} min (still working; the user was told)",
                        f.sent.elapsed().as_secs() / 60
                    ),
                    FuState::Waiting => format!("waiting {} min", f.sent.elapsed().as_secs() / 60),
                    FuState::Answered { at, .. } => format!("answered {}", at.format("%H:%M")),
                    FuState::Reported { at, .. } => {
                        format!("answered {} (told; read_tab has it)", at.format("%H:%M"))
                    }
                    FuState::TimedOut => "stopped waiting, no answer found (told)".into(),
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
