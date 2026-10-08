//! Choosing the assistant's provider, account and model at any time (the
//! title menu, voice, set_setting), keeping the conversation across the
//! switch, the Grok brain's own login, and falling back to Claude when a
//! provider's login is refused.

use serde_json::{json, Value};

use crate::app::App;
use crate::providers::{self, PROVIDERS};

impl App {
    /// The recent exchanges, carried into a new brain.
    pub fn conversation_carry(&self) -> Option<String> {
        use crate::app_assistant::Who;
        let last: Vec<String> = self
            .assistant
            .log
            .iter()
            .rev()
            .filter(|e| matches!(e.who, Who::User | Who::Reply))
            .take(8)
            .map(|e| {
                let who = if e.who == Who::User { "user" } else { "you" };
                format!("{who}: {}", e.text.chars().take(240).collect::<String>())
            })
            .collect();
        (!last.is_empty()).then(|| last.into_iter().rev().collect::<Vec<_>>().join(" / "))
    }

    /// Switch provider / account / model (each optional); the next turn
    /// starts the new brain with the conversation so far.
    pub fn switch_assistant(
        &mut self,
        provider: Option<&str>,
        account: Option<&Value>,
        model: Option<&str>,
    ) -> Result<String, String> {
        let cur = self.cfg.assistant.provider.clone();
        let p = match provider {
            Some(id) => {
                let id = id.trim().to_lowercase();
                PROVIDERS
                    .iter()
                    .find(|p| p.id == id || p.name.to_lowercase() == id)
                    .ok_or_else(|| {
                        format!(
                            "providers: {}",
                            PROVIDERS
                                .iter()
                                .map(|p| p.id)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?
            }
            None => providers::by_id(&cur),
        };
        let write = |key: &'static str, v: &str| {
            let _ = crate::settings::write(
                &crate::config::Config::path(),
                &crate::settings::Key::Global(key),
                Some(toml_edit::value(v)),
            );
        };
        if p.id != cur {
            write("assistant.provider", p.id);
            self.cfg.assistant.provider = p.id.to_string();
        }
        if let Some(a) = account.filter(|a| !a.is_null()) {
            let h = p
                .harness
                .ok_or_else(|| format!("{} uses its own login, not an account", p.name))?;
            let i = self.account_arg(a)?;
            if self.cfg.accounts[i].harness() != h {
                return Err(format!(
                    "{} is not a {} account",
                    self.cfg.accounts[i].display(),
                    p.name
                ));
            }
            let name = self.cfg.accounts[i].name.clone();
            write("assistant.account", &name);
            self.cfg.assistant.account = name;
        }
        if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
            let m = p
                .models
                .iter()
                .find(|(id, label)| {
                    id.eq_ignore_ascii_case(m)
                        || label.to_lowercase().starts_with(&m.to_lowercase())
                })
                .map(|(id, _)| id.to_string())
                .unwrap_or_else(|| m.to_string());
            if p.id == "claude" {
                write("assistant.model", &m);
                self.cfg.assistant.model = m;
            } else {
                let _ = crate::settings::write(
                    &crate::config::Config::path(),
                    &crate::settings::Key::Global(if p.id == "grok" {
                        "assistant.models.grok"
                    } else {
                        "assistant.models.other"
                    }),
                    Some(toml_edit::value(m.clone())),
                );
                self.cfg.assistant.models.insert(p.id.to_string(), m);
            }
        }
        self.config_mtime = crate::app::config_mtime();
        let had_remote = self.assistant.remote.clone();
        if had_remote.is_some() {
            self.remote_lost("the assistant switched");
        }
        // The conversation goes along.
        if let Some(c) = self.conversation_carry() {
            self.assistant.carry = Some(c);
        }
        if !self.assistant.busy {
            self.assistant.brain = None;
        }
        self.assistant.account_after = None;
        let model = providers::model_for(p, &self.cfg.assistant);
        let whom = match p.harness {
            None => format!("its own {} login", p.name),
            Some(_) => self
                .assistant_account()
                .map(|a| self.cfg.accounts[a].display().to_string())
                .unwrap_or_else(|| "no logged in account".into()),
        };
        crate::log::info(&format!(
            "assistant: switched to {} ({whom}, {model})",
            p.name
        ));
        let mut say = format!("Switched to {} ({whom}, {model}).", p.name);
        if had_remote.is_some() {
            say.push_str(if p.caps.remote_control {
                " Remote Control ended with the switch; say turn on remote control to start it again."
            } else {
                " Remote Control ended with the switch (it works only on Claude)."
            });
        }
        if let Some(home) = p.own_home {
            if !crate::harness::grok::logged_in(&home()) {
                say.push_str(&format!(
                    " Its {} login is missing: say log in the assistant's {}.",
                    p.name, p.name
                ));
            }
        }
        Ok(say)
    }

    /// The state block's line on the assistant itself.
    pub fn provider_state_line(&self) -> String {
        let p = providers::by_id(&self.cfg.assistant.provider);
        let model = providers::model_for(p, &self.cfg.assistant);
        let on = match p.own_home {
            Some(_) if providers::grok::logged_in() => "its own login".to_string(),
            Some(_) => "its own login, NOT signed in (assistant_login)".to_string(),
            None => self
                .assistant_account()
                .map(|a| format!("a{}", a + 1))
                .unwrap_or_else(|| "no logged in account".into()),
        };
        format!(
            "you run on: {} ({on}, {model}); switch_assistant changes provider ({}), account or model\n",
            p.name,
            PROVIDERS.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
        )
    }

    /// The providers, their accounts and models (for get_state and menus).
    pub fn providers_json(&self) -> Value {
        json!(PROVIDERS
            .iter()
            .map(|p| {
                let accounts: Vec<Value> = match p.harness {
                    Some(h) => (0..self.cfg.accounts.len())
                        .filter(|&a| self.cfg.accounts[a].harness() == h)
                        .map(|a| json!({"account": a + 1, "label": self.cfg.accounts[a].display(), "logged_in": self.accounts[a].login.logged_in()}))
                        .collect(),
                    None => vec![json!({"label": format!("{}'s own login", p.name), "logged_in": p.own_home.map(|h| crate::harness::grok::logged_in(&h())).unwrap_or(false)})],
                };
                json!({
                    "provider": p.id,
                    "name": p.name,
                    "active": self.cfg.assistant.provider == p.id,
                    "models": p.models.iter().map(|(id, l)| json!({"id": id, "label": l})).collect::<Vec<_>>(),
                    "model": providers::model_for(p, &self.cfg.assistant),
                    "accounts": accounts,
                    "persistent_process": p.caps.persistent,
                    "streams": p.caps.partial,
                    "efforts": p.caps.efforts,
                })
            })
            .collect::<Vec<_>>())
    }

    /// A provider's login was refused: say so once and go back to Claude.
    pub fn provider_refused(&mut self, why: &str) {
        let p = providers::by_id(&self.cfg.assistant.provider);
        if p.id == "claude" {
            return;
        }
        crate::log::info(&format!(
            "assistant: {} refused ({why}); back to Claude",
            p.name
        ));
        let _ = self.switch_assistant(Some("claude"), None, None);
        let msg = format!(
            "The assistant's {} login was refused ({}). Switched back to Claude; say log in the assistant's {} to try again.",
            p.name,
            crate::sessions::snippet(why, 80),
            p.name
        );
        self.assistant_note(msg.clone());
        self.flash(msg);
    }

    /// The Grok brain's own login: `grok login` in its home, the sign in
    /// page opened when grok cannot, progress spoken.
    pub fn assistant_grok_login(&mut self) -> Result<String, String> {
        let home = providers::grok::home();
        std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        let bin = crate::harness::Harness::Grok.bin(self.cfg.grok_bin.as_deref());
        self.admin_record("login started for the assistant's Grok");
        self.start_cmd_login(bin, vec!["login".into()], home, "the assistant's Grok");
        Ok("Starting the assistant's Grok login; sign in in your browser.".into())
    }
}
