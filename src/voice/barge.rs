//! Barge-in on words, not noise. Loud sound over our reply (the echo
//! gate's trigger) only starts a decision: the reply is ducked, the
//! overlapping audio goes to the fast ASR, and the reply stops only when
//! that holds real speech: a command word ("stop", "wait", ...) at once,
//! or `min_words` real words. A vacuum, a cough, a clap, whisper's stock
//! hallucinations or our own voice echoed back restore the volume, and
//! each such false alarm raises the bar for the next one (a rolling noise
//! floor plus a margin that decays), so sustained noise stops retriggering.
//!
//! Pure logic on millisecond timestamps, so it is tested without a mic.

/// Words that stop the reply on their own, as soon as a partial has them.
const COMMANDS: &[&str] = &["stop", "wait", "cancel", "pause", "no", "enough", "quiet"];
/// Two word commands.
const COMMAND_PAIRS: &[(&str, &str)] = &[("hey", "god"), ("shut", "up"), ("hold", "on")];
/// Not words: hesitations and backchannel noises.
const FILLERS: &[&str] = &[
    "um", "umm", "uh", "uhh", "uhm", "hmm", "hm", "mm", "mmm", "mhm", "ah", "ahh", "oh", "er",
    "erm", "eh", "huh", "uh-huh", "mm-hmm", "ha", "haha", "hah",
];
/// What whisper writes for noise or silence (whole transcript).
const HALLUCINATIONS: &[&str] = &[
    "you",
    "thank you",
    "thanks",
    "thank you very much",
    "thank you so much",
    "thanks for watching",
    "thank you for watching",
    "thanks for listening",
    "bye",
    "bye bye",
    "goodbye",
    "blank audio",
    "silence",
    "music",
    "applause",
    "laughter",
    "please subscribe",
    "subtitles by the amara org community",
];

/// Settings for the decision.
#[derive(Debug, Clone, Copy)]
pub struct BargeCfg {
    pub enabled: bool,
    /// Real words (fillers and repeats not counted) that stop the reply.
    pub min_words: usize,
    /// Duck the reply while deciding instead of letting it play on.
    pub duck: bool,
    /// Volume while ducked (0 to 1).
    pub duck_level: f32,
    /// Decide by then (ms after the trigger) when no words came.
    pub decide_ms: u64,
    /// A transcript still on its way may hold the decision this long.
    pub max_wait_ms: u64,
}

impl BargeCfg {
    pub fn new(enabled: bool, min_words: usize, duck: bool) -> BargeCfg {
        BargeCfg {
            enabled,
            min_words: min_words.max(1),
            duck,
            duck_level: 0.4,
            decide_ms: 700,
            max_wait_ms: 1100,
        }
    }
}

/// What a transcript of the overlapping audio holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// A command word: stop now.
    Command(String),
    /// This many real words.
    Words(usize),
    /// Our own reply coming back through the mic.
    Echo,
    /// Nothing that is speech (empty, a sound tag, a hallucination,
    /// fillers, or audio whisper itself doubts).
    Noise(&'static str),
}

/// Lowercase words of `text`, without bracketed or starred sound tags
/// ("[BLANK_AUDIO]", "(coughs)", "*clap*") and punctuation.
pub fn words(text: &str) -> Vec<String> {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0i32;
    let mut star = false;
    for c in text.chars() {
        match c {
            '[' | '(' | '{' | '<' => depth += 1,
            ']' | ')' | '}' | '>' => depth = (depth - 1).max(0),
            '*' | '♪' => star = !star,
            _ if depth > 0 || star => {}
            c if c.is_alphanumeric() || c == '\'' || c == '-' => out.extend(c.to_lowercase()),
            _ => out.push(' '),
        }
    }
    out.split_whitespace()
        .map(|w| w.trim_matches(|c| c == '\'' || c == '-').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Classify one transcript of the audio over our reply. `said`: what we
/// were speaking around then (the echo check). `no_speech` and `logprob`:
/// whisper's own doubt, when known.
pub fn classify(
    text: &str,
    said: &[String],
    no_speech: Option<f32>,
    logprob: Option<f32>,
) -> Heard {
    let w = words(text);
    if w.is_empty() {
        return Heard::Noise(if text.trim().is_empty() {
            "empty"
        } else {
            "sound tag"
        });
    }
    let joined = w.join(" ");
    if HALLUCINATIONS.contains(&joined.as_str()) {
        return Heard::Noise("hallucination");
    }
    let doubtful = no_speech.is_some_and(|n| n > 0.6) || logprob.is_some_and(|l| l < -1.0);
    // Our own words: most of what was heard is in the sentence we are saying.
    let ours: std::collections::HashSet<String> = said.iter().flat_map(|s| words(s)).collect();
    let real: Vec<&String> = w
        .iter()
        .filter(|x| !FILLERS.contains(&x.as_str()))
        .collect();
    let echoed = !real.is_empty() && {
        let hits = real.iter().filter(|x| ours.contains(x.as_str())).count();
        hits * 10 >= real.len() * 7
    };
    if echoed {
        return Heard::Echo;
    }
    if !doubtful {
        for (i, x) in w.iter().enumerate() {
            if COMMANDS.contains(&x.as_str()) {
                return Heard::Command(x.clone());
            }
            if let Some(n) = w.get(i + 1) {
                if COMMAND_PAIRS.iter().any(|(a, b)| a == x && b == n) {
                    return Heard::Command(format!("{x} {n}"));
                }
            }
        }
    }
    if real.is_empty() {
        return Heard::Noise("filler");
    }
    if doubtful {
        return Heard::Noise("unclear");
    }
    // Repeats do not count ("the the the" is one word).
    let mut distinct: Vec<&String> = vec![];
    for x in real {
        if !distinct.contains(&x) {
            distinct.push(x);
        }
    }
    Heard::Words(distinct.len())
}

/// The outcome of one trigger.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// Stop the reply (the user is talking) or let it go on (noise).
    pub stop: bool,
    pub reason: String,
    /// The transcript the decision was made on (may be empty).
    pub text: String,
    /// ms from the trigger to the decision.
    pub after_ms: u64,
    /// Let go because the words were our own reply coming back.
    pub echo: bool,
}

/// What to do right after a trigger.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Start {
    /// Barge-in is off: nothing happens.
    Off,
    /// A false alarm just before: ignore this one too.
    Cooldown,
    /// Decide on the transcript; duck the reply to `duck` meanwhile.
    Decide { duck: f32 },
}

struct Pending {
    since: u64,
    last_text: String,
    last: Option<Heard>,
}

/// Rolling noise floor (dBFS) of the mic and an extra margin raised by
/// false alarms; together they lift the barge-in bar in a noisy room.
#[derive(Debug, Clone)]
pub struct NoiseFloor {
    pub floor_db: f32,
    /// Added to the bar after false alarms; decays back to 0.
    pub extra_db: f32,
    last_ms: u64,
    frames: u32,
}

/// The bar sits this far over the rolling floor.
pub const FLOOR_MARGIN_DB: f32 = 10.0;
/// Each false alarm adds this much.
pub const FALSE_ALARM_DB: f32 = 4.0;
pub const MAX_EXTRA_DB: f32 = 16.0;
/// The extra margin decays this many dB per second.
pub const EXTRA_DECAY_DB_S: f32 = 0.5;

impl Default for NoiseFloor {
    fn default() -> Self {
        NoiseFloor {
            floor_db: -70.0,
            extra_db: 0.0,
            last_ms: 0,
            frames: 0,
        }
    }
}

impl NoiseFloor {
    /// One mic frame. The floor falls quickly to quiet frames and rises
    /// slowly (a vacuum lifts it over a few seconds; speech barely does).
    pub fn push(&mut self, level_db: f32, now_ms: u64) {
        let l = level_db.clamp(-100.0, 0.0);
        self.frames = self.frames.saturating_add(1);
        let rate = if self.frames < 15 {
            0.3
        } else if l < self.floor_db + 1.0 {
            0.1
        } else {
            0.01
        };
        self.floor_db += (l - self.floor_db) * rate;
        let dt = now_ms.saturating_sub(self.last_ms) as f32 / 1000.0;
        self.last_ms = now_ms;
        self.extra_db = (self.extra_db - dt * EXTRA_DECAY_DB_S).max(0.0);
    }

    /// Time passing on a frame that must not move the floor (our own reply
    /// coming back from a browser's speakers is no room noise): only the
    /// extra margin decays.
    pub fn hold(&mut self, now_ms: u64) {
        let dt = now_ms.saturating_sub(self.last_ms) as f32 / 1000.0;
        self.last_ms = now_ms;
        self.extra_db = (self.extra_db - dt * EXTRA_DECAY_DB_S).max(0.0);
    }

    /// A trigger turned out to be noise.
    pub fn false_alarm(&mut self) {
        self.extra_db = (self.extra_db + FALSE_ALARM_DB).min(MAX_EXTRA_DB);
    }

    /// The lowest level that may count as the user over our reply.
    pub fn bar_db(&self) -> f32 {
        self.floor_db + FLOOR_MARGIN_DB + self.extra_db
    }
}

/// Decides each trigger on the words heard over the reply.
pub struct Decider {
    pub cfg: BargeCfg,
    pending: Option<Pending>,
    pub noise: NoiseFloor,
    /// No new decision before then (ms) after a false alarm.
    quiet_until: u64,
}

/// After a false alarm, triggers are ignored this long.
pub const COOLDOWN_MS: u64 = 600;

impl Decider {
    pub fn new(cfg: BargeCfg) -> Decider {
        Decider {
            cfg,
            pending: None,
            noise: NoiseFloor::default(),
            quiet_until: 0,
        }
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The output volume factor right now (ducked while deciding).
    pub fn duck(&self) -> f32 {
        if self.pending.is_some() && self.cfg.duck {
            self.cfg.duck_level
        } else {
            1.0
        }
    }

    /// The echo gate fired (sustained loud sound over our reply).
    pub fn trigger(&mut self, now_ms: u64) -> Start {
        if !self.cfg.enabled {
            return Start::Off;
        }
        if self.pending.is_some() {
            return Start::Decide { duck: self.duck() };
        }
        if now_ms < self.quiet_until {
            return Start::Cooldown;
        }
        self.pending = Some(Pending {
            since: now_ms,
            last_text: String::new(),
            last: None,
        });
        Start::Decide { duck: self.duck() }
    }

    /// A transcript of the overlapping audio so far. Stops at once on a
    /// command or enough words; otherwise waits for more audio (or the
    /// deadline, see `tick`).
    pub fn transcript(
        &mut self,
        now_ms: u64,
        text: &str,
        said: &[String],
        no_speech: Option<f32>,
        logprob: Option<f32>,
    ) -> Option<Decision> {
        let p = self.pending.as_mut()?;
        let h = classify(text, said, no_speech, logprob);
        p.last_text = text.trim().to_string();
        let reason = match &h {
            Heard::Command(c) => Some(format!("command \"{c}\"")),
            Heard::Words(n) if *n >= self.cfg.min_words => Some(format!("{n} words")),
            _ => None,
        };
        p.last = Some(h);
        let reason = reason?;
        Some(self.finish(now_ms, true, reason))
    }

    /// Time passing: past the deadline with no words, it was noise.
    /// `in_flight`: a transcript is on its way (worth a short wait).
    pub fn tick(&mut self, now_ms: u64, in_flight: bool) -> Option<Decision> {
        let p = self.pending.as_ref()?;
        let age = now_ms.saturating_sub(p.since);
        if age < self.cfg.decide_ms || (in_flight && age < self.cfg.max_wait_ms) {
            return None;
        }
        let reason = match &p.last {
            None => "no transcript in time".to_string(),
            Some(Heard::Noise(why)) => format!("noise ({why})"),
            Some(Heard::Echo) => "our own words echoed".to_string(),
            Some(Heard::Words(n)) => format!("only {n} word{}", if *n == 1 { "" } else { "s" }),
            Some(Heard::Command(_)) => "command".to_string(),
        };
        Some(self.finish(now_ms, false, reason))
    }

    /// Drop the decision (we stopped talking for another reason).
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    fn finish(&mut self, now_ms: u64, stop: bool, reason: String) -> Decision {
        let p = self.pending.take().expect("pending");
        if !stop {
            self.noise.false_alarm();
            self.quiet_until = now_ms + COOLDOWN_MS;
        }
        Decision {
            stop,
            reason,
            text: p.last_text,
            after_ms: now_ms.saturating_sub(p.since),
            echo: !stop && p.last == Some(Heard::Echo),
        }
    }

    /// One log line for a decision.
    pub fn log_line(d: &Decision) -> String {
        format!(
            "voice: barge-in {} after {} ms ({}): {:?}",
            if d.stop { "accepted" } else { "ignored" },
            d.after_ms,
            d.reason,
            d.text
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec() -> Decider {
        Decider::new(BargeCfg::new(true, 2, true))
    }

    fn said() -> Vec<String> {
        vec!["The build finished and all forty tests passed on the main branch.".into()]
    }

    #[test]
    fn noise_burst_with_empty_transcript_does_not_stop() {
        let mut d = dec();
        assert_eq!(d.trigger(0), Start::Decide { duck: 0.4 });
        assert_eq!(d.transcript(300, "", &said(), None, None), None);
        assert_eq!(d.tick(500, false), None);
        let r = d.tick(700, false).expect("decided");
        assert!(!r.stop, "{r:?}");
        assert!(r.reason.contains("empty"), "{r:?}");
    }

    #[test]
    fn no_transcript_at_all_is_noise() {
        let mut d = dec();
        d.trigger(0);
        // One is still on its way: wait a little longer, then give up.
        assert_eq!(d.tick(800, true), None);
        let r = d.tick(1100, true).expect("decided");
        assert!(!r.stop && r.reason.contains("no transcript"), "{r:?}");
    }

    #[test]
    fn sound_tags_and_hallucinations_do_not_stop() {
        for t in [
            "(coughs)",
            "[BLANK_AUDIO]",
            "[Music]",
            "*clapping*",
            " Thank you.",
            "you",
            ".",
            "♪ ♪",
        ] {
            let mut d = dec();
            d.trigger(0);
            assert_eq!(d.transcript(250, t, &said(), None, None), None, "{t}");
            let r = d.tick(700, false).expect("decided");
            assert!(!r.stop, "{t}: {r:?}");
        }
    }

    #[test]
    fn one_filler_or_repeated_filler_does_not_stop() {
        for t in ["Um.", "uh uh uh", "Hmm, hmm.", "Okay."] {
            let mut d = dec();
            d.trigger(0);
            assert_eq!(d.transcript(300, t, &said(), None, None), None, "{t}");
            assert!(!d.tick(700, false).unwrap().stop, "{t}");
        }
        // A repeated word is one word.
        assert_eq!(classify("the the the", &[], None, None), Heard::Words(1));
    }

    #[test]
    fn stop_stops_immediately() {
        let mut d = dec();
        d.trigger(0);
        let r = d
            .transcript(180, "Stop.", &said(), None, None)
            .expect("now");
        assert!(r.stop && r.after_ms == 180, "{r:?}");
        assert!(r.reason.contains("stop"));
        assert_eq!(d.duck(), 1.0);
        for t in ["wait", "Cancel that", "hey God", "no no", "pause"] {
            let mut d = dec();
            d.trigger(0);
            assert!(
                d.transcript(200, t, &said(), None, None).unwrap().stop,
                "{t}"
            );
        }
    }

    #[test]
    fn two_real_words_stop() {
        let mut d = dec();
        d.trigger(0);
        assert_eq!(d.transcript(250, "Actually", &said(), None, None), None);
        let r = d
            .transcript(450, "Actually, open", &said(), None, None)
            .expect("two words");
        assert!(r.stop && r.reason == "2 words", "{r:?}");
        // min_words is a setting.
        let mut d = Decider::new(BargeCfg::new(true, 3, true));
        d.trigger(0);
        assert_eq!(d.transcript(300, "open it", &said(), None, None), None);
        assert!(
            d.transcript(500, "open it now", &said(), None, None)
                .unwrap()
                .stop
        );
    }

    #[test]
    fn whisper_doubt_is_noise() {
        assert_eq!(
            classify("open the door", &[], Some(0.9), None),
            Heard::Noise("unclear")
        );
        assert_eq!(
            classify("open the door", &[], None, Some(-1.6)),
            Heard::Noise("unclear")
        );
        assert_eq!(
            classify("open the door", &[], Some(0.1), Some(-0.3)),
            Heard::Words(3)
        );
    }

    #[test]
    fn our_own_sentence_echoed_does_not_stop() {
        let mut d = dec();
        d.trigger(0);
        assert_eq!(
            d.transcript(400, "all forty tests passed on the", &said(), None, None),
            None
        );
        let r = d.tick(700, false).unwrap();
        assert!(!r.stop && r.reason.contains("echo"), "{r:?}");
        // A command we are saying ourselves does not count either.
        let s = vec!["Say stop when you want me to stop.".to_string()];
        assert_eq!(classify("stop", &s, None, None), Heard::Echo);
        // The user's own words over it do.
        assert_eq!(
            classify("open my calendar", &said(), None, None),
            Heard::Words(3)
        );
    }

    #[test]
    fn sustained_noise_raises_the_floor_and_stops_retriggering() {
        let mut d = dec();
        // A quiet room.
        for i in 0..100 {
            d.noise.push(-60.0, i * 30);
        }
        let quiet_bar = d.noise.bar_db();
        assert!(quiet_bar < -45.0, "{quiet_bar}");
        // A vacuum starts at -30 dBFS and keeps going.
        let mut t = 3000;
        let mut triggers = 0;
        let mut stops = 0;
        for _ in 0..400 {
            t += 30;
            d.noise.push(-30.0, t);
            // The gate fires only while the vacuum is over the bar.
            if -30.0 > d.noise.bar_db() && !d.pending() {
                if let Start::Decide { .. } = d.trigger(t) {
                    triggers += 1;
                    d.transcript(t + 300, "", &said(), None, None);
                }
            }
            if let Some(r) = d.tick(t, false) {
                stops += r.stop as u32;
            }
        }
        assert_eq!(stops, 0);
        assert!(triggers <= 3, "{triggers} triggers");
        assert!(d.noise.bar_db() > -30.0, "bar {}", d.noise.bar_db());
        // A false alarm raised the margin and starts a cooldown.
        assert!(d.noise.extra_db > 0.0);
    }

    #[test]
    fn false_alarm_margin_decays() {
        let mut n = NoiseFloor::default();
        n.push(-60.0, 0);
        n.false_alarm();
        n.false_alarm();
        assert_eq!(n.extra_db, 2.0 * FALSE_ALARM_DB);
        for i in 1..=600 {
            n.push(-60.0, i * 30);
        }
        assert_eq!(n.extra_db, 0.0);
    }

    #[test]
    fn duck_is_restored_after_noise() {
        let mut d = dec();
        assert_eq!(d.duck(), 1.0);
        d.trigger(0);
        assert_eq!(d.duck(), 0.4);
        d.transcript(300, "(coughs)", &said(), None, None);
        assert_eq!(d.duck(), 0.4);
        let r = d.tick(700, false).unwrap();
        assert!(!r.stop);
        assert_eq!(d.duck(), 1.0);
        // Just after, another burst is ignored (cooldown), and not ducked.
        assert_eq!(d.trigger(900), Start::Cooldown);
        assert_eq!(d.duck(), 1.0);
        assert!(matches!(d.trigger(1400), Start::Decide { .. }));
        // Without ducking, the volume never changes.
        let mut d = Decider::new(BargeCfg::new(true, 2, false));
        assert_eq!(d.trigger(0), Start::Decide { duck: 1.0 });
    }

    #[test]
    fn echo_held_out_of_the_noise_floor() {
        // A quiet room, then a reply at -25 dBFS coming back for 5 s.
        let mut lifted = NoiseFloor::default();
        let mut held = NoiseFloor::default();
        let mut t = 0;
        for _ in 0..100 {
            t += 30;
            lifted.push(-70.0, t);
            held.push(-70.0, t);
        }
        held.false_alarm();
        let extra = held.extra_db;
        for _ in 0..170 {
            t += 30;
            lifted.push(-25.0, t);
            held.hold(t);
        }
        // Learned from, the echo lifts the bar towards itself: the user
        // talking over it would have to be far louder.
        assert!(lifted.bar_db() > -40.0, "{}", lifted.bar_db());
        // Held out, the floor stays the room's and only the extra decays.
        assert!((held.floor_db - -70.0).abs() < 1.0, "{}", held.floor_db);
        assert!(
            held.extra_db < extra && held.extra_db > 0.0,
            "{}",
            held.extra_db
        );
    }

    #[test]
    fn barge_in_off_never_stops() {
        let mut d = Decider::new(BargeCfg::new(false, 2, true));
        assert_eq!(d.trigger(0), Start::Off);
        assert_eq!(d.transcript(100, "stop", &[], None, None), None);
        assert_eq!(d.tick(5000, false), None);
        assert_eq!(d.duck(), 1.0);
    }

    #[test]
    fn words_strip_tags() {
        assert_eq!(words("[BLANK_AUDIO] Hey, (coughs) wait!"), ["hey", "wait"]);
        assert_eq!(words("It's *laughs* fine"), ["it's", "fine"]);
        assert!(words(" . ... ").is_empty());
    }

    /// Fixtures made once with tests/fixtures/barge/make.sh: pink noise
    /// and a `say` sentence. Run through the echo gate as if we were
    /// talking: the gate may fire on either (energy alone); the transcript
    /// decides, and noise (no words) never stops the reply.
    #[test]
    fn wav_fixtures_through_the_gate() {
        fn pcm(bytes: &[u8]) -> Vec<i16> {
            // 16 kHz mono s16 WAV: the samples follow the "data" chunk header.
            let at = bytes.windows(4).position(|w| w == b"data").expect("data") + 8;
            bytes[at..]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect()
        }
        use super::super::audio;
        let run = |samples: &[i16], text: &str| -> (bool, Option<Decision>) {
            let mut gate = audio::EchoGate::new(true, 15.0);
            let mut d = dec();
            let mut fired = false;
            let mut out = None;
            for (i, f) in samples.chunks(audio::FRAME).enumerate() {
                let now = i as u64 * audio::FRAME_MS as u64;
                let l = audio::db(audio::rms(f));
                d.noise.push(l, now);
                gate.noise_bar_db = d.noise.bar_db();
                if !d.pending() && gate.push(l, -50.0, true, None) == audio::Gate::BargeIn {
                    fired = true;
                    d.trigger(now);
                    out = d.transcript(now + 300, text, &said(), None, None);
                }
                if let Some(r) = d.tick(now, false) {
                    out = Some(r);
                }
                if out.as_ref().is_some_and(|r| r.stop) {
                    break;
                }
            }
            (fired, out)
        };
        let noise = pcm(include_bytes!("../../tests/fixtures/barge/pink_noise.wav"));
        let speech = pcm(include_bytes!("../../tests/fixtures/barge/speech.wav"));
        assert!(noise.len() > 16_000 && speech.len() > 8_000);
        let (_, r) = run(&noise, "");
        assert!(r.is_none_or(|r| !r.stop), "noise stopped the reply");
        let (fired, r) = run(&speech, "Wait, open the other tab");
        assert!(fired, "speech never reached the gate");
        assert!(r.is_some_and(|r| r.stop));
    }
}
