//! Brainstorm mode: talk as long as you like and nothing is sent. Every
//! utterance is appended to a draft in the assistant's composer (to the
//! assistant, or in direct mode to the tab), shown live with a chip
//! "● brainstorm, 2m 14s, 312 words". "Send it", "done" or "that's it"
//! sends the whole draft as one message, lightly cleaned; "cancel
//! brainstorm" discards it (with Undo); "read it back" says the last
//! sentences. Enter in the composer sends, Esc leaves the mode and keeps
//! the draft. Nothing is replied to meanwhile, so barge-in and spoken
//! updates stay quiet, and the open mic does not go to sleep.
//!
//! The phrases are a short local list, like direct mode's exit phrases.
//! A finish phrase counts only as the whole utterance or at its very end
//! after a sentence break ("... the cache. Send it."), so "I'm done with
//! the API part" is dictation. A weaker one at the end ("... and that's
//! it.") asks "Send it?" first.
//!
//! Outside the mode, speech that sounds unfinished (a trailing "and",
//! "so", "um", a comma, a long run-on, "wait, I'm not done") or several
//! quick fragments in a row get a rare tip to use it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_assistant::{Entry, Who};
use crate::app_direct::core_words;
use crate::voice::grammar::normalize;

/// The card shown on first use (and from the ⓘ).
pub const EXPLAIN: &str = "Brainstorm mode: talk as long as you like; nothing is sent. Say 'done', 'send it' or 'that's it' to send everything, 'cancel brainstorm' to discard, 'read it back' to hear the draft. Or edit it in the box and press Enter.";
/// Its spoken one line version.
pub const EXPLAIN_SPOKEN: &str =
    "Brainstorm mode. Talk as long as you like; nothing is sent until you say send it.";
/// The tip when speech sounds unfinished.
pub const SUGGEST: &str =
    "Sounds like you have more to say. Say 'brainstorm mode' to talk without sending.";

/// A pause this long between utterances starts a new paragraph.
const PARAGRAPH_GAP: Duration = Duration::from_secs(8);
/// "Send it?" stays answerable this long.
const CONFIRM_TTL: Duration = Duration::from_secs(90);
/// A discarded draft can be brought back this long.
const UNDO_TTL: Duration = Duration::from_secs(300);
/// The tip at most this often.
const SUGGEST_EVERY: Duration = Duration::from_secs(300);
/// Fragments this close together count as "in a row".
const FRAGMENT_GAP: Duration = Duration::from_secs(12);
/// A fragment is at most this many words.
const FRAGMENT_WORDS: usize = 12;
/// This many quick fragments in a row get the tip.
const FRAGMENTS: usize = 3;
/// A single utterance this long (words) is a run-on worth the tip.
const RUN_ON_WORDS: usize = 90;
/// After the silence timeout, a yes sends the kept draft this long.
const OFFER_TTL: Duration = Duration::from_secs(180);

/// Where the draft goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Assistant,
    /// A tab in direct mode (its uid and spoken name).
    Tab(u64, String),
}

pub struct Session {
    pub since: Instant,
    pub target: Target,
    /// The last utterance appended (or the start).
    pub last_at: Instant,
    /// Utterances appended.
    pub parts: usize,
    /// "Send it?" was asked.
    pub confirm: Option<Instant>,
}

#[derive(Default)]
pub struct BrainstormState {
    pub on: Option<Session>,
    /// The explanation card shows in the panel.
    pub card: bool,
    /// The last discarded draft, for Undo.
    pub undo: Option<(String, Target, Instant)>,
    /// The silence timeout ended it: a yes sends the kept draft.
    pub offer: Option<(Target, Instant)>,
    /// When the tip was last given.
    pub suggested: Option<Instant>,
    /// Recent utterances outside the mode (fragments in a row).
    pub recent: VecDeque<Instant>,
    /// What it said out loud, newest last (the panel and tests).
    pub said: Vec<String>,
    /// What was sent, newest last: (to the assistant or a tab id, text).
    pub sent: Vec<(String, String)>,
}

/// What an utterance means while brainstorming.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    Send,
    /// Send with a request to structure it first.
    Organize,
    Cancel,
    ReadBack,
    /// "brainstorm mode" again.
    Start,
    /// A sure finish at the end of longer speech: the words before it are
    /// appended, then it is sent.
    SendAfter(String),
    /// A finish that may be part of what was said: appended, then
    /// "Send it?".
    Doubt,
    /// Words for the draft.
    Dictation,
}

const SEND: &[&str] = &[
    "send it",
    "send",
    "send that",
    "send it all",
    "send everything",
    "send it off",
    "send the draft",
    "send the brainstorm",
    "go ahead and send it",
    "you can send it",
    "done",
    "i'm done",
    "im done",
    "i am done",
    "all done",
    "done send it",
    "i'm done send it",
    "that's it",
    "thats it",
    "that is it",
    "that's it send it",
    "that's all",
    "thats all",
    "i'm finished",
    "im finished",
    "finished",
    "end brainstorm",
    "end brainstorm mode",
    "end the brainstorm",
    "end brain dump",
    "end braindump",
    "finish brainstorm",
    "stop brainstorm",
    "stop brainstorming",
    "brainstorm off",
    "brainstorm mode off",
    "exit brainstorm",
    "exit brainstorm mode",
];
const ORGANIZE: &[&str] = &[
    "organize it",
    "organise it",
    "organize this",
    "organize that",
    "organize it for me",
    "organize and send",
    "organize it and send it",
    "send it organized",
    "send it and organize it",
    "organize the brainstorm",
    "organize my notes",
];
const CANCEL: &[&str] = &[
    "cancel brainstorm",
    "cancel brainstorm mode",
    "cancel the brainstorm",
    "cancel brain dump",
    "discard",
    "discard it",
    "discard that",
    "discard brainstorm",
    "discard the brainstorm",
    "discard the draft",
    "throw it away",
    "scrap it",
    "scrap that",
];
const READ_BACK: &[&str] = &[
    "read it back",
    "read back",
    "read that back",
    "read it back to me",
    "read me back",
    "read me what i have",
    "what do i have so far",
    "what have i got so far",
    "what have i said so far",
];
const START: &[&str] = &[
    "brainstorm",
    "brainstorm mode",
    "brainstorming mode",
    "brain storm mode",
    "brain dump",
    "brain dump mode",
    "braindump",
    "braindump mode",
    "brainstorm mode on",
    "start brainstorm",
    "start brainstorm mode",
    "start brainstorming",
    "start a brainstorm",
    "start a brain dump",
    "start brain dump",
    "enter brainstorm mode",
    "turn on brainstorm mode",
    "let's brainstorm",
    "lets brainstorm",
    "let me brainstorm",
    "let me brain dump",
    "i want to brainstorm",
];
/// Openers that may lead straight into the dictation ("Brainstorm mode.
/// So the first thing...").
const START_PREFIX: &[&str] = &[
    "brainstorm mode",
    "brain dump mode",
    "braindump mode",
    "start brainstorm mode",
];
/// Sure finishes at the end of longer speech, after a sentence break.
const END_SURE: &[&str] = &[
    "end brainstorm",
    "end brainstorm mode",
    "end the brainstorm",
    "end brain dump",
    "send the brainstorm",
];
/// Finishes at the end that need a sentence break, and a yes when it is
/// only a comma.
const END_SEND: &[&str] = &["send it", "go ahead and send it", "send it all"];
/// Finishes at the end that could be part of what was said: asked.
const END_WEAK: &[&str] = &[
    "that's it",
    "thats it",
    "that's all",
    "i'm done",
    "im done",
    "i am done",
    "done",
    "i'm finished",
];
const UNDO: &[&str] = &[
    "undo",
    "undo that",
    "undo it",
    "undo discard",
    "undo the discard",
    "bring it back",
    "restore it",
    "restore the brainstorm",
    "bring back the brainstorm",
];
const NOT_DONE: &[&str] = &[
    "i'm not done",
    "im not done",
    "i am not done",
    "i'm not finished",
    "i am not finished",
    "i wasn't done",
    "i wasn't finished",
    "not done yet",
    "i'm not done yet",
    "wait i'm not done",
    "wait wait",
    "hold on i'm not done",
    "hang on i'm not done",
];
/// "Don't suggest that again" right after a tip.
const DISMISS: &[&str] = &[
    "don't suggest that again",
    "dont suggest that again",
    "don't suggest it again",
    "never suggest that again",
    "stop suggesting that",
    "stop suggesting it",
    "don't suggest that",
];
/// The same naming it (any time).
const DISMISS_NAMED: &[&str] = &[
    "don't suggest brainstorm mode",
    "dont suggest brainstorm mode",
    "don't suggest brainstorm mode again",
    "stop suggesting brainstorm mode",
    "never suggest brainstorm mode",
    "turn off brainstorm suggestions",
    "no more brainstorm suggestions",
];

fn words_of(s: &str) -> Vec<String> {
    s.split(' ').map(str::to_string).collect()
}

/// normalize, with typographic apostrophes as plain ones.
fn norm(raw: &str) -> String {
    normalize(&raw.replace(['\u{2019}', '\u{2018}'], "'"))
}

/// The utterance's words, fillers at either end dropped, as one string.
fn core(raw: &str) -> String {
    core_words(&norm(raw)).join(" ")
}

fn whole(raw: &str, list: &[&str]) -> bool {
    let s = core(raw);
    !s.is_empty() && list.contains(&s.as_str())
}

/// `raw` without its last words: as many whitespace tokens as it takes to
/// cover `n` normalized words.
fn drop_last_words(raw: &str, n: usize) -> String {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    let mut k = 0;
    let mut covered = 0;
    while k < toks.len() && covered < n {
        k += 1;
        covered += norm(toks[toks.len() - k]).split_whitespace().count();
    }
    toks[..toks.len() - k].join(" ")
}

/// `raw` without its first words (as many tokens as cover `n`).
fn drop_first_words(raw: &str, n: usize) -> String {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    let mut k = 0;
    let mut covered = 0;
    while k < toks.len() && covered < n {
        covered += norm(toks[k]).split_whitespace().count();
        k += 1;
    }
    toks[k..].join(" ")
}

/// How many normalized words at the end core_words dropped (fillers like
/// "please" after the phrase).
fn tail_dropped(raw: &str) -> usize {
    let n = norm(raw);
    let all = n.split_whitespace().count();
    let core = core_words(&n);
    // core_words drops at both ends: count the tail by matching from the
    // front of what is left.
    let words: Vec<&str> = n.split_whitespace().collect();
    let lead = (0..=all.saturating_sub(core.len()))
        .find(|&i| {
            words
                .get(i..i + core.len())
                .is_some_and(|w| w.iter().zip(&core).all(|(a, b)| a == b))
        })
        .unwrap_or(0);
    all - lead - core.len()
}

/// Whether `head` ends at a sentence break (or only a comma).
fn break_after(head: &str) -> Option<bool> {
    let h = head.trim_end();
    if h.ends_with(['.', '!', '?', ':', ';']) {
        Some(true)
    } else if h.ends_with([',', '-']) {
        Some(false)
    } else {
        None
    }
}

/// What an utterance means while brainstorming (see the module doc).
pub fn classify(raw: &str) -> Said {
    if whole(raw, SEND) {
        return Said::Send;
    }
    if whole(raw, ORGANIZE) {
        return Said::Organize;
    }
    if whole(raw, CANCEL) {
        return Said::Cancel;
    }
    if whole(raw, READ_BACK) {
        return Said::ReadBack;
    }
    if whole(raw, START) {
        return Said::Start;
    }
    let s = core(raw);
    let w = words_of(&s);
    let ends = |p: &str| {
        let pw = words_of(p);
        w.len() > pw.len() && w[w.len() - pw.len()..] == pw[..]
    };
    let head_for = |p: &str| {
        let n = p.split(' ').count() + tail_dropped(raw);
        drop_last_words(raw, n)
    };
    for p in END_SURE {
        if ends(p) {
            let head = head_for(p);
            if break_after(&head).is_some() {
                return Said::SendAfter(head);
            }
        }
    }
    for p in END_SEND {
        if ends(p) {
            let head = head_for(p);
            return match break_after(&head) {
                Some(true) => Said::SendAfter(head),
                Some(false) => Said::Doubt,
                None => Said::Dictation,
            };
        }
    }
    for p in END_WEAK {
        if ends(p) && break_after(&head_for(p)).is_some() {
            return Said::Doubt;
        }
    }
    Said::Dictation
}

/// "brainstorm mode" (Some with what followed it, maybe empty), or None.
pub fn start_phrase(raw: &str) -> Option<String> {
    if whole(raw, START) {
        return Some(String::new());
    }
    let n = norm(raw);
    let lead = n.split_whitespace().count() - core_words(&n).len() - tail_dropped(raw);
    let s = core(raw);
    for p in START_PREFIX {
        if let Some(rest) = s.strip_prefix(&format!("{p} ")) {
            if !rest.trim().is_empty() {
                let rest = drop_first_words(raw, lead + p.split(' ').count());
                let rest = rest.trim_start_matches(|c: char| !c.is_alphanumeric());
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Speech that sounds unfinished: a trailing connective, filler or comma,
/// or a long run-on.
pub fn sounds_unfinished(raw: &str) -> bool {
    crate::voice::grammar::ends_unfinished(raw)
        || normalize(raw).split_whitespace().count() >= RUN_ON_WORDS
}

/// The draft as it is sent: fragments joined, lone fillers dropped,
/// spaces collapsed, paragraphs kept at the long pauses.
pub fn clean(draft: &str) -> String {
    const FILLERS: &[&str] = &["um", "uh", "umm", "uhm", "erm", "er", "hmm", "mm"];
    let mut paras = vec![];
    for p in draft.split("\n\n") {
        let mut out: Vec<String> = vec![];
        for tok in p.split_whitespace() {
            let bare: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase();
            if FILLERS.contains(&bare.as_str()) {
                // "um," or "uh." keeps nothing; a period ends the word before.
                if tok.ends_with(['.', '?', '!']) {
                    if let Some(last) = out.last_mut() {
                        let t = last.trim_end_matches(',').to_string();
                        if !t.ends_with(['.', '?', '!']) {
                            *last = format!("{t}.");
                        }
                    }
                }
                continue;
            }
            out.push(tok.to_string());
        }
        // A fragment cut at a pause ("the cache and. Then the queue") is
        // joined: the period after a connective goes, the next word is
        // lower case again.
        for i in 0..out.len().saturating_sub(1) {
            let t = out[i].clone();
            let word = t.trim_end_matches('.').to_lowercase();
            if t.ends_with('.')
                && !t.ends_with("..")
                && crate::voice::grammar::CONNECTIVES.contains(&word.as_str())
            {
                out[i] = t.trim_end_matches('.').to_string();
                let next = out[i + 1].clone();
                let first_word = next.trim_end_matches(|c: char| !c.is_alphanumeric());
                if first_word != "I" && !next.starts_with("I'") {
                    let mut cs = next.chars();
                    if let Some(c) = cs.next() {
                        out[i + 1] = format!("{}{}", c.to_lowercase(), cs.as_str());
                    }
                }
            }
        }
        let s = out.join(" ");
        let s = s.trim().trim_start_matches([',', '.']).trim().to_string();
        if s.is_empty() {
            continue;
        }
        let mut cs = s.chars();
        let first = cs.next().map(|c| c.to_uppercase().collect::<String>());
        paras.push(format!("{}{}", first.unwrap_or_default(), cs.as_str()));
    }
    paras.join("\n\n")
}

/// "2m 14s", "45s".
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

/// Words in a draft.
pub fn word_count(s: &str) -> usize {
    s.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count()
}

/// The last couple of sentences, for "read it back".
pub fn last_sentences(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut ends: Vec<usize> = flat
        .char_indices()
        .filter(|(i, c)| {
            matches!(c, '.' | '!' | '?') && flat[i + 1..].starts_with(' ') && *i + 1 < flat.len()
        })
        .map(|(i, _)| i + 1)
        .collect();
    // The cut is after the n-th sentence end from the back (the last
    // one closes the draft).
    if flat.ends_with(['.', '!', '?']) {
        ends.push(flat.len());
    }
    let cut = if ends.len() > n {
        ends[ends.len() - n - 1]
    } else {
        0
    };
    let t = flat[cut..].trim();
    if t.chars().count() > 280 {
        let skip = t.chars().count() - 280;
        let rest: String = t.chars().skip(skip).collect();
        format!("...{}", rest.trim_start())
    } else {
        t.to_string()
    }
}

impl App {
    pub fn brainstorm_on(&self) -> bool {
        self.brainstorm.on.is_some()
    }

    /// "● brainstorm, 2m 14s, 312 words" for the composer.
    pub fn brainstorm_chip(&self) -> Option<String> {
        let s = self.brainstorm.on.as_ref()?;
        let to = match &s.target {
            Target::Assistant => String::new(),
            Target::Tab(_, name) => format!(" to {}", crate::paths::middle(name, 18)),
        };
        Some(format!(
            "● brainstorm{to}, {}, {} words",
            clock(s.since.elapsed()),
            word_count(&self.assistant.input)
        ))
    }

    /// Say a brainstorm line (recorded for the panel and tests).
    fn bs_say(&mut self, text: &str) {
        self.brainstorm.said.push(text.to_string());
        let n = self.brainstorm.said.len();
        if n > 30 {
            self.brainstorm.said.drain(..n - 30);
        }
        self.speak(text);
    }

    /// Ctrl-a B, the quick bar, the menus: on, or off keeping the draft.
    pub fn toggle_brainstorm(&mut self) {
        if self.brainstorm_on() {
            self.leave_brainstorm();
        } else {
            let say = self.start_brainstorm(true);
            self.flash(say);
        }
    }

    /// Start it: the draft goes to the direct tab when direct mode is on,
    /// else to the assistant. The first time, the card explains it. With
    /// `speak`, its one line is said. Returns that line.
    pub fn start_brainstorm(&mut self, speak: bool) -> String {
        if self.brainstorm_on() {
            return "Already in brainstorm mode. Say send it when you're done.".into();
        }
        let target = match self.direct.on.as_ref() {
            Some(d) => Target::Tab(d.uid, d.name.clone()),
            None => Target::Assistant,
        };
        crate::log::info(&format!("brainstorm: on ({target:?})"));
        self.brainstorm.on = Some(Session {
            since: Instant::now(),
            target,
            last_at: Instant::now(),
            parts: 0,
            confirm: None,
        });
        self.brainstorm.offer = None;
        self.assistant.show = true;
        if self.assistant.win == crate::app_panelwin::PanelWin::Min {
            self.assistant.win = crate::app_panelwin::PanelWin::Normal;
        }
        let first = !self.cfg.voice.brainstorm_explained;
        if first {
            self.brainstorm.card = true;
            self.cfg.voice.brainstorm_explained = true;
            if !cfg!(test) {
                let _ = crate::settings::write(
                    &crate::config::Config::path(),
                    &crate::settings::Key::Global("voice.brainstorm_explained"),
                    Some(toml_edit::value(true)),
                );
                self.config_mtime = crate::app::config_mtime();
            }
        }
        // Nothing is said over the user meanwhile; open mic transcribes
        // everything (no wake word screening in the engine).
        if let Some(v) = &self.voice.engine {
            v.stop_speaking();
            v.set_brainstorm(true);
        }
        self.voice.wake_until = None;
        self.voice.action = Some("brainstorm: nothing is sent".into());
        let line = if first {
            EXPLAIN_SPOKEN.to_string()
        } else {
            "Brainstorm mode. Say send it when you're done.".to_string()
        };
        if speak {
            self.bs_say(&line);
        }
        line
    }

    /// The mode ends (sent, discarded or left).
    fn end_session(&mut self) -> Option<Session> {
        let s = self.brainstorm.on.take()?;
        self.brainstorm.card = false;
        if let Some(v) = &self.voice.engine {
            v.set_brainstorm(false);
        }
        // The open mic's sleep timer starts over.
        self.voice.last_heard = Instant::now();
        Some(s)
    }

    /// Esc, or Ctrl-a B again: out of the mode, the draft kept in the box.
    pub fn leave_brainstorm(&mut self) {
        if self.end_session().is_none() {
            return;
        }
        crate::log::info("brainstorm: left, draft kept");
        self.voice.action = Some("brainstorm off: the draft is in the box".into());
        self.flash(if self.assistant.input.trim().is_empty() {
            "Brainstorm off".to_string()
        } else {
            "Brainstorm off: the draft stays in the box (Enter sends it)".to_string()
        });
    }

    /// Append an utterance to the draft: a new paragraph after a long
    /// pause, else a space.
    pub fn brainstorm_append(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let gap = self
            .brainstorm
            .on
            .as_ref()
            .map(|s| s.parts > 0 && s.last_at.elapsed() >= PARAGRAPH_GAP)
            .unwrap_or(false);
        let cur = self.assistant.input.trim_end().to_string();
        let joined = if cur.is_empty() {
            text.to_string()
        } else if gap {
            format!("{cur}\n\n{text}")
        } else {
            format!("{cur} {text}")
        };
        self.assistant.set_input(joined);
        self.assistant.hist_pos = None;
        if let Some(s) = self.brainstorm.on.as_mut() {
            s.parts += 1;
            s.last_at = Instant::now();
        }
        let n = word_count(&self.assistant.input);
        self.voice.action = Some(format!("brainstorm: {n} words drafted"));
    }

    /// An utterance while brainstorming (after mute and pause): nothing
    /// is sent except on a finish phrase.
    pub fn brainstorm_heard(&mut self, text: &str, norm: &str, woke: Option<String>) {
        // The wake word alone: nothing to answer; with words, the words.
        let said = match woke {
            Some(rest) if rest.trim().is_empty() => {
                self.voice.action = Some("brainstorm: listening".into());
                return;
            }
            Some(_) => {
                // Keep the user's own punctuation for the draft: drop the
                // wake word's tokens from the raw text.
                let n = norm.split_whitespace().count()
                    - self
                        .strip_wake(norm)
                        .map(|r| r.split_whitespace().count())
                        .unwrap_or(0);
                drop_first_words(text, n)
            }
            None => text.trim().to_string(),
        };
        let said = said
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .to_string();
        if crate::app_openmic::is_noise(&normalize(&said)) {
            self.voice.ignored += 1;
            self.voice.action = Some("(noise, ignored)".into());
            return;
        }
        // The mic mute still works.
        if core(&said) == "mute" || core(&said) == "mute the mic" {
            self.set_muted(true);
            return;
        }
        // "Send it?" was asked.
        let asked = self
            .brainstorm
            .on
            .as_mut()
            .and_then(|s| s.confirm.take())
            .is_some_and(|t| t.elapsed() < CONFIRM_TTL);
        let what = classify(&said);
        if asked {
            if what == Said::Send || crate::instant::affirmative(&said) {
                return self.finish_brainstorm(false, true);
            }
            if crate::instant::negative(&said) || whole(&said, &["keep going", "not yet"]) {
                self.bs_say("Okay, keep going.");
                return;
            }
        }
        crate::log::info(&format!("brainstorm: {what:?}: {said}"));
        match what {
            Said::Send => self.finish_brainstorm(false, true),
            Said::Organize => self.finish_brainstorm(true, true),
            Said::Cancel => self.cancel_brainstorm(),
            Said::ReadBack => self.brainstorm_read_back(),
            Said::Start => self.bs_say("Still in brainstorm mode. Say send it when you're done."),
            Said::SendAfter(head) => {
                self.brainstorm_append(&head);
                self.finish_brainstorm(false, true);
            }
            Said::Doubt => {
                self.brainstorm_append(&said);
                if let Some(s) = self.brainstorm.on.as_mut() {
                    s.confirm = Some(Instant::now());
                }
                self.bs_say("Send it?");
            }
            Said::Dictation => self.brainstorm_append(&said),
        }
    }

    /// Send the whole draft as one message (to the assistant or the tab);
    /// `organize` asks for it structured into notes and tasks first.
    pub fn finish_brainstorm(&mut self, organize: bool, by_voice: bool) {
        let Some(s) = self.brainstorm.on.as_ref() else {
            return;
        };
        let target = s.target.clone();
        let took = s.since.elapsed();
        let draft = clean(&self.assistant.input);
        if draft.is_empty() {
            self.end_session();
            self.assistant.set_input(String::new());
            self.bs_say("Nothing was drafted, so I sent nothing. Brainstorm mode is off.");
            return;
        }
        if target == Target::Assistant && !self.assistant_on() {
            self.flash("The assistant is off: the draft stays in the box (Settings > Assistant)");
            return;
        }
        self.end_session();
        self.assistant.set_input(String::new());
        let n = word_count(&draft);
        crate::log::info(&format!(
            "brainstorm: sending {n} words after {} ({target:?}){}",
            clock(took),
            if organize { ", organized" } else { "" }
        ));
        self.voice.action = Some(format!("brainstorm sent: {n} words"));
        match target {
            Target::Assistant => {
                self.brainstorm
                    .sent
                    .push(("assistant".into(), draft.clone()));
                if self.assistant.input_hist.last() != Some(&draft) {
                    self.assistant.input_hist.push(draft.clone());
                }
                let how = if organize {
                    " Before acting on any of it, organize this brain dump into short notes and a task list, show them, and ask which to start with."
                } else {
                    " Treat it as one request; the fragments belong together."
                };
                self.assistant.brainstorm_note = Some(format!(
                    "(Brainstorm: the user talked for {} in brainstorm mode, {n} words, nothing sent until now; this is all of it.{how})",
                    clock(took)
                ));
                if by_voice {
                    self.quiet_user_spoke();
                } else {
                    self.quiet_user_typed();
                }
                self.assistant.show_admin = false;
                let heard = by_voice.then_some(draft.as_str());
                self.start_turn(&draft, heard, None);
            }
            Target::Tab(uid, name) => {
                let Some((_, _)) = self.locate_uid(uid) else {
                    self.assistant.set_input(draft);
                    self.bs_say(&format!("{name} is gone, so the draft stays in the box."));
                    return;
                };
                let text = if organize {
                    format!("Organize this brain dump into short notes and a task list before acting on any of it, and ask me which to start with:\n\n{draft}")
                } else {
                    draft.clone()
                };
                self.brainstorm
                    .sent
                    .push((crate::control::tab_id(uid), text.clone()));
                if let Some(v) = &self.voice.engine {
                    v.stop_speaking();
                }
                let id = self.deliver_request(uid, &text);
                crate::log::info(&format!(
                    "brainstorm: typed into {} as #{id}",
                    crate::control::tab_id(uid)
                ));
                if let Some(d) = self.direct.on.as_mut().filter(|d| d.uid == uid) {
                    d.log.push((true, text.clone()));
                    d.last_sent = Some(text.clone());
                }
                self.direct_line(format!(
                    "you → {name}: (brainstorm, {n} words) {}",
                    crate::sessions::snippet(&text, 200)
                ));
                self.bs_say(&format!("Sent to {name}."));
            }
        }
    }

    /// "cancel brainstorm": the draft is cleared (Undo brings it back).
    pub fn cancel_brainstorm(&mut self) {
        let Some(s) = self.end_session() else {
            return;
        };
        let draft = std::mem::take(&mut self.assistant.input);
        self.assistant.set_input(String::new());
        crate::log::info(&format!(
            "brainstorm: discarded {} words",
            word_count(&draft)
        ));
        if !draft.trim().is_empty() {
            self.brainstorm.undo = Some((draft, s.target, Instant::now()));
        }
        self.voice.action = Some("brainstorm discarded".into());
        self.flash("Brainstorm discarded: say \"undo\" or click Undo to bring it back");
        self.bs_say("Discarded. Say undo to bring it back.");
    }

    /// Whether a discarded draft can still come back.
    pub fn brainstorm_undo_ready(&self) -> bool {
        self.brainstorm
            .undo
            .as_ref()
            .is_some_and(|u| u.2.elapsed() < UNDO_TTL)
    }

    /// Undo: the discarded draft back in the box, brainstorming again.
    pub fn undo_brainstorm(&mut self) -> bool {
        if !self.brainstorm_undo_ready() || self.brainstorm_on() {
            return false;
        }
        let Some((draft, target, _)) = self.brainstorm.undo.take() else {
            return false;
        };
        let cur = self.assistant.input.trim().to_string();
        self.assistant.set_input(if cur.is_empty() {
            draft
        } else {
            format!("{draft}\n\n{cur}")
        });
        self.brainstorm.on = Some(Session {
            since: Instant::now(),
            target,
            last_at: Instant::now(),
            parts: 1,
            confirm: None,
        });
        if let Some(v) = &self.voice.engine {
            v.set_brainstorm(true);
        }
        self.assistant.show = true;
        self.flash("The brainstorm is back: keep talking, or say send it");
        self.bs_say("It's back. Keep going.");
        true
    }

    /// "read it back": how long it is and its last sentences.
    pub fn brainstorm_read_back(&mut self) {
        let d = self.assistant.input.clone();
        let n = word_count(&d);
        if n == 0 {
            self.bs_say("Nothing drafted yet. Go ahead.");
            return;
        }
        let last = last_sentences(&d, 2);
        let line = if n <= 40 {
            format!(
                "So far: {}",
                d.split_whitespace().collect::<Vec<_>>().join(" ")
            )
        } else {
            format!("{n} words so far. It ends: {last}")
        };
        self.bs_say(&line);
    }

    /// Every tick: a long silence ends the mode (the draft is kept and a
    /// yes sends it).
    pub fn brainstorm_tick(&mut self) {
        if let Some((_, t)) = &self.brainstorm.offer {
            if t.elapsed() >= OFFER_TTL {
                self.brainstorm.offer = None;
            }
        }
        let min = self.cfg.voice.brainstorm_timeout_min.max(1) as u64;
        let Some(s) = self.brainstorm.on.as_ref() else {
            return;
        };
        if s.last_at.elapsed() < Duration::from_secs(min * 60) {
            return;
        }
        let target = s.target.clone();
        self.end_session();
        crate::log::info("brainstorm: silence timeout, draft kept");
        if self.assistant.input.trim().is_empty() {
            self.flash("Brainstorm mode ended after a long silence");
            return;
        }
        self.brainstorm.offer = Some((target, Instant::now()));
        let mins = crate::app_pause::spoken_duration(min * 60);
        self.flash(format!(
            "Brainstorm ended after {mins} of silence: the draft stays in the box (Enter sends it)"
        ));
        self.bs_say(&format!(
            "You've been quiet for {mins}, so I kept your brainstorm as a draft. Send it?"
        ));
    }

    /// Outside the mode, before an utterance is routed: "brainstorm mode",
    /// undo, an answer to the timeout's "Send it?", "I'm not done" and
    /// "don't suggest that again". True when it was handled here.
    pub fn brainstorm_intercept(&mut self, text: &str, command: &str) -> bool {
        if let Some(rest) = start_phrase(command) {
            self.start_brainstorm(true);
            // "Brainstorm mode. So the first thing..." keeps the rest.
            let rest = if rest.is_empty() {
                start_phrase(text).unwrap_or_default()
            } else {
                start_phrase(text).filter(|r| !r.is_empty()).unwrap_or(rest)
            };
            if !rest.trim().is_empty() {
                self.brainstorm_append(&rest);
            }
            return true;
        }
        if whole(command, UNDO) && self.undo_brainstorm() {
            return true;
        }
        if let Some((target, _)) = self.brainstorm.offer.clone() {
            let yes = whole(command, SEND) || crate::instant::affirmative(text);
            let no = crate::instant::negative(text);
            if yes || no {
                self.brainstorm.offer = None;
            }
            if yes && !self.assistant.input.trim().is_empty() {
                self.brainstorm.on = Some(Session {
                    since: Instant::now(),
                    target,
                    last_at: Instant::now(),
                    parts: 1,
                    confirm: None,
                });
                self.finish_brainstorm(false, true);
                return true;
            }
            if no {
                self.bs_say("Okay, it stays in the box.");
                return true;
            }
        }
        if whole(command, DISMISS_NAMED)
            || (whole(command, DISMISS)
                && self
                    .brainstorm
                    .suggested
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(900)))
        {
            self.set_brainstorm_suggest(false);
            self.bs_say("Okay, I won't suggest it again.");
            return true;
        }
        // "Wait, I'm not done": not a request; the tip (when allowed).
        if whole(command, NOT_DONE) && self.cfg.voice.brainstorm_suggest {
            if let Some(v) = &self.voice.engine {
                v.stop_speaking();
            }
            if self.suggest_allowed() {
                self.give_suggestion();
            } else {
                self.voice.action = Some("(not done: waiting)".into());
            }
            return true;
        }
        false
    }

    /// voice.brainstorm_suggest, kept in config.toml.
    pub fn set_brainstorm_suggest(&mut self, on: bool) {
        self.cfg.voice.brainstorm_suggest = on;
        if !cfg!(test) {
            let _ = crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global("voice.brainstorm_suggest"),
                Some(toml_edit::value(on)),
            );
            self.config_mtime = crate::app::config_mtime();
        }
    }

    fn suggest_allowed(&self) -> bool {
        self.cfg.voice.brainstorm_suggest
            && self
                .brainstorm
                .suggested
                .is_none_or(|t| t.elapsed() >= SUGGEST_EVERY)
    }

    fn give_suggestion(&mut self) {
        crate::log::info("brainstorm: suggested");
        self.brainstorm.suggested = Some(Instant::now());
        self.brainstorm.recent.clear();
        self.assistant.log.push(Entry::new(
            Who::Note,
            format!("{SUGGEST} (Ctrl-a B; \"don't suggest that again\" turns this off)"),
        ));
        self.bs_say(SUGGEST);
    }

    /// An utterance on its way (to the assistant or a tab): when it
    /// sounds unfinished, or comes as one of several quick fragments,
    /// the rare tip. True when the tip was given.
    pub fn brainstorm_suggest_check(&mut self, text: &str) -> bool {
        // Fragments in a row: short utterances, each soon after the one
        // before (a long one, or a pause, starts the count over).
        let now = Instant::now();
        let short = normalize(text).split_whitespace().count() <= FRAGMENT_WORDS;
        let soon = self
            .brainstorm
            .recent
            .back()
            .is_some_and(|t| now.duration_since(*t) <= FRAGMENT_GAP);
        if !short || !soon {
            self.brainstorm.recent.clear();
        }
        if short {
            self.brainstorm.recent.push_back(now);
        }
        if self.brainstorm_on() || !self.suggest_allowed() {
            return false;
        }
        let fragments = self.brainstorm.recent.len() >= FRAGMENTS;
        if sounds_unfinished(text) || fragments {
            self.give_suggestion();
            return true;
        }
        false
    }

    /// The panel's ⓘ: the card shown (or hidden again).
    pub fn toggle_brainstorm_card(&mut self) {
        self.brainstorm.card = !self.brainstorm.card;
        if self.brainstorm.card {
            self.assistant.show = true;
            self.speak(EXPLAIN_SPOKEN);
        }
    }

    /// The brainstorm tool: on starts it (its one line returned to say),
    /// off leaves it with the draft kept.
    pub fn brainstorm_tool(&mut self, args: &Value) -> Result<Value, String> {
        let on = args.get("on").and_then(Value::as_bool).unwrap_or(true);
        if on {
            let say = self.start_brainstorm(false);
            return Ok(json!({"ok": true, "result": {
                "brainstorm": true,
                "say": say,
                "next": "Say exactly the say text and nothing else. From now on nothing the user says reaches you: it is drafted until they say send it (or done, that's it), and then the whole draft arrives as one message."
            }}));
        }
        let was = self.brainstorm_on();
        self.leave_brainstorm();
        Ok(json!({"ok": true, "result": {
            "brainstorm": false,
            "say": if was { "Brainstorm mode is off. The draft stays in the box." } else { "Brainstorm mode was not on." },
        }}))
    }

    /// For the state block.
    pub fn brainstorm_state_words(&self) -> Option<String> {
        self.brainstorm.on.as_ref().map(|s| {
            let to = match &s.target {
                Target::Assistant => String::new(),
                Target::Tab(uid, _) => format!(" for {}", crate::control::tab_id(*uid)),
            };
            format!(
                "brainstorm: on, {} words drafted{to} (nothing reaches you until the user sends it)",
                word_count(&self.assistant.input)
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_phrases_are_whole_or_at_the_very_end() {
        for s in [
            "Send it.",
            "Done.",
            "That's it.",
            "I'm done.",
            "okay, send it please",
            "End brainstorm.",
        ] {
            assert_eq!(classify(s), Said::Send, "{s}");
        }
        for s in [
            "I'm done with the API part",
            "We send it to the server and wait",
            "then we compute the hash and send it",
            "The parser is done",
            "that's it for the cache, now the queue",
        ] {
            assert_eq!(classify(s), Said::Dictation, "{s}");
        }
        assert_eq!(
            classify("And the cache needs a TTL. Send it."),
            Said::SendAfter("And the cache needs a TTL.".into())
        );
        assert_eq!(
            classify("the cache needs a TTL. End brainstorm."),
            Said::SendAfter("the cache needs a TTL.".into())
        );
        // Could be part of what was said: asked.
        assert_eq!(classify("And the cache, that's it."), Said::Doubt);
        assert_eq!(classify("The queue is slow. I'm done."), Said::Doubt);
        assert_eq!(classify("the queue, send it"), Said::Doubt);
        assert_eq!(classify("Cancel brainstorm."), Said::Cancel);
        assert_eq!(classify("Discard it."), Said::Cancel);
        assert_eq!(classify("Read it back."), Said::ReadBack);
        assert_eq!(classify("Organize it."), Said::Organize);
    }

    #[test]
    fn starting_phrases() {
        for s in [
            "Brainstorm mode.",
            "brain dump mode",
            "Braindump.",
            "Okay, brainstorm mode",
        ] {
            assert_eq!(start_phrase(s).as_deref(), Some(""), "{s}");
        }
        assert_eq!(
            start_phrase("Brainstorm mode. So the first thing is the cache.").as_deref(),
            Some("So the first thing is the cache.")
        );
        assert_eq!(start_phrase("we should brainstorm mode names"), None);
        assert_eq!(start_phrase("run the tests"), None);
    }

    #[test]
    fn the_draft_is_cleaned_lightly() {
        let d = "um so the cache and. Then the queue, uh, is slow\n\nsecond part  here";
        assert_eq!(
            clean(d),
            "So the cache and then the queue, is slow\n\nSecond part here"
        );
        assert_eq!(clock(Duration::from_secs(134)), "2m 14s");
        assert_eq!(clock(Duration::from_secs(45)), "45s");
        assert_eq!(word_count("one two, three. -"), 3);
        assert_eq!(
            last_sentences("First one. Second one. Third one.", 2),
            "Second one. Third one."
        );
    }

    #[test]
    fn unfinished_speech_sounds_unfinished() {
        assert!(sounds_unfinished("so we need the cache and"));
        assert!(sounds_unfinished("first the parser, um"));
        assert!(!sounds_unfinished("Run the tests."));
        let long = "word ".repeat(100);
        assert!(sounds_unfinished(&format!("{long}.")));
    }
}
