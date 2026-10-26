//! Direct mode: the voice goes to one tab. Every utterance is typed into
//! that tab (paste, then Enter once it shows, with receipts), never to the
//! assistant; the tab's replies are said when its turns end (code and
//! tables named, not read); its approval prompts are announced and a yes
//! or no answers them by the usual rules. "Back to the assistant", the
//! wake word, Ctrl-a K or a click ends it, and so does the tab closing;
//! the assistant then gets a short note of what was said.
//!
//! What is caught here before anything reaches the tab is kept small: a
//! short list of exit phrases, the wake word, "talk to <tab>" (switch the
//! target, only when it names exactly one other tab), yes/no while the tab
//! waits for approval, and the instant commands (stop, mute, pause).
//! Anything unclear goes to the tab.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::app_assistant::{Entry, Who};
use crate::control::tab_id;
use crate::pane::Activity;

/// A yes asked about an approval stays answerable this long.
const CONFIRM_TTL: Duration = Duration::from_secs(120);
/// After a turn ends, how long its reply is looked for in the transcript
/// before the screen is read instead.
const REPLY_WAIT: Duration = Duration::from_secs(4);

#[derive(Default)]
pub struct DirectState {
    pub on: Option<DirectMode>,
    /// What direct mode said out loud, newest last (the panel and tests).
    pub said: Vec<String>,
}

pub struct DirectMode {
    pub uid: u64,
    /// How the tab is named out loud (kept: the name lookup reads a file).
    pub name: String,
    pub since: chrono::DateTime<chrono::Local>,
    /// What was said both ways: (from the user, text).
    pub log: Vec<(bool, String)>,
    /// The tab's activity at the last tick.
    pub last_act: Activity,
    /// Its last reply already said (or there when direct mode began).
    pub last_reply: Option<String>,
    /// A turn ended at this time: its reply is being looked for.
    pub reply_wait: Option<Instant>,
    /// The last thing typed in (to find the reply on a Grok screen).
    pub last_sent: Option<String>,
    /// An approval answer waiting for a yes.
    pub confirm: Option<DirectConfirm>,
}

#[derive(Clone)]
pub struct DirectConfirm {
    pub plan: Value,
    pub choice: &'static str,
    pub at: Instant,
}

/// Whether direct mode can start with this tab.
fn tab_can_direct(tab: &crate::pane::Pane) -> bool {
    (tab.suspended || tab.pending.is_some() || tab.is_running()) && tab.activity != Activity::Exited
}

/// The live state on the mic badge: muted, listening or nothing.
pub fn mic_state(app: &App) -> Option<&'static str> {
    if app.voice.muted {
        Some("muted")
    } else if app.voice.status == crate::voice::VoiceStatus::Listening {
        Some("listening")
    } else {
        None
    }
}

/// Lower case words, as `grammar::normalize` gives them, without fillers
/// at either end.
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

/// "back to the assistant", "hand me back", "exit direct mode": the user
/// wants the assistant again. Short utterances only; anything else is for
/// the tab.
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
    // "... the assistant" / "... main assistant" at the end, with a word
    // that asks to go there.
    let to_assistant = w[n - 1] == "assistant"
        && n >= 2
        && matches!(w[n - 2].as_str(), "the" | "main" | "global" | "my");
    let go = has("back") || has("hand") || has("return") || has("switch") || has("over");
    if to_assistant && go {
        return true;
    }
    // "... direct mode" at the end with an exit verb.
    let direct = n >= 2 && w[n - 2] == "direct" && w[n - 1] == "mode";
    direct && (has("exit") || has("end") || has("leave") || has("stop") || has("quit"))
}

/// "talk to <name>": the tab named, as said (None: not a switch).
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
    // "talk to it" or "talk to me about..." is conversation, not a tab.
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

/// Remove markdown marks from one line: emphasis, inline code ticks and
/// links (their text stays), bare URLs (said as "a link").
fn clean_inline(line: &str) -> String {
    let mut s = line.replace("**", "").replace("__", "").replace('`', "");
    // [text](url) -> text
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

/// A tab's reply as it can be said: code blocks and tables are named, not
/// read; headings, bullets and markdown marks are dropped.
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
        // Indented code (four spaces or a tab).
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
        // "1. text" -> "text"
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

/// What is said of a reply: at most `n` sentences of its speakable text.
/// Returns (said, whether more was left out).
pub fn spoken_reply(text: &str, n: usize) -> (String, bool) {
    let sp = speakable(text);
    let (say, _, rest) = crate::assistant::spoken_part(&sp, n.max(1));
    (say, rest.is_some())
}

/// The reply on a screen, for a tab with no transcript to read (Grok):
/// the lines under the last echo of what was typed, up to the input box.
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
    /// Start talking to tab (s, t) directly. Returns the line to say.
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
        let act = tab.activity;
        let name = self.spoken_tab_name(uid);
        if self.direct.on.as_ref().is_some_and(|d| d.uid == uid) {
            return Ok(format!(
                "You're already talking to {name} directly. Say 'back to the assistant' to return."
            ));
        }
        // Another tab: that conversation ends (its note is kept).
        if self.direct.on.is_some() {
            self.end_direct("switched", false);
        }
        let last_reply = self
            .transcript_path(s, t)
            .and_then(|p| crate::sessions::last_assistant_text(&p));
        crate::log::info(&format!("direct: talking to {} ({name})", tab_id(uid)));
        self.direct.on = Some(DirectMode {
            uid,
            name: name.clone(),
            since: chrono::Local::now(),
            log: vec![],
            last_act: act,
            last_reply,
            reply_wait: None,
            last_sent: None,
            confirm: None,
        });
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

    /// Tabs the mic can move to from here: every other tab direct mode
    /// could start with, as (uid, "account · name").
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

    /// The mic menu's pick: talk to that tab directly.
    pub fn mic_move_to(&mut self, uid: u64) {
        match self.find_tab(uid) {
            Some((s, t)) => self.direct_on_tab(s, t),
            None => self.flash("Direct mode: that tab is gone"),
        }
    }

    /// The talk_to_tab tool.
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

    /// Ctrl-a K, the tab menu and the status chip: on for the focused (or
    /// given) tab, off when it is on.
    pub fn toggle_direct(&mut self) {
        if self.direct.on.is_some() {
            self.end_direct("by key", true);
            return;
        }
        let (s, t) = (self.focus, self.panes[self.focus].active);
        self.direct_on_tab(s, t);
    }

    /// Start direct mode on tab (s, t) from the UI: said and flashed.
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

    /// Whether direct mode is on with this tab.
    pub fn direct_with(&self, uid: u64) -> bool {
        self.direct.on.as_ref().is_some_and(|d| d.uid == uid)
    }

    /// End direct mode: the assistant gets a note of what was said; with
    /// `say`, "Back to the assistant." is said.
    pub fn end_direct(&mut self, why: &str, say: bool) {
        let Some(d) = self.direct.on.take() else {
            return;
        };
        crate::log::info(&format!(
            "direct: ended ({why}) with {} after {} exchange lines",
            tab_id(d.uid),
            d.log.len()
        ));
        let note = direct_note(&d);
        self.assistant.direct_note = Some(match self.assistant.direct_note.take() {
            Some(prev) => format!("{prev}\n{note}"),
            None => note,
        });
        self.assistant.log.push(Entry::new(
            Who::Note,
            format!("Back to the assistant (direct mode with {} ended)", d.name),
        ));
        self.voice.action = Some("back to the assistant".into());
        if say {
            self.direct_say("Back to the assistant.");
        }
    }

    /// Say a line in direct mode (queued after what is being said).
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

    /// A dim line in the panel for a direct exchange.
    pub(crate) fn direct_line(&mut self, text: String) {
        self.assistant.log.push(Entry::new(Who::Preamble, text));
        let n = self.assistant.log.len();
        if n > 200 {
            self.assistant.log.drain(..n - 200);
        }
    }

    /// An utterance while direct mode is on (after mute and pause).
    pub fn direct_heard(
        &mut self,
        text: &str,
        norm: &str,
        woke: Option<String>,
        ptt: bool,
        stats: crate::voice::wake_profile::UttStats,
        table: &[(String, crate::instant::Instant)],
    ) {
        // The wake word: back to the assistant, with what followed it.
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
        // A short answer to the agent ("Yes.", "Do it.") is not noise.
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

    /// Where the direct tab is now.
    pub fn direct_tab(&self) -> Option<(usize, usize)> {
        let d = self.direct.on.as_ref()?;
        self.locate_uid(d.uid)
    }

    /// Route one utterance in direct mode.
    pub fn direct_route(&mut self, text: &str, norm: &str) {
        if self.direct.on.is_none() {
            return;
        }
        if is_exit_phrase(norm) {
            crate::log::info(&format!("direct: exit phrase: {text}"));
            self.end_direct("exit phrase", true);
            return;
        }
        // "talk to <other tab>": switch, only on a sure match.
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
        // A yes asked about an approval.
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
        // The tab asks for approval: yes, no or always answers it.
        if self.panes[s].tabs[t].activity == Activity::Permission {
            if let Some(choice) = approval_word(norm, &self.cfg.voice.instant) {
                self.direct_answer(s, t, choice);
                return;
            }
        }
        // stop, mute, pause and the like still work.
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
        let tab = &self.panes[s].tabs[t];
        let busy = matches!(
            tab.activity,
            Activity::Working | Activity::Permission | Activity::Starting
        ) || !tab.is_running()
            || self.queued_for(uid) > 0;
        self.brainstorm_suggest_check(text);
        let id = self.deliver_request(uid, text);
        crate::log::info(&format!(
            "direct: typed into {} as #{id}{}",
            tab_id(uid),
            if busy { " (queued: busy)" } else { "" }
        ));
        if let Some(d) = self.direct.on.as_mut() {
            d.log.push((true, text.to_string()));
            d.last_sent = Some(text.to_string());
        }
        self.direct_line(format!("you → {name}: {text}"));
        self.voice.action = Some(format!("→ {name}"));
        if self.panes[s].tabs[t].activity == Activity::Permission {
            self.direct_say("Queued. It's waiting for approval: say yes or no.");
        } else if busy {
            self.direct_say("Queued.");
        }
    }

    /// The tab "talk to <name>" names, when it names exactly one tab
    /// other than the current one.
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

    /// Answer the direct tab's approval prompt by the usual rules: a no, a
    /// risky command or the confirm policy asks for a yes first.
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
                    self.direct_run_answer(&plan, choice);
                }
            }
            Err(e) => self.direct_say(&format!("I couldn't answer it: {e}.")),
        }
    }

    /// A yes to the question about an approval: the same plan runs, if the
    /// prompt is still the one asked about.
    fn direct_answer_confirmed(&mut self, s: usize, t: usize, c: DirectConfirm) {
        let tid = tab_id(self.panes[s].tabs[t].uid);
        let args = json!({"tab": tid, "choice": c.choice});
        match self.plan_for("answer_prompt", &args) {
            Ok((plan, _, _)) if plan == c.plan => self.direct_run_answer(&plan, c.choice),
            Ok(_) => self.direct_say("The prompt changed meanwhile, so I did nothing."),
            Err(e) => self.direct_say(&format!("I couldn't answer it: {e}.")),
        }
    }

    fn direct_run_answer(&mut self, plan: &Value, choice: &str) {
        match self.run_plan("answer_prompt", plan) {
            Ok(_) => {
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

    /// Every tick: follow the tab (closed or ended ends direct mode), say
    /// its approval prompts and its replies when its turns end.
    pub fn direct_tick(&mut self) {
        let Some(uid) = self.direct.on.as_ref().map(|d| d.uid) else {
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
        // Its own announcements ("needs approval", "finished") are said
        // here instead.
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
        // A pending yes for a prompt that is gone is dropped.
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
    }

    /// Look for the reply of the turn that just ended and say it.
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
        // Background agents still running: the turn is not over.
        let screen = self.tab_screen(s, t);
        if crate::app_continue::background_busy(&screen) {
            return;
        }
        let last = d.last_reply.clone();
        let sent = d.last_sent.clone();
        let name = d.name.clone();
        let fresh = self
            .transcript_path(s, t)
            .and_then(|p| crate::sessions::last_assistant_text(&p))
            .filter(|r| Some(r) != last.as_ref() && !r.trim().is_empty());
        let reply = match fresh {
            Some(r) => Some(r),
            None if since.elapsed() < REPLY_WAIT => return,
            // No transcript to read (Grok), or nothing new in it: the screen.
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

    /// Say a tab's reply: as it is when short, else summed up by the
    /// assistant (direct_reply auto) or its first sentences.
    pub fn direct_say_reply(&mut self, name: &str, reply: &str) {
        let n = self.cfg.voice.direct_sentences.clamp(1, 10);
        let (say, long) = spoken_reply(reply, n);
        if let Some(d) = self.direct.on.as_mut() {
            d.log.push((false, reply.to_string()));
        }
        self.direct_line(format!(
            "{name}: {}",
            crate::sessions::snippet(&speakable(reply), 300)
        ));
        if long && self.cfg.voice.direct_reply == "auto" && self.direct_summarize(name, reply) {
            return;
        }
        self.direct_say(&say);
    }

    /// Have the assistant sum up a long reply (the tab-answer report
    /// shape). False when it cannot take a turn now.
    fn direct_summarize(&mut self, name: &str, reply: &str) -> bool {
        if !self.assistant_on() || self.assistant.busy {
            return false;
        }
        let tid = self
            .direct
            .on
            .as_ref()
            .map(|d| tab_id(d.uid))
            .unwrap_or_default();
        let msg = format!(
            "(The agent in {tid} ({name}) replied to the user in direct mode; the user talks to it directly, this is only to be read out:) {}\nSummarize it for the user in one or two spoken sentences, as what {name} said. Never read code or tables. Call no tools.",
            crate::sessions::first_sentences(reply, 1500)
        );
        self.assistant.turn_spoken = true;
        self.ask_assistant_system(&format!("({name} replied; summing it up)"), &msg);
        self.assistant.busy
    }
}

/// yes / always / no for an approval prompt, from the words said.
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

/// What the assistant is told when direct mode ends.
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

/// The status bar chip and the panel line while direct mode is on.
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
        let (say, long) = spoken_reply(r, 2);
        assert_eq!(say, "Here is the fix. It showed some code.");
        assert!(long);
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
