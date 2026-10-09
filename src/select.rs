//! Text selection inside one pane (or the assistant panel), as in tmux:
//! drag selects, a double click a word, a triple click a line. Positions
//! are in buffer lines (scrollback included), so a selection stays on its
//! text while the view scrolls.

use std::time::Instant;

/// A cell of the buffer: `line` counts from the oldest scrollback line
/// (for a pane) or from the first drawn line (the assistant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Char,
    Word,
    Line,
}

/// What the selection is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A tab of a pane, by slot and tab uid (another tab shown: not drawn).
    Pane {
        slot: usize,
        uid: u64,
    },
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub target: Target,
    pub anchor: Pos,
    pub head: Pos,
    pub unit: Unit,
    /// The button is still down.
    pub dragging: bool,
    /// A char selection shows once the mouse moved off its first cell.
    pub moved: bool,
    /// Held past the top (-1) or bottom (1) edge: the tick keeps scrolling.
    pub edge: i8,
}

impl Selection {
    pub fn new(target: Target, at: Pos, unit: Unit) -> Selection {
        Selection {
            target,
            anchor: at,
            head: at,
            unit,
            dragging: true,
            moved: unit != Unit::Char,
            edge: 0,
        }
    }

    /// Anything to show or copy.
    pub fn visible(&self) -> bool {
        self.moved
    }
}

/// The last press, to count double and triple clicks.
#[derive(Debug, Clone, Copy)]
pub struct Press {
    pub at: Instant,
    pub target: Target,
    pub pos: Pos,
    pub count: u8,
}

/// Lines of text a selection reads: a pane's buffer or the panel's lines.
pub trait Lines {
    /// The cells of a line (one string per column; "" for the right half
    /// of a wide character) and whether it wraps onto the next.
    fn line(&mut self, n: usize) -> Option<(Vec<String>, bool)>;
}

/// A vt100 screen, scrollback included. Reading moves the view and puts it
/// back.
pub struct ScreenLines<'a>(pub &'a mut vt100::Screen);

/// Lines in the scrollback of a screen (the view is left where it was).
pub fn scrollback_len(s: &mut vt100::Screen) -> usize {
    let cur = s.scrollback();
    s.set_scrollback(usize::MAX);
    let n = s.scrollback();
    s.set_scrollback(cur);
    n
}

/// The buffer line shown on visible row `row`.
pub fn line_of_row(s: &mut vt100::Screen, row: u16) -> usize {
    scrollback_len(s) - s.scrollback() + row as usize
}

impl Lines for ScreenLines<'_> {
    fn line(&mut self, n: usize) -> Option<(Vec<String>, bool)> {
        let s = &mut *self.0;
        let (rows, cols) = s.size();
        let len = scrollback_len(s);
        let (off, row) = if n >= len { (0, n - len) } else { (len - n, 0) };
        if row >= rows as usize {
            return None;
        }
        let cur = s.scrollback();
        s.set_scrollback(off);
        let row = row as u16;
        let cells = (0..cols)
            .map(|c| match s.cell(row, c) {
                Some(cell) if cell.is_wide_continuation() => String::new(),
                Some(cell) if cell.has_contents() => cell.contents().to_string(),
                _ => " ".to_string(),
            })
            .collect();
        let wrapped = s.row_wrapped(row);
        s.set_scrollback(cur);
        Some((cells, wrapped))
    }
}

/// Plain text lines, one cell per character (the assistant panel).
pub struct TextLines<'a>(pub &'a [String]);

impl Lines for TextLines<'_> {
    fn line(&mut self, n: usize) -> Option<(Vec<String>, bool)> {
        let l = self.0.get(n)?;
        let mut cells = vec![];
        for ch in l.chars() {
            cells.push(ch.to_string());
            if unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) == 2 {
                cells.push(String::new());
            }
        }
        Some((cells, false))
    }
}

/// Characters that end a word: spaces, brackets, quotes and box drawing.
fn is_word(s: &str) -> bool {
    let Some(c) = s.chars().next() else {
        return true; // the right half of a wide character goes with it
    };
    !(c.is_whitespace() || "()[]{}<>\"'`,;|".contains(c) || ('\u{2500}'..='\u{259f}').contains(&c))
}

/// The word around `col` (just that cell when it is not in a word).
fn word_at(cells: &[String], col: u16) -> (u16, u16) {
    if cells.is_empty() {
        return (col, col);
    }
    let mut col = (col as usize).min(cells.len() - 1);
    // The right half of a wide character: the character itself.
    if cells[col].is_empty() && col > 0 {
        col -= 1;
    }
    if !is_word(&cells[col]) {
        return (col as u16, col as u16);
    }
    let mut a = col;
    while a > 0 && is_word(&cells[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b + 1 < cells.len() && is_word(&cells[b + 1]) {
        b += 1;
    }
    (a as u16, b as u16)
}

/// The selected range, in order and grown to words or lines: inclusive.
pub fn span(src: &mut dyn Lines, sel: &Selection) -> (Pos, Pos) {
    let (mut a, mut b) = if sel.anchor <= sel.head {
        (sel.anchor, sel.head)
    } else {
        (sel.head, sel.anchor)
    };
    match sel.unit {
        Unit::Char => {}
        Unit::Word => {
            if let Some((cells, _)) = src.line(a.line) {
                a.col = word_at(&cells, a.col).0;
            }
            if let Some((cells, _)) = src.line(b.line) {
                b.col = word_at(&cells, b.col).1;
            }
        }
        Unit::Line => {
            // Whole lines, a long line that wrapped counting as one.
            while a.line > 0 && src.line(a.line - 1).is_some_and(|(_, w)| w) {
                a.line -= 1;
            }
            while src.line(b.line).is_some_and(|(_, w)| w) && src.line(b.line + 1).is_some() {
                b.line += 1;
            }
            a.col = 0;
            b.col = u16::MAX;
        }
    }
    (a, b)
}

/// Whether `p` is in the span `(a, b)`.
pub fn contains(span: (Pos, Pos), p: Pos) -> bool {
    span.0 <= p && p <= span.1
}

/// The selected text: trailing spaces trimmed per line (and blank lines
/// at the end), a wrapped line joined back into one.
pub fn text(src: &mut dyn Lines, sel: &Selection) -> String {
    if !sel.visible() {
        return String::new();
    }
    let (a, b) = span(src, sel);
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    for n in a.line..=b.line {
        let Some((cells, wrapped)) = src.line(n) else {
            break;
        };
        let from = if n == a.line { a.col as usize } else { 0 };
        let to = if n == b.line {
            (b.col as usize).min(cells.len().saturating_sub(1))
        } else {
            cells.len().saturating_sub(1)
        };
        for c in cells.iter().take(to + 1).skip(from) {
            cur.push_str(c);
        }
        if wrapped && n != b.line {
            continue;
        }
        out.push(cur.trim_end().to_string());
        cur.clear();
    }
    if !cur.is_empty() {
        out.push(cur.trim_end().to_string());
    }
    // Dragged on into the empty rows below the text: not those.
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// Standard base64, for OSC 52.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16
            | (*ch.get(1).unwrap_or(&0) as u32) << 8
            | *ch.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= ch.len() {
                s.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// The OSC 52 sequence that puts `text` on the terminal's clipboard.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// "Copied 12 characters".
pub fn copied_msg(text: &str) -> String {
    match text.chars().count() {
        1 => "Copied 1 character".to_string(),
        n => format!("Copied {n} characters"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(rows: u16, cols: u16, text: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(rows, cols, 100);
        p.process(text.as_bytes());
        p
    }

    fn sel(anchor: (usize, u16), head: (usize, u16), unit: Unit) -> Selection {
        let mut s = Selection::new(
            Target::Pane { slot: 0, uid: 1 },
            Pos {
                line: anchor.0,
                col: anchor.1,
            },
            unit,
        );
        s.head = Pos {
            line: head.0,
            col: head.1,
        };
        s.moved = true;
        s
    }

    fn pane_text(p: &mut vt100::Parser, s: &Selection) -> String {
        text(&mut ScreenLines(p.screen_mut()), s)
    }

    #[test]
    fn multi_row_trims_and_orders() {
        let mut p = parser(5, 20, "first line   \r\nsecond\r\nthird one");
        // Forward and backward drags give the same text.
        let s = sel((0, 6), (2, 4), Unit::Char);
        assert_eq!(pane_text(&mut p, &s), "line\nsecond\nthird");
        let back = sel((2, 4), (0, 6), Unit::Char);
        assert_eq!(pane_text(&mut p, &back), "line\nsecond\nthird");
        // One row, part of it.
        assert_eq!(pane_text(&mut p, &sel((1, 1), (1, 3), Unit::Char)), "eco");
        // Not moved: nothing.
        let mut still = sel((0, 0), (0, 0), Unit::Char);
        still.moved = false;
        assert_eq!(pane_text(&mut p, &still), "");
    }

    #[test]
    fn word_and_line() {
        let mut p = parser(5, 30, "run cargo-test --all now\r\n│ path/to/file.rs │");
        assert_eq!(
            pane_text(&mut p, &sel((0, 6), (0, 6), Unit::Word)),
            "cargo-test"
        );
        // A drag in word mode grows both ends to whole words.
        assert_eq!(
            pane_text(&mut p, &sel((0, 1), (0, 13), Unit::Word)),
            "run cargo-test"
        );
        // Box drawing ends a word.
        assert_eq!(
            pane_text(&mut p, &sel((1, 5), (1, 5), Unit::Word)),
            "path/to/file.rs"
        );
        // On a space: just that cell (trimmed away).
        assert_eq!(pane_text(&mut p, &sel((0, 3), (0, 3), Unit::Word)), "");
        assert_eq!(
            pane_text(&mut p, &sel((0, 9), (0, 9), Unit::Line)),
            "run cargo-test --all now"
        );
        assert_eq!(
            pane_text(&mut p, &sel((1, 2), (0, 2), Unit::Line)),
            "run cargo-test --all now\n│ path/to/file.rs │"
        );
    }

    #[test]
    fn wrapped_rows_join() {
        // 10 columns: "abcdefghijKLMNO" wraps after j.
        let mut p = parser(4, 10, "abcdefghijKLMNO\r\nnext");
        let s = sel((0, 0), (2, 3), Unit::Char);
        assert_eq!(pane_text(&mut p, &s), "abcdefghijKLMNO\nnext");
        // A triple click on the second half takes the whole long line.
        assert_eq!(
            pane_text(&mut p, &sel((1, 2), (1, 2), Unit::Line)),
            "abcdefghijKLMNO"
        );
    }

    #[test]
    fn clamped_columns_and_wide_chars() {
        let mut p = parser(3, 12, "日本 text\r\nb");
        // Past the end of the row: up to the last column, trimmed.
        assert_eq!(
            pane_text(&mut p, &sel((0, 0), (0, u16::MAX), Unit::Char)),
            "日本 text"
        );
        assert_eq!(pane_text(&mut p, &sel((0, 2), (0, 3), Unit::Char)), "本");
        // Past the last line: stops at what exists.
        assert_eq!(pane_text(&mut p, &sel((1, 0), (99, 5), Unit::Char)), "b");
    }

    #[test]
    fn scrollback_lines_stay_put() {
        // 3 rows; 6 lines written, so 3 scrolled off.
        let mut p = parser(3, 10, "l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");
        let s = p.screen_mut();
        assert_eq!(scrollback_len(s), 3);
        assert_eq!(line_of_row(s, 0), 3);
        // Scrolled up 2: row 0 shows line 1.
        s.set_scrollback(2);
        assert_eq!(line_of_row(s, 0), 1);
        let a = sel((1, 0), (4, 1), Unit::Char);
        assert_eq!(pane_text(&mut p, &a), "l1\nl2\nl3\nl4");
        // Reading put the view back.
        assert_eq!(p.screen().scrollback(), 2);
        // Scrolled to the bottom: the same lines, the same text.
        p.screen_mut().set_scrollback(0);
        assert_eq!(pane_text(&mut p, &a), "l1\nl2\nl3\nl4");
    }

    #[test]
    fn text_lines_for_the_panel() {
        let lines = vec!["you: hello there".to_string(), "  it: hi".to_string()];
        let mut s = sel((0, 5), (1, 7), Unit::Char);
        s.target = Target::Assistant;
        assert_eq!(text(&mut TextLines(&lines), &s), "hello there\n  it: hi");
        let w = sel((1, 6), (1, 6), Unit::Word);
        assert_eq!(text(&mut TextLines(&lines), &w), "hi");
    }

    #[test]
    fn osc52_and_message() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi"), "aGk=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(copied_msg("a"), "Copied 1 character");
        assert_eq!(copied_msg("日本"), "Copied 2 characters");
    }
}
