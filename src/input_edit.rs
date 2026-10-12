//! Editing the assistant panel's input: a caret (a byte index, always on
//! a grapheme boundary) and an optional selection anchor over a String,
//! the moves and edits the keys make, and the wrapped layout the panel
//! draws (rows, the caret's row and column, a click back to a byte index).
//! Wide characters and emoji count by their cells.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Where the caret is in the input, and where a selection started.
#[derive(Debug, Default, Clone)]
pub struct Caret {
    pub pos: usize,
    pub anchor: Option<usize>,
    /// The column Up and Down aim for (kept over a run of them).
    pub goal: Option<usize>,
    /// The text length this caret was set for: text changed elsewhere
    /// (cleared, sent, set to "yes") puts the caret at its end.
    seen: Option<usize>,
}

/// One visual row of the wrapped input: bytes start..end of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
    /// The row was wrapped: the next row goes on the same line.
    pub soft: bool,
}

/// The cells a grapheme takes (as ratatui draws it).
pub fn gw(g: &str) -> usize {
    g.width()
}

/// The text as rows of at most `w` cells: its own lines, wrapped.
pub fn rows(text: &str, w: usize) -> Vec<Row> {
    let w = w.max(1);
    let mut out = vec![];
    let mut base = 0;
    for line in text.split('\n') {
        let mut start = base;
        let mut n = 0;
        for (i, g) in line.grapheme_indices(true) {
            let cw = gw(g);
            if n + cw > w && base + i > start {
                out.push(Row {
                    start,
                    end: base + i,
                    soft: true,
                });
                start = base + i;
                n = 0;
            }
            n += cw;
        }
        out.push(Row {
            start,
            end: base + line.len(),
            soft: false,
        });
        base += line.len() + 1;
    }
    out
}

/// The caret's (row, column) in `rows`. A caret after a full row is at
/// the start of the next one (which may be past the last row).
pub fn caret_rc(text: &str, rows: &[Row], pos: usize, w: usize) -> (usize, usize) {
    for (r, row) in rows.iter().enumerate() {
        if pos < row.start {
            continue;
        }
        if pos < row.end || (pos == row.end && !row.soft) {
            let col = text[row.start..pos].width();
            if col >= w.max(1) && pos == row.end {
                return (r + 1, 0);
            }
            return (r, col);
        }
    }
    (rows.len().saturating_sub(1), 0)
}

/// The byte index nearest to column `col` of row `r` (a click there, or
/// Up/Down aiming at it). A wrapped row ends before its last grapheme
/// (its end is drawn as the next row's start).
pub fn pos_at(text: &str, rows: &[Row], r: usize, col: usize) -> usize {
    let Some(row) = rows.get(r.min(rows.len().saturating_sub(1))) else {
        return 0;
    };
    let mut n = 0;
    let mut last = row.start;
    for (i, g) in text[row.start..row.end].grapheme_indices(true) {
        let at = row.start + i;
        let cw = gw(g);
        if col < n + cw {
            // The right half of a wide character: after it.
            return if (col - n) * 2 < cw.max(1) {
                at
            } else {
                at + g.len()
            };
        }
        n += cw;
        last = at;
    }
    if row.soft && row.end > row.start {
        last
    } else {
        row.end
    }
}

/// The grapheme boundary at or before `i`.
pub fn snap(text: &str, i: usize) -> usize {
    let i = i.min(text.len());
    if i == text.len() {
        return i;
    }
    let mut best = 0;
    for (j, _) in text.grapheme_indices(true) {
        if j > i {
            break;
        }
        best = j;
    }
    best
}

impl Caret {
    /// The caret and selection over `text` as the next edit will see
    /// them (for drawing).
    pub fn view(&self, text: &str) -> (usize, Option<(usize, usize)>) {
        if self.seen != Some(text.len()) {
            return (text.len(), None);
        }
        let p = snap(text, self.pos);
        let sel = self
            .anchor
            .map(|a| snap(text, a))
            .filter(|&a| a != p)
            .map(|a| (a.min(p), a.max(p)));
        (p, sel)
    }
}

fn is_word(g: &str) -> bool {
    g.chars().any(|c| c.is_alphanumeric() || c == '_')
}

/// The text with its caret, for one edit (the caret is synced first).
pub struct Ed<'a> {
    pub text: &'a mut String,
    pub c: &'a mut Caret,
}

impl Drop for Ed<'_> {
    fn drop(&mut self) {
        self.c.seen = Some(self.text.len());
    }
}

impl<'a> Ed<'a> {
    pub fn new(text: &'a mut String, c: &'a mut Caret) -> Self {
        let e = Ed { text, c };
        if e.c.seen != Some(e.text.len()) {
            e.c.pos = e.text.len();
            e.c.anchor = None;
            e.c.goal = None;
        }
        e.c.pos = e.snap(e.c.pos);
        e.c.anchor = e.c.anchor.map(|a| e.snap(a));
        e
    }

    pub fn pos(&self) -> usize {
        self.c.pos
    }

    /// The grapheme boundary at or before `i`.
    pub fn snap(&self, i: usize) -> usize {
        snap(self.text, i)
    }

    pub fn prev(&self, i: usize) -> usize {
        self.text[..i]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(j, _)| j)
    }

    pub fn next(&self, i: usize) -> usize {
        self.text[i..]
            .graphemes(true)
            .next()
            .map_or(self.text.len(), |g| i + g.len())
    }

    /// The selected bytes (start, end), when there are any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.c.anchor?;
        let p = self.c.pos;
        (a != p).then(|| (a.min(p), a.max(p)))
    }

    /// Put the caret at `to`; `extend` grows the selection to it.
    pub fn go(&mut self, to: usize, extend: bool) {
        if extend {
            self.c.anchor.get_or_insert(self.c.pos);
        } else {
            self.c.anchor = None;
        }
        self.c.pos = self.snap(to);
        self.c.goal = None;
    }

    pub fn left(&mut self, extend: bool) {
        match self.selection() {
            Some((a, _)) if !extend => self.go(a, false),
            _ => self.go(self.prev(self.c.pos), extend),
        }
    }

    pub fn right(&mut self, extend: bool) {
        match self.selection() {
            Some((_, b)) if !extend => self.go(b, false),
            _ => self.go(self.next(self.c.pos), extend),
        }
    }

    /// The start of the word before `i` (punctuation and spaces skipped).
    pub fn word_start(&self, mut i: usize) -> usize {
        while i > 0 {
            let j = self.prev(i);
            if is_word(&self.text[j..i]) {
                break;
            }
            i = j;
        }
        while i > 0 {
            let j = self.prev(i);
            if !is_word(&self.text[j..i]) {
                break;
            }
            i = j;
        }
        i
    }

    /// The end of the word after `i`.
    pub fn word_end(&self, mut i: usize) -> usize {
        let n = self.text.len();
        while i < n {
            let j = self.next(i);
            if is_word(&self.text[i..j]) {
                break;
            }
            i = j;
        }
        while i < n {
            let j = self.next(i);
            if !is_word(&self.text[i..j]) {
                break;
            }
            i = j;
        }
        i
    }

    pub fn word_left(&mut self, extend: bool) {
        self.go(self.word_start(self.c.pos), extend);
    }

    pub fn word_right(&mut self, extend: bool) {
        self.go(self.word_end(self.c.pos), extend);
    }

    /// The start and end of the caret's own line.
    pub fn line_start(&self) -> usize {
        self.text[..self.c.pos].rfind('\n').map_or(0, |i| i + 1)
    }

    pub fn line_end(&self) -> usize {
        self.text[self.c.pos..]
            .find('\n')
            .map_or(self.text.len(), |i| self.c.pos + i)
    }

    pub fn home(&mut self, extend: bool) {
        self.go(self.line_start(), extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.go(self.line_end(), extend);
    }

    /// Up (or Down) a visual row at `w` cells; false on the first (last)
    /// row, where it does nothing (the history is there).
    pub fn vertical(&mut self, down: bool, w: usize, extend: bool) -> bool {
        let rs = rows(self.text, w);
        let (r, col) = caret_rc(self.text, &rs, self.c.pos, w);
        let to = if down {
            if r + 1 >= rs.len() {
                return false;
            }
            r + 1
        } else {
            if r == 0 {
                return false;
            }
            r - 1
        };
        let goal = self.c.goal.unwrap_or(col);
        let p = pos_at(self.text, &rs, to, goal);
        self.go(p, extend);
        self.c.goal = Some(goal);
        true
    }

    /// Remove bytes a..b; the caret goes to a.
    pub fn remove(&mut self, a: usize, b: usize) {
        self.text.replace_range(a..b, "");
        self.c.pos = a;
        self.c.anchor = None;
        self.c.goal = None;
    }

    /// Remove the selection; true when there was one.
    pub fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((a, b)) => {
                self.remove(a, b);
                true
            }
            None => {
                self.c.anchor = None;
                false
            }
        }
    }

    /// Typed or pasted text at the caret (over the selection).
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let p = self.c.pos;
        self.text.insert_str(p, s);
        self.c.pos = p + s.len();
        self.c.goal = None;
    }

    pub fn backspace(&mut self) {
        if !self.delete_selection() && self.c.pos > 0 {
            let p = self.c.pos;
            self.remove(self.prev(p), p);
        }
    }

    pub fn delete(&mut self) {
        if !self.delete_selection() && self.c.pos < self.text.len() {
            let p = self.c.pos;
            self.remove(p, self.next(p));
        }
    }

    /// Option+Backspace: the word before the caret.
    pub fn delete_word_back(&mut self) {
        if !self.delete_selection() {
            let p = self.c.pos;
            self.remove(self.word_start(p), p);
        }
    }

    /// Option+Delete: the word after the caret.
    pub fn delete_word_forward(&mut self) {
        if !self.delete_selection() {
            let p = self.c.pos;
            self.remove(p, self.word_end(p));
        }
    }

    /// Ctrl-W: back to the previous space (the shell's word).
    pub fn rubout(&mut self) {
        if self.delete_selection() {
            return;
        }
        let p = self.c.pos;
        let mut i = p;
        let ws = |s: &str| s.chars().all(char::is_whitespace);
        while i > 0 && ws(&self.text[self.prev(i)..i]) {
            i = self.prev(i);
        }
        while i > 0 && !ws(&self.text[self.prev(i)..i]) {
            i = self.prev(i);
        }
        self.remove(i, p);
    }

    /// Ctrl-U: to the start of the line.
    pub fn kill_to_start(&mut self) {
        if !self.delete_selection() {
            let p = self.c.pos;
            self.remove(self.line_start(), p);
        }
    }

    /// Ctrl-K: to the end of the line (at its end: the line break).
    pub fn kill_to_end(&mut self) {
        if !self.delete_selection() {
            let p = self.c.pos;
            let e = self.line_end();
            let e = if e == p { self.next(p) } else { e };
            self.remove(p, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str, pos: usize) -> (String, Caret) {
        let mut t = s.to_string();
        let mut c = Caret::default();
        {
            let mut e = Ed::new(&mut t, &mut c);
            e.go(pos, false);
        }
        (t, c)
    }

    #[test]
    fn caret_moves_by_character_word_and_line() {
        let (mut t, mut c) = at("hello brave world", 0);
        let mut e = Ed::new(&mut t, &mut c);
        e.right(false);
        e.right(false);
        assert_eq!(e.pos(), 2);
        e.left(false);
        assert_eq!(e.pos(), 1);
        e.word_right(false);
        assert_eq!(e.pos(), 5);
        e.word_right(false);
        assert_eq!(e.pos(), 11);
        e.word_left(false);
        assert_eq!(e.pos(), 6);
        e.word_left(false);
        e.word_left(false);
        assert_eq!(e.pos(), 0);
        e.left(false);
        assert_eq!(e.pos(), 0, "stops at the start");
        e.end(false);
        assert_eq!(e.pos(), 17);
        e.right(false);
        assert_eq!(e.pos(), 17, "stops at the end");
        e.home(false);
        assert_eq!(e.pos(), 0);
    }

    #[test]
    fn home_and_end_are_the_caret_line() {
        let (mut t, mut c) = at("one\ntwo three\nfour", 6);
        let mut e = Ed::new(&mut t, &mut c);
        e.home(false);
        assert_eq!(e.pos(), 4);
        e.end(false);
        assert_eq!(e.pos(), 13);
        e.kill_to_start();
        assert_eq!(e.text.as_str(), "one\n\nfour");
        e.kill_to_end();
        assert_eq!(e.text.as_str(), "one\nfour", "at the line end: the break");
    }

    #[test]
    fn insert_and_delete_at_the_caret() {
        let (mut t, mut c) = at("helo", 3);
        let mut e = Ed::new(&mut t, &mut c);
        e.insert("l");
        assert_eq!((e.text.as_str(), e.pos()), ("hello", 4));
        e.backspace();
        e.backspace();
        assert_eq!((e.text.as_str(), e.pos()), ("heo", 2));
        e.delete();
        assert_eq!((e.text.as_str(), e.pos()), ("he", 2));
        e.go(0, false);
        e.delete();
        assert_eq!((e.text.as_str(), e.pos()), ("e", 0));
        e.backspace();
        assert_eq!(e.text.as_str(), "e", "nothing before the caret");
    }

    #[test]
    fn word_deletes() {
        let (mut t, mut c) = at("git commit -m 'fix it'", 13);
        let mut e = Ed::new(&mut t, &mut c);
        e.rubout();
        assert_eq!(e.text.as_str(), "git commit  'fix it'");
        e.delete_word_back();
        assert_eq!(e.text.as_str(), "git  'fix it'");
        e.delete_word_forward();
        assert_eq!(e.text.as_str(), "git  it'");
    }

    #[test]
    fn selection_extends_and_is_replaced() {
        let (mut t, mut c) = at("hello world", 6);
        let mut e = Ed::new(&mut t, &mut c);
        e.word_right(true);
        assert_eq!(e.selection(), Some((6, 11)));
        e.insert("there");
        assert_eq!(e.text.as_str(), "hello there");
        assert_eq!(e.selection(), None);
        e.home(true);
        e.right(true);
        assert_eq!(e.selection(), Some((1, 11)));
        // A plain arrow collapses it to its side.
        e.left(false);
        assert_eq!((e.pos(), e.selection()), (1, None));
        e.end(true);
        e.backspace();
        assert_eq!(e.text.as_str(), "h");
    }

    #[test]
    fn wide_characters_and_emoji_move_as_one() {
        // A family emoji (a ZWJ sequence), a flag, a combining accent, CJK.
        let s = "a👨‍👩‍👧b🇯🇵e\u{301}漢字";
        let (mut t, mut c) = at(s, 0);
        let mut e = Ed::new(&mut t, &mut c);
        let mut stops = vec![0];
        for _ in 0..7 {
            e.right(false);
            stops.push(e.pos());
        }
        let want: Vec<usize> = std::iter::once(0)
            .chain(s.grapheme_indices(true).map(|(i, g)| i + g.len()))
            .collect();
        assert_eq!(stops, want);
        assert_eq!(s.graphemes(true).count(), 7);
        e.backspace();
        assert_eq!(e.text.as_str(), "a👨‍👩‍👧b🇯🇵e\u{301}漢");
        e.go(1, false);
        e.delete();
        assert_eq!(e.text.as_str(), "ab🇯🇵e\u{301}漢");
        // A caret set inside a grapheme snaps back to its start.
        e.go(3, false);
        assert_eq!(e.pos(), 2);
    }

    #[test]
    fn rows_and_caret_through_the_wrap() {
        let s = "abcdef\n漢字漢字x";
        let rs = rows(s, 4);
        let cut: Vec<&str> = rs.iter().map(|r| &s[r.start..r.end]).collect();
        assert_eq!(cut, ["abcd", "ef", "漢字", "漢字", "x"]);
        // After "abcd" (a full row): the next row's start.
        assert_eq!(caret_rc(s, &rs, 4, 4), (1, 0));
        assert_eq!(caret_rc(s, &rs, 6, 4), (1, 2));
        // Wide characters are two cells.
        assert_eq!(caret_rc(s, &rs, 7 + 3, 4), (2, 2));
        // A full last row: the caret goes under it.
        let rs = rows("abcd", 4);
        assert_eq!(caret_rc("abcd", &rs, 4, 4), (1, 0));
        // A click: the cell's character, the right half of a wide one after it.
        let rs = rows(s, 4);
        assert_eq!(pos_at(s, &rs, 2, 0), 7);
        assert_eq!(pos_at(s, &rs, 2, 1), 10);
        assert_eq!(pos_at(s, &rs, 2, 2), 10);
        assert_eq!(pos_at(s, &rs, 1, 9), 6, "past the end of a line");
        assert_eq!(
            pos_at(s, &rs, 0, 9),
            3,
            "a wrapped row ends before its last"
        );
    }

    #[test]
    fn up_and_down_across_wrapped_rows() {
        let s = "abcdefghij\nxy";
        let (mut t, mut c) = at(s, 7);
        let mut e = Ed::new(&mut t, &mut c);
        // Rows of 4: "abcd" "efgh" "ij" "xy"; the caret at "h" (row 1, col 3).
        assert!(e.vertical(false, 4, false));
        assert_eq!(e.pos(), 3);
        assert!(!e.vertical(false, 4, false), "the first row: history");
        assert!(e.vertical(true, 4, false));
        assert!(e.vertical(true, 4, false));
        assert_eq!(e.pos(), 10, "the end of the short row");
        assert!(e.vertical(true, 4, false));
        assert_eq!(e.pos(), 13);
        assert!(!e.vertical(true, 4, false), "the last row: history");
        // The goal column is kept across the short row.
        e.go(3, false);
        e.vertical(true, 4, false);
        e.vertical(true, 4, false);
        e.vertical(false, 4, false);
        assert_eq!(e.pos(), 7);
    }

    #[test]
    fn text_changed_elsewhere_puts_the_caret_at_its_end() {
        let (_, mut c) = at("hello", 1);
        let mut t = String::from("something else");
        let e = Ed::new(&mut t, &mut c);
        assert_eq!((e.pos(), e.selection()), (14, None));
    }
}
