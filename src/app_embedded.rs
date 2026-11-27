use std::sync::Arc;

use serde_json::{json, Value};

use crate::app::App;
use crate::embedded::{self, Ctx};

impl App {
    pub fn embedded_ctx(&self, uid: u64) -> Result<Ctx, String> {
        if !self.cfg.web.embedded_browser {
            return Err("the embedded browser is off: the user turns it on in GodTerm's settings (Web app, Embedded browser)".into());
        }
        if self.find_tab(uid).is_none() {
            return Err("this tab is gone".into());
        }
        self.embedded_limits();
        Ok(Ctx {
            uid,
            bin: self.cfg.web.embedded_browser_bin.clone(),
        })
    }

    fn embedded_limits(&self) {
        embedded::set_page_cap(self.cfg.web.embedded_browser_max_pages);
        embedded::set_sleep_after(std::time::Duration::from_secs(
            self.cfg.web.embedded_browser_sleep_minutes.min(1440) * 60,
        ));
    }

    pub fn embedded_tool_ctx(&self, uid: u64) -> Value {
        match self.embedded_ctx(uid) {
            Ok(c) => json!({"ok": true, "_embedded": {"uid": c.uid, "bin": c.bin}}),
            Err(e) => json!({"ok": false, "error": e}),
        }
    }

    pub fn web_embedded(&mut self, client: u64, v: Value) {
        let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) else {
            return;
        };
        let uid = v["tab"].as_u64().or_else(|| {
            v["tab"]
                .as_str()
                .and_then(|t| t.trim_start_matches('t').parse().ok())
        });
        let fail = |e: &str| {
            c.out.text(
                json!({"t": "ebv", "id": v["id"], "tab": v["tab"], "op": v["op"], "ok": false, "error": e})
                    .to_string(),
            );
        };
        let Some(uid) = uid else {
            return fail("no such tab");
        };
        let closing = matches!(v["op"].as_str(), Some("close" | "kill" | "keep"));
        let ctx = match self.embedded_ctx(uid) {
            Ok(c) => c,
            Err(_) if closing && self.find_tab(uid).is_some() => Ctx {
                uid,
                bin: String::new(),
            },
            Err(e) => return fail(&e),
        };
        let out = Arc::clone(&c.out);
        let _ = std::thread::Builder::new()
            .name("embedded-req".into())
            .spawn(move || embedded::handle_web(&ctx, client, &out, &v));
    }

    pub fn embedded_tick(&mut self) {
        let live: Vec<u64> = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| t.uid))
            .collect();
        self.embedded_limits();
        if embedded::tick(&live, self.cfg.web.embedded_browser) {
            self.web_changed();
        }
    }
}
