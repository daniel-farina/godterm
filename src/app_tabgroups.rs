//! Tab groups, pins and sorting in the app: the tab and group menus, the
//! dialogs they open, dropping a dragged tab on a group, and the
//! assistant's tools for all of it.

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};

use crate::app::{App, Modal};
use crate::tab_groups::{TabSort, SORTS};

/// A group header's right click menu.
pub const GROUP_MENU: &[&str] = &[
    "Rename...",
    "Color...",
    "Collapse / expand",
    "Close group...",
    "Move group to...",
    "Ungroup",
];

fn ok(v: Value) -> Value {
    json!({"ok": true, "result": v})
}

/// The marker of a pinned tab (plain, like the rest of the list).
pub const PIN: &str = "▴";

impl App {
    pub fn ungrouped_top(&self) -> bool {
        self.cfg.ungrouped_tabs == "top"
    }

    pub fn set_pinned(&mut self, s: usize, t: usize, on: bool) {
        if let Some(tab) = self.panes.get_mut(s).and_then(|p| p.tabs.get_mut(t)) {
            tab.pinned = on;
            let n = tab.name();
            self.state_dirty = true;
            self.flash(if on {
                format!("Pinned {n}: first in the list, asks before closing")
            } else {
                format!("Unpinned {n}")
            });
        }
    }

    pub fn toggle_pin(&mut self, s: usize, t: usize) {
        let on = self
            .panes
            .get(s)
            .and_then(|p| p.tabs.get(t))
            .is_some_and(|x| !x.pinned);
        self.set_pinned(s, t, on);
    }

    /// Put tabs (by uid) of pane `s` in group `name`.
    pub fn add_to_group(&mut self, s: usize, uids: &[u64], name: &str, color: Option<String>) {
        let Some(slot) = self.panes.get_mut(s) else {
            return;
        };
        let ix: Vec<usize> = uids.iter().filter_map(|u| slot.find_uid(*u)).collect();
        let g = slot.group_tabs(&ix, name, color);
        let gname = slot.groups[g].name.clone();
        self.state_dirty = true;
        self.flash(format!(
            "{} in {gname}",
            crate::control::plural(ix.len(), "tab")
        ));
    }

    pub fn set_tab_sort(&mut self, s: usize, sort: TabSort) {
        if let Some(slot) = self.panes.get_mut(s) {
            slot.sort = sort;
            *slot.order_cache.borrow_mut() = None;
            self.state_dirty = true;
            self.flash(format!("Tabs sorted: {}", sort.label()));
        }
    }

    pub fn toggle_group(&mut self, s: usize, g: usize) {
        if let Some(grp) = self.panes.get_mut(s).and_then(|p| p.groups.get_mut(g)) {
            grp.collapsed = !grp.collapsed;
            self.state_dirty = true;
        }
    }

    pub fn open_group_menu(&mut self, s: usize, g: usize) {
        if self.panes.get(s).is_some_and(|p| g < p.groups.len()) {
            self.modal = Modal::GroupMenu(s, g, 0);
        }
    }

    fn group_uids(&self, s: usize, g: usize) -> Vec<u64> {
        self.panes
            .get(s)
            .map(|p| p.members(g).iter().map(|&i| p.tabs[i].uid).collect())
            .unwrap_or_default()
    }

    pub fn group_menu_run(&mut self, s: usize, g: usize, i: usize) {
        self.modal = Modal::None;
        let Some(name) = self
            .panes
            .get(s)
            .and_then(|p| p.groups.get(g))
            .map(|x| x.name.clone())
        else {
            return;
        };
        match i {
            0 => {
                self.modal = Modal::GroupName {
                    slot: s,
                    group: Some(g),
                    tabs: vec![],
                    buf: name,
                }
            }
            1 => self.open_color_pick(crate::color_pick::Target::Group(s, g)),
            2 => self.toggle_group(s, g),
            3 => self.modal = Modal::ConfirmCloseGroup(s, g),
            4 => self.modal = Modal::GroupMove(s, g, 0),
            _ => {
                self.panes[s].ungroup(g);
                self.state_dirty = true;
                self.flash(format!("Ungrouped {name}"));
            }
        }
    }

    /// The tab menu's "Move to group": its choices (groups, then New
    /// group..., then No group).
    pub fn group_choices(&self, s: usize) -> Vec<String> {
        let mut v: Vec<String> = self
            .panes
            .get(s)
            .map(|p| p.groups.iter().map(|g| g.name.clone()).collect())
            .unwrap_or_default();
        v.push("New group...".into());
        v.push("No group".into());
        v
    }

    pub fn group_pick_run(&mut self, s: usize, t: usize, i: usize) {
        self.modal = Modal::None;
        let Some(uid) = self.panes.get(s).and_then(|p| p.tabs.get(t)).map(|x| x.uid) else {
            return;
        };
        let n = self.panes[s].groups.len();
        if i < n {
            let name = self.panes[s].groups[i].name.clone();
            self.add_to_group(s, &[uid], &name, None);
        } else if i == n {
            self.modal = Modal::GroupName {
                slot: s,
                group: None,
                tabs: vec![uid],
                buf: String::new(),
            };
        } else {
            self.panes[s].tabs[t].group = None;
            self.state_dirty = true;
            self.flash("Out of its group");
        }
    }

    /// Close every tab of a group at once (one Undo brings them back).
    pub fn close_group_now(&mut self, s: usize, g: usize) {
        self.modal = Modal::None;
        let uids = self.group_uids(s, g);
        let n = self.close_uids(&uids, "user");
        self.undo_toast = Some((
            std::time::Instant::now(),
            format!(
                "Closed {}. Undo: Ctrl-a W",
                crate::control::plural(n, "tab")
            ),
        ));
    }

    /// Close these tabs now as one close group; returns how many closed.
    pub fn close_uids(&mut self, uids: &[u64], by: &str) -> usize {
        let group = self.next_close_group();
        let mut closed = 0;
        for u in uids {
            let Some((sl, t)) = self.find_tab(*u) else {
                continue;
            };
            self.remember_closed(sl, t, group);
            self.tab_event(sl, t, "close", Some(by), None);
            let fallback = self.panes[sl]
                .account
                .map(|a| self.cfg.accounts[a].work_dir())
                .unwrap_or_else(crate::config::home_dir);
            self.panes[sl].close_tab(t, fallback);
            self.deliveries.retain(|d| d.uid != *u);
            closed += 1;
        }
        self.state_dirty = true;
        closed
    }

    /// Move a group's tabs to another account (each one as move_tab does;
    /// the group comes along).
    pub fn move_group_to(&mut self, s: usize, g: usize, to: usize) -> Value {
        self.modal = Modal::None;
        let Some(grp) = self.panes.get(s).and_then(|p| p.groups.get(g)).cloned() else {
            return json!({"error": "no such group"});
        };
        let ids: Vec<String> = self
            .group_uids(s, g)
            .iter()
            .map(|u| crate::control::tab_id(*u))
            .collect();
        let v = self.control_call(
            "move_tab",
            &json!({"_client": "ui", "tab": ids, "to": to + 1}),
        );
        if let Some(say) = v["result"]["say"].as_str() {
            self.flash(say.to_string());
        } else if let Some(e) = v["error"].as_str() {
            self.flash(format!("Not moved: {e}"));
        }
        let _ = grp;
        v
    }

    /// After tabs moved: the group exists on the new account too, with
    /// its color, and holds them.
    pub fn regroup_moved(&mut self, grp: &crate::tab_groups::TabGroup, new_uids: &[u64]) {
        for u in new_uids {
            if let Some((sl, t)) = self.find_tab(*u) {
                let slot = &mut self.panes[sl];
                slot.group_tabs(&[t], &grp.name, grp.color.clone());
            }
        }
        self.state_dirty = true;
    }

    /// A tab dragged in pane `s` and dropped: on a group header it joins
    /// that group; on another tab (manual order) it moves there.
    pub fn drop_tab(&mut self, s: usize, t: usize, on: Option<crate::hits::UiAction>) {
        use crate::hits::UiAction;
        match on {
            Some(UiAction::GroupHeader(s2, g)) if s2 == s => {
                if let Some(name) = self.panes[s].groups.get(g).map(|x| x.name.clone()) {
                    let uid = self.panes[s].tabs[t].uid;
                    self.add_to_group(s, &[uid], &name, None);
                }
            }
            Some(UiAction::SelectTab(s2, t2)) if s2 == s && t2 != t => {
                let slot = &mut self.panes[s];
                let active = slot.tabs[slot.active].uid;
                let tab = slot.tabs.remove(t);
                // Joining the group of the tab it lands on.
                let into = slot.tabs[if t2 > t { t2 - 1 } else { t2 }].group.clone();
                let mut tab = tab;
                if !tab.pinned {
                    tab.group = into;
                }
                let at = if t2 > t { t2 } else { t2 };
                slot.tabs.insert(at.min(slot.tabs.len()), tab);
                slot.active = slot.find_uid(active).unwrap_or(0);
                if slot.sort != TabSort::Manual {
                    self.flash(
                        "Moved (the list is sorted; Tabs ▾ > Sort tabs > Manual keeps your order)",
                    );
                }
                self.state_dirty = true;
            }
            _ => {}
        }
    }

    pub fn on_group_modal_key(&mut self, k: KeyEvent) {
        match self.modal.clone() {
            Modal::GroupMenu(s, g, sel) => match k.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Up => self.modal = Modal::GroupMenu(s, g, sel.saturating_sub(1)),
                KeyCode::Down => {
                    self.modal = Modal::GroupMenu(s, g, (sel + 1).min(GROUP_MENU.len() - 1))
                }
                KeyCode::Enter => self.group_menu_run(s, g, sel),
                KeyCode::Char('r') => self.group_menu_run(s, g, 0),
                KeyCode::Char('c') => self.group_menu_run(s, g, 1),
                KeyCode::Char(' ') => self.group_menu_run(s, g, 2),
                KeyCode::Char('x') => self.group_menu_run(s, g, 3),
                KeyCode::Char('m') => self.group_menu_run(s, g, 4),
                KeyCode::Char('u') => self.group_menu_run(s, g, 5),
                _ => {}
            },
            Modal::GroupPick(s, t, sel) => {
                let n = self.group_choices(s).len();
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Up => self.modal = Modal::GroupPick(s, t, sel.saturating_sub(1)),
                    KeyCode::Down => {
                        self.modal = Modal::GroupPick(s, t, (sel + 1).min(n.saturating_sub(1)))
                    }
                    KeyCode::Enter => self.group_pick_run(s, t, sel),
                    KeyCode::Char('n') => self.group_pick_run(s, t, n.saturating_sub(2)),
                    _ => {}
                }
            }
            Modal::GroupName {
                slot,
                group,
                tabs,
                mut buf,
            } => match k.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Enter => {
                    self.modal = Modal::None;
                    let name = buf.trim().to_string();
                    if name.is_empty() {
                        return;
                    }
                    match group {
                        Some(g) => {
                            self.panes[slot].rename_group(g, &name);
                            self.state_dirty = true;
                            self.flash(format!("Group renamed to {name}"));
                        }
                        None => self.add_to_group(slot, &tabs, &name, None),
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                    self.modal = Modal::GroupName {
                        slot,
                        group,
                        tabs,
                        buf,
                    };
                }
                KeyCode::Char(c) if buf.chars().count() < 40 => {
                    buf.push(c);
                    self.modal = Modal::GroupName {
                        slot,
                        group,
                        tabs,
                        buf,
                    };
                }
                _ => {}
            },
            Modal::ConfirmCloseGroup(s, g) => match k.code {
                KeyCode::Enter | KeyCode::Char('y') => self.close_group_now(s, g),
                _ => self.modal = Modal::None,
            },
            Modal::GroupMove(s, g, sel) => {
                let n = self.cfg.accounts.len();
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Up => self.modal = Modal::GroupMove(s, g, sel.saturating_sub(1)),
                    KeyCode::Down => {
                        self.modal = Modal::GroupMove(s, g, (sel + 1).min(n.saturating_sub(1)))
                    }
                    KeyCode::Enter => {
                        self.move_group_to(s, g, sel);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// The lines and buttons of the group dialogs, for ui.rs.
    pub fn group_modal_body(
        &self,
    ) -> Option<(String, Vec<String>, Vec<(String, crate::hits::UiAction)>)> {
        use crate::hits::UiAction;
        let sel_line =
            |i: usize, sel: usize, l: &str| format!("{}{l}", if i == sel { "› " } else { "  " });
        Some(match &self.modal {
            Modal::GroupMenu(s, g, sel) => {
                let name = self.panes.get(*s)?.groups.get(*g)?.name.clone();
                (
                    format!("group {name}"),
                    GROUP_MENU
                        .iter()
                        .enumerate()
                        .map(|(i, l)| sel_line(i, *sel, l))
                        .collect(),
                    GROUP_MENU
                        .iter()
                        .enumerate()
                        .map(|(i, l)| (l.to_string(), UiAction::GroupMenuItem(i)))
                        .collect(),
                )
            }
            Modal::GroupPick(s, _, sel) => {
                let c = self.group_choices(*s);
                (
                    "move to group".into(),
                    c.iter()
                        .enumerate()
                        .map(|(i, l)| sel_line(i, *sel, l))
                        .collect(),
                    c.iter()
                        .enumerate()
                        .map(|(i, l)| (l.clone(), UiAction::GroupPickItem(i)))
                        .collect(),
                )
            }
            Modal::GroupName { group, buf, .. } => (
                if group.is_some() {
                    "rename group"
                } else {
                    "new group"
                }
                .into(),
                vec![format!("> {buf}▏"), "Enter saves, Esc cancels.".into()],
                vec![
                    ("Save".into(), UiAction::ModalOk),
                    ("Cancel".into(), UiAction::ModalCancel),
                ],
            ),
            Modal::ConfirmCloseGroup(s, g) => {
                let p = self.panes.get(*s)?;
                let grp = p.groups.get(*g)?;
                let names: Vec<String> = p.members(*g).iter().map(|&i| p.tabs[i].name()).collect();
                (
                    format!("close group {}", grp.name),
                    vec![
                        format!(
                            "Close {}: {}?",
                            crate::control::plural(names.len(), "tab"),
                            names.join(", ")
                        ),
                        "Undo brings them back (Ctrl-a W).".into(),
                    ],
                    vec![
                        ("Close them".into(), UiAction::ModalOk),
                        ("Cancel".into(), UiAction::ModalCancel),
                    ],
                )
            }
            Modal::GroupMove(s, g, sel) => {
                let p = self.panes.get(*s)?;
                let grp = p.groups.get(*g)?;
                let accts: Vec<String> = self
                    .cfg
                    .accounts
                    .iter()
                    .map(|a| a.display().to_string())
                    .collect();
                let mut body = vec![format!(
                    "Move {} of {} to:",
                    crate::control::plural(p.members(*g).len(), "tab"),
                    grp.name
                )];
                body.extend(accts.iter().enumerate().map(|(i, l)| sel_line(i, *sel, l)));
                (
                    format!("move group {}", grp.name),
                    body,
                    accts
                        .iter()
                        .enumerate()
                        .map(|(i, l)| (l.clone(), UiAction::GroupMoveTo(i)))
                        .collect(),
                )
            }
            _ => return None,
        })
    }

    /// The group or the tabs a tool names in pane terms: (pane, group).
    pub fn group_named(&self, name: &str, acct: Option<usize>) -> Result<(usize, usize), String> {
        let mut all: Vec<(usize, usize, String)> = vec![];
        for (s, p) in self.panes.iter().enumerate() {
            if acct.is_some_and(|a| p.account != Some(a)) {
                continue;
            }
            for (g, grp) in p.groups.iter().enumerate() {
                all.push((s, g, grp.name.clone()));
            }
        }
        if all.is_empty() {
            return Err("there are no tab groups yet".into());
        }
        // "the api group" is api.
        let low = name.trim().to_lowercase();
        let low = low.strip_prefix("the ").unwrap_or(&low);
        let low = low.strip_suffix(" group").unwrap_or(low).trim().to_string();
        if let Some((s, g, _)) = all.iter().find(|(_, _, n)| n.to_lowercase() == low) {
            return Ok((*s, *g));
        }
        let name = low.as_str();
        let keys: Vec<(String, String)> = all
            .iter()
            .map(|(s, g, n)| (format!("{s}:{g}"), n.clone()))
            .collect();
        let pick =
            match crate::fuzzy::best(name, keys.iter().map(|(k, n)| (k.as_str(), n.as_str()))) {
                crate::fuzzy::Match::Sure(k) => k,
                crate::fuzzy::Match::Unsure(c) if c.len() == 1 => c[0].value.clone(),
                crate::fuzzy::Match::Unsure(c) => {
                    let names: Vec<String> = c
                        .iter()
                        .filter_map(|x| {
                            keys.iter()
                                .find(|(k, _)| *k == x.value)
                                .map(|(_, n)| n.clone())
                        })
                        .collect();
                    return Err(format!("which group: {}?", names.join(", ")));
                }
                crate::fuzzy::Match::None => {
                    return Err(format!(
                        "no group called {name}; the groups are {}",
                        all.iter()
                            .map(|x| x.2.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
            };
        let (s, g) = pick.split_once(':').ok_or("bad group")?;
        Ok((
            s.parse().map_err(|_| "bad group")?,
            g.parse().map_err(|_| "bad group")?,
        ))
    }

    /// The assistant's group, pin, color and sort tools; None for others.
    pub fn tabgroup_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        const TOOLS: &[&str] = &[
            "group_tabs",
            "ungroup",
            "rename_group",
            "set_group_color",
            "collapse_group",
            "pin_tab",
            "unpin_tab",
            "set_tab_color",
            "sort_tabs",
        ];
        if !TOOLS.contains(&tool) {
            return None;
        }
        Some(self.tabgroup_inner(tool, args))
    }

    fn color_arg(v: Option<&Value>) -> Result<Option<String>, String> {
        let Some(c) = v.and_then(Value::as_str) else {
            return Ok(None);
        };
        let c = c.trim().to_lowercase();
        if matches!(c.as_str(), "default" | "none" | "") {
            return Ok(Some(String::new()));
        }
        // Plain color words map onto the muted swatches.
        let alias = match c.as_str() {
            "green" => "sage",
            "red" => "clay",
            "yellow" | "orange" => "sand",
            "blue" => "slate",
            "purple" | "violet" | "pink" => "mauve",
            "gray" | "grey" | "brown" => "stone",
            x => x,
        };
        crate::theme::SWATCHES
            .iter()
            .find(|s| **s == alias)
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| {
                format!(
                    "no color {c}; the colors are {}",
                    crate::theme::SWATCHES.join(", ")
                )
            })
    }

    fn tabgroup_inner(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let acct = match args.get("account") {
            Some(v) if !v.is_null() => Some(self.account_arg(v)?),
            _ => None,
        };
        match tool {
            "group_tabs" => {
                let name = s("group")
                    .filter(|g| !g.trim().is_empty())
                    .ok_or("group (its name) is required")?;
                // ("group" names the new group here, not tabs to target.)
                let mut targ = args.clone();
                if let Some(o) = targ.as_object_mut() {
                    o.remove("group");
                }
                let tabs = self.tab_targets(&targ, true)?;
                let color = Self::color_arg(args.get("color"))?.filter(|c| !c.is_empty());
                let mut by_pane: std::collections::BTreeMap<usize, Vec<u64>> = Default::default();
                for &(sl, t) in &tabs {
                    by_pane
                        .entry(sl)
                        .or_default()
                        .push(self.panes[sl].tabs[t].uid);
                }
                for (sl, uids) in &by_pane {
                    let ix: Vec<usize> = uids
                        .iter()
                        .filter_map(|u| self.panes[*sl].find_uid(*u))
                        .collect();
                    self.panes[*sl].group_tabs(&ix, &name, color.clone());
                }
                self.state_dirty = true;
                let say = format!(
                    "Grouped {} as {name}.",
                    crate::control::plural(tabs.len(), "tab")
                );
                self.flash(say.clone());
                Ok(ok(
                    json!({"group": name, "tabs": self.uids(&tabs), "say": say}),
                ))
            }
            "ungroup" | "rename_group" | "set_group_color" | "collapse_group" => {
                let name = s("group").ok_or("group is required")?;
                let (sl, g) = self.group_named(&name, acct)?;
                let gname = self.panes[sl].groups[g].name.clone();
                let say = match tool {
                    "ungroup" => {
                        self.panes[sl].ungroup(g);
                        format!("Ungrouped {gname}; its tabs stay open.")
                    }
                    "rename_group" => {
                        let to = s("name")
                            .filter(|n| !n.trim().is_empty())
                            .ok_or("name (the new name) is required")?;
                        self.panes[sl].rename_group(g, &to);
                        format!("Renamed the {gname} group to {to}.")
                    }
                    "set_group_color" => {
                        let c = Self::color_arg(args.get("color"))?.ok_or("color is required")?;
                        self.panes[sl].groups[g].color = (!c.is_empty()).then(|| c.clone());
                        if c.is_empty() {
                            format!("The {gname} group has no color now.")
                        } else {
                            format!("The {gname} group is {c} now.")
                        }
                    }
                    _ => {
                        let on = args
                            .get("collapsed")
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        self.panes[sl].groups[g].collapsed = on;
                        format!(
                            "{} the {gname} group.",
                            if on { "Collapsed" } else { "Expanded" }
                        )
                    }
                };
                self.state_dirty = true;
                self.flash(say.clone());
                Ok(ok(json!({"group": gname, "say": say})))
            }
            "pin_tab" | "unpin_tab" => {
                let tabs = self.tab_targets(args, true)?;
                let on = tool == "pin_tab";
                for &(sl, t) in &tabs {
                    self.panes[sl].tabs[t].pinned = on;
                }
                self.state_dirty = true;
                let names: Vec<String> = tabs
                    .iter()
                    .map(|&(sl, t)| self.panes[sl].tabs[t].name())
                    .collect();
                let say = format!(
                    "{} {}.",
                    if on { "Pinned" } else { "Unpinned" },
                    names.join(", ")
                );
                self.flash(say.clone());
                Ok(ok(
                    json!({"tabs": self.uids(&tabs), "pinned": on, "say": say}),
                ))
            }
            "set_tab_color" => {
                let tabs = self.tab_targets(args, true)?;
                let c = Self::color_arg(args.get("color"))?.ok_or("color is required")?;
                for &(sl, t) in &tabs {
                    self.panes[sl].tabs[t].accent = (!c.is_empty()).then(|| c.clone());
                }
                self.state_dirty = true;
                let say = if c.is_empty() {
                    format!(
                        "{} back to the default color.",
                        crate::control::plural(tabs.len(), "tab")
                    )
                } else {
                    format!("{} {c} now.", crate::control::plural(tabs.len(), "tab"))
                };
                Ok(ok(
                    json!({"tabs": self.uids(&tabs), "color": c, "say": say}),
                ))
            }
            _ => {
                let by = s("by")
                    .ok_or("by is required: manual, recent, opened, name, folder or state")?;
                let sort = TabSort::parse(&by).ok_or_else(|| {
                    format!(
                        "sort by {}",
                        SORTS
                            .iter()
                            .map(|x| x.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
                let panes: Vec<usize> = match acct {
                    Some(a) => self
                        .panes
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.account == Some(a))
                        .map(|(i, _)| i)
                        .collect(),
                    None => vec![self.focus],
                };
                if panes.is_empty() {
                    return Err("that account has no pane open".into());
                }
                for p in &panes {
                    self.set_tab_sort(*p, sort);
                }
                let who = acct
                    .map(|a| self.cfg.accounts[a].display().to_string())
                    .unwrap_or_else(|| "this account".into());
                let say = format!("Sorted {who}'s tabs by {}.", sort.label().to_lowercase());
                Ok(ok(json!({"sort": sort.name(), "say": say})))
            }
        }
    }
}
