//! Voice onboarding: the first window on a new home, before the grid.
//! A calm welcome speaks a short hook and asks one thing: turn voice on
//! (Enter, or say "yes") or skip (Esc). With a yes it checks what voice
//! needs (the deps code installs the missing pieces after a second yes),
//! tests the mic with a live meter and the speaker with the local voice,
//! then walks through a few short steps the user completes by SAYING a
//! request that the real assistant performs: ask how something works, log
//! in a Claude session, create a project in account 1, log in a Grok
//! session, change the layout or open loops. Every step can be skipped
//! ("skip", "next", a key); "stop onboarding" ends it. A recap card of
//! the most useful phrases closes it, and can be reopened (Ctrl-a J, the
//! palette).
//!
//! This file is the pure part (steps, phrases, the first run decision,
//! what voice still needs); app_onboarding.rs runs it in the app and
//! ui_onboarding.rs draws it.
//!
//! Runs once: a marker file in GodTerm's home records how it ended. Only
//! a brand new home (no config.toml when GodTerm started) shows it on its
//! own; existing users never see it unless they ask (`--onboarding`, the
//! palette, Settings > Voice). Nothing here changes accounts or logs in:
//! those go through the assistant's tools, which ask for a yes first.

use std::path::PathBuf;

use crate::deps::{Dep, Group};

/// The marker in GodTerm's home: "done", "skipped" or "stopped".
pub const MARKER: &str = "voice_onboarding";

/// The hook, spoken in the first seconds of the welcome.
pub const HOOK_SPOKEN: &str = "Welcome to GodTerm. With voice on, you run every Claude and Grok session just by saying what you want, hands free. Say yes or press Enter to turn it on, or press Escape to skip.";
/// The hook as shown.
pub const HOOK_TITLE: &str = "Run every Claude and Grok session just by talking.";
pub const HOOK_BODY: &str = "Ask how anything works, open projects, log in, approve, switch layouts: say it, and GodTerm does it. Hands free, on this machine.";
/// After this long on the welcome with no answer, a gentle reminder.
pub const REMIND_AFTER_S: u64 = 20;
pub const REMINDER: &str =
    "Still here? Press Enter to set up voice (about a minute), or Esc to go straight to GodTerm.";

/// The steps of the lesson, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepId {
    Ask,
    ClaudeLogin,
    Project,
    GrokLogin,
    Ui,
}

pub const STEPS: [StepId; 5] = [
    StepId::Ask,
    StepId::ClaudeLogin,
    StepId::Project,
    StepId::GrokLogin,
    StepId::Ui,
];

impl StepId {
    /// Its line in the step list.
    pub fn title(self) -> &'static str {
        match self {
            StepId::Ask => "Ask how something works",
            StepId::ClaudeLogin => "Log in a Claude session",
            StepId::Project => "Create a project in account 1",
            StepId::GrokLogin => "Log in a Grok session",
            StepId::Ui => "Change the layout or open loops",
        }
    }

    /// What to say, as an example (shown and spoken).
    pub fn example(self) -> &'static str {
        match self {
            StepId::Ask => "how do loops work?",
            StepId::ClaudeLogin => "log in a new Claude account called Work",
            StepId::Project => "create a new project called hello world in account one",
            StepId::GrokLogin => "log in a Grok account called Grok",
            StepId::Ui => "switch to the columns layout",
        }
    }

    /// The short spoken line that starts the step.
    pub fn intro(self) -> &'static str {
        match self {
            StepId::Ask => "First, ask me how something works. Try: how do loops work?",
            StepId::ClaudeLogin => {
                "Now log in a Claude session. Say: log in a new Claude account. I'll ask before anything changes."
            }
            StepId::Project => {
                "Next, a project. Say: create a new project called hello world in account one."
            }
            StepId::GrokLogin => "Now Grok. Say: log in a Grok account. Or say skip.",
            StepId::Ui => "Last one. Say: switch to the columns layout, or show loops.",
        }
    }

    /// A line under the example (what happens, what is safe).
    pub fn note(self) -> &'static str {
        match self {
            StepId::Ask => "The assistant answers out loud and in the panel.",
            StepId::ClaudeLogin => {
                "It asks first: nothing is added or logged in until you say yes."
            }
            StepId::Project => "A new folder and a Claude tab open in account 1.",
            StepId::GrokLogin => "It asks first. No Grok? Say skip.",
            StepId::Ui => "Or say: show loops, show the dashboard, zoom.",
        }
    }

    /// For the assistant: what this step is about (its turn note).
    pub fn for_assistant(self) -> &'static str {
        match self {
            StepId::Ask => {
                "the user asks how something in GodTerm works: answer in one or two short spoken sentences"
            }
            StepId::ClaudeLogin => {
                "log in a Claude session: add a Claude account with add_account (or relogin_account for one that is not logged in yet)"
            }
            StepId::Project => {
                "create a new project in Claude account 1: open_tab on account 1 with a short name"
            }
            StepId::GrokLogin => {
                "log in a Grok session: add_account with harness grok (or relogin_account); if Grok Build is missing, offer install_dependency grok"
            }
            StepId::Ui => {
                "a UI action: set_layout or show (loops, dashboard, overview, zoom)"
            }
        }
    }
}

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Todo,
    Done,
    Skipped,
}

/// The lesson's step machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Steps {
    pub marks: Vec<Mark>,
    /// The current step (== len when every step is through).
    pub cur: usize,
}

impl Default for Steps {
    fn default() -> Self {
        Steps {
            marks: vec![Mark::Todo; STEPS.len()],
            cur: 0,
        }
    }
}

impl Steps {
    pub fn current(&self) -> Option<StepId> {
        STEPS.get(self.cur).copied()
    }

    fn advance(&mut self, m: Mark) -> Option<StepId> {
        if let Some(x) = self.marks.get_mut(self.cur) {
            *x = m;
            self.cur += 1;
        }
        self.current()
    }

    /// The current step is done: the next one (None: all through).
    pub fn done(&mut self) -> Option<StepId> {
        self.advance(Mark::Done)
    }

    /// Skip the current step: the next one.
    pub fn skip(&mut self) -> Option<StepId> {
        self.advance(Mark::Skipped)
    }

    /// Back one step (it is to do again).
    pub fn back(&mut self) -> Option<StepId> {
        if self.cur > 0 {
            self.cur -= 1;
            self.marks[self.cur] = Mark::Todo;
        }
        self.current()
    }

    pub fn count(&self, m: Mark) -> usize {
        self.marks.iter().filter(|x| **x == m).count()
    }
}

/// What a short utterance means to the onboarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    Yes,
    No,
    Skip,
    Next,
    Back,
    Repeat,
    Stop,
    /// Anything else (a request for the assistant, or the mic test).
    Other,
}

const YES: &[&str] = &[
    "yes",
    "yeah",
    "yep",
    "yup",
    "sure",
    "ok",
    "okay",
    "alright",
    "all right",
    "yes please",
    "of course",
    "let's go",
    "lets go",
    "go ahead",
    "do it",
    "sounds good",
    "turn it on",
    "turn on voice",
    "turn voice on",
    "voice on",
    "i heard you",
    "i heard it",
    "i can hear you",
    "loud and clear",
];
const NO: &[&str] = &[
    "no",
    "nope",
    "no thanks",
    "no thank you",
    "not now",
    "later",
    "maybe later",
    "i heard nothing",
    "nothing",
    "i can't hear you",
    "i cannot hear you",
];
const SKIP: &[&str] = &[
    "skip",
    "skip it",
    "skip this",
    "skip that",
    "skip this step",
    "skip this one",
    "skip step",
    "pass",
];
const NEXT: &[&str] = &[
    "next",
    "next step",
    "next one",
    "go on",
    "move on",
    "continue",
    "done",
    "i'm done",
    "im done",
];
const BACK: &[&str] = &["back", "go back", "previous", "previous step", "step back"];
const REPEAT: &[&str] = &[
    "repeat",
    "repeat that",
    "again",
    "say that again",
    "say it again",
    "what was that",
    "one more time",
    "play again",
    "play it again",
    "what do i say",
    "what should i say",
];
const STOP: &[&str] = &[
    "stop onboarding",
    "stop the onboarding",
    "end onboarding",
    "end the onboarding",
    "exit onboarding",
    "quit onboarding",
    "skip onboarding",
    "skip the onboarding",
    "cancel onboarding",
    "stop the tour",
    "end the tour",
    "skip the tour",
    "exit the tour",
    "stop the tutorial",
    "skip the tutorial",
    "skip everything",
    "skip all",
];
/// Phrases that start (or replay) the onboarding from anywhere.
const START: &[&str] = &[
    "voice onboarding",
    "start voice onboarding",
    "replay voice onboarding",
    "start the voice onboarding",
    "replay the voice onboarding",
    "voice tutorial",
    "start the voice tutorial",
    "voice tour",
    "start the voice tour",
    "teach me how to use voice",
];
/// Phrases that reopen the recap card.
const PHRASES: &[&str] = &[
    "voice phrases",
    "show voice phrases",
    "show the voice phrases",
    "what can i say",
    "what can i say to you",
];

/// Lowercase words only, no punctuation or fillers at the ends.
fn clean(raw: &str) -> String {
    let n = crate::voice::grammar::normalize(raw);
    let mut w: Vec<&str> = n.split_whitespace().collect();
    while w
        .first()
        .is_some_and(|x| matches!(*x, "um" | "uh" | "so" | "oh" | "well" | "please"))
    {
        w.remove(0);
    }
    loop {
        let n = w.len();
        if n > 2 && w[n - 2] == "thank" && w[n - 1] == "you" {
            w.truncate(n - 2);
        } else if n > 1 && matches!(w[n - 1], "please" | "thanks" | "now") {
            w.pop();
        } else {
            break;
        }
    }
    w.join(" ")
}

fn whole(c: &str, list: &[&str]) -> bool {
    list.contains(&c)
}

/// Whole utterances only: "skip the tests" is a request, "skip" is not.
pub fn classify(raw: &str) -> Said {
    let c = clean(raw);
    if c.is_empty() {
        return Said::Other;
    }
    if whole(&c, STOP) {
        Said::Stop
    } else if whole(&c, SKIP) {
        Said::Skip
    } else if whole(&c, NEXT) {
        Said::Next
    } else if whole(&c, BACK) {
        Said::Back
    } else if whole(&c, REPEAT) {
        Said::Repeat
    } else if whole(&c, YES) {
        Said::Yes
    } else if whole(&c, NO) {
        Said::No
    } else {
        Said::Other
    }
}

/// "voice onboarding", "start the voice tour": start it from anywhere.
pub fn is_start(raw: &str) -> bool {
    whole(&clean(raw), START)
}

/// "voice phrases", "what can I say": the recap card.
pub fn is_phrases(raw: &str) -> bool {
    whole(&clean(raw), PHRASES)
}

/// The recap: the most useful phrases (said, what it does).
pub const RECAP: [(&str, &str); 5] = [
    ("hey god, ...", "wake me, then say anything you want done"),
    ("how do loops work?", "ask how anything in GodTerm works"),
    (
        "new project called notes in account one",
        "a new folder and tab on that account",
    ),
    ("approve / deny", "answer the tab that is waiting for you"),
    (
        "stop talking / hold on a minute",
        "quiet me, or pause listening",
    ),
];
pub const RECAP_SPOKEN: &str = "That's it. Say hey god, then anything: ask how things work, open projects, approve or deny. Say stop talking to quiet me. These phrases stay on the card; Control A, J brings it back.";

/// How the onboarding ended (the marker's text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Skipped,
    Stopped,
}

impl Outcome {
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Skipped => "skipped",
            Outcome::Stopped => "stopped",
        }
    }
}

pub fn marker_path() -> PathBuf {
    crate::config::app_home().join(MARKER)
}

pub fn read_marker() -> Option<String> {
    std::fs::read_to_string(marker_path())
        .ok()
        .map(|s| s.trim().to_string())
}

/// Record how it ended, so it never starts on its own again.
pub fn write_marker(o: Outcome) {
    let _ = std::fs::create_dir_all(crate::config::app_home());
    let _ = std::fs::write(
        marker_path(),
        format!("{} {}\n", o.word(), env!("CARGO_PKG_VERSION")),
    );
}

/// Show it at this start? `forced`: --onboarding (or a replay). Otherwise
/// only on a brand new home (`fresh_home`: no config.toml when GodTerm
/// started), never once it ended (`marker`), never on a quiet run (tests,
/// smoke runs, no terminal).
pub fn due(forced: bool, fresh_home: bool, marker: Option<&str>, quiet: bool) -> bool {
    if forced {
        return true;
    }
    !quiet && fresh_home && marker.is_none()
}

/// What voice still needs, read from the deps list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Needs {
    /// Missing pieces GodTerm can install (ids), with their names.
    pub install: Vec<(&'static str, &'static str)>,
    /// Missing pieces to install by hand: (name, how).
    pub manual: Vec<(&'static str, String)>,
    /// Download size of `install`.
    pub bytes: u64,
    /// Speech can be recognized now (whisper, its model and ffmpeg, or
    /// Apple's on-device recognizer).
    pub can_hear: bool,
    /// The local Kokoro voice is ready (else `say`, where there is one).
    pub kokoro: bool,
}

impl Needs {
    pub fn ready(&self) -> bool {
        self.can_hear && self.install.is_empty() && self.manual.is_empty()
    }
}

/// The voice pieces of `deps`. `apple`: Apple's recognizer works here,
/// so whisper and its model are not needed to hear (still offered when
/// missing, not required). The speaker lock model is left out: voice
/// works without it.
pub fn needs(deps: &[Dep], apple: bool) -> Needs {
    let ok = |id: &str| deps.iter().any(|d| d.id == id && d.status.ok());
    let mut n = Needs {
        can_hear: apple || (ok("ffmpeg") && ok("whisper") && ok("whisper-model")),
        kokoro: ok("espeak") && ok("kokoro"),
        ..Default::default()
    };
    for d in deps
        .iter()
        .filter(|d| d.group == Group::Voice && d.id != "speaker" && !d.status.ok())
    {
        // With Apple recognition, whisper is a nice to have only.
        if apple && matches!(d.id, "whisper" | "whisper-model") {
            continue;
        }
        // Steps with sudo run in a tab of their own: by hand here.
        if d.installable() && !d.steps.iter().any(crate::deps::Step::needs_sudo) {
            n.install.push((d.id, d.name));
            n.bytes += d.download_bytes();
        } else {
            n.manual.push((
                d.name,
                d.steps
                    .iter()
                    .map(crate::deps::Step::shown)
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
    }
    n
}

/// "Heart" for af_heart: the voice's name as said.
pub fn voice_name(kokoro_voice: &str) -> String {
    let n = kokoro_voice.split('_').nth(1).unwrap_or(kokoro_voice);
    let mut c = n.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "GodTerm".into(),
    }
}

/// A level meter: `w` cells for -60..0 dBFS.
pub fn meter(db: f32, w: usize) -> String {
    let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let full = (frac * w as f32).round() as usize;
    format!("{}{}", "█".repeat(full), "░".repeat(w - full))
}
