//! Translate crossterm key events into the byte sequences a terminal sends.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Encode a key for the child PTY. `app_cursor` is DECCKM (application cursor
/// keys) as reported by the child's screen state.
pub fn encode_key(key: &KeyEvent, app_cursor: bool) -> Vec<u8> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);
    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl
    let modp = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;

    let csi_mod = |base: &str, fin: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{fin}").into_bytes()
        } else {
            base.as_bytes().to_vec()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[{n};{modp}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let arrow = |fin: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{fin}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{fin}").into_bytes()
        } else {
            format!("\x1b[{fin}").into_bytes()
        }
    };

    let mut out = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                match ctrl_byte(c) {
                    Some(b) => vec![b],
                    None => c.to_string().into_bytes(),
                }
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => {
            // Shift+Enter inserts a newline in Claude Code when sent as ESC CR.
            if shift || alt {
                return b"\x1b\r".to_vec();
            }
            b"\r".to_vec()
        }
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if ctrl {
                vec![0x08]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => arrow('A'),
        KeyCode::Down => arrow('B'),
        KeyCode::Right => arrow('C'),
        KeyCode::Left => arrow('D'),
        KeyCode::Home => csi_mod("\x1b[H", 'H'),
        KeyCode::End => csi_mod("\x1b[F", 'F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n) => match n {
            1 => csi_mod("\x1bOP", 'P'),
            2 => csi_mod("\x1bOQ", 'Q'),
            3 => csi_mod("\x1bOR", 'R'),
            4 => csi_mod("\x1bOS", 'S'),
            5 => tilde(15),
            6 => tilde(17),
            7 => tilde(18),
            8 => tilde(19),
            9 => tilde(20),
            10 => tilde(21),
            11 => tilde(23),
            12 => tilde(24),
            _ => vec![],
        },
        _ => vec![],
    };
    // Alt as meta prefix for plain characters and a few control keys.
    if alt
        && matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Esc | KeyCode::Tab
        )
    {
        out.insert(0, 0x1b);
    }
    out
}

fn ctrl_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Wrap pasted text for the child, honoring bracketed paste mode.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{body}\x1b[201~").into_bytes()
    } else {
        body.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn basic_keys() {
        let n = KeyModifiers::NONE;
        assert_eq!(encode_key(&k(KeyCode::Char('x'), n), false), b"x");
        assert_eq!(
            encode_key(&k(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            [3]
        );
        assert_eq!(
            encode_key(&k(KeyCode::Char('b'), KeyModifiers::ALT), false),
            b"\x1bb"
        );
        assert_eq!(encode_key(&k(KeyCode::Enter, n), false), b"\r");
        assert_eq!(
            encode_key(&k(KeyCode::Enter, KeyModifiers::SHIFT), false),
            b"\x1b\r"
        );
        assert_eq!(encode_key(&k(KeyCode::Up, n), false), b"\x1b[A");
        assert_eq!(encode_key(&k(KeyCode::Up, n), true), b"\x1bOA");
        assert_eq!(
            encode_key(&k(KeyCode::Left, KeyModifiers::CONTROL), true),
            b"\x1b[1;5D"
        );
        assert_eq!(encode_key(&k(KeyCode::Delete, n), false), b"\x1b[3~");
        assert_eq!(
            encode_key(&k(KeyCode::BackTab, KeyModifiers::SHIFT), false),
            b"\x1b[Z"
        );
        assert_eq!(encode_key(&k(KeyCode::Backspace, n), false), [0x7f]);
    }

    /// Property style: thousands of random key events never panic and keep
    /// basic invariants.
    #[test]
    fn random_keys_keep_invariants() {
        let mut seed: u64 = 0x2545F4914F6CDD1D;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let codes = [
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::Esc,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::F(1),
            KeyCode::F(5),
            KeyCode::F(12),
            KeyCode::F(20),
            KeyCode::Null,
            KeyCode::CapsLock,
        ];
        for _ in 0..20_000 {
            let r = rnd();
            let mods = KeyModifiers::from_bits_truncate((r >> 8) as u8 & 0b0000_0111);
            let code = if r % 3 == 0 {
                codes[(r as usize >> 16) % codes.len()]
            } else {
                // Random unicode scalar values, including control and wide chars.
                let c = char::from_u32(((r >> 20) % 0x2_0000) as u32).unwrap_or('x');
                KeyCode::Char(c)
            };
            let ev = KeyEvent::new(code, mods);
            let app = r & 1 == 0;
            let out = encode_key(&ev, app);
            if let KeyCode::Char(c) = code {
                let ctrl = mods.contains(KeyModifiers::CONTROL);
                let alt = mods.contains(KeyModifiers::ALT);
                if !ctrl && !alt {
                    assert_eq!(out, c.to_string().into_bytes());
                }
                if ctrl && !alt && c.is_ascii_alphabetic() {
                    assert_eq!(out.len(), 1);
                    assert!((1..=26).contains(&out[0]));
                }
                if alt {
                    assert_eq!(out[0], 0x1b);
                }
            }
            if matches!(
                code,
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
            ) {
                assert_eq!(out[0], 0x1b);
                assert!(std::str::from_utf8(&out).is_ok());
            }
        }
    }

    #[test]
    fn paste() {
        assert_eq!(encode_paste("a\nb", true), b"\x1b[200~a\rb\x1b[201~");
        assert_eq!(encode_paste("a\r\nb", false), b"a\rb");
    }
}
