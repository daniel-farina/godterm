//! Which accounts the grid shows, and how: closing an account takes it
//! out of the grid (it stays configured and logged in, its tabs keep
//! running), opening brings it back in place, and any split of the panes
//! is a layout. All of it by voice (close_accounts, open_accounts,
//! set_layout), from the View menu, and saved in config.toml.

use serde_json::{json, Value};

use crate::app::App;
use crate::grid_layout::{self, Node};
use crate::harness::Harness;

impl App {
    pub fn account_closed(&self, a: usize) -> bool {
        self.cfg
            .accounts
            .get(a)
            .is_some_and(|x| self.cfg.closed_accounts.contains(&x.name))
    }

    /// Accounts named by a flexible target: a number, a label, a list,
    /// "all", or a harness ("grok", "grok accounts", "claude").
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
                for h in [Harness::Claude, Harness::Grok] {
                    if low == h.name() {
                        return Ok((0..n)
                            .filter(|&a| self.cfg.accounts[a].harness() == h)
                            .collect());
                    }
                }
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

    /// Close accounts in the grid (not a logout; nothing stops).
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

    /// Open accounts again (in place). `only`: close every other one.
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
        // An account without a pane gets one.
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

    /// The custom layout, if one is set and valid.
    pub fn layout_tree(&self) -> Option<Node> {
        if self.cfg.layout_tree.trim().is_empty() {
            return None;
        }
        let v: Value = serde_json::from_str(&self.cfg.layout_tree).ok()?;
        grid_layout::parse(&v, &|x| self.account_arg(x).ok()).ok()
    }

    /// set_layout: a mode name ("auto", "grid" with "3x2", "columns",
    /// "rows", "focus") or a split tree.
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
            // Accounts it names are opened.
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

    /// The grid line for the assistant's state block.
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
        format!(
            "grid: open {}{} · layout {layout} (close_accounts hides from the grid, logged in and running; logout_account logs out)\n",
            if open.is_empty() { "none".to_string() } else { open.join(" ") },
            if closed.is_empty() { String::new() } else { format!(", closed {}", closed.join(" ")) },
        )
    }

    /// close_accounts / open_accounts / set_layout; None for other tools.
    pub fn grid_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        let r = match tool {
            "close_accounts" | "open_accounts" => (|| {
                let mut accts = self.grid_targets(
                    args.get("accounts")
                        .or(args.get("account"))
                        .unwrap_or(&Value::Null),
                )?;
                if let Some(x) = args.get("except").filter(|x| !x.is_null()) {
                    let ex = self.grid_targets(x)?;
                    accts.retain(|a| !ex.contains(a));
                }
                let say = if tool == "close_accounts" {
                    self.close_accounts(&accts)
                } else {
                    self.open_accounts(&accts, args["only"].as_bool() == Some(true))
                };
                Ok(json!({"ok": true, "result": {"say": say, "open": self.visible_panes().len()}}))
            })(),
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
