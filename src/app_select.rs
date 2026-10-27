use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::{Duration, Instant};

use crate::app::{App, Modal, View};
use crate::hits::{Region, UiAction};
use crate::pane::PaneState;
use crate::select::{self, Pos, Press, Selection, Target, Unit};

const MULTI_CLICK: Duration = Duration::from_millis(450);

impl App {
    pub fn selection_mouse(&mut self, m: &MouseEvent, region: Option<&Region>) -> bool {
        if let Some(target) = self
            .selection
            .as_ref()
            .filter(|s| s.dragging)
            .map(|s| s.target)
        {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.extend_selection(target, m.column, m.row);
                    return true;
                }
                MouseEventKind::Up(_) => {
                    self.finish_selection();
                    return true;
                }

                _ => self.finish_selection(),
            }
        }
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return false;
        }
        let Some((target, take)) = self.selectable_at(m, region) else {
            if self.modal == Modal::None {
                self.selection = None;
            }
            return false;
        };
        let Some((pos, _)) = self.sel_pos(target, m.column, m.row) else {
            return false;
        };
        let count = match self.sel_press {
            Some(p) if p.target == target && p.pos == pos && p.at.elapsed() < MULTI_CLICK => {
                p.count % 3 + 1
            }
            _ => 1,
        };
        self.sel_press = Some(Press {
            at: Instant::now(),
            target,
            pos,
            count,
        });
        let unit = match count {
            2 => Unit::Word,
            3 => Unit::Line,
            _ => Unit::Char,
        };
        self.selection = Some(Selection::new(target, pos, unit));
        if take {
            if let Target::Pane { slot, .. } = target {
                self.focus = slot;
            }
        }
        take
    }

    fn selectable_at(&self, m: &MouseEvent, region: Option<&Region>) -> Option<(Target, bool)> {
        if self.modal != Modal::None {
            return None;
        }
        if let Some((r, _)) = self.assistant_text.borrow().as_ref() {
            if crate::hits::contains(*r, m.column, m.row) {
                return Some((Target::Assistant, false));
            }
        }
        let Some(UiAction::PaneBody(i)) = region.map(|r| r.action.clone()) else {
            return None;
        };
        if self.view != View::Grid {
            return None;
        }
        let pane = self.panes.get(i)?.cur();
        if !matches!(pane.state, PaneState::Running | PaneState::Exited(_)) {
            return None;
        }
        let forced = m
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        let reports = pane.is_running()
            && pane
                .parser
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .screen()
                .mouse_protocol_mode()
                != vt100::MouseProtocolMode::None;
        if reports && !forced {
            return None;
        }
        Some((
            Target::Pane {
                slot: i,
                uid: pane.uid,
            },
            reports,
        ))
    }

    fn sel_pos(&mut self, target: Target, col: u16, row: u16) -> Option<(Pos, i8)> {
        let rect = match target {
            Target::Pane { slot, .. } => *self.term_rects.get(slot)?,
            Target::Assistant => self.assistant_text.borrow().as_ref()?.0,
        };
        if rect.width == 0 || rect.height == 0 {
            return None;
        }
        let edge = if row < rect.y {
            -1
        } else if row >= rect.y + rect.height {
            1
        } else {
            0
        };
        let c = col.clamp(rect.x, rect.x + rect.width - 1) - rect.x;
        let r = row.clamp(rect.y, rect.y + rect.height - 1) - rect.y;
        let line = match target {
            Target::Pane { slot, .. } => {
                let pane = self.panes[slot].cur();
                let mut p = pane.parser.lock().unwrap_or_else(|e| e.into_inner());
                let s = p.screen_mut();
                let r = r.min(s.size().0.saturating_sub(1));
                select::line_of_row(s, r)
            }
            Target::Assistant => r as usize,
        };
        Some((Pos { line, col: c }, edge))
    }

    fn extend_selection(&mut self, target: Target, col: u16, row: u16) {
        let Some((pos, edge)) = self.sel_pos(target, col, row) else {
            return;
        };
        if let Some(s) = self.selection.as_mut() {
            s.edge = edge;
            s.moved = s.moved || pos != s.anchor;
            s.head = pos;
        }

        if edge != 0 {
            self.selection_scroll(edge, 1);
        }
    }

    fn selection_scroll(&mut self, edge: i8, lines: isize) {
        let Some(Selection {
            target: Target::Pane { slot, .. },
            head,
            ..
        }) = self.selection.clone()
        else {
            return;
        };
        let pane = self.panes[slot].cur_mut();
        pane.scroll_by(if edge < 0 { lines } else { -lines });
        let mut p = pane.parser.lock().unwrap_or_else(|e| e.into_inner());
        let s = p.screen_mut();
        let row = if edge < 0 {
            0
        } else {
            s.size().0.saturating_sub(1)
        };
        let line = select::line_of_row(s, row);
        drop(p);
        if let Some(sel) = self.selection.as_mut() {
            sel.head = Pos {
                line,
                col: head.col,
            };
            sel.moved = sel.moved || sel.head != sel.anchor;
        }
    }

    pub fn selection_tick(&mut self) {
        if let Some(edge) = self
            .selection
            .as_ref()
            .filter(|s| s.dragging && s.edge != 0)
            .map(|s| s.edge)
        {
            self.selection_scroll(edge, 2);
        }
    }

    fn finish_selection(&mut self) {
        let Some(s) = self.selection.as_mut() else {
            return;
        };
        s.dragging = false;
        s.edge = 0;
        if !s.visible() {
            self.selection = None;
            return;
        }
        self.copy_selection();
    }

    pub fn selection_text(&self) -> Option<String> {
        let sel = self.selection.as_ref().filter(|s| s.visible())?;
        let t = match sel.target {
            Target::Pane { slot, uid } => {
                let pane = self.panes.get(slot)?.cur();
                if pane.uid != uid {
                    return None;
                }
                let mut p = pane.parser.lock().unwrap_or_else(|e| e.into_inner());
                select::text(&mut select::ScreenLines(p.screen_mut()), sel)
            }
            Target::Assistant => {
                let at = self.assistant_text.borrow();
                let (_, lines) = at.as_ref()?;
                select::text(&mut select::TextLines(lines), sel)
            }
        };
        Some(t)
    }

    pub fn copy_selection(&mut self) {
        match self.selection_text() {
            None => self.flash("Nothing selected: drag over text in a pane to select it"),
            Some(t) if t.trim().is_empty() => self.flash("Nothing to copy: only blank space"),
            Some(t) => self.copy_text(t),
        }
    }

    pub fn copy_text(&mut self, text: String) {
        self.flash(select::copied_msg(&text));
        let quiet = cfg!(test) || crate::demo::active();
        if !quiet {
            if crate::procs::osc52_wanted() {
                crate::tty_out::write_raw(select::osc52(&text).into_bytes());
            }
            crate::procs::copy_to_clipboard(text.clone());
        }
        self.last_copied = Some(text);
    }

    pub fn selection_key(&mut self, k: &KeyEvent) -> bool {
        let Some(sel) = &self.selection else {
            return false;
        };
        let here = match sel.target {
            Target::Pane { slot, .. } => {
                self.view == View::Grid && slot == self.focus && !self.assistant_has_focus()
            }
            Target::Assistant => self.assistant_has_focus(),
        };
        if !here {
            return false;
        }
        let shown = sel.visible();
        self.selection = None;
        k.code == KeyCode::Esc && shown
    }
}

#[cfg(test)]
mod tests {
    use crate::config::testing::LOCK as ENV_LOCK;
    use crate::hits::UiAction;
    use crate::pane::PaneState;
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn test_app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!("godterm-sel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        crate::config::testing::set_home(&home);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(crate::config::Config::default(), tx);
        let base = home.join("newtabs");
        std::fs::create_dir_all(&base).unwrap();
        app.cfg.new_tab_base = base.to_string_lossy().into_owned();

        app.cfg.claude_bin = Some(crate::test_stub::true_bin());
        (app, home)
    }

    fn draw(app: &mut crate::app::App) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 44)).unwrap();
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        term
    }

    fn mouse(app: &mut crate::app::App, kind: MouseEventKind, col: u16, row: u16, m: KeyModifiers) {
        app.handle(crate::app::AppEvent::Input(Event::Mouse(MouseEvent {
            kind,
            column: col,
            row,
            modifiers: m,
        })));
    }

    fn fill(app: &mut crate::app::App, slot: usize, text: &str) -> ratatui::layout::Rect {
        let p = app.panes[slot].cur_mut();
        p.state = PaneState::Exited(Some(0));
        let mut term = draw(app);
        let p = app.panes[slot].cur_mut();
        p.parser
            .lock()
            .unwrap()
            .process(format!("\x1b[H{text}").as_bytes());
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        app.term_rects[slot]
    }

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const DOWN: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
    const UP: MouseEventKind = MouseEventKind::Up(MouseButton::Left);
    const DRAG: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);

    #[test]
    fn plain_click_focuses_and_selects_nothing() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = test_app("click");
        let t = fill(&mut app, 1, "hello world");
        assert_eq!(app.focus, 0);
        assert!(app
            .last_hits
            .regions
            .iter()
            .any(|r| r.action == UiAction::PaneBody(1)));
        mouse(&mut app, DOWN, t.x + 2, t.y, NONE);
        mouse(&mut app, UP, t.x + 2, t.y, NONE);

        assert_eq!(app.focus, 1);
        assert!(app.selection.is_none());
        assert!(app.last_copied.is_none());
        assert_eq!(app.modal, crate::app::Modal::None);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn drag_selects_in_its_pane_and_copies() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = test_app("drag");
        let t = fill(&mut app, 0, "hello world\r\nsecond row");
        mouse(&mut app, DOWN, t.x + 6, t.y, NONE);

        mouse(&mut app, DRAG, t.x + 3, t.y + 1, NONE);
        mouse(&mut app, DRAG, t.x + t.width + 30, t.y + 1, NONE);
        mouse(&mut app, UP, t.x + t.width + 30, t.y + 1, NONE);
        assert_eq!(app.last_copied.as_deref(), Some("world\nsecond row"));
        assert_eq!(app.flash.as_ref().unwrap().0, "Copied 16 characters");

        let term = draw(&mut app);
        let b = term.backend().buffer();
        assert_eq!(b[(t.x + 6, t.y)].bg, crate::theme::SELECT_BG);
        assert_ne!(b[(t.x + 5, t.y)].bg, crate::theme::SELECT_BG);

        app.last_copied = None;
        app.on_command(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            NONE,
        ));
        assert_eq!(app.last_copied.as_deref(), Some("world\nsecond row"));
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            NONE,
        ));
        assert!(app.selection.is_none());

        mouse(&mut app, DOWN, t.x + 8, t.y, NONE);
        mouse(&mut app, UP, t.x + 8, t.y, NONE);
        mouse(&mut app, DOWN, t.x + 8, t.y, NONE);
        mouse(&mut app, UP, t.x + 8, t.y, NONE);
        assert_eq!(app.last_copied.as_deref(), Some("world"));
        mouse(&mut app, DOWN, t.x + 8, t.y, NONE);
        mouse(&mut app, UP, t.x + 8, t.y, NONE);
        assert_eq!(app.last_copied.as_deref(), Some("hello world"));

        mouse(&mut app, DOWN, 1, 0, NONE);
        assert!(app.selection.is_none());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn drag_past_the_top_scrolls_history() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = test_app("scroll");
        let t = fill(&mut app, 0, "");
        let lines: String = (0..80).map(|n| format!("line {n}\r\n")).collect();
        app.panes[0]
            .cur_mut()
            .parser
            .lock()
            .unwrap()
            .process(lines.as_bytes());
        let _ = draw(&mut app);
        let bottom = t.y + t.height - 1;
        mouse(&mut app, DOWN, t.x, bottom, NONE);
        for _ in 0..5 {
            mouse(&mut app, DRAG, t.x, t.y - 1, NONE);
        }
        assert_eq!(app.panes[0].cur().scroll, 5);
        mouse(&mut app, UP, t.x, t.y - 1, NONE);
        let got = app.last_copied.clone().unwrap();

        let first_shown = 80 + 1 - t.height as usize;
        assert!(
            got.starts_with(&format!("line {}\n", first_shown - 5)),
            "{got}"
        );
        assert!(got.ends_with("line 79"), "{got}");
        assert_eq!(got.lines().count(), t.height as usize + 4);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn assistant_conversation_selects_too() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = test_app("panel");
        app.toggle_assistant_panel();
        let _ = draw(&mut app);
        let (body, lines) = app.assistant_text.borrow().clone().unwrap();
        let first = lines.iter().position(|l| !l.trim().is_empty()).unwrap();
        let y = body.y + first as u16;
        mouse(&mut app, DOWN, body.x, y, NONE);
        mouse(&mut app, DRAG, body.x + body.width + 10, y, NONE);
        mouse(&mut app, UP, body.x + body.width + 10, y, NONE);
        assert_eq!(app.last_copied.as_deref(), Some(lines[first].trim_end()));

        let term = draw(&mut app);
        assert_eq!(
            term.backend().buffer()[(body.x, y)].bg,
            crate::theme::SELECT_BG
        );
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            NONE,
        ));
        assert!(app.selection.is_none());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn mouse_reporting_tab_gets_plain_drags_shift_selects() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = test_app("report");

        let t = fill(&mut app, 0, "\x1b[?1000hpick me");

        mouse(&mut app, DOWN, t.x, t.y, NONE);
        mouse(&mut app, DRAG, t.x + 3, t.y, NONE);
        mouse(&mut app, UP, t.x + 3, t.y, NONE);
        assert_eq!(app.last_copied.as_deref(), Some("pick"));

        app.panes[0].cur_mut().state = PaneState::Running;
        let _ = draw(&mut app);
        app.last_copied = None;
        mouse(&mut app, DOWN, t.x, t.y, NONE);
        assert!(app.selection.is_none());
        mouse(&mut app, DRAG, t.x + 3, t.y, NONE);
        mouse(&mut app, UP, t.x + 3, t.y, NONE);
        assert!(app.last_copied.is_none());

        mouse(&mut app, DOWN, t.x + 5, t.y, KeyModifiers::SHIFT);
        mouse(&mut app, DRAG, t.x + 6, t.y, KeyModifiers::SHIFT);
        mouse(&mut app, UP, t.x + 6, t.y, KeyModifiers::SHIFT);
        assert_eq!(app.last_copied.as_deref(), Some("me"));
        app.panes[0].cur_mut().state = PaneState::Exited(Some(0));
        let _ = std::fs::remove_dir_all(home);
    }
}
