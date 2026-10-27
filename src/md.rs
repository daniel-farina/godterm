use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthChar;

use crate::theme::{DIM, FAINT, FG, SEL_BG, SLATE};

pub type Line = Vec<(String, Style)>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Para(String),
    Heading(usize, String),
    Item {
        depth: usize,
        marker: String,
        text: String,
    },
    Code(Vec<String>),
    Table {
        head: Vec<String>,
        align: Vec<Align>,
        rows: Vec<Vec<String>>,
    },
    Quote(String),
    Rule,
}

fn fence(t: &str) -> Option<&'static str> {
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn heading(t: &str) -> Option<(usize, String)> {
    let n = t.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&n) {
        return None;
    }
    let rest = &t[n..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let h = rest.trim().trim_end_matches('#').trim_end();
    Some((n, h.to_string()))
}

fn is_rule(t: &str) -> bool {
    let s: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && matches!(s[0], '-' | '*' | '_') && s.iter().all(|c| *c == s[0])
}

fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") {
        &t[..t.len() - 1]
    } else {
        t
    };
    let mut out = vec![];
    let mut cur = String::new();
    let mut code = false;
    let mut esc = false;
    for c in t.chars() {
        if esc {
            if c != '|' {
                cur.push('\\');
            }
            cur.push(c);
            esc = false;
            continue;
        }
        match c {
            '\\' => esc = true,
            '`' => {
                code = !code;
                cur.push(c);
            }
            '|' if !code => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    if esc {
        cur.push('\\');
    }
    out.push(cur.trim().to_string());
    out
}

fn table_sep(line: &str) -> Option<Vec<Align>> {
    let t = line.trim();
    if !t.contains('-') {
        return None;
    }
    let mut v = vec![];
    for c in cells(t) {
        let c = c.trim();
        let inner = c.trim_start_matches(':').trim_end_matches(':');
        if inner.is_empty() || !inner.chars().all(|x| x == '-') {
            return None;
        }
        v.push(match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            (true, false) => Align::Left,
            _ => Align::None,
        });
    }
    Some(v)
}

fn item(l: &str) -> Option<(usize, String, String)> {
    let spaces: usize = l
        .chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum();
    let t = l.trim_start();
    let depth = (spaces / 2).min(6);
    for b in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(b) {
            if !rest.trim().is_empty() {
                return Some((depth, b[..1].to_string(), rest.trim().to_string()));
            }
        }
    }
    let n = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&n) {
        let rest = &t[n..];
        for d in [". ", ") "] {
            if let Some(r) = rest.strip_prefix(d) {
                if !r.trim().is_empty() {
                    return Some((depth, format!("{}.", &t[..n]), r.trim().to_string()));
                }
            }
        }
    }
    None
}

pub fn parse(md: &str) -> Vec<Block> {
    let lines: Vec<&str> = md.lines().collect();
    let mut out: Vec<Block> = vec![];
    let mut para: Vec<&str> = vec![];
    let mut item_open = false;
    let flush = |para: &mut Vec<&str>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Para(para.join("\n")));
            para.clear();
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim();
        if t.is_empty() {
            flush(&mut para, &mut out);
            item_open = false;
            i += 1;
            continue;
        }
        if let Some(f) = fence(t) {
            flush(&mut para, &mut out);
            let mut code = vec![];
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(f) {
                code.push(lines[i].trim_end().replace('\t', "    "));
                i += 1;
            }
            i += 1;
            out.push(Block::Code(code));
            item_open = false;
            continue;
        }
        if let Some((n, h)) = heading(t) {
            flush(&mut para, &mut out);
            out.push(Block::Heading(n, h));
            item_open = false;
            i += 1;
            continue;
        }
        if t.contains('|') {
            if let Some(align) = lines.get(i + 1).and_then(|n| table_sep(n)) {
                let head = cells(t);
                if head.len() == align.len() {
                    flush(&mut para, &mut out);
                    let mut rows = vec![];
                    i += 2;
                    while i < lines.len() && !lines[i].trim().is_empty() && lines[i].contains('|') {
                        let mut r = cells(lines[i]);
                        r.resize(head.len(), String::new());
                        rows.push(r);
                        i += 1;
                    }
                    out.push(Block::Table { head, align, rows });
                    item_open = false;
                    continue;
                }
            }
        }
        if is_rule(t) {
            flush(&mut para, &mut out);
            out.push(Block::Rule);
            item_open = false;
            i += 1;
            continue;
        }
        if t.starts_with('>') {
            flush(&mut para, &mut out);
            let mut q = vec![];
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let s = lines[i].trim_start()[1..].trim();
                q.push(s.to_string());
                i += 1;
            }
            out.push(Block::Quote(q.join("\n").trim().to_string()));
            item_open = false;
            continue;
        }
        if let Some((depth, marker, text)) = item(l) {
            flush(&mut para, &mut out);
            out.push(Block::Item {
                depth,
                marker,
                text,
            });
            item_open = true;
            i += 1;
            continue;
        }

        if item_open && l.starts_with([' ', '\t']) {
            if let Some(Block::Item { text, .. }) = out.last_mut() {
                text.push('\n');
                text.push_str(t);
                i += 1;
                continue;
            }
        }
        item_open = false;
        para.push(t);
        i += 1;
    }
    flush(&mut para, &mut out);
    out
}

pub fn to_md(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut prev_item = false;
    for b in blocks {
        let is_item = matches!(b, Block::Item { .. });
        if !out.is_empty() {
            out.push_str(if is_item && prev_item { "\n" } else { "\n\n" });
        }
        prev_item = is_item;
        match b {
            Block::Para(t) => out.push_str(t),
            Block::Heading(n, t) => out.push_str(&format!("{} {t}", "#".repeat(*n))),
            Block::Item {
                depth,
                marker,
                text,
            } => {
                let pad = "  ".repeat(*depth);
                let cont = format!("\n{pad}{}", " ".repeat(marker.chars().count() + 1));
                out.push_str(&format!("{pad}{marker} {}", text.replace('\n', &cont)));
            }
            Block::Code(l) => out.push_str(&format!("```\n{}\n```", l.join("\n"))),
            Block::Table { head, align, rows } => {
                let line = |c: &[String]| {
                    format!(
                        "| {} |",
                        c.iter()
                            .map(|x| x.replace('|', "\\|"))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    )
                };
                out.push_str(&line(head));
                out.push('\n');
                let sep: Vec<String> = align
                    .iter()
                    .map(|a| {
                        match a {
                            Align::None => "---",
                            Align::Left => ":---",
                            Align::Center => ":---:",
                            Align::Right => "---:",
                        }
                        .to_string()
                    })
                    .collect();
                out.push_str(&line(&sep));
                for r in rows {
                    out.push('\n');
                    out.push_str(&line(r));
                }
            }
            Block::Quote(t) => out.push_str(
                &t.lines()
                    .map(|l| format!("> {l}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Block::Rule => out.push_str("---"),
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Code,
    Link,
    Url,
}

struct Piece {
    text: String,
    kind: Kind,
    bold: bool,
    italic: bool,
    strike: bool,
}

fn closes_later(chars: &[char], from: usize, delim: &[char]) -> bool {
    let n = delim.len();
    let mut j = from;
    while j + n <= chars.len() {
        if chars[j..j + n] == *delim && j > 0 && !chars[j - 1].is_whitespace() {
            let next_same = chars.get(j + n) == Some(&delim[0]);
            let prev_same = j > from && chars[j - 1] == delim[0];
            if n > 1 || !(next_same || prev_same) {
                return true;
            }
        }
        j += 1;
    }
    false
}

fn inline(s: &str) -> Vec<Piece> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Piece> = vec![];
    let (mut bold, mut italic, mut strike) = (false, false, false);
    let mut cur = String::new();
    macro_rules! flush {
        () => {
            if !cur.is_empty() {
                out.push(Piece {
                    text: std::mem::take(&mut cur),
                    kind: Kind::Text,
                    bold,
                    italic,
                    strike,
                });
            }
        };
    }
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev = if i > 0 { Some(chars[i - 1]) } else { None };
        match c {
            '\\' if chars.get(i + 1).is_some_and(|n| n.is_ascii_punctuation()) => {
                cur.push(chars[i + 1]);
                i += 2;
                continue;
            }
            '`' => {
                let n = chars[i..].iter().take_while(|x| **x == '`').count();
                let mut j = i + n;
                let mut found = None;
                while j < chars.len() {
                    if chars[j] == '`' {
                        let m = chars[j..].iter().take_while(|x| **x == '`').count();
                        if m == n {
                            found = Some(j);
                            break;
                        }
                        j += m;
                    } else {
                        j += 1;
                    }
                }
                if let Some(j) = found {
                    flush!();
                    let code: String = chars[i + n..j].iter().collect();
                    let code = if code.len() > 1 && code.starts_with(' ') && code.ends_with(' ') {
                        code[1..code.len() - 1].to_string()
                    } else {
                        code
                    };
                    out.push(Piece {
                        text: code,
                        kind: Kind::Code,
                        bold,
                        italic,
                        strike,
                    });
                    i = j + n;
                    continue;
                }
                cur.extend(&chars[i..i + n]);
                i += n;
                continue;
            }
            '*' | '_' | '~' => {
                let n = chars[i..].iter().take_while(|x| **x == c).count();
                let next = chars.get(i + n).copied();
                let word = |x: Option<char>| x.is_some_and(|x| x.is_alphanumeric());
                let mut can_open = next.is_some_and(|x| !x.is_whitespace());
                let mut can_close = prev.is_some_and(|x| !x.is_whitespace());
                if c == '_' {
                    can_open &= !word(prev);
                    can_close &= !word(next);
                }
                if c == '~' {
                    if n == 2 {
                        if strike && can_close {
                            flush!();
                            strike = false;
                        } else if can_open && closes_later(&chars, i + 2, &['~', '~']) {
                            flush!();
                            strike = true;
                        } else {
                            cur.push_str("~~");
                        }
                    } else {
                        cur.extend(&chars[i..i + n]);
                    }
                    i += n;
                    continue;
                }
                let mut left = n;
                if left >= 2 && n <= 3 {
                    if bold && can_close {
                        flush!();
                        bold = false;
                        left -= 2;
                    } else if can_open && closes_later(&chars, i + n, &[c, c]) {
                        flush!();
                        bold = true;
                        left -= 2;
                    }
                }
                if left == 1 {
                    if italic && can_close {
                        flush!();
                        italic = false;
                        left = 0;
                    } else if can_open && closes_later(&chars, i + n, &[c]) {
                        flush!();
                        italic = true;
                        left = 0;
                    }
                }
                for _ in 0..left {
                    cur.push(c);
                }
                i += n;
                continue;
            }
            '[' | '!' => {
                let at = if c == '!' { i + 1 } else { i };
                if chars.get(at) == Some(&'[') {
                    if let Some(close) = chars[at..].iter().position(|x| *x == ']') {
                        let close = at + close;
                        if chars.get(close + 1) == Some(&'(') {
                            if let Some(end) = chars[close + 1..].iter().position(|x| *x == ')') {
                                let end = close + 1 + end;
                                let text: String = chars[at + 1..close].iter().collect();
                                let url: String = chars[close + 2..end].iter().collect();
                                let url = url.split_whitespace().next().unwrap_or("").to_string();
                                flush!();
                                let shown = if text.trim().is_empty() {
                                    url.clone()
                                } else {
                                    text.clone()
                                };
                                out.push(Piece {
                                    text: shown,
                                    kind: Kind::Link,
                                    bold,
                                    italic,
                                    strike,
                                });
                                let same = url.is_empty()
                                    || url == text
                                    || url.trim_start_matches("mailto:") == text
                                    || text.trim().is_empty();
                                if !same {
                                    out.push(Piece {
                                        text: format!(" ({url})"),
                                        kind: Kind::Url,
                                        bold: false,
                                        italic: false,
                                        strike: false,
                                    });
                                }
                                i = end + 1;
                                continue;
                            }
                        }
                    }
                }
                cur.push(c);
            }
            '<' => {
                if let Some(end) = chars[i..].iter().position(|x| *x == '>') {
                    let inner: String = chars[i + 1..i + end].iter().collect();
                    if inner.starts_with("http://") || inner.starts_with("https://") {
                        flush!();
                        out.push(Piece {
                            text: inner,
                            kind: Kind::Link,
                            bold,
                            italic,
                            strike,
                        });
                        i += end + 1;
                        continue;
                    }
                }
                cur.push(c);
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    flush!();
    out
}

pub fn styled(s: &str, base: Style) -> Line {
    inline(s)
        .into_iter()
        .map(|p| {
            let mut st = match p.kind {
                Kind::Text => base,
                Kind::Code => base.fg(FG).bg(SEL_BG),
                Kind::Link => base.fg(SLATE),
                Kind::Url => Style::default().fg(DIM),
            };
            if p.bold {
                st = st.add_modifier(Modifier::BOLD);
            }
            if p.italic {
                st = st.add_modifier(Modifier::ITALIC);
            }
            if p.strike {
                st = st.add_modifier(Modifier::CROSSED_OUT);
            }
            (p.text, st)
        })
        .collect()
}

pub fn plain(s: &str) -> String {
    let t: String = inline(s)
        .into_iter()
        .filter(|p| p.kind != Kind::Url)
        .map(|p| p.text)
        .collect();
    t.replace(['*', '`'], "")
        .replace('|', ",")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn units(b: &Block) -> Vec<String> {
    match b {
        Block::Para(t) | Block::Quote(t) => crate::assistant::sentence_spans(t)
            .into_iter()
            .map(|(a, e)| plain(&t[a..e]))
            .filter(|s| !s.is_empty())
            .collect(),
        Block::Item { text, .. } => {
            let mut s = plain(text);
            if s.is_empty() {
                return vec![];
            }
            if !s.ends_with(['.', '!', '?']) {
                s = s.trim_end_matches([':', ';', ',']).to_string();
                s.push('.');
            }
            vec![s]
        }
        Block::Table { rows, .. } => vec![format!(
            "A table of {} row{} is on screen.",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        )],
        Block::Heading(..) | Block::Code(_) | Block::Rule => vec![],
    }
}

pub fn speech(md: &str) -> String {
    parse(md)
        .iter()
        .flat_map(units)
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct Reply {
    pub narration: Vec<String>,
    pub spoken: Vec<Block>,
    pub rest: Vec<Block>,
    pub say: String,
    pub rest_say: Option<String>,

    pub shown: String,
}

pub fn reply(text: &str, max: usize) -> Reply {
    use crate::assistant::{is_narration, sentence_spans};
    let blocks = parse(text);
    let has_content = blocks.iter().any(|b| match b {
        Block::Para(t) => sentence_spans(t)
            .iter()
            .any(|&(a, e)| !is_narration(&t[a..e])),
        Block::Rule => false,
        _ => true,
    });
    let mut narration = vec![];
    let blocks: Vec<Block> = if !has_content {
        blocks
    } else {
        blocks
            .into_iter()
            .filter_map(|b| match b {
                Block::Para(t) => {
                    let spans = sentence_spans(&t);
                    let mut kept = String::new();
                    for (k, &(a, e)) in spans.iter().enumerate() {
                        if is_narration(&t[a..e]) {
                            narration.push(t[a..e].to_string());
                        } else {
                            kept.push_str(&t[a..e]);
                            if let Some(&(na, _)) = spans.get(k + 1) {
                                kept.push_str(&t[e..na]);
                            }
                        }
                    }
                    let kept = kept.trim();
                    (!kept.is_empty()).then(|| Block::Para(kept.to_string()))
                }
                b => Some(b),
            })
            .collect()
    };
    let shown = if narration.is_empty() {
        text.trim().to_string()
    } else {
        to_md(&blocks)
    };
    let mut spoken = vec![];
    let mut rest = vec![];
    let mut said: Vec<String> = vec![];
    let mut later: Vec<String> = vec![];
    for b in blocks {
        let u = units(&b);
        if max != 0 && said.len() >= max {
            later.extend(u);
            rest.push(b);
            continue;
        }
        if max == 0 || said.len() + u.len() <= max {
            said.extend(u);
            spoken.push(b);
            continue;
        }

        let k = max - said.len();
        let (t, quote) = match &b {
            Block::Para(t) => (t.clone(), false),
            Block::Quote(t) => (t.clone(), true),
            _ => {
                said.extend(u);
                spoken.push(b);
                continue;
            }
        };
        let spans: Vec<(usize, usize)> = sentence_spans(&t)
            .into_iter()
            .filter(|&(a, e)| !plain(&t[a..e]).is_empty())
            .collect();
        let cut_end = spans[k - 1].1;
        let cut_start = spans[k].0;
        let mk = |s: &str| {
            if quote {
                Block::Quote(s.trim().to_string())
            } else {
                Block::Para(s.trim().to_string())
            }
        };
        spoken.push(mk(&t[..cut_end]));
        rest.push(mk(&t[cut_start..]));
        said.extend(u[..k].iter().cloned());
        later.extend(u[k..].iter().cloned());
    }

    if later.is_empty() {
        spoken.append(&mut rest);
    }
    Reply {
        narration,
        spoken,
        rest,
        say: said.join(" "),
        rest_say: (!later.is_empty()).then(|| later.join(" ")),
        shown,
    }
}

fn cw(c: char) -> usize {
    c.width().unwrap_or(0)
}

pub fn width(l: &Line) -> usize {
    l.iter()
        .map(|(s, _)| s.chars().map(cw).sum::<usize>())
        .sum()
}

fn flat(parts: &[(String, Style)]) -> Vec<(char, Style)> {
    let mut v = vec![];
    for (s, st) in parts {
        for c in s.chars() {
            let c = if c == '\t' { ' ' } else { c };
            if cw(c) > 0 {
                v.push((c, *st));
            }
        }
    }
    v
}

fn join(chars: &[(char, Style)]) -> Line {
    let mut out: Line = vec![];
    for (c, st) in chars {
        match out.last_mut() {
            Some((s, s_st)) if s_st == st => s.push(*c),
            _ => out.push((c.to_string(), *st)),
        }
    }
    out
}

fn wrap(parts: &[(String, Style)], w: usize, first: &Line, rest: &Line) -> Vec<Line> {
    let (first, rest) = if width(first).max(width(rest)) + 2 > w {
        (vec![], vec![])
    } else {
        (first.clone(), rest.clone())
    };
    let cells = flat(parts);
    let mut lines: Vec<Line> = vec![];
    let mut cur: Vec<(char, Style)> = flat(&first);
    let mut pre = width(&first);
    let mut col = pre;
    let mut i = 0;
    let rest_w = width(&rest);
    let newline = |lines: &mut Vec<Line>, cur: &mut Vec<(char, Style)>| {
        lines.push(join(cur));
        *cur = flat(&rest);
    };
    while i < cells.len() {
        let s0 = i;
        while i < cells.len() && cells[i].0 == ' ' {
            i += 1;
        }
        let spaces = &cells[s0..i];
        let w0 = i;
        while i < cells.len() && cells[i].0 != ' ' {
            i += 1;
        }
        let word = &cells[w0..i];
        if word.is_empty() {
            break;
        }
        let ww: usize = word.iter().map(|(c, _)| cw(*c)).sum();
        let sw: usize = spaces.iter().map(|(c, _)| cw(*c)).sum();
        if col > pre {
            if col + sw + ww > w {
                newline(&mut lines, &mut cur);
                pre = rest_w;
                col = pre;
            } else {
                cur.extend_from_slice(spaces);
                col += sw;
            }
        }
        if col + ww > w {
            for &(c, st) in word {
                if col + cw(c) > w && col > pre {
                    newline(&mut lines, &mut cur);
                    pre = rest_w;
                    col = pre;
                }
                cur.push((c, st));
                col += cw(c);
            }
        } else {
            cur.extend_from_slice(word);
            col += ww;
        }
    }
    lines.push(join(&cur));
    lines
}

fn truncate(l: &Line, w: usize) -> Line {
    if width(l) <= w {
        return l.clone();
    }
    let cells = flat(l);
    let mut out = vec![];
    let mut col = 0;
    for (c, st) in cells {
        if col + cw(c) + 1 > w {
            out.push(('…', st));
            break;
        }
        out.push((c, st));
        col += cw(c);
    }
    if w == 0 {
        out.clear();
    }
    join(&out)
}

fn fit(l: &Line, w: usize, align: Align, base: Style) -> Line {
    let mut v = truncate(l, w);
    let pad = w.saturating_sub(width(&v));
    let (left, right) = match align {
        Align::Right => (pad, 0),
        Align::Center => (pad / 2, pad - pad / 2),
        _ => (0, pad),
    };
    if left > 0 {
        v.insert(0, (" ".repeat(left), base));
    }
    if right > 0 {
        v.push((" ".repeat(right), base));
    }
    v
}

fn numeric(s: &str) -> bool {
    s.trim()
        .trim_start_matches(['+', '-', '−', '$', '€', '£', '~', '≈'])
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
}

fn shrink(nat: &[usize], avail: usize) -> Option<Vec<usize>> {
    let sum: usize = nat.iter().sum();
    if sum <= avail {
        return Some(nat.to_vec());
    }
    let min: Vec<usize> = nat.iter().map(|n| (*n).min(6)).collect();
    if min.iter().sum::<usize>() > avail || avail * 10 < sum * 6 {
        return None;
    }
    let mut w: Vec<usize> = nat
        .iter()
        .zip(&min)
        .map(|(n, m)| (n * avail / sum).max(*m))
        .collect();
    while w.iter().sum::<usize>() > avail {
        let (k, _) = w
            .iter()
            .enumerate()
            .max_by_key(|(k, x)| **x - min[*k])
            .expect("columns");
        w[k] -= 1;
    }
    while w.iter().sum::<usize>() < avail {
        let (k, d) = w
            .iter()
            .enumerate()
            .map(|(k, x)| (k, nat[k] - x))
            .max_by_key(|(_, d)| *d)
            .expect("columns");
        if d == 0 {
            break;
        }
        w[k] += 1;
    }
    Some(w)
}

fn table(
    head: &[String],
    align: &[Align],
    rows: &[Vec<String>],
    w: usize,
    base: Style,
) -> Vec<Line> {
    let n = head.len();
    let bold = base.add_modifier(Modifier::BOLD);
    let hcells: Vec<Line> = head.iter().map(|h| styled(h, bold)).collect();
    let bcells: Vec<Vec<Line>> = rows
        .iter()
        .map(|r| r.iter().map(|c| styled(c, base)).collect())
        .collect();
    let nat: Vec<usize> = (0..n)
        .map(|c| {
            bcells
                .iter()
                .map(|r| width(&r[c]))
                .chain([width(&hcells[c])])
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();

    let aligns: Vec<Align> = (0..n)
        .map(|c| {
            if align[c] != Align::None || c == 0 {
                return align[c];
            }
            let vals: Vec<&str> = rows
                .iter()
                .map(|r| r[c].trim())
                .filter(|v| !v.is_empty() && !["-", "—", "–", "n/a", "N/A"].contains(v))
                .collect();
            let num = vals.iter().filter(|v| numeric(v)).count();
            if !vals.is_empty() && num * 10 >= vals.len() * 6 {
                Align::Right
            } else {
                Align::Left
            }
        })
        .collect();
    const GAP: usize = 2;
    let gaps = GAP * n.saturating_sub(1);
    let Some(cols) = w.checked_sub(gaps).and_then(|avail| shrink(&nat, avail)) else {
        return cards(&hcells, &bcells, w, base);
    };
    let line = |cells: &[Line]| -> Line {
        let mut l: Line = vec![];
        for (c, cell) in cells.iter().enumerate() {
            if c > 0 {
                l.push((" ".repeat(GAP), base));
            }
            l.extend(fit(cell, cols[c], aligns[c], base));
        }

        while let Some((s, _)) = l.last_mut() {
            let t = s.trim_end().to_string();
            if t.is_empty() {
                l.pop();
            } else {
                *s = t;
                break;
            }
        }
        l
    };
    let total: usize = cols.iter().sum::<usize>() + gaps;
    let mut out = vec![line(&hcells)];
    out.push(vec![("─".repeat(total), Style::default().fg(FAINT))]);
    for r in &bcells {
        out.push(line(r));
    }
    out
}

fn cards(head: &[Line], rows: &[Vec<Line>], w: usize, base: Style) -> Vec<Line> {
    let mut out = vec![];
    for (k, r) in rows.iter().enumerate() {
        if k > 0 {
            out.push(vec![]);
        }
        let title: Line = r[0]
            .iter()
            .map(|(s, st)| (s.clone(), st.add_modifier(Modifier::BOLD)))
            .collect();
        out.extend(wrap(&title, w, &vec![], &vec![]));
        for c in 1..r.len() {
            let key: String = head[c].iter().map(|(s, _)| s.as_str()).collect();
            let first: Line = vec![
                ("  ".into(), base),
                (format!("{key}: "), Style::default().fg(DIM)),
            ];
            out.extend(wrap(&r[c], w, &first, &vec![("    ".into(), base)]));
        }
    }
    out
}

pub fn render(blocks: &[Block], w: usize, base: Style) -> Vec<Line> {
    let w = w.max(4);
    let mut out: Vec<Line> = vec![];
    let mut prev_item = false;
    for (k, b) in blocks.iter().enumerate() {
        let is_item = matches!(b, Block::Item { .. });
        if k > 0 && !(is_item && prev_item) {
            out.push(vec![]);
        }
        prev_item = is_item;
        match b {
            Block::Para(t) => {
                for l in t.split('\n') {
                    out.extend(wrap(&styled(l, base), w, &vec![], &vec![]));
                }
            }
            Block::Heading(_, t) => {
                out.extend(wrap(
                    &styled(t, base.add_modifier(Modifier::BOLD)),
                    w,
                    &vec![],
                    &vec![],
                ));
            }
            Block::Item {
                depth,
                marker,
                text,
            } => {
                let pad = "  ".repeat(*depth);
                let m = if marker.ends_with('.') {
                    marker.clone()
                } else if *depth == 0 {
                    "•".into()
                } else {
                    "◦".into()
                };
                let hang = format!("{pad}{}", " ".repeat(m.chars().map(cw).sum::<usize>() + 1));
                let first: Line = vec![(pad.clone(), base), (format!("{m} "), base.fg(DIM))];
                let rest: Line = vec![(hang, base)];
                for (j, l) in text.split('\n').enumerate() {
                    let f = if j == 0 { &first } else { &rest };
                    out.extend(wrap(&styled(l, base), w, f, &rest));
                }
            }
            Block::Code(lines) => {
                let st = Style::default().fg(FG).bg(SEL_BG);
                let inner = lines
                    .iter()
                    .map(|l| l.chars().map(cw).sum::<usize>())
                    .max()
                    .unwrap_or(0)
                    .min(w.saturating_sub(2));
                for l in lines {
                    let cut = truncate(&vec![(l.clone(), st)], inner);
                    let pad = inner - width(&cut);
                    let mut v: Line = vec![(" ".into(), st)];
                    v.extend(cut);
                    v.push((" ".repeat(pad + 1), st));
                    out.push(v);
                }
            }
            Block::Table { head, align, rows } => out.extend(table(head, align, rows, w, base)),
            Block::Quote(t) => {
                let bar: Line = vec![("│ ".into(), Style::default().fg(FAINT))];
                for l in t.split('\n') {
                    out.extend(wrap(&styled(l, base), w, &bar, &bar));
                }
            }
            Block::Rule => out.push(vec![("─".repeat(w), Style::default().fg(FAINT))]),
        }
    }
    out
}

type Cache = HashMap<(u64, usize, usize, u8), Rc<Vec<Line>>>;

thread_local! {
    static CACHE: RefCell<Cache> =
        RefCell::new(HashMap::new());
}

fn cached(
    text: &str,
    max: usize,
    w: usize,
    kind: u8,
    f: impl FnOnce() -> Vec<Line>,
) -> Rc<Vec<Line>> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    let key = (h.finish(), max, w, kind);
    if let Some(v) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return v;
    }
    let v = Rc::new(f());
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 1024 {
            c.clear();
        }
        c.insert(key, v.clone());
    });
    v
}

pub fn view(text: &str, w: usize) -> Rc<Vec<Line>> {
    cached(text, 0, w, 0, || {
        render(&parse(text), w, Style::default().fg(FG))
    })
}

pub fn reply_view(text: &str, max: usize, w: usize) -> Rc<Vec<Line>> {
    cached(text, max, w, 1, || {
        let base = Style::default().fg(FG);
        let r = reply(text, max);
        let mut v = render(&r.spoken, w, base);
        if !r.rest.is_empty() {
            v.push(vec![("more ›".into(), Style::default().fg(FAINT))]);
            v.extend(render(&r.rest, w, base));
        }
        v
    })
}

#[cfg(test)]
pub fn text_of(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| l.iter().map(|(s, _)| s.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    pub const USAGE: &str = "Here is usage per account. Weekly is the binding limit on each.

| Account | 5 hour left | Weekly left | Weekly resets |
|---|---|---|---|
| 1 (you) | 16% | 8% | Sat 12:00 |
| 2 | 100% | 0% (out) | Tue 7:59 AM |
| 3 | 100% | 0% (out) | Thu 7:59 PM |
| 4 | 100% | 0% (out) | Wed 10:00 PM |
| 5 (test, Grok) | not reported | 0% (out) | Fri 10:16 AM |
| 6 (demo8, Grok) | not reported | 83% | not reported |";

    fn lines_at(text: &str, w: usize) -> Vec<String> {
        text_of(&render(&parse(text), w, Style::default()))
            .lines()
            .map(String::from)
            .collect()
    }

    pub fn col(line: &str, pat: &str) -> usize {
        let at = line
            .find(pat)
            .unwrap_or_else(|| panic!("{pat:?} in {line:?}"));
        line[..at].width()
    }

    #[test]
    fn usage_table_is_an_aligned_grid() {
        for w in [60usize, 90] {
            let v = lines_at(USAGE, w);
            println!("--- {w} columns ---\n{}", v.join("\n"));
            let h = v.iter().position(|l| l.starts_with("Account")).unwrap();
            let head = &v[h];
            assert!(v[h + 1].chars().all(|c| c == '─'), "{}", v[h + 1]);
            assert_eq!(v[h + 1].width(), head.width());
            let rows = &v[h + 2..h + 8];

            let resets = col(head, "Weekly resets");
            for (r, val) in rows.iter().zip([
                "Sat 12:00",
                "Tue 7:59 AM",
                "Thu 7:59 PM",
                "Wed 10:00 PM",
                "Fri 10:16 AM",
                "not reported",
            ]) {
                let at = r.rfind(val).unwrap();
                assert_eq!(r[..at].width(), resets, "{r}");
                assert!(r.width() <= w);
            }

            let wk_end = col(head, "Weekly left") + "Weekly left".len();
            for (r, val) in rows
                .iter()
                .zip(["8%", "0% (out)", "0% (out)", "0% (out)", "0% (out)", "83%"])
            {
                assert_eq!(col(r, &format!("{val}  ")) + val.len(), wk_end, "{r}");
            }
            let h5_end = col(head, "5 hour left") + "5 hour left".len();
            assert_eq!(col(&rows[0], "16%") + 3, h5_end, "{}", rows[0]);
            assert_eq!(col(&rows[1], "100%") + 4, h5_end, "{}", rows[1]);
            assert!(rows[5].starts_with("6 (demo8, Grok)"));
        }

        let l = render(&parse(USAGE), 60, Style::default());
        let h = l
            .iter()
            .find(|l| l.iter().any(|(s, _)| s.contains("Account")))
            .unwrap();
        assert!(h
            .iter()
            .filter(|(s, _)| !s.trim().is_empty())
            .all(|(_, st)| st.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn narrow_tables_shrink_then_become_cards() {
        let v = lines_at(USAGE, 50);
        println!("{}", v.join("\n"));
        assert!(v.iter().all(|l| l.width() <= 50), "{v:#?}");
        assert!(v.iter().any(|l| l.contains('…')), "{v:#?}");
        assert!(v.iter().any(|l| l.starts_with('─')));

        let v = lines_at(USAGE, 26);
        println!("{}", v.join("\n"));
        assert!(v.iter().all(|l| l.width() <= 26), "{v:#?}");
        assert!(!v.iter().any(|l| l.starts_with('─')));
        let at = v.iter().position(|l| l == "1 (you)").unwrap();
        assert_eq!(v[at + 1], "  5 hour left: 16%");
        assert_eq!(v[at + 2], "  Weekly left: 8%");
        assert_eq!(v[at + 3], "  Weekly resets: Sat 12:00");
        assert!(v.contains(&"6 (demo8, Grok)".to_string()));
    }

    #[test]
    fn wide_characters_keep_the_grid() {
        let t = "| Name | Note |\n|---|---|\n| 東京 | 晴れ |\n| ab | 🙂 ok |\n";
        let v = lines_at(t, 40);
        let c = col(&v[0], "Note");
        assert_eq!(col(&v[2], "晴れ"), c, "{v:#?}");
        assert_eq!(col(&v[3], "🙂"), c, "{v:#?}");

        for w in 4..30 {
            assert!(lines_at(t, w).iter().all(|l| l.width() <= w), "{w}");
            assert!(lines_at(USAGE, w).iter().all(|l| l.width() <= w), "{w}");
        }
    }

    #[test]
    fn lists_code_and_bold() {
        let t = "Steps:\n- **Build** it first, then wait for it to finish completely\n- run `cargo test`\n  with care\n  - nested\n1. one\n2. two\n\n```\nfn main() { println!(\"a very long line of code here\"); }\n```\n\n> quoted *words*\n\n## Heading\n\nSee [the docs](https://example.com/docs) and [https://x.io](https://x.io).";
        let l = render(&parse(t), 40, Style::default());
        let v: Vec<String> = text_of(&l).lines().map(String::from).collect();
        println!("{}", v.join("\n"));
        assert_eq!(v[0], "Steps:");
        assert_eq!(v[1], "");
        assert_eq!(v[2], "• Build it first, then wait for it to");
        assert_eq!(v[3], "  finish completely", "hanging indent");
        assert_eq!(v[4], "• run cargo test");
        assert_eq!(v[5], "  with care");
        assert_eq!(v[6], "  ◦ nested");
        assert_eq!(v[7], "1. one");
        assert_eq!(v[8], "2. two");

        let bold = l[2].iter().find(|(s, _)| s == "Build").unwrap();
        assert!(bold.1.add_modifier.contains(Modifier::BOLD));
        let code = l[4].iter().find(|(s, _)| s == "cargo test").unwrap();
        assert_eq!(code.1.bg, Some(SEL_BG));

        let ci = v.iter().position(|s| s.contains("fn main")).unwrap();
        assert!(v[ci].ends_with("… ") && v[ci].width() == 40, "{:?}", v[ci]);
        assert!(l[ci].iter().all(|(_, st)| st.bg == Some(SEL_BG)));
        assert!(v.contains(&"│ quoted words".to_string()));
        let hi = v.iter().position(|s| s == "Heading").unwrap();
        assert!(l[hi][0].1.add_modifier.contains(Modifier::BOLD));
        let last = v[hi + 2..].join(" ");
        assert!(
            last.contains("See the docs (https://example.com/docs) and https://x.io."),
            "{last}"
        );
        assert!(!v.iter().any(|s| s.contains("**") || s.contains('`')));
    }

    #[test]
    fn line_breaks_are_kept() {
        let v = lines_at("Line one\nLine two\n\nPara two", 40);
        assert_eq!(v, vec!["Line one", "Line two", "", "Para two"]);

        assert_eq!(
            lines_at("a snake_case_name and 5 * 3", 40),
            vec!["a snake_case_name and 5 * 3"]
        );
    }

    #[test]
    fn speech_has_no_markup() {
        let r = reply(USAGE, 2);
        assert_eq!(
            r.say,
            "Here is usage per account. Weekly is the binding limit on each."
        );
        assert_eq!(
            r.rest_say.as_deref(),
            Some("A table of 6 rows is on screen.")
        );
        let all = speech(&format!(
            "{USAGE}\n\n- **Bold** item\n- `code` item:\n\n```\nlet x = a | b;\n```"
        ));
        assert!(
            !all.contains('|') && !all.contains('*') && !all.contains('`'),
            "{all}"
        );
        assert!(all.ends_with("Bold item. code item."), "{all}");

        let v = text_of(&reply_view(USAGE, 2, 70));
        let v: Vec<&str> = v.lines().collect();
        assert_eq!(
            v[0],
            "Here is usage per account. Weekly is the binding limit on each."
        );
        assert_eq!(v[1], "more ›");
        assert!(v[2].starts_with("Account"));

        let r = reply("Let me check. One.\nTwo. Three.\nFour.", 2);
        assert_eq!(r.narration, vec!["Let me check."]);
        assert_eq!(r.say, "One. Two.");
        assert_eq!(r.spoken, vec![Block::Para("One.\nTwo.".into())]);
        assert_eq!(r.rest, vec![Block::Para("Three.\nFour.".into())]);
        assert_eq!(r.shown, "One.\nTwo. Three.\nFour.");
    }
}
