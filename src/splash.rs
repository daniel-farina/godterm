//! The start up splash: a glass crystal assembles, a beam of light
//! passes through it and fans out into the account colors, then the
//! wordmark, the tagline and the version appear. About 3.6 s, then it
//! holds (the crystal turning slowly).
//!
//! Shown once on the first launch, before Setup, and once after an update
//! ("updated to vX"); it closes by itself after a short hold, and any key
//! skips it. Never in tests, under the test guard, with GODTERM_NO_SPLASH,
//! in demo mode, without a terminal, or when the config says
//! `splash = "never"` or `setup_dont_show = true` (smoke runs).
//!
//! Drawn through ratatui, so only the cells that change are sent. The
//! scene is half block pixels (two per cell, supersampled), or whole cells
//! in Terminal.app, whose block glyphs do not split a cell evenly. The
//! wordmark is always whole cells in solid colors: crisp anywhere. Calm
//! blues and slate, no glow. NO_COLOR gets a plain drawing; `splash_motion`
//! = "off" (or "auto" with Reduce Motion on) a still frame.
//!
//! `godterm splash` shows it any time (`--preview`: r replays, q quits;
//! `--at <secs>` freezes one moment).

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

use crate::config::Config;
use crate::theme;

/// When the intro is over and the hold begins.
pub const END: f32 = 3.6;
/// At start up the splash closes by itself this long after the intro.
const HOLD_AT_START: f32 = 1.4;
const FRAME: Duration = Duration::from_millis(33);
/// Turns the start up splash off (smoke runs, scripts).
pub const NO_SPLASH_ENV: &str = "GODTERM_NO_SPLASH";

const TAGLINE: &str = "Every Claude and Grok session, one terminal. Say hey god.";
const TAGLINE_SHORT: &str = "Claude and Grok, one terminal. Say hey god.";
const HINT: &str = "press any key";
const PREVIEW_HINT: &str = "preview   r replay   q quit";

type Rgb = [f32; 3];

// Background: deep navy at the crystal, near black at the edges.
const BG_IN: Rgb = [17.0, 27.0, 47.0];
const BG_OUT: Rgb = [6.0, 8.0, 14.0];
// Glass.
const DEEP: Rgb = [26.0, 50.0, 98.0];
const MID: Rgb = [92.0, 134.0, 198.0];
const ICE: Rgb = [214.0, 226.0, 244.0];
const EDGE_FRONT: Rgb = [200.0, 214.0, 238.0];
const EDGE_BACK: Rgb = [96.0, 126.0, 178.0];
const BEAM: Rgb = [232.0, 235.0, 240.0];
// The fan out of the crystal: the account palette (theme.rs), top to bottom.
const RAYS: [Rgb; 6] = [
    [138.0, 140.0, 174.0], // dusk
    [128.0, 148.0, 170.0], // slate
    [122.0, 156.0, 152.0], // teal
    [138.0, 160.0, 128.0], // sage
    [196.0, 176.0, 134.0], // sand
    [180.0, 140.0, 146.0], // rose
];
// Wordmark: "God" in pale ice, "Term" in a soft blue.
const WM_GOD: Rgb = [226.0, 233.0, 246.0];
const WM_TERM: Rgb = [132.0, 164.0, 214.0];
const TEXT: Rgb = [164.0, 178.0, 200.0];
const TEXT_HI: Rgb = [214.0, 226.0, 244.0];
const VERSION: Rgb = [98.0, 112.0, 136.0];
const HINT_C: Rgb = [118.0, 130.0, 150.0];

#[derive(Default, Clone, Copy)]
struct Opts {
    preview: bool,
    at: Option<f32>,
    /// A still frame instead of the animation.
    still: bool,
    /// Close by itself this many seconds in (start up).
    close_at: Option<f32>,
    /// The version line reads "updated to vX".
    updated: bool,
}

/// How a frame is drawn.
#[derive(Default, Clone, Copy)]
struct Look {
    mono: bool,
    preview: bool,
    updated: bool,
    /// Whole cells instead of half blocks for the scene.
    whole: bool,
}

/// Whole cells for the scene: Terminal.app's block glyphs do not split a
/// cell evenly. GODTERM_SPLASH_CELLS=1 or 0 forces it.
fn whole_cells() -> bool {
    match std::env::var("GODTERM_SPLASH_CELLS").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal"),
    }
}

/// `godterm splash [--preview] [--at <secs>]`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let at = args
        .iter()
        .position(|a| a == "--at")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok());
    // `--dump <file> <cols>x<rows>`: write one frame's cells (for stills).
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let path = args.get(i + 1).cloned().unwrap_or_default();
        let (w, h) = args
            .get(i + 2)
            .and_then(|s| s.split_once('x'))
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .unwrap_or((120, 36));
        return dump(&path, w, h, at.unwrap_or(END), no_color());
    }
    // The motion setting, when there is a config (never writes one).
    let motion = if Config::path().exists() {
        Config::load_or_init()
            .map(|c| c.splash_motion)
            .unwrap_or_default()
    } else {
        String::new()
    };
    play(Opts {
        preview: args.iter().any(|a| a == "--preview"),
        at,
        still: !animate(&motion),
        updated: args.iter().any(|a| a == "--updated"),
        ..Default::default()
    })?;
    Ok(())
}

/// One frame as text: a line per cell, "x y fg bg bold symbol".
fn dump(path: &str, w: u16, h: u16, t: f32, mono: bool) -> anyhow::Result<()> {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    let look = Look {
        mono,
        whole: whole_cells(),
        ..Default::default()
    };
    draw(&mut buf, area, t, look);
    let hex = |c: Color| match c {
        Color::Rgb(r, g, b) => format!("{r:02x}{g:02x}{b:02x}"),
        _ => "-".into(),
    };
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            let c = &buf[(x, y)];
            let bold = c.modifier.contains(Modifier::BOLD) as u8;
            out += &format!(
                "{x} {y} {} {} {bold} {}\n",
                hex(c.fg),
                hex(c.bg),
                c.symbol()
            );
        }
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// Why the splash plays at start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Startup {
    /// Never seen on this home.
    First,
    /// Last seen with another version.
    Updated,
}

/// Remembers the version the splash was last shown for.
fn seen_path() -> PathBuf {
    crate::config::app_home().join("splash_seen")
}

/// Whether the splash plays at start: `seen` is the version it was last
/// shown for, `quiet` is a test, smoke or scripted run.
pub fn decide(
    splash: &str,
    setup_dont_show: bool,
    seen: Option<&str>,
    version: &str,
    tty: bool,
    quiet: bool,
) -> Option<Startup> {
    if quiet || !tty || setup_dont_show || splash == "never" {
        return None;
    }
    match seen.map(str::trim) {
        None | Some("") => Some(Startup::First),
        Some(v) if v != version => Some(Startup::Updated),
        Some(_) => None,
    }
}

/// A test, the test guard's children, smoke or demo runs: never a splash.
fn quiet() -> bool {
    cfg!(test)
        || std::env::var_os(crate::test_guard::ENV).is_some()
        || std::env::var_os(NO_SPLASH_ENV).is_some_and(|v| !v.is_empty() && v != "0")
        || crate::demo::active()
}

/// At start up, before Setup and the TUI: plays the splash when it is due
/// (see `decide`), then remembers this version. Closes by itself shortly
/// after the intro, so it never holds up more than its own few seconds.
pub fn at_start(cfg: &Config) {
    let version = env!("CARGO_PKG_VERSION");
    let tty = io::stdin().is_terminal() && io::stdout().is_terminal();
    let seen = std::fs::read_to_string(seen_path()).ok();
    let Some(why) = decide(
        &cfg.splash,
        cfg.setup_dont_show,
        seen.as_deref(),
        version,
        tty,
        quiet(),
    ) else {
        return;
    };
    let still = !animate(&cfg.splash_motion);
    let r = play(Opts {
        still,
        close_at: Some(if still { 2.0 } else { END + HOLD_AT_START }),
        updated: why == Startup::Updated,
        ..Default::default()
    });
    if let Err(e) = r {
        crate::log::info(&format!("splash: {e}"));
    }
    let _ = std::fs::create_dir_all(crate::config::app_home());
    let _ = std::fs::write(seen_path(), version);
}

fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// `splash_motion`: "off" is still, "auto" follows Reduce Motion, anything
/// else ("on", the default) animates. GODTERM_REDUCED_MOTION=1 forces still.
fn animate(motion: &str) -> bool {
    if let Some(v) = std::env::var_os("GODTERM_REDUCED_MOTION") {
        if !v.is_empty() && v != "0" {
            return false;
        }
    }
    match motion {
        "off" => false,
        "auto" => !reduced_motion(),
        _ => true,
    }
}

/// macOS Reduce Motion (Accessibility > Display).
fn reduced_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = std::process::Command::new("defaults")
            .args(["read", "com.apple.universalaccess", "reduceMotion"])
            .stderr(std::process::Stdio::null())
            .output()
        {
            return String::from_utf8_lossy(&o.stdout).trim() == "1";
        }
    }
    false
}

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = execute!(out, cursor::Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let _ = out.flush();
    }
}

fn play(o: Opts) -> io::Result<()> {
    theme::detect_truecolor();
    let look = Look {
        mono: no_color(),
        preview: o.preview,
        updated: o.updated,
        whole: whole_cells(),
    };
    let still = look.mono || o.at.is_some() || o.still;
    enable_raw_mode()?;
    let _restore = Restore;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    term.clear()?;
    let shown = Instant::now();
    let mut start = Instant::now();
    let mut next = Instant::now();
    loop {
        let t = match o.at {
            Some(a) => a,
            None if still => END,
            None => start.elapsed().as_secs_f32(),
        };
        if o.close_at
            .is_some_and(|c| shown.elapsed().as_secs_f32() >= c)
        {
            break;
        }
        term.draw(|f| {
            let area = f.area();
            draw(f.buffer_mut(), area, t, look);
        })?;
        next += FRAME;
        let now = Instant::now();
        if next < now {
            next = now;
        }
        let wait = if still {
            Duration::from_millis(100)
        } else {
            next - now
        };
        if !event::poll(wait)? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                let ctrl_c =
                    k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
                if !o.preview || ctrl_c || matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                    break;
                }
                if k.code == KeyCode::Char('r') {
                    start = Instant::now();
                    next = start;
                    term.clear()?;
                } else if t < END {
                    // Any other key skips to the held frame.
                    start = Instant::now() - Duration::from_secs_f32(END);
                }
            }
            Event::Resize(..) => term.clear()?,
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- timing

fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}
/// 0 before `a`, 1 after `b`, linear between.
fn seg(t: f32, a: f32, b: f32) -> f32 {
    clamp01((t - a) / (b - a))
}
fn ease_out(x: f32) -> f32 {
    let x = clamp01(x);
    1.0 - (1.0 - x).powi(3)
}
fn ease_in_out(x: f32) -> f32 {
    let x = clamp01(x);
    x * x * (3.0 - 2.0 * x)
}
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}
fn scale(a: Rgb, k: f32) -> Rgb {
    [a[0] * k, a[1] * k, a[2] * k]
}

// ---------------------------------------------------------------- layout

#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    cols: u16,
    rows: u16,
    /// Crystal center and radius, in half block pixels.
    crystal: Option<(f32, f32, f32)>,
    /// Wordmark left column, top pixel row and font (an index into FONTS).
    wordmark: Option<(usize, usize, usize)>,
    /// A plain text wordmark row when there is no room for the drawn one.
    wm_text: Option<u16>,
    tagline: Option<(u16, &'static str)>,
    version: Option<u16>,
    hint: Option<u16>,
}

/// Vertical stretch of the crystal.
const YS: f32 = 1.12;

fn layout(cols: u16, rows: u16) -> Layout {
    let (w, r) = (cols as i32, rows as i32);
    let tagline = [TAGLINE, TAGLINE_SHORT]
        .into_iter()
        .find(|s| s.chars().count() as i32 + 4 <= w);
    let hint = (r >= 10).then(|| (r - 2) as u16);
    // Rows the text needs: tagline, a gap, the version.
    let text_rows = 3;
    let bottom = if hint.is_some() { 3 } else { 0 };
    // The drawn wordmark: the largest hand drawn size that fits.
    let font = FONTS
        .iter()
        .position(|f| w >= f.width() as i32 + 6 && r >= f.min_rows);
    let wm_rows = font.map_or(1, |f| {
        let f = &FONTS[f];
        f.rows() as i32
    });
    let gap = if r >= 30 { 2 } else { 1 };
    let fixed = 1 + gap + wm_rows + gap + text_rows + bottom;
    let avail = r - fixed;
    let mut radius = (avail as f32 / YS - 0.5).min(w as f32 / 5.0).min(30.0);
    if radius < 4.0 {
        radius = 0.0;
    }
    let crystal_rows = if radius > 0.0 {
        (radius * YS).ceil() as i32 + 1
    } else {
        0
    };
    let used = crystal_rows + (if radius > 0.0 { gap } else { 0 }) + wm_rows + gap + text_rows;
    // Center the stack in the space above the hint.
    let space = r - bottom;
    let mut y = ((space - used) / 2).max(0);
    let crystal = (radius > 0.0).then(|| {
        let cy = (y as f32 + crystal_rows as f32 / 2.0) * 2.0;
        y += crystal_rows + gap;
        (w as f32 / 2.0, cy, radius)
    });
    let (wordmark, wm_text) = if let Some(f) = font {
        let left = (w as usize - FONTS[f].width()) / 2;
        let wm = (left, y as usize, f);
        y += wm_rows + gap;
        (Some(wm), None)
    } else {
        let row = (y < r).then_some(y as u16);
        y += 2;
        (None, row)
    };
    let tag = tagline.filter(|_| y < r && hint.is_none_or(|h| (y as u16) < h));
    let tag = tag.map(|s| (y as u16, s));
    y += 2;
    let version = (y < r && hint.is_none_or(|h| (y as u16) < h)).then_some(y as u16);
    Layout {
        cols,
        rows,
        crystal,
        wordmark,
        wm_text,
        tagline: tag,
        version,
        hint,
    }
}

// ---------------------------------------------------------------- canvas

/// A supersampled RGB canvas over half block pixels.
struct Canvas {
    w: usize,
    h: usize,
    ss: usize,
    px: Vec<Rgb>,
}

/// Coverage at the supersampled resolution, kept as the maximum so shared
/// strokes do not double up.
struct Mask {
    v: Vec<f32>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Canvas {
        let ss = if w * h > 60_000 { 2 } else { 3 };
        Canvas {
            w,
            h,
            ss,
            px: vec![[0.0; 3]; w * ss * h * ss],
        }
    }
    fn sw(&self) -> usize {
        self.w * self.ss
    }
    fn sh(&self) -> usize {
        self.h * self.ss
    }
    fn mask(&self) -> Mask {
        Mask {
            v: vec![0.0; self.px.len()],
        }
    }

    fn fill(&mut self, f: impl Fn(f32, f32) -> Rgb) {
        let (sw, s) = (self.sw(), self.ss as f32);
        for (i, p) in self.px.iter_mut().enumerate() {
            let x = ((i % sw) as f32 + 0.5) / s;
            let y = ((i / sw) as f32 + 0.5) / s;
            *p = f(x, y);
        }
    }

    fn blend(&mut self, i: usize, c: Rgb, a: f32) {
        let p = &mut self.px[i];
        for k in 0..3 {
            p[k] += (c[k] - p[k]) * a;
        }
    }

    /// Fills a triangle given in pixels.
    fn tri(&mut self, p: [(f32, f32); 3], c: Rgb, a: f32) {
        if a <= 0.0 {
            return;
        }
        let s = self.ss as f32;
        let q = p.map(|(x, y)| (x * s, y * s));
        let area = (q[1].0 - q[0].0) * (q[2].1 - q[0].1) - (q[1].1 - q[0].1) * (q[2].0 - q[0].0);
        if area.abs() < 1e-3 {
            return;
        }
        let (x0, x1, y0, y1) = self.bbox(&q, 0.0);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let inside = (0..3).all(|k| {
                    let (a0, a1) = (q[k], q[(k + 1) % 3]);
                    let e = (a1.0 - a0.0) * (py - a0.1) - (a1.1 - a0.1) * (px - a0.0);
                    e * area.signum() >= 0.0
                });
                if inside {
                    let i = y * self.sw() + x;
                    self.blend(i, c, a);
                }
            }
        }
    }

    fn bbox(&self, q: &[(f32, f32)], pad: f32) -> (usize, usize, usize, usize) {
        let minx = q.iter().map(|p| p.0).fold(f32::MAX, f32::min) - pad;
        let maxx = q.iter().map(|p| p.0).fold(f32::MIN, f32::max) + pad;
        let miny = q.iter().map(|p| p.1).fold(f32::MAX, f32::min) - pad;
        let maxy = q.iter().map(|p| p.1).fold(f32::MIN, f32::max) + pad;
        let cx = |v: f32, m: usize| v.floor().clamp(0.0, m as f32) as usize;
        (
            cx(minx, self.sw()),
            cx(maxx + 1.0, self.sw()),
            cx(miny, self.sh()),
            cx(maxy + 1.0, self.sh()),
        )
    }

    /// Calls `f(index, coverage, t along the segment)` for a round capped
    /// stroke `width` pixels wide from `a` to `b`.
    fn stroke(&self, a: (f32, f32), b: (f32, f32), width: f32, mut f: impl FnMut(usize, f32, f32)) {
        let s = self.ss as f32;
        let (a, b) = ((a.0 * s, a.1 * s), (b.0 * s, b.1 * s));
        let hw = width * s / 2.0;
        let (x0, x1, y0, y1) = self.bbox(&[a, b], hw + 1.0);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = clamp01(((px - a.0) * dx + (py - a.1) * dy) / len2);
                let (ex, ey) = (a.0 + dx * t - px, a.1 + dy * t - py);
                let d = (ex * ex + ey * ey).sqrt();
                let cov = clamp01(hw + 0.5 - d);
                if cov > 0.0 {
                    f(y * self.sw() + x, cov, t);
                }
            }
        }
    }

    /// A stroke blended straight onto the canvas, its alpha by position.
    fn line(
        &mut self,
        a: (f32, f32),
        b: (f32, f32),
        width: f32,
        c: Rgb,
        alpha: impl Fn(f32) -> f32,
    ) {
        let mut hits = vec![];
        self.stroke(a, b, width, |i, cov, t| hits.push((i, cov * alpha(t))));
        for (i, a) in hits {
            self.blend(i, c, clamp01(a));
        }
    }

    fn mask_line(&self, m: &mut Mask, a: (f32, f32), b: (f32, f32), width: f32, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        self.stroke(a, b, width, |i, cov, _| {
            let v = cov * alpha;
            if v > m.v[i] {
                m.v[i] = v;
            }
        });
    }

    /// Blends a mask on, coloured and weighted by position.
    fn composite(&mut self, m: &Mask, f: impl Fn(f32, f32) -> (Rgb, f32)) {
        let (sw, s) = (self.sw(), self.ss as f32);
        for i in 0..self.px.len() {
            let v = m.v[i];
            if v <= 0.0 {
                continue;
            }
            let x = ((i % sw) as f32 + 0.5) / s;
            let y = ((i / sw) as f32 + 0.5) / s;
            let (c, a) = f(x, y);
            self.blend(i, c, clamp01(v * a));
        }
    }

    /// Box filtered down to one value per half block pixel.
    fn resolve(&self) -> Vec<Rgb> {
        let (s, sw) = (self.ss, self.sw());
        let n = (s * s) as f32;
        let mut out = vec![[0.0; 3]; self.w * self.h];
        for y in 0..self.h {
            for x in 0..self.w {
                let mut acc = [0.0; 3];
                for yy in 0..s {
                    let row = (y * s + yy) * sw + x * s;
                    for p in &self.px[row..row + s] {
                        for k in 0..3 {
                            acc[k] += p[k];
                        }
                    }
                }
                out[y * self.w + x] = scale(acc, 1.0 / n);
            }
        }
        out
    }
}

// ---------------------------------------------------------------- crystal

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-6);
    [a[0] / l, a[1] / l, a[2] / l]
}
fn rot_x(v: V3, a: f32) -> V3 {
    let (s, c) = a.sin_cos();
    [v[0], v[1] * c - v[2] * s, v[1] * s + v[2] * c]
}
fn rot_y(v: V3, a: f32) -> V3 {
    let (s, c) = a.sin_cos();
    [v[0] * c + v[2] * s, v[1], -v[0] * s + v[2] * c]
}
fn rot_z(v: V3, a: f32) -> V3 {
    let (s, c) = a.sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c, v[2]]
}

/// An icosahedron standing on a vertex: 12 points, 20 faces.
fn icosahedron() -> (Vec<V3>, Vec<[usize; 3]>) {
    let p = (1.0 + 5f32.sqrt()) / 2.0;
    let mut v: Vec<V3> = vec![];
    for &a in &[-1.0, 1.0] {
        for &b in &[-p, p] {
            v.push([0.0, a, b]);
            v.push([a, b, 0.0]);
            v.push([b, 0.0, a]);
        }
    }
    // Tip a vertex to the top: (0, 1, p) rotated about x by atan(p).
    let tip = (1.0f32 / p).atan();
    let v: Vec<V3> = v.into_iter().map(|q| norm(rot_x(q, -tip))).collect();
    let edge = 2.0 / (1.0 + p * p).sqrt();
    let near = |a: V3, b: V3| (dot(sub(a, b), sub(a, b)).sqrt() - edge).abs() < 0.05;
    let mut f = vec![];
    for i in 0..12 {
        for j in i + 1..12 {
            for k in j + 1..12 {
                if near(v[i], v[j]) && near(v[j], v[k]) && near(v[i], v[k]) {
                    f.push([i, j, k]);
                }
            }
        }
    }
    (v, f)
}

struct Scene {
    px: Vec<Rgb>,
    w: usize,
}

fn render(l: &Layout, t: f32) -> Scene {
    let (w, h) = (l.cols as usize, l.rows as usize * 2);
    let mut cv = Canvas::new(w, h);
    let (ccx, ccy, cr) = l.crystal.unwrap_or((w as f32 / 2.0, h as f32 / 3.0, 0.0));

    // Background, fading up from black.
    let fade = ease_out(seg(t, 0.0, 0.6));
    let (fw, fh) = (w as f32, h as f32);
    cv.fill(|x, y| {
        let dx = (x - ccx) / fw;
        let dy = (y - ccy) / fh * 0.9;
        let d = (dx * dx + dy * dy).sqrt();
        scale(mix(BG_IN, BG_OUT, ease_in_out(d / 0.62)), fade)
    });

    if cr > 0.0 {
        crystal(&mut cv, l, t, (ccx, ccy, cr));
    }
    Scene {
        px: cv.resolve(),
        w,
    }
}

fn crystal(cv: &mut Canvas, l: &Layout, t: f32, (cx, cy, r): (f32, f32, f32)) {
    let w = cv.w as f32;
    let held = (t - END).max(0.0);
    let energy = ease_out(seg(t, 0.9, 1.5));

    // The beam in from the left, and the fan out to the right.
    let p_in = (cx - 0.80 * r, cy - 0.10 * r);
    let p_out = (cx + 0.80 * r, cy + 0.10 * r);
    let start = (0.0, cy - 0.10 * r - (cx * 0.10).min(0.45 * r));
    let grow = ease_out(seg(t, 0.15, 0.95));
    if grow > 0.0 {
        let head = (
            start.0 + (p_in.0 - start.0) * grow,
            start.1 + (p_in.1 - start.1) * grow,
        );
        cv.line(start, head, 0.9, BEAM, |s| 0.15 + 0.8 * s * grow);
    }
    let fan = ease_out(seg(t, 1.25, 2.15));
    if fan > 0.0 {
        // Spread so the lowest ray ends above the wordmark.
        let floor = l
            .wordmark
            .map(|w| w.1 as f32 * 2.0 - 3.0)
            .unwrap_or(cv.h as f32 - 2.0);
        let run = (w - p_out.0).max(1.0);
        let lo = ((floor - p_out.1) / run).clamp(0.02, 0.21);
        for (i, c) in RAYS.iter().enumerate() {
            let k = i as f32 / (RAYS.len() - 1) as f32;
            let slope = -0.20 + (lo + 0.20) * k;
            let end = (w + 1.0, p_out.1 + slope * (w + 1.0 - p_out.0));
            let head = (
                p_out.0 + (end.0 - p_out.0) * fan,
                p_out.1 + (end.1 - p_out.1) * fan,
            );
            // In the hold, a soft pulse runs out along each ray now and then.
            let pulse_at = ((held * 0.32 + k * 0.17) % 1.6) - 0.2;
            let pulse = held.min(1.0);
            cv.line(p_out, head, 1.05, *c, |s| {
                let s = s * fan;
                let base = 0.9 - 0.5 * s;
                let p = (-((s - pulse_at) / 0.07).powi(2)).exp() * 0.35 * pulse;
                base + p
            });
        }
    }

    // The crystal itself.
    let (verts, faces) = icosahedron();
    let spin = 0.6 + 2.6 * ease_out(seg(t, 0.0, 2.4)) + 0.21 * t;
    let tilt = 0.30 + 0.03 * (t * 0.5).sin() * seg(t, END, END + 2.0);
    let pose = |v: V3| rot_z(rot_x(rot_y(v, spin), tilt), 0.10);
    let rv: Vec<V3> = verts
        .iter()
        .map(|&v| pose([v[0], v[1] * YS, v[2]]))
        .collect();
    let light = norm([-0.45, 0.65, 0.62]);
    let half = norm([light[0], light[1], light[2] + 1.0]);
    let grow_in = ease_out(seg(t, 0.25, 1.1));
    let size = r * (0.82 + 0.18 * grow_in);
    let proj = |v: V3| {
        let p = 3.4 / (3.4 - v[2]);
        (cx + v[0] * size * p, cy - v[1] * size * p)
    };

    struct F {
        pts: [(f32, f32); 3],
        n: V3,
        z: f32,
        a: f32,
    }
    let mut fs: Vec<F> = faces
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let [a, b, c] = f.map(|k| rv[k]);
            let mut n = norm(cross(sub(b, a), sub(c, a)));
            let mid = scale(add3(add3(a, b), c), 1.0 / 3.0);
            if dot(n, mid) < 0.0 {
                n = scale(n, -1.0);
            }
            // Each face flies in from outside along its own direction.
            let delay = 0.30 + 0.75 * (((i * 7 + 3) % 20) as f32 / 20.0);
            let p = ease_out(seg(t, delay, delay + 0.75));
            let off = scale(norm(mid), (1.0 - p) * 1.6);
            let pts = [a, b, c].map(|v| proj(add3(v, off)));
            F {
                pts,
                n,
                z: mid[2],
                a: p,
            }
        })
        .collect();
    fs.sort_by(|a, b| a.z.total_cmp(&b.z));

    let glow = 0.62 + 0.38 * energy;
    let edge_w = (r / 20.0).clamp(0.6, 1.0);
    let mut back_edges = cv.mask();
    let mut front_edges = cv.mask();
    for f in fs.iter().filter(|f| f.n[2] <= 0.0) {
        cv.tri(f.pts, scale(DEEP, 0.75 * glow), 0.30 * f.a);
        for k in 0..3 {
            cv.mask_line(
                &mut back_edges,
                f.pts[k],
                f.pts[(k + 1) % 3],
                edge_w * 0.8,
                f.a,
            );
        }
    }
    cv.composite(&back_edges, |_, _| (EDGE_BACK, 0.45 * glow));

    // The light inside.
    if energy > 0.0 {
        cv.line(p_in, p_out, 0.9, ICE, |_| 0.35 * energy);
    }
    for f in fs.iter().filter(|f| f.n[2] > 0.0) {
        let d = dot(f.n, light).max(0.0);
        let spec = dot(f.n, half).max(0.0).powi(18);
        let base = mix(DEEP, MID, 0.25 + 0.75 * d);
        let c = mix(scale(base, glow), ICE, (spec * 0.6 + 0.06) * glow);
        cv.tri(f.pts, c, (0.40 + 0.30 * d) * f.a);
        for k in 0..3 {
            cv.mask_line(&mut front_edges, f.pts[k], f.pts[(k + 1) % 3], edge_w, f.a);
        }
    }
    cv.composite(&front_edges, |_, _| (EDGE_FRONT, 0.7 + 0.3 * glow));
}

fn add3(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

// ---------------------------------------------------------------- wordmark

/// A hand drawn font for "GodTerm" at cell resolution: each `#` is one
/// whole cell painted with a background color, so the letters do not
/// depend on how the terminal's font draws block glyphs, and nothing is
/// blended. A cell is twice as tall as wide, so stems are two columns
/// and bars one row: the same thickness on screen. Lowercase glyphs give
/// only their x-height rows and sit on the baseline.
struct Font {
    cap: usize,
    gap: usize,
    /// Terminal rows the layout needs before choosing this size.
    min_rows: i32,
    shadow: bool,
    glyphs: [&'static [&'static str]; 7],
}

impl Font {
    fn width(&self) -> usize {
        let w: usize = self.glyphs.iter().map(|g| g[0].len()).sum();
        w + self.gap * 6 + self.shadow as usize
    }

    fn rows(&self) -> usize {
        self.cap + self.shadow as usize
    }

    /// Calls `f(x, y, is_term)` for every lit cell, relative to the top left.
    fn each(&self, mut f: impl FnMut(usize, usize, bool)) {
        let mut x0 = 0;
        for (i, g) in self.glyphs.iter().enumerate() {
            let top = self.cap - g.len();
            for (y, row) in g.iter().enumerate() {
                for (x, b) in row.bytes().enumerate() {
                    if b == b'#' {
                        f(x0 + x, top + y, i >= 3);
                    }
                }
            }
            x0 += g[0].len() + self.gap;
        }
    }
}

/// Large (9 rows, stepped curves), medium (7 rows, cut corners) and
/// compact (6 rows, square, for 80x24), largest first.
const FONTS: [Font; 3] = [
    Font {
        cap: 9,
        gap: 3,
        min_rows: 40,
        shadow: true,
        glyphs: [
            &[
                "..########..",
                ".##......##.",
                "##..........",
                "##..........",
                "##....######",
                "##........##",
                "##........##",
                ".##......##.",
                "..########..",
            ],
            &[
                "..########..",
                ".##......##.",
                "##........##",
                "##........##",
                "##........##",
                ".##......##.",
                "..########..",
            ],
            &[
                "..........##",
                "..........##",
                "..##########",
                ".##.......##",
                "##........##",
                "##........##",
                "##........##",
                ".##.......##",
                "..##########",
            ],
            &[
                "############",
                ".....##.....",
                ".....##.....",
                ".....##.....",
                ".....##.....",
                ".....##.....",
                ".....##.....",
                ".....##.....",
                ".....##.....",
            ],
            &[
                "..########..",
                ".##......##.",
                "##........##",
                "############",
                "##..........",
                ".##.........",
                "..##########",
            ],
            &[
                "##..######",
                "##.##.....",
                "####......",
                "##........",
                "##........",
                "##........",
                "##........",
            ],
            &[
                "##..####...####..",
                "##.##..##.##..##.",
                "####....###....##",
                "##......##.....##",
                "##......##.....##",
                "##......##.....##",
                "##......##.....##",
            ],
        ],
    },
    Font {
        cap: 7,
        gap: 3,
        min_rows: 30,
        shadow: true,
        glyphs: [
            &[
                ".########.",
                "##......##",
                "##........",
                "##...#####",
                "##......##",
                "##......##",
                ".########.",
            ],
            &[
                ".########.",
                "##......##",
                "##......##",
                "##......##",
                ".########.",
            ],
            &[
                "........##",
                "........##",
                ".#########",
                "##......##",
                "##......##",
                "##......##",
                ".#########",
            ],
            &[
                "##########",
                "....##....",
                "....##....",
                "....##....",
                "....##....",
                "....##....",
                "....##....",
            ],
            &[
                ".########.",
                "##......##",
                "##########",
                "##........",
                ".#########",
            ],
            &[
                "##.######",
                "####.....",
                "##.......",
                "##.......",
                "##.......",
            ],
            &[
                "##.####..####.",
                "####..####..##",
                "##....##....##",
                "##....##....##",
                "##....##....##",
            ],
        ],
    },
    Font {
        cap: 6,
        gap: 2,
        min_rows: 14,
        shadow: false,
        glyphs: [
            &["######", "##....", "##..##", "##..##", "##..##", "######"],
            &["######", "##..##", "##..##", "##..##", "######"],
            &["....##", "######", "##..##", "##..##", "##..##", "######"],
            &["######", "..##..", "..##..", "..##..", "..##..", "..##.."],
            &["######", "##..##", "######", "##....", "######"],
            &["#####", "##...", "##...", "##...", "##..."],
            &[
                "##########",
                "##..##..##",
                "##..##..##",
                "##..##..##",
                "##..##..##",
            ],
        ],
    },
];

const WM_SHADOW: Rgb = [34.0, 45.0, 70.0];

/// Paints the wordmark into the cells: whole columns appear left to right,
/// solid colors only.
fn wordmark(
    cells: &mut [Option<Rgb>],
    cols: usize,
    rows: usize,
    t: f32,
    (left, top, f): (usize, usize, usize),
) {
    let font = &FONTS[f];
    let reveal = ease_in_out(seg(t, 1.8, 2.6));
    if reveal <= 0.0 {
        return;
    }
    let edge = left + ((font.width() + 1) as f32 * reveal).round() as usize;
    let mut put = |x: usize, y: usize, c: Rgb| {
        if x < cols && y < rows && x < edge {
            cells[y * cols + x] = Some(c);
        }
    };
    if font.shadow {
        font.each(|x, y, _| put(left + x + 1, top + y + 1, WM_SHADOW));
    }
    font.each(|x, y, term| {
        put(left + x, top + y, if term { WM_TERM } else { WM_GOD });
    });
}

// ---------------------------------------------------------------- output

fn version_line(updated: bool) -> String {
    let v = env!("CARGO_PKG_VERSION");
    if updated {
        format!("updated to v{v}")
    } else {
        format!("v{v}")
    }
}

fn to_color(c: Rgb) -> Color {
    let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    theme::rgb((q(c[0]), q(c[1]), q(c[2])))
}

fn draw(buf: &mut Buffer, area: Rect, t: f32, look: Look) {
    let Look {
        mono,
        preview,
        updated,
        whole,
    } = look;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let l = layout(area.width, area.height);
    if mono {
        draw_mono(buf, area, &l, preview, updated);
        return;
    }
    let sc = render(&l, t);
    let at = |x: u16, y: u16| -> (Rgb, Rgb) {
        let (x, y) = (x as usize, y as usize);
        (sc.px[2 * y * sc.w + x], sc.px[(2 * y + 1) * sc.w + x])
    };
    let (cols, rows) = (area.width as usize, area.height as usize);
    let mut wm = vec![None; cols * rows];
    if let Some(w) = l.wordmark {
        wordmark(&mut wm, cols, rows, t, w);
    }
    for y in 0..area.height {
        for x in 0..area.width {
            let (top, bot) = at(x, y);
            let (ct, cb) = (to_color(top), to_color(bot));
            if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                cell.reset();
                if let Some(c) = wm[y as usize * cols + x as usize] {
                    cell.set_symbol(" ").set_bg(to_color(c));
                } else if whole {
                    cell.set_symbol(" ").set_bg(to_color(mix(top, bot, 0.5)));
                } else if ct == cb {
                    cell.set_symbol(" ").set_bg(cb);
                } else {
                    cell.set_symbol("▀").set_fg(ct).set_bg(cb);
                }
            }
        }
    }
    // Text sits on the scene: each character blended over its cell.
    let mut text = |row: u16, s: &str, alpha: &dyn Fn(usize, char) -> (Rgb, f32), bold: bool| {
        let n = s.chars().count() as u16;
        if n > area.width {
            return;
        }
        let x0 = (area.width - n) / 2;
        for (i, ch) in s.chars().enumerate() {
            let x = x0 + i as u16;
            let (top, bot) = at(x, row);
            let bg = mix(top, bot, 0.5);
            let (c, a) = alpha(i, ch);
            if let Some(cell) = buf.cell_mut((area.x + x, area.y + row)) {
                cell.reset();
                cell.set_bg(to_color(bg));
                if a > 0.02 && ch != ' ' {
                    cell.set_char(ch).set_fg(to_color(mix(bg, c, clamp01(a))));
                    if bold {
                        cell.modifier.insert(Modifier::BOLD);
                    }
                } else {
                    cell.set_symbol(" ");
                }
            }
        }
    };
    if let Some(row) = l.wm_text {
        let a = ease_out(seg(t, 1.8, 2.5));
        text(
            row,
            "GodTerm",
            &|i, _| (if i < 3 { WM_GOD } else { WM_TERM }, a),
            true,
        );
    }
    if let Some((row, s)) = l.tagline {
        let n = s.chars().count() as f32;
        let p = seg(t, 2.45, 3.15);
        let hi = s.find("hey god").map(|b| s[..b].chars().count());
        text(
            row,
            s,
            &|i, _| {
                // Each character fades in a moment after the one before it.
                let a = clamp01((p * (n + 10.0) - i as f32) / 10.0);
                let c = match hi {
                    Some(h) if i >= h && i < h + 7 => TEXT_HI,
                    _ => TEXT,
                };
                (c, a)
            },
            false,
        );
    }
    if let Some(row) = l.version {
        let a = ease_out(seg(t, 2.9, 3.4));
        let v = version_line(updated);
        text(row, &v, &|_, _| (VERSION, a), false);
    }
    if let Some(row) = l.hint {
        let held = (t - END).max(0.0);
        let breathe = 0.82 + 0.18 * (held * 1.6).cos();
        let a = ease_out(seg(t, 3.2, 3.8)) * breathe;
        text(row, HINT, &|_, _| (HINT_C, a), false);
        if preview && area.height > row + 1 {
            text(row + 1, PREVIEW_HINT, &|_, _| (VERSION, 0.8), false);
        }
    }
}

/// NO_COLOR: the crystal as a braille line drawing, plain text below.
fn draw_mono(buf: &mut Buffer, area: Rect, l: &Layout, preview: bool, updated: bool) {
    let put = |buf: &mut Buffer, row: u16, s: &str, bold: bool| {
        let n = s.chars().count() as u16;
        if n > area.width || row >= area.height {
            return;
        }
        let x = area.x + (area.width - n) / 2;
        let st = if bold {
            ratatui::style::Style::default().add_modifier(Modifier::BOLD)
        } else {
            ratatui::style::Style::default()
        };
        buf.set_string(x, area.y + row, s, st);
    };
    if let Some((cx, cy, r)) = l.crystal {
        // Braille dots: 2 across, 4 down per cell; pixel y is half a cell.
        let (dw, dh) = (area.width as usize * 2, area.height as usize * 4);
        let mut dots = vec![false; dw * dh];
        let (verts, faces) = icosahedron();
        let pose = |v: V3| rot_z(rot_x(rot_y(v, 0.6 + 2.6 + 0.21 * END), 0.30), 0.10);
        let rv: Vec<V3> = verts
            .iter()
            .map(|&v| pose([v[0], v[1] * YS, v[2]]))
            .collect();
        let proj = |v: V3| {
            let p = 3.4 / (3.4 - v[2]);
            ((cx + v[0] * r * p) * 2.0, (cy - v[1] * r * p) * 2.0)
        };
        for f in &faces {
            for k in 0..3 {
                let (a, b) = (proj(rv[f[k]]), proj(rv[f[(k + 1) % 3]]));
                let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) * 1.5).ceil() as usize + 1;
                for s in 0..=steps {
                    let u = s as f32 / steps as f32;
                    let (x, y) = (a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u);
                    let (xi, yi) = (x as isize, y as isize);
                    if xi >= 0 && yi >= 0 && (xi as usize) < dw && (yi as usize) < dh {
                        dots[yi as usize * dw + xi as usize] = true;
                    }
                }
            }
        }
        const BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        for cyc in 0..area.height as usize {
            for cxc in 0..area.width as usize {
                let mut b = 0;
                for (dx, col) in BITS.iter().enumerate() {
                    for (dy, bit) in col.iter().enumerate() {
                        if dots[(cyc * 4 + dy) * dw + cxc * 2 + dx] {
                            b |= bit;
                        }
                    }
                }
                if b != 0 {
                    let ch = char::from_u32(0x2800 + b).unwrap_or(' ');
                    if let Some(cell) = buf.cell_mut((area.x + cxc as u16, area.y + cyc as u16)) {
                        cell.set_char(ch);
                    }
                }
            }
        }
    }
    if let Some((left, top, f)) = l.wordmark {
        // The same font in full blocks, in the terminal's own color.
        FONTS[f].each(|x, y, _| {
            if let Some(cell) =
                buf.cell_mut((area.x + (left + x) as u16, area.y + (top + y) as u16))
            {
                cell.set_char('█');
            }
        });
    } else if let Some(row) = l.wm_text {
        put(buf, row, "GodTerm", true);
    }
    if let Some((row, s)) = l.tagline {
        put(buf, row, s, false);
    }
    if let Some(row) = l.version {
        put(buf, row, &version_line(updated), false);
    }
    if let Some(row) = l.hint {
        put(buf, row, HINT, false);
        if preview {
            put(buf, row + 1, PREVIEW_HINT, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_every_size() {
        for (w, h) in [
            (20, 6),
            (40, 12),
            (80, 24),
            (100, 30),
            (120, 36),
            (200, 60),
            (300, 90),
        ] {
            let l = layout(w, h);
            for row in [l.version, l.hint, l.wm_text, l.tagline.map(|t| t.0)]
                .into_iter()
                .flatten()
            {
                assert!(row < h, "{w}x{h}: {l:?}");
            }
            if let Some((left, _, f)) = l.wordmark {
                assert!(left + FONTS[f].width() <= w as usize, "{w}x{h}: {l:?}");
            }
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for t in [0.0, 1.0, 2.0, END, END + 5.0] {
                for whole in [false, true] {
                    let look = Look {
                        preview: true,
                        whole,
                        ..Default::default()
                    };
                    draw(&mut buf, area, t, look);
                }
            }
            let mono = Look {
                mono: true,
                updated: true,
                ..Default::default()
            };
            draw(&mut buf, area, END, mono);
        }
    }

    #[test]
    fn shows_once_per_home_and_version() {
        let v = "1.2.3";
        // First run shows; the same version seen again does not.
        assert_eq!(
            decide("once", false, None, v, true, false),
            Some(Startup::First)
        );
        assert_eq!(
            decide("once", false, Some(""), v, true, false),
            Some(Startup::First)
        );
        assert_eq!(decide("once", false, Some("1.2.3\n"), v, true, false), None);
        // A new version: once more, as "updated to".
        assert_eq!(
            decide("once", false, Some("1.2.2"), v, true, false),
            Some(Startup::Updated)
        );
    }

    #[test]
    fn never_quiet_or_without_a_terminal() {
        let v = "1.2.3";
        assert_eq!(decide("never", false, None, v, true, false), None);
        // Smoke runs set setup_dont_show.
        assert_eq!(decide("once", true, None, v, true, false), None);
        assert_eq!(decide("once", false, None, v, false, false), None);
        assert_eq!(decide("once", false, None, v, true, true), None);
        // Tests are always quiet.
        assert!(quiet());
    }

    #[test]
    fn at_start_never_plays_in_tests() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        at_start(&Config::default());
        assert!(!seen_path().exists());
    }

    #[test]
    fn motion_setting() {
        if std::env::var_os("GODTERM_REDUCED_MOTION").is_none() {
            assert!(animate("on"));
            assert!(animate(""));
        }
        assert!(!animate("off"));
    }

    #[test]
    fn icosahedron_shape() {
        let (v, f) = icosahedron();
        assert_eq!((v.len(), f.len()), (12, 20));
    }
}
