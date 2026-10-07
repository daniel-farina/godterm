//! Drawing the live map: a Braille canvas of the core, the account ring,
//! the tab satellites and the token particles, a glow pass on the cell
//! backgrounds, shaded block glyphs for the core, and the HUD (totals,
//! sparklines, legend, ticker, tooltip). This view alone is vivid on
//! purpose: it is a showpiece; the rest of the app keeps its calm palette.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Painter, Shape};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::livemap::{
    hash01, hit, human, layout, Link, LiveMap, Node, NodeId, Placed, TabSnap, TabState, TickKind,
};

type C = (u8, u8, u8);

const IN: C = (40, 196, 255);
const IN_HOT: C = (150, 240, 255);
const OUT: C = (255, 146, 28);
const OUT_HOT: C = (255, 228, 120);
const WAIT_A: C = (255, 186, 0);
const WAIT_B: C = (255, 48, 190);
const BRAIN: C = (186, 118, 255);
const GHOST: C = (128, 140, 176);
const SUB: C = (80, 255, 186);
const SHELL: C = (255, 236, 100);
const PAUSED: C = (96, 108, 140);
const TEXT: C = (226, 230, 242);
const MUTE: C = (120, 128, 156);
const PANEL: C = (14, 16, 30);

// ---------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------

fn col(c: C) -> Color {
    crate::theme::rgb(c)
}

fn hsv(h: f64, s: f64, v: f64) -> C {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i as i32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn to_hsv(c: C) -> (f64, f64, f64) {
    let (r, g, b) = (c.0 as f64 / 255.0, c.1 as f64 / 255.0, c.2 as f64 / 255.0);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let h = if d == 0.0 {
        0.0
    } else if mx == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if mx == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, if mx == 0.0 { 0.0 } else { d / mx }, mx)
}

/// An account's calm color turned up for the showpiece.
fn vivid(c: C) -> C {
    let (h, s, _) = to_hsv(c);
    hsv(h, (s * 3.0).clamp(0.62, 0.95), 1.0)
}

fn mix(a: C, b: C, t: f64) -> C {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn scale(c: C, k: f64) -> C {
    let k = k.max(0.0);
    let m = |x: u8| (x as f64 * k).min(255.0) as u8;
    (m(c.0), m(c.1), m(c.2))
}

/// Percent left to a vivid traffic light: green, yellow, orange, red.
fn quota_color(left: f64) -> C {
    let l = left.clamp(0.0, 100.0);
    if l >= 60.0 {
        mix((255, 220, 40), (40, 255, 140), (l - 60.0) / 40.0)
    } else if l >= 25.0 {
        mix((255, 120, 30), (255, 220, 40), (l - 25.0) / 35.0)
    } else {
        mix((255, 40, 80), (255, 120, 30), l / 25.0)
    }
}

// ---------------------------------------------------------------------
// Painting dots
// ---------------------------------------------------------------------

/// Dots with y pointing down, on a canvas of `h` dot rows.
struct Pen<'p, 'a, 'b> {
    p: &'p mut Painter<'a, 'b>,
    h: f64,
}

impl Pen<'_, '_, '_> {
    fn dot(&mut self, x: f64, y: f64, c: C) {
        if let Some((gx, gy)) = self.p.get_point(x, self.h - 1.0 - y) {
            self.p.paint(gx, gy, col(c));
        }
    }

    /// An arc from angle a0 to a1 (radians, clockwise on screen as y is
    /// down). `dash` = (on, off) in dots.
    fn arc(&mut self, x: f64, y: f64, r: f64, a0: f64, a1: f64, c: C, dash: Option<(f64, f64)>) {
        if r <= 0.0 {
            return;
        }
        let step = (0.8 / r).min(0.5);
        let mut a = a0;
        let mut s = 0.0;
        while a <= a1 {
            let on = match dash {
                Some((on, off)) => (s % (on + off)) < on,
                None => true,
            };
            if on {
                self.dot(x + r * a.cos(), y + r * a.sin(), c);
            }
            a += step;
            s += step * r;
        }
    }

    fn circle(&mut self, x: f64, y: f64, r: f64, c: C, dash: Option<(f64, f64)>) {
        self.arc(x, y, r, 0.0, TAU, c, dash);
    }

    fn disc(&mut self, x: f64, y: f64, r: f64, c: C) {
        let r = r.max(0.6);
        let ri = r.ceil() as i64;
        for dy in -ri..=ri {
            for dx in -ri..=ri {
                if (dx * dx + dy * dy) as f64 <= r * r + 0.3 {
                    self.dot(x + dx as f64, y + dy as f64, c);
                }
            }
        }
    }

    fn hexagon(&mut self, x: f64, y: f64, r: f64, c: C, fill: Option<C>, rot: f64) {
        let pts: Vec<(f64, f64)> = (0..6)
            .map(|i| {
                let a = rot + TAU * i as f64 / 6.0;
                (x + r * a.cos(), y + r * a.sin())
            })
            .collect();
        if let Some(fc) = fill {
            let ri = r.ceil() as i64;
            for dy in -ri..=ri {
                for dx in -ri..=ri {
                    let (px, py) = (x + dx as f64, y + dy as f64);
                    if inside(&pts, px, py) {
                        self.dot(px, py, fc);
                    }
                }
            }
        }
        for i in 0..6 {
            let (a, b) = (pts[i], pts[(i + 1) % 6]);
            self.seg(a.0, a.1, b.0, b.1, c, None, 0.0);
        }
    }

    /// A straight line; `dash` with a moving `phase`.
    #[allow(clippy::too_many_arguments)]
    fn seg(
        &mut self,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        c: C,
        dash: Option<(f64, f64)>,
        phase: f64,
    ) {
        let len = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
        let n = (len / 0.8).ceil().max(1.0) as usize;
        for i in 0..=n {
            let s = len * i as f64 / n as f64;
            if let Some((on, off)) = dash {
                if (s + phase).rem_euclid(on + off) >= on {
                    continue;
                }
            }
            let k = i as f64 / n as f64;
            self.dot(x1 + (x2 - x1) * k, y1 + (y2 - y1) * k, c);
        }
    }
}

fn inside(pts: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut c = false;
    let n = pts.len();
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + n - 1) % n]);
        if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

/// A point on the quadratic curve p0 -> p2 bent toward p1.
fn bez(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    (
        u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0,
        u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1,
    )
}

/// The control point of the core to account link: a gentle swirl.
fn swirl(core: (f64, f64), a: (f64, f64)) -> (f64, f64) {
    let (mx, my) = ((core.0 + a.0) / 2.0, (core.1 + a.1) / 2.0);
    let (dx, dy) = (a.0 - core.0, a.1 - core.1);
    (mx - dy * 0.22, my + dx * 0.22)
}

/// Where a particle is: in goes core, account, tab; out the other way.
/// The first 72% of the trip is the swirl to the account.
fn along(core: (f64, f64), acc: Option<(f64, f64)>, end: (f64, f64), t: f64) -> (f64, f64) {
    match acc {
        Some(a) => {
            const SPLIT: f64 = 0.72;
            if t < SPLIT {
                bez(core, swirl(core, a), a, t / SPLIT)
            } else {
                let k = (t - SPLIT) / (1.0 - SPLIT);
                (a.0 + (end.0 - a.0) * k, a.1 + (end.1 - a.1) * k)
            }
        }
        None => {
            let c = swirl(core, end);
            bez(core, c, end, t)
        }
    }
}

// ---------------------------------------------------------------------
// The scene
// ---------------------------------------------------------------------

struct Scene<'a> {
    lm: &'a LiveMap,
    placed: &'a Placed,
    w: f64,
    h: f64,
    reduced: bool,
    tabs: HashMap<u64, &'a TabSnap>,
    acc_colors: Vec<C>,
}

impl Scene<'_> {
    fn node(&self, id: NodeId) -> Option<&Node> {
        self.placed.get(id)
    }

    fn acc_pos(&self, i: usize) -> Option<(f64, f64)> {
        self.node(NodeId::Account(i)).map(|n| (n.x, n.y))
    }

    fn core_hue(&self) -> f64 {
        self.lm.t * 0.045
    }

    fn back(&self, pen: &mut Pen) {
        let t = self.lm.t;
        // Stars.
        let n = ((self.w * self.h) / 380.0) as u64;
        for i in 0..n {
            let x = hash01(i * 7 + 1) * self.w;
            let y = hash01(i * 7 + 2) * self.h;
            let ph = hash01(i * 7 + 3) * TAU;
            let sp = 0.6 + hash01(i * 7 + 4) * 1.8;
            let tw = if self.reduced {
                0.6
            } else {
                0.5 + 0.5 * (t * sp + ph).sin()
            };
            if tw < 0.25 {
                continue;
            }
            let hue = hash01(i * 7 + 5);
            let c = scale(hsv(0.55 + hue * 0.25, 0.35, 1.0), 0.22 + 0.4 * tw);
            pen.dot(x, y, c);
        }
        let (cx, cy) = self.placed.core;
        let (rx, ry) = self.placed.ring;
        // The account ring: a faint dashed ellipse whose dashes travel.
        if !self.lm.snap.accounts.is_empty() {
            let steps = ((rx + ry) * 2.2) as usize;
            for i in 0..steps {
                let a = TAU * i as f64 / steps as f64;
                let s = (a * (rx + ry) * 0.5 + t * 6.0).rem_euclid(9.0);
                if s < 4.0 {
                    let c = hsv(self.core_hue() + 0.6 + a / TAU * 0.3, 0.7, 0.32);
                    pen.dot(cx + rx * a.cos(), cy + ry * a.sin(), c);
                }
            }
        }
        // Orbit guides around the accounts that have tabs.
        for (i, ac) in self.acc_colors.iter().enumerate() {
            let Some(a) = self.acc_pos(i) else { continue };
            let mut radii: Vec<f64> = self
                .placed
                .nodes
                .iter()
                .filter(|n| n.parent == Some(i))
                .map(|n| ((n.x - a.0).powi(2) + (n.y - a.1).powi(2)).sqrt())
                .map(|d| (d * 2.0).round() / 2.0)
                .collect();
            radii.sort_by(|x, y| x.total_cmp(y));
            radii.dedup_by(|x, y| (*x - *y).abs() < 1.5);
            for r in radii {
                pen.circle(a.0, a.1, r, scale(*ac, 0.22), Some((1.0, 3.0)));
            }
        }
        // Links: core to accounts (a swirl with a travelling shimmer),
        // accounts to their busy tabs, the brain, other terminals.
        for (i, ac) in self.acc_colors.iter().enumerate() {
            let Some(a) = self.acc_pos(i) else { continue };
            let busy = self.lm.snap.tabs.iter().any(|tb| {
                tb.account == Some(i) && matches!(tb.state, TabState::Working | TabState::Waiting)
            });
            let ctrl = swirl((cx, cy), a);
            let len = ((a.0 - cx).powi(2) + (a.1 - cy).powi(2)).sqrt();
            let n = (len / 1.1) as usize;
            for k in 0..n {
                let u = k as f64 / n as f64;
                let (x, y) = bez((cx, cy), ctrl, a, u);
                let wave = 0.5 + 0.5 * ((u * len) * 0.18 - t * 5.0).sin();
                let base = if busy { 0.34 } else { 0.18 };
                let c = mix(
                    scale(*ac, base + 0.3 * wave * f64::from(u8::from(busy))),
                    scale(IN, 0.4),
                    0.15,
                );
                if busy || k % 2 == 0 {
                    pen.dot(x, y, c);
                }
            }
        }
        for n in &self.placed.nodes {
            let NodeId::Tab(uid) = n.id else { continue };
            let Some(tb) = self.tabs.get(&uid) else {
                continue;
            };
            let from = match n.parent.and_then(|p| self.acc_pos(p)) {
                Some(a) => a,
                None => (cx, cy),
            };
            let ac = n.parent.map(|p| self.acc_colors[p]).unwrap_or(TEXT);
            let (c, dash) = match tb.state {
                TabState::Working => (scale(ac, 0.6), None),
                TabState::Waiting => (scale(WAIT_A, 0.45), Some((2.0, 2.0))),
                TabState::Background => (scale(SUB, 0.35), Some((1.0, 2.0))),
                TabState::Idle | TabState::Starting => (scale(ac, 0.2), Some((1.0, 3.0))),
                _ => continue,
            };
            pen.seg(from.0, from.1, n.x, n.y, c, dash, -t * 8.0);
        }
        if let Some(b) = self.node(NodeId::Brain) {
            let c = if self.lm.snap.brain_busy {
                scale(BRAIN, 0.6)
            } else {
                scale(BRAIN, 0.25)
            };
            pen.seg(cx, cy, b.x, b.y, c, Some((2.0, 2.0)), -t * 6.0);
        }
        for (k, _) in self.lm.snap.ghosts.iter().enumerate() {
            if let Some(g) = self.node(NodeId::Ghost(k)) {
                pen.seg(
                    cx,
                    cy,
                    g.x,
                    g.y,
                    scale(GHOST, 0.3),
                    Some((2.0, 5.0)),
                    t * 2.0,
                );
            }
        }
        self.particles(pen);
        self.ripples(pen);
        self.core_rings(pen);
    }

    fn particles(&self, pen: &mut Pen) {
        let core = self.placed.core;
        for p in &self.lm.sim.particles {
            let (acc, end) = match p.link {
                Link::Tab(uid) => {
                    let Some(n) = self.node(NodeId::Tab(uid)) else {
                        continue;
                    };
                    (n.parent.and_then(|i| self.acc_pos(i)), (n.x, n.y))
                }
                Link::Brain => {
                    let Some(n) = self.node(NodeId::Brain) else {
                        continue;
                    };
                    (None, (n.x, n.y))
                }
                Link::Ghost(k) => {
                    let Some(n) = self.node(NodeId::Ghost(k)) else {
                        continue;
                    };
                    (None, (n.x, n.y))
                }
            };
            // Out flows from the tab back to the core.
            let pos = |t: f64| {
                let t = t.clamp(0.0, 1.0);
                along(core, acc, end, if p.out { 1.0 - t } else { t })
            };
            let (base, hot) = match (p.link, p.out) {
                (Link::Brain, _) => (BRAIN, (230, 200, 255)),
                (Link::Ghost(_), _) => (GHOST, (200, 210, 240)),
                (_, false) => (IN, IN_HOT),
                (_, true) => (OUT, OUT_HOT),
            };
            let shade = mix(base, hot, p.shade * 0.6 + if p.burst { 0.4 } else { 0.0 });
            // Sideways wobble, so a stream reads as a stream.
            let (x0, y0) = pos(p.t);
            let (x1, y1) = pos(p.t + 0.01);
            let (dx, dy) = (x1 - x0, y1 - y0);
            let l = (dx * dx + dy * dy).sqrt().max(1e-6);
            let wob = p.wob * (p.t * PI).sin();
            let (nx, ny) = (-dy / l * wob, dx / l * wob);
            let trail = if p.burst { 4 } else { 2 };
            for k in (0..=trail).rev() {
                let tt = p.t - k as f64 * 0.018 * p.speed.max(0.4);
                if tt < 0.0 {
                    continue;
                }
                let (x, y) = pos(tt);
                let fade = 1.0 - k as f64 / (trail as f64 + 1.0);
                pen.dot(x + nx, y + ny, scale(shade, 0.35 + 0.65 * fade));
            }
        }
    }

    fn ripples(&self, pen: &mut Pen) {
        for r in &self.lm.ripples {
            let Some(n) = self.node(r.node) else { continue };
            let age = ((self.lm.t - r.born) / 1.6).clamp(0.0, 1.0);
            let rad = n.r + 1.0 + r.reach * age;
            pen.circle(n.x, n.y, rad, scale(r.color, 1.0 - age), Some((2.0, 1.0)));
        }
    }

    fn core_rings(&self, pen: &mut Pen) {
        let (cx, cy) = self.placed.core;
        let t = self.lm.t;
        let r0 = self.placed.core_r;
        let hue = self.core_hue();
        let (tin, tout) = self.lm.totals();
        let energy = ((1.0 + tin + tout).log10() / 3.5).clamp(0.0, 1.0);
        let pulse = (t * 2.2).sin() * (0.6 + energy);
        let r1 = r0 + 2.5 + pulse * 0.6;
        let a = t * (1.0 + energy * 1.5);
        pen.arc(cx, cy, r1, a, a + 4.4, hsv(hue, 0.8, 1.0), None);
        let r2 = r0 + 5.5;
        let b = -t * 0.7;
        pen.arc(cx, cy, r2, b, b + 1.5, hsv(hue + 0.33, 0.85, 1.0), None);
        pen.arc(
            cx,
            cy,
            r2,
            b + PI,
            b + PI + 1.5,
            hsv(hue + 0.33, 0.85, 1.0),
            None,
        );
        let r3 = r0 + 8.5;
        let c3 = t * 0.3;
        pen.arc(
            cx,
            cy,
            r3,
            c3,
            c3 + TAU,
            scale(hsv(hue + 0.66, 0.7, 1.0), 0.6),
            Some((1.0, 2.0)),
        );
        // Sunburst: rays that turn slowly and breathe with the token flow.
        let rays = 14;
        for i in 0..rays {
            let a = t * 0.18 + TAU * i as f64 / rays as f64;
            let len = 4.0 + 5.0 * energy * (0.5 + 0.5 * (t * 1.7 + i as f64 * 1.3).sin());
            let (r_in, r_out) = (r3 + 2.0, r3 + 2.0 + len);
            let n = ((r_out - r_in) / 0.9) as usize;
            for k in 0..=n {
                let rr = r_in + (r_out - r_in) * k as f64 / n.max(1) as f64;
                let fade = 1.0 - k as f64 / (n as f64 + 1.0);
                let c = scale(
                    hsv(hue + i as f64 / rays as f64 * 0.5, 0.75, 1.0),
                    0.25 + 0.55 * fade,
                );
                pen.dot(cx + rr * a.cos(), cy + rr * a.sin(), c);
            }
        }
        // Motes circling at different speeds.
        for i in 0..10 {
            let sp = 0.25 + hash01(900 + i) * 0.9;
            let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
            let a = dir * t * sp + hash01(800 + i) * TAU;
            let rr = r0 + 4.0 + hash01(700 + i) * 10.0;
            let c = hsv(hue + hash01(600 + i), 0.5, 1.0);
            pen.dot(cx + rr * a.cos(), cy + rr * a.sin(), c);
            pen.dot(
                cx + rr * (a - dir * 0.08).cos(),
                cy + rr * (a - dir * 0.08).sin(),
                scale(c, 0.5),
            );
        }
        // A slow expanding halo.
        let k = (t * 0.45).fract();
        pen.circle(
            cx,
            cy,
            r0 + 3.0 + 14.0 * k,
            scale(hsv(hue + 0.15, 0.6, 1.0), 0.55 * (1.0 - k)),
            Some((1.0, 2.0)),
        );
    }

    fn front(&self, pen: &mut Pen) {
        let t = self.lm.t;
        for (i, a) in self.lm.snap.accounts.iter().enumerate() {
            let Some(n) = self.node(NodeId::Account(i)) else {
                continue;
            };
            let c = self.acc_colors[i];
            let (x, y, r) = (n.x, n.y, n.r);
            if a.grok {
                let fill = if a.logged_in {
                    Some(scale(c, 0.85))
                } else {
                    None
                };
                pen.hexagon(x, y, r + 0.8, mix(c, (255, 255, 255), 0.35), fill, t * 0.15);
            } else if a.logged_in {
                pen.disc(x, y, r, scale(c, 0.85));
                pen.disc(
                    x - r * 0.3,
                    y - r * 0.3,
                    r * 0.35,
                    mix(c, (255, 255, 255), 0.6),
                );
            } else {
                pen.circle(x, y, r, scale(c, 0.6), Some((1.0, 1.0)));
            }
            // Ring gauges: 5 hour inside, weekly outside, from 12 o'clock.
            for (k, left) in [(0, a.five_left), (1, a.week_left)] {
                let rr = r + 2.6 + k as f64 * 2.2;
                match left {
                    Some(l) => {
                        let ext = TAU * (l / 100.0).clamp(0.0, 1.0);
                        pen.arc(
                            x,
                            y,
                            rr,
                            -FRAC_PI_2 + ext,
                            -FRAC_PI_2 + TAU,
                            (52, 56, 78),
                            Some((1.0, 1.5)),
                        );
                        for d in [0.0, 0.75] {
                            pen.arc(
                                x,
                                y,
                                rr + d,
                                -FRAC_PI_2,
                                -FRAC_PI_2 + ext,
                                quota_color(l),
                                None,
                            );
                        }
                        // A bright tip where the quota ends.
                        let tip = -FRAC_PI_2 + ext;
                        pen.disc(
                            x + (rr + 0.4) * tip.cos(),
                            y + (rr + 0.4) * tip.sin(),
                            0.9,
                            mix(quota_color(l), (255, 255, 255), 0.5),
                        );
                    }
                    None if k == 1 && a.five_left.is_none() => {
                        pen.circle(x, y, rr, (60, 64, 88), Some((1.0, 2.0)));
                    }
                    None => {}
                }
            }
        }
        for n in &self.placed.nodes {
            let NodeId::Tab(uid) = n.id else { continue };
            let Some(tb) = self.tabs.get(&uid) else {
                continue;
            };
            let ac = n.parent.map(|p| self.acc_colors[p]).unwrap_or(TEXT);
            let ph = hash01(uid) * TAU;
            let (x, y, r) = (n.x, n.y, n.r);
            match tb.state {
                TabState::Working => {
                    let rr = r * (1.0 + 0.2 * (t * 6.0 + ph).sin());
                    let c = mix(ac, (255, 255, 255), 0.35 + 0.25 * (t * 6.0 + ph).sin());
                    pen.disc(x, y, rr, c);
                    let a = t * 5.0 + ph;
                    pen.arc(x, y, rr + 1.8, a, a + 2.2, IN_HOT, None);
                    pen.arc(x, y, rr + 1.8, a + PI, a + PI + 1.2, OUT_HOT, None);
                }
                TabState::Waiting => {
                    let on = (t * 2.4 + ph).fract() < 0.5;
                    let c = if on { WAIT_A } else { WAIT_B };
                    pen.disc(x, y, r, c);
                    let k = (t * 1.1 + ph).fract();
                    pen.circle(x, y, r + 1.5 + 6.0 * k, scale(c, 1.0 - k), None);
                    pen.circle(x, y, r + 1.5, scale(WAIT_B, 0.8), Some((1.0, 1.0)));
                }
                TabState::Idle => {
                    let b = 0.4 + 0.1 * (t * 0.8 + ph).sin();
                    pen.disc(x, y, (r * 0.85).max(1.8), scale(ac, b));
                }
                TabState::Background => {
                    pen.disc(x, y, r * 0.9, scale(ac, 0.7));
                    let subs = tb.subagents.min(6);
                    let k = (tb.subagents + tb.shells).clamp(1, 8);
                    for i in 0..k {
                        let a = t * 1.8 + ph + TAU * i as f64 / k as f64;
                        let c = if i < subs { SUB } else { SHELL };
                        let (sx, sy) = (x + (r + 3.0) * a.cos(), y + (r + 3.0) * a.sin());
                        pen.disc(sx, sy, 0.8, c);
                    }
                }
                TabState::Starting => {
                    let a = t * 4.0 + ph;
                    pen.arc(x, y, r, a, a + 4.5, ac, None);
                }
                TabState::Suspended => {
                    pen.circle(x, y, r.max(1.8), PAUSED, None);
                }
                TabState::Exited => {
                    pen.circle(x, y, 1.2, (80, 84, 100), None);
                }
            }
        }
        if let Some(b) = self.node(NodeId::Brain) {
            let busy = self.lm.snap.brain_busy;
            let sp = if busy { 3.0 } else { 0.6 };
            let rr = b.r * (1.0 + if busy { 0.18 * (t * 5.0).sin() } else { 0.0 });
            pen.disc(
                b.x,
                b.y,
                rr,
                if busy {
                    mix(BRAIN, (255, 255, 255), 0.25)
                } else {
                    scale(BRAIN, 0.7)
                },
            );
            for i in 0..4 {
                let a = t * sp + FRAC_PI_2 * i as f64;
                pen.seg(
                    b.x + (rr + 1.0) * a.cos(),
                    b.y + (rr + 1.0) * a.sin(),
                    b.x + (rr + 3.5) * a.cos(),
                    b.y + (rr + 3.5) * a.sin(),
                    scale(BRAIN, 0.9),
                    None,
                    0.0,
                );
            }
        }
        for (k, g) in self.lm.snap.ghosts.iter().enumerate() {
            let Some(n) = self.node(NodeId::Ghost(k)) else {
                continue;
            };
            let fl = 0.55 + 0.35 * (t * 0.9 + k as f64).sin();
            let c = scale(
                if g.grok {
                    mix(GHOST, (200, 120, 255), 0.4)
                } else {
                    GHOST
                },
                fl,
            );
            pen.circle(n.x, n.y, n.r + 0.5, c, Some((1.0, 1.0)));
            pen.circle(n.x, n.y, n.r + 3.0, scale(c, 0.5), Some((1.0, 3.0)));
        }
    }
}

struct Layer<'s, 'a> {
    scene: &'s Scene<'a>,
    front: bool,
}

impl Shape for Layer<'_, '_> {
    fn draw(&self, painter: &mut Painter) {
        let mut pen = Pen {
            p: painter,
            h: self.scene.h,
        };
        if self.front {
            self.scene.front(&mut pen);
        } else {
            self.scene.back(&mut pen);
        }
    }
}

// ---------------------------------------------------------------------
// Glow, core orb, labels
// ---------------------------------------------------------------------

struct Glow {
    w: usize,
    h: usize,
    px: Vec<[f32; 3]>,
}

impl Glow {
    fn new(w: usize, h: usize) -> Glow {
        let mut px = vec![[0.0f32; 3]; w * h];
        // Deep space: a violet center fading to near black.
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        for y in 0..h {
            for x in 0..w {
                let dx = (x as f32 - cx) / cx.max(1.0);
                let dy = (y as f32 - cy) / cy.max(1.0);
                let k = (-(dx * dx + dy * dy) * 1.1).exp();
                px[y * w + x] = [4.0 + 10.0 * k, 5.0 + 5.0 * k, 12.0 + 20.0 * k];
            }
        }
        Glow { w, h, px }
    }

    /// A soft light at cell (x, y), `r` cells wide (cells are twice as tall
    /// as wide, so the falloff is round on screen).
    fn add(&mut self, x: f64, y: f64, r: f64, c: C, k: f64) {
        if r <= 0.0 || k <= 0.0 {
            return;
        }
        // Reach far enough that the falloff ends below one step of color.
        let reach = r * ((k * 255.0).max(1.0).ln() / 1.5).sqrt().max(1.0);
        let (x0, x1) = (
            (x - reach * 2.0).floor().max(0.0) as usize,
            ((x + reach * 2.0).ceil() as usize).min(self.w),
        );
        let (y0, y1) = (
            (y - reach).floor().max(0.0) as usize,
            ((y + reach).ceil() as usize).min(self.h),
        );
        let inv = 1.0 / (r * r);
        for cy in y0..y1 {
            for cx in x0..x1 {
                let dx = (cx as f64 + 0.5 - x) * 0.5;
                let dy = cy as f64 + 0.5 - y;
                let f = (-(dx * dx + dy * dy) * inv * 1.5).exp() * k;
                if f < 0.002 {
                    continue;
                }
                let p = &mut self.px[cy * self.w + cx];
                p[0] += (c.0 as f64 * f) as f32;
                p[1] += (c.1 as f64 * f) as f32;
                p[2] += (c.2 as f64 * f) as f32;
            }
        }
    }

    fn write(&self, buf: &mut Buffer, area: Rect) {
        for y in 0..self.h.min(area.height as usize) {
            for x in 0..self.w.min(area.width as usize) {
                let p = self.px[y * self.w + x];
                let c = (
                    p[0].min(255.0) as u8,
                    p[1].min(255.0) as u8,
                    p[2].min(255.0) as u8,
                );
                buf[(area.x + x as u16, area.y + y as u16)].set_bg(col(c));
            }
        }
    }
}

/// Cell coordinates of a dot point.
fn cell_of(x: f64, y: f64) -> (f64, f64) {
    (x / 2.0, y / 4.0)
}

/// The core: a shaded orb of block glyphs, hottest in the middle.
fn draw_core(buf: &mut Buffer, area: Rect, placed: &Placed, t: f64, hue: f64) {
    let (cx, cy) = cell_of(placed.core.0, placed.core.1);
    let pulse = 1.0 + 0.08 * (t * 2.2).sin();
    let (rx, ry) = (placed.core_r / 2.0 * pulse, placed.core_r / 4.0 * pulse);
    let (x0, x1) = (
        (cx - rx - 1.0).max(0.0) as u16,
        ((cx + rx + 1.0) as u16).min(area.width),
    );
    let (y0, y1) = (
        (cy - ry - 1.0).max(0.0) as u16,
        ((cy + ry + 1.0) as u16).min(area.height),
    );
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = (x as f64 + 0.5 - cx) / rx;
            let dy = (y as f64 + 0.5 - cy) / ry;
            let d = (dx * dx + dy * dy).sqrt();
            if d > 1.0 {
                continue;
            }
            let g = if d < 0.38 {
                '█'
            } else if d < 0.62 {
                '▓'
            } else if d < 0.84 {
                '▒'
            } else {
                '░'
            };
            // Swirling color across the orb.
            let ang = dy.atan2(dx);
            let c = hsv(hue + ang / TAU * 0.25 + d * 0.2, 0.25 + 0.65 * d, 1.0);
            let c = mix((255, 255, 255), c, d.powf(0.7));
            let cell = &mut buf[(area.x + x, area.y + y)];
            cell.set_char(g).set_fg(col(c));
        }
    }
}

/// Labels avoid each other with a coarse occupancy grid.
struct Labels {
    w: u16,
    taken: Vec<bool>,
}

impl Labels {
    fn new(area: Rect) -> Labels {
        Labels {
            w: area.width,
            taken: vec![false; area.width as usize * area.height as usize],
        }
    }
    fn reserve(&mut self, x: u16, y: u16, len: u16) {
        for i in x..x + len {
            if let Some(v) = self
                .taken
                .get_mut(y as usize * self.w as usize + i as usize)
            {
                *v = true;
            }
        }
    }
    fn free(&self, x: u16, y: u16, len: u16) -> bool {
        (x..x + len).all(|i| {
            i < self.w
                && !self
                    .taken
                    .get(y as usize * self.w as usize + i as usize)
                    .copied()
                    .unwrap_or(true)
        })
    }
}

fn put(buf: &mut Buffer, area: Rect, x: u16, y: u16, spans: &[(String, Style)]) {
    let mut cx = area.x + x;
    let y = area.y + y;
    for (s, st) in spans {
        for ch in s.chars() {
            if cx >= area.x + area.width {
                return;
            }
            buf[(cx, y)].set_char(ch).set_style(*st);
            cx += 1;
        }
    }
}

/// The end of a long path: "…/code/api".
fn tail(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n {
        return s.to_string();
    }
    let t: String = s.chars().skip(len + 1 - n).collect();
    format!("…{t}")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

// ---------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let started = Instant::now();
    app.livemap_frame();
    if area.width < 80 || area.height < 22 {
        app.livemap.canvas = Rect::default();
        draw_compact(f, app, area);
    } else {
        draw_full(f, app, area);
    }
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let lm = &mut app.livemap;
    lm.render_ms = if lm.frames == 0 {
        ms
    } else {
        lm.render_ms * 0.9 + ms * 0.1
    };
    lm.frames += 1;
}

fn draw_full(f: &mut Frame, app: &mut App, area: Rect) {
    let reduced = app.livemap_reduced();
    let canvas = Rect::new(area.x, area.y + 2, area.width, area.height - 3);
    let (w, h) = (canvas.width as f64 * 2.0, canvas.height as f64 * 4.0);
    let lm = &app.livemap;
    let placed = layout(&lm.snap, w, h, lm.zoom, lm.t, &lm.sizes);
    let acc_colors: Vec<C> = lm.snap.accounts.iter().map(|a| vivid(a.color)).collect();
    let scene = Scene {
        lm,
        placed: &placed,
        w,
        h,
        reduced,
        tabs: lm.snap.tabs.iter().map(|t| (t.uid, t)).collect(),
        acc_colors: acc_colors.clone(),
    };
    let back = Layer {
        scene: &scene,
        front: false,
    };
    let front = Layer {
        scene: &scene,
        front: true,
    };
    let cv = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, w - 1.0])
        .y_bounds([0.0, h - 1.0])
        .paint(|ctx| {
            ctx.draw(&back);
            ctx.layer();
            ctx.draw(&front);
        });
    f.render_widget(cv, canvas);

    // Glow on the cell backgrounds.
    let t = lm.t;
    let hue = t * 0.045;
    let mut glow = Glow::new(canvas.width as usize, canvas.height as usize);
    let (tin, tout) = lm.totals();
    let energy = ((1.0 + tin + tout).log10() / 3.5).clamp(0.0, 1.0);
    let (ccx, ccy) = cell_of(placed.core.0, placed.core.1);
    // Faint nebula clouds drifting behind everything.
    let (cw, ch) = (canvas.width as f64, canvas.height as f64);
    for (i, c) in [
        (150, 40, 200),
        (20, 120, 170),
        (190, 40, 110),
        (40, 90, 200),
    ]
    .into_iter()
    .enumerate()
    {
        let ph = i as f64 * 1.9;
        let x = cw * (0.5 + 0.36 * (t * 0.021 + ph).cos());
        let y = ch * (0.5 + 0.34 * (t * 0.017 + ph * 1.3).sin());
        glow.add(x, y, ch * 0.32, c, 0.07);
    }
    glow.add(
        ccx,
        ccy,
        placed.core_r / 4.0 * 4.6,
        hsv(hue, 0.8, 1.0),
        0.24 + 0.1 * energy + 0.05 * (t * 2.2).sin(),
    );
    glow.add(
        ccx,
        ccy,
        placed.core_r / 4.0 * 1.8,
        hsv(hue + 0.1, 0.4, 1.0),
        0.22,
    );
    for (i, a) in lm.snap.accounts.iter().enumerate() {
        let Some(n) = placed.get(NodeId::Account(i)) else {
            continue;
        };
        let busy = lm
            .snap
            .tabs
            .iter()
            .any(|tb| tb.account == Some(i) && tb.state == TabState::Working);
        let (x, y) = cell_of(n.x, n.y);
        let k = if !a.logged_in {
            0.05
        } else if busy {
            0.2 + 0.05 * (t * 3.0 + i as f64).sin()
        } else {
            0.1
        };
        glow.add(x, y, 4.4, acc_colors[i], k);
    }
    for n in &placed.nodes {
        let NodeId::Tab(uid) = n.id else { continue };
        let Some(tb) = scene.tabs.get(&uid) else {
            continue;
        };
        let (x, y) = cell_of(n.x, n.y);
        let ph = hash01(uid) * TAU;
        match tb.state {
            TabState::Working => glow.add(
                x,
                y,
                2.4,
                mix(IN, (255, 255, 255), 0.2),
                0.16 + 0.06 * (t * 6.0 + ph).sin(),
            ),
            TabState::Waiting => {
                let on = (t * 2.4 + ph).fract() < 0.5;
                glow.add(x, y, 3.0, if on { WAIT_A } else { WAIT_B }, 0.26);
            }
            TabState::Background => glow.add(x, y, 1.6, SUB, 0.12),
            _ => {}
        }
    }
    if let Some(b) = placed.get(NodeId::Brain) {
        let (x, y) = cell_of(b.x, b.y);
        glow.add(x, y, 2.0, BRAIN, if lm.snap.brain_busy { 0.3 } else { 0.1 });
    }
    let buf = f.buffer_mut();
    glow.write(buf, canvas);
    draw_core(buf, canvas, &placed, t, hue);

    // Labels.
    let mut occ = Labels::new(canvas);
    // Reserve the core's cells.
    let (cr_x, cr_y) = (placed.core_r / 2.0, placed.core_r / 4.0);
    for y in (ccy - cr_y).max(0.0) as u16..((ccy + cr_y + 1.0) as u16).min(canvas.height) {
        let x0 = (ccx - cr_x).max(0.0) as u16;
        occ.reserve(x0, y, (cr_x * 2.0 + 1.0) as u16);
    }
    let b = |c: C| Style::default().fg(col(c)).add_modifier(Modifier::BOLD);
    let p = |c: C| Style::default().fg(col(c));
    {
        let label = "GODTERM";
        let y = (ccy + cr_y + 1.6) as u16;
        let x = (ccx - label.len() as f64 / 2.0).max(0.0) as u16;
        if y < canvas.height && occ.free(x, y, label.len() as u16) {
            let spans: Vec<(String, Style)> = label
                .chars()
                .enumerate()
                .map(|(i, ch)| (ch.to_string(), b(hsv(hue + i as f64 * 0.06, 0.6, 1.0))))
                .collect();
            put(buf, canvas, x, y, &spans);
            occ.reserve(x, y, label.len() as u16);
        }
    }
    for (i, a) in lm.snap.accounts.iter().enumerate() {
        let Some(n) = placed.get(NodeId::Account(i)) else {
            continue;
        };
        let (x, y) = cell_of(n.x, n.y + n.r + 6.5);
        let name = truncate(&a.label, 16);
        let pct = a.eff_left.map(|l| format!(" {l:.0}%")).unwrap_or_default();
        let len = (name.chars().count() + pct.chars().count() + if a.grok { 2 } else { 0 }) as u16;
        let x = (x - len as f64 / 2.0).max(0.0) as u16;
        let y = (y as u16).min(canvas.height.saturating_sub(1));
        if occ.free(x, y, len) {
            let mut sp = vec![];
            if a.grok {
                sp.push((
                    "⬢ ".to_string(),
                    b(mix(acc_colors[i], (255, 255, 255), 0.3)),
                ));
            }
            sp.push((name, b(acc_colors[i])));
            if let Some(l) = a.eff_left {
                sp.push((pct, b(quota_color(l))));
            }
            put(buf, canvas, x, y, &sp);
            occ.reserve(x, y, len);
        }
    }
    if lm.labels {
        // Busy tabs first, so they keep their label when space runs out.
        let mut order: Vec<&Node> = placed
            .nodes
            .iter()
            .filter(|n| matches!(n.id, NodeId::Tab(_)))
            .collect();
        let rank = |n: &&Node| match n.id {
            NodeId::Tab(u) => match scene.tabs.get(&u).map(|t| t.state) {
                Some(TabState::Waiting) => 0,
                Some(TabState::Working) => 1,
                Some(TabState::Background) => 2,
                _ => 3,
            },
            _ => 4,
        };
        order.sort_by_key(rank);
        for n in order {
            let NodeId::Tab(uid) = n.id else { continue };
            let Some(tb) = scene.tabs.get(&uid) else {
                continue;
            };
            let name = truncate(&tb.name, 14);
            let len = name.chars().count() as u16;
            let (x, y) = cell_of(n.x, n.y);
            let y = y as u16;
            let right = (x + n.r / 2.0 + 1.5) as u16;
            let left = (x - n.r / 2.0 - 1.0 - len as f64).max(0.0) as u16;
            let ac = n.parent.map(|p| acc_colors[p]).unwrap_or(TEXT);
            let st = match tb.state {
                TabState::Working => b(mix(ac, (255, 255, 255), 0.5)),
                TabState::Waiting => b(WAIT_A),
                TabState::Background => p(mix(SUB, ac, 0.3)),
                TabState::Suspended | TabState::Exited => p(PAUSED),
                _ => p(scale(ac, 0.75)),
            };
            for x in [right, left] {
                if x + len <= canvas.width && y < canvas.height && occ.free(x, y, len) {
                    put(buf, canvas, x, y, &[(name.clone(), st)]);
                    occ.reserve(x.saturating_sub(1), y, len + 2);
                    break;
                }
            }
        }
        for (k, g) in lm.snap.ghosts.iter().enumerate() {
            let Some(n) = placed.get(NodeId::Ghost(k)) else {
                continue;
            };
            let text = format!("◌ {}", truncate(&g.label, 16));
            let len = text.chars().count() as u16;
            let (x, y) = cell_of(n.x, n.y + 6.0);
            let x = (x - len as f64 / 2.0).max(0.0) as u16;
            let y = (y as u16).min(canvas.height.saturating_sub(1));
            if x + len <= canvas.width && occ.free(x, y, len) {
                put(
                    buf,
                    canvas,
                    x,
                    y,
                    &[(text, p(GHOST).add_modifier(Modifier::ITALIC))],
                );
                occ.reserve(x, y, len);
            }
        }
        if let Some(bn) = placed.get(NodeId::Brain) {
            let text = if lm.snap.brain_busy {
                "assistant ✶ thinking"
            } else {
                "assistant"
            };
            let len = text.chars().count() as u16;
            let (x, y) = cell_of(bn.x, bn.y);
            let x = (x + 2.5) as u16;
            let y = y as u16;
            if x + len <= canvas.width && y < canvas.height && occ.free(x, y, len) {
                put(buf, canvas, x, y, &[(text.to_string(), b(BRAIN))]);
            }
        }
    }
    draw_hud(f, app, Rect::new(area.x, area.y, area.width, 2));
    draw_legend(f, app, canvas);
    draw_ticker(
        f,
        app,
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
    let lm = &mut app.livemap;
    lm.placed = placed;
    lm.canvas = canvas;
    draw_tooltip(f, app, canvas);
}

/// Unicode sparkline of the last `n` values, scaled to their max.
fn spark(vals: &[f64], n: usize) -> Vec<(char, f64)> {
    const B: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let take: Vec<f64> = vals.iter().rev().take(n).rev().copied().collect();
    let max = take.iter().cloned().fold(0.0, f64::max).max(1.0);
    let mut out: Vec<(char, f64)> = vec![(' ', 0.0); n.saturating_sub(take.len())];
    for v in take {
        let k = (v / max).clamp(0.0, 1.0);
        let i = ((k * 7.0).round() as usize).min(7);
        out.push((if v <= 0.0 { '▁' } else { B[i] }, k));
    }
    out
}

fn draw_hud(f: &mut Frame, app: &App, area: Rect) {
    let lm = &app.livemap;
    let s = &lm.snap;
    let t = lm.t;
    let buf = f.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].reset();
            buf[(x, y)].set_bg(col(PANEL));
        }
    }
    let bold = |c: C| {
        Style::default()
            .fg(col(c))
            .bg(col(PANEL))
            .add_modifier(Modifier::BOLD)
    };
    let pl = |c: C| Style::default().fg(col(c)).bg(col(PANEL));
    let mut row: Vec<(String, Style)> = vec![(" ".into(), pl(TEXT))];
    let title = "◆ LIVE MAP";
    for (i, ch) in title.chars().enumerate() {
        row.push((
            ch.to_string(),
            bold(hsv(t * 0.08 + i as f64 * 0.05, 0.75, 1.0)),
        ));
    }
    let running = s
        .tabs
        .iter()
        .filter(|x| !matches!(x.state, TabState::Suspended | TabState::Exited))
        .count();
    row.push((format!("   {running} running"), bold(TEXT)));
    row.push((format!(" / {} tabs", s.tabs.len()), pl(MUTE)));
    let chip = |row: &mut Vec<(String, Style)>, glyph: &str, n: usize, word: &str, c: C| {
        row.push(("  ".into(), pl(TEXT)));
        row.push((glyph.into(), bold(if n > 0 { c } else { scale(c, 0.4) })));
        row.push((format!(" {n} {word}"), pl(if n > 0 { TEXT } else { MUTE })));
    };
    let blink = (t * 2.4).fract() < 0.5;
    chip(&mut row, "●", s.count(TabState::Working), "working", IN_HOT);
    chip(
        &mut row,
        "◉",
        s.count(TabState::Waiting),
        "waiting",
        if blink { WAIT_A } else { WAIT_B },
    );
    chip(
        &mut row,
        "✺",
        s.count(TabState::Background),
        "background",
        SUB,
    );
    chip(
        &mut row,
        "●",
        s.count(TabState::Idle),
        "idle",
        (120, 140, 200),
    );
    chip(&mut row, "○", s.count(TabState::Suspended), "zz", PAUSED);
    if !s.ghosts.is_empty() {
        chip(&mut row, "◌", s.ghosts.len(), "elsewhere", GHOST);
    }
    if lm.paused {
        row.push(("   ❚❚ PAUSED".into(), bold(WAIT_A)));
    }
    let used: usize = row.iter().map(|r| r.0.chars().count()).sum();
    let fps = if app.livemap_reduced() {
        "reduced motion"
    } else {
        ""
    };
    if !fps.is_empty() && used + fps.len() + 2 < area.width as usize {
        row.push((
            " ".repeat(area.width as usize - used - fps.len() - 1),
            pl(MUTE),
        ));
        row.push((fps.into(), pl(MUTE)));
    }
    put(buf, area, 0, 0, &row);

    // Second row: token sparklines and quota.
    let (tin, tout) = lm.totals();
    let ins: Vec<f64> = lm.hist.iter().map(|h| h.0).collect();
    let outs: Vec<f64> = lm.hist.iter().map(|h| h.1).collect();
    let n = if area.width > 150 { 24 } else { 14 };
    let mut row: Vec<(String, Style)> = vec![(" ".into(), pl(TEXT))];
    row.push(("▼ IN  ".into(), bold(IN)));
    for (c, k) in spark(&ins, n) {
        row.push((c.to_string(), pl(mix(scale(IN, 0.55), IN_HOT, k))));
    }
    row.push((format!(" {:>6}/min", human(tin * 60.0)), bold(IN_HOT)));
    row.push(("   ▲ OUT ".into(), bold(OUT)));
    for (c, k) in spark(&outs, n) {
        row.push((c.to_string(), pl(mix(scale(OUT, 0.55), OUT_HOT, k))));
    }
    row.push((format!(" {:>6}/min", human(tout * 60.0)), bold(OUT_HOT)));
    row.push(("   │ ".into(), pl(MUTE)));
    let mut used: usize = row.iter().map(|r| r.0.chars().count()).sum();
    for a in &s.accounts {
        let Some(l) = a.eff_left else { continue };
        let name = truncate(&a.label, 12);
        let cells = 5usize;
        let full = ((l / 100.0) * cells as f64).round() as usize;
        let piece = name.chars().count() + cells + 7;
        if used + piece > area.width as usize {
            break;
        }
        used += piece;
        row.push((name, pl(vivid(a.color))));
        row.push((" ".into(), pl(TEXT)));
        row.push(("▰".repeat(full.min(cells)), pl(quota_color(l))));
        row.push(("▱".repeat(cells - full.min(cells)), pl((60, 64, 90))));
        row.push((format!(" {l:>3.0}%  "), bold(quota_color(l))));
    }
    if let Some(c) = s.brain_cost {
        let txt = format!("assistant ${c:.3}");
        if used + txt.len() + 2 < area.width as usize {
            row.push((txt, pl(BRAIN)));
        }
    }
    put(buf, area, 0, 1, &row);
}

fn draw_legend(f: &mut Frame, app: &App, canvas: Rect) {
    if canvas.height < 18 || canvas.width < 100 || !app.livemap.labels {
        return;
    }
    let t = app.livemap.t;
    let blink = (t * 2.4).fract() < 0.5;
    let pl = |c: C| Style::default().fg(col(c));
    let rows: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("● ", pl(IN_HOT)),
            Span::styled("working   ", pl(TEXT)),
            Span::styled("◉ ", pl(if blink { WAIT_A } else { WAIT_B })),
            Span::styled("needs you", pl(TEXT)),
        ]),
        Line::from(vec![
            Span::styled("● ", pl((110, 130, 190))),
            Span::styled("idle      ", pl(TEXT)),
            Span::styled("○ ", pl(PAUSED)),
            Span::styled("paused zz", pl(TEXT)),
        ]),
        Line::from(vec![
            Span::styled("✺ ", pl(SUB)),
            Span::styled("subagents ", pl(TEXT)),
            Span::styled("✺ ", pl(SHELL)),
            Span::styled("shells", pl(TEXT)),
        ]),
        Line::from(vec![
            Span::styled("━ ", pl(IN)),
            Span::styled("tokens in ", pl(TEXT)),
            Span::styled("━ ", pl(OUT)),
            Span::styled("tokens out", pl(TEXT)),
        ]),
        Line::from(vec![
            Span::styled("⬢ ", pl((200, 160, 255))),
            Span::styled("grok      ", pl(TEXT)),
            Span::styled("◌ ", pl(GHOST)),
            Span::styled("elsewhere", pl(TEXT)),
        ]),
        Line::from(vec![Span::styled("rings: 5h inside, week out", pl(MUTE))]),
    ];
    let w = 29u16;
    let h = rows.len() as u16 + 2;
    let r = Rect::new(canvas.x + 1, canvas.y + canvas.height - h, w, h);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(pl((70, 76, 110)))
        .title(Span::styled(" legend ", pl(MUTE)))
        .style(Style::default().bg(col(PANEL)));
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(rows).block(block), r);
}

fn tick_color(k: &TickKind) -> C {
    match k {
        TickKind::Opened => (100, 255, 170),
        TickKind::Closed => (150, 156, 186),
        TickKind::Turn => OUT_HOT,
        TickKind::Approval => WAIT_B,
        TickKind::Loop => (120, 200, 255),
        TickKind::Brain => BRAIN,
        TickKind::Tokens => IN_HOT,
    }
}

fn draw_ticker(f: &mut Frame, app: &App, area: Rect) {
    let lm = &app.livemap;
    let pl = |c: C| Style::default().fg(col(c)).bg(col(PANEL));
    let mut strip: Vec<(char, Style)> = vec![];
    let push = |strip: &mut Vec<(char, Style)>, s: &str, st: Style| {
        for ch in s.chars() {
            strip.push((ch, st));
        }
    };
    if lm.ticker.is_empty() {
        push(
            &mut strip,
            "  watching every session: events show up here  ",
            pl(MUTE),
        );
    }
    for e in lm.ticker.iter().rev().take(12) {
        let ago = (lm.t - e.at).max(0.0);
        let when = if ago < 60.0 {
            format!("{ago:.0}s")
        } else {
            format!("{:.0}m", ago / 60.0)
        };
        push(&mut strip, "  ◆ ", pl(tick_color(&e.kind)));
        push(&mut strip, &e.text, pl(TEXT));
        push(&mut strip, &format!(" {when}"), pl(MUTE));
    }
    push(&mut strip, "     ", pl(MUTE));
    let buf = f.buffer_mut();
    let w = area.width as usize;
    let lead = 13usize;
    // A fixed lead, then the strip scrolling right to left.
    let head = " ⟫ EVENTS  ";
    for x in 0..area.width {
        buf[(area.x + x, area.y)].reset();
        buf[(area.x + x, area.y)].set_bg(col(PANEL));
    }
    put(
        buf,
        area,
        0,
        0,
        &[(head.into(), pl((255, 200, 90)).add_modifier(Modifier::BOLD))],
    );
    if strip.is_empty() || w <= lead {
        return;
    }
    let n = strip.len();
    let off = if strip.len() + lead < w {
        0
    } else {
        (lm.t * 9.0) as usize % n
    };
    for x in lead..w {
        let i = x - lead + off;
        if strip.len() + lead < w && i >= n {
            break;
        }
        let (ch, st) = strip[i % n];
        buf[(area.x + x as u16, area.y)].set_char(ch).set_style(st);
    }
}

fn draw_tooltip(f: &mut Frame, app: &App, canvas: Rect) {
    let Some((mx, my)) = app.mouse_pos else {
        return;
    };
    if app.modal != crate::app::Modal::None || !crate::hits::contains(canvas, mx, my) {
        return;
    }
    let lm = &app.livemap;
    let (x, y) = crate::livemap::cell_to_dots(mx, my, canvas.x, canvas.y);
    let Some(id) = hit(&lm.placed, x, y) else {
        return;
    };
    let pl = |c: C| Style::default().fg(col(c));
    let b = |c: C| pl(c).add_modifier(Modifier::BOLD);
    let usage = |a: usize| -> Option<Line<'static>> {
        let ac = lm.snap.accounts.get(a)?;
        let f = |v: Option<f64>| v.map(|l| format!("{l:.0}%")).unwrap_or_else(|| "?".into());
        Some(Line::from(vec![
            Span::styled(format!("{}: ", ac.label), pl(vivid(ac.color))),
            Span::styled(
                format!("5h {} ", f(ac.five_left)),
                pl(ac.five_left.map(quota_color).unwrap_or(MUTE)),
            ),
            Span::styled(
                format!("week {} left", f(ac.week_left)),
                pl(ac.week_left.map(quota_color).unwrap_or(MUTE)),
            ),
        ]))
    };
    let mut lines: Vec<Line> = vec![];
    match id {
        NodeId::Tab(uid) => {
            let Some(tb) = lm.snap.tabs.iter().find(|t| t.uid == uid) else {
                return;
            };
            let r = lm.rates.get(&uid).cloned().unwrap_or_default();
            let ac = tb.account.and_then(|a| lm.snap.accounts.get(a));
            let c = ac.map(|a| vivid(a.color)).unwrap_or(TEXT);
            lines.push(Line::from(Span::styled(tb.name.clone(), b(c))));
            lines.push(Line::from(Span::styled(tail(&tb.folder, 48), pl(MUTE))));
            let mut st = tb.state.label().to_string();
            if tb.subagents > 0 {
                st.push_str(&format!(", {} subagents", tb.subagents));
            }
            if tb.shells > 0 {
                st.push_str(&format!(", {} shells", tb.shells));
            }
            if tb.loops > 0 {
                st.push_str(&format!(", {} loops", tb.loops));
            }
            let sc = match tb.state {
                TabState::Working => IN_HOT,
                TabState::Waiting => WAIT_A,
                TabState::Background => SUB,
                _ => TEXT,
            };
            lines.push(Line::from(Span::styled(st, b(sc))));
            lines.push(Line::from(vec![
                Span::styled(format!("in {}/s  ", human(r.ema_in)), pl(IN_HOT)),
                Span::styled(format!("out {}/s", human(r.ema_out)), pl(OUT_HOT)),
            ]));
            let mut ctx = format!("context {}", human(r.context as f64));
            if r.seen_in + r.seen_out > 0 {
                ctx.push_str(&format!(
                    ", {} in / {} out here",
                    human(r.seen_in as f64),
                    human(r.seen_out as f64)
                ));
            }
            lines.push(Line::from(Span::styled(ctx, pl(TEXT))));
            if let Some((i, o)) = tb.session_tokens {
                lines.push(Line::from(Span::styled(
                    format!("session {} in, {} out", human(i as f64), human(o as f64)),
                    pl(MUTE),
                )));
            }
            if let Some(l) = tb.account.and_then(usage) {
                lines.push(l);
            }
            lines.push(Line::from(Span::styled("click to open this tab", pl(MUTE))));
        }
        NodeId::Account(a) => {
            let Some(ac) = lm.snap.accounts.get(a) else {
                return;
            };
            lines.push(Line::from(vec![
                Span::styled(if ac.grok { "⬢ " } else { "● " }, b(vivid(ac.color))),
                Span::styled(ac.label.clone(), b(vivid(ac.color))),
                Span::styled(if ac.grok { "  grok" } else { "  claude" }, pl(MUTE)),
            ]));
            if !ac.logged_in {
                lines.push(Line::from(Span::styled("not logged in", pl(WAIT_A))));
            }
            if let Some(l) = usage(a) {
                lines.push(l);
            }
            if let Some(e) = ac.eff_left {
                lines.push(Line::from(Span::styled(
                    format!("really left: {e:.0}%"),
                    b(quota_color(e)),
                )));
            }
            let mine: Vec<&TabSnap> = lm
                .snap
                .tabs
                .iter()
                .filter(|t| t.account == Some(a))
                .collect();
            let w = mine.iter().filter(|t| t.state == TabState::Working).count();
            lines.push(Line::from(Span::styled(
                format!("{} tabs, {w} working", mine.len()),
                pl(TEXT),
            )));
            lines.push(Line::from(Span::styled(
                if ac.pane.is_some() {
                    "click to focus its pane"
                } else {
                    "no pane shows it"
                },
                pl(MUTE),
            )));
        }
        NodeId::Core => {
            let (i, o) = lm.totals();
            lines.push(Line::from(Span::styled(
                "GodTerm",
                b(hsv(lm.t * 0.045, 0.6, 1.0)),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "{} accounts, {} tabs",
                    lm.snap.accounts.len(),
                    lm.snap.tabs.len()
                ),
                pl(TEXT),
            )));
            lines.push(Line::from(vec![
                Span::styled(format!("in {}/min  ", human(i * 60.0)), pl(IN_HOT)),
                Span::styled(format!("out {}/min", human(o * 60.0)), pl(OUT_HOT)),
            ]));
            lines.push(Line::from(Span::styled(
                format!(
                    "{} particles, {:.1} ms a frame",
                    lm.sim.particles.len(),
                    lm.render_ms
                ),
                pl(MUTE),
            )));
        }
        NodeId::Brain => {
            lines.push(Line::from(Span::styled("assistant", b(BRAIN))));
            lines.push(Line::from(Span::styled(
                if lm.snap.brain_busy {
                    "thinking"
                } else {
                    "ready"
                },
                pl(TEXT),
            )));
            lines.push(Line::from(Span::styled(
                format!("{} turns in this brain", lm.snap.brain_turns),
                pl(MUTE),
            )));
            if let Some(c) = lm.snap.brain_cost {
                lines.push(Line::from(Span::styled(
                    format!("last turn ${c:.4}"),
                    pl(MUTE),
                )));
            }
            lines.push(Line::from(Span::styled(
                "click to open its panel",
                pl(MUTE),
            )));
        }
        NodeId::Ghost(k) => {
            let Some(g) = lm.snap.ghosts.get(k) else {
                return;
            };
            lines.push(Line::from(Span::styled(g.label.clone(), b(GHOST))));
            lines.push(Line::from(Span::styled(g.cwd.clone(), pl(MUTE))));
            lines.push(Line::from(Span::styled(g.place.clone(), pl(TEXT))));
            lines.push(Line::from(Span::styled(
                if g.busy { "busy" } else { "idle" },
                pl(if g.busy { IN_HOT } else { MUTE }),
            )));
            lines.push(Line::from(Span::styled(
                "bring it here: Sessions, Take over",
                pl(MUTE),
            )));
        }
    }
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(10).min(56) as u16 + 4;
    let h = lines.len() as u16 + 2;
    let mut x = mx + 2;
    if x + w > canvas.x + canvas.width {
        x = mx.saturating_sub(w + 1).max(canvas.x);
    }
    let mut y = my + 1;
    if y + h > canvas.y + canvas.height {
        y = my.saturating_sub(h).max(canvas.y);
    }
    let r = Rect::new(x, y, w.min(canvas.width), h.min(canvas.height));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(pl((120, 130, 200)))
        .style(Style::default().bg(col((18, 20, 38))))
        .padding(ratatui::widgets::Padding::horizontal(1));
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(block), r);
}

/// Under about 80x24: a radial list, still animated.
fn draw_compact(f: &mut Frame, app: &App, area: Rect) {
    let lm = &app.livemap;
    let s = &lm.snap;
    let t = lm.t;
    const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let pl = |c: C| Style::default().fg(col(c));
    let b = |c: C| pl(c).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = vec![];
    let mut title = vec![];
    for (i, ch) in "◆ LIVE MAP".chars().enumerate() {
        title.push(Span::styled(
            ch.to_string(),
            b(hsv(t * 0.08 + i as f64 * 0.05, 0.75, 1.0)),
        ));
    }
    let (tin, tout) = lm.totals();
    title.push(Span::styled(format!("  {} tabs  ", s.tabs.len()), pl(TEXT)));
    title.push(Span::styled(
        format!("▼{}/m ", human(tin * 60.0)),
        b(IN_HOT),
    ));
    title.push(Span::styled(
        format!("▲{}/m", human(tout * 60.0)),
        b(OUT_HOT),
    ));
    lines.push(Line::from(title));
    let spin = SPIN[((t * 12.0) as usize) % SPIN.len()];
    let blink = (t * 2.4).fract() < 0.5;
    let mut groups: Vec<(Option<usize>, Vec<&TabSnap>)> = (0..s.accounts.len())
        .map(|i| {
            (
                Some(i),
                s.tabs.iter().filter(|x| x.account == Some(i)).collect(),
            )
        })
        .collect();
    let loose: Vec<&TabSnap> = s.tabs.iter().filter(|x| x.account.is_none()).collect();
    if !loose.is_empty() {
        groups.push((None, loose));
    }
    for (a, tabs) in groups {
        let (name, c, left, grok) = match a.and_then(|i| s.accounts.get(i)) {
            Some(ac) => (ac.label.clone(), vivid(ac.color), ac.eff_left, ac.grok),
            None => ("no account".into(), TEXT, None, false),
        };
        let mut row = vec![
            Span::styled(if grok { "⬢ " } else { "◉ " }, b(c)),
            Span::styled(truncate(&name, 18), b(c)),
        ];
        if let Some(l) = left {
            let full = (l / 20.0).round() as usize;
            row.push(Span::styled("  ", pl(TEXT)));
            row.push(Span::styled("▰".repeat(full.min(5)), pl(quota_color(l))));
            row.push(Span::styled("▱".repeat(5 - full.min(5)), pl((60, 64, 90))));
            row.push(Span::styled(format!(" {l:.0}%"), b(quota_color(l))));
        }
        lines.push(Line::from(row));
        let n = tabs.len();
        for (j, tb) in tabs.into_iter().enumerate() {
            let branch = if j + 1 == n { "  └─" } else { "  ├─" };
            let (g, gc) = match tb.state {
                TabState::Working => (spin, IN_HOT),
                TabState::Waiting => ('◉', if blink { WAIT_A } else { WAIT_B }),
                TabState::Background => ('✺', SUB),
                TabState::Idle => ('●', scale(c, 0.6)),
                TabState::Starting => (spin, c),
                TabState::Suspended => ('○', PAUSED),
                TabState::Exited => ('·', PAUSED),
            };
            let r = lm.rates.get(&tb.uid).cloned().unwrap_or_default();
            let mut row = vec![
                Span::styled(branch, pl(scale(c, 0.5))),
                Span::styled(format!("{g} "), b(gc)),
                Span::styled(truncate(&tb.name, 18), pl(TEXT)),
                Span::styled(format!("  {}", tb.state.label()), pl(MUTE)),
            ];
            if r.total() > 0.0 {
                row.push(Span::styled(
                    format!("  ▼{}/s", human(r.ema_in)),
                    pl(IN_HOT),
                ));
                row.push(Span::styled(
                    format!(" ▲{}/s", human(r.ema_out)),
                    pl(OUT_HOT),
                ));
            }
            lines.push(Line::from(row));
        }
    }
    if let Some(e) = lm.ticker.back() {
        lines.push(Line::from(vec![
            Span::styled("⟫ ", b(tick_color(&e.kind))),
            Span::styled(e.text.clone(), pl(TEXT)),
        ]));
    }
    let bg = Style::default().bg(col((8, 9, 20)));
    f.render_widget(Paragraph::new(lines).style(bg), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_stay_in_range() {
        for h in 0..20 {
            let c = hsv(h as f64 / 20.0, 0.8, 1.0);
            assert!(c.0 > 0 || c.1 > 0 || c.2 > 0);
        }
        assert_eq!(
            vivid((138, 160, 128)).1,
            255,
            "turned up to full brightness"
        );
        assert!(quota_color(100.0).1 > 200 && quota_color(0.0).0 > 200);
        let sp = spark(&[0.0, 5.0, 10.0], 5);
        assert_eq!(sp.len(), 5);
        assert_eq!(sp[4].0, '█');
        assert_eq!(tail("/a/b/c/d", 5), "…/c/d");
        assert_eq!(tail("/a", 5), "/a");
    }
}
