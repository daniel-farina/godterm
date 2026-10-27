use std::path::PathBuf;

use crate::deps::{Dep, Group};

pub const MARKER: &str = "voice_onboarding";

pub const HOOK_SPOKEN: &str = "Welcome to GodTerm. With voice on, you run every Claude and Grok session just by saying what you want, hands free. Say yes or press Enter to turn it on, or press Escape to skip.";

pub const HOOK_TITLE: &str = "Run all your coding agent sessions just by talking.";
pub const HOOK_BODY: &str = "Ask how anything works, open projects, log in, approve, switch layouts: say it, and GodTerm does it. Hands free, on this machine.";

pub const REMIND_AFTER_S: u64 = 20;
pub const REMINDER: &str =
    "Still here? Press Enter to set up voice (about a minute), or Esc to go straight to GodTerm.";

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
    pub fn title(self) -> &'static str {
        match self {
            StepId::Ask => "Ask how something works",
            StepId::ClaudeLogin => "Log in a Claude session",
            StepId::Project => "Create a project in account 1",
            StepId::GrokLogin => "Log in a Grok session",
            StepId::Ui => "Change the layout or open loops",
        }
    }

    pub fn example(self) -> &'static str {
        match self {
            StepId::Ask => "how do loops work?",
            StepId::ClaudeLogin => "log in a new Claude account called Work",
            StepId::Project => "create a new project called hello world in account one",
            StepId::GrokLogin => "log in a Grok account called Grok",
            StepId::Ui => "switch to the columns layout",
        }
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Todo,
    Done,
    Skipped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Steps {
    pub marks: Vec<Mark>,

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

    pub fn done(&mut self) -> Option<StepId> {
        self.advance(Mark::Done)
    }

    pub fn skip(&mut self) -> Option<StepId> {
        self.advance(Mark::Skipped)
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    Yes,
    No,
    Skip,
    Next,
    Back,
    Repeat,
    Stop,

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

const PHRASES: &[&str] = &[
    "voice phrases",
    "show voice phrases",
    "show the voice phrases",
    "what can i say",
    "what can i say to you",
];

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

pub fn is_start(raw: &str) -> bool {
    whole(&clean(raw), START)
}

pub fn is_phrases(raw: &str) -> bool {
    whole(&clean(raw), PHRASES)
}

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

pub fn write_marker(o: Outcome) {
    let _ = std::fs::create_dir_all(crate::config::app_home());
    let _ = std::fs::write(
        marker_path(),
        format!("{} {}\n", o.word(), env!("CARGO_PKG_VERSION")),
    );
}

pub fn due(forced: bool, fresh_home: bool, marker: Option<&str>, quiet: bool) -> bool {
    if forced {
        return true;
    }
    !quiet && fresh_home && marker.is_none()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Needs {
    pub install: Vec<(&'static str, &'static str)>,

    pub manual: Vec<(&'static str, String)>,

    pub bytes: u64,

    pub can_hear: bool,

    pub kokoro: bool,
}

impl Needs {
    pub fn ready(&self) -> bool {
        self.can_hear && self.install.is_empty() && self.manual.is_empty()
    }
}

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
        if apple && matches!(d.id, "whisper" | "whisper-model") {
            continue;
        }

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

pub fn voice_name(kokoro_voice: &str) -> String {
    let n = kokoro_voice.split('_').nth(1).unwrap_or(kokoro_voice);
    let mut c = n.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "GodTerm".into(),
    }
}

pub fn meter(db: f32, w: usize) -> String {
    let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let full = (frac * w as f32).round() as usize;
    format!("{}{}", "█".repeat(full), "░".repeat(w - full))
}
