use ratatui::layout::Rect;
use serde_json::{json, Value};

pub const MIN_W: u16 = 20;
pub const MIN_H: u16 = 6;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Split {
        columns: bool,
        sizes: Vec<f32>,
        children: Vec<Node>,
    },

    Account(usize),

    Rest,
}

pub fn parse(v: &Value, account: &dyn Fn(&Value) -> Option<usize>) -> Result<Node, String> {
    if let Some(a) = v.get("account") {
        return account(a)
            .map(Node::Account)
            .ok_or_else(|| format!("no account {a}"));
    }
    if v.get("rest").is_some() {
        return Ok(Node::Rest);
    }
    let dir = v.get("split").and_then(Value::as_str).ok_or(
        "each part is a split (\"columns\" or \"rows\" with children), an account, or the rest",
    )?;
    let columns = match dir {
        "columns" | "horizontal" | "side by side" | "cols" => true,
        "rows" | "vertical" | "stacked" => false,
        d => return Err(format!("split is \"columns\" or \"rows\", not {d}")),
    };
    let kids = v
        .get("children")
        .and_then(Value::as_array)
        .filter(|k| !k.is_empty())
        .ok_or("a split needs children")?;
    let children = kids
        .iter()
        .map(|k| parse(k, account))
        .collect::<Result<Vec<_>, _>>()?;
    let sizes: Vec<f32> = v
        .get("sizes")
        .and_then(Value::as_array)
        .map(|s| {
            s.iter()
                .filter_map(Value::as_f64)
                .map(|x| x as f32)
                .collect()
        })
        .filter(|s: &Vec<f32>| s.len() == children.len() && s.iter().all(|x| *x > 0.0))
        .unwrap_or_else(|| vec![1.0; children.len()]);
    Ok(Node::Split {
        columns,
        sizes,
        children,
    })
}

pub fn to_json(n: &Node) -> Value {
    match n {
        Node::Account(a) => json!({"account": a + 1}),
        Node::Rest => json!({"rest": true}),
        Node::Split {
            columns,
            sizes,
            children,
        } => json!({
            "split": if *columns { "columns" } else { "rows" },
            "sizes": sizes,
            "children": children.iter().map(to_json).collect::<Vec<_>>(),
        }),
    }
}

pub fn describe(n: &Node) -> String {
    match n {
        Node::Account(a) => format!("a{}", a + 1),
        Node::Rest => "rest".into(),
        Node::Split {
            columns, children, ..
        } => {
            let inner: Vec<String> = children
                .iter()
                .map(|c| match c {
                    Node::Split { .. } => format!("({})", describe(c)),
                    _ => describe(c),
                })
                .collect();
            inner.join(if *columns { " | " } else { " / " })
        }
    }
}

fn mentioned(n: &Node, out: &mut Vec<usize>) {
    match n {
        Node::Account(a) => out.push(*a),
        Node::Rest => {}
        Node::Split { children, .. } => children.iter().for_each(|c| mentioned(c, out)),
    }
}

fn split(start: u16, len: u16, sizes: &[f32]) -> Vec<(u16, u16)> {
    let total: f32 = sizes.iter().sum::<f32>().max(f32::EPSILON);
    let mut out = vec![];
    let mut acc = 0f32;
    let mut pos = start;
    for (i, s) in sizes.iter().enumerate() {
        acc += s;
        let end = if i + 1 == sizes.len() {
            start + len
        } else {
            start + (len as f32 * acc / total).round() as u16
        };
        out.push((pos, end.saturating_sub(pos)));
        pos = end;
    }
    out
}

pub fn arrange(tree: &Node, area: Rect, open: &[Option<usize>]) -> Option<Vec<(usize, Rect)>> {
    let mut named = vec![];
    mentioned(tree, &mut named);
    let rest: Vec<usize> = (0..open.len())
        .filter(|&i| open[i].is_none_or(|a| !named.contains(&a)))
        .collect();
    let mut out = vec![];
    let mut used = vec![];
    place(tree, area, open, &rest, true, &mut used, &mut out)?;
    (!out.is_empty()).then_some(out)
}

fn covers(n: &Node, open: &[Option<usize>], rest: &[usize]) -> bool {
    match n {
        Node::Account(a) => open.contains(&Some(*a)),
        Node::Rest => !rest.is_empty(),
        Node::Split { children, .. } => children.iter().any(|c| covers(c, open, rest)),
    }
}

fn place(
    n: &Node,
    area: Rect,
    open: &[Option<usize>],
    rest: &[usize],
    parent_columns: bool,
    used: &mut Vec<usize>,
    out: &mut Vec<(usize, Rect)>,
) -> Option<()> {
    if area.width < MIN_W || area.height < MIN_H {
        return None;
    }
    match n {
        Node::Account(a) => {
            let i = (0..open.len()).find(|&i| open[i] == Some(*a) && !used.contains(&i))?;
            used.push(i);
            out.push((i, area));
            Some(())
        }
        Node::Rest => {
            let k = rest.len();
            if k == 0 {
                return Some(());
            }
            let parts = if parent_columns {
                split(area.y, area.height, &vec![1.0; k])
                    .into_iter()
                    .map(|(y, h)| Rect::new(area.x, y, area.width, h))
                    .collect::<Vec<_>>()
            } else {
                split(area.x, area.width, &vec![1.0; k])
                    .into_iter()
                    .map(|(x, w)| Rect::new(x, area.y, w, area.height))
                    .collect()
            };
            for (i, r) in rest.iter().zip(parts) {
                if r.width < MIN_W || r.height < MIN_H {
                    return None;
                }
                used.push(*i);
                out.push((*i, r));
            }
            Some(())
        }
        Node::Split {
            columns,
            sizes,
            children,
        } => {
            let live: Vec<(usize, &Node)> = children
                .iter()
                .enumerate()
                .filter(|(_, c)| covers(c, open, rest))
                .collect();
            if live.is_empty() {
                return Some(());
            }
            let w: Vec<f32> = live
                .iter()
                .map(|(i, _)| sizes.get(*i).copied().unwrap_or(1.0))
                .collect();
            let tracks = if *columns {
                split(area.x, area.width, &w)
            } else {
                split(area.y, area.height, &w)
            };
            for ((_, c), (p, l)) in live.iter().zip(tracks) {
                let r = if *columns {
                    Rect::new(p, area.y, l, area.height)
                } else {
                    Rect::new(area.x, p, area.width, l)
                };
                place(c, r, open, rest, *columns, used, out)?;
            }
            Some(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Open,
    Row,
    Col,
}

pub const COL_W: u16 = 4;

#[cfg(test)]
pub fn fold(placed: &[(usize, Rect)], folded: &dyn Fn(usize) -> bool) -> Vec<(usize, Rect)> {
    fold_dirs(placed, &|i| if folded(i) { Fold::Row } else { Fold::Open })
}

pub fn fold_dirs(placed: &[(usize, Rect)], fold_of: &dyn Fn(usize) -> Fold) -> Vec<(usize, Rect)> {
    if placed.is_empty() || placed.iter().all(|(i, _)| fold_of(*i) == Fold::Open) {
        return placed.to_vec();
    }
    let area = bbox(placed);
    if placed.iter().all(|(i, _)| fold_of(*i) == Fold::Row) {
        let mut order = placed.to_vec();
        order.sort_by_key(|(_, r)| (r.y, r.x));
        return order
            .iter()
            .enumerate()
            .map(|(k, (i, _))| {
                let y = area.y + (k as u16).min(area.height.saturating_sub(1));
                (*i, Rect::new(area.x, y, area.width, 1))
            })
            .collect();
    }

    if placed.iter().all(|(i, _)| fold_of(*i) == Fold::Col) {
        let mut order = placed.to_vec();
        order.sort_by_key(|(_, r)| (r.x, r.y));
        return order
            .iter()
            .enumerate()
            .map(|(k, (i, _))| {
                let x = (area.x + k as u16 * COL_W).min(area.x + area.width.saturating_sub(1));
                let w = COL_W.min(area.x + area.width - x);
                (*i, Rect::new(x, area.y, w, area.height))
            })
            .collect();
    }
    let tree = cut(placed, fold_of);
    let mut out = vec![];
    put(&tree, area, fold_of, &mut out);

    out.sort_by_key(|(i, _)| placed.iter().position(|(j, _)| j == i));
    out
}

#[derive(Debug)]
enum Cut {
    Leaf(usize),
    Split {
        columns: bool,
        parts: Vec<(Cut, Rect)>,
    },

    Fixed(Vec<(usize, Rect)>),
}

fn bbox(v: &[(usize, Rect)]) -> Rect {
    let x0 = v.iter().map(|(_, r)| r.x).min().unwrap_or(0);
    let y0 = v.iter().map(|(_, r)| r.y).min().unwrap_or(0);
    let x1 = v.iter().map(|(_, r)| r.x + r.width).max().unwrap_or(0);
    let y1 = v.iter().map(|(_, r)| r.y + r.height).max().unwrap_or(0);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

fn groups(v: &[(usize, Rect)], columns: bool) -> Vec<Vec<(usize, Rect)>> {
    let lo = |r: &Rect| if columns { r.x } else { r.y };
    let hi = |r: &Rect| {
        if columns {
            r.x + r.width
        } else {
            r.y + r.height
        }
    };
    let start = v.iter().map(|(_, r)| lo(r)).min().unwrap_or(0);
    let mut cuts: Vec<u16> = v
        .iter()
        .map(|(_, r)| lo(r))
        .filter(|&c| c > start)
        .filter(|&c| v.iter().all(|(_, r)| !(lo(r) < c && c < hi(r))))
        .collect();
    cuts.sort();
    cuts.dedup();
    let mut out: Vec<Vec<(usize, Rect)>> = vec![vec![]; cuts.len() + 1];
    for it in v {
        let k = cuts.iter().filter(|&&c| lo(&it.1) >= c).count();
        out[k].push(*it);
    }
    out.retain(|g| !g.is_empty());
    out
}

fn cut(v: &[(usize, Rect)], fold_of: &dyn Fn(usize) -> Fold) -> Cut {
    if v.len() == 1 {
        return Cut::Leaf(v[0].0);
    }

    let count = |f: Fold| v.iter().filter(|(i, _)| fold_of(*i) == f).count();
    let order = if count(Fold::Col) > count(Fold::Row) {
        [false, true]
    } else {
        [true, false]
    };
    for columns in order {
        let g = groups(v, columns);
        if g.len() > 1 {
            return Cut::Split {
                columns,
                parts: g.iter().map(|x| (cut(x, fold_of), bbox(x))).collect(),
            };
        }
    }
    Cut::Fixed(v.to_vec())
}

fn fixed(c: &Cut, fold_of: &dyn Fn(usize) -> Fold, width: bool) -> Option<u16> {
    let (want, unit) = if width {
        (Fold::Col, COL_W)
    } else {
        (Fold::Row, 1)
    };
    match c {
        Cut::Leaf(i) => (fold_of(*i) == want).then_some(unit),
        Cut::Fixed(v) => v
            .iter()
            .all(|(i, _)| fold_of(*i) == want)
            .then_some(v.len() as u16 * unit),
        Cut::Split { columns, parts } => {
            let h: Option<Vec<u16>> = parts
                .iter()
                .map(|(p, _)| fixed(p, fold_of, width))
                .collect();
            let h = h?;
            Some(if *columns == width {
                h.into_iter().sum()
            } else {
                h.into_iter().max().unwrap_or(unit)
            })
        }
    }
}

fn put(c: &Cut, area: Rect, fold_of: &dyn Fn(usize) -> Fold, out: &mut Vec<(usize, Rect)>) {
    match c {
        Cut::Leaf(i) => out.push((*i, area)),
        Cut::Fixed(v) => out.extend(v.iter().copied()),
        Cut::Split { columns, parts } => {
            let columns = *columns;

            let (a0, alen, c0, clen) = if columns {
                (area.x, area.width, area.y, area.height)
            } else {
                (area.y, area.height, area.x, area.width)
            };
            let mk = |a: u16, al: u16, c: u16, cl: u16| {
                if columns {
                    Rect::new(a, c, al, cl)
                } else {
                    Rect::new(c, a, cl, al)
                }
            };
            let along_len = |k: usize| {
                let r = parts[k].1;
                (if columns { r.width } else { r.height }).max(1) as f32
            };
            let inline: Vec<Option<u16>> = parts
                .iter()
                .map(|(p, _)| fixed(p, fold_of, columns))
                .collect();
            let across: Vec<Option<u16>> = parts
                .iter()
                .map(|(p, _)| fixed(p, fold_of, !columns))
                .collect();
            let band_ix: Vec<usize> = (0..parts.len())
                .filter(|&k| inline[k].is_none() && across[k].is_some())
                .collect();
            let band = band_ix
                .iter()
                .filter_map(|&k| across[k])
                .max()
                .unwrap_or(0)
                .min(clen);

            let w: Vec<f32> = band_ix.iter().map(|&k| along_len(k)).collect();
            for (&k, (a, l)) in band_ix.iter().zip(split(a0, alen, &w)) {
                put(&parts[k].0, mk(a, l, c0, band), fold_of, out);
            }
            if band_ix.len() == parts.len() {
                return;
            }
            let (rc0, rclen) = (c0 + band, clen - band);
            let rest: Vec<usize> = (0..parts.len()).filter(|k| !band_ix.contains(k)).collect();
            let need: u16 = rest.iter().filter_map(|&k| inline[k]).sum();
            let free = alen.saturating_sub(need);
            let live: Vec<f32> = rest
                .iter()
                .filter(|&&k| inline[k].is_none())
                .map(|&k| along_len(k))
                .collect();
            let mut tracks = split(0, free, &live).into_iter();
            let mut a = a0;
            for &k in &rest {
                let l = match inline[k] {
                    Some(l) => l,
                    None => tracks.next().map(|t| t.1).unwrap_or(0),
                };
                let l = l.min((a0 + alen).saturating_sub(a));
                put(&parts[k].0, mk(a, l, rc0, rclen), fold_of, out);
                a += l;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(v: &Value) -> Option<usize> {
        v.as_u64()
            .and_then(|n| (n as usize).checked_sub(1))
            .filter(|i| *i < 4)
    }

    fn rects(v: Value, open: &[Option<usize>]) -> Option<Vec<(usize, Rect)>> {
        let t = parse(&v, &acct).unwrap();
        arrange(&t, Rect::new(0, 0, 200, 50), open)
    }

    #[test]
    fn shapes() {
        let open = [Some(0), Some(1), Some(2), Some(3)];

        let r = rects(
            json!({"split": "columns", "children": [{"account": 1}, {"account": 2}]}),
            &open,
        )
        .unwrap();
        assert_eq!(
            r,
            vec![
                (0, Rect::new(0, 0, 100, 50)),
                (1, Rect::new(100, 0, 100, 50))
            ]
        );

        let r = rects(
            json!({"split": "rows", "children": [
            {"split": "columns", "children": [{"account": 1}, {"account": 2}]},
            {"split": "columns", "children": [{"account": 3}, {"account": 4}]}]}),
            &open,
        )
        .unwrap();
        assert_eq!(r[3], (3, Rect::new(100, 25, 100, 25)));

        let t = json!({"split": "columns", "sizes": [2, 1], "children": [{"account": 1}, {"rest": true}]});
        let r = rects(t.clone(), &open).unwrap();
        assert_eq!(r[0], (0, Rect::new(0, 0, 133, 50)));
        assert_eq!(r.len(), 4);
        assert!(r[1..].iter().all(|(_, x)| x.x == 133 && x.width == 67));
        assert_eq!(describe(&parse(&t, &acct).unwrap()), "a1 | rest");

        let r = rects(
            json!({"split": "rows", "children": [{"account": 1}, {"account": 2}, {"account": 3}]}),
            &open,
        )
        .unwrap();
        assert_eq!(r.iter().map(|x| x.1.height).sum::<u16>(), 50);

        let r = rects(json!({"account": 2}), &open).unwrap();
        assert_eq!(r, vec![(1, Rect::new(0, 0, 200, 50))]);

        let r = rects(
            json!({"split": "columns", "children": [{"account": 1}, {"account": 2}]}),
            &[Some(0)],
        )
        .unwrap();
        assert_eq!(r, vec![(0, Rect::new(0, 0, 200, 50))]);

        let many = json!({"split": "columns", "children": (1..=4).map(|a| json!({"account": a})).collect::<Vec<_>>()});
        let t = parse(&many, &acct).unwrap();
        assert!(arrange(&t, Rect::new(0, 0, 60, 30), &open).is_none());

        assert!(parse(
            &json!({"split": "diagonal", "children": [{"account": 1}]}),
            &acct
        )
        .unwrap_err()
        .contains("columns"));
        assert!(parse(&json!({"account": 9}), &acct).is_err());
        let back = to_json(
            &parse(
                &json!({"split": "rows", "children": [{"account": 1}, {"rest": true}]}),
                &acct,
            )
            .unwrap(),
        );
        assert_eq!(back["children"][0]["account"], json!(1));
    }

    fn grid2x2() -> Vec<(usize, Rect)> {
        vec![
            (0, Rect::new(0, 0, 100, 25)),
            (1, Rect::new(100, 0, 100, 25)),
            (2, Rect::new(0, 25, 100, 25)),
            (3, Rect::new(100, 25, 100, 25)),
        ]
    }

    #[test]
    fn folding_gives_the_space_to_siblings() {
        let r = fold(&grid2x2(), &|i| i < 2);
        assert_eq!(
            r,
            vec![
                (0, Rect::new(0, 0, 100, 1)),
                (1, Rect::new(100, 0, 100, 1)),
                (2, Rect::new(0, 1, 100, 49)),
                (3, Rect::new(100, 1, 100, 49)),
            ]
        );

        let r = fold(&grid2x2(), &|i| i == 0);
        assert_eq!(r[0].1, Rect::new(0, 0, 100, 1));
        assert_eq!(r[2].1, Rect::new(0, 1, 100, 49));
        assert_eq!(r[1].1, Rect::new(100, 0, 100, 25));

        let cols = vec![
            (0, Rect::new(0, 0, 60, 40)),
            (1, Rect::new(60, 0, 60, 40)),
            (2, Rect::new(120, 0, 60, 40)),
        ];
        let r = fold(&cols, &|i| i == 1);
        assert_eq!(r[1].1, Rect::new(0, 0, 180, 1));
        assert_eq!(r[0].1, Rect::new(0, 1, 90, 39));
        assert_eq!(r[2].1, Rect::new(90, 1, 90, 39));

        let r = fold(&grid2x2(), &|_| true);
        assert_eq!(
            r.iter().map(|x| x.1).collect::<Vec<_>>(),
            (0..4).map(|y| Rect::new(0, y, 200, 1)).collect::<Vec<_>>()
        );

        assert_eq!(fold(&grid2x2(), &|_| false), grid2x2());

        let three = vec![
            (0, Rect::new(0, 0, 100, 25)),
            (1, Rect::new(100, 0, 100, 25)),
            (2, Rect::new(0, 25, 200, 25)),
        ];
        let r = fold(&three, &|i| i == 2);
        assert_eq!(r[0].1, Rect::new(0, 0, 100, 49));
        assert_eq!(r[2].1, Rect::new(0, 49, 200, 1));

        let r = fold(&three, &|i| i == 0);
        assert_eq!(r[0].1.height + r[0].1.y, 1);
        assert_eq!(r[2].1.y + r[2].1.height, 50);
    }

    #[test]
    fn sideways_folding_gives_the_width_to_the_row_sibling() {
        let side = |set: &'static [usize]| {
            move |i: usize| {
                if set.contains(&i) {
                    Fold::Col
                } else {
                    Fold::Open
                }
            }
        };

        let r = fold_dirs(&grid2x2(), &side(&[0]));
        assert_eq!(r[0].1, Rect::new(0, 0, COL_W, 25));
        assert_eq!(r[1].1, Rect::new(COL_W, 0, 200 - COL_W, 25));
        assert_eq!(r[2].1, Rect::new(0, 25, 100, 25));
        assert_eq!(r[3].1, Rect::new(100, 25, 100, 25));

        let cols = vec![
            (0, Rect::new(0, 0, 60, 40)),
            (1, Rect::new(60, 0, 60, 40)),
            (2, Rect::new(120, 0, 60, 40)),
        ];
        let r = fold_dirs(&cols, &side(&[1]));
        assert_eq!(r[1].1.width, COL_W);
        assert_eq!(r[0].1.x, 0);
        assert_eq!(r[1].1.x, r[0].1.width);
        assert_eq!(r[2].1.x + r[2].1.width, 180);
        assert_eq!(r[0].1.width + r[2].1.width, 180 - COL_W);

        let rows = vec![
            (0, Rect::new(0, 0, 120, 20)),
            (1, Rect::new(0, 20, 120, 20)),
        ];
        let r = fold_dirs(&rows, &side(&[0]));
        assert_eq!(r[0].1, Rect::new(0, 0, COL_W, 40));
        assert_eq!(r[1].1, Rect::new(COL_W, 0, 120 - COL_W, 40));

        let mixed = |i: usize| match i {
            0 => Fold::Col,
            3 => Fold::Row,
            _ => Fold::Open,
        };
        let r = fold_dirs(&grid2x2(), &mixed);
        assert_eq!(r[0].1.width, COL_W);
        assert_eq!(r[3].1.height, 1);
        let area: u32 = r
            .iter()
            .map(|(_, x)| x.width as u32 * x.height as u32)
            .sum();
        assert!(area <= 200 * 50);
        for (_, a) in &r {
            for (_, b) in &r {
                if a != b {
                    assert!(a.intersection(*b).area() == 0, "{a:?} {b:?}");
                }
            }
        }

        let r = fold_dirs(&grid2x2(), &|_| Fold::Col);
        let x_of = |id: usize| r.iter().find(|x| x.0 == id).unwrap().1.x;

        assert_eq!(
            [x_of(0), x_of(2), x_of(1), x_of(3)],
            [0, COL_W, 2 * COL_W, 3 * COL_W]
        );
        assert!(r.iter().all(|x| x.1.width == COL_W && x.1.height == 50));

        for g in ["◂", "▸", "_", "□", "▣", "▭", "×", "●", "│"] {
            assert_eq!(unicode_width::UnicodeWidthStr::width(g), 1, "{g}");
        }
    }
}
