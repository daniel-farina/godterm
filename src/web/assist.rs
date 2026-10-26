//! The assistant and the web app: a turn typed in a browser says so in its
//! <state> (which browser, phone or desktop, its panes and open sheet),
//! and the web_* tools change that browser's view with a `ui` frame.

use crate::app::App;
use serde_json::{json, Value};

const TOOLS: &[&str] = &["web_layout", "web_show", "web_view", "web_font"];

impl App {
    /// The <state> line for a turn typed in the web app (empty otherwise).
    pub fn web_turn_line(&self) -> String {
        let Some(id) = self.assistant.web_client else {
            return String::new();
        };
        let view = self
            .web
            .as_ref()
            .and_then(|w| w.hub.client(id))
            .map(|c| c.view.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .unwrap_or(Value::Null);
        format!(
            "web: this request was typed in the web app (browser {id}{}). Its view is its own, apart from the terminal UI's: layout words (\"split 4 ways\", \"side by side\", \"open X\", \"show the chat\") mean this browser: use web_layout, web_show, web_view, web_font, not the terminal UI's layout tools, unless the user names the terminal, the Mac or the GodTerm window.\n",
            describe(&view)
        )
    }

    /// web_layout, web_show, web_view, web_font.
    pub fn web_tool(&mut self, tool: &str, args: &Value) -> Option<Result<Value, String>> {
        if !TOOLS.contains(&tool) {
            return None;
        }
        Some(self.web_tool_run(tool, args))
    }

    fn web_tool_run(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        let Some(w) = self.web.as_ref() else {
            return Err("the web app is off: no browser to change".into());
        };
        let ready: Vec<_> = w
            .hub
            .clients()
            .into_iter()
            .filter(|c| c.ready.load(std::sync::atomic::Ordering::Relaxed))
            .collect();
        let targets: Vec<_> = match &args["client"] {
            Value::String(s) if s == "all" => ready,
            Value::Number(n) => ready
                .into_iter()
                .filter(|c| Some(c.id) == n.as_u64())
                .collect(),
            _ => match self.assistant.web_client {
                Some(id) => ready.into_iter().filter(|c| c.id == id).collect(),
                None if ready.len() == 1 => ready,
                None if ready.is_empty() => vec![],
                None => {
                    return Err(format!(
                        "{} browsers are open: say which (client: an id, or \"all\")",
                        ready.len()
                    ))
                }
            },
        };
        if targets.is_empty() {
            return Err("no web app is open in a browser now".into());
        }
        let tab_of = |app: &mut App, v: &Value| -> Result<u64, String> {
            let id = v.as_str().ok_or("tabs are ids like t12")?;
            let a = json!({ "tab": id });
            let (s, t) = app.one_tab(&a)?;
            Ok(app.panes[s].tabs[t].uid)
        };
        let phone = |c: &super::Client| {
            c.view.lock().unwrap_or_else(|e| e.into_inner())["device"] == "phone"
        };
        let (frame, say) = match tool {
            "web_layout" => {
                let tabs: Vec<u64> = args["tabs"]
                    .as_array()
                    .map(|a| a.iter().map(|v| tab_of(self, v)).collect::<Result<_, _>>())
                    .transpose()?
                    .unwrap_or_default();
                let n = args["panes"].as_u64().unwrap_or(match tabs.len() {
                    0 | 1 => 1,
                    2 => 2,
                    _ => 4,
                });
                if ![1, 2, 4].contains(&n) {
                    return Err("panes is 1, 2 or 4".into());
                }
                if tabs.len() > n as usize {
                    return Err(format!("{} tabs for {n} panes", tabs.len()));
                }
                let names: Vec<String> = tabs.iter().map(|u| self.spoken_tab_name(*u)).collect();
                let mut say = match (n, names.is_empty()) {
                    (1, true) => "One pane in the web app.".to_string(),
                    (_, true) => format!("{n} panes in the web app."),
                    _ => format!("The web app shows {} in {n} panes.", join(&names)),
                };
                if n > 1 && targets.iter().any(|c| phone(c)) {
                    say.push_str(
                        " A phone shows one pane at a time; the others are in its tab list.",
                    );
                }
                (
                    json!({"t": "ui", "action": "layout", "panes": n, "tabs": tabs}),
                    say,
                )
            }
            "web_show" => {
                let u = tab_of(self, &args["tab"])?;
                let pane = args["pane"].as_u64();
                (
                    json!({"t": "ui", "action": "show", "tab": u, "pane": pane}),
                    format!("Showing {} in the web app.", self.spoken_tab_name(u)),
                )
            }
            "web_view" => {
                let v = args["view"].as_str().unwrap_or("");
                if ![
                    "terminal",
                    "chat",
                    "sessions",
                    "manage",
                    "approvals",
                    "settings",
                    "accounts",
                ]
                .contains(&v)
                {
                    return Err(
                        "view is terminal, chat, sessions, manage, approvals, settings or accounts"
                            .into(),
                    );
                }
                (
                    json!({"t": "ui", "action": "view", "view": v}),
                    format!("Opened {v} in the web app."),
                )
            }
            _ => {
                if args["fit"].as_bool() == Some(true) {
                    (
                        json!({"t": "ui", "action": "font", "fit": true}),
                        "The web app's font is back to its default.".to_string(),
                    )
                } else {
                    let f = args["size"].as_f64().ok_or("size (px) or fit: true")?;
                    if !(8.0..=30.0).contains(&f) {
                        return Err("size is 8 to 30 px".into());
                    }
                    (
                        json!({"t": "ui", "action": "font", "size": f}),
                        format!("The web app's font is {f} px."),
                    )
                }
            }
        };
        let text = frame.to_string();
        for c in &targets {
            c.out.text(text.clone());
        }
        crate::log::info(&format!("web: {tool} to {} browser(s)", targets.len()));
        Ok(
            json!({"ok": true, "result": {"clients": targets.iter().map(|c| c.id).collect::<Vec<_>>(), "say": say}}),
        )
    }
}

/// "phone, 1 pane: t3; sheet sessions".
fn describe(v: &Value) -> String {
    if v.is_null() {
        return String::new();
    }
    let mut parts = vec![];
    if let Some(d) = v["device"].as_str() {
        parts.push(d.to_string());
    }
    let panes = v["panes"].as_u64().unwrap_or(1);
    let tabs: Vec<String> = v["tabs"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(panes as usize)
                .enumerate()
                .map(|(i, t)| match t.as_u64() {
                    Some(u) => format!("{} t{u}", i + 1),
                    None => format!("{} empty", i + 1),
                })
                .collect()
        })
        .unwrap_or_default();
    parts.push(format!(
        "{panes} pane{}{}",
        if panes == 1 { "" } else { "s" },
        if tabs.is_empty() {
            String::new()
        } else {
            format!(": {}", tabs.join(", "))
        }
    ));
    if let Some(f) = v["focused"].as_u64() {
        parts.push(format!("focused pane {}", f + 1));
    }
    if let Some(s) = v["open"].as_str().filter(|s| !s.is_empty()) {
        parts.push(format!("open: {s}"));
    }
    format!(", {}", parts.join(", "))
}

fn join(v: &[String]) -> String {
    match v.len() {
        0 => String::new(),
        1 => v[0].clone(),
        n => format!("{} and {}", v[..n - 1].join(", "), v[n - 1]),
    }
}
