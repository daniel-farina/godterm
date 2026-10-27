use serde_json::{json, Value};

use crate::app::App;
use crate::grid_layout::{self, Fold, Node};
use crate::harness::Harness;

impl App {
    pub fn account_closed(&self, a: usize) -> bool {
        self.cfg
            .accounts
            .get(a)
            .is_some_and(|x| self.cfg.closed_accounts.contains(&x.name))
    }

    pub fn grid_targets(&self, v: &Value) -> Result<Vec<usize>, String> {
        let n = self.cfg.accounts.len();
        let one = |x: &Value| -> Result<Vec<usize>, String> {
            if let Some(s) = x.as_str() {
                let low = s.trim().to_lowercase();
                let low = low
                    .trim_end_matches(" accounts")
                    .trim_end_matches(" account")
                    .trim();
                if low == "all" || low == "every" || low == "everything" {
                    return Ok((0..n).collect());
                }
                for &h in crate::harness::ALL {
                    if low == h.name() {
                        return Ok((0..n)
                            .filter(|&a| self.cfg.accounts[a].harness() == h)
                            .collect());
                    }
                }
            }
            if let Some(v) = x.as_str().and_then(|s| self.accounts_at(s)) {
                return Ok(v);
            }
            Ok(vec![self.account_arg(x)?])
        };
        let mut out = vec![];
        match v {
            Value::Array(a) => {
                for x in a {
                    out.extend(one(x)?);
                }
            }
            Value::Null => {
                return Err(
                    "say which accounts (numbers, labels, \"all\", \"grok\" or \"claude\")".into(),
                )
            }
            x => out.extend(one(x)?),
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn accounts_at(&self, s: &str) -> Option<Vec<usize>> {
        let low = s.to_lowercase();
        let words: Vec<&str> = low
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        let side = words.iter().find_map(|w| match *w {
            "top" | "upper" => Some(0),
            "bottom" | "lower" => Some(1),
            "left" | "leftmost" => Some(2),
            "right" | "rightmost" => Some(3),
            _ => None,
        })?;

        const FILL: &[&str] = &[
            "the",
            "top",
            "upper",
            "bottom",
            "lower",
            "left",
            "leftmost",
            "right",
            "rightmost",
            "one",
            "ones",
            "pane",
            "panes",
            "account",
            "accounts",
            "row",
            "column",
            "side",
            "two",
            "three",
            "four",
            "five",
            "both",
            "pair",
            "and",
            "on",
            "of",
        ];
        if words
            .iter()
            .any(|w| !FILL.contains(w) && w.parse::<usize>().is_err())
        {
            return None;
        }
        let count = words.iter().find_map(|w| match *w {
            "two" | "both" | "pair" => Some(2),
            "three" => Some(3),
            "four" => Some(4),
            "five" => Some(5),
            w => w.parse::<usize>().ok().filter(|n| *n > 0),
        });
        let area = if self.main_area.width >= 40 && self.main_area.height >= 10 {
            self.main_area
        } else {
            ratatui::layout::Rect::new(0, 0, 200, 50)
        };
        let (vis, a) = self.arrange_unfolded(area);
        let fi = vis.iter().position(|&p| p == self.focus).unwrap_or(0);
        let page = fi / a.per_page.max(1);
        let mut on: Vec<(usize, ratatui::layout::Rect)> = a
            .placed
            .iter()
            .filter(|p| p.page == page)
            .filter_map(|p| vis.get(p.index).map(|&i| (i, p.rect)))
            .filter(|(i, _)| self.panes[*i].account.is_some())
            .collect();
        let key = |r: &ratatui::layout::Rect| -> (i32, i32) {
            match side {
                0 => (r.y as i32, r.x as i32),
                1 => (-((r.y + r.height) as i32), r.x as i32),
                2 => (r.x as i32, r.y as i32),
                _ => (-((r.x + r.width) as i32), r.y as i32),
            }
        };
        on.sort_by_key(|(_, r)| key(r));
        let first = on.first().map(|(_, r)| key(r).0)?;
        let picked: Vec<usize> = match count {
            Some(n) => on.iter().take(n).map(|(i, _)| *i).collect(),
            None => on
                .iter()
                .filter(|(_, r)| key(r).0 == first)
                .map(|(i, _)| *i)
                .collect(),
        };
        let mut out: Vec<usize> = picked
            .iter()
            .filter_map(|&i| self.panes[i].account)
            .collect();
        out.sort();
        out.dedup();
        Some(out)
    }

    fn names_of(&self, accts: &[usize]) -> String {
        let n: Vec<String> = accts
            .iter()
            .map(|a| self.cfg.accounts[*a].display().to_string())
            .collect();
        match n.as_slice() {
            [] => "no account".into(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    }

    fn save_closed(&mut self) {
        let mut arr = toml_edit::Array::new();
        for n in &self.cfg.closed_accounts {
            arr.push(n.as_str());
        }
        let _ = crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("closed_accounts"),
            Some(toml_edit::value(arr)),
        );
        self.config_mtime = crate::app::config_mtime();
        self.state_dirty = true;
        self.ensure_focus_visible();
    }

    pub fn account_collapsed(&self, a: usize) -> bool {
        self.account_fold(a) != Fold::Open
    }

    pub fn account_fold(&self, a: usize) -> Fold {
        let Some(x) = self.cfg.accounts.get(a) else {
            return Fold::Open;
        };
        if self.cfg.collapsed_sideways.contains(&x.name) {
            Fold::Col
        } else if self.cfg.collapsed_accounts.contains(&x.name) {
            Fold::Row
        } else {
            Fold::Open
        }
    }

    pub fn pane_fold(&self, p: usize) -> Fold {
        if self.zoom {
            return Fold::Open;
        }
        self.panes
            .get(p)
            .and_then(|s| s.account)
            .map(|a| self.account_fold(a))
            .unwrap_or(Fold::Open)
    }

    pub fn pane_folded(&self, p: usize) -> bool {
        self.pane_fold(p) != Fold::Open
    }

    fn save_collapsed(&mut self) {
        for (key, list) in [
            ("collapsed_accounts", self.cfg.collapsed_accounts.clone()),
            ("collapsed_sideways", self.cfg.collapsed_sideways.clone()),
        ] {
            let mut arr = toml_edit::Array::new();
            for n in &list {
                arr.push(n.as_str());
            }
            let _ = crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global(key),
                Some(toml_edit::value(arr)),
            );
        }
        self.config_mtime = crate::app::config_mtime();
        self.state_dirty = true;
    }

    pub fn collapse_accounts_dir(&mut self, accts: &[usize], dir: Fold) -> String {
        let mut done = vec![];
        for &a in accts {
            if self.account_fold(a) == dir {
                continue;
            }
            let name = self.cfg.accounts[a].name.clone();
            self.cfg.collapsed_accounts.retain(|n| *n != name);
            self.cfg.collapsed_sideways.retain(|n| *n != name);
            match dir {
                Fold::Col => self.cfg.collapsed_sideways.push(name),
                _ => self.cfg.collapsed_accounts.push(name),
            }
            done.push(a);
        }
        self.save_collapsed();
        self.view = crate::app::View::Grid;
        let how = if dir == Fold::Col { " sideways" } else { "" };
        crate::log::info(&format!("grid: collapsed{how} {}", self.names_of(&done)));
        if done.is_empty() {
            return format!(
                "{} {} already collapsed{how}.",
                self.names_of(accts),
                if accts.len() == 1 { "is" } else { "are" }
            );
        }
        let all = (0..self.cfg.accounts.len())
            .filter(|a| !self.account_closed(*a))
            .all(|a| self.account_collapsed(a));
        format!(
            "Collapsed {}{how}.{}",
            self.names_of(&done),
            if all {
                " Everything is collapsed now."
            } else {
                ""
            }
        )
    }

    pub fn expand_accounts(&mut self, accts: &[usize]) -> String {
        let names: Vec<String> = accts
            .iter()
            .map(|a| self.cfg.accounts[*a].name.clone())
            .collect();
        let had: Vec<usize> = accts
            .iter()
            .copied()
            .filter(|&a| self.account_collapsed(a))
            .collect();
        self.cfg.collapsed_accounts.retain(|n| !names.contains(n));
        self.cfg.collapsed_sideways.retain(|n| !names.contains(n));
        self.save_collapsed();
        self.view = crate::app::View::Grid;
        crate::log::info(&format!("grid: expanded {}", self.names_of(&had)));
        if had.is_empty() {
            return format!(
                "{} {} not collapsed.",
                self.names_of(accts),
                if accts.len() == 1 { "is" } else { "are" }
            );
        }
        format!("Expanded {}.", self.names_of(&had))
    }

    pub fn toggle_fold_pane(&mut self, p: usize) {
        self.toggle_fold_dir(p, Fold::Row);
    }

    pub fn toggle_fold_sideways(&mut self, p: usize) {
        self.toggle_fold_dir(p, Fold::Col);
    }

    fn toggle_fold_dir(&mut self, p: usize, dir: Fold) {
        let Some(a) = self.panes.get(p).and_then(|s| s.account) else {
            self.flash("This pane has no account to collapse");
            return;
        };

        let say = if self.account_collapsed(a) {
            self.focus = p;
            self.expand_accounts(&[a])
        } else {
            self.collapse_accounts_dir(&[a], dir)
        };
        self.flash(say);
    }

    pub fn expand_on_focus(&mut self, before: usize) {
        if self.focus == before || !self.cfg.expand_collapsed_on_focus {
            return;
        }
        if let Some(a) = self.panes.get(self.focus).and_then(|s| s.account) {
            if self.account_collapsed(a) {
                self.expand_accounts(&[a]);
            }
        }
    }

    pub fn close_accounts(&mut self, accts: &[usize]) -> String {
        let mut done = vec![];
        for &a in accts {
            let name = self.cfg.accounts[a].name.clone();
            if !self.cfg.closed_accounts.contains(&name) {
                self.cfg.closed_accounts.push(name);
                done.push(a);
            }
        }
        self.save_closed();
        crate::log::info(&format!("grid: closed {}", self.names_of(&done)));
        if done.is_empty() {
            return format!(
                "{} {} already closed.",
                self.names_of(accts),
                if accts.len() == 1 { "is" } else { "are" }
            );
        }
        let all_grok = done
            .iter()
            .all(|&a| self.cfg.accounts[a].harness() == Harness::Grok)
            && done.len() > 1;
        let back = if all_grok {
            "open Grok".to_string()
        } else if done.len() == 1 {
            format!("open {}", self.cfg.accounts[done[0]].display())
        } else {
            "open them".into()
        };
        format!(
            "Closed {}{}. They stay logged in; say {back} to bring {} back.",
            if all_grok {
                "both Grok accounts".to_string()
            } else {
                self.names_of(&done)
            },
            if self.visible_panes().is_empty() {
                " (nothing is open now)"
            } else {
                ""
            },
            if done.len() == 1 { "it" } else { "them" }
        )
    }

    pub fn open_accounts(&mut self, accts: &[usize], only: bool) -> String {
        let names: Vec<String> = accts
            .iter()
            .map(|a| self.cfg.accounts[*a].name.clone())
            .collect();
        if only {
            self.cfg.closed_accounts = self
                .cfg
                .accounts
                .iter()
                .map(|a| a.name.clone())
                .filter(|n| !names.contains(n))
                .collect();
        } else {
            self.cfg.closed_accounts.retain(|n| !names.contains(n));
        }

        for p in self.panes.iter_mut() {
            if p.hidden && p.account.is_some_and(|a| accts.contains(&a)) {
                p.hidden = false;
                self.state_dirty = true;
            }
        }

        for &a in accts {
            if !self.panes.iter().any(|p| p.account == Some(a)) {
                self.panes.push(crate::slot::Slot::new(
                    Some(a),
                    self.cfg.accounts[a].work_dir(),
                ));
            }
        }
        self.save_closed();
        if let Some(&a) = accts.first() {
            if let Some(p) = self
                .panes
                .iter()
                .position(|p| p.account == Some(a) && !p.hidden)
            {
                self.focus = p;
            }
        }
        self.view = crate::app::View::Grid;
        crate::log::info(&format!(
            "grid: opened {}{}",
            self.names_of(accts),
            if only { " only" } else { "" }
        ));
        if only {
            format!("Showing only {}.", self.names_of(accts))
        } else {
            format!("Opened {}.", self.names_of(accts))
        }
    }

    pub fn layout_tree(&self) -> Option<Node> {
        if self.cfg.layout_tree.trim().is_empty() {
            return None;
        }
        let v: Value = serde_json::from_str(&self.cfg.layout_tree).ok()?;
        grid_layout::parse(&v, &|x| self.account_arg(x).ok()).ok()
    }

    pub fn apply_layout(
        &mut self,
        mode: Option<&str>,
        grid: Option<&str>,
        tree: Option<&Value>,
    ) -> Result<String, String> {
        if let Some(t) = tree {
            let node = grid_layout::parse(t, &|x| self.account_arg(x).ok())?;
            let json = grid_layout::to_json(&node).to_string();
            crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global("layout_tree"),
                Some(toml_edit::value(json.clone())),
            )
            .map_err(|e| format!("not saved: {e:#}"))?;
            self.config_mtime = crate::app::config_mtime();
            self.cfg.layout_tree = json;
            self.rt_layout = Some("custom".into());
            self.state_dirty = true;
            self.view = crate::app::View::Grid;

            let mut named = vec![];
            fn walk(n: &Node, out: &mut Vec<usize>) {
                match n {
                    Node::Account(a) => out.push(*a),
                    Node::Split { children, .. } => children.iter().for_each(|c| walk(c, out)),
                    Node::Rest => {}
                }
            }
            walk(&node, &mut named);
            if named.iter().any(|a| self.account_closed(*a)) {
                self.open_accounts(&named, false);
            }
            self.ensure_focus_visible();
            let d = grid_layout::describe(&node);
            crate::log::info(&format!("grid: layout {d}"));
            return Ok(format!("Layout set: {d}."));
        }
        let m = mode.unwrap_or("auto");
        if !crate::layout::MODES.contains(&m) {
            return Err(format!(
                "layout is one of {} or a split tree",
                crate::layout::MODES.join(", ")
            ));
        }
        if let Some(g) = grid {
            if crate::layout::parse_grid(g).is_none() {
                return Err("grid is like 3x2 (columns x rows, up to 8)".into());
            }
            let _ = crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global("grid"),
                Some(toml_edit::value(g)),
            );
            self.config_mtime = crate::app::config_mtime();
            self.cfg.grid = g.to_string();
        }
        self.set_layout(m);
        Ok(format!(
            "Layout: {m}{}.",
            grid.map(|g| format!(" {g}")).unwrap_or_default()
        ))
    }

    pub fn grid_state_line(&self) -> String {
        let open: Vec<String> = (0..self.cfg.accounts.len())
            .filter(|a| !self.account_closed(*a))
            .map(|a| format!("a{}", a + 1))
            .collect();
        let closed: Vec<String> = (0..self.cfg.accounts.len())
            .filter(|a| self.account_closed(*a))
            .map(|a| format!("a{}", a + 1))
            .collect();
        let layout = match (self.layout_name().as_str(), self.layout_tree()) {
            ("custom", Some(t)) => format!("custom {}", grid_layout::describe(&t)),
            ("grid", _) => format!("grid {}", self.cfg.grid),
            (n, _) => n.to_string(),
        };
        let collapsed: Vec<String> = (0..self.cfg.accounts.len())
            .filter(|a| self.account_collapsed(*a) && !self.account_closed(*a))
            .map(|a| format!("a{}", a + 1))
            .collect();
        format!(
            "grid: open {}{}{} · layout {layout} (close_accounts hides from the grid, logged in and running; collapse_accounts folds a pane to a one row strip, or sideways to a narrow column with direction column; logout_account logs out)\n",
            if open.is_empty() { "none".to_string() } else { open.join(" ") },
            if closed.is_empty() { String::new() } else { format!(", closed {}", closed.join(" ")) },
            if collapsed.is_empty() { String::new() } else { format!(", collapsed {}", collapsed.join(" ")) },
        )
    }

    pub fn grid_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        let r = match tool {
            "close_accounts" | "open_accounts" | "collapse_accounts" | "expand_accounts" => {
                (|| {
                    let mut accts = self.grid_targets(
                        args.get("accounts")
                            .or(args.get("account"))
                            .unwrap_or(&Value::Null),
                    )?;
                    if let Some(x) = args.get("except").filter(|x| !x.is_null()) {
                        let ex = self.grid_targets(x)?;
                        accts.retain(|a| !ex.contains(a));
                    }
                    let say = match tool {
                        "close_accounts" => self.close_accounts(&accts),
                        "collapse_accounts" => {
                            let dir = match args["direction"].as_str() {
                                Some("column") | Some("sideways") | Some("col") => Fold::Col,
                                _ => Fold::Row,
                            };
                            self.collapse_accounts_dir(&accts, dir)
                        }
                        "expand_accounts" => self.expand_accounts(&accts),
                        _ => self.open_accounts(&accts, args["only"].as_bool() == Some(true)),
                    };
                    let collapsed: Vec<usize> = (0..self.cfg.accounts.len())
                        .filter(|a| self.account_collapsed(*a))
                        .map(|a| a + 1)
                        .collect();
                    Ok(
                        json!({"ok": true, "result": {"say": say, "open": self.visible_panes().len(), "collapsed": collapsed}}),
                    )
                })()
            }
            "set_layout" => (|| {
                let say = self.apply_layout(
                    args["mode"].as_str(),
                    args["grid"].as_str(),
                    args.get("tree").filter(|t| !t.is_null()),
                )?;
                Ok(json!({"ok": true, "result": {"say": say}}))
            })(),
            _ => return None,
        };
        Some(r)
    }
}
