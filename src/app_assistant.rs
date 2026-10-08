//! The assistant in the app: which account it runs on, the conversation
//! panel, routing speech and typed requests to it, and speaking its
//! replies as they stream.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

use crate::app::App;
use crate::assistant::{Brain, BrainEvent};
use crate::pane::Activity;

/// A barge-in colors only the request that follows soon after it.
const CUT_OFF_FRESH: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Who {
    User,
    Reply,
    Tool,
    Note,
    /// Text it wrote before a tool call (shown dim, not spoken).
    Preamble,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub who: Who,
    pub text: String,
}

#[derive(Default)]
pub struct AssistantState {
    pub brain: Option<Brain>,
    pub log: Vec<Entry>,
    /// The reply being streamed.
    pub current: String,
    pub busy: bool,
    pub show: bool,
    /// Typing goes to the panel (it has the focus); false: to the grid.
    pub focused: bool,
    pub input: String,
    /// Streamed deltas this turn (so whole text blocks are not repeated).
    got_delta: bool,
    /// Spoken replies open a follow up window when speech ends.
    pub follow_up: bool,
    warned_low: bool,
    pub last_cost: Option<f64>,
    pub started: Option<Instant>,
    /// Turns sent to the current process.
    pub turns_in_brain: u32,
    /// Summary carried into the next process.
    pub carry: Option<String>,
    /// When the last utterance arrived (for the latency breakdown).
    pub heard_at: Option<Instant>,
    pub timing: Timing,
    /// The last finished turn's numbers, shown in the panel.
    pub last_timing: Option<String>,
    /// Retry the warm start no more often than this.
    pub warm_tried: Option<Instant>,
    /// "stop": say nothing more of the current reply.
    pub muted: bool,
    /// The current (or last user) turn came in by voice.
    pub turn_spoken: bool,
    /// When its last reply ended.
    pub asked_at: Option<Instant>,
    /// The text block being streamed, and whether a tool came this turn.
    block: String,
    tools_this_turn: u32,
    /// Text blocks of the answer, joined with spaces.
    finals: Vec<String>,
    /// Text before any tool call, not spoken yet: the answer if the turn
    /// ends, narration if a tool call follows.
    held: Vec<String>,
    /// The saved conversation.
    pub conv: Option<crate::assistant_history::ConvLog>,
    /// The History tab of the panel.
    pub history: Option<crate::assistant_history::HistoryUi>,
    /// Switch to this saved conversation when the turn ends.
    pub resume_after: Option<String>,
    /// Start a new conversation when the turn ends.
    pub reset_after_turn: bool,
    /// What the user said this turn (a confirmation must be a clear yes).
    pub last_user: String,
    /// Turns the user cut off (barge-in): their output is dropped and no
    /// further tool runs for them.
    pub stale: u32,
    /// The next request follows an interruption (when it was, so a cut
    /// off before a sleep does not color the morning's first request).
    pub cut_off: Option<Instant>,
    /// "(Listening resumed at 20:37 after a 5 minute pause.)" for the next request.
    pub resume_note: Option<String>,
    /// The brain process is new: its first message carries the memory of
    /// recent conversations.
    pub brain_fresh: bool,
    /// The learned rules changed: restart the brain at the next turn.
    pub prompt_dirty: bool,
    /// Tools that read tab or file contents ran in this turn.
    pub turn_reads: u32,
    /// Conversations the memory holds, and when that was counted.
    pub memory_count: Option<(Instant, usize)>,
    /// The brain's last sign of life this turn (an event or a tool call):
    /// a turn silent too long is ended and the brain restarted.
    pub last_event: Option<Instant>,
    /// A new turn went out while a cut off one was still finishing.
    queued_after_cut: bool,
    /// The model chose to ignore this turn's utterance: say nothing.
    pub ignored_turn: bool,
    /// Move to this account (None: best) when the turn ends.
    pub account_after: Option<Option<usize>>,
    /// Tab ids in the memory block that now name a different tab (or no
    /// tab), with where that tab is now: a tool call naming one is refused
    /// once with that.
    pub stale_ids: std::collections::HashMap<u64, String>,
    purged: bool,
    /// The part of the last reply not spoken (past the spoken sentence
    /// limit), and when: "more" says it.
    pub unsaid: Option<(Instant, String)>,
    /// What was spoken lately (newest last), for the panel and tests.
    pub spoken: Vec<String>,
    /// This turn was started by GodTerm (a delegated answer arrived), not
    /// by the user.
    pub system_turn: bool,
    /// A raised reply limit (after a reply hit the old one), for every
    /// brain started from now on.
    pub token_cap: Option<u32>,
    /// This turn was already retried with a higher limit.
    cap_retried: bool,
    /// The current turn, to send again: (text, heard, system note).
    last_turn: Option<(String, Option<String>, Option<String>)>,
    /// The panel shows the admin actions instead of the chat.
    pub show_admin: bool,
    /// The panel shows the full timing of the last turn.
    pub show_details: bool,
    /// Requests typed in the panel, oldest first (↑/↓ walk them).
    pub input_hist: Vec<String>,
    pub hist_pos: Option<usize>,
    /// When the last character was typed into the input (a key burst
    /// with an Enter in it is a paste, not a send).
    pub last_key_at: Option<Instant>,
    /// Tool chips opened to show their raw arguments (log indices).
    pub expanded: std::collections::HashSet<usize>,
    /// Remote Control, while on (it belongs to the current brain process).
    pub remote: Option<crate::app_remote::Remote>,
    pub remote_pending: Option<crate::app_remote::RemotePending>,
    /// Messages GodTerm sent (their echoes are not Remote Control).
    pub sent: std::collections::VecDeque<String>,
    /// This turn came through Remote Control.
    pub turn_remote: bool,
    /// The menu's Remote Control waits for its second pick.
    pub remote_armed: Option<Instant>,
    /// Questions sent to tabs whose answers are reported back.
    pub follow_ups: Vec<crate::app_followup::FollowUp>,
    pub follow_seq: u64,
    /// The assistant could not start for a report: try again after this.
    pub report_retry: Option<Instant>,
}

/// Where the time of one turn went.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timing {
    pub heard: Option<Instant>,
    pub asked: Option<Instant>,
    pub first_token: Option<Instant>,
    pub first_tool: Option<Instant>,
    pub first_audio: Option<Instant>,
    pub stt_ms: Option<u32>,
}

impl Timing {
    /// "STT 240 ms · first token 610 ms · first audio 880 ms after you stopped".
    pub fn summary(&self, endpoint_ms: u32) -> String {
        let base = self.heard.or(self.asked);
        let ms =
            |t: Option<Instant>| -> Option<u128> { Some(t?.duration_since(base?).as_millis()) };
        let mut parts = vec![];
        if self.heard.is_some() {
            parts.push(format!("end of speech {endpoint_ms} ms"));
        }
        if let Some(s) = self.stt_ms {
            parts.push(format!("STT {s} ms"));
        }
        if let Some(m) = ms(self.asked) {
            parts.push(format!("sent +{m}"));
        }
        if let Some(m) = ms(self.first_tool) {
            parts.push(format!("first tool +{m}"));
        }
        if let Some(m) = ms(self.first_token) {
            parts.push(format!("first token +{m}"));
        }
        if let Some(m) = ms(self.first_audio) {
            parts.push(format!("first audio +{m} ms"));
        }
        parts.join(" · ")
    }
}

impl App {
    pub fn assistant_on(&self) -> bool {
        self.cfg.assistant.mode != "off"
    }

    /// The account it runs on: the configured one, or the logged in one
    /// with the most 5 hour quota left.
    pub fn assistant_account(&self) -> Option<usize> {
        // Only accounts of the provider's harness (none: it has its own login).
        let Some(h) = crate::providers::by_id(&self.cfg.assistant.provider).harness else {
            return None;
        };
        let claude = |a: usize| self.cfg.accounts[a].harness() == h;
        let want = self.cfg.assistant.account.trim();
        if !want.is_empty() && want != "best" {
            return self
                .cfg
                .accounts
                .iter()
                .position(|a| a.name == want || a.display() == want)
                .filter(|a| claude(*a));
        }
        let best = self
            .accounts
            .iter()
            .enumerate()
            .filter(|(i, st)| claude(*i) && st.login.logged_in())
            .filter_map(|(i, st)| st.effective_left().map(|l| (i, l)))
            .filter(|(_, l)| *l >= 2.0)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i);
        best.or_else(|| {
            (0..self.cfg.accounts.len()).find(|&a| claude(a) && self.accounts[a].login.logged_in())
        })
    }

    /// A line from GodTerm in the panel (and the saved conversation).
    pub fn assistant_note(&mut self, text: impl Into<String>) {
        self.note(text);
    }

    pub(crate) fn note(&mut self, text: impl Into<String>) {
        let text = text.into();
        if let Some(c) = &self.assistant.conv {
            c.write("note", serde_json::json!({"text": text}));
        }
        self.assistant.log.push(Entry {
            who: Who::Note,
            text,
        });
        self.trim_log();
    }

    fn trim_log(&mut self) {
        if self.assistant.log.len() > 200 {
            let cut = self.assistant.log.len() - 200;
            self.assistant.log.drain(..cut);
        }
    }

    /// Start the brain on its account if it is not running (also used to
    /// warm it up on the wake word, push to talk and speech start).
    pub fn ensure_brain(&mut self) -> Result<(), String> {
        // A long conversation is restarted with a one line summary, so
        // context (and answer time) stays small.
        // Never in the middle of something (a question waiting for its
        // yes, prompts on their way), and with the recent exchanges carried.
        let mid = self.pending_confirms.iter().any(|p| p.answerable())
            || self.deliveries.iter().any(|d| !d.done());
        if self.assistant.turns_in_brain >= self.cfg.assistant.reset_after_turns.max(2)
            && !self.assistant.busy
            && !mid
        {
            let carry = match &self.assistant.conv {
                Some(c) => crate::assistant_history::carry_summary(
                    &crate::assistant_history::read(&c.path),
                ),
                None => {
                    let last: Vec<String> = self
                        .assistant
                        .log
                        .iter()
                        .rev()
                        .filter(|e| matches!(e.who, Who::User | Who::Reply))
                        .take(6)
                        .map(|e| e.text.chars().take(200).collect())
                        .collect();
                    last.into_iter().rev().collect::<Vec<_>>().join(" / ")
                }
            };
            self.assistant.carry = (!carry.is_empty()).then_some(carry);
            self.assistant.brain = None;
            self.assistant.turns_in_brain = 0;
        }
        let provider = crate::providers::by_id(&self.cfg.assistant.provider);
        let best = self.cfg.assistant.account == "best";
        let want = self.assistant_account();
        if let Some(b) = self.assistant.brain.as_mut() {
            // With "best", a working brain stays put (the tick moves it off
            // an account under 5%); otherwise it follows the setting.
            if b.running()
                && b.provider == provider.id
                && (provider.harness.is_none() || best || b.account == want)
            {
                return Ok(());
            }
        }
        self.assistant.brain = None;
        self.remote_lost("the assistant restarted");
        let (account, home, label) = match provider.own_home {
            Some(home) => (None, home(), format!("{}'s own login", provider.name)),
            None => {
                let a = self
                    .assistant_account()
                    .ok_or("no logged in account for the assistant")?;
                if !self.accounts[a].login.logged_in() {
                    return Err(format!(
                        "{} is not logged in",
                        self.cfg.accounts[a].display()
                    ));
                }
                let dir = self.cfg.accounts[a].config_dir();
                let _ = crate::trust::seed_trust(&dir, &crate::assistant::dir());
                (Some(a), dir, self.cfg.accounts[a].display().to_string())
            }
        };
        let mut acfg = self.cfg.assistant.clone();
        if let Some(c) = self.assistant.token_cap {
            acfg.max_output_tokens = acfg.max_output_tokens.max(c);
        }
        let model = crate::providers::model_for(provider, &acfg);
        let ctx = crate::providers::StartCtx {
            bin: (provider.bin)(&self.cfg),
            home,
            cfg: &acfg,
            model: model.clone(),
            pass_env: &self.cfg.pass_env,
            events: self.tx.clone(),
            gen: crate::assistant::next_gen(),
            system_prompt: format!(
                "{}{}",
                crate::assistant::system_prompt(&acfg.style),
                crate::learned::prompt_section(&crate::learned::load())
            ),
        };
        let b = Brain::start(provider, account, ctx).map_err(|e| format!("{e:#}"))?;
        crate::log::info(&format!(
            "assistant: started {} on {label} ({model})",
            provider.name
        ));
        self.note(format!(
            "Assistant: {} on {label} ({model}){}.",
            provider.name,
            if account.is_some() {
                "; it spends this account's quota"
            } else {
                ""
            }
        ));
        self.assistant.brain = Some(b);
        self.assistant.brain_fresh = true;
        self.assistant.started = Some(Instant::now());
        Ok(())
    }

    /// A compact snapshot for each turn, so "what's going on" is answered
    /// from fresh data.
    pub fn state_preamble(&self) -> String {
        // About 600 tokens at most: one line per account and per tab, the
        // waiting prompt summarized; detail comes from the tools.
        const BUDGET: usize = 2400;
        let mut s = String::from("<state>\n");
        for a in 0..self.cfg.accounts.len() {
            let st = &self.accounts[a];
            let left = match st.five_hour_left() {
                // The kept percent is from before the reset.
                Some(_) if st.five_hour_reset_passed() => "5h reset, refreshing".to_string(),
                Some(l) => format!("{l:.0}% 5h"),
                None => "?".into(),
            };
            let week = self
                .week_left(a)
                .map(|l| format!(" {l:.0}% wk"))
                .unwrap_or_default();
            let week = match st.binding() {
                Some((e, which, at)) if e < 2.0 => format!(
                    "{week} OUT ({which} limit, resets {})",
                    at.map(|t| t
                        .with_timezone(&chrono::Local)
                        .format("%a %H:%M")
                        .to_string())
                        .unwrap_or_else(|| "?".into())
                ),
                _ => week,
            };
            // Old numbers say how old they are (a failing fetch keeps them).
            let stale = st
                .usage_good_at
                .filter(|t| chrono::Utc::now() - *t > chrono::Duration::minutes(10))
                .map(|t| format!(" (as of {} ago)", crate::usage::age(t, chrono::Utc::now())))
                .unwrap_or_default();
            s.push_str(&format!(
                "a{} {}: {}{left}{week}{stale}{}\n",
                a + 1,
                self.cfg.accounts[a].display(),
                if st.login.logged_in() {
                    ""
                } else {
                    "LOGGED OUT "
                },
                if self.brain_account() == Some(a) {
                    " (you)"
                } else {
                    ""
                }
            ));
        }
        let mut shown = 0;
        let total: usize = self.panes.iter().map(|p| p.tabs.len()).sum();
        'outer: for (si, slot) in self.panes.iter().enumerate() {
            for (ti, t) in slot.tabs.iter().enumerate() {
                let state = match t.activity {
                    Activity::Working => "working".to_string(),
                    Activity::Permission => format!(
                        "WAITING {}",
                        crate::sessions::snippet(
                            &self
                                .request_of(si, ti)
                                .map(|r| r.summary())
                                .unwrap_or_else(|| "approval".into()),
                            60
                        )
                    ),
                    // Ready on screen, but subagents or shells may still run.
                    Activity::Ready => {
                        let a = self.tab_activity(si, ti);
                        if a.background() > 0 {
                            format!("ready, {}", a.state)
                        } else {
                            "ready".into()
                        }
                    }
                    Activity::Starting => "starting".into(),
                    Activity::Exited => "ended".into(),
                    Activity::Idle => {
                        if t.suspended {
                            "paused".into()
                        } else {
                            "off".into()
                        }
                    }
                };
                let focus = if si == self.focus && ti == slot.active {
                    " *"
                } else {
                    ""
                };
                let _ = si;
                let loops = self.loops_in(t.uid);
                let queued = self.queued_for(t.uid);
                let line = format!(
                    "{} a{} tab{} \"{}\" {}: {state}{}{}{focus}{}\n",
                    crate::control::tab_id(t.uid),
                    slot.account.map(|a| a + 1).unwrap_or(0),
                    ti + 1,
                    crate::sessions::snippet(&t.name(), 24),
                    crate::sessions::snippet(&crate::config::tilde(&t.cwd), 32),
                    if loops > 0 {
                        format!(" {loops}loop")
                    } else {
                        String::new()
                    },
                    if queued > 0 {
                        format!(" {queued}queued")
                    } else {
                        String::new()
                    },
                    if self.last_target == Some(t.uid) {
                        " last"
                    } else {
                        ""
                    }
                );
                if s.len() + line.len() > BUDGET {
                    break 'outer;
                }
                s.push_str(&line);
                shown += 1;
            }
        }
        if shown < total {
            s.push_str(&format!("...{} more tabs (get_state)\n", total - shown));
        }
        for d in self.deliveries.iter().filter(|d| !d.done()) {
            let (st, _) = d.status();
            s.push_str(&format!(
                "prompt {st} for {} ({} s)\n",
                crate::control::tab_id(d.uid),
                d.created.elapsed().as_secs()
            ));
        }
        // Only a question still answerable is pending; an expired one
        // runs nothing on a yes and is only asked again.
        for p in &self.pending_confirms {
            if p.answerable() {
                s.push_str(&format!(
                    "PENDING {} token {}: \"{}\" asked {} s ago\n",
                    p.tool,
                    p.token,
                    p.summary,
                    p.age().as_secs()
                ));
            } else {
                s.push_str(&format!(
                    "expired question (a yes now does nothing; reissue_token {} asks it again): {} \"{}\"\n",
                    p.token, p.tool, p.summary
                ));
            }
        }
        s.push_str(&self.modes_line());
        s.push_str(&self.pending_answers_line());
        s.push_str(&self.update_state_line());
        s.push_str(&self.admin_state_lines());
        s.push_str(&self.grid_state_line());
        s.push_str(&self.provider_state_line());
        s.push_str(&self.speaker_state_line());
        s.push_str(&self.failover_state_line());
        s.push_str(&self.setup_state_line());
        s.push_str(&self.remote_state_line());
        // Listening is GodTerm's business: a message here means it is on.
        s.push_str(&match self.pause_left() {
            Some(left) => format!("listening_paused: {left} s left\n"),
            None => "listening_paused: no (listening is active)\n".to_string(),
        });
        s.push_str("(t.. = tab id, a = account, tabN = position in the account, * focused, last = the tab you used last)\n</state>");
        s
    }

    /// Where the tabs a memory names are now, by their session id or
    /// folder (never by the remembered id: ids from before a restart may
    /// name another tab). Ids that now mean something else are kept for a
    /// one time refusal (see control_call).
    pub fn remembered_tabs_now(&mut self, refs: &[crate::assistant_memory::TabRef]) -> String {
        self.assistant.stale_ids.clear();
        let mut lines = vec![];
        for r in refs {
            let Some(old) = r.tab.strip_prefix('t').and_then(|n| n.parse::<u64>().ok()) else {
                continue;
            };
            if r.session.is_none() && r.folder.is_none() {
                continue;
            }
            let what = match (&r.session, &r.folder) {
                (_, Some(f)) => f.clone(),
                (Some(s), None) => format!("session {}", &s[..8.min(s.len())]),
                _ => continue,
            };
            let folder = r.folder.as_ref().map(|f| crate::config::expand_tilde(f));
            let matches = |t: &crate::pane::Pane| {
                r.session.is_some() && t.session_id == r.session
                    || folder.as_ref().is_some_and(|f| *f == t.cwd)
            };
            let all = self.all_tabs();
            let now = all
                .iter()
                .find(|&&(s, t)| {
                    let p = &self.panes[s].tabs[t];
                    r.session.is_some() && p.session_id == r.session
                })
                .or_else(|| all.iter().find(|&&(s, t)| matches(&self.panes[s].tabs[t])))
                .copied();
            let same = self.find_tab(old).filter(|&(s, t)| {
                let p = &self.panes[s].tabs[t];
                match (&r.session, &p.session_id) {
                    (Some(a), Some(b)) => a == b,
                    _ => matches(p),
                }
            });
            if same.is_some() {
                continue;
            }
            let where_now = match now {
                Some((s, t)) => format!(
                    "{what} is {} on {}",
                    crate::control::tab_id(self.panes[s].tabs[t].uid),
                    self.describe_tab(s)
                ),
                None => format!("{what} is not open in a tab now"),
            };
            let other = self
                .find_tab(old)
                .map(|(s, t)| {
                    format!(
                        "{} is now \"{}\" on {}, a different tab; ",
                        r.tab,
                        crate::sessions::snippet(&self.panes[s].tabs[t].name(), 24),
                        self.describe_tab(s)
                    )
                })
                .unwrap_or_else(|| format!("{} is gone; ", r.tab));
            let line = format!("{other}{where_now}.");
            if self.find_tab(old).is_some() {
                self.assistant.stale_ids.insert(old, line.clone());
            }
            lines.push(line);
        }
        if lines.is_empty() {
            return String::new();
        }
        format!(
            "(Tab ids in that memory are from then and may name other tabs now: {} Use the ids in the state.)\n",
            lines.join(" ")
        )
    }

    /// The account the brain runs on now (its process, else the one it
    /// would start on).
    pub fn brain_account(&self) -> Option<usize> {
        self.assistant
            .brain
            .as_ref()
            .and_then(|b| b.account)
            .or_else(|| self.assistant_account())
    }

    /// The modes the user can flip in the UI at any time, stated every
    /// turn so nothing the conversation said earlier outlives them.
    pub fn modes_line(&self) -> String {
        let voice = match self.voice_mode() {
            0 => "off",
            1 => "push to talk",
            2 => "wake word",
            _ => "open mic",
        };
        let mut parts = vec![
            chrono::Local::now().format("now %a %H:%M").to_string(),
            format!("voice {voice}"),
        ];
        if self.voice.muted {
            parts.push("mic MUTED (the user unmutes it)".into());
        }
        parts.push(if self.privacy() {
            "privacy ON (never say an email address)".into()
        } else if self.show_email_global() {
            "emails shown".into()
        } else {
            "emails hidden".into()
        });
        let focus = self
            .panes
            .get(self.focus)
            .and_then(|p| p.account)
            .map(|a| format!("a{}", a + 1))
            .unwrap_or_else(|| "?".into());
        parts.push(format!(
            "layout {}{}, focus {focus}",
            self.layout_name(),
            if self.zoom {
                ", ZOOMED"
            } else {
                ", not zoomed"
            }
        ));
        let hidden: Vec<String> = self
            .panes
            .iter()
            .filter(|p| p.hidden)
            .filter_map(|p| p.account.map(|a| format!("a{}", a + 1)))
            .collect();
        if !hidden.is_empty() {
            parts.push(format!("hidden panes {}", hidden.join(" ")));
        }
        if self.memory_saver_on() {
            parts.push("memory saver on".into());
        }
        if !self.pending_confirms.iter().any(|p| p.answerable()) {
            parts.push("no question waits for a yes".into());
        }
        if !self.deliveries.iter().any(|d| !d.done()) {
            parts.push("no prompt queued or sending".into());
        }
        format!("{}\n", parts.join(" · "))
    }

    /// Send a typed request to the assistant.
    pub fn ask_assistant(&mut self, text: &str) {
        self.ask_assistant_from(text, None);
    }

    /// Send a request; `heard` is whisper's transcript when it was spoken.
    pub fn ask_assistant_from(&mut self, text: &str, heard: Option<&str>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.say_more(text) || self.cancel_follow_ups_said(text) {
            return;
        }
        self.start_turn(text, heard, None);
    }

    /// A turn GodTerm starts itself (a delegated answer arrived): `note`
    /// shows in the panel, `msg` goes to the brain.
    pub fn ask_assistant_system(&mut self, note: &str, msg: &str) {
        self.start_turn(msg, None, Some(note));
    }

    fn start_turn(&mut self, text: &str, heard: Option<&str>, system: Option<&str>) {
        self.assistant.cap_retried = false;
        self.assistant.last_turn = Some((
            text.to_string(),
            heard.map(str::to_string),
            system.map(str::to_string),
        ));
        self.assistant.unsaid = None;
        self.assistant.system_turn = system.is_some();
        // Spoken in, spoken out; a turn of our own (a tab finished...)
        // follows how the user last talked to it.
        if system.is_none() {
            self.assistant.turn_spoken = heard.is_some();
        }
        self.assistant.log.push(match system {
            Some(note) => Entry {
                who: Who::Note,
                text: note.to_string(),
            },
            None => Entry {
                who: Who::User,
                text: text.to_string(),
            },
        });
        // A turn of our own is never the user's yes.
        self.assistant.last_user = if system.is_some() {
            String::new()
        } else {
            text.to_string()
        };
        self.assistant.ignored_turn = false;
        self.trim_log();
        // New learned rules: the brain restarts with them, between turns.
        self.refresh_brain_for_rules();
        self.assistant.turn_reads = 0;
        if let Err(e) = self.ensure_brain() {
            self.note(format!("Could not start: {e}"));
            self.flash(format!("Assistant: {e}"));
            return;
        }
        // A new process remembers recent conversations (before this one's
        // request is written, so it is not repeated).
        let memory = if std::mem::take(&mut self.assistant.brain_fresh)
            && self.cfg.assistant.memory_days > 0
        {
            crate::assistant_memory::block(self.cfg.assistant.memory_days).map(|b| {
                let refs = crate::assistant_memory::recent_tab_refs(self.cfg.assistant.memory_days);
                format!("{b}{}", self.remembered_tabs_now(&refs))
            })
        } else {
            None
        };
        self.assistant_turn += 1;
        self.assistant_calls = 0;
        self.assistant.last_event = Some(Instant::now());
        self.assistant.turns_in_brain += 1;
        self.assistant.timing = Timing {
            asked: Some(Instant::now()),
            heard: self.assistant.heard_at.take(),
            stt_ms: self.voice.cur_stats_stt(),
            ..Default::default()
        };
        self.assistant.current.clear();
        self.assistant.block.clear();
        self.assistant.finals.clear();
        self.assistant.held.clear();
        self.assistant.tools_this_turn = 0;
        self.assistant.muted = false;
        self.assistant.got_delta = false;
        self.assistant.busy = true;
        self.voice.action = Some("asking the assistant...".into());
        let (account, model) = self
            .assistant
            .brain
            .as_ref()
            .map(|b| {
                (
                    b.account
                        .map(|a| self.cfg.accounts[a].display().to_string())
                        .unwrap_or_else(|| format!("{}'s own login", b.provider)),
                    b.model.clone(),
                )
            })
            .unwrap_or_default();
        let conv = self
            .assistant
            .conv
            .get_or_insert_with(crate::assistant_history::ConvLog::new);
        match heard {
            _ if system.is_some() => conv.write("system", serde_json::json!({"text": text, "account": account, "model": model})),
            Some(h) => conv.write("user", serde_json::json!({"text": text, "via": "voice", "heard": h, "account": account, "model": model})),
            None => conv.write("user", serde_json::json!({"text": text, "via": "typed", "account": account, "model": model})),
        }
        // The request first and the state last: the system prompt and the
        // conversation so far stay byte stable for prompt caching.
        let mut earlier = memory.unwrap_or_default();
        if let Some(c) = self.assistant.carry.take() {
            earlier.push_str(&format!("(Earlier in this conversation: {c})\n"));
        }
        if let Some(n) = self.assistant.resume_note.take() {
            earlier.push_str(&format!("{n}\n"));
        }
        if self.take_cut_off() {
            earlier.push_str("(The user talked over your previous answer and cut it off; nothing more of it runs. What they say now takes over: \"no, I meant...\" corrects that request.)\n");
        }
        self.assistant.queued_after_cut = self.assistant.stale > 0;
        // Spoken requests are marked: speech recognition mishears names.
        let spoken = if heard.is_some() { "(spoken) " } else { "" };
        let msg = format!("{earlier}{spoken}{text}\n\n{}", self.state_preamble());
        self.assistant.turn_remote = false;
        self.assistant.sent.push_back(msg.clone());
        if self.assistant.sent.len() > 20 {
            self.assistant.sent.pop_front();
        }
        let sent = self.assistant.brain.as_mut().map(|b| b.send(&msg));
        if let Some(Err(e)) = sent {
            self.assistant.busy = false;
            self.assistant.brain = None;
            self.note(format!("Could not send: {e}"));
        }
        if system.is_some() {
            crate::log::info(&format!(
                "assistant: system turn: {}",
                text.chars().take(300).collect::<String>()
            ));
        } else {
            crate::log::info(&format!("assistant: user: {text}"));
        }
    }

    /// "new conversation" / "forget that".
    pub fn reset_assistant(&mut self) {
        self.assistant.brain = None;
        self.assistant.conv = None;
        self.assistant.carry = None;
        self.last_target = None;
        self.assistant.busy = false;
        self.assistant.current.clear();
        self.pending_confirms.clear();
        self.note("New conversation.");
        self.flash("Assistant: new conversation");
    }

    /// From a tool call: switch once the current answer is done.
    pub fn set_assistant_account_after_turn(&mut self, a: Option<usize>) {
        if self.assistant.busy {
            self.assistant.account_after = Some(a);
        } else {
            self.set_assistant_account(a);
        }
    }

    /// Point it at another account (by index, or None for "best").
    pub fn set_assistant_account(&mut self, a: Option<usize>) {
        let name = a
            .map(|i| self.cfg.accounts[i].name.clone())
            .unwrap_or_else(|| "best".into());
        let _ = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("assistant.account"),
            Some(toml_edit::value(name.clone())),
        );
        self.cfg.assistant.account = name;
        self.assistant.brain = None;
        let label = a
            .map(|i| self.cfg.accounts[i].display().to_string())
            .unwrap_or_else(|| "the account with the most left".into());
        self.note(format!("Now on {label}."));
        self.flash(format!("Assistant uses {label} from now on"));
    }

    /// Whether this turn's answer is said out loud: not while the speaker
    /// is muted; a typed message only with assistant.speak_typed.
    pub fn reply_spoken(&self) -> bool {
        !self.cfg.voice.speaker_muted
            && (self.assistant.turn_spoken || self.cfg.assistant.speak_typed)
    }

    /// Whether replies are spoken at all.
    pub fn tts_on(&self) -> bool {
        self.voice.engine.as_ref().is_some_and(|v| v.tts) || self.cfg.voice.tts
    }

    /// Speak a line of the assistant's reply (queued after what is being
    /// said, unlike confirmations which cut in).
    pub fn speak_assistant(&mut self, text: &str) {
        if self.cfg.voice.speaker_muted {
            return;
        }
        self.assistant
            .timing
            .first_audio
            .get_or_insert_with(Instant::now);
        if let Some(v) = &self.voice.engine {
            if v.tts {
                v.say_queued(text);
                self.assistant.follow_up = true;
                return;
            }
        }
        // Tests never start a speaker: its worker loads the real voice
        // model, and a model still loading when the test process exits
        // crashed it (onnxruntime against exit time destructors).
        if self.voice.preview.is_none() && self.cfg.voice.tts && !cfg!(test) {
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.voice.preview = Some(crate::voice::tts::Speaker::start(&self.cfg.voice, flag));
        }
        if let Some(p) = &self.voice.preview {
            p.enqueue(text);
        }
    }

    pub fn assistant_log_tool(
        &mut self,
        tool: &str,
        args: &serde_json::Value,
        v: &serde_json::Value,
    ) {
        let status = if v.get("needs_confirmation").is_some() {
            "needs your yes".to_string()
        } else if v.get("_wait_uid").is_some() || v.get("_wait").is_some() {
            "opening".to_string()
        } else if v.get("ok") == Some(&serde_json::json!(true)) {
            "ok".to_string()
        } else {
            v.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("failed")
                .to_string()
        };
        // Shown as a readable chip; the raw arguments on a click.
        self.assistant.log.push(Entry {
            who: Who::Tool,
            text: serde_json::json!({"tool": tool, "args": args, "status": status}).to_string(),
        });
        self.trim_log();
        if let Some(c) = &self.assistant.conv {
            let r = v.to_string();
            let r: String = r.chars().take(800).collect();
            c.write(
                "tool",
                serde_json::json!({"name": tool, "args": args, "result": r}),
            );
        }
    }

    /// Speak a piece of the answer unless "stop" was said (or the model
    /// chose to ignore what it heard).
    fn say_part(&mut self, t: &str) {
        // A turn from Remote Control is answered there, not out loud here.
        if !self.assistant.muted
            && !self.assistant.ignored_turn
            && !self.assistant.turn_remote
            && self.reply_spoken()
            && !t.trim().is_empty()
        {
            self.assistant.spoken.push(t.trim().to_string());
            let n = self.assistant.spoken.len();
            if n > 20 {
                self.assistant.spoken.drain(..n - 20);
            }
            self.speak_assistant(t.trim());
        }
    }

    /// The text block streamed so far has ended. It is held: a tool call
    /// next makes it narration (shown dim, never spoken), the end of the
    /// turn makes it the answer. Nothing before the last tool call is
    /// spoken.
    fn end_block(&mut self, tool_next: bool) {
        let b = std::mem::take(&mut self.assistant.block).trim().to_string();
        if tool_next {
            if !b.is_empty() {
                self.assistant.log.push(Entry {
                    who: Who::Preamble,
                    text: b,
                });
            }
        } else if !b.is_empty() {
            self.assistant.held.push(b);
        }
        self.assistant.got_delta = false;
        self.refresh_current();
    }

    /// Speak the answer: narration dropped (shown dim), at most the
    /// spoken sentence limit, the rest offered. Returns the reply without
    /// its narration.
    fn speak_reply(&mut self, reply: &str) -> String {
        let max = self.cfg.assistant.spoken_sentences;
        let (say, narr, rest) = crate::assistant::spoken_part(reply, max);
        if !narr.is_empty() {
            self.assistant.log.push(Entry {
                who: Who::Preamble,
                text: narr.join(" "),
            });
        }
        let shown = match &rest {
            Some(r) => format!("{say} {r}"),
            None => say.clone(),
        };
        match rest {
            Some(rest) => {
                let offer = if say.contains('?') {
                    "Say more for the rest."
                } else {
                    "Want the rest?"
                };
                self.say_part(&format!("{say} {offer}"));
                self.assistant.unsaid = Some((Instant::now(), rest));
            }
            None => self.say_part(&say),
        }
        shown
    }

    /// "more", "go on", or a yes to "Want the rest?": say what the last
    /// reply left unsaid, without a turn.
    fn say_more(&mut self, text: &str) -> bool {
        let Some((at, rest)) = self.assistant.unsaid.clone() else {
            return false;
        };
        if at.elapsed() > Duration::from_secs(180) {
            self.assistant.unsaid = None;
            return false;
        }
        let c = text
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '\'')
            .collect::<String>();
        let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
        let more = [
            "more",
            "say more",
            "tell me more",
            "go on",
            "keep going",
            "continue",
            "the rest",
            "yes the rest",
            "read the rest",
            "say the rest",
            "details",
            "the details",
            "give me the details",
            "yes the details",
            "more please",
            "yes more",
        ];
        let yes = [
            "yes",
            "yeah",
            "yep",
            "sure",
            "yes please",
            "please",
            "ok",
            "okay",
        ];
        let offered_plainly = !self
            .assistant
            .log
            .iter()
            .rev()
            .find(|e| e.who == Who::Reply)
            .is_some_and(|e| {
                crate::assistant::spoken_part(&e.text, self.cfg.assistant.spoken_sentences)
                    .0
                    .contains('?')
            });
        // A yes with a question waiting answers that question instead.
        let yes_ok = offered_plainly && self.pending_confirms.is_empty();
        if !(more.contains(&c.as_str()) || (yes_ok && yes.contains(&c.as_str()))) {
            return false;
        }
        self.assistant.unsaid = None;
        self.assistant.log.push(Entry {
            who: Who::User,
            text: text.to_string(),
        });
        crate::log::info(&format!("assistant: said the rest ({} chars)", rest.len()));
        self.assistant.muted = false;
        self.say_part(&rest);
        self.assistant.follow_up = true;
        true
    }

    fn refresh_current(&mut self) {
        let mut parts: Vec<String> = self.assistant.held.clone();
        parts.extend(self.assistant.finals.iter().cloned());
        if !self.assistant.block.trim().is_empty() {
            parts.push(self.assistant.block.trim().to_string());
        }
        self.assistant.current = parts.join(" ");
    }

    /// The user talked over a reply: stop its speech, cut the turn off
    /// (claude interrupt), and take what they say next as a new request
    /// without the wake word.
    pub fn assistant_barge_in(&mut self, speech_ms: u32, stop_ms: u32) {
        crate::log::info(&format!(
            "assistant: barge-in after {speech_ms} ms of speech; audio stopped in {stop_ms} ms"
        ));
        if let Some(p) = &self.voice.preview {
            p.stop();
        }
        self.voice.wake_until = Some(Instant::now() + Duration::from_secs(8));
        self.voice.action = Some("listening (you cut in)".into());
        self.assistant.muted = true;
        if self.assistant.busy && self.assistant.stale == 0 {
            if let Some(b) = self.assistant.brain.as_mut() {
                match b.interrupt() {
                    Ok(()) => crate::log::info("assistant: interrupt sent to the brain"),
                    Err(e) => crate::log::info(&format!("assistant: interrupt failed: {e}")),
                }
            }
            self.assistant.stale += 1;
            self.assistant.last_event = Some(Instant::now());
            self.assistant.log.push(Entry {
                who: Who::Note,
                text: "(cut off; listening)".into(),
            });
        }
        self.assistant.cut_off = Some(Instant::now());
    }

    /// Whether the next request follows a barge-in: only one soon before
    /// it, sleep included.
    pub fn take_cut_off(&mut self) -> bool {
        self.assistant
            .cut_off
            .take()
            .is_some_and(|t| crate::clock::age(t) < CUT_OFF_FRESH)
    }

    pub fn on_brain(&mut self, ev: BrainEvent) {
        self.assistant.last_event = Some(Instant::now());
        match ev {
            BrainEvent::Control(id, r) => return self.on_remote_response(&id, r),
            BrainEvent::UserText(t) => return self.on_user_text(&t),
            _ => {}
        }
        // A cut off turn: drop its output; its end frees the next one.
        if self.assistant.stale > 0 && !matches!(ev, BrainEvent::Exited(_)) {
            if let BrainEvent::Done { text, .. } = &ev {
                self.assistant.stale -= 1;
                crate::log::info(&format!(
                    "assistant: cut off turn ended: {}",
                    text.chars().take(120).collect::<String>()
                ));
                if let Some(c) = &self.assistant.conv {
                    c.write(
                        "reply",
                        serde_json::json!({"text": text, "interrupted": true}),
                    );
                }
                if self.assistant.stale == 0 {
                    self.assistant.busy = std::mem::take(&mut self.assistant.queued_after_cut);
                    self.assistant.muted = false;
                }
            }
            return;
        }
        match ev {
            BrainEvent::Delta(t) => {
                self.assistant
                    .timing
                    .first_token
                    .get_or_insert_with(Instant::now);
                self.assistant.got_delta = true;
                self.assistant.block.push_str(&t);
                self.refresh_current();
            }
            BrainEvent::Text(t) => {
                // A whole block (after its deltas, or alone).
                if !self.assistant.got_delta {
                    self.assistant.block = t;
                }
                self.end_block(false);
            }
            BrainEvent::BlockStart(kind) => {
                if kind == "tool_use" && !self.assistant.block.trim().is_empty() {
                    self.end_block(true);
                }
            }
            BrainEvent::Tool(..) => {
                if !self.assistant.block.trim().is_empty() {
                    self.end_block(true);
                }
                // Text held before this tool was narration.
                for h in std::mem::take(&mut self.assistant.held) {
                    self.assistant.log.push(Entry {
                        who: Who::Preamble,
                        text: h,
                    });
                }
                self.assistant.tools_this_turn += 1;
                self.assistant
                    .timing
                    .first_tool
                    .get_or_insert_with(Instant::now);
            }
            BrainEvent::ToolResult(_) => {
                // Tool calls are logged by the control API itself.
            }
            BrainEvent::Done { text, cost, error } => {
                if !self.assistant.block.trim().is_empty() {
                    self.end_block(false);
                }
                // No tool followed the held text: it is the answer.
                for h in std::mem::take(&mut self.assistant.held) {
                    self.assistant.finals.push(h);
                }
                let reply = if self.assistant.finals.is_empty() {
                    text.trim().to_string()
                } else {
                    self.assistant.finals.join(" ")
                };
                self.assistant.finals.clear();
                self.assistant.current.clear();
                // A provider's login refused (Grok's own login): back to Claude.
                if error && self.cfg.assistant.provider != "claude" {
                    let low = format!("{reply} {text}").to_lowercase();
                    if [
                        "401",
                        "403",
                        "unauthorized",
                        "not logged in",
                        "login",
                        "authentication",
                        "invalid_grant",
                        "revoked",
                    ]
                    .iter()
                    .any(|k| low.contains(k))
                    {
                        self.provider_refused(&if reply.is_empty() {
                            text.clone()
                        } else {
                            reply.clone()
                        });
                    }
                }
                let (reply, error) = if crate::assistant::token_cap_hit(&reply)
                    || (error && crate::assistant::token_cap_hit(&text))
                {
                    if self.retry_with_higher_cap() {
                        return;
                    }
                    (
                        "That answer came out longer than my reply limit allows, even after raising it. Try asking for a shorter part, or raise Longest reply in Settings > Assistant.".to_string(),
                        true,
                    )
                } else {
                    (reply, error)
                };
                if self.assistant.ignored_turn {
                    // Not addressed to it: nothing said, nothing shown but a note.
                    if let Some(u) = self
                        .assistant
                        .log
                        .iter_mut()
                        .rev()
                        .find(|e| e.who == Who::User)
                    {
                        u.who = Who::Preamble;
                        u.text = format!("(ignored) {}", u.text);
                    }
                    self.voice.ignored += 1;
                    self.voice.action = Some("(not for the assistant, ignored)".into());
                    crate::log::info(&format!("assistant: ignored: {}", self.assistant.last_user));
                } else if !reply.is_empty() {
                    let reply = self.speak_reply(&reply);
                    // Voice off: a report of ours still reaches the user.
                    if self.assistant.system_turn && !self.tts_on() {
                        self.flash(format!(
                            "Assistant: {}",
                            crate::sessions::snippet(&reply, 160)
                        ));
                    }
                    crate::log::info(&format!(
                        "assistant: reply: {}",
                        reply.chars().take(300).collect::<String>()
                    ));
                    self.voice.action = Some(format!(
                        "assistant: {}",
                        crate::sessions::snippet(&reply, 80)
                    ));
                    self.assistant.log.push(Entry {
                        who: if error { Who::Note } else { Who::Reply },
                        text: reply.clone(),
                    });
                }
                self.assistant.busy = false;
                self.assistant.system_turn = false;
                self.assistant.turn_remote = false;
                self.assistant.asked_at = Some(Instant::now());
                self.assistant.last_cost = cost;
                let ep = self.cfg.assistant.endpoint_ms;
                let sum = self.assistant.timing.summary(ep);
                crate::log::info(&format!("assistant: timing: {sum}"));
                if let Some(c) = &self.assistant.conv {
                    c.write("reply", serde_json::json!({"text": reply, "error": error, "cost_usd": cost, "timing": sum}));
                }
                self.assistant.last_timing = Some(sum);
                self.trim_log();
                if let Some(id) = self.assistant.resume_after.take() {
                    self.resume_conversation(&id);
                }
                if std::mem::take(&mut self.assistant.reset_after_turn) {
                    self.reset_assistant();
                }
                if let Some(a) = self.assistant.account_after.take() {
                    self.set_assistant_account(a);
                }
            }
            BrainEvent::Control(..) | BrainEvent::UserText(_) => {}
            BrainEvent::Exited(tail) => {
                self.remote_lost("the assistant stopped");
                if self.assistant.brain.is_some() {
                    let why = tail
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("it stopped")
                        .to_string();
                    self.note(format!("The assistant stopped: {why}"));
                    crate::log::info(&format!("assistant: exited: {tail}"));
                }
                self.assistant.brain = None;
                self.assistant.busy = false;
                // A cut off turn cannot finish in a process that is gone.
                self.assistant.stale = 0;
                self.assistant.queued_after_cut = false;
                self.assistant.muted = false;
            }
        }
    }

    /// The reply hit the output token limit: start a brain with twice the
    /// limit (up to 32000) and send the same turn again, once. False when
    /// it was already retried or cannot go higher.
    fn retry_with_higher_cap(&mut self) -> bool {
        let cur = self
            .assistant
            .token_cap
            .unwrap_or(0)
            .max(self.cfg.assistant.max_output_tokens.max(64));
        let new = (cur * 2).min(crate::assistant::MAX_OUTPUT_CAP);
        let Some((text, heard, system)) = self.assistant.last_turn.clone() else {
            return false;
        };
        if self.assistant.cap_retried || new <= cur {
            crate::log::info(&format!(
                "assistant: reply exceeded the {cur} token limit again; giving up"
            ));
            return false;
        }
        crate::log::info(&format!(
            "assistant: reply exceeded the {cur} token limit; retrying with {new}"
        ));
        self.assistant.token_cap = Some(new);
        self.assistant.brain = None;
        self.assistant.busy = false;
        self.assistant.log.push(Entry {
            who: Who::Preamble,
            text: format!(
                "(that reply was too long for the {cur} token limit; trying again with {new})"
            ),
        });
        let n = self.assistant.log.len();
        self.start_turn(&text, heard.as_deref(), system.as_deref());
        // The request is shown once.
        if self.assistant.log.len() > n {
            self.assistant.log.remove(n);
        }
        self.assistant.cap_retried = true;
        true
    }

    /// End a turn the brain never finished: drop the process (it starts
    /// again on the next request), clear every cut off and busy flag.
    pub fn reset_hung_turn(&mut self, why: &str) {
        crate::log::info(&format!("assistant: {why}"));
        self.assistant.brain = None;
        self.assistant.stale = 0;
        self.assistant.queued_after_cut = false;
        self.assistant.busy = false;
        self.assistant.muted = false;
        self.assistant.last_event = None;
        self.assistant.carry = None;
        if self
            .voice
            .action
            .as_deref()
            .is_some_and(|a| a.starts_with("asking"))
        {
            self.voice.action = None;
        }
        self.note(why.to_string());
        self.flash(format!("Assistant: {why}"));
    }

    /// Deadlines for a turn: a cut off one must end within 8 s, an
    /// ordinary one must show some life within 60 s (tools waiting on a
    /// tab count as life).
    pub fn assistant_deadlines(&mut self) {
        let Some(at) = self.assistant.last_event else {
            return;
        };
        let quiet = at.elapsed();
        if self.assistant.stale > 0 && quiet > Duration::from_secs(8) {
            self.reset_hung_turn(
                "the cut off request did not stop, so the assistant was restarted",
            );
        } else if self.assistant.busy
            && self.assistant.stale == 0
            && self.ctl_waits.is_empty()
            && quiet > Duration::from_secs(60)
        {
            self.reset_hung_turn(
                "the assistant did not answer in 60 s, so it was restarted; ask again",
            );
        }
    }

    /// Warm start: bring the process up without sending anything.
    pub fn prewarm_assistant(&mut self) {
        if !self.assistant_on() || !self.cfg.assistant.prewarm || cfg!(test) {
            return;
        }
        if self.assistant.brain.is_some()
            || self
                .assistant
                .warm_tried
                .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        self.assistant.warm_tried = Some(Instant::now());
        if self
            .assistant_account()
            .is_some_and(|a| self.accounts[a].login.logged_in())
        {
            if let Err(e) = self.ensure_brain() {
                crate::log::info(&format!("assistant: warm start failed: {e}"));
            }
        }
    }

    /// Every second or so: move off a nearly empty account.
    pub fn assistant_tick(&mut self) {
        self.assistant_deadlines();
        self.follow_ups_tick();
        if self
            .assistant
            .memory_count
            .is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(30))
        {
            let n = crate::assistant_memory::count(self.cfg.assistant.memory_days);
            self.assistant.memory_count = Some((Instant::now(), n));
        }
        if !self.assistant.purged {
            self.assistant.purged = true;
            if !cfg!(test) {
                let gone = crate::tab_history::purge(self.cfg.assistant.history_days);
                if gone > 0 {
                    crate::log::info(&format!(
                        "tab history: dropped {gone} record(s) older than {} days",
                        self.cfg.assistant.history_days
                    ));
                }
                let n = crate::assistant_history::purge(self.cfg.assistant.history_days);
                if n > 0 {
                    crate::log::info(&format!(
                        "assistant history: deleted {n} conversation(s) older than {} days",
                        self.cfg.assistant.history_days
                    ));
                }
            }
        }
        self.prewarm_assistant();
        if self.assistant.brain.is_some() {
            // Keep the session index warm for the sessions tool.
            self.kick_index(false);
        }
        let Some(a) = self.assistant.brain.as_ref().and_then(|b| b.account) else {
            return;
        };
        let left = self.accounts[a].effective_left();
        if left.is_some_and(|l| l < 5.0) {
            if self.cfg.assistant.account == "best"
                && self.assistant_account() != Some(a)
                && !self.assistant.busy
            {
                self.assistant.brain = None;
                let to = self
                    .assistant_account()
                    .map(|i| self.cfg.accounts[i].display().to_string())
                    .unwrap_or_default();
                self.note(format!(
                    "Switched to {to}: the previous account was under 5% left."
                ));
            } else if !self.assistant.warned_low {
                self.assistant.warned_low = true;
                let msg = format!("The assistant's account {} is under 5% left; pick another in Settings > Assistant", self.cfg.accounts[a].display());
                self.note(msg.clone());
                self.flash(msg);
            }
        }
    }

    /// Ctrl-a .: open the panel (with the keys), give it the keys back
    /// when it is open without them, close it when it has them.
    pub fn toggle_assistant_panel(&mut self) {
        if !self.assistant.show {
            self.assistant.show = true;
            self.assistant.focused = true;
        } else if !self.assistant.focused {
            self.assistant.focused = true;
        } else {
            self.assistant.show = false;
            self.assistant.focused = false;
        }
    }

    /// The panel is open and typing goes to it.
    pub fn assistant_has_focus(&self) -> bool {
        self.assistant.show && self.assistant.focused
    }

    /// Keys while the panel is open: typing goes to its input.
    pub fn on_assistant_key(&mut self, k: KeyEvent) -> bool {
        if !self.assistant_has_focus() || k.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        if self.assistant.history.is_some() {
            return self.on_history_key(k);
        }
        match k.code {
            // Esc gives the keys back to the grid; the panel stays open.
            KeyCode::Esc => self.assistant.focused = false,
            // Shift or Alt+Enter, or an Enter inside a burst of keys (a
            // paste the terminal typed out): a new line, not a send.
            KeyCode::Enter
                if k.modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                    || self.key_burst.unwrap_or_else(|| {
                        self.assistant
                            .last_key_at
                            .is_some_and(|t| t.elapsed() < Duration::from_millis(15))
                    }) =>
            {
                self.assistant.input.push('\n');
                self.assistant.last_key_at = Some(Instant::now());
            }
            KeyCode::Enter => self.send_assistant_input(),
            KeyCode::Tab => self.assistant.input.push_str("    "),
            KeyCode::Up => {
                let n = self.assistant.input_hist.len();
                if n > 0 {
                    let i = self
                        .assistant
                        .hist_pos
                        .map(|p| p.saturating_sub(1))
                        .unwrap_or(n - 1);
                    self.assistant.hist_pos = Some(i);
                    self.assistant.input = self.assistant.input_hist[i].clone();
                }
            }
            KeyCode::Down => match self.assistant.hist_pos {
                Some(p) if p + 1 < self.assistant.input_hist.len() => {
                    self.assistant.hist_pos = Some(p + 1);
                    self.assistant.input = self.assistant.input_hist[p + 1].clone();
                }
                Some(_) => {
                    self.assistant.hist_pos = None;
                    self.assistant.input.clear();
                }
                None => {}
            },
            KeyCode::Backspace => {
                self.assistant.input.pop();
            }
            KeyCode::Char(c) => {
                self.assistant.input.push(c);
                self.assistant.last_key_at = Some(Instant::now());
                self.assistant.hist_pos = None;
            }
            _ => return false,
        }
        true
    }

    /// A paste into the panel's input: line ends kept (as \n), other
    /// control characters dropped, at most PASTE_MAX characters.
    pub fn assistant_paste(&mut self, text: &str) {
        const PASTE_MAX: usize = 16_000;
        let norm = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ");
        let clean: String = norm
            .chars()
            .filter(|c| *c == '\n' || !c.is_control())
            .collect();
        let have = self.assistant.input.chars().count();
        let room = PASTE_MAX.saturating_sub(have);
        let n = clean.chars().count();
        let take: String = clean.chars().take(room).collect();
        self.assistant.input.push_str(&take);
        self.assistant.hist_pos = None;
        if n > room {
            self.flash(format!(
                "Pasted the first {} KB of {} KB (the input holds {} KB)",
                room / 1000,
                n.div_ceil(1000),
                PASTE_MAX / 1000
            ));
        }
    }

    /// Enter or Send in the panel: the typed request goes out and joins
    /// the input history.
    pub fn send_assistant_input(&mut self) {
        let t = std::mem::take(&mut self.assistant.input);
        self.assistant.hist_pos = None;
        if t.trim().is_empty() {
            return;
        }
        if self.assistant.input_hist.last() != Some(&t) {
            self.assistant.input_hist.push(t.clone());
            if self.assistant.input_hist.len() > 50 {
                self.assistant.input_hist.remove(0);
            }
        }
        self.assistant.show_admin = false;
        self.ask_assistant(&t);
    }

    /// After a spoken reply ends, listen for the follow up (wake mode).
    pub fn assistant_follow_up(&mut self, speaking: bool) {
        if self.assistant.follow_up && !speaking && !self.assistant.busy {
            self.assistant.follow_up = false;
            if self.voice.always_on {
                self.voice.wake_until =
                    Some(Instant::now() + Duration::from_secs(self.cfg.assistant.follow_up_s));
                self.voice.action = Some("listening for your reply".into());
            }
        }
    }
}
