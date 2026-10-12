//! The assistant panel's input as a text field: the editing keys (as
//! Terminal.app and iTerm send them), image chips kept whole, and the
//! mouse (a click places the caret, a drag selects). The text model is
//! input_edit; the panel draws it in ui_assistant.
//!
//! Keys: Left/Right a character, Option+Left/Right (or ESC b / ESC f, or
//! Ctrl+Left/Right) a word, Home/End and Ctrl-E (Cmd+Left/Right when the
//! terminal sends Home/End) the line, Up/Down a row (the request history
//! from the first or last row), Shift with any of them selects. Backspace
//! and Delete, Option+Backspace and Ctrl-W a word back, Option+Delete (ESC
//! d) a word on, Ctrl-U to the line start, Ctrl-K to its end. Esc drops a
//! selection. Ctrl-A stays GodTerm's prefix.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::{App, Modal};
use crate::app_assistant::AssistantState;
use crate::hits::UiAction;
use crate::input_edit::{self, Ed};

/// Where the input's text was drawn: its first cell, rows shown, the
/// wrap width and the first row shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputGeo {
    /// The box (lead included): x0..right, y..y + h.
    pub x0: u16,
    pub right: u16,
    /// The text's first column.
    pub tx: u16,
    pub y: u16,
    pub h: u16,
    pub w: usize,
    pub top: usize,
}

impl InputGeo {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x0 && x < self.right && y >= self.y && y < self.y + self.h
    }
}

impl AssistantState {
    /// The input with its caret, for an edit.
    pub fn ed(&mut self) -> Ed<'_> {
        Ed::new(&mut self.input, &mut self.caret)
    }

    /// New text in the input, the caret at its end.
    pub fn set_input(&mut self, t: String) {
        self.input = t;
        let mut e = self.ed();
        let n = e.text.len();
        e.go(n, false);
    }
}

impl App {
    /// The wrap width the input was drawn at (none yet: its own lines).
    fn input_wrap(&self) -> usize {
        self.assistant
            .input_geo
            .get()
            .map_or(usize::MAX / 2, |g| g.w)
    }

    /// A caret inside a chip goes to its edge: the way it moved (`dir`
    /// -1 or 1), or the nearer one (0).
    fn snap_out_of_chip(&mut self, dir: i8) {
        let p = self.assistant.ed().pos();
        let Some((s, e, _)) = self
            .chip_spans()
            .into_iter()
            .find(|&(s, e, _)| p > s && p < e)
        else {
            return;
        };
        let to = match dir {
            d if d < 0 => s,
            d if d > 0 => e,
            _ if p - s <= e - p => s,
            _ => e,
        };
        self.assistant.caret.pos = to;
    }

    /// A Ctrl key in the input: true when it edited.
    pub(crate) fn input_ctrl_key(&mut self, k: KeyEvent) -> bool {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let mut dir = 0;
        {
            let mut ed = self.assistant.ed();
            match k.code {
                KeyCode::Char('e' | 'E') => ed.end(false),
                KeyCode::Char('w' | 'W') => ed.rubout(),
                KeyCode::Char('u' | 'U') => ed.kill_to_start(),
                KeyCode::Char('k' | 'K') => ed.kill_to_end(),
                KeyCode::Left => {
                    ed.word_left(shift);
                    dir = -1;
                }
                KeyCode::Right => {
                    ed.word_right(shift);
                    dir = 1;
                }
                KeyCode::Home => ed.go(0, shift),
                KeyCode::End => {
                    let n = ed.text.len();
                    ed.go(n, shift)
                }
                _ => return false,
            }
        }
        self.snap_out_of_chip(dir);
        true
    }

    /// Moves, selection and deletes (no Ctrl): true when it was one.
    pub(crate) fn input_edit_key(&mut self, k: KeyEvent) -> bool {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let w = self.input_wrap();
        let mut dir = 0;
        match k.code {
            // Esc drops the selection first.
            KeyCode::Esc if self.assistant.ed().selection().is_some() => {
                self.assistant.caret.anchor = None;
            }
            KeyCode::Left | KeyCode::Right => {
                let right = k.code == KeyCode::Right;
                let mut ed = self.assistant.ed();
                match (alt, right) {
                    (true, false) => ed.word_left(shift),
                    (true, true) => ed.word_right(shift),
                    (false, false) => ed.left(shift),
                    (false, true) => ed.right(shift),
                }
                dir = if right { 1 } else { -1 };
            }
            // ESC b / ESC f: Option+Left/Right in Terminal.app.
            KeyCode::Char(c @ ('b' | 'f' | 'B' | 'F')) if alt => {
                let mut ed = self.assistant.ed();
                let sel = shift || c.is_ascii_uppercase();
                if c.eq_ignore_ascii_case(&'b') {
                    ed.word_left(sel);
                    dir = -1;
                } else {
                    ed.word_right(sel);
                    dir = 1;
                }
            }
            KeyCode::Char('d') if alt => self.assistant.ed().delete_word_forward(),
            KeyCode::Home => self.assistant.ed().home(shift),
            KeyCode::End => self.assistant.ed().end(shift),
            KeyCode::Up | KeyCode::Down => {
                let down = k.code == KeyCode::Down;
                let moved = self.assistant.ed().vertical(down, w, shift);
                if !moved {
                    if shift {
                        // Shift+Up on the first row: to the start (the end).
                        let mut ed = self.assistant.ed();
                        let to = if down { ed.text.len() } else { 0 };
                        ed.go(to, true);
                    } else {
                        self.input_history(down);
                    }
                }
                self.snap_out_of_chip(0);
                return true;
            }
            KeyCode::Backspace => {
                if alt {
                    self.assistant.ed().delete_word_back();
                } else if !self.assistant.ed().delete_selection() && !self.backspace_chip() {
                    self.assistant.ed().backspace();
                }
            }
            KeyCode::Delete => {
                if alt {
                    self.assistant.ed().delete_word_forward();
                } else if !self.assistant.ed().delete_selection() && !self.delete_chip() {
                    self.assistant.ed().delete();
                }
            }
            _ => return false,
        }
        self.snap_out_of_chip(dir);
        true
    }

    /// The byte index under (x, y) in the input as drawn (clamped into it).
    fn input_pos_at(&self, g: InputGeo, x: u16, y: u16) -> usize {
        let text = &self.assistant.input;
        let rows = input_edit::rows(text, g.w);
        let r = if y < g.y {
            g.top.saturating_sub((g.y - y) as usize)
        } else {
            g.top + (y - g.y) as usize
        };
        if r >= rows.len() {
            return text.len();
        }
        input_edit::pos_at(text, &rows, r, x.saturating_sub(g.tx) as usize)
    }

    /// The mouse in the input: a click places the caret (Shift+click
    /// selects to it), a drag selects. True when it took the event.
    pub fn input_mouse(&mut self, m: &MouseEvent) -> bool {
        if self.assistant.input_drag {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(g) = self.assistant.input_geo.get() {
                        let p = self.input_pos_at(g, m.column, m.row);
                        let ed = self.assistant.ed();
                        let p = ed.snap(p);
                        ed.c.pos = p;
                        ed.c.goal = None;
                    }
                    return true;
                }
                MouseEventKind::Up(_) => {
                    self.assistant.input_drag = false;
                    return true;
                }
                _ => self.assistant.input_drag = false,
            }
        }
        if m.kind != MouseEventKind::Down(MouseButton::Left)
            || self.modal != Modal::None
            || !self.assistant.show
            || self.assistant.history.is_some()
        {
            return false;
        }
        let Some(g) = self
            .assistant
            .input_geo
            .get()
            .filter(|g| g.contains(m.column, m.row))
        else {
            return false;
        };
        // Nothing drawn over it there (only the panel itself).
        if self.region_at(m.column, m.row).map(|r| r.action) != Some(UiAction::AssistantFocus) {
            return false;
        }
        self.assistant.focused = true;
        self.selection = None;
        let p = self.input_pos_at(g, m.column, m.row);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        {
            let mut ed = self.assistant.ed();
            ed.go(p, shift);
        }
        self.snap_out_of_chip(0);
        if !shift {
            self.assistant.caret.anchor = Some(self.assistant.caret.pos);
        }
        self.assistant.input_drag = true;
        true
    }
}
