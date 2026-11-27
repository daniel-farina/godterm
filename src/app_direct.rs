use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_assistant::{Entry, Who};
use crate::control::tab_id;
use crate::pane::Activity;

const CONFIRM_TTL: Duration = Duration::from_secs(120);

const REPLY_WAIT: Duration = Duration::from_secs(4);

const FULL_REPLY: usize = 40;

const SPOKEN_KEPT: usize = 200;

pub const FOLLOW_SETTLE: Duration = Duration::from_millis(700);

pub const SPOKEN_QUIET: Duration = Duration::from_millis(1200);

pub const SPOKEN_MAX: Duration = Duration::from_secs(4);

const STEER_SETTLE: Duration = Duration::from_millis(400);

const TYPING_HOLD: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Now,
    Steer,
    Hold(&'static str),
}

pub fn join_spoken(parts: &[String]) -> String {
    let parts: Vec<&str> = parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return parts.first().map(|p| p.to_string()).unwrap_or_default();
    }
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(p);
        let closed = p
            .chars()
            .last()
            .is_some_and(|c| matches!(c, '.' | '!' | '?' | ',' | ';' | ':' | '\u{2026}'));
        if !closed {
            out.push('.');
        }
    }
    out
}

fn shown_activity(h: crate::harness::Harness, screen: &str) -> Activity {
    crate::prompt_cli::activity_for(h, screen)
        .unwrap_or_else(|| crate::pane::detect_activity(screen))
}

pub(crate) fn input_line(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let k = lines.len().saturating_sub(8);
    lines[k..].iter().any(|l| {
        let t = l.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '\u{2502}' | '|'));
        t.starts_with('>') || t.starts_with('\u{276f}')
    })
}

#[derive(Default)]
pub struct DirectState {
    pub on: Option<DirectMode>,

    pub said: Vec<String>,

    pub spoken: Vec<u64>,

    pub picked: Option<(u64, Instant)>,

    pub shown: Option<u64>,

    pub held: bool,

    pub hold: Vec<(u64, &'static str)>,
}

pub struct DirectMode {
    pub uid: u64,

    pub name: String,
    pub since: chrono::DateTime<chrono::Local>,

    pub log: Vec<(bool, String)>,

    pub last_act: Activity,

    pub last_reply: Option<String>,

    pub reply_wait: Option<Instant>,

    pub last_sent: Option<String>,

    pub confirm: Option<DirectConfirm>,
}

#[derive(Clone)]
pub struct DirectConfirm {
    pub plan: Value,
    pub choice: &'static str,
    pub at: Instant,
}

pub(crate) fn tab_can_direct(tab: &crate::pane::Pane) -> bool {
    (tab.suspended || tab.pending.is_some() || tab.is_running()) && tab.activity != Activity::Exited
}

pub fn mic_state(app: &App) -> Option<&'static str> {
    if app.voice.muted {
        Some("muted")
    } else if app.voice.status == crate::voice::VoiceStatus::Listening {
        Some("listening")
    } else {
        None
    }
}

pub(crate) fn core_words(norm: &str) -> Vec<String> {
    let mut w: Vec<String> = norm
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    const LEAD: &[&str] = &[
        "okay", "ok", "alright", "all", "right", "now", "so", "please", "and", "well", "um", "uh",
    ];
    const TAIL: &[&str] = &["please", "now", "thanks", "thank", "you", "again"];
    while w.first().is_some_and(|x| LEAD.contains(&x.as_str())) {
        w.remove(0);
    }
    while w.last().is_some_and(|x| TAIL.contains(&x.as_str())) {
        w.pop();
    }
    w
}

pub fn is_exit_phrase(norm: &str) -> bool {
    let w = core_words(norm);
    if w.is_empty() || w.len() > 9 {
        return false;
    }
    let s = w.join(" ");
    const EXACT: &[&str] = &[
        "hand me back",
        "take me back",
        "exit direct mode",
        "end direct mode",
        "leave direct mode",
        "stop direct mode",
        "quit direct mode",
        "direct mode off",
        "back to god",
        "back to godterm",
        "back to god term",
    ];
    if EXACT.contains(&s.as_str()) {
        return true;
    }
    let has = |x: &str| w.iter().any(|y| y == x);
    let n = w.len();

    let to_assistant = w[n - 1] == "assistant"
        && n >= 2
        && matches!(w[n - 2].as_str(), "the" | "main" | "global" | "my");
    let go = has("back") || has("hand") || has("return") || has("switch") || has("over");
    if to_assistant && go {
        return true;
    }

    let direct = n >= 2 && w[n - 2] == "direct" && w[n - 1] == "mode";
    direct && (has("exit") || has("end") || has("leave") || has("stop") || has("quit"))
}

pub fn switch_target(norm: &str) -> Option<String> {
    let w = core_words(norm);
    let s = w.join(" ");
    const PREFIX: &[&str] = &[
        "let me talk directly to ",
        "let me talk to ",
        "talk directly to ",
        "talk to ",
        "put me through to ",
        "hand me to ",
        "hand me over to ",
        "switch me to ",
    ];
    let rest = PREFIX.iter().find_map(|p| s.strip_prefix(p))?;
    let rest = rest
        .trim_start_matches("the ")
        .trim_end_matches(" directly")
        .trim_end_matches(" tab")
        .trim_end_matches(" agent")
        .trim();

    let first = rest.split(' ').next().unwrap_or("");
    if rest.is_empty()
        || rest.split(' ').count() > 5
        || matches!(
            first,
            "it" | "me" | "you" | "them" | "him" | "her" | "us" | "that" | "this" | "there" | "all"
        )
    {
        return None;
    }
    Some(rest.to_string())
}

fn clean_inline(line: &str) -> String {
    let mut s = line.replace("**", "").replace("__", "").replace('`', "");

    while let Some(a) = s.find("](") {
        let Some(open) = s[..a].rfind('[') else {
            break;
        };
        let Some(close) = s[a..].find(')') else {
            break;
        };
        let text = s[open + 1..a].to_string();
        s.replace_range(open..a + close + 1, &text);
    }
    s.split(' ')
        .map(|w| {
            if w.starts_with("http://") || w.starts_with("https://") {
                "a link"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn speakable(text: &str) -> String {
    let mut out: Vec<String> = vec![];
    let mut in_code = false;
    let mut in_table = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            if !in_code && out.last().map(String::as_str) != Some("It showed some code.") {
                out.push("It showed some code.".into());
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if t.starts_with('|') && t.matches('|').count() >= 2 {
            if !in_table {
                out.push("It showed a table.".into());
                in_table = true;
            }
            continue;
        }
        in_table = false;

        if line.starts_with("    ") || line.starts_with('\t') {
            if out.last().map(String::as_str) != Some("It showed some code.") {
                out.push("It showed some code.".into());
            }
            continue;
        }
        let mut t = t.trim_start_matches('#').trim();
        for b in ["- ", "* ", "+ ", "• ", "> "] {
            if let Some(r) = t.strip_prefix(b) {
                t = r.trim();
            }
        }

        if let Some((n, r)) = t.split_once(". ") {
            if !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()) {
                t = r.trim();
            }
        }
        if t.is_empty() || t.chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | '─')) {
            continue;
        }
        let mut l = clean_inline(t);
        if l.ends_with(':') {
            l.pop();
            l.push('.');
        } else if !l.ends_with(['.', '!', '?', ';']) {
            l.push('.');
        }
        out.push(l);
    }
    out.join(" ")
}

fn is_emoji(c: char) -> bool {
    matches!(
        c as u32,
        0x200D
            | 0x20E3
            | 0xFE00..=0xFE0F
            | 0x2190..=0x21FF
            | 0x2300..=0x23FF
            | 0x25A0..=0x27BF
            | 0x2900..=0x297F
            | 0x2B00..=0x2BFF
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
            | 0xE0020..=0xE007F
    )
}

fn no_emoji(s: &str) -> String {
    let kept: String = s.chars().filter(|&c| !is_emoji(c)).collect();
    let mut out = String::new();
    for w in kept.split_whitespace() {
        let w = match w
            .strip_prefix("https://")
            .or_else(|| w.strip_prefix("http://"))
        {
            Some(rest) => {
                let tail = rest
                    .chars()
                    .last()
                    .filter(|c| matches!(c, '.' | ',' | '!' | '?' | ';' | ':'));
                format!("a link{}", tail.map(String::from).unwrap_or_default())
            }
            None => w.to_string(),
        };
        let joins = w
            .chars()
            .all(|c| matches!(c, '.' | ',' | '!' | '?' | ';' | ':'));
        if !out.is_empty() && !joins {
            out.push(' ');
        }
        out.push_str(&w);
    }
    out
}

fn number_word(marker: &str) -> Option<String> {
    const WORDS: [&str; 20] = [
        "One",
        "Two",
        "Three",
        "Four",
        "Five",
        "Six",
        "Seven",
        "Eight",
        "Nine",
        "Ten",
        "Eleven",
        "Twelve",
        "Thirteen",
        "Fourteen",
        "Fifteen",
        "Sixteen",
        "Seventeen",
        "Eighteen",
        "Nineteen",
        "Twenty",
    ];
    let n: usize = marker.trim_end_matches('.').parse().ok()?;
    Some(match n {
        1..=20 => WORDS[n - 1].to_string(),
        _ => n.to_string(),
    })
}

fn clean_piece(s: &str) -> Option<String> {
    let s = no_emoji(&crate::md::plain(s));
    s.chars().any(char::is_alphanumeric).then_some(s)
}

fn closed(mut s: String) -> String {
    if !s.ends_with(['.', '!', '?']) {
        s = s.trim_end_matches([':', ';', ',']).to_string();
        s.push('.');
    }
    s
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Part {
    Prose,
    Head,
    Note,
    Intro,
    Lead,
    Ask,
    Choice,
    Point,
}

impl Part {
    fn keep(self) -> bool {
        !matches!(self, Part::Prose | Part::Head | Part::Note)
    }
}

const CHOICE_WORDS: &[&str] = &[
    "which",
    "option",
    "options",
    "choose",
    "pick",
    "prefer",
    "either",
    "or",
    "alternative",
    "alternatives",
    "choice",
    "choices",
    "select",
];

fn asks_to_choose(text: &str) -> bool {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| CHOICE_WORDS.contains(&w))
}

fn choice_lists(blocks: &[crate::md::Block]) -> Vec<bool> {
    use crate::md::Block;
    fn prose(b: Option<&Block>) -> Option<&str> {
        match b {
            Some(Block::Para(t) | Block::Quote(t)) => Some(t.as_str()),
            _ => None,
        }
    }
    let mut out = vec![false; blocks.len()];
    let mut i = 0;
    while i < blocks.len() {
        let Block::Item { marker, .. } = &blocks[i] else {
            i += 1;
            continue;
        };
        let mut end = i;
        while matches!(blocks.get(end + 1), Some(Block::Item { .. })) {
            end += 1;
        }
        let before = i.checked_sub(1).and_then(|k| prose(blocks.get(k)));
        let after = prose(blocks.get(end + 1)).filter(|t| t.contains('?'));
        let choice = number_word(marker).is_some()
            || before.is_some_and(asks_to_choose)
            || after.is_some_and(asks_to_choose);
        for c in &mut out[i..=end] {
            *c = choice;
        }
        i = end + 1;
    }
    out
}

fn tagged_parts(text: &str, aloud: bool) -> Vec<(String, Part)> {
    use crate::md::Block;
    let blocks = crate::md::parse(text);
    let choice = choice_lists(&blocks);
    let mut out: Vec<(String, Part)> = vec![];
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Para(t) | Block::Quote(t) => {
                let before_list = matches!(blocks.get(i + 1), Some(Block::Item { .. }));
                let pieces: Vec<String> = crate::assistant::sentence_spans(t)
                    .into_iter()
                    .filter_map(|(a, e)| clean_piece(&t[a..e]))
                    .collect();
                let last = pieces.len().saturating_sub(1);
                for (k, s) in pieces.into_iter().enumerate() {
                    let lead = k == last && (before_list || s.ends_with(':'));
                    let s = if aloud || !s.ends_with(':') {
                        closed(s)
                    } else {
                        s
                    };
                    let kind = if s.ends_with('?') {
                        Part::Ask
                    } else if lead && before_list && choice[i + 1] {
                        Part::Lead
                    } else if lead {
                        Part::Intro
                    } else {
                        Part::Prose
                    };
                    out.push((s, kind));
                }
            }
            Block::Heading(_, t) => {
                if let Some(s) = clean_piece(t) {
                    out.push((closed(s), Part::Head));
                }
            }
            Block::Item { marker, text, .. } => {
                let Some(s) = clean_piece(text) else {
                    continue;
                };
                let s = closed(s);
                let s = match number_word(marker) {
                    Some(n) if aloud => format!("{n}: {s}"),
                    Some(_) => format!("{marker} {s}"),
                    None => s,
                };
                out.push((s, if choice[i] { Part::Choice } else { Part::Point }));
            }
            Block::Table { rows, .. } if !rows.is_empty() => out.push((
                format!(
                    "A table of {} row{} is on screen.",
                    rows.len(),
                    if rows.len() == 1 { "" } else { "s" }
                ),
                Part::Note,
            )),
            Block::Table { .. } | Block::Code(_) | Block::Rule => {}
        }
    }
    out
}

pub fn reply_parts(text: &str, aloud: bool) -> Vec<(String, bool)> {
    tagged_parts(text, aloud)
        .into_iter()
        .map(|(s, k)| (s, k.keep()))
        .collect()
}

fn pick(parts: Vec<(String, bool)>, max: usize) -> Vec<String> {
    let mut room = max;
    parts
        .into_iter()
        .filter(|(_, keep)| {
            if *keep {
                true
            } else if room > 0 {
                room -= 1;
                true
            } else {
                false
            }
        })
        .map(|p| p.0)
        .collect()
}

pub fn spoken_reply(text: &str, n: usize) -> String {
    pick(reply_parts(text, true), n.max(1)).join(" ")
}

const CUE: &str = "More on screen.";

const CUE_AT: usize = 6;

pub fn leveled_reply(
    text: &str,
    level: crate::app_talkback::TalkLevel,
    sentences: usize,
) -> String {
    use crate::app_talkback::TalkLevel;
    let parts = tagged_parts(text, true);
    let said: Vec<bool> = match level {
        TalkLevel::Detailed => return spoken_reply(text, FULL_REPLY),
        TalkLevel::Normal => {
            let mut room = sentences.max(1);
            parts
                .iter()
                .map(|(_, k)| {
                    if k.keep() {
                        true
                    } else if room > 0 {
                        room -= 1;
                        true
                    } else {
                        false
                    }
                })
                .collect()
        }
        TalkLevel::Brief => {
            let first = [
                &[Part::Prose][..],
                &[Part::Intro],
                &[Part::Head, Part::Point, Part::Note],
            ]
            .iter()
            .find_map(|kinds| parts.iter().position(|(_, k)| kinds.contains(k)));
            parts
                .iter()
                .enumerate()
                .map(|(i, (_, k))| {
                    Some(i) == first || matches!(k, Part::Lead | Part::Ask | Part::Choice)
                })
                .collect()
        }
    };
    let dropped = said.iter().filter(|s| !**s).count();
    let cue_at = (dropped >= CUE_AT)
        .then(|| said.iter().position(|s| !*s))
        .flatten();
    let mut out: Vec<&str> = vec![];
    for (i, (s, _)) in parts.iter().enumerate() {
        if Some(i) == cue_at {
            out.push(CUE);
        }
        if said[i] {
            out.push(s);
        }
    }
    out.join(" ")
}

pub fn shown_reply(text: &str) -> String {
    pick(reply_parts(text, false), usize::MAX).join(" ")
}

pub fn screen_reply(screen: &str, sent: &str) -> Option<String> {
    let head: String = sent
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(20)
        .collect();
    if head.is_empty() {
        return None;
    }
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines.iter().rposition(|l| {
        l.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains(&head)
    })?;
    let mut out = vec![];
    for l in &lines[at + 1..] {
        let t = l.trim();
        if t.starts_with('╭')
            || t.starts_with('─')
            || t.starts_with('>')
            || t.starts_with('❯')
            || t.contains("? for shortcuts")
            || t.contains("esc to interrupt")
        {
            break;
        }
        let t = t
            .trim_matches(|c: char| matches!(c, '│' | '⏺' | '●' | '⎿' | '✻'))
            .trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
    }
    let s = out.join("\n");
    (!s.trim().is_empty()).then_some(s)
}

impl App {
    pub fn enter_direct(&mut self, s: usize, t: usize) -> Result<String, String> {
        let tab = self
            .panes
            .get(s)
            .and_then(|p| p.tabs.get(t))
            .ok_or("that tab is gone")?;
        if !tab_can_direct(tab) {
            return Err("that tab is not running; start it first".into());
        }
        let uid = tab.uid;
        let name = self.spoken_tab_name(uid);
        self.direct.picked = None;
        self.direct.held = false;
        if self.direct.on.as_ref().is_some_and(|d| d.uid == uid) {
            return Ok(format!(
                "You're already talking to {name} directly. Say 'back to the assistant' to return."
            ));
        }

        if self.direct.on.is_some() {
            self.end_direct("switched", false);
        }
        crate::log::info(&format!("direct: talking to {} ({name})", tab_id(uid)));
        self.direct_begin(s, t, name.clone());
        self.jump_to(s, t);
        self.assistant.log.push(Entry::new(
            Who::Note,
            format!("Direct mode with {name}; say 'back to the assistant'"),
        ));
        self.voice.action = Some(format!("direct: {name}"));
        Ok(format!(
            "You're now talking to {name} directly. Say 'back to the assistant' to return."
        ))
    }

    fn direct_begin(&mut self, s: usize, t: usize, name: String) {
        let tab = &self.panes[s].tabs[t];
        let (uid, act) = (tab.uid, tab.activity);
        let last_reply = self
            .transcript_path(s, t)
            .and_then(|p| crate::sessions::last_assistant_text(&p));
        self.direct.on = Some(DirectMode {
            uid,
            name,
            since: chrono::Local::now(),
            log: vec![],
            last_act: act,
            last_reply,
            reply_wait: None,
            last_sent: None,
            confirm: None,
        });
        self.web_changed();
    }

    pub fn set_direct_follow(&mut self, on: bool) {
        if self.cfg.voice.direct_follow == on {
            return;
        }
        let _ = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("voice.direct_follow"),
            Some(toml_edit::value(on)),
        );
        self.config_mtime = crate::app::config_mtime();
        self.cfg.voice.direct_follow = on;
        self.direct.picked = None;
        self.direct.held = false;
        self.web_changed();
        crate::log::info(&format!(
            "direct: following the active tab is {}",
            if on { "on" } else { "off" }
        ));
    }

    pub fn user_picked_tab(&mut self, uid: u64, whence: &str) {
        if self.direct.on.is_none() || !self.cfg.voice.direct_follow {
            return;
        }
        crate::log::info(&format!(
            "direct: the user went to {} ({whence})",
            tab_id(uid)
        ));
        self.direct.picked = Some((uid, Instant::now()));
        self.direct.held = false;
    }

    fn follow_watch_focus(&mut self) {
        let cur = self.panes.get(self.focus).map(|p| p.cur().uid);
        if cur == self.direct.shown {
            return;
        }
        let prev = std::mem::replace(&mut self.direct.shown, cur);
        let same_tab = prev.is_some_and(|p| Some(self.current_uid(p)) == cur);
        if let (Some(uid), Some(_), false) = (cur, prev, same_tab) {
            self.user_picked_tab(uid, "in the app");
        }
    }

    fn follow_tick(&mut self) {
        let Some((want, at)) = self.direct.picked else {
            return;
        };
        let cur = self.direct.on.as_ref().map(|d| d.uid);
        let (Some(cur), true) = (cur, self.cfg.voice.direct_follow) else {
            self.direct.picked = None;
            return;
        };
        let want = self.current_uid(want);
        if want == cur {
            self.direct.picked = None;
            return;
        }
        if at.elapsed() < FOLLOW_SETTLE {
            return;
        }
        let spot = self
            .find_tab(want)
            .filter(|&(s, t)| tab_can_direct(&self.panes[s].tabs[t]));
        let Some((s, t)) = spot else {
            crate::log::info(&format!(
                "direct: not following to {}: it is not running",
                tab_id(want)
            ));
            self.direct.picked = None;
            return;
        };
        let confirming = self.direct.on.as_ref().is_some_and(|d| {
            d.confirm
                .as_ref()
                .is_some_and(|c| c.at.elapsed() < CONFIRM_TTL)
        });
        let pasting = self
            .deliveries
            .iter()
            .any(|d| d.uid == cur && d.in_flight());
        if confirming || pasting {
            if !self.direct.held {
                self.direct.held = true;
                crate::log::info(&format!(
                    "direct: waiting to follow to {}: {}",
                    tab_id(want),
                    if confirming {
                        "an approval is being answered by voice"
                    } else {
                        "a message is being typed into the current tab"
                    }
                ));
            }
            return;
        }
        self.follow_to(s, t);
    }

    fn follow_to(&mut self, s: usize, t: usize) {
        let uid = self.panes[s].tabs[t].uid;
        let name = self.spoken_tab_name(uid);
        self.direct.picked = None;
        self.direct.held = false;
        if let Some(old) = self.direct.on.take() {
            crate::log::info(&format!(
                "direct: followed the active tab from {} to {} ({name}) after {} exchange lines",
                tab_id(old.uid),
                tab_id(uid),
                old.log.len()
            ));
            if !old.log.is_empty() {
                self.keep_direct_note(&old);
            }
        }
        self.direct_begin(s, t, name.clone());
        self.assistant.log.push(Entry::new(
            Who::Note,
            format!("Voice followed the active tab: now talking to {name}"),
        ));
        self.voice.action = Some(format!("direct: {name}"));
        let say = format!("Now talking to {name}.");
        self.flash(say.clone());
        self.direct_say(&say);
    }

    fn keep_direct_note(&mut self, d: &DirectMode) {
        let note = direct_note(d);
        self.assistant.direct_note = Some(match self.assistant.direct_note.take() {
            Some(prev) => format!("{prev}\n{note}"),
            None => note,
        });
    }

    pub fn mic_targets(&self) -> Vec<(u64, String)> {
        let cur = self.direct.on.as_ref().map(|d| d.uid);
        self.all_tabs()
            .into_iter()
            .filter(|&(s, t)| {
                let tab = &self.panes[s].tabs[t];
                Some(tab.uid) != cur && tab_can_direct(tab)
            })
            .map(|(s, t)| {
                let uid = self.panes[s].tabs[t].uid;
                let acct = self.panes[s]
                    .account
                    .map(|a| self.account_cfg(a).display().to_string())
                    .unwrap_or_else(|| "no account".into());
                (
                    uid,
                    format!(
                        "{acct} · {}",
                        crate::paths::middle(&self.spoken_tab_name(uid), 28)
                    ),
                )
            })
            .collect()
    }

    pub fn mic_move_to(&mut self, uid: u64) {
        match self.find_tab(uid) {
            Some((s, t)) => self.direct_on_tab(s, t),
            None => self.flash("Direct mode: that tab is gone"),
        }
    }

    pub fn talk_to_tab_tool(&mut self, args: &Value) -> Result<Value, String> {
        let (s, t) = self.one_tab(args)?;
        let uid = self.panes[s].tabs[t].uid;
        let say = self.enter_direct(s, t)?;
        Ok(json!({"ok": true, "result": {
            "tab": tab_id(uid),
            "direct_mode": true,
            "say": say,
            "next": "Say exactly the say text and nothing else. From now on the user's speech goes to that tab, not to you, until they come back."
        }}))
    }

    pub fn toggle_direct(&mut self) {
        if self.direct.on.is_some() {
            self.end_direct("by key", true);
            return;
        }
        let (s, t) = (self.focus, self.panes[self.focus].active);
        self.direct_on_tab(s, t);
    }

    pub fn direct_on_tab(&mut self, s: usize, t: usize) {
        match self.enter_direct(s, t) {
            Ok(say) => {
                if self.voice.engine.is_none() {
                    self.flash(format!(
                        "{say} Voice is off: Ctrl-a space talks (push to talk)."
                    ));
                } else {
                    self.flash(say.clone());
                }
                self.direct_say(&say);
            }
            Err(e) => self.flash(format!("Direct mode: {e}")),
        }
    }

    pub fn direct_with(&self, uid: u64) -> bool {
        self.direct.on.as_ref().is_some_and(|d| d.uid == uid)
    }

    pub(crate) fn spoken_prompt_up(&self, s: usize, t: usize) -> bool {
        let tab = &self.panes[s].tabs[t];
        let screen = self.tab_screen(s, t);
        tab.activity == Activity::Permission
            || shown_activity(tab.harness, &screen) == Activity::Permission
            || crate::pane::asks_question(&screen)
            || crate::prompt_cli::parse_prompt_for(tab.harness, &screen).is_some()
            || crate::prompt::parse_prompt(&screen).is_some()
    }

    pub(crate) fn spoken_gate(&self, s: usize, t: usize) -> Gate {
        use crate::harness::Harness;
        let tab = &self.panes[s].tabs[t];
        if !tab.is_running() {
            return Gate::Hold("the tab is not running yet");
        }
        if self.spoken_prompt_up(s, t) {
            return Gate::Hold("the tab shows a prompt");
        }
        let screen = self.tab_screen(s, t);
        let shown = shown_activity(tab.harness, &screen);
        let claude = tab.harness == Harness::Claude
            && self.pane_harness(s).is_none_or(|h| h == Harness::Claude);
        let gate = match (tab.activity, shown) {
            (Activity::Ready, Activity::Ready) => Gate::Now,
            (Activity::Working, Activity::Working) => {
                if !claude {
                    return Gate::Hold("the tab is working and its CLI takes no input mid-turn");
                }
                if !input_line(&screen) {
                    return Gate::Hold("the tab is working and shows no input box");
                }
                if tab.activity_since.elapsed() < STEER_SETTLE {
                    return Gate::Hold("the tab just started working");
                }
                Gate::Steer
            }
            (Activity::Working, _) | (_, Activity::Working) => {
                return Gate::Hold("the tab is working")
            }
            _ => return Gate::Hold("the tab is not ready"),
        };
        if self.pending_enters.iter().any(|p| p.uid == tab.uid) {
            return Gate::Hold("the tab is being typed in");
        }
        let typing = if claude {
            tab.user_typed_within(TYPING_HOLD)
        } else {
            tab.last_input.elapsed() < TYPING_HOLD
        };
        if typing {
            return Gate::Hold("the tab is being typed in");
        }
        let drafting = crate::pane::typed_draft(
            tab.parser
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .screen(),
        );
        if drafting {
            return Gate::Hold("its input box holds unsent text");
        }
        gate
    }

    fn spoken_hold(&mut self, uid: u64, n: usize, why: &'static str) {
        if self.direct.hold.contains(&(uid, why)) {
            return;
        }
        self.direct.hold.retain(|(u, _)| *u != uid);
        self.direct.hold.push((uid, why));
        crate::log::info(&format!(
            "deliver: holding {n} spoken message(s) for {}: {why}",
            tab_id(uid)
        ));
    }

    pub(crate) fn spoken_release_tick(&mut self) {
        use crate::control::Stage;
        let mut tabs: Vec<u64> = vec![];
        for d in &self.deliveries {
            if d.rc.spoken && d.stage == Stage::Queued {
                let uid = self.current_uid(d.uid);
                if !tabs.contains(&uid) {
                    tabs.push(uid);
                }
            }
        }
        self.direct.hold.retain(|(u, _)| tabs.contains(u));
        for uid in tabs {
            let ids: Vec<u64> = self
                .deliveries
                .iter()
                .filter(|d| {
                    d.rc.spoken && d.stage == Stage::Queued && self.current_uid(d.uid) == uid
                })
                .map(|d| d.id)
                .collect();
            let Some((s, t)) = self.find_tab(uid) else {
                continue;
            };
            let blocked = self
                .deliveries
                .iter()
                .any(|d| !d.done() && !ids.contains(&d.id) && self.current_uid(d.uid) == uid);
            let gate = if blocked {
                Gate::Hold("an earlier message is still going in")
            } else {
                self.spoken_gate(s, t)
            };
            let mine = |d: &&crate::control::Delivery| ids.contains(&d.id);
            let first = self.deliveries.iter().filter(mine).map(|d| d.created).min();
            let last = self.deliveries.iter().filter(mine).map(|d| d.created).max();
            let mid = match gate {
                Gate::Now => false,
                Gate::Steer => {
                    let quiet = last.is_none_or(|x| x.elapsed() >= SPOKEN_QUIET);
                    let capped = first.is_none_or(|x| x.elapsed() >= SPOKEN_MAX);
                    if !quiet && !capped {
                        self.spoken_hold(uid, ids.len(), "waiting a moment for more speech");
                        continue;
                    }
                    true
                }
                Gate::Hold(why) => {
                    self.spoken_hold(uid, ids.len(), why);
                    continue;
                }
            };
            let texts: Vec<String> = self
                .deliveries
                .iter()
                .filter(mine)
                .map(|d| d.text.clone())
                .collect();
            let merged = join_spoken(&texts);
            let keep = ids[0];
            self.deliveries
                .retain(|d| d.id == keep || !ids.contains(&d.id));
            if let Some(d) = self.deliveries.iter_mut().find(|d| d.id == keep) {
                d.text = merged.clone();
                d.rc.go = true;
                d.rc.mid_turn = mid;
            }
            let others = if ids.len() > 1 {
                format!(
                    ", merged {}",
                    ids[1..]
                        .iter()
                        .map(|i| format!("#{i}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            } else {
                String::new()
            };
            crate::log::info(&format!(
                "deliver: spoken #{keep} for {}: {} as one message ({} chars), {}{others}",
                tab_id(uid),
                crate::control::plural(ids.len(), "utterance"),
                merged.len(),
                if mid {
                    "mid-turn while the tab works"
                } else {
                    "the tab is ready"
                }
            ));
            self.direct.hold.retain(|(u, _)| *u != uid);
            if ids.len() > 1 {
                if let Some(d) = self.direct.on.as_mut().filter(|d| d.uid == uid) {
                    d.last_sent = Some(merged);
                }
            }
            self.state_dirty = true;
        }
    }

    pub fn drop_spoken_queue(&mut self, why: &str) -> usize {
        let ids = self.direct.spoken.clone();
        if ids.is_empty() {
            return 0;
        }
        self.direct.hold.clear();
        let before = self.deliveries.len();
        self.deliveries
            .retain(|d| !(ids.contains(&d.id) && d.stage == crate::control::Stage::Queued));
        let n = before - self.deliveries.len();
        if n > 0 {
            let left: Vec<u64> = self.deliveries.iter().map(|d| d.id).collect();
            self.direct.spoken.retain(|id| left.contains(id));
            crate::log::info(&format!(
                "direct: dropped {n} spoken message(s) still waiting for their tab ({why})"
            ));
            self.direct_line(format!(
                "Dropped {n} spoken message{} that had not reached the tab yet",
                if n == 1 { "" } else { "s" }
            ));
        }
        n
    }

    pub fn end_direct(&mut self, why: &str, say: bool) {
        let Some(d) = self.direct.on.take() else {
            return;
        };
        crate::log::info(&format!(
            "direct: ended ({why}) with {} after {} exchange lines",
            tab_id(d.uid),
            d.log.len()
        ));
        self.keep_direct_note(&d);
        self.direct.picked = None;
        self.direct.held = false;
        self.web_changed();
        self.assistant.log.push(Entry::new(
            Who::Note,
            format!("Back to the assistant (direct mode with {} ended)", d.name),
        ));
        self.voice.action = Some("back to the assistant".into());
        if say {
            self.direct_say("Back to the assistant.");
        }
    }

    pub fn direct_say(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        crate::log::info(&format!("direct: saying: {text}"));
        self.direct.said.push(text.to_string());
        let n = self.direct.said.len();
        if n > 50 {
            self.direct.said.drain(..n - 50);
        }
        if self.will_speak_assistant() {
            self.told_record("direct mode", text, crate::told::Channel::Spoken, false);
        }
        self.speak_assistant(text);
    }

    pub(crate) fn direct_line(&mut self, text: String) {
        self.assistant.log.push(Entry::new(Who::Preamble, text));
        let n = self.assistant.log.len();
        if n > 200 {
            self.assistant.log.drain(..n - 200);
        }
    }

    pub fn direct_heard(
        &mut self,
        text: &str,
        norm: &str,
        woke: Option<String>,
        ptt: bool,
        stats: crate::voice::wake_profile::UttStats,
        table: &[(String, crate::instant::Instant)],
    ) {
        if let Some(rest) = woke {
            self.end_direct("wake word", false);
            if rest.trim().is_empty() {
                return self.bare_wake("Back to the assistant. Yes?", ptt, true);
            }
            return self.run_heard(rest);
        }
        let waiting = self
            .direct_tab()
            .is_some_and(|(s, t)| self.panes[s].tabs[t].activity == Activity::Permission)
            || self.direct.on.as_ref().is_some_and(|d| d.confirm.is_some());
        let why = if ptt {
            crate::app_openmic::is_noise(norm).then_some("noise")
        } else {
            crate::app_openmic::hands_free_filter(norm, stats, table)
        };

        let why = why.filter(|w| {
            !(*w == "too short"
                && (waiting
                    || crate::instant::affirmative(text)
                    || crate::instant::negative(text)
                    || is_exit_phrase(norm)))
        });
        if let Some(why) = why {
            self.voice.ignored += 1;
            crate::log::info(&format!("direct: ignored ({why}): {text}"));
            self.voice.action = Some(format!("({why}, ignored)"));
            return;
        }
        self.direct_route(text.trim(), norm);
    }

    pub fn direct_tab(&self) -> Option<(usize, usize)> {
        let d = self.direct.on.as_ref()?;
        self.locate_uid(d.uid)
    }

    pub fn direct_route(&mut self, text: &str, norm: &str) {
        if self.direct.on.is_none() {
            return;
        }
        if is_exit_phrase(norm) {
            crate::log::info(&format!("direct: exit phrase: {text}"));
            self.end_direct("exit phrase", true);
            return;
        }

        if let Some(name) = switch_target(norm) {
            if let Some((s, t)) = self.direct_switch_target(&name) {
                crate::log::info(&format!("direct: switching to {name}"));
                if let Ok(say) = self.enter_direct(s, t) {
                    self.direct_say(&say);
                }
                return;
            }
        }
        let Some((s, t)) = self.direct_tab() else {
            self.end_direct("tab gone", true);
            return;
        };
        let uid = self.panes[s].tabs[t].uid;
        let name = self
            .direct
            .on
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_default();

        if let Some(c) = self.direct.on.as_mut().and_then(|d| d.confirm.take()) {
            if c.at.elapsed() < CONFIRM_TTL {
                if crate::instant::negative(text) {
                    self.direct_say("Okay, I left it as it is.");
                    return;
                }
                if crate::instant::affirmative(text) {
                    self.direct_answer_confirmed(s, t, c);
                    return;
                }
            }
        }

        if self.panes[s].tabs[t].activity == Activity::Permission {
            if let Some(choice) = approval_word(norm, &self.cfg.voice.instant) {
                self.direct_answer(s, t, choice);
                return;
            }
        }

        if self.cfg.voice.instant_commands {
            let table = crate::instant::table(&self.cfg.voice.instant);
            if let Some(i) = crate::instant::match_instant(text, &table) {
                use crate::instant::Instant as I;
                if !matches!(i, I::Approve | I::Deny | I::Resume) {
                    if let Some(msg) = self.run_instant(i) {
                        crate::log::info(&format!("direct: instant {i:?}: {msg}"));
                        self.voice.action = Some(msg);
                        return;
                    }
                }
            }
        }
        if self.resend_said(text) {
            return;
        }
        if let Some(v) = &self.voice.engine {
            v.stop_speaking();
        }
        let waits = self.panes[s].tabs[t].activity == Activity::Permission;
        let steer = self.spoken_gate(s, t) == Gate::Steer;
        let id = self.deliver_as(uid, text, true);
        self.direct_sent(id);
        let went = !self
            .deliveries
            .iter()
            .any(|d| d.id == id && d.stage == crate::control::Stage::Queued);
        let busy = !went && !steer;
        crate::log::info(&format!(
            "direct: typed into {} as #{id}{}",
            tab_id(uid),
            if busy {
                " (queued: busy)"
            } else if !went {
                " (goes in mid-turn)"
            } else {
                ""
            }
        ));
        if let Some(d) = self.direct.on.as_mut() {
            d.log.push((true, text.to_string()));
            d.last_sent = Some(text.to_string());
        }
        self.direct_line(if waits {
            format!("you: {text} (queued: {name} waits for your yes or no)")
        } else if busy {
            format!("you: {text} (queued: {name} is busy)")
        } else {
            format!("you: {text}")
        });
        self.voice.action = Some(format!("→ {name}"));
        if waits {
            self.direct_say("It still needs your yes or no first.");
        }
    }

    fn direct_switch_target(&self, name: &str) -> Option<(usize, usize)> {
        let cur = self.direct.on.as_ref()?.uid;
        let tries = [name.to_string(), name.replace(' ', "-")];
        for n in tries.iter() {
            if let Ok(v) = self.tab_targets(&json!({"tab": n}), false) {
                if let [(s, t)] = v.as_slice() {
                    if self.panes[*s].tabs[*t].uid != cur {
                        return Some((*s, *t));
                    }
                    return None;
                }
            }
        }
        None
    }

    fn direct_answer(&mut self, s: usize, t: usize, choice: &'static str) {
        let tid = tab_id(self.panes[s].tabs[t].uid);
        let args = json!({"tab": tid, "choice": choice});
        match self.plan_for("answer_prompt", &args) {
            Ok((plan, question, needs)) => {
                if needs {
                    if let Some(d) = self.direct.on.as_mut() {
                        d.confirm = Some(DirectConfirm {
                            plan,
                            choice,
                            at: Instant::now(),
                        });
                    }
                    self.direct_say(&format!("{question} Say yes to go ahead."));
                } else {
                    self.direct_run_answer(s, t, &plan, choice);
                }
            }
            Err(e) => self.direct_say(&format!("I couldn't answer it: {e}.")),
        }
    }

    fn direct_answer_confirmed(&mut self, s: usize, t: usize, c: DirectConfirm) {
        let tid = tab_id(self.panes[s].tabs[t].uid);
        let args = json!({"tab": tid, "choice": c.choice});
        match self.plan_for("answer_prompt", &args) {
            Ok((plan, _, _)) if plan == c.plan => self.direct_run_answer(s, t, &plan, c.choice),
            Ok(_) => self.direct_say("The prompt changed meanwhile, so I did nothing."),
            Err(e) => self.direct_say(&format!("I couldn't answer it: {e}.")),
        }
    }

    fn direct_run_answer(&mut self, s: usize, t: usize, plan: &Value, choice: &str) {
        let uid = self.panes.get(s).and_then(|p| p.tabs.get(t)).map(|x| x.uid);
        let before = uid.map(|_| self.panes[s].tabs[t].last_input);
        match self.run_plan("answer_prompt", plan) {
            Ok(_) => {
                if let Some(tab) = self.panes.get_mut(s).and_then(|p| p.tabs.get_mut(t)) {
                    if Some(tab.uid) == uid && Some(tab.last_input) != before {
                        tab.own_input = Some(tab.last_input);
                    }
                }
                let said = match choice {
                    "yes" => "Approved.",
                    "always" => "Always allowed.",
                    _ => "Denied.",
                };
                if let Some(d) = self.direct.on.as_mut() {
                    d.log
                        .push((true, format!("({})", said.trim_end_matches('.'))));
                }
                self.direct_say(said);
            }
            Err(e) => self.direct_say(&format!("I couldn't answer it: {e}.")),
        }
    }

    pub fn direct_tick(&mut self) {
        self.follow_watch_focus();
        let Some(uid) = self.direct.on.as_ref().map(|d| d.uid) else {
            self.direct.picked = None;
            return;
        };
        let now_uid = self.current_uid(uid);
        if let Some(d) = self.direct.on.as_mut() {
            d.uid = now_uid;
        }
        let Some((s, t)) = self.find_tab(now_uid) else {
            let name = self
                .direct
                .on
                .as_ref()
                .map(|d| d.name.clone())
                .unwrap_or_default();
            self.end_direct("tab closed", false);
            self.direct_say(&format!("{name} was closed. Back to the assistant."));
            return;
        };
        let tab = &self.panes[s].tabs[t];
        if tab.activity == Activity::Exited
            || (!tab.is_running() && tab.pending.is_none() && !tab.suspended)
        {
            let name = self
                .direct
                .on
                .as_ref()
                .map(|d| d.name.clone())
                .unwrap_or_default();
            self.end_direct("tab ended", false);
            self.direct_say(&format!("{name} ended. Back to the assistant."));
            return;
        }
        let act = tab.activity;

        self.attention.retain(|a| a.uid != now_uid);
        let Some(d) = self.direct.on.as_mut() else {
            return;
        };
        let prev = d.last_act;
        d.last_act = act;
        let name = d.name.clone();
        if act != prev {
            if act == Activity::Permission {
                let what = self
                    .request_of(s, t)
                    .map(|r| crate::sessions::snippet(&r.summary(), 90))
                    .unwrap_or_else(|| "something".into());
                self.direct_line(format!("{name} waits for approval: {what}"));
                self.direct_say(&format!("{name} needs approval: {what}. Say yes or no."));
            }
            if act == Activity::Ready && matches!(prev, Activity::Working | Activity::Permission) {
                if let Some(d) = self.direct.on.as_mut() {
                    d.reply_wait = Some(Instant::now());
                }
            }
            if act == Activity::Working {
                if let Some(d) = self.direct.on.as_mut() {
                    d.reply_wait = None;
                }
            }
        }

        if act != Activity::Permission {
            if let Some(d) = self.direct.on.as_mut() {
                if d.confirm
                    .as_ref()
                    .is_some_and(|c| c.at.elapsed() > CONFIRM_TTL)
                {
                    d.confirm = None;
                }
            }
        }
        self.direct_reply_tick(s, t);
        self.follow_tick();
    }

    fn direct_reply_tick(&mut self, s: usize, t: usize) {
        let Some(d) = self.direct.on.as_ref() else {
            return;
        };
        let Some(since) = d.reply_wait else {
            return;
        };
        if self.panes[s].tabs[t].activity != Activity::Ready {
            return;
        }

        let screen = self.tab_screen(s, t);
        let last = d.last_reply.clone();
        let sent = d.last_sent.clone();
        let name = d.name.clone();
        let fresh = self
            .transcript_path(s, t)
            .and_then(|p| crate::sessions::last_assistant_text(&p))
            .filter(|r| Some(r) != last.as_ref() && !r.trim().is_empty());
        if fresh.is_none() && crate::app_continue::background_busy(&screen) {
            return;
        }
        let reply = match fresh {
            Some(r) => Some(r),
            None if since.elapsed() < REPLY_WAIT => return,

            None => sent
                .as_deref()
                .and_then(|x| screen_reply(&screen, x))
                .filter(|r| Some(r) != last.as_ref()),
        };
        if let Some(d) = self.direct.on.as_mut() {
            d.reply_wait = None;
            if let Some(r) = &reply {
                d.last_reply = Some(r.clone());
            }
        }
        let Some(reply) = reply else {
            self.direct_say(&format!("{name} finished."));
            return;
        };
        self.direct_say_reply(&name, &reply);
    }

    pub fn direct_say_reply(&mut self, name: &str, reply: &str) {
        let sentences = self.cfg.voice.direct_sentences.clamp(1, 10);
        let say = match crate::app_talkback::TalkLevel::chosen(&self.cfg) {
            Some(level) => leveled_reply(reply, level, sentences),
            None if self.cfg.voice.direct_reply == "sentences" => spoken_reply(reply, sentences),
            None => spoken_reply(reply, FULL_REPLY),
        };
        if let Some(d) = self.direct.on.as_mut() {
            d.log.push((false, reply.to_string()));
        }
        if say.is_empty() {
            self.direct_line(format!("{name} finished (nothing to read out)"));
            self.direct_say(&format!("{name} finished."));
            return;
        }
        self.direct_line(format!(
            "{name}: {}",
            crate::sessions::snippet(&shown_reply(reply), 600)
        ));
        self.direct_say(&say);
    }

    pub(crate) fn direct_spoke(&self, id: u64) -> bool {
        self.direct.spoken.contains(&id)
    }

    pub(crate) fn direct_sent(&mut self, id: u64) {
        self.direct.spoken.push(id);
        let n = self.direct.spoken.len();
        if n > SPOKEN_KEPT {
            self.direct.spoken.drain(..n - SPOKEN_KEPT);
        }
    }
}

fn approval_word(norm: &str, instant: &[String]) -> Option<&'static str> {
    let s = core_words(norm).join(" ");
    if matches!(
        s.as_str(),
        "always" | "always allow" | "allow always" | "yes always" | "yes and don't ask again"
    ) {
        return Some("always");
    }
    let table = crate::instant::table(instant);
    match crate::instant::match_instant(&s, &table) {
        Some(crate::instant::Instant::Approve) => Some("yes"),
        Some(crate::instant::Instant::Deny) => Some("no"),
        _ => match s.as_str() {
            "approve" | "approve it" | "yes" | "yes go ahead" | "go ahead" | "allow it"
            | "allow" => Some("yes"),
            "deny" | "deny it" | "no" | "don't" | "reject" | "reject it" => Some("no"),
            _ => None,
        },
    }
}

fn direct_note(d: &DirectMode) -> String {
    let id = tab_id(d.uid);
    let from = d.since.format("%H:%M");
    let to = chrono::Local::now().format("%H:%M");
    if d.log.is_empty() {
        return format!(
            "(Direct mode: from {from} to {to} the user was talking to {id} ({}) directly; nothing was said to it.)",
            d.name
        );
    }
    let user: Vec<String> = d
        .log
        .iter()
        .filter(|(u, _)| *u)
        .map(|(_, t)| format!("\"{}\"", crate::sessions::snippet(t, 100)))
        .collect();
    let replies: Vec<&String> = d.log.iter().filter(|(u, _)| !*u).map(|(_, t)| t).collect();
    let user_part = {
        let k = user.len().saturating_sub(4);
        let more = if k > 0 {
            format!("{k} earlier, then ")
        } else {
            String::new()
        };
        format!("{more}{}", user[k..].join(", "))
    };
    let reply_part = match replies.last() {
        Some(r) => format!(
            " Its last reply: \"{}\".",
            crate::sessions::snippet(&speakable(r), 240)
        ),
        None => " It has not replied yet.".into(),
    };
    format!(
        "(Direct mode: from {from} to {to} the user talked with {id} ({}) directly, not through you. They said: {user_part}.{reply_part} recent_turns {id} has the detail. You are back now: answer the user as usual.)",
        d.name
    )
}

pub fn indicator(app: &App) -> Option<String> {
    app.direct
        .on
        .as_ref()
        .map(|d| format!("● direct: {}", crate::paths::middle(&d.name, 24)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_phrases_are_few_and_clear() {
        let n = crate::voice::grammar::normalize;
        for yes in [
            "Back to the assistant.",
            "Okay, now hand me over to the main assistant.",
            "hand me back",
            "Exit direct mode.",
            "go back to the assistant please",
            "Take me back to the assistant",
            "switch me back to the main assistant",
            "end direct mode",
        ] {
            assert!(is_exit_phrase(&n(yes)), "{yes}");
        }
        for no in [
            "go back to the assistant's suggestion and rerun the tests in the other folder",
            "the assistant crate needs a test",
            "back up the database",
            "Can you check what the assistant module does?",
            "hand me the logs",
            "",
        ] {
            assert!(!is_exit_phrase(&n(no)), "{no}");
        }
    }

    #[test]
    fn utterances_join_as_sentences_in_order() {
        let j = |v: &[&str]| join_spoken(&v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(j(&["fix the build"]), "fix the build");
        assert_eq!(
            j(&[
                "fix the build",
                " then run it! ",
                "",
                "is it green?",
                "and, also,"
            ]),
            "fix the build. then run it! is it green? and, also,"
        );
        assert_eq!(j(&[]), "");
    }

    #[test]
    fn the_input_box_is_found_near_the_bottom_only() {
        let working = include_str!("../tests/fixtures/screens/working.txt");
        assert!(input_line(working));
        assert!(!input_line("* Pondering (3s . esc to interrupt)\n"));
        let far = format!("> old message\n{}", "line\n".repeat(12));
        assert!(!input_line(&far));
        for name in [
            "permission_bash",
            "ask_question",
            "login_method",
            "trust_prompt",
        ] {
            let s = std::fs::read_to_string(format!(
                "{}/tests/fixtures/screens/{name}.txt",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            assert!(
                crate::prompt::parse_prompt(&s).is_some()
                    || crate::pane::detect_activity(&s) == Activity::Permission,
                "{name} counts as a prompt"
            );
        }
    }

    #[test]
    fn switch_needs_a_name() {
        let n = crate::voice::grammar::normalize;
        assert_eq!(
            switch_target(&n("Talk to the trading room.")).as_deref(),
            Some("trading room")
        );
        assert_eq!(
            switch_target(&n("let me talk directly to fotmesh")).as_deref(),
            Some("fotmesh")
        );
        assert_eq!(switch_target(&n("talk to me about the tests")), None);
        assert_eq!(switch_target(&n("talk to it")), None);
        assert_eq!(switch_target(&n("run the tests")), None);
    }

    #[test]
    fn code_and_tables_are_named_not_read() {
        let r = "Here is the fix:\n```rust\nfn x() {}\n```\n| a | b |\n|---|---|\n| 1 | 2 |\n\n## Result\n- All **tests** pass, see [the log](https://x.y/z).\n- Second point";
        let s = speakable(r);
        assert_eq!(
            s,
            "Here is the fix. It showed some code. It showed a table. Result. All tests pass, see the log. Second point."
        );
    }

    #[test]
    fn a_reply_is_cleaned_for_speech_in_its_own_words() {
        let r = "\u{2705} Here is the **fix**:\n```rust\nfn x() {}\n```\n| a | b |\n|---|---|\n| 1 | 2 |\n\n## Result\nAll `tests` pass \u{1F389}, see [the log](https://x.y/z) or https://example.com/run/1 for more.";
        assert_eq!(
            spoken_reply(r, 40),
            "Here is the fix. A table of 1 row is on screen. Result. All tests pass, see the log or a link for more."
        );
        assert_eq!(spoken_reply("```sh\nls\n```", 40), "");
        assert_eq!(spoken_reply("\u{1F44D}", 40), "");
    }

    #[test]
    fn options_are_said_in_order_with_their_numbers() {
        let r = "I can do this two ways.\n\nOptions:\n1. Keep the cache and add a lock\n2. Drop the cache\n\nWhich?";
        assert_eq!(
            spoken_reply(r, 40),
            "I can do this two ways. Options. One: Keep the cache and add a lock. Two: Drop the cache. Which?"
        );
        assert_eq!(
            shown_reply(r),
            "I can do this two ways. Options: 1. Keep the cache and add a lock. 2. Drop the cache. Which?"
        );
        let tight =
            "I can do this two ways.\nOptions:\n1. Keep the cache\n2. Drop the cache\nWhich?";
        let said = spoken_reply(tight, 40);
        assert!(said.contains("One: Keep the cache."), "{said}");
        assert!(said.contains("Two: Drop the cache."), "{said}");
        assert!(said.ends_with("Which?"), "{said}");
        let bullets = "Pick one:\n- **Fast**: skip the tests\n- Safe: run them all";
        assert_eq!(
            spoken_reply(bullets, 40),
            "Pick one. Fast: skip the tests. Safe: run them all."
        );
    }

    #[test]
    fn a_limit_drops_prose_and_never_the_options_or_the_question() {
        let prose: Vec<String> = (1..=30)
            .map(|i| format!("Detail number {i} is here."))
            .collect();
        let r = format!(
            "{}\n\nOptions:\n1. Ship it now\n2. Wait a day\n\nWhich one do you want?",
            prose.join(" ")
        );
        assert_eq!(
            spoken_reply(&r, 2),
            "Detail number 1 is here. Detail number 2 is here. Options. One: Ship it now. Two: Wait a day. Which one do you want?"
        );
        let all = spoken_reply(&r, 40);
        assert!(all.contains("Detail number 30 is here."), "{all}");
    }

    const LONG: &str = "I looked into the cache bug. The lock is taken twice on the hot path, which is why it stalls under load. Fixing it means touching the writer and the reader. The tests cover the writer only. The reader has no tests at all. I also saw that the config loader swallows errors. That is a separate problem. It can wait.\n\nWhat changed so far:\n- The writer takes the lock once\n- The reader is untouched\n\nOptions:\n1. Fix the reader now\n2. Add reader tests first\n3. Leave it for later\n\nWhich one do you want?";

    const ASKED: &str =
        "Options. One: Fix the reader now. Two: Add reader tests first. Three: Leave it for later. Which one do you want?";

    #[test]
    fn each_level_says_the_options_and_less_and_less_of_the_rest() {
        use crate::app_talkback::TalkLevel;
        let detailed = leveled_reply(LONG, TalkLevel::Detailed, 3);
        let normal = leveled_reply(LONG, TalkLevel::Normal, 3);
        let brief = leveled_reply(LONG, TalkLevel::Brief, 3);
        assert_eq!(detailed, spoken_reply(LONG, FULL_REPLY));
        assert!(detailed.contains("It can wait."), "{detailed}");
        assert!(!detailed.contains(CUE), "{detailed}");
        assert_eq!(
            normal,
            format!("I looked into the cache bug. The lock is taken twice on the hot path, which is why it stalls under load. Fixing it means touching the writer and the reader. What changed so far. The writer takes the lock once. The reader is untouched. {ASKED}")
        );
        assert_eq!(brief, format!("I looked into the cache bug. {CUE} {ASKED}"));
        for said in [&detailed, &normal, &brief] {
            assert!(said.ends_with(ASKED), "{said}");
        }
        assert!(brief.len() < normal.len() && normal.len() < detailed.len());
    }

    #[test]
    fn brief_keeps_bullets_that_are_a_choice_and_drops_the_rest() {
        use crate::app_talkback::TalkLevel;
        let b = |t: &str| leveled_reply(t, TalkLevel::Brief, 3);
        assert_eq!(
            b("Pick one:\n- **Fast**: skip the tests\n- Safe: run them all"),
            "Pick one. Fast: skip the tests. Safe: run them all."
        );
        assert_eq!(
            b("Done.\n\n- Keep the cache\n- Drop the cache\n\nWhich do you prefer?"),
            "Done. Keep the cache. Drop the cache. Which do you prefer?"
        );
        assert_eq!(
            b("All tests pass. I changed three files.\n\nWhat changed:\n- The parser\n- The lexer\n\nWant me to commit?"),
            "All tests pass. Want me to commit?"
        );
        assert_eq!(b("Done, all green."), "Done, all green.");
        assert_eq!(
            b("## Result\nHere is what I did:\n- One thing\n- Another"),
            "Here is what I did."
        );
        assert_eq!(b("```sh\nls\n```"), "");
    }

    #[test]
    fn the_cue_is_only_for_long_replies() {
        use crate::app_talkback::TalkLevel;
        let short = "It works. I ran it twice. Both runs were clean.";
        assert_eq!(leveled_reply(short, TalkLevel::Brief, 3), "It works.");
        let long: Vec<String> = (1..=9).map(|i| format!("Detail {i} is here.")).collect();
        assert_eq!(
            leveled_reply(&long.join(" "), TalkLevel::Normal, 3),
            format!("Detail 1 is here. Detail 2 is here. Detail 3 is here. {CUE}")
        );
    }

    #[test]
    fn grok_screen_reply_is_found_under_the_echo() {
        let screen = "  > what is the status\n⏺ All good. Two tests fail.\n  Fixing them next.\n╭──────╮\n│ >    │\n╰──────╯";
        assert_eq!(
            screen_reply(screen, "what is the status").as_deref(),
            Some("All good. Two tests fail.\nFixing them next.")
        );
        assert_eq!(screen_reply(screen, "nothing like it"), None);
    }
}
