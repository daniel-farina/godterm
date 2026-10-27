use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::{App, View};
use crate::harness::Harness;
use crate::onboarding::{self, Needs, Outcome, Said, StepId, Steps};

pub use crate::onboarding::Mark;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Welcome,

    Install,

    Mic,

    Speaker,

    Lesson,

    Recap,
}

impl Phase {
    pub fn full_screen(self) -> bool {
        matches!(
            self,
            Phase::Welcome | Phase::Install | Phase::Mic | Phase::Speaker
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Snap {
    claude: usize,
    grok: usize,
    tabs_first: usize,
    layout: String,
    zoom: bool,
    view: View,
    turn: u64,
}

pub struct Onb {
    pub phase: Phase,
    pub phase_at: Instant,
    pub steps: Steps,
    pub step_at: Instant,
    snap: Option<Snap>,

    pub voice: bool,

    pub heard: Option<String>,
    queue: VecDeque<String>,
    pub reminded: bool,

    pub installing: bool,
    pub install_error: Option<String>,

    pub mic_heard: Option<String>,

    pub speaker: Option<bool>,

    pub login_walk: bool,
}

#[derive(Default)]
pub struct OnbState {
    pub on: Option<Onb>,

    pub card: bool,

    apple: Arc<AtomicU8>,

    pub said: Vec<String>,

    pub apple_for_test: Option<bool>,
}

pub const BTN_VOICE_ON: u8 = 0;
pub const BTN_SKIP_ALL: u8 = 1;
pub const BTN_INSTALL: u8 = 2;
pub const BTN_NO_VOICE: u8 = 3;
pub const BTN_NEXT: u8 = 4;
pub const BTN_AGAIN: u8 = 5;
pub const BTN_NOT_HEARD: u8 = 6;
pub const BTN_SKIP_STEP: u8 = 7;
pub const BTN_STOP: u8 = 8;
pub const BTN_FINISH: u8 = 9;
pub const BTN_CLOSE_CARD: u8 = 10;
pub const BTN_REPLAY: u8 = 11;

impl App {
    pub fn onb_phase(&self) -> Option<Phase> {
        self.onb.on.as_ref().map(|o| o.phase)
    }

    pub fn onb_full_screen(&self) -> bool {
        self.onb_phase().is_some_and(Phase::full_screen)
    }

    pub fn voice_onboarding_due(&self, fresh_home: bool, forced: bool) -> bool {
        let quiet = self.cfg.setup_dont_show
            || crate::config::env_var("NO_SPLASH").is_some()
            || std::env::var_os(crate::test_guard::ENV).is_some()
            || !crate::setup::stdin_is_tty();
        onboarding::due(
            forced,
            fresh_home,
            onboarding::read_marker().as_deref(),
            quiet,
        )
    }

    pub fn start_voice_onboarding(&mut self, login_walk: bool) {
        crate::log::info("onboarding: the voice welcome");
        self.onb.card = false;
        self.modal = crate::app::Modal::None;
        self.onb.on = Some(Onb {
            phase: Phase::Welcome,
            phase_at: Instant::now(),
            steps: Steps::default(),
            step_at: Instant::now(),
            snap: None,
            voice: false,
            heard: None,
            queue: VecDeque::new(),
            reminded: false,
            installing: false,
            install_error: None,
            mic_heard: None,
            speaker: None,
            login_walk,
        });
        self.onb_check_apple();

        if !self.voice.muted && self.onb_needs().can_hear {
            self.onb_open_mic();
        }
        self.onb_say(onboarding::HOOK_SPOKEN);
    }

    fn onb_check_apple(&self) {
        if self.onb.apple.load(Ordering::Relaxed) != 0 || cfg!(test) {
            return;
        }
        let wants =
            cfg!(target_os = "macos") && matches!(self.cfg.voice.engine.as_str(), "auto" | "apple");
        if !wants {
            self.onb.apple.store(1, Ordering::Relaxed);
            return;
        }
        let flag = Arc::clone(&self.onb.apple);
        let _ = std::thread::Builder::new()
            .name("onb-apple".into())
            .spawn(move || {
                let ok = crate::voice::apple::AppleSpeech::available();
                flag.store(if ok { 2 } else { 1 }, Ordering::Relaxed);
            });
    }

    fn onb_apple(&self) -> bool {
        if let Some(a) = self.onb.apple_for_test {
            return a;
        }
        self.onb.apple.load(Ordering::Relaxed) == 2
    }

    pub fn onb_needs(&self) -> Needs {
        onboarding::needs(&self.deps(), self.onb_apple())
    }

    fn onb_set(&mut self, p: Phase) {
        if let Some(o) = &mut self.onb.on {
            crate::log::info(&format!("onboarding: {:?} -> {p:?}", o.phase));
            o.phase = p;
            o.phase_at = Instant::now();
        }
    }

    fn onb_open_mic(&mut self) {
        if let Some(p) = self.voice.preview.take() {
            p.stop();
        }
        if self.voice.engine.is_some() && self.voice.open_mic {
            return;
        }
        self.stop_voice();
        self.voice.open_mic = true;
        self.voice.open_mic_asleep = false;
        self.voice.asleep = false;
        self.voice.last_heard = Instant::now();
        self.start_voice(true);
    }

    pub fn onb_voice_on(&mut self) {
        let n = self.onb_needs();
        if let Some(o) = &mut self.onb.on {
            o.voice = true;
        }
        if n.install.is_empty() && n.manual.is_empty() && n.can_hear {
            return self.onb_to_mic();
        }
        self.onb_set(Phase::Install);
        if n.install.is_empty() {
            self.onb_say("Voice needs a few things I can't install here. They're on the screen. You can go on without voice.");
        } else {
            let names: Vec<&str> = n.install.iter().map(|x| x.1).collect();
            let size = if n.bytes > 0 {
                format!(", about {}", crate::deps::size(n.bytes))
            } else {
                String::new()
            };
            self.onb_say(&format!(
                "First, voice needs {}{size}. Shall I install it? Say yes or press Enter.",
                join_and(&names)
            ));
        }
    }

    pub fn onb_install(&mut self) {
        let n = self.onb_needs();
        if n.install.is_empty() {
            return;
        }
        let ids: Vec<String> = n.install.iter().map(|x| x.0.to_string()).collect();
        let say = self.run_install(&ids);
        crate::log::info(&format!("onboarding: install: {say}"));
        if let Some(o) = &mut self.onb.on {
            o.installing = true;
            o.install_error = None;
        }
        self.onb_say("Installing now. I'll say each step.");
    }

    pub fn onb_without_voice(&mut self) {
        if let Some(o) = &mut self.onb.on {
            o.voice = false;
        }
        self.onb_say("Okay, no voice for now. You can type the same requests to the assistant.");
        self.onb_to_lesson();
    }

    fn onb_to_mic(&mut self) {
        self.onb_set(Phase::Mic);
        if let Some(o) = &mut self.onb.on {
            o.mic_heard = None;
        }
        self.onb_open_mic();
        self.onb_say("Mic check. Say anything, like hello GodTerm.");
    }

    fn onb_to_speaker(&mut self) {
        self.onb_set(Phase::Speaker);
        if let Some(o) = &mut self.onb.on {
            o.speaker = None;
        }
        self.onb_speaker_sample();
    }

    fn onb_speaker_sample(&mut self) {
        let name = onboarding::voice_name(&self.cfg.voice.kokoro_voice);
        let heard = self.onb.on.as_ref().is_some_and(|o| o.mic_heard.is_some());
        let lead = if heard { "I heard you. " } else { "" };
        self.onb_say(&format!(
            "{lead}This is {name}, the voice of GodTerm. Did you hear me? Say yes, or press Enter."
        ));
    }

    pub fn onb_voice_label(&self) -> String {
        let (engine, why) = crate::voice::tts::effective_engine(&self.cfg.voice);
        match engine {
            "kokoro" => format!("Kokoro {}", self.cfg.voice.kokoro_voice),
            "off" => "talk back is off (Settings > Voice)".into(),
            e => match why {
                Some(w) => format!("{e} (Kokoro unavailable: {w})"),
                None => e.to_string(),
            },
        }
    }

    fn onb_to_lesson(&mut self) {
        self.onb_set(Phase::Lesson);
        self.view = View::Grid;
        self.open_panel_at_start();
        if !self.onb.on.as_ref().is_some_and(|o| o.voice) {
            self.assistant.focused = true;
        }
        self.onb_begin_step();
    }

    fn onb_begin_step(&mut self) {
        let Some(step) = self.onb.on.as_ref().and_then(|o| o.steps.current()) else {
            return self.onb_to_recap();
        };

        if step == StepId::Project && self.cfg.accounts.is_empty() {
            self.onb_say("There's no account yet, so I'll skip the project step.");
            if let Some(o) = &mut self.onb.on {
                o.steps.skip();
            }
            return self.onb_begin_step();
        }
        let snap = self.onb_snap();
        let n = self.onb.on.as_ref().map_or(0, |o| o.steps.cur);
        if let Some(o) = &mut self.onb.on {
            o.snap = Some(snap);
            o.step_at = Instant::now();
        }
        let typed = !self.onb.on.as_ref().is_some_and(|o| o.voice);
        let mut line = format!(
            "Step {} of {}. {}",
            n + 1,
            onboarding::STEPS.len(),
            step.intro()
        );
        if typed {
            line = line.replace("Say:", "Type:").replace("Try:", "Type:");
        }
        self.onb_say(&line);
    }

    fn onb_snap(&self) -> Snap {
        let count = |h: Harness| {
            self.cfg
                .accounts
                .iter()
                .filter(|a| a.harness() == h)
                .count()
        };
        Snap {
            claude: count(Harness::Claude),
            grok: count(Harness::Grok),
            tabs_first: self
                .panes
                .iter()
                .filter(|p| p.account == Some(0))
                .map(|p| p.tabs.len())
                .sum(),
            layout: self.layout_name(),
            zoom: self.zoom,
            view: self.view,
            turn: self.assistant_turn,
        }
    }

    pub fn onb_step_met(&self) -> bool {
        let Some(o) = &self.onb.on else { return false };
        let (Some(step), Some(s)) = (o.steps.current(), &o.snap) else {
            return false;
        };
        let now = self.onb_snap();
        let flow_of = |h: Harness| {
            self.admin.logins.iter().any(|f| {
                f.started >= o.step_at
                    && self
                        .cfg
                        .accounts
                        .get(f.account)
                        .is_some_and(|a| a.harness() == h)
            })
        };
        match step {
            StepId::Ask => now.turn > s.turn && !self.assistant.busy && !self.assistant.system_turn,
            StepId::ClaudeLogin => now.claude > s.claude || flow_of(Harness::Claude),
            StepId::Project => now.tabs_first > s.tabs_first,
            StepId::GrokLogin => now.grok > s.grok || flow_of(Harness::Grok),
            StepId::Ui => {
                now.layout != s.layout
                    || now.zoom != s.zoom
                    || (now.view != s.view
                        && matches!(
                            now.view,
                            View::Loops
                                | View::Overview
                                | View::Dashboard
                                | View::LiveMap
                                | View::Sessions
                        ))
            }
        }
    }

    fn onb_step_done(&mut self) {
        let next = self.onb.on.as_mut().and_then(|o| o.steps.done());
        crate::log::info(&format!("onboarding: step done, next {next:?}"));
        self.onb_say("Nice, that worked.");
        match next {
            Some(_) => self.onb_begin_step(),
            None => self.onb_to_recap(),
        }
    }

    pub fn onb_skip_step(&mut self) {
        let Some(o) = &mut self.onb.on else { return };
        if o.phase != Phase::Lesson {
            return;
        }
        let next = o.steps.skip();
        self.onb_say("Skipped.");
        match next {
            Some(_) => self.onb_begin_step(),
            None => self.onb_to_recap(),
        }
    }

    fn onb_back(&mut self) {
        if let Some(o) = &mut self.onb.on {
            o.steps.back();
        }
        self.onb_begin_step();
    }

    fn onb_repeat(&mut self) {
        match self.onb_phase() {
            Some(Phase::Welcome) => self.onb_say(onboarding::HOOK_SPOKEN),
            Some(Phase::Mic) => self.onb_say("Say anything, like hello GodTerm."),
            Some(Phase::Speaker) => self.onb_speaker_sample(),
            Some(Phase::Lesson) => self.onb_begin_step(),
            Some(Phase::Recap) => self.onb_say(onboarding::RECAP_SPOKEN),
            _ => {}
        }
    }

    fn onb_to_recap(&mut self) {
        self.onb_set(Phase::Recap);
        self.onb_say(onboarding::RECAP_SPOKEN);
    }

    pub fn end_voice_onboarding(&mut self, how: Outcome) {
        let Some(o) = self.onb.on.take() else { return };
        crate::log::info(&format!(
            "onboarding: ended ({}), {} done, {} skipped",
            how.word(),
            o.steps.count(Mark::Done),
            o.steps.count(Mark::Skipped)
        ));
        onboarding::write_marker(how);
        self.view = View::Grid;
        if let Some(p) = &self.voice.preview {
            p.stop();
        }

        if self.voice.engine.is_some() && self.voice.open_mic && how == Outcome::Done {
            self.voice.open_mic = false;
            self.stop_voice();
            self.start_voice(true);
        }
        match how {
            Outcome::Done => {
                self.finish_tour();
                self.flash("Voice onboarding done. Ctrl-a J shows the phrases again.");
            }
            _ => {
                self.flash("Voice skipped. Later: Ctrl-a : \"voice onboarding\".");
            }
        }

        if self.cfg.accounts.is_empty() {
            self.open_first_account();
        } else if !cfg!(test) && !self.required_missing().is_empty() {
            self.maybe_show_setup(false);
        } else if o.login_walk {
            self.start_onboarding();
        } else if how != Outcome::Done {
            self.maybe_start_tour();
        }
    }

    pub fn onb_say(&mut self, text: &str) {
        self.onb.said.push(text.to_string());
        if let Some(o) = &mut self.onb.on {
            o.queue.push_back(text.to_string());
        }
        self.onb_flush();
    }

    fn onb_quiet(&self) -> bool {
        use crate::voice::VoiceStatus;
        let talking = matches!(
            self.voice.status,
            VoiceStatus::Listening | VoiceStatus::Transcribing
        ) && self.voice.engine.is_some();
        let speaking = self.voice.engine.as_ref().is_some_and(|v| v.speaking())
            || self.voice.preview.as_ref().is_some_and(|p| p.speaking());
        !talking && !speaking && !self.assistant.busy && !self.paused()
    }

    fn onb_flush(&mut self) {
        if !self.onb_quiet() {
            return;
        }
        let Some(o) = &mut self.onb.on else { return };
        if o.queue.is_empty() {
            return;
        }

        let text: Vec<String> = o.queue.drain(..).collect();
        let text = text.join(" ");
        if self.cfg.voice.speaker_muted || !self.cfg.voice.tts {
            return;
        }
        if self.speaker_on() {
            self.told_record("onboarding", &text, crate::told::Channel::Spoken, false);
        }
        if let Some(v) = &self.voice.engine {
            if v.tts {
                v.say(&text);
                return;
            }
        }
        if self.voice.preview.is_none() && !cfg!(test) {
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.voice.preview = Some(crate::voice::tts::Speaker::start(&self.cfg.voice, flag));
        }
        if let Some(p) = &self.voice.preview {
            p.say(&text);
        }
    }

    pub fn onboarding_tick(&mut self) {
        let Some(o) = &self.onb.on else { return };
        let phase = o.phase;
        let since = o.phase_at.elapsed();
        if phase == Phase::Welcome
            && !o.reminded
            && since >= Duration::from_secs(onboarding::REMIND_AFTER_S)
        {
            if let Some(o) = &mut self.onb.on {
                o.reminded = true;
            }
            self.onb_say("Still here? Press Enter to set up voice, or Escape to skip.");
        }
        if phase == Phase::Install
            && self.onb.on.as_ref().is_some_and(|o| o.installing)
            && self.setup.running.is_empty()
        {
            self.deps_changed();
            let n = self.onb_needs();
            if let Some(o) = &mut self.onb.on {
                o.installing = false;
            }
            if n.install.is_empty() && n.manual.is_empty() && n.can_hear {
                self.onb_say("All set.");
                self.onb_to_mic();
            } else {
                let err = self
                    .admin
                    .said
                    .iter()
                    .rev()
                    .find(|s| s.contains("failed"))
                    .cloned()
                    .unwrap_or_else(|| "Some pieces are still missing.".into());
                if let Some(o) = &mut self.onb.on {
                    o.install_error = Some(err);
                }
                self.onb_say("That didn't fully work. The details are on screen. Press r to try again, or c to go on without voice.");
            }
        }
        if phase == Phase::Lesson && self.onb_step_met() {
            self.onb_step_done();
        }
        self.onb_flush();
    }

    pub fn onboarding_heard(&mut self, text: &str, norm: &str) -> bool {
        let Some(phase) = self.onb_phase() else {
            let woke = self.strip_wake(norm).map(str::to_string);
            if woke.is_none() && !self.voice.open_mic && self.voice.always_on {
                return false;
            }
            let said = woke.unwrap_or_else(|| text.to_string());
            if onboarding::is_start(&said) {
                self.start_voice_onboarding(false);
                return true;
            }
            if onboarding::is_phrases(&said) {
                self.onb.card = true;
                return true;
            }
            return false;
        };

        let said_text = self
            .strip_wake(norm)
            .filter(|r| !r.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| text.to_string());

        if crate::app_openmic::is_noise(norm) {
            return phase.full_screen();
        }
        if let Some(o) = &mut self.onb.on {
            o.heard = Some(text.trim().to_string());
        }
        let said = onboarding::classify(&said_text);
        crate::log::info(&format!("onboarding: heard ({phase:?}, {said:?}): {text}"));
        match phase {
            Phase::Welcome => match said {
                Said::Yes => self.onb_voice_on(),
                Said::No | Said::Skip | Said::Stop => self.end_voice_onboarding(Outcome::Skipped),
                Said::Repeat => self.onb_repeat(),
                _ => {}
            },
            Phase::Install => match said {
                Said::Yes if !self.onb.on.as_ref().is_some_and(|o| o.installing) => {
                    if self.onb_needs().install.is_empty() {
                        self.onb_without_voice();
                    } else {
                        self.onb_install();
                    }
                }
                Said::No | Said::Skip | Said::Next => self.onb_without_voice(),
                Said::Stop => self.end_voice_onboarding(Outcome::Skipped),
                _ => {}
            },
            Phase::Mic => match said {
                Said::Stop => self.end_voice_onboarding(Outcome::Stopped),
                Said::Skip | Said::Next => self.onb_to_speaker(),
                _ => {
                    if let Some(o) = &mut self.onb.on {
                        o.mic_heard = Some(text.trim().to_string());
                    }
                    self.onb_to_speaker();
                }
            },
            Phase::Speaker => match said {
                Said::Yes | Said::Next | Said::Skip => self.onb_speaker_ok(true),
                Said::No => self.onb_speaker_ok(false),
                Said::Repeat => self.onb_speaker_sample(),
                Said::Stop => self.end_voice_onboarding(Outcome::Stopped),
                _ => {}
            },
            Phase::Lesson => match said {
                Said::Stop => self.end_voice_onboarding(Outcome::Stopped),
                Said::Skip | Said::Next => self.onb_skip_step(),
                Said::Back => self.onb_back(),
                Said::Repeat => self.onb_repeat(),

                _ => return false,
            },
            Phase::Recap => match said {
                Said::Yes | Said::Next | Said::Skip | Said::Stop | Said::No => {
                    self.end_voice_onboarding(Outcome::Done)
                }
                Said::Repeat => self.onb_repeat(),
                _ => return false,
            },
        }
        true
    }

    pub fn onboarding_typed(&mut self, text: &str) -> bool {
        match self.onb_phase() {
            Some(Phase::Lesson | Phase::Recap) => {
                let n = crate::voice::grammar::normalize(text);
                matches!(
                    onboarding::classify(text),
                    Said::Stop | Said::Skip | Said::Next | Said::Back | Said::Repeat
                ) && self.onboarding_heard(text, &n)
            }
            None if onboarding::is_start(text) => {
                self.start_voice_onboarding(false);
                true
            }
            _ => false,
        }
    }

    fn onb_speaker_ok(&mut self, ok: bool) {
        let first = self.onb.on.as_ref().is_some_and(|o| o.speaker.is_none());
        if let Some(o) = &mut self.onb.on {
            o.speaker = Some(ok);
        }
        if !ok && first {
            self.onb_say("No sound? Turn the volume up or pick the output in Settings, Voice. Press Enter to go on.");
            return;
        }
        self.onb_to_lesson();
    }

    pub fn onboarding_key(&mut self, k: &KeyEvent) -> bool {
        if self.onb.card && self.onb.on.is_none() {
            if matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                self.onb.card = false;
            }
            return true;
        }
        let Some(phase) = self.onb_phase() else {
            return false;
        };

        let ctrl_a = k.code == KeyCode::Char('a') && k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_a || self.prefix {
            return false;
        }
        let c = match k.code {
            KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        let enter = k.code == KeyCode::Enter;
        let esc = k.code == KeyCode::Esc;
        match phase {
            Phase::Welcome => {
                if enter || c == Some('y') {
                    self.onb_voice_on();
                } else if esc || matches!(c, Some('s' | 'n' | 'q')) {
                    self.end_voice_onboarding(Outcome::Skipped);
                }
            }
            Phase::Install => {
                let installing = self.onb.on.as_ref().is_some_and(|o| o.installing);
                if (enter || c == Some('y') || c == Some('r')) && !installing {
                    if self.onb_needs().install.is_empty() {
                        self.onb_without_voice();
                    } else {
                        self.onb_install();
                    }
                } else if c == Some('c') {
                    self.onb_without_voice();
                } else if esc {
                    self.end_voice_onboarding(Outcome::Skipped);
                }
            }
            Phase::Mic => {
                if enter {
                    self.onb_to_speaker();
                } else if c == Some('r') {
                    self.stop_voice();
                    self.onb_to_mic();
                } else if c == Some('c') {
                    self.onb_without_voice();
                } else if esc {
                    self.end_voice_onboarding(Outcome::Stopped);
                }
            }
            Phase::Speaker => {
                if enter || c == Some('y') {
                    self.onb_speaker_ok(true);
                } else if c == Some('r') {
                    self.onb_speaker_sample();
                } else if c == Some('n') {
                    self.onb_speaker_ok(false);
                } else if c == Some('c') {
                    self.onb_without_voice();
                } else if esc {
                    self.end_voice_onboarding(Outcome::Stopped);
                }
            }

            Phase::Lesson => return false,
            Phase::Recap => {
                if enter || esc || c == Some('q') {
                    self.end_voice_onboarding(Outcome::Done);
                } else if c == Some('r') {
                    self.onb_repeat();
                } else {
                    return false;
                }
            }
        }
        true
    }

    pub fn onboarding_mouse(&mut self, m: &MouseEvent) -> bool {
        if !(self.onb_full_screen() || self.onb.card && self.onb.on.is_none()) {
            return false;
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            let hit = self
                .last_hits
                .regions
                .iter()
                .rev()
                .find(|r| crate::hits::contains(r.rect, m.column, m.row))
                .map(|r| r.action.clone());
            if let Some(crate::hits::UiAction::Onboarding(n)) = hit {
                self.onboarding_click(n);
            }
        }
        if matches!(m.kind, MouseEventKind::Moved) {
            self.mouse_pos = Some((m.column, m.row));
        }
        true
    }

    #[allow(clippy::collapsible_match)]
    pub fn onboarding_click(&mut self, n: u8) {
        match n {
            BTN_VOICE_ON => self.onb_voice_on(),
            BTN_SKIP_ALL => {
                let how = if self.onb_phase() == Some(Phase::Welcome) {
                    Outcome::Skipped
                } else {
                    Outcome::Stopped
                };
                self.end_voice_onboarding(how)
            }
            BTN_INSTALL => {
                if !self.onb.on.as_ref().is_some_and(|o| o.installing) {
                    self.onb_install();
                }
            }
            BTN_NO_VOICE => self.onb_without_voice(),
            BTN_NEXT => match self.onb_phase() {
                Some(Phase::Mic) => self.onb_to_speaker(),
                Some(Phase::Speaker) => self.onb_speaker_ok(true),
                _ => {}
            },
            BTN_AGAIN => match self.onb_phase() {
                Some(Phase::Mic) => {
                    self.stop_voice();
                    self.onb_to_mic();
                }
                _ => self.onb_repeat(),
            },
            BTN_NOT_HEARD => self.onb_speaker_ok(false),
            BTN_SKIP_STEP => self.onb_skip_step(),
            BTN_STOP => self.end_voice_onboarding(Outcome::Stopped),
            BTN_FINISH => self.end_voice_onboarding(Outcome::Done),
            BTN_CLOSE_CARD => self.onb.card = false,
            BTN_REPLAY => self.start_voice_onboarding(false),
            _ => {}
        }
    }

    pub fn onboarding_ctrl_j(&mut self) {
        match self.onb_phase() {
            Some(Phase::Lesson) => self.onb_skip_step(),
            Some(Phase::Recap) => self.end_voice_onboarding(Outcome::Done),
            Some(_) => {}
            None => self.onb.card = !self.onb.card,
        }
    }

    pub fn onboarding_turn_note(&self) -> Option<String> {
        let o = self.onb.on.as_ref()?;
        if o.phase != Phase::Lesson {
            return None;
        }
        let step = o.steps.current()?;
        Some(format!(
            "(Voice onboarding is on, step '{}': {}. The user is learning GodTerm; do what they ask with your real tools and say in one short sentence what happened. Adding an account or starting a login goes through needs_confirmation: ask, and act only on their clear yes in a later message. Never say or show a secret, token or login code.)\n",
            step.title(),
            step.for_assistant()
        ))
    }
}

fn join_and(v: &[&str]) -> String {
    match v.len() {
        0 => String::new(),
        1 => v[0].to_string(),
        n => format!("{} and {}", v[..n - 1].join(", "), v[n - 1]),
    }
}
