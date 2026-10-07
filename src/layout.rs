//! Pane layout: how N panes are arranged in the main area, with paging
//! when they do not fit at a usable size (at least MIN_W x MIN_H each).

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MIN_W: u16 = 40;
pub const MIN_H: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 1, 2 side by side, 3 = 2 + 1, 4 = 2x2, 5-6 = 3x2, 7-9 = 3x3.
    Auto,
    /// A fixed grid, columns x rows.
    Grid(usize, usize),
    Columns,
    Rows,
    /// The focused pane large, the others stacked beside it.
    Focus,
}

pub const MODES: &[&str] = &["auto", "grid", "columns", "rows", "focus"];

pub fn parse_mode(layout: &str, grid: &str) -> Mode {
    match layout {
        "grid" => {
            let (c, r) = parse_grid(grid).unwrap_or((2, 2));
            Mode::Grid(c, r)
        }
        "columns" => Mode::Columns,
        "rows" => Mode::Rows,
        "focus" => Mode::Focus,
        _ => Mode::Auto,
    }
}

/// "3x2" -> (3 columns, 2 rows).
pub fn parse_grid(s: &str) -> Option<(usize, usize)> {
    let (c, r) = s
        .trim()
        .to_lowercase()
        .split_once('x')
        .map(|(a, b)| (a.to_string(), b.to_string()))?;
    let (c, r) = (c.trim().parse().ok()?, r.trim().parse().ok()?);
    ((1..=8).contains(&c) && (1..=8).contains(&r)).then_some((c, r))
}

pub fn next_mode(cur: &str) -> &'static str {
    let i = MODES.iter().position(|m| *m == cur).unwrap_or(0);
    MODES[(i + 1) % MODES.len()]
}

/// Column widths and row heights a user dragged, per shape ("3x2").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ratios {
    pub cols: Vec<f32>,
    pub rows: Vec<f32>,
}

pub type RatioMap = BTreeMap<String, Ratios>;

/// Where pane `index` (into the list of visible panes) goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub index: usize,
    pub page: usize,
    pub rect: Rect,
}

/// A border between two columns or rows that can be dragged.
#[derive(Debug, Clone, PartialEq)]
pub struct Border {
    /// true: between columns (drag left / right).
    pub vertical: bool,
    /// Between track `i` and `i + 1`.
    pub i: usize,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arranged {
    pub placed: Vec<Placed>,
    pub pages: usize,
    pub per_page: usize,
    /// Shape key of the current page ("3x2", "2+1"), for ratios.
    pub shape: String,
    pub borders: Vec<Border>,
    /// Cells of a fixed grid on the current page with no pane in them.
    pub empty: Vec<Rect>,
}

fn auto_shape(n: usize) -> (usize, usize) {
    match n {
        0 | 1 => (1, 1),
        2 => (2, 1),
        3 | 4 => (2, 2),
        5 | 6 => (3, 2),
        _ => (3, 3),
    }
}

/// Split `len` into tracks by `weights` (equal when they do not fit).
fn tracks(start: u16, len: u16, n: usize, weights: Option<&Vec<f32>>) -> Vec<(u16, u16)> {
    let n = n.max(1);
    let w: Vec<f32> = match weights {
        Some(w) if w.len() == n && w.iter().all(|x| *x > 0.0) => w.clone(),
        _ => vec![1.0; n],
    };
    let total: f32 = w.iter().sum();
    let mut out = Vec::with_capacity(n);
    let mut pos = start;
    let mut acc = 0f32;
    for (i, wi) in w.iter().enumerate() {
        acc += wi;
        let end = if i + 1 == n {
            start + len
        } else {
            start + ((len as f32) * acc / total).round() as u16
        };
        out.push((pos, end.saturating_sub(pos)));
        pos = end;
    }
    out
}

/// Lay out `n` visible panes in `area`. `focus` is an index into them;
/// the page shown is the one holding it.
pub fn arrange(n: usize, area: Rect, mode: Mode, focus: usize, ratios: &RatioMap) -> Arranged {
    let max_c = ((area.width / MIN_W) as usize).max(1);
    let max_r = ((area.height / MIN_H) as usize).max(1);
    if n == 0 {
        return Arranged {
            placed: vec![],
            pages: 1,
            per_page: 1,
            shape: "0".into(),
            borders: vec![],
            empty: vec![],
        };
    }
    if mode == Mode::Focus {
        return arrange_focus(n, area, focus.min(n - 1), max_r, ratios);
    }
    // Capacity of a page.
    let (c, r) = match mode {
        Mode::Grid(c, r) => (c.min(max_c), r.min(max_r)),
        Mode::Columns => (n.min(max_c), 1),
        Mode::Rows => (1, n.min(max_r)),
        _ => {
            let (c, r) = auto_shape(n);
            (c.min(max_c), r.min(max_r))
        }
    };
    let per_page = (c * r).max(1);
    let pages = n.div_ceil(per_page);
    let mut placed = vec![];
    let mut borders = vec![];
    let mut empty = vec![];
    let mut shape = String::new();
    let cur_page = focus.min(n - 1) / per_page;
    for page in 0..pages {
        let first = page * per_page;
        let k = (n - first).min(per_page);
        // A partly filled page of an automatic layout gets the shape for
        // its own count; fixed grids keep their cells.
        let (pc, pr) = match mode {
            Mode::Auto => {
                let (a, b) = auto_shape(k);
                (a.min(c), b.min(r))
            }
            Mode::Columns => (k, 1),
            Mode::Rows => (1, k),
            _ => (c, r),
        };
        let three = mode == Mode::Auto && k == 3 && pc == 2 && pr == 2;
        let key = if three {
            "2+1".to_string()
        } else {
            format!("{pc}x{pr}")
        };
        let rt = ratios.get(&key);
        let rows = tracks(area.y, area.height, pr, rt.map(|x| &x.rows));
        let cols = tracks(area.x, area.width, pc, rt.map(|x| &x.cols));
        for j in 0..k {
            let (ci, ri) = (j % pc, j / pc);
            let (y, h) = rows[ri.min(rows.len() - 1)];
            let (x, w) = if three && ri == 1 {
                (area.x, area.width)
            } else {
                cols[ci]
            };
            placed.push(Placed {
                index: first + j,
                page,
                rect: Rect::new(x, y, w, h),
            });
        }
        if page == cur_page {
            for j in (k..pc * pr).filter(|_| !three) {
                let (ci, ri) = (j % pc, j / pc);
                let (y, h) = rows[ri.min(rows.len() - 1)];
                let (x, w) = cols[ci.min(cols.len() - 1)];
                empty.push(Rect::new(x, y, w, h));
            }
            shape = key;
            for (i, (x, w)) in cols.iter().enumerate().take(pc.saturating_sub(1)) {
                let rows_span = if three {
                    rows[0]
                } else {
                    (area.y, area.height)
                };
                borders.push(Border {
                    vertical: true,
                    i,
                    rect: Rect::new((x + w).saturating_sub(1), rows_span.0, 2, rows_span.1),
                });
            }
            for (i, (y, h)) in rows.iter().enumerate().take(pr.saturating_sub(1)) {
                borders.push(Border {
                    vertical: false,
                    i,
                    // One row: the upper pane's bottom border. The row
                    // under it is the lower pane's header (its account ▾,
                    // tabs, ×), which must stay clickable.
                    rect: Rect::new(area.x, (y + h).saturating_sub(1), area.width, 1),
                });
            }
        }
    }
    Arranged {
        placed,
        pages,
        per_page,
        shape,
        borders,
        empty,
    }
}

fn arrange_focus(n: usize, area: Rect, focus: usize, max_r: usize, ratios: &RatioMap) -> Arranged {
    let others: Vec<usize> = (0..n).filter(|i| *i != focus).collect();
    let side = !others.is_empty() && area.width >= MIN_W * 2;
    let key = "focus".to_string();
    let rt = ratios.get(&key);
    let cols = if side {
        tracks(
            area.x,
            area.width,
            2,
            rt.map(|x| &x.cols).or(Some(&vec![2.0, 1.0])),
        )
    } else {
        vec![(area.x, area.width)]
    };
    let mut placed = vec![Placed {
        index: focus,
        page: 0,
        rect: Rect::new(cols[0].0, area.y, cols[0].1, area.height),
    }];
    let mut borders = vec![];
    if side {
        let shown = others.len().min(max_r);
        let rows = tracks(area.y, area.height, shown, None);
        for (k, i) in others.iter().take(shown).enumerate() {
            placed.push(Placed {
                index: *i,
                page: 0,
                rect: Rect::new(cols[1].0, rows[k].0, cols[1].1, rows[k].1),
            });
        }
        borders.push(Border {
            vertical: true,
            i: 0,
            rect: Rect::new(
                (cols[0].0 + cols[0].1).saturating_sub(1),
                area.y,
                2,
                area.height,
            ),
        });
    }
    // Panes that do not fit are reachable by number; they are not paged.
    Arranged {
        placed,
        pages: 1,
        per_page: n,
        shape: key,
        borders,
        empty: vec![],
    }
}

/// New weights after dragging border `i` to `pos` (a column or row
/// coordinate) in a track list of `n` covering `start..start + len`.
pub fn drag(weights: &[f32], n: usize, start: u16, len: u16, i: usize, pos: u16) -> Vec<f32> {
    let mut w: Vec<f32> = if weights.len() == n {
        weights.to_vec()
    } else {
        vec![1.0; n]
    };
    if i + 1 >= n || len == 0 {
        return w;
    }
    let total: f32 = w.iter().sum();
    let before: f32 = w[..i].iter().sum();
    let pair = w[i] + w[i + 1];
    let min_frac = (MIN_W.min(MIN_H) as f32 / 2.0) / len as f32 * total;
    let at = ((pos.saturating_sub(start)) as f32 / len as f32 * total - before)
        .clamp(min_frac, pair - min_frac);
    w[i] = at;
    w[i + 1] = pair - at;
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlap(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    #[test]
    fn auto_shapes() {
        let area = Rect::new(0, 1, 200, 50);
        let none = RatioMap::new();
        let shape = |n| arrange(n, area, Mode::Auto, 0, &none).shape;
        assert_eq!(shape(1), "1x1");
        assert_eq!(shape(2), "2x1");
        assert_eq!(shape(3), "2+1");
        assert_eq!(shape(4), "2x2");
        assert_eq!(shape(5), "3x2");
        assert_eq!(shape(6), "3x2");
        assert_eq!(shape(7), "3x3");
        assert_eq!(shape(9), "3x3");
        // 2 + 1: the third pane spans the bottom.
        let a = arrange(3, area, Mode::Auto, 0, &none);
        assert_eq!(a.placed[2].rect.width, 200);
        assert_eq!(a.placed[0].rect.width + a.placed[1].rect.width, 200);
    }

    #[test]
    fn every_count_fits_without_overlap() {
        let none = RatioMap::new();
        for (w, h) in [(200u16, 50u16), (120, 40), (80, 24), (300, 80)] {
            let area = Rect::new(0, 1, w, h);
            for mode in [
                Mode::Auto,
                Mode::Grid(3, 2),
                Mode::Columns,
                Mode::Rows,
                Mode::Focus,
            ] {
                for n in 1..=16 {
                    let a = arrange(n, area, mode, 0, &none);
                    if mode != Mode::Focus {
                        assert_eq!(a.placed.len(), n, "{mode:?} {n} in {w}x{h}");
                        let mut idx: Vec<usize> = a.placed.iter().map(|p| p.index).collect();
                        idx.sort();
                        assert_eq!(idx, (0..n).collect::<Vec<_>>());
                        assert_eq!(a.pages, n.div_ceil(a.per_page));
                    }
                    for p in &a.placed {
                        let r = p.rect;
                        assert!(
                            r.x >= area.x && r.right() <= area.right(),
                            "{mode:?} {n} {r:?}"
                        );
                        assert!(
                            r.y >= area.y && r.bottom() <= area.bottom(),
                            "{mode:?} {n} {r:?}"
                        );
                        // Big enough, unless the whole area is smaller.
                        if mode != Mode::Focus {
                            assert!(
                                r.width >= MIN_W.min(w) && r.height >= MIN_H.min(h),
                                "{mode:?} {n} {r:?} in {w}x{h}"
                            );
                        }
                    }
                    for page in 0..a.pages {
                        let on: Vec<Rect> = a
                            .placed
                            .iter()
                            .filter(|p| p.page == page)
                            .map(|p| p.rect)
                            .collect();
                        for i in 0..on.len() {
                            for j in i + 1..on.len() {
                                assert!(
                                    !overlap(on[i], on[j]),
                                    "{mode:?} {n} page {page}: {:?} {:?}",
                                    on[i],
                                    on[j]
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn paging() {
        let none = RatioMap::new();
        let area = Rect::new(0, 0, 200, 50);
        // 3x3 per page: 12 panes take two pages; the second has a 2x2.
        let a = arrange(12, area, Mode::Auto, 0, &none);
        assert_eq!((a.pages, a.per_page), (2, 9));
        assert_eq!(a.placed.iter().filter(|p| p.page == 1).count(), 3);
        assert_eq!(arrange(12, area, Mode::Auto, 10, &none).shape, "2+1");
        // A small terminal pages sooner: 80x24 holds 2x2.
        let a = arrange(5, Rect::new(0, 0, 80, 24), Mode::Auto, 0, &none);
        assert_eq!((a.pages, a.per_page), (2, 4));
        // Columns: as many as fit side by side.
        let a = arrange(6, area, Mode::Columns, 0, &none);
        assert_eq!((a.per_page, a.pages), (5, 2));
    }

    #[test]
    fn focus_mode() {
        let none = RatioMap::new();
        let area = Rect::new(0, 0, 200, 50);
        let a = arrange(4, area, Mode::Focus, 2, &none);
        assert_eq!(a.placed[0].index, 2);
        assert!(a.placed[0].rect.width > 120);
        assert_eq!(a.placed.len(), 4);
        // Too narrow for a side column: just the focused pane.
        let a = arrange(4, Rect::new(0, 0, 70, 30), Mode::Focus, 1, &none);
        assert_eq!(a.placed.len(), 1);
    }

    #[test]
    fn grid_parsing_and_cycling() {
        assert_eq!(parse_grid("3x2"), Some((3, 2)));
        assert_eq!(parse_grid(" 2 X 4 "), Some((2, 4)));
        assert_eq!(parse_grid("0x2"), None);
        assert_eq!(parse_grid("abc"), None);
        assert_eq!(parse_mode("grid", "4x1"), Mode::Grid(4, 1));
        assert_eq!(parse_mode("grid", "bad"), Mode::Grid(2, 2));
        assert_eq!(parse_mode("nonsense", ""), Mode::Auto);
        assert_eq!(next_mode("auto"), "grid");
        assert_eq!(next_mode("focus"), "auto");
    }

    #[test]
    fn dragging_borders() {
        let mut map = RatioMap::new();
        let area = Rect::new(0, 0, 200, 50);
        let w = drag(&[], 2, 0, 200, 0, 140);
        assert!((w[0] / (w[0] + w[1]) - 0.7).abs() < 0.01, "{w:?}");
        map.insert(
            "2x1".into(),
            Ratios {
                cols: w,
                rows: vec![],
            },
        );
        let a = arrange(2, area, Mode::Auto, 0, &map);
        assert_eq!(a.placed[0].rect.width, 140);
        assert_eq!(a.borders.len(), 1);
        // Never smaller than half the minimum.
        let w = drag(&[1.0, 1.0], 2, 0, 200, 0, 1);
        assert!(w[0] / (w[0] + w[1]) * 200.0 >= 4.9, "{w:?}");
        // Out of range border: unchanged.
        assert_eq!(drag(&[1.0, 1.0], 2, 0, 200, 1, 50), vec![1.0, 1.0]);
    }
}
