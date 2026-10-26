//! PTY output on its way to the vt100 parser: counts in the CSI sequences
//! that vt100 runs once per unit are capped at the screen's size.
//!
//! vt100 0.16 inserts characters one at a time, so `ESC [ 65535 @` (ICH;
//! any count above 65535 is read as 65535) takes over a second, and
//! inserting or scrolling lines (IL, SD) by thousands takes milliseconds
//! each. A tab that prints a binary file, or a program that sends such
//! counts, held the parser lock for that long per sequence and the UI,
//! which draws from the same parser, froze with it. Counts larger than the
//! screen do nothing more than the screen size does (the rest falls off the
//! edge), so capping them changes nothing on screen.

/// Sequences whose first parameter is a repeat count: ICH, IL, DL, DCH,
/// SU, SD, ECH (`ESC [ n @` and friends).
const COUNTED: &[u8] = b"@LMPSTX";

/// A CSI longer than this is passed on as it is (vt100 drops it).
const MAX_SEQ: usize = 256;

#[derive(Default)]
pub struct CsiCap {
    /// A sequence started in an earlier read: ESC, or ESC [ and what
    /// followed so far.
    pending: Vec<u8>,
}

impl CsiCap {
    /// Copy `input` to `out`, capping counts at `cap`. A sequence cut by the
    /// end of `input` is held back until the next call.
    pub fn filter(&mut self, input: &[u8], cap: u16, out: &mut Vec<u8>) {
        let cap = cap.max(1);
        for &b in input {
            if self.pending.is_empty() {
                if b == 0x1b {
                    self.pending.push(b);
                } else {
                    out.push(b);
                }
                continue;
            }
            if self.pending.len() == 1 {
                // After ESC: a CSI starts, or it was another escape.
                if b == b'[' {
                    self.pending.push(b);
                } else {
                    out.push(0x1b);
                    self.pending.clear();
                    if b == 0x1b {
                        self.pending.push(b);
                    } else {
                        out.push(b);
                    }
                }
                continue;
            }
            if b == 0x1b {
                // A new escape cuts the sequence short (vt100 does the same).
                out.append(&mut self.pending);
                self.pending.push(b);
                continue;
            }
            self.pending.push(b);
            if (0x40..=0x7e).contains(&b) {
                rewrite(&self.pending, cap, out);
                self.pending.clear();
            } else if self.pending.len() > MAX_SEQ {
                out.append(&mut self.pending);
            }
        }
    }
}

/// Emit the whole sequence `seq` (ESC [ ... final), its counts capped.
fn rewrite(seq: &[u8], cap: u16, out: &mut Vec<u8>) {
    let fin = seq[seq.len() - 1];
    let params = &seq[2..seq.len() - 1];
    // Only plain counts: no private marker, intermediate or sub-parameter.
    if !COUNTED.contains(&fin) || !params.iter().all(|c| c.is_ascii_digit() || *c == b';') {
        out.extend_from_slice(seq);
        return;
    }
    out.extend_from_slice(b"\x1b[");
    for (i, p) in params.split(|c| *c == b';').enumerate() {
        if i > 0 {
            out.push(b';');
        }
        let big = p.len() > 5
            || std::str::from_utf8(p)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|n| n > u32::from(cap));
        if big {
            out.extend_from_slice(cap.to_string().as_bytes());
        } else {
            out.extend_from_slice(p);
        }
    }
    out.push(fin);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&[u8]], cap: u16) -> Vec<u8> {
        let mut f = CsiCap::default();
        let mut out = Vec::new();
        for c in chunks {
            f.filter(c, cap, &mut out);
        }
        out
    }

    #[test]
    fn big_counts_are_capped_and_the_rest_is_untouched() {
        assert_eq!(run(&[b"\x1b[4294967295@"], 80), b"\x1b[80@");
        assert_eq!(run(&[b"a\x1b[65535Lb"], 24), b"a\x1b[24Lb");
        assert_eq!(run(&[b"\x1b[999999999999999999999T"], 50), b"\x1b[50T");
        assert_eq!(run(&[b"\x1b[30@\x1b[81M"], 80), b"\x1b[30@\x1b[80M");
        // Not a count: positions, colours, modes, OSC, private sequences.
        for s in [
            &b"\x1b[65535;65535H"[..],
            b"\x1b[38;2;255;0;0m",
            b"\x1b[?1049h",
            b"\x1b[?2004l",
            b"\x1b[>4;2m",
            b"\x1b[4:3m",
            b"\x1b]0;99999 title\x07",
            b"\x1b[ q",
            b"\x1bc\x1b7\x1b8",
            b"\x1b[@",
            b"\x1b[L",
            "日本語 ✻ 🚀".as_bytes(),
        ] {
            assert_eq!(run(&[s], 80), s, "{:?}", String::from_utf8_lossy(s));
        }
    }

    #[test]
    fn a_sequence_cut_across_reads_is_held_then_capped() {
        assert_eq!(run(&[b"x\x1b", b"[9999", b"9@y"], 100), b"x\x1b[100@y");
        // Held, nothing lost, until the rest comes.
        let mut f = CsiCap::default();
        let mut out = Vec::new();
        f.filter(b"ab\x1b[12", 80, &mut out);
        assert_eq!(out, b"ab");
        f.filter(b"3;4H", 80, &mut out);
        assert_eq!(out, b"ab\x1b[123;4H");
        // An escape that cuts a CSI short passes both on.
        assert_eq!(run(&[b"\x1b[12\x1b[3@"], 80), b"\x1b[12\x1b[3@");
        assert_eq!(run(&[b"\x1b\x1b[5@"], 80), b"\x1b\x1b[5@");
        // An endless one is let through once it is too long.
        let long = [b"\x1b[".to_vec(), vec![b'1'; 300]].concat();
        assert_eq!(run(&[&long], 80), long);
    }

    #[test]
    fn the_screen_is_the_same_with_or_without_the_cap() {
        for (seq, rows, cols) in [
            ("abc\x1b[1;2H\x1b[65535@", 5u16, 20u16),
            ("1\r\n2\r\n3\r\n4\x1b[2;1H\x1b[65535L", 6, 10),
            ("1\r\n2\r\n3\r\n4\x1b[2;1H\x1b[9999M", 6, 10),
            ("abcdef\x1b[1;3H\x1b[70000P", 3, 12),
            ("abcdef\x1b[1;3H\x1b[70000X", 3, 12),
        ] {
            let mut a = vt100::Parser::new(rows, cols, 0);
            a.process(seq.as_bytes());
            let mut b = vt100::Parser::new(rows, cols, 0);
            b.process(&run(&[seq.as_bytes()], rows.max(cols)));
            assert_eq!(a.screen().contents(), b.screen().contents(), "{seq:?}");
        }
    }

    #[test]
    fn a_capped_insert_is_fast() {
        let mut p = vt100::Parser::new(60, 200, 500);
        let t = std::time::Instant::now();
        let bytes: Vec<u8> = b"\x1b[4294967295@\x1b[65535L\x1b[65535T".repeat(200);
        p.process(&run(&[&bytes], 200));
        assert!(
            t.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            t.elapsed()
        );
    }
}
