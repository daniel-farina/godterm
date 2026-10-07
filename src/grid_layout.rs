//! Any split of the account panes: a tree of column and row splits with
//! sizes, whose leaves are accounts (or "the rest"). The assistant fills
//! it in from what the user says ("account 1 big on the left, the rest
//! stacked on the right"); it is saved as JSON in config.toml
//! (`layout_tree`, with `layout = "custom"`).

use ratatui::layout::Rect;
use serde_json::{json, Value};

/// Smallest pane a custom layout may make; smaller falls back to auto.
pub const MIN_W: u16 = 20;
pub const MIN_H: u16 = 6;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// Children side by side (columns) or stacked (rows), sized by weights.
    Split {
        columns: bool,
        sizes: Vec<f32>,
        children: Vec<Node>,
    },
    /// An account by index (0 based).
    Account(usize),
    /// Every open account not named elsewhere.
    Rest,
}

/// From the JSON the model gives: {"split": "columns"|"rows", "sizes":
/// [2, 1], "children": [...]}, {"account": 2} (1 based), {"rest": true}.
/// `account` resolves labels and names to indices.
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

/// "a1 | (a2 / a3)": | side by side, / stacked.
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

/// Rects for the open panes. `open[i]` is the account of visible pane i.
/// Accounts that are not open drop out (their siblings share the space).
/// None when a pane would be smaller than MIN_W x MIN_H (or nothing
/// matches): the caller falls back to the automatic grid.
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

/// The visible panes a node covers (for dropping empty parts).
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
            // The rest stack across the parent's direction.
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
        // Two columns.
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
        // 2x2.
        let r = rects(
            json!({"split": "rows", "children": [
            {"split": "columns", "children": [{"account": 1}, {"account": 2}]},
            {"split": "columns", "children": [{"account": 3}, {"account": 4}]}]}),
            &open,
        )
        .unwrap();
        assert_eq!(r[3], (3, Rect::new(100, 25, 100, 25)));
        // Account 1 big on the left, the rest stacked on the right.
        let t = json!({"split": "columns", "sizes": [2, 1], "children": [{"account": 1}, {"rest": true}]});
        let r = rects(t.clone(), &open).unwrap();
        assert_eq!(r[0], (0, Rect::new(0, 0, 133, 50)));
        assert_eq!(r.len(), 4);
        assert!(r[1..].iter().all(|(_, x)| x.x == 133 && x.width == 67));
        assert_eq!(describe(&parse(&t, &acct).unwrap()), "a1 | rest");
        // Three rows.
        let r = rects(
            json!({"split": "rows", "children": [{"account": 1}, {"account": 2}, {"account": 3}]}),
            &open,
        )
        .unwrap();
        assert_eq!(r.iter().map(|x| x.1.height).sum::<u16>(), 50);
        // Just account 2.
        let r = rects(json!({"account": 2}), &open).unwrap();
        assert_eq!(r, vec![(1, Rect::new(0, 0, 200, 50))]);
        // A closed account drops out; its sibling takes the space.
        let r = rects(
            json!({"split": "columns", "children": [{"account": 1}, {"account": 2}]}),
            &[Some(0)],
        )
        .unwrap();
        assert_eq!(r, vec![(0, Rect::new(0, 0, 200, 50))]);
        // Too small: fall back.
        let many = json!({"split": "columns", "children": (1..=4).map(|a| json!({"account": a})).collect::<Vec<_>>()});
        let t = parse(&many, &acct).unwrap();
        assert!(arrange(&t, Rect::new(0, 0, 60, 30), &open).is_none());
        // Bad input is explained.
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
}
