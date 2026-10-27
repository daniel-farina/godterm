use std::time::Instant;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Pane { slot: usize, uid: u64 },
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub target: Target,
    pub anchor: Pos,
    pub head: Pos,
    pub unit: Unit,

    pub dragging: bool,

    pub moved: bool,

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

    pub fn visible(&self) -> bool {
        self.moved
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Press {
    pub at: Instant,
    pub target: Target,
    pub pos: Pos,
    pub count: u8,
}

pub trait Lines {
    fn line(&mut self, n: usize) -> Option<(Vec<String>, bool)>;
}

pub struct ScreenLines<'a>(pub &'a mut vt100::Screen);

pub fn scrollback_len(s: &mut vt100::Screen) -> usize {
    let cur = s.scrollback();
    s.set_scrollback(usize::MAX);
    let n = s.scrollback();
    s.set_scrollback(cur);
    n
}

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

fn is_word(s: &str) -> bool {
    let Some(c) = s.chars().next() else {
        return true;
    };
    !(c.is_whitespace() || "()[]{}<>\"'`,;|".contains(c) || ('\u{2500}'..='\u{259f}').contains(&c))
}

fn word_at(cells: &[String], col: u16) -> (u16, u16) {
    if cells.is_empty() {
        return (col, col);
    }
    let mut col = (col as usize).min(cells.len() - 1);

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

pub fn contains(span: (Pos, Pos), p: Pos) -> bool {
    span.0 <= p && p <= span.1
}

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

    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

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

pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

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

        let s = sel((0, 6), (2, 4), Unit::Char);
        assert_eq!(pane_text(&mut p, &s), "line\nsecond\nthird");
        let back = sel((2, 4), (0, 6), Unit::Char);
        assert_eq!(pane_text(&mut p, &back), "line\nsecond\nthird");

        assert_eq!(pane_text(&mut p, &sel((1, 1), (1, 3), Unit::Char)), "eco");

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

        assert_eq!(
            pane_text(&mut p, &sel((0, 1), (0, 13), Unit::Word)),
            "run cargo-test"
        );

        assert_eq!(
            pane_text(&mut p, &sel((1, 5), (1, 5), Unit::Word)),
            "path/to/file.rs"
        );

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
        let mut p = parser(4, 10, "abcdefghijKLMNO\r\nnext");
        let s = sel((0, 0), (2, 3), Unit::Char);
        assert_eq!(pane_text(&mut p, &s), "abcdefghijKLMNO\nnext");

        assert_eq!(
            pane_text(&mut p, &sel((1, 2), (1, 2), Unit::Line)),
            "abcdefghijKLMNO"
        );
    }

    #[test]
    fn clamped_columns_and_wide_chars() {
        let mut p = parser(3, 12, "日本 text\r\nb");

        assert_eq!(
            pane_text(&mut p, &sel((0, 0), (0, u16::MAX), Unit::Char)),
            "日本 text"
        );
        assert_eq!(pane_text(&mut p, &sel((0, 2), (0, 3), Unit::Char)), "本");

        assert_eq!(pane_text(&mut p, &sel((1, 0), (99, 5), Unit::Char)), "b");
    }

    #[test]
    fn scrollback_lines_stay_put() {
        let mut p = parser(3, 10, "l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");
        let s = p.screen_mut();
        assert_eq!(scrollback_len(s), 3);
        assert_eq!(line_of_row(s, 0), 3);

        s.set_scrollback(2);
        assert_eq!(line_of_row(s, 0), 1);
        let a = sel((1, 0), (4, 1), Unit::Char);
        assert_eq!(pane_text(&mut p, &a), "l1\nl2\nl3\nl4");

        assert_eq!(p.screen().scrollback(), 2);

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
