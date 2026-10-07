//! The learned rules in the app: the brain's tools for them (with the
//! checks that they come from the user and stay within the safety rules),
//! refreshing the brain when they change, and the Learned rules view
//! (Settings > Assistant, View ▾).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use serde_json::{json, Value};

use crate::app::{App, View};
use crate::hits::UiAction;
use crate::learned::{self, Store};
use crate::theme::{self, DIM, FAINT, FG, SEL_BG};

pub const TOOL_NAMES: &[&str] = &[
    "learn",
    "update_learning",
    "forget_learning",
    "list_learnings",
    "learning_history",
    "revert_learning",
];

#[derive(Debug, Clone, Default)]
pub struct LearnedUi {
    pub sel: usize,
    /// Showing the revision history instead of the rules.
    pub history: bool,
    /// Editing the selected rule's text.
    pub edit: Option<String>,
}

fn ok(v: Value) -> Value {
    json!({"ok": true, "result": v})
}

impl App {
    /// Run a learned rules tool; None when `tool` is not one of them.
    pub fn learned_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        if !TOOL_NAMES.contains(&tool) {
            return None;
        }
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let num = |k: &str| {
            args.get(k)
                .and_then(|v| {
                    v.as_u64().or_else(|| {
                        v.as_str()
                            .and_then(|x| x.trim_start_matches(['r', '#']).parse().ok())
                    })
                })
                .map(|n| n as u32)
        };
        let mut store = learned::load();
        let r = (|| -> Result<Value, String> {
            match tool {
                "list_learnings" => {
                    let all = args
                        .get("include_disabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let rules: Vec<Value> = store
                        .rules
                        .iter()
                        .filter(|r| all || r.enabled)
                        .map(learned::rule_json)
                        .collect();
                    let say = if rules.is_empty() {
                        "I haven't learned any rules yet.".to_string()
                    } else {
                        format!("{}.", crate::control::plural(rules.len(), "rule"))
                    };
                    Ok(ok(
                        json!({"rules": rules, "revision": store.rev, "say": say}),
                    ))
                }
                "learning_history" => {
                    let id = num("id");
                    let since = s("since")
                        .and_then(|w| crate::session_index::parse_when(&w, chrono::Local::now()));
                    let limit = args
                        .get("limit")
                        .and_then(Value::as_u64)
                        .unwrap_or(10)
                        .clamp(1, 50) as usize;
                    let revs: Vec<Value> = learned::history()
                        .into_iter()
                        .rev()
                        .filter(|h| id.is_none_or(|i| h.rule_id == i))
                        .filter(|h| since.is_none_or(|t| chrono::DateTime::parse_from_rfc3339(&h.time).map(|x| std::time::SystemTime::from(x) >= t).unwrap_or(true)))
                        .take(limit)
                        .map(|h| json!({"rev": h.rev, "time": h.time.chars().take(16).collect::<String>().replace('T', " "), "op": h.op, "rule": h.rule_id, "before": h.before.map(|b| b.text), "after": h.after.map(|a| a.text), "why": h.why, "by": h.by}))
                        .collect();
                    Ok(ok(json!({"revisions": revs})))
                }
                _ => {
                    // Changes come from the user's own words, in a turn the
                    // user started, never from what a tab or file said.
                    self.check_learning_source()?;
                    let by = "assistant";
                    let reason = s("reason").unwrap_or_default();
                    let (say, rev, id) = match tool {
                        "learn" => {
                            let text = s("text").ok_or("text is required")?;
                            let turn = format!(
                                "{}#{}",
                                self.assistant
                                    .conv
                                    .as_ref()
                                    .map(|c| c.id.clone())
                                    .unwrap_or_default(),
                                self.assistant_turn
                            );
                            let (id, rev) = store.add(
                                &text,
                                &reason,
                                s("scope").as_deref().unwrap_or("global"),
                                &turn,
                                by,
                            )?;
                            let mut say = format!(
                                "Got it, I'll remember: {}",
                                text.trim().trim_end_matches('.')
                            );
                            say.push_str(&format!(". (rule {id}, rev {rev})"));
                            (say, rev, id)
                        }
                        "update_learning" => {
                            let id = num("id").ok_or("id is required")?;
                            let text = s("text").ok_or("text is required")?;
                            let rev = store.edit(id, &text, &reason, by)?;
                            (
                                format!(
                                    "Updated rule {id}: {}. (rev {rev})",
                                    text.trim().trim_end_matches('.')
                                ),
                                rev,
                                id,
                            )
                        }
                        "forget_learning" => {
                            let id = num("id").ok_or("id is required")?;
                            let rev = store.set_enabled(id, false, &reason, by)?;
                            (
                                format!("Okay, I've dropped rule {id}. (rev {rev})"),
                                rev,
                                id,
                            )
                        }
                        _ => {
                            let rev = num("rev").ok_or("rev is required")?;
                            let (id, new) = store.revert(rev, by)?;
                            (
                                format!("Undid revision {rev} of rule {id}. (rev {new})"),
                                new,
                                id,
                            )
                        }
                    };
                    self.learned_changed(&say);
                    let mut v = json!({"rule": id, "revision": rev, "say": say});
                    if learned::over_cap(&store) {
                        v["consolidate"] = json!(format!("{} rules is near the limit: merge related ones with update_learning and forget_learning.", store.enabled().len()));
                    }
                    Ok(ok(v))
                }
            }
        })();
        Some(r)
    }

    /// A change to the rules must come from the user in this turn: a turn
    /// they started by speaking or typing, whose words teach something,
    /// and not one where tab or file contents could have planted it.
    fn check_learning_source(&self) -> Result<(), String> {
        let said = self.assistant.last_user.trim();
        if said.is_empty() || self.assistant.conv.is_none() {
            return Err(
                "refused: rules are only learned from something the user said in this conversation"
                    .into(),
            );
        }
        if !learned::user_teaches(said) {
            return Err("refused: the user's last message does not correct you or state a preference; only learn from what they say".into());
        }
        if self.assistant.turn_reads > 0
            && !said.to_lowercase().contains("remember")
            && !said.to_lowercase().contains("learn")
        {
            // Text from tabs or files is in this turn: only an explicit
            // "remember" / "learn" from the user makes it a rule.
            return Err("refused: this turn read tab or file contents; a rule must come from the user's own words (they can say \"remember that ...\")".into());
        }
        Ok(())
    }

    /// The rules changed: a toast, and the brain picks them up at the next
    /// turn boundary (its system prompt changes then, not mid turn).
    pub fn learned_changed(&mut self, say: &str) {
        self.assistant.prompt_dirty = true;
        self.assistant.log.push(crate::app_assistant::Entry {
            who: crate::app_assistant::Who::Note,
            text: format!("Learned: {say}"),
        });
        self.flash(format!("Assistant learned: {say}"));
    }

    /// At a turn boundary: restart the brain if its rules changed, keeping
    /// the conversation (it is carried over like any reset).
    pub fn refresh_brain_for_rules(&mut self) {
        if !self.assistant.prompt_dirty || self.assistant.busy || self.assistant.brain.is_none() {
            return;
        }
        self.assistant.prompt_dirty = false;
        if let Some(c) = &self.assistant.conv {
            let carry =
                crate::assistant_history::carry_summary(&crate::assistant_history::read(&c.path));
            self.assistant.carry = (!carry.is_empty()).then_some(carry);
        }
        self.assistant.brain = None;
        crate::log::info("assistant: learned rules changed, the brain restarts with them");
    }

    pub fn open_learned_view(&mut self) {
        self.view = if self.view == View::Learned {
            View::Grid
        } else {
            View::Learned
        };
        self.learned_ui = LearnedUi::default();
    }

    pub fn on_learned_key(&mut self, k: KeyEvent) {
        if let Some(buf) = self.learned_ui.edit.as_mut() {
            match k.code {
                KeyCode::Esc => self.learned_ui.edit = None,
                KeyCode::Enter => {
                    let text = buf.clone();
                    self.learned_ui.edit = None;
                    if let Some(r) = learned::load().rules.get(self.learned_ui.sel).cloned() {
                        let mut s = learned::load();
                        match s.edit(r.id, &text, "edited in Settings", "user") {
                            Ok(rev) => {
                                self.learned_changed(&format!("rule {} edited (rev {rev})", r.id))
                            }
                            Err(e) => self.flash(e),
                        }
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => buf.push(c),
                _ => {}
            }
            return;
        }
        let store = learned::load();
        let n = if self.learned_ui.history {
            learned::history().len()
        } else {
            store.rules.len()
        };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.view = View::Grid,
            KeyCode::Up | KeyCode::Char('k') => {
                self.learned_ui.sel = self.learned_ui.sel.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.learned_ui.sel = (self.learned_ui.sel + 1).min(n.saturating_sub(1))
            }
            KeyCode::Char('h') | KeyCode::Tab => {
                self.learned_ui.history = !self.learned_ui.history;
                self.learned_ui.sel = 0;
            }
            KeyCode::Char(' ') | KeyCode::Char('t') if !self.learned_ui.history => {
                self.learned_toggle(self.learned_ui.sel)
            }
            KeyCode::Char('e') | KeyCode::Enter if !self.learned_ui.history => {
                if let Some(r) = store.rules.get(self.learned_ui.sel) {
                    self.learned_ui.edit = Some(r.text.clone());
                }
            }
            KeyCode::Char('d') | KeyCode::Delete if !self.learned_ui.history => {
                if let Some(r) = store.rules.get(self.learned_ui.sel).filter(|r| r.enabled) {
                    let mut s = learned::load();
                    if let Ok(rev) = s.set_enabled(r.id, false, "deleted in Settings", "user") {
                        self.learned_changed(&format!("rule {} turned off (rev {rev})", r.id));
                    }
                }
            }
            KeyCode::Char('r') | KeyCode::Enter if self.learned_ui.history => {
                let revs: Vec<learned::Revision> = learned::history().into_iter().rev().collect();
                if let Some(h) = revs.get(self.learned_ui.sel) {
                    let mut s = learned::load();
                    match s.revert(h.rev, "user") {
                        Ok((id, rev)) => {
                            self.learned_changed(&format!("rule {id} reverted (rev {rev})"))
                        }
                        Err(e) => self.flash(e),
                    }
                }
            }
            _ => {}
        }
    }

    pub fn learned_toggle(&mut self, i: usize) {
        let store = learned::load();
        let Some(r) = store.rules.get(i).cloned() else {
            return;
        };
        let mut s = store;
        match s.set_enabled(r.id, !r.enabled, "toggled in Settings", "user") {
            Ok(rev) => self.learned_changed(&format!(
                "rule {} {} (rev {rev})",
                r.id,
                if r.enabled { "off" } else { "on" }
            )),
            Err(e) => self.flash(e),
        }
    }
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    if area.height < 4 {
        return;
    }
    let buf = f.buffer_mut();
    let limit = area.x + area.width;
    let store: Store = learned::load();
    let mut hits = app.hits.borrow_mut();
    let ui = &app.learned_ui;
    let mut x = crate::hits::text(
        buf,
        area.x,
        area.y,
        limit,
        &format!(
            " Learned rules: {} on, revision {}  ",
            store.enabled().len(),
            store.rev
        ),
        Style::default().fg(FG).add_modifier(Modifier::BOLD),
    );
    let tabs: [(&str, bool); 2] = [("Rules", !ui.history), ("History", ui.history)];
    for (label, on) in tabs {
        let shown = if on {
            format!("[{label}]")
        } else {
            label.to_string()
        };
        x = crate::hits::button(
            buf,
            &mut hits,
            app.mouse_pos,
            x,
            area.y,
            limit,
            &shown,
            UiAction::LearnedKey('h'),
            "Rules or their revision history (h)",
            theme::SLATE,
        ) + 1;
    }
    let hint = if ui.history {
        "↑↓ choose · Enter / r revert to before it · h rules"
    } else {
        "space toggles · e edit · d delete (turns it off) · h history"
    };
    crate::hits::text(buf, x + 1, area.y, limit, hint, Style::default().fg(FAINT));
    let top = area.y + 2;
    let rows = area.bottom().saturating_sub(top) as usize;
    if !ui.history {
        if store.rules.is_empty() {
            crate::hits::text(
                buf,
                area.x + 2,
                top,
                limit,
                "No rules yet. Correct the assistant (\"next time, always ...\") and it learns.",
                Style::default().fg(FAINT),
            );
        }
        let first = ui.sel.saturating_sub(rows.saturating_sub(1));
        for (k, (i, r)) in store
            .rules
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .enumerate()
        {
            let y = top + k as u16;
            let on = i == ui.sel;
            let st = if on {
                Style::default().fg(FG).bg(SEL_BG)
            } else if r.enabled {
                Style::default().fg(FG)
            } else {
                Style::default().fg(DIM)
            };
            let text = match (&ui.edit, on) {
                (Some(e), true) => format!("{e}▏"),
                _ => r.text.clone(),
            };
            let line = format!(
                "{} [{}] {:>3}  {}   {}",
                if on { "›" } else { " " },
                if r.enabled { "x" } else { " " },
                r.id,
                text,
                crate::sessions::snippet(&r.reason, 40)
            );
            crate::hits::text(buf, area.x + 1, y, limit, &line, st);
            hits.add(
                Rect::new(area.x, y, area.width, 1),
                UiAction::LearnedRow(i),
                "Select (click the box to toggle)",
            );
            hits.add(
                Rect::new(area.x + 3, y, 3, 1),
                UiAction::LearnedToggle(i),
                "Turn this rule on or off",
            );
        }
    } else {
        let revs: Vec<learned::Revision> = learned::history().into_iter().rev().collect();
        let mut y = top;
        for (i, h) in revs
            .iter()
            .enumerate()
            .skip(ui.sel.saturating_sub(rows / 3))
        {
            if y + 2 >= area.bottom() {
                break;
            }
            let on = i == ui.sel;
            let st = if on {
                Style::default().fg(FG).bg(SEL_BG)
            } else {
                Style::default().fg(FG)
            };
            crate::hits::text(
                buf,
                area.x + 1,
                y,
                limit,
                &format!(
                    "{} rev {}  {}  {} rule {} by {}  {}",
                    if on { "›" } else { " " },
                    h.rev,
                    h.time
                        .chars()
                        .take(16)
                        .collect::<String>()
                        .replace('T', " "),
                    h.op,
                    h.rule_id,
                    h.by,
                    crate::sessions::snippet(&h.why, 50)
                ),
                st,
            );
            hits.add(
                Rect::new(area.x, y, area.width, 1),
                UiAction::LearnedRow(i),
                "Select (Enter reverts to before it)",
            );
            let before = h
                .before
                .as_ref()
                .map(|b| format!("{}{}", if b.enabled { "" } else { "(off) " }, b.text))
                .unwrap_or_else(|| "(none)".into());
            let after = h
                .after
                .as_ref()
                .map(|a| format!("{}{}", if a.enabled { "" } else { "(off) " }, a.text))
                .unwrap_or_else(|| "(none)".into());
            crate::hits::text(
                buf,
                area.x + 5,
                y + 1,
                limit,
                &format!("- {before}"),
                Style::default().fg(theme::CLAY),
            );
            crate::hits::text(
                buf,
                area.x + 5,
                y + 2,
                limit,
                &format!("+ {after}"),
                Style::default().fg(theme::SAGE),
            );
            y += 3;
        }
        if revs.is_empty() {
            crate::hits::text(
                buf,
                area.x + 2,
                top,
                limit,
                "No changes yet.",
                Style::default().fg(FAINT),
            );
        }
    }
}
