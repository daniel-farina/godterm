//! Calm, muted palette. No saturated or glowing hues.

use ratatui::style::Color;

pub const FG: Color = Color::Rgb(208, 204, 196);
pub const DIM: Color = Color::Rgb(132, 130, 124);
pub const FAINT: Color = Color::Rgb(84, 86, 90);
pub const BAR_BG: Color = Color::Rgb(38, 40, 43);
pub const SEL_BG: Color = Color::Rgb(50, 53, 57);

pub const SAGE: Color = Color::Rgb(138, 160, 128);
pub const SAND: Color = Color::Rgb(196, 176, 134);
pub const SLATE: Color = Color::Rgb(128, 148, 170);
/// Mic muted: a clear red, not neon.
pub const MUTED_RED: Color = Color::Rgb(178, 52, 46);
pub const CLAY: Color = Color::Rgb(184, 128, 106);
pub const MAUVE: Color = Color::Rgb(158, 134, 156);
pub const STONE: Color = Color::Rgb(160, 156, 144);
/// The one accent for live work (a tab running a turn): a muted teal,
/// clear against the calm palette without glowing.
pub const ACTIVE: Color = Color::Rgb(104, 176, 166);

pub const ROTATION: [&str; 6] = ["sage", "sand", "slate", "clay", "mauve", "stone"];

/// Account colors to pick from: the rotation plus four more muted ones.
pub const SWATCHES: [&str; 10] = [
    "sage", "sand", "slate", "clay", "mauve", "stone", "moss", "teal", "rose", "dusk",
];

/// Palette name or "#rrggbb". Unknown values fall back to stone.
pub fn parse_color(s: &str) -> Color {
    match s.trim().to_ascii_lowercase().as_str() {
        "sage" | "green" => SAGE,
        "sand" | "yellow" => SAND,
        "slate" | "blue" => SLATE,
        "clay" | "red" => CLAY,
        "mauve" | "purple" => MAUVE,
        "stone" | "gray" | "grey" => STONE,
        "moss" => Color::Rgb(143, 154, 106),
        "teal" => Color::Rgb(122, 156, 152),
        "rose" => Color::Rgb(180, 140, 146),
        "dusk" => Color::Rgb(138, 140, 174),
        hex => parse_hex(hex).unwrap_or(STONE),
    }
}

fn parse_hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// How usage is colored (`usage_colors`): 0 gradient, 1 bands, 2 mono.
static USAGE_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// The terminal shows 24 bit color (COLORTERM); otherwise the nearest of
/// the 256 color palette is used.
static TRUECOLOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_usage_colors(mode: &str) {
    let m = match mode {
        "bands" => 1,
        "mono" => 2,
        _ => 0,
    };
    USAGE_MODE.store(m, std::sync::atomic::Ordering::Relaxed);
}

pub fn detect_truecolor() {
    let ct = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_lowercase();
    let term = std::env::var("TERM_PROGRAM").unwrap_or_default();
    // Terminal.app has no 24 bit color; iTerm2, kitty, WezTerm, Ghostty do.
    let tc = ct.contains("truecolor")
        || ct.contains("24bit")
        || (term != "Apple_Terminal" && !term.is_empty());
    TRUECOLOR.store(tc, std::sync::atomic::Ordering::Relaxed);
}

/// Usage anchors by percent LEFT: muted sage, olive, sand, amber, clay.
const ANCHORS: [(f64, (u8, u8, u8)); 5] = [
    (100.0, (138, 160, 128)),
    (62.5, (166, 168, 122)),
    (37.5, (196, 176, 134)),
    (17.5, (200, 146, 100)),
    (5.0, (184, 106, 90)),
];

/// Band index by percent left: 75+, 50 to 75, 25 to 50, 10 to 25, under 10.
pub fn band(left: f64) -> usize {
    match left {
        l if l >= 75.0 => 0,
        l if l >= 50.0 => 1,
        l if l >= 25.0 => 2,
        l if l >= 10.0 => 3,
        _ => 4,
    }
}

fn gradient(left: f64) -> (u8, u8, u8) {
    let l = left.clamp(0.0, 100.0);
    if l >= ANCHORS[0].0 {
        return ANCHORS[0].1;
    }
    for w in ANCHORS.windows(2) {
        let ((hi, a), (lo, b)) = (w[0], w[1]);
        if l >= lo {
            let t = (l - lo) / (hi - lo);
            let mix = |x: u8, y: u8| (y as f64 + (x as f64 - y as f64) * t).round() as u8;
            return (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2));
        }
    }
    ANCHORS[4].1
}

/// Nearest xterm 256 color for an RGB value (the 6x6x6 cube or the grays).
pub fn to_256(r: u8, g: u8, b: u8) -> u8 {
    let level = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as u16 - 35) / 40) as u8
        }
    };
    let (cr, cg, cb) = (level(r), level(g), level(b));
    let cube = 16 + 36 * cr + 6 * cg + cb;
    let val = |c: u8| if c == 0 { 0 } else { 55 + 40 * c as i32 };
    let dc =
        (val(cr) - r as i32).pow(2) + (val(cg) - g as i32).pow(2) + (val(cb) - b as i32).pow(2);
    let avg = (r as i32 + g as i32 + b as i32) / 3;
    let gi = ((avg - 8).max(0) / 10).min(23);
    let gv = 8 + 10 * gi;
    let dg = (gv - r as i32).pow(2) + (gv - g as i32).pow(2) + (gv - b as i32).pow(2);
    if dg < dc {
        232 + gi as u8
    } else {
        cube
    }
}

pub fn rgb(c: (u8, u8, u8)) -> Color {
    if TRUECOLOR.load(std::sync::atomic::Ordering::Relaxed) {
        Color::Rgb(c.0, c.1, c.2)
    } else {
        Color::Indexed(to_256(c.0, c.1, c.2))
    }
}

/// Color for the percentage LEFT in a window, by `usage_colors`: a smooth
/// gradient from sage through sand to clay, five flat bands, or mono.
pub fn remaining(left: f64) -> Color {
    match USAGE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => rgb(ANCHORS[band(left)].1),
        2 => {
            if left < 10.0 {
                FG
            } else {
                DIM
            }
        }
        _ => rgb(gradient(left)),
    }
}

/// `remaining` plus bold under 10% left.
pub fn remaining_style(left: f64) -> ratatui::style::Style {
    let s = ratatui::style::Style::default().fg(remaining(left));
    if left < 10.0 {
        s.add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        s
    }
}

/// A marker for nearly empty windows (under 5% left).
pub fn low_marker(left: f64) -> &'static str {
    if left < 5.0 {
        " ▼"
    } else {
        ""
    }
}

/// Tests that switch the usage color mode take turns.
#[cfg(test)]
pub static MODE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_256() {
        assert_eq!(to_256(0, 0, 0), 16);
        assert_eq!(to_256(255, 255, 255), 231);
        assert_eq!(to_256(255, 0, 0), 196);
        assert_eq!(to_256(128, 128, 128), 244);
        // The usage anchors land on distinct palette entries.
        let mut v: Vec<u8> = ANCHORS.iter().map(|(_, c)| to_256(c.0, c.1, c.2)).collect();
        v.dedup();
        assert_eq!(v.len(), 5, "{v:?}");
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("Sage"), SAGE);
        assert_eq!(parse_color("#102030"), Color::Rgb(16, 32, 48));
        assert_eq!(parse_color("nonsense"), STONE);
        let _g = MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_usage_colors("bands");
        assert_eq!(remaining(80.0), Color::Rgb(138, 160, 128));
        assert_eq!(remaining(60.0), Color::Rgb(166, 168, 122));
        assert_eq!(remaining(30.0), Color::Rgb(196, 176, 134));
        assert_eq!(remaining(15.0), Color::Rgb(200, 146, 100));
        assert_eq!(remaining(4.0), Color::Rgb(184, 106, 90));
        assert_eq!(band(75.0), 0);
        assert_eq!(band(74.9), 1);
        assert_eq!(band(50.0), 1);
        assert_eq!(band(25.0), 2);
        assert_eq!(band(10.0), 3);
        assert_eq!(band(9.9), 4);
        set_usage_colors("gradient");
        // Anchors are hit exactly and the gradient moves smoothly between.
        assert_eq!(remaining(100.0), Color::Rgb(138, 160, 128));
        assert_eq!(remaining(37.5), Color::Rgb(196, 176, 134));
        assert_eq!(remaining(0.0), Color::Rgb(184, 106, 90));
        let Color::Rgb(r50, _, _) = remaining(50.0) else {
            panic!()
        };
        assert!(r50 > 166 && r50 < 196, "{r50}");
        set_usage_colors("mono");
        assert_eq!(remaining(80.0), DIM);
        assert_eq!(remaining(5.0), FG);
        set_usage_colors("gradient");
        assert!(remaining_style(9.0)
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD));
        assert!(!remaining_style(11.0)
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD));
        assert_eq!(low_marker(4.0), " ▼");
        assert_eq!(low_marker(6.0), "");
    }
}
