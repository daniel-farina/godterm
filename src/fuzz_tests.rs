use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
}

const PIECES: &[&str] = &[
    "\x1b[2J",
    "\x1b[H",
    "\x1b[999;999H",
    "\x1b[0;0H",
    "\x1b[65535;65535H",
    "\x1b[?1049h",
    "\x1b[?1049l",
    "\x1b[?25l",
    "\x1b[?25h",
    "\x1b[?2004h",
    "\x1b[?1000h\x1b[?1006h",
    "\x1b[38;2;255;0;0m",
    "\x1b[48;5;300m",
    "\x1b[1;3;4;7;9m",
    "\x1b[0m",
    "\x1b[K",
    "\x1b[2K",
    "\x1b[J",
    "\x1b[10A",
    "\x1b[99999B",
    "\x1b[5C",
    "\x1b[5D",
    "\x1b[3;20r",
    "\x1b[r",
    "\x1b[S",
    "\x1b[T",
    "\x1b[4294967295L",
    "\x1b[M",
    "\x1b[@",
    "\x1b[P",
    "\x1b[6n",
    "\x1b[5n",
    "\x1b[c",
    "\x1b[>c",
    "\x1b[>q",
    "\x1b]0;title\x07",
    "\x1b]2;a very long title that goes on\x1b\\",
    "\x1b]10;?\x07",
    "\x1b]11;?\x1b\\",
    "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
    "\x1b]52;c;aGVsbG8=\x07",
    "\x1bP>|x\x1b\\",
    "\x1b7",
    "\x1b8",
    "\x1bM",
    "\x1bc",
    "\x1b(0lqk\x1b(B",
    "\r\n",
    "\n",
    "\r",
    "\t",
    "\x08\x08",
    "\x07",
    "\u{0}",
    "╭──────────────╮",
    "│ > ",
    "╰──────────────╯",
    "? for shortcuts",
    "Do you want to proceed?",
    "❯ 1. Yes",
    "  2. No, and tell Claude what to do differently (esc)",
    "Enter to select · Esc to cancel",
    "esc to interrupt",
    "✻ Thinking… (12s · ↑ 1.2k tokens)",
    "⏺ Bash(cargo test)",
    "Opus 4.7 · 40% left",
    " Weekly limit left: 40% · Grok 4.7 (high) · always-approve",
    "Claude usage limit reached. Your limit will reset at 5pm",
    "https://claude.ai/oauth/authorize?code=abc",
    "日本語の文字",
    "e\u{301}\u{301}",
    "👩‍👩‍👧‍👦",
    "\u{202e}rtl",
    "\u{200b}",
    "\u{fe0f}",
    "\u{ffff}",
];

fn stream(r: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..r.below(60) + 1 {
        match r.below(10) {
            0..=5 => out.extend_from_slice(r.pick(PIECES).as_bytes()),
            6 => {
                for _ in 0..r.below(40) {
                    out.push(r.next() as u8);
                }
            }
            7 => out.extend(std::iter::repeat_n(b'x', r.below(500))),
            8 => {
                out.extend_from_slice(b"\x1b[");
                for _ in 0..r.below(6) {
                    out.extend_from_slice(r.next().to_string().as_bytes());
                    out.push(b';');
                }
                out.push(*r.pick(b"ABCDHJKLMPSTXZ@`fmnrhlq"));
            }
            _ => {
                let s = r.pick(PIECES).as_bytes();
                out.extend_from_slice(&s[..r.below(s.len() + 1)]);
            }
        }
    }
    out
}

fn text_of(r: &mut Rng) -> String {
    String::from_utf8_lossy(&stream(r)).into_owned()
}

fn read_screen(screen: &vt100::Screen, chunk: &[u8], area: ratatui::layout::Rect) {
    use crate::harness::Harness;
    let _ = crate::pane::query_replies(chunk, screen);
    let text = screen.contents();
    for h in crate::harness::ALL {
        let _ = crate::app_tabmodel::input_empty(*h, screen);
        let _ = crate::app_tabmodel::screen_model(*h, &text);
        let _ = crate::app_tabmodel::screen_effort(*h, &text);
    }
    let _ = Harness::Claude;
    let _ = crate::pane::detect_activity(&text);
    let _ = crate::pane::asks_question(&text);
    let _ = crate::prompt::parse_prompt(&text);
    let _ = crate::app_failover::limit_error(&text);
    let _ = crate::app_busy::scan_side(&text, "what is it?");
    let _ = crate::app_direct::screen_reply(&text, "fix it");
    let _ = crate::app_continue::background_busy(&text);
    let _ = crate::archive::unsent_input(&text);
    let _ = crate::admin::url_on_screen(&text);
    let _ = crate::control::shows_prompt(&text, "fix it");
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::ui::render_screen(screen, area, &mut buf);
}

#[test]
fn random_pty_output_never_panics_the_parser_or_a_screen_reader() {
    use ratatui::layout::Rect;
    let mut slowest = std::time::Duration::ZERO;
    for seed in 0..300u64 {
        let mut r = Rng::new(seed);
        let (rows, cols) = (r.below(60) as u16 + 2, r.below(200) as u16 + 10);
        let mut p = vt100::Parser::new(rows, cols, r.below(500));

        let mut guard = crate::vt_guard::CsiCap::default();
        for _ in 0..r.below(12) + 1 {
            let chunk = stream(&mut r);
            let mut clean = Vec::new();
            let (h, w) = p.screen().size();
            guard.filter(&chunk, h.max(w), &mut clean);
            let t = std::time::Instant::now();
            let _ = crate::pane::process_guarded(&mut p, &clean, 500);
            slowest = slowest.max(t.elapsed());
            let area = Rect::new(
                r.below(5) as u16,
                r.below(5) as u16,
                r.below(220) as u16,
                r.below(70) as u16,
            );
            read_screen(p.screen(), &chunk, area);
            if r.chance(20) {
                let (rows, cols) = (r.below(80) as u16 + 2, r.below(300) as u16 + 10);
                p.screen_mut().set_size(rows, cols);
            }
            if r.chance(20) {
                let s = p.screen_mut();
                s.set_scrollback(r.below(1000));
                let _ = crate::select::scrollback_len(s);
                let _ = crate::select::line_of_row(s, r.below(rows as usize + 5) as u16);
                s.set_scrollback(0);
            }
        }
    }

    assert!(
        slowest < std::time::Duration::from_millis(500),
        "slowest chunk {slowest:?}"
    );
}

#[test]
fn random_text_never_panics_the_text_helpers() {
    for seed in 0..400u64 {
        let mut r = Rng::new(1_000 + seed);
        let t = text_of(&mut r);
        let w = r.below(120);
        let _ = crate::md::view(&t, w);
        let _ = crate::md::reply_view(&t, r.below(2_000), w);
        let _ = crate::md::reply(&t, r.below(2_000));
        let _ = crate::md::plain(&t);
        let _ = crate::assistant::sentences(&t);
        let _ = crate::assistant::sentence_spans(&t);
        let _ = crate::assistant::spoken_part(&t, r.below(500));
        let _ = crate::app_direct::speakable(&t);
        let _ = crate::app_direct::spoken_reply(&t, r.below(10));
        let _ = crate::app_privacy::mask_emails(&t);
        let _ = crate::app_privacy::middle_truncate(&t, r.below(40));
        let _ = crate::paths::middle(&t, r.below(40));
        let _ = crate::paths::shorten_path(&t, r.below(40));
        let _ = crate::paths::head(&t, r.below(40));
        let _ = crate::keys::encode_paste(&t, r.chance(50));
        let _ = crate::fuzzy::key(&t);
        let _ = crate::fuzzy::sound(&t);
        let _ = crate::fuzzy::spoken_punctuation(&t);
        let _ = crate::jobs::parse_every(&t);
        let _ = crate::app_jobs::parse_when(&t, 1_790_000_000, r.chance(50));
        let _ = crate::app_memory::parse_duration(&t);
        let _ = crate::layout::parse_grid(&t);
        let _ = crate::session_rename::clean_name(&t);
        let _ = crate::app_busy::is_question(&t, None);
        let _ = crate::assistant_images::paste_paths(&t);

        let rows = crate::input_edit::rows(&t, w);
        for _ in 0..5 {
            let pos = crate::input_edit::snap(&t, r.below(t.len() + 3));
            let _ = crate::input_edit::caret_rc(&t, &rows, pos, w);
            let _ = crate::input_edit::pos_at(&t, &rows, r.below(rows.len() + 3), r.below(w + 3));
        }
    }
}

fn app(tag: &str) -> (crate::app::App, std::path::PathBuf) {
    let home = std::env::temp_dir().join(format!("godterm-fuzz-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    crate::config::testing::set_home(&home);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut cfg = crate::config::Config::default();
    cfg.claude_bin = Some(crate::test_stub::true_bin());
    cfg.autostart = false;
    cfg.save().unwrap();
    (crate::app::App::new(cfg, tx), home)
}

const COMMANDS: &[char] = &[
    'h', 'j', 'k', 'l', '1', '2', '3', '9', '[', ']', 'L', 'o', '|', 'n', 'p', 'P', 'z', ';', '\'',
    'b', 'y', 'g', 'G', '@', '?', 'c', 'U', '{', '}', '<', '>', '.', 'Z', 't', 'w', '_', '/',
];

fn key(code: KeyCode, m: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, m))
}

fn random_event(r: &mut Rng, w: u16, h: u16) -> Vec<Event> {
    let any = |r: &mut Rng, n: u16| -> u16 {
        match r.below(10) {
            0 => u16::MAX,
            1 => n.saturating_add(r.below(10) as u16),
            _ => r.below(n as usize + 1) as u16,
        }
    };
    match r.below(10) {
        0..=2 => {
            let (column, row) = (any(r, w), any(r, h));
            let kind = *r.pick(&[
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::Drag(MouseButton::Left),
                MouseEventKind::Down(MouseButton::Right),
                MouseEventKind::Down(MouseButton::Middle),
                MouseEventKind::Moved,
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollLeft,
            ]);
            let modifiers = *r.pick(&[KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::ALT]);
            let m = MouseEvent {
                kind,
                column,
                row,
                modifiers,
            };
            if kind == MouseEventKind::Down(MouseButton::Left) && r.chance(60) {
                let up = MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    ..m
                };
                vec![Event::Mouse(m), Event::Mouse(up)]
            } else {
                vec![Event::Mouse(m)]
            }
        }
        3..=4 => vec![
            key(KeyCode::Char('a'), KeyModifiers::CONTROL),
            key(KeyCode::Char(*r.pick(COMMANDS)), KeyModifiers::NONE),
        ],
        5 => vec![Event::Paste(text_of(r))],
        6 => vec![key(
            *r.pick(&[
                KeyCode::Esc,
                KeyCode::Enter,
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::Backspace,
                KeyCode::Delete,
                KeyCode::Up,
                KeyCode::Down,
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::Home,
                KeyCode::End,
                KeyCode::PageUp,
                KeyCode::PageDown,
                KeyCode::F(1),
            ]),
            *r.pick(&[KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::ALT]),
        )],
        7 => {
            let s = text_of(r);
            s.chars()
                .filter(|c| !c.is_control())
                .take(20)
                .map(|c| key(KeyCode::Char(c), KeyModifiers::NONE))
                .collect()
        }
        8 => vec![Event::FocusGained, Event::FocusLost],
        _ => vec![key(KeyCode::Esc, KeyModifiers::NONE)],
    }
}

#[test]
fn random_terminal_events_at_any_size_never_panic_the_app() {
    let _g = crate::config::testing::LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = app("events");
    let sizes: &[(u16, u16)] = &[
        (1, 1),
        (2, 2),
        (5, 3),
        (10, 4),
        (20, 6),
        (40, 10),
        (80, 24),
        (120, 40),
        (200, 60),
        (400, 120),
        (30, 150),
        (300, 5),
    ];
    let mut r = Rng::new(7);
    for step in 0..1_500 {
        let (w, h) = *r.pick(sizes);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        for _ in 0..r.below(6) + 1 {
            for ev in random_event(&mut r, w, h) {
                app.handle(crate::app::AppEvent::Input(ev));
            }
            if r.chance(10) {
                app.handle(crate::app::AppEvent::Input(Event::Resize(w, h)));
            }
            term.draw(|f| crate::ui::draw(f, &mut app))
                .unwrap_or_else(|e| panic!("step {step} at {w}x{h}: {e}"));
        }

        app.quit = false;
    }
    for p in app.panes.iter_mut() {
        p.kill_all();
    }
    let _ = std::fs::remove_dir_all(&home);
}
