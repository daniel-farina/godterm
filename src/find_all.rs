use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui::Frame;

use crate::app::{App, Modal};
use crate::hits::UiAction;
use crate::theme::{self, BAR_BG, DIM, FG, SEL_BG};

pub const LINES_BACK: usize = 400;

const PER_TAB: usize = 3;
const MAX_HITS: usize = 40;

pub const MIN_QUERY: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct FindHit {
    pub uid: u64,

    pub place: String,

    pub line: String,

    pub back: usize,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FindAll {
    pub input: String,
    pub sel: usize,
    pub hits: Vec<FindHit>,
}

pub fn recent_lines(p: &mut vt100::Parser, n: usize) -> Vec<String> {
    let (rows, cols) = p.screen().size();
    let was = p.screen().scrollback();
    p.screen_mut().set_scrollback(usize::MAX);
    let total = p.screen().scrollback();
    let back = total.min(n.saturating_sub(rows as usize));
    let mut out = Vec::with_capacity(back + rows as usize);
    for k in (1..=back).rev() {
        p.screen_mut().set_scrollback(k);
        out.push(p.screen().rows(0, cols).next().unwrap_or_default());
    }
    p.screen_mut().set_scrollback(0);
    out.extend(p.screen().rows(0, cols));
    p.screen_mut().set_scrollback(was);
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out
}

pub fn matches_in(lines: &[String], q: &str, max: usize) -> Vec<(String, usize)> {
    let q = q.trim().to_lowercase();
    if q.chars().count() < MIN_QUERY {
        return vec![];
    }
    let n = lines.len();
    lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, l)| l.to_lowercase().contains(&q))
        .take(max)
        .map(|(i, l)| (l.trim().to_string(), n - 1 - i))
        .collect()
}

pub fn around(line: &str, q: &str, w: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= w {
        return line.to_string();
    }
    let low = line.to_lowercase();
    let at = low
        .find(&q.trim().to_lowercase())
        .map(|b| low[..b].chars().count())
        .unwrap_or(0);
    let start = at.saturating_sub(w / 3);
    let end = (start + w.saturating_sub(2)).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

impl App {
    pub fn open_find_all(&mut self) {
        self.modal = Modal::FindAll(FindAll::default());
    }

    pub fn find_all_hits(&self, q: &str) -> Vec<FindHit> {
        if q.trim().chars().count() < MIN_QUERY {
            return vec![];
        }
        let mut hits = vec![];
        for slot in &self.panes {
            if slot.account.is_some_and(|a| self.account_closed(a)) {
                continue;
            }
            let acct = slot
                .account
                .map(|a| self.account_cfg(a).display().to_string())
                .unwrap_or_default();
            for tab in &slot.tabs {
                let lines = {
                    let mut p = tab.parser.lock().unwrap_or_else(|e| e.into_inner());
                    recent_lines(&mut p, LINES_BACK)
                };
                for (line, back) in matches_in(&lines, q, PER_TAB) {
                    hits.push(FindHit {
                        uid: tab.uid,
                        place: format!("{acct} · {}", tab.name()),
                        line,
                        back,
                    });
                }
            }
        }

        hits.sort_by_key(|h| h.back);
        hits.truncate(MAX_HITS);
        hits
    }

    pub fn on_find_all_key(&mut self, k: KeyEvent) {
        let Modal::FindAll(mut fa) = self.modal.clone() else {
            return;
        };
        let n = fa.hits.len();
        match k.code {
            KeyCode::Esc => {
                self.modal = Modal::None;
                return;
            }
            KeyCode::Enter => {
                self.find_all_go(fa.sel);
                return;
            }
            KeyCode::Up => fa.sel = fa.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab if n > 0 => fa.sel = (fa.sel + 1) % n,
            KeyCode::Backspace => {
                fa.input.pop();
                fa.hits = self.find_all_hits(&fa.input);
                fa.sel = 0;
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                fa.input.push(c);
                fa.hits = self.find_all_hits(&fa.input);
                fa.sel = 0;
            }
            _ => {}
        }
        self.modal = Modal::FindAll(fa);
    }

    pub fn find_all_go(&mut self, i: usize) {
        let Modal::FindAll(fa) = self.modal.clone() else {
            return;
        };
        let Some(h) = fa.hits.get(i) else {
            return;
        };
        self.modal = Modal::None;
        if let Some((s, t)) = self.find_tab(h.uid) {
            self.panes[s].hidden = false;
            self.jump_to(s, t);
            self.flash(format!("Found in {}", h.place));
        }
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App, fa: &FindAll) {
    let w = area.width.saturating_sub(4).min(110);
    let rows = fa.hits.len().max(1) as u16;
    let h = (rows + 5).clamp(8, area.height.saturating_sub(2).max(8));
    let r = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h.min(area.height),
    );
    f.render_widget(Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
        .title(" find in every tab ")
        .style(Style::default().bg(BAR_BG));
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height < 3 || inner.width < 10 {
        return;
    }
    let limit = inner.x + inner.width;
    let buf = f.buffer_mut();
    let mut x = crate::hits::text(
        buf,
        inner.x,
        inner.y,
        limit,
        "find: ",
        Style::default().fg(theme::SAND),
    );
    x = crate::hits::text(buf, x, inner.y, limit, &fa.input, Style::default().fg(FG));
    crate::hits::text(
        buf,
        x,
        inner.y,
        limit,
        "▏",
        Style::default().fg(theme::SAND),
    );
    let mut hits = app.hits.borrow_mut();
    let top = inner.y + 2;
    let room = inner.y + inner.height - top;
    if fa.hits.is_empty() {
        let msg = if fa.input.trim().chars().count() < MIN_QUERY {
            "Type a word: every tab's recent output is searched, on every account."
        } else {
            "No tab shows that in its recent output."
        };
        crate::hits::text(buf, inner.x, top, limit, msg, Style::default().fg(DIM));
        return;
    }

    let first = fa.sel.saturating_sub(room.saturating_sub(1) as usize);
    let place_w = fa
        .hits
        .iter()
        .map(|h| h.place.chars().count())
        .max()
        .unwrap_or(0)
        .min(inner.width as usize / 3) as u16;
    for (k, hit) in fa.hits.iter().enumerate().skip(first).take(room as usize) {
        let y = top + (k - first) as u16;
        let sel = k == fa.sel;
        let bg = if sel { SEL_BG } else { BAR_BG };
        for cx in inner.x..limit {
            if let Some(c) = buf.cell_mut((cx, y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(bg));
            }
        }
        let mark = if sel { "› " } else { "  " };
        let mut cx = crate::hits::text(
            buf,
            inner.x,
            y,
            limit,
            mark,
            Style::default().fg(theme::SAND).bg(bg),
        );
        let place = crate::sessions::snippet(&hit.place, place_w as usize);
        crate::hits::text(
            buf,
            cx,
            y,
            limit,
            &place,
            Style::default().fg(theme::SLATE).bg(bg),
        );
        cx += place_w + 2;
        let lw = limit.saturating_sub(cx) as usize;
        let st = if sel {
            Style::default().fg(FG).bg(bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(FG).bg(bg)
        };
        crate::hits::text(buf, cx, y, limit, &around(&hit.line, &fa.input, lw), st);
        hits.add(
            Rect::new(inner.x, y, inner.width, 1),
            UiAction::FindAllRow(k),
            format!("Go to {} (Enter)", hit.place),
        );
    }
}
