use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub const PROTOCOL: &str = "2025-06-18";

#[cfg(test)]
pub fn handle(
    req: &Value,
    call: &mut dyn FnMut(&str, &Value) -> anyhow::Result<Value>,
) -> Option<Value> {
    handle_as(req, call, false)
}

pub fn handle_as(
    req: &Value,
    call: &mut dyn FnMut(&str, &Value) -> anyhow::Result<Value>,
    tab: bool,
) -> Option<Value> {
    let id = req.get("id").cloned()?;
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => {
            let v = req
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL);
            json!({
                "protocolVersion": v,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "godterm", "version": env!("CARGO_PKG_VERSION")},
                "instructions": if tab {
                    "Manage this tab's own scheduled jobs in GodTerm (they survive restarts and show in Loops & schedules). The user approves each change in GodTerm."
                } else {
                    "Tools to see and drive godterm: its accounts, tabs, prompts, usage, layout and loops."
                },
            })
        }
        "ping" => json!({}),
        "tools/list" if tab => json!({"tools": crate::tab_jobs::tab_tool_list()}),
        "tools/list" => json!({"tools": crate::control::TOOLS.iter().map(|t| json!({
            "name": t.name,
            "description": t.description,
            "inputSchema": (t.schema)(),
        })).collect::<Vec<_>>()}),
        "tools/call" => {
            let name = req
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = req
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            let refused = tab && !crate::tab_jobs::TAB_TOOLS.contains(&name);
            match if refused {
                Err(anyhow::anyhow!("unknown tool {name}"))
            } else {
                call(name, &args)
            } {
                Ok(v) => {
                    let failed =
                        v.get("ok") == Some(&json!(false)) && v.get("needs_confirmation").is_none();
                    json!({"content": [{"type": "text", "text": v.to_string()}], "isError": failed})
                }
                Err(e) => {
                    json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true})
                }
            }
        }
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {method}")}}),
            );
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub fn run() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let tab = crate::config::env_var("CLIENT").is_some_and(|c| c == "tab");
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => handle_as(&req, &mut |t, a| crate::control::call(t, a), tab),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            ),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_and_calls() {
        let calls = std::cell::RefCell::new(vec![]);
        let mut call = |t: &str, a: &Value| {
            calls.borrow_mut().push((t.to_string(), a.clone()));
            Ok(json!({"ok": true, "result": [1, 2]}))
        };
        let init = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}), &mut call).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "godterm");
        assert!(handle(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            &mut call
        )
        .is_none());
        let list = handle(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            &mut call,
        )
        .unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert!(tools
            .iter()
            .any(|t| t["name"] == "answer_prompt" && t["inputSchema"]["type"] == "object"));
        let r = handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "list_tabs", "arguments": {}}}), &mut call).unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"ok\":true"));
        assert_eq!(calls.borrow()[0].0, "list_tabs");
        let bad = handle(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "nope"}),
            &mut call,
        )
        .unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }
}
