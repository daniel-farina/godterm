use crate::app::App;
use crate::control::tab_id;
use crate::pane::{LaunchKind, PaneState};
use crate::routes::{self, Bridge, ProxyUrl, RouteCfg, RoutesCfg};
use crate::routes_ts::{Handle, TsState};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOLD: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq)]
pub struct RouteBlock {
    pub reason: String,
    pub kind: LaunchKind,
    pub auto: bool,
}

type Shared<T> = Arc<Mutex<T>>;

#[derive(Default)]
pub struct RoutesRt {
    pub ts: HashMap<String, Handle>,
    pub bridges: HashMap<String, (String, Bridge)>,
    pub checks: Shared<HashMap<String, (Instant, bool)>>,
    pub tests: Shared<HashMap<String, Value>>,
    pub sys_nodes: Shared<Option<(Instant, Result<Vec<String>, String>)>>,
    pub last_tick: Option<Instant>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn bins_of(c: &RoutesCfg) -> RoutesCfg {
    RoutesCfg {
        tailscaled_bin: c.tailscaled_bin.clone(),
        tailscale_bin: c.tailscale_bin.clone(),
        list: vec![],
    }
}

fn local(port: u16, scheme: &str) -> String {
    format!("{scheme}://127.0.0.1:{port}")
}

impl App {
    fn route_ts_ensure(&mut self, r: &RouteCfg) -> Result<(), String> {
        if !self.routes.ts.contains_key(&r.name) {
            let h = Handle::spawn(
                &crate::config::app_home(),
                r.clone(),
                bins_of(&self.cfg.routes),
            )?;
            crate::log::info(&format!(
                "routes: {}: starting the managed tailscale",
                r.name
            ));
            self.routes.ts.insert(r.name.clone(), h);
        }
        Ok(())
    }

    pub(crate) fn route_endpoint(
        &mut self,
        name: &str,
    ) -> Result<(String, String, u16), (String, bool)> {
        let Some(r) = self.cfg.routes.get(name).cloned() else {
            return Err((
                "this route no longer exists; choose another route or Direct".into(),
                false,
            ));
        };
        if r.is_tailscale() {
            self.route_ts_ensure(&r).map_err(|e| (e, false))?;
            let snap = self.routes.ts[name].snapshot();
            return match &snap.state {
                TsState::Ready if snap.proxy => {
                    routes::reachable("127.0.0.1", snap.port, routes::CONNECT_TIMEOUT).map_err(
                        |_| ("the tailscale proxy is not answering yet".to_string(), true),
                    )?;
                    Ok((
                        local(snap.port, "http"),
                        local(snap.port, "socks5h"),
                        snap.port,
                    ))
                }
                s => Err((s.detail(), s.waits())),
            };
        }
        let u = ProxyUrl::parse(&r.url).map_err(|e| (e, false))?;
        let up = routes::reachable(&u.host, u.port, routes::CONNECT_TIMEOUT).is_ok();
        lock(&self.routes.checks).insert(name.to_string(), (Instant::now(), up));
        if !up {
            return Err(("the proxy did not accept a connection".into(), false));
        }
        if !u.scheme.is_socks() {
            let url = r.url.trim().to_string();
            return Ok((url.clone(), url, 0));
        }
        let fresh = self
            .routes
            .bridges
            .get(name)
            .is_some_and(|(url, _)| *url == r.url);
        if !fresh {
            let b = Bridge::start(u).map_err(|e| (e, false))?;
            self.routes
                .bridges
                .insert(name.to_string(), (r.url.clone(), b));
        }
        let port = self.routes.bridges[name].1.port;
        Ok((local(port, "http"), local(port, "http"), 0))
    }

    pub(crate) fn route_gate(
        &mut self,
        pane: usize,
        kind: &LaunchKind,
    ) -> Option<Vec<(String, String)>> {
        let tab = self.panes[pane].cur_mut();
        let uid = tab.uid;
        let Some(name) = tab.route.clone() else {
            tab.route_live = None;
            tab.route_block = None;
            tab.route_skip = false;
            return Some(vec![]);
        };
        if std::mem::take(&mut tab.route_skip) {
            tab.route_live = None;
            tab.route_block = None;
            crate::log::info(&format!(
                "routes: {} launches without its route {name}: the user chose to",
                tab_id(uid)
            ));
            return Some(vec![]);
        }
        match self.route_endpoint(&name) {
            Ok((http, all, port)) => {
                let tab = self.panes[pane].cur_mut();
                tab.route_live = Some(name);
                tab.route_port = port;
                tab.route_block = None;
                Some(routes::proxy_env(
                    &http,
                    &all,
                    routes::existing_no_proxy().as_deref(),
                ))
            }
            Err((reason, auto)) => {
                crate::log::info(&format!(
                    "routes: {} not launched: route {name} is not ready ({reason})",
                    tab_id(uid)
                ));
                let tab = self.panes[pane].cur_mut();
                tab.kill();
                tab.generation += 1;
                if let LaunchKind::Resume(id) = kind {
                    tab.set_session(Some(id.clone()), "held for its route");
                }
                tab.route_live = None;
                tab.state = PaneState::Failed(format!("route {name}: {reason}"));
                tab.set_activity(crate::pane::Activity::Idle);
                tab.route_block = Some(RouteBlock {
                    reason: reason.clone(),
                    kind: kind.clone(),
                    auto,
                });
                self.state_dirty = true;
                self.flash(format!(
                    "Not started: route {name} is not ready ({reason}). It was not launched without the route."
                ));
                None
            }
        }
    }

    pub(crate) fn tab_route_json(&self, s: usize, t: usize) -> Option<Value> {
        let tab = &self.panes[s].tabs[t];
        let name = tab.route.as_ref()?;
        let cfg = self.cfg.routes.get(name);
        let ts = cfg.is_some_and(|c| c.is_tailscale());
        let (state, detail) = if let Some(b) = &tab.route_block {
            ("blocked", b.reason.clone())
        } else if tab.is_running() {
            if tab.route_live.as_ref() != Some(name) {
                (
                    "bypassed",
                    "running without the route: you chose to launch it directly".to_string(),
                )
            } else if ts {
                match self.routes.ts.get(name).map(|h| h.snapshot()) {
                    Some(s) if s.state == TsState::Ready && s.proxy && s.port == tab.route_port => {
                        ("on", String::new())
                    }
                    Some(s) if s.state == TsState::Ready && s.proxy => (
                        "warn",
                        "the route restarted on a new port: reload this tab to reconnect it"
                            .to_string(),
                    ),
                    Some(s) => ("warn", s.state.detail()),
                    None => ("warn", "the managed tailscale is not running".to_string()),
                }
            } else {
                ("on", String::new())
            }
        } else if cfg.is_none() {
            ("blocked", "this route no longer exists".to_string())
        } else {
            ("idle", String::new())
        };
        let mut v =
            json!({"name": name, "kind": if ts { "tailscale" } else { "custom" }, "state": state});
        if !detail.is_empty() {
            v["detail"] = json!(detail);
        }
        Some(v)
    }

    pub fn route_label(&self, s: usize, t: usize) -> Option<(String, bool)> {
        let tab = &self.panes[s].tabs[t];
        let name = tab.route.as_ref()?;
        let warn = tab.route_block.is_some()
            || (tab.is_running() && tab.route_live.as_ref() != Some(name));
        Some((
            if warn {
                format!("via {name}!")
            } else {
                format!("via {name}")
            },
            warn,
        ))
    }

    fn route_users(&self, name: &str) -> usize {
        self.panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| t.route.as_deref() == Some(name))
            .count()
    }

    pub(crate) fn routes_tick(&mut self) {
        if self
            .routes
            .last_tick
            .is_some_and(|t| t.elapsed() < Duration::from_millis(700))
        {
            return;
        }
        self.routes.last_tick = Some(Instant::now());
        let names: Vec<String> = self.routes.ts.keys().cloned().collect();
        let bins = bins_of(&self.cfg.routes);
        for n in names {
            let cfg = self
                .cfg
                .routes
                .get(&n)
                .filter(|c| c.is_tailscale())
                .cloned();
            let used = self.route_users(&n) > 0;
            let held = self.routes.ts[&n]
                .hold_until
                .is_some_and(|t| Instant::now() < t);
            match cfg {
                Some(c) if used || held => self.routes.ts[&n].set_cfg(&c, &bins),
                _ => {
                    crate::log::info(&format!(
                        "routes: {n}: no tab uses it, stopping the managed tailscale"
                    ));
                    if let Some(h) = self.routes.ts.remove(&n) {
                        h.stop(true);
                    }
                }
            }
        }
        let used: Vec<String> = self.routes.bridges.keys().cloned().collect();
        for n in used {
            if self.route_users(&n) == 0 {
                self.routes.bridges.remove(&n);
            }
        }
        let mut retry = vec![];
        for (s, p) in self.panes.iter().enumerate() {
            for (t, tab) in p.tabs.iter().enumerate() {
                let (Some(b), Some(n)) = (&tab.route_block, &tab.route) else {
                    continue;
                };
                if !b.auto {
                    continue;
                }
                let ready = self
                    .routes
                    .ts
                    .get(n)
                    .map(|h| h.snapshot())
                    .is_some_and(|x| x.state == TsState::Ready && x.proxy);
                if ready {
                    retry.push((s, t, b.kind.clone()));
                } else if let Some(h) = self.routes.ts.get(n) {
                    let now = h.snapshot().state;
                    if now.detail() != b.reason {
                        retry.push((s, t, b.kind.clone()));
                    }
                }
            }
        }
        for (s, t, kind) in retry {
            self.launch_tab(s, t, kind);
        }
    }

    pub(crate) fn routes_shutdown(&mut self) {
        for (_, h) in self.routes.ts.drain() {
            h.stop(true);
        }
        self.routes.bridges.clear();
    }

    fn write_routes(&mut self, list: Vec<RouteCfg>) -> Result<(), String> {
        let item = (!list.is_empty()).then(|| {
            let mut arr = toml_edit::Array::new();
            for r in &list {
                let mut t = toml_edit::InlineTable::new();
                t.insert("name", r.name.as_str().into());
                t.insert("kind", r.kind.as_str().into());
                if !r.url.is_empty() {
                    t.insert("url", r.url.as_str().into());
                }
                if !r.exit_node.is_empty() {
                    t.insert("exit_node", r.exit_node.as_str().into());
                }
                if r.allow_lan {
                    t.insert("allow_lan", true.into());
                }
                arr.push(toml_edit::Value::InlineTable(t));
            }
            toml_edit::Item::Value(toml_edit::Value::Array(arr))
        });
        crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global("routes.list"),
            item,
        )
        .map_err(|_| "could not write config.toml".to_string())?;
        self.cfg.routes.list = list;
        self.config_mtime = crate::app::config_mtime();
        Ok(())
    }

    fn write_bin(&mut self, key: &'static str, value: &str) -> Result<(), String> {
        let v = value.trim();
        if !v.is_empty() && !std::path::Path::new(v).is_absolute() {
            return Err("give the full path of the program, starting with /".into());
        }
        crate::settings::write(
            &crate::config::Config::path(),
            &crate::settings::Key::Global(key),
            (!v.is_empty()).then(|| toml_edit::value(v)),
        )
        .map_err(|_| "could not write config.toml".to_string())?;
        self.config_mtime = crate::app::config_mtime();
        Ok(())
    }

    fn custom_check(&self, r: &RouteCfg) -> (&'static str, String) {
        let Ok(u) = ProxyUrl::parse(&r.url) else {
            return ("error", "The proxy URL is not valid".into());
        };
        let known = lock(&self.routes.checks).get(&r.name).copied();
        let stale = known.is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(5));
        if stale {
            let (checks, name) = (Arc::clone(&self.routes.checks), r.name.clone());
            let _ = std::thread::Builder::new()
                .name("route-check".into())
                .spawn(move || {
                    let up = routes::reachable(&u.host, u.port, routes::CONNECT_TIMEOUT).is_ok();
                    lock(&checks).insert(name, (Instant::now(), up));
                });
        }
        match known {
            Some((_, true)) => ("ready", "Ready".into()),
            Some((_, false)) => (
                "unreachable",
                "The proxy does not accept connections".into(),
            ),
            None => ("starting", "Checking".into()),
        }
    }

    pub(crate) fn routes_view(&mut self, with_system: bool) -> Value {
        let bins = bins_of(&self.cfg.routes);
        let resolved = crate::routes_ts::resolve(&bins);
        let list: Vec<Value> = self
            .cfg
            .routes
            .list
            .clone()
            .iter()
            .map(|r| {
                let mut v = json!({"name": r.name, "kind": if r.is_tailscale() { "tailscale" } else { "custom" },
                    "tabs": self.route_users(&r.name)});
                if r.is_tailscale() {
                    v["exit_node"] = json!(r.exit_node);
                    v["allow_lan"] = json!(r.allow_lan);
                    let snap = self.routes.ts.get(&r.name).map(|h| h.snapshot());
                    let state = match (&snap, &resolved) {
                        (Some(s), _) => s.state.clone(),
                        (None, Err(e)) => TsState::Missing(e.clone()),
                        (None, Ok(_)) => TsState::Stopped,
                    };
                    v["state"] = json!(state.word());
                    v["detail"] = json!(state.detail());
                    if let TsState::Login(url) = &state {
                        v["login_url"] = json!(url);
                    }
                    if let Some(s) = &snap {
                        v["running"] = json!(true);
                        v["nodes"] = s
                            .nodes
                            .iter()
                            .map(|n| json!({"name": n.name, "ip": n.ip, "online": n.online, "active": n.active}))
                            .collect();
                    }
                } else {
                    let (state, detail) = self.custom_check(r);
                    v["url"] = json!(routes::mask_url(&r.url));
                    v["has_secret"] = json!(ProxyUrl::parse(&r.url).is_ok_and(|u| u.has_secret()));
                    v["state"] = json!(state);
                    v["detail"] = json!(detail);
                }
                if let Some(t) = lock(&self.routes.tests).get(&r.name) {
                    v["test"] = t.clone();
                }
                v
            })
            .collect();
        let mut out = json!({
            "routes": list,
            "limits": routes::LIMITS,
            "test_host": routes::TEST_HOST,
            "bins": {
                "tailscaled": self.cfg.routes.tailscaled_bin,
                "tailscale": self.cfg.routes.tailscale_bin,
                "found": resolved.is_ok(),
                "problem": resolved.as_ref().err(),
                "hint": crate::routes_ts::INSTALL_HINT,
            },
        });
        if with_system {
            let known = lock(&self.routes.sys_nodes).clone();
            if known
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(30))
            {
                let (slot, home) = (
                    Arc::clone(&self.routes.sys_nodes),
                    crate::config::app_home(),
                );
                let _ = std::thread::Builder::new()
                    .name("route-nodes".into())
                    .spawn(move || {
                        let r = crate::routes_ts::system_exit_nodes(&bins, &home.join("tailscale"));
                        *lock(&slot) = Some((Instant::now(), r));
                    });
            }
            if let Some((_, Ok(n))) = known {
                out["system_nodes"] = json!(n);
            }
        }
        out
    }

    fn route_test(&mut self, name: &str) -> Result<(), String> {
        let r = self
            .cfg
            .routes
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no route called {name}"))?;
        let via = if r.is_tailscale() {
            self.route_ts_ensure(&r)?;
            if let Some(h) = self.routes.ts.get_mut(name) {
                h.hold_until = Some(Instant::now() + HOLD);
            }
            let snap = self.routes.ts[name].snapshot();
            if snap.state != TsState::Ready || !snap.proxy {
                lock(&self.routes.tests).insert(
                    name.to_string(),
                    json!({"state": "failed", "detail": format!("Not ready yet: {}", snap.state.detail())}),
                );
                return Ok(());
            }
            ProxyUrl::parse(&local(snap.port, "http"))?
        } else {
            ProxyUrl::parse(&r.url)?
        };
        lock(&self.routes.tests).insert(name.to_string(), json!({"state": "running"}));
        let (tests, name) = (Arc::clone(&self.routes.tests), name.to_string());
        std::thread::Builder::new()
            .name("route-test".into())
            .spawn(move || {
                let p = routes::probe(&via, routes::TEST_HOST);
                crate::log::info(&format!(
                    "routes: {name}: test {}",
                    if p.ok { "passed" } else { "failed" }
                ));
                lock(&tests).insert(
                    name,
                    json!({"state": if p.ok { "ok" } else { "failed" }, "detail": p.detail}),
                );
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn route_relaunch(&mut self, s: usize, t: usize) {
        let tab = &mut self.panes[s].tabs[t];
        let kind = match tab.route_block.take() {
            Some(b) => b.kind,
            None => tab.relaunch_kind(),
        };
        tab.kill();
        self.launch_tab(s, t, kind);
    }

    fn route_save(&mut self, args: &Value) -> Result<String, String> {
        let text = |k: &str| args[k].as_str().unwrap_or("").trim().to_string();
        let name = text("name").to_lowercase();
        let was = text("was");
        if !routes::valid_name(&name) {
            return Err(
                "a route name is 1 to 24 lowercase letters, digits, - or _ (and not \"direct\")"
                    .into(),
            );
        }
        let mut list = self.cfg.routes.list.clone();
        let old = list.iter().position(|r| {
            r.name
                == if was.is_empty() {
                    name.clone()
                } else {
                    was.clone()
                }
        });
        if !was.is_empty() && old.is_none() {
            return Err(format!("no route called {was}"));
        }
        if was.is_empty() && old.is_some() && args["replace"].as_bool() != Some(true) {
            return Err(format!("a route called {name} already exists"));
        }
        if !was.is_empty() && was != name && list.iter().any(|r| r.name == name) {
            return Err(format!("a route called {name} already exists"));
        }
        let kind = match text("kind").as_str() {
            "tailscale" => "tailscale",
            "custom" | "" => "custom",
            _ => return Err("a route is custom or tailscale".into()),
        };
        let prev = old.map(|i| list[i].clone());
        let mut r = RouteCfg {
            name: name.clone(),
            kind: kind.into(),
            ..Default::default()
        };
        if kind == "custom" {
            let url = text("url");
            r.url = if url.is_empty() {
                prev.as_ref()
                    .filter(|p| !p.is_tailscale())
                    .map(|p| p.url.clone())
                    .ok_or("the proxy URL is required")?
            } else {
                url
            };
            ProxyUrl::parse(&r.url)?;
        } else {
            let node = text("exit_node");
            if node.chars().count() > 120
                || node
                    .chars()
                    .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':')))
            {
                return Err("an exit node is a device name or IP address".into());
            }
            r.exit_node = node;
            r.allow_lan = args["allow_lan"].as_bool().unwrap_or(false);
        }
        match old {
            Some(i) => list[i] = r,
            None => list.push(r),
        }
        self.write_routes(list)?;
        lock(&self.routes.checks).remove(&name);
        lock(&self.routes.tests).remove(&name);
        if !was.is_empty() && was != name {
            for p in &mut self.panes {
                for tab in &mut p.tabs {
                    if tab.route.as_deref() == Some(was.as_str()) {
                        tab.route = Some(name.clone());
                        if tab.route_live.as_deref() == Some(was.as_str()) {
                            tab.route_live = Some(name.clone());
                        }
                    }
                }
            }
            if let Some(h) = self.routes.ts.remove(&was) {
                h.stop(true);
            }
            self.routes.bridges.remove(&was);
            self.state_dirty = true;
        }
        let n = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| t.route.as_deref() == Some(name.as_str()) && t.is_running())
            .count();
        Ok(if n > 0 && kind == "custom" && prev.is_some() {
            format!("Saved {name}. Running tabs keep the old proxy until they are reloaded ({n}).")
        } else {
            format!("Saved {name}")
        })
    }

    fn tab_route_tool(&mut self, args: &Value) -> Result<Value, String> {
        let op = args["op"].as_str().unwrap_or("view").to_string();
        let name = args["name"].as_str().unwrap_or("").trim().to_string();
        let say = match op.as_str() {
            "view" => {
                return Ok(self.routes_view(args["system"].as_bool() == Some(true)));
            }
            "set" => {
                let (s, t) = self.one_tab(args)?;
                let want = args["route"].as_str().unwrap_or("").trim().to_string();
                let new = (!want.is_empty() && want != "direct").then_some(want);
                if let Some(n) = &new {
                    if self.cfg.routes.get(n).is_none() {
                        return Err(format!("no route called {n}"));
                    }
                }
                let tab = &self.panes[s].tabs[t];
                let label = tab.name();
                let running = tab.is_running();
                let blocked = tab.route_block.is_some();
                if tab.route == new && !blocked && (!running || tab.route_live == new) {
                    return Ok(json!({"say": "Nothing changed", "view": self.routes_view(false)}));
                }
                if running && self.tab_busy(s, t) && args["force"].as_bool() != Some(true) {
                    return Ok(json!({"needs_confirm": true, "say": format!(
                        "{label} is working. Changing its route restarts it now and resumes the same conversation; the work in progress is interrupted."
                    )}));
                }
                let tab = &mut self.panes[s].tabs[t];
                tab.route = new.clone();
                tab.route_skip = false;
                self.state_dirty = true;
                if running || blocked {
                    self.route_relaunch(s, t);
                } else {
                    self.panes[s].tabs[t].route_block = None;
                }
                let tab = &self.panes[s].tabs[t];
                match (&new, &tab.route_block) {
                    (Some(n), Some(b)) => {
                        format!("{label} is set to {n} but not started: {}", b.reason)
                    }
                    (Some(n), None) if running => format!("{label} restarted through {n}"),
                    (Some(n), None) => format!("{label} will start through {n}"),
                    (None, _) if running || blocked => {
                        format!("{label} restarted with a direct connection")
                    }
                    (None, _) => format!("{label} uses a direct connection"),
                }
            }
            "retry" | "launch_direct" => {
                let (s, t) = self.one_tab(args)?;
                let tab = &mut self.panes[s].tabs[t];
                let label = tab.name();
                if tab.route_block.is_none() {
                    return Err("that tab is not waiting for its route".into());
                }
                tab.route_skip = op == "launch_direct";
                self.route_relaunch(s, t);
                let tab = &self.panes[s].tabs[t];
                match &tab.route_block {
                    Some(b) => format!("Still not started: {}", b.reason),
                    None if op == "launch_direct" => format!("{label} started without its route"),
                    None => format!("{label} started through its route"),
                }
            }
            "save" => self.route_save(args)?,
            "delete" => {
                let n = self.route_users(&name);
                if n > 0 {
                    return Err(format!(
                        "{n} tab{} use {name}: set them to Direct first",
                        if n == 1 { "" } else { "s" }
                    ));
                }
                let mut list = self.cfg.routes.list.clone();
                let before = list.len();
                list.retain(|r| r.name != name);
                if list.len() == before {
                    return Err(format!("no route called {name}"));
                }
                self.write_routes(list)?;
                if let Some(h) = self.routes.ts.remove(&name) {
                    h.stop(true);
                }
                self.routes.bridges.remove(&name);
                format!("Deleted {name}. Its tailscale login, if any, stays in the route folder until you remove the device from your tailnet.")
            }
            "bins" => {
                let (d, c) = (
                    args["tailscaled"].as_str().unwrap_or("").trim().to_string(),
                    args["tailscale"].as_str().unwrap_or("").trim().to_string(),
                );
                self.write_bin("routes.tailscaled_bin", &d)?;
                self.write_bin("routes.tailscale_bin", &c)?;
                self.cfg.routes.tailscaled_bin = d;
                self.cfg.routes.tailscale_bin = c;
                "Saved the Tailscale program paths".to_string()
            }
            "connect" => {
                let r = self
                    .cfg
                    .routes
                    .get(&name)
                    .filter(|r| r.is_tailscale())
                    .cloned()
                    .ok_or_else(|| format!("no tailscale route called {name}"))?;
                crate::routes_ts::resolve(&bins_of(&self.cfg.routes))?;
                self.route_ts_ensure(&r)?;
                if let Some(h) = self.routes.ts.get_mut(&name) {
                    h.hold_until = Some(Instant::now() + HOLD);
                }
                format!("Starting {name}")
            }
            "test" => {
                self.route_test(&name)?;
                format!("Testing {name}")
            }
            _ => return Err(format!("unknown op {op}")),
        };
        Ok(json!({"say": say, "view": self.routes_view(false)}))
    }

    pub(crate) fn tab_route_call(&mut self, args: &Value) -> Value {
        if !self.web_caller {
            crate::log::info("control: tab_route refused: not the user's own press in the web app");
            return json!({"ok": false, "error": "refused: routes are chosen only by the user, in the web app's Route menu and Manage routes. An agent never sets, changes or clears a route: ask the user to do it there."});
        }
        crate::log::info(&format!(
            "control: tab_route {} {} {}",
            args["op"].as_str().unwrap_or("view"),
            args["tab"].as_str().unwrap_or(""),
            args["name"]
                .as_str()
                .or(args["route"].as_str())
                .unwrap_or("")
        ));
        match self.tab_route_tool(args) {
            Ok(v) => json!({"ok": true, "result": v}),
            Err(e) => json!({"ok": false, "error": e}),
        }
    }
}
