//! Remembering the terminal window's size and position across launches,
//! including when the monitor it was on is gone.
//!
//! Coordinates are AppleScript style: origin at the top left of the main
//! screen, y growing down, other monitors possibly negative. NSScreen frames
//! (bottom left origin, y up) are flipped into that space.

use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn area(&self) -> f64 {
        self.w.max(0.0) * self.h.max(0.0)
    }
    pub fn intersect(&self, o: &Rect) -> f64 {
        let w = (self.x + self.w).min(o.x + o.w) - self.x.max(o.x);
        let h = (self.y + self.h).min(o.y + o.h) - self.y.max(o.y);
        if w > 0.0 && h > 0.0 {
            w * h
        } else {
            0.0
        }
    }
    pub fn valid(&self) -> bool {
        self.w >= 100.0
            && self.h >= 60.0
            && [self.x, self.y, self.w, self.h]
                .iter()
                .all(|v| v.is_finite())
    }
    /// AppleScript bounds: left, top, right, bottom.
    pub fn bounds(&self) -> [i64; 4] {
        [
            self.x.round() as i64,
            self.y.round() as i64,
            (self.x + self.w).round() as i64,
            (self.y + self.h).round() as i64,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    /// NSScreenNumber, stable while the display stays connected.
    pub id: Option<String>,
    pub frame: Rect,
    /// Frame minus menu bar and dock.
    pub visible: Rect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SavedWindow {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub screen_id: Option<String>,
    #[serde(default)]
    pub screen_frame: Option<Rect>,
}

impl SavedWindow {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, self.h)
    }
}

/// Flip an NSScreen rect (bottom left origin) into top left coordinates,
/// given the height of the main screen.
pub fn flip(ns: Rect, main_h: f64) -> Rect {
    Rect::new(ns.x, main_h - (ns.y + ns.h), ns.w, ns.h)
}

fn centered_in(size: (f64, f64), area: &Rect) -> Rect {
    let w = size.0.min(area.w);
    let h = size.1.min(area.h);
    Rect::new(
        area.x + (area.w - w) / 2.0,
        area.y + (area.h - h) / 2.0,
        w,
        h,
    )
}

/// About 80% of the main screen, centered.
pub fn default_rect(screens: &[Screen]) -> Option<Rect> {
    let main = screens.first()?;
    Some(centered_in(
        (main.visible.w * 0.8, main.visible.h * 0.8),
        &main.visible,
    ))
}

/// Where the window should go. `screens[0]` is the main screen.
pub fn place(saved: Option<&SavedWindow>, screens: &[Screen]) -> Option<Rect> {
    let main = screens.first()?;
    let Some(s) = saved else {
        return default_rect(screens);
    };
    let r = s.rect();
    if !r.valid()
        || screens
            .iter()
            .all(|sc| r.w > sc.frame.w || r.h > sc.frame.h)
    {
        return default_rect(screens);
    }
    // The display it was on: by id, else by an identical frame.
    let same = screens.iter().find(|sc| {
        (s.screen_id.is_some() && sc.id == s.screen_id)
            || s.screen_frame.is_some_and(|f| f == sc.frame)
    });
    let on_screens: f64 = screens.iter().map(|sc| r.intersect(&sc.frame)).sum();
    let mostly_visible = on_screens >= r.area() * 0.5;
    if same.is_some() && mostly_visible {
        return Some(r);
    }
    // Moved, gone, or mostly off screen: same size on the main screen.
    let size = (r.w.min(main.visible.w), r.h.min(main.visible.h));
    Some(centered_in(size, &main.visible))
}

pub(crate) fn osascript(lang_js: bool, script: &str, timeout: Duration) -> Option<String> {
    let mut cmd = Command::new("osascript");
    if lang_js {
        cmd.args(["-l", "JavaScript"]);
    }
    cmd.args(["-e", script]);
    let out = crate::creds::output_with_timeout(cmd, timeout)?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Current screens, main first, in top left coordinates.
pub fn query_screens() -> Vec<Screen> {
    let js = r#"ObjC.import('AppKit');
var s = $.NSScreen.screens; var out = [];
for (var i = 0; i < s.count; i++) {
  var sc = s.objectAtIndex(i); var f = sc.frame; var v = sc.visibleFrame;
  var id = ObjC.unwrap(sc.deviceDescription.objectForKey('NSScreenNumber'));
  out.push([id, f.origin.x, f.origin.y, f.size.width, f.size.height, v.origin.x, v.origin.y, v.size.width, v.size.height].join(' '));
}
out.join('\n')"#;
    let Some(text) = osascript(true, js, Duration::from_secs(3)) else {
        return vec![];
    };
    parse_screens(&text)
}

/// Parse the JXA output: `id fx fy fw fh vx vy vw vh` per line (NSScreen
/// coordinates), main screen first.
pub fn parse_screens(text: &str) -> Vec<Screen> {
    let rows: Vec<Vec<f64>> = text
        .lines()
        .map(|l| {
            l.split_whitespace()
                .filter_map(|n| n.parse::<f64>().ok())
                .collect::<Vec<f64>>()
        })
        .filter(|v| v.len() == 9)
        .collect();
    let Some(main_h) = rows.first().map(|r| r[4]) else {
        return vec![];
    };
    rows.iter()
        .map(|r| Screen {
            id: Some(format!("{}", r[0] as i64)),
            frame: flip(Rect::new(r[1], r[2], r[3], r[4]), main_h),
            visible: flip(Rect::new(r[5], r[6], r[7], r[8]), main_h),
        })
        .collect()
}

/// The terminal app hosting this process, when it is one we can script.
pub fn terminal_app() -> Option<&'static str> {
    if std::env::var_os("TMUX").is_some() {
        return None;
    }
    match std::env::var("TERM_PROGRAM").ok()?.as_str() {
        "iTerm.app" => Some("iTerm"),
        "Apple_Terminal" => Some("Terminal"),
        _ => None,
    }
}

/// Our controlling terminal, e.g. /dev/ttys012.
pub fn own_tty() -> Option<String> {
    crate::platform::own_tty()
}

/// Bounds of the window whose session uses `tty`.
pub fn query_window(app: &str, tty: &str) -> Option<Rect> {
    let script = match app {
        "iTerm" => format!(
            r#"tell application "iTerm"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if tty of s is "{tty}" then return bounds of w
      end repeat
    end repeat
  end repeat
end tell"#
        ),
        _ => format!(
            r#"tell application "Terminal"
  repeat with w in windows
    repeat with t in tabs of w
      if tty of t is "{tty}" then return bounds of w
    end repeat
  end repeat
end tell"#
        ),
    };
    let out = osascript(false, &script, Duration::from_secs(3))?;
    parse_bounds(&out)
}

/// "left, top, right, bottom" into a rect.
pub fn parse_bounds(s: &str) -> Option<Rect> {
    let v: Vec<f64> = s.split(',').filter_map(|n| n.trim().parse().ok()).collect();
    (v.len() == 4 && v[2] > v[0] && v[3] > v[1])
        .then(|| Rect::new(v[0], v[1], v[2] - v[0], v[3] - v[1]))
}

/// The window now, with the screen it is mostly on.
pub fn capture() -> Option<SavedWindow> {
    let app = terminal_app()?;
    let tty = own_tty()?;
    let r = query_window(app, &tty)?;
    let screens = query_screens();
    let on = screens.iter().max_by(|a, b| {
        r.intersect(&a.frame)
            .partial_cmp(&r.intersect(&b.frame))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Some(SavedWindow {
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
        screen_id: on.and_then(|s| s.id.clone()),
        screen_frame: on.map(|s| s.frame),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(id: &str, x: f64, y: f64, w: f64, h: f64) -> Screen {
        Screen {
            id: Some(id.into()),
            frame: Rect::new(x, y, w, h),
            visible: Rect::new(x, y + 25.0, w, h - 25.0 - 60.0),
        }
    }

    fn saved(x: f64, y: f64, w: f64, h: f64, id: Option<&str>, frame: Option<Rect>) -> SavedWindow {
        SavedWindow {
            x,
            y,
            w,
            h,
            screen_id: id.map(str::to_string),
            screen_frame: frame,
        }
    }

    #[test]
    fn same_display_restores_exactly() {
        let scr = vec![
            screen("1", 0.0, 0.0, 1728.0, 1117.0),
            screen("2", 1728.0, -300.0, 2560.0, 1440.0),
        ];
        let s = saved(1900.0, 0.0, 1400.0, 900.0, Some("2"), None);
        assert_eq!(
            place(Some(&s), &scr),
            Some(Rect::new(1900.0, 0.0, 1400.0, 900.0))
        );
        // Matched by an identical frame when the id changed.
        let s2 = saved(
            1900.0,
            0.0,
            1400.0,
            900.0,
            Some("99"),
            Some(Rect::new(1728.0, -300.0, 2560.0, 1440.0)),
        );
        assert_eq!(place(Some(&s2), &scr), Some(s2.rect()));
    }

    #[test]
    fn missing_monitor_moves_to_main_and_clamps() {
        let scr = vec![screen("1", 0.0, 0.0, 1440.0, 900.0)];
        let s = saved(2000.0, 100.0, 2200.0, 1200.0, Some("2"), None);
        let r = place(Some(&s), &scr).unwrap();
        let vis = scr[0].visible;
        assert!(r.w <= vis.w && r.h <= vis.h, "clamped to the visible frame");
        assert!(
            r.x >= vis.x
                && r.y >= vis.y
                && r.x + r.w <= vis.x + vis.w + 0.1
                && r.y + r.h <= vis.y + vis.h + 0.1
        );
        // Centered.
        assert!(((r.x + r.w / 2.0) - (vis.x + vis.w / 2.0)).abs() < 1.0);
    }

    #[test]
    fn mostly_offscreen_is_recentered() {
        let scr = vec![screen("1", 0.0, 0.0, 1440.0, 900.0)];
        // Same display, but 80% hangs off the right edge.
        let s = saved(1300.0, 100.0, 700.0, 500.0, Some("1"), None);
        let r = place(Some(&s), &scr).unwrap();
        assert_eq!((r.w, r.h), (700.0, 500.0));
        assert!(r.x + r.w <= 1440.0);
    }

    #[test]
    fn arrangement_change_and_negative_left_monitor() {
        // A monitor on the left has negative x; the window was there.
        let scr = vec![
            screen("1", 0.0, 0.0, 1728.0, 1117.0),
            screen("3", -1920.0, 0.0, 1920.0, 1080.0),
        ];
        let s = saved(-1700.0, 100.0, 1200.0, 800.0, Some("3"), None);
        assert_eq!(place(Some(&s), &scr), Some(s.rect()));
        // The same monitor moved to the right: id matches but the old rect
        // is now off screen, so it is recentered on the main screen.
        let moved = vec![
            screen("1", 0.0, 0.0, 1728.0, 1117.0),
            screen("3", 1728.0, 0.0, 1920.0, 1080.0),
        ];
        let r = place(Some(&s), &moved).unwrap();
        assert!(r.x >= 0.0 && r.x + r.w <= 1728.0);
    }

    #[test]
    fn too_large_or_corrupt_falls_back_to_default() {
        let scr = vec![screen("1", 0.0, 0.0, 1440.0, 900.0)];
        let def = default_rect(&scr).unwrap();
        assert!((def.w - scr[0].visible.w * 0.8).abs() < 0.01);
        assert_eq!(
            place(
                Some(&saved(0.0, 0.0, 5000.0, 4000.0, Some("1"), None)),
                &scr
            ),
            Some(def)
        );
        assert_eq!(
            place(Some(&saved(f64::NAN, 0.0, 800.0, 600.0, None, None)), &scr),
            Some(def)
        );
        assert_eq!(
            place(Some(&saved(0.0, 0.0, 10.0, 10.0, None, None)), &scr),
            Some(def)
        );
        assert_eq!(place(None, &scr), Some(def));
        assert_eq!(place(None, &[]), None);
        assert!(serde_json::from_str::<SavedWindow>("{\"x\":\"bad\"}").is_err());
    }

    #[test]
    fn flips_and_parses() {
        // A 1000 tall main screen; a window 100 from the bottom, 200 tall.
        assert_eq!(
            flip(Rect::new(10.0, 100.0, 300.0, 200.0), 1000.0),
            Rect::new(10.0, 700.0, 300.0, 200.0)
        );
        let scr = parse_screens(
            "1 0 0 1728 1117 0 0 1728 1085\n2 -1920 37 1920 1080 -1920 37 1920 1055\nbad line",
        );
        assert_eq!(scr.len(), 2);
        assert_eq!(scr[0].frame, Rect::new(0.0, 0.0, 1728.0, 1117.0));
        assert_eq!(scr[1].frame, Rect::new(-1920.0, 0.0, 1920.0, 1080.0));
        assert_eq!(scr[1].id.as_deref(), Some("2"));
        assert_eq!(
            parse_bounds("10, 20, 810, 620"),
            Some(Rect::new(10.0, 20.0, 800.0, 600.0))
        );
        assert_eq!(parse_bounds("garbage"), None);
        assert_eq!(Rect::new(1.4, 2.6, 10.0, 10.0).bounds(), [1, 3, 11, 13]);
    }
}
