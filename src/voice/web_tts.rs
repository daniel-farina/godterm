use std::sync::Mutex;

use serde_json::{json, Value};

use crate::config::VoiceCfg;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebTts {
    pub engine: String,
    pub voice: String,
}

impl WebTts {
    pub fn new(engine: &str, voice: &str) -> WebTts {
        WebTts {
            engine: engine.into(),
            voice: voice.into(),
        }
    }

    pub fn json(&self) -> Value {
        json!({"engine": self.engine, "voice": self.voice})
    }
}

pub const ENGINES: &[&str] = &["kokoro", "chatterbox", "ming", "say", "grok", "off", "mac"];

pub fn default_choice() -> WebTts {
    WebTts::new("kokoro", "af_heart")
}

pub fn parse(v: &Value) -> Result<WebTts, String> {
    if v.is_null() {
        return Ok(WebTts::new("mac", ""));
    }
    let Some(o) = v.as_object() else {
        return Err("web_tts must be {engine, voice} or null".into());
    };
    let engine = o
        .get("engine")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let voice = match o.get("voice") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(_) => return Err("voice must be a string".into()),
    };
    if !ENGINES.contains(&engine.as_str()) {
        return Err(if engine.is_empty() {
            "no engine given".to_string()
        } else {
            format!("unknown engine {engine:?} (one of {})", ENGINES.join(", "))
        });
    }
    if voice.chars().count() > 64
        || voice
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(format!("{voice:?} is not a voice name"));
    }
    let ok = match engine.as_str() {
        "kokoro" => {
            voice.is_empty()
                || (voice.len() > 3
                    && voice.as_bytes()[2] == b'_'
                    && voice
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        }
        "ming" => voice.is_empty() || super::catalog::ming_choices().contains(&voice),
        "chatterbox" => matches!(voice.as_str(), "" | "default" | "clone"),
        _ => true,
    };
    if !ok {
        return Err(format!("{voice:?} is not a {engine} voice"));
    }
    Ok(WebTts { engine, voice })
}

pub fn web_cfg(mac: &VoiceCfg, choice: Option<&WebTts>) -> VoiceCfg {
    let c = choice.cloned().unwrap_or_else(default_choice);
    if c.engine == "mac" {
        return mac.clone();
    }
    let mut v = mac.clone();
    let voice = c.voice.as_str();
    v.tts_engine = c.engine.clone();
    match c.engine.as_str() {
        "kokoro" => {
            v.kokoro_voice = if voice.is_empty() { "af_heart" } else { voice }.into();
        }
        "grok" => {
            v.grok.voice = if voice.is_empty() {
                crate::config::GrokTtsCfg::default().voice
            } else {
                voice.into()
            };
        }
        "say" => {
            v.tts_voice = (!voice.is_empty() && voice != "system").then(|| voice.to_string());
        }

        "chatterbox" if voice != "clone" => v.local.chatterbox_ref = String::new(),
        "ming" => {
            v.local.ming_voice = if voice.is_empty() {
                crate::config::LocalTtsCfg::default().ming_voice
            } else {
                voice.into()
            };
        }
        _ => {}
    }
    v
}

pub fn mac_choice(mac: &VoiceCfg) -> WebTts {
    let voice = match mac.tts_engine.as_str() {
        "kokoro" => mac.kokoro_voice.clone(),
        "grok" => mac.grok.voice.clone(),
        "say" => mac.tts_voice.clone().unwrap_or_default(),
        "chatterbox" if !mac.local.chatterbox_ref.trim().is_empty() => "clone".into(),
        "ming" => mac.local.ming_voice.clone(),
        _ => String::new(),
    };
    WebTts::new(&mac.tts_engine, &voice)
}

struct Route {
    client: u64,
    choice: Option<WebTts>,
}

static ROUTE: Mutex<Option<Route>> = Mutex::new(None);

pub fn set_route(r: Option<(u64, Option<WebTts>)>) {
    *ROUTE.lock().unwrap_or_else(|e| e.into_inner()) =
        r.map(|(client, choice)| Route { client, choice });
}

pub fn choice_changed(client: u64, choice: Option<WebTts>) {
    if let Some(r) = ROUTE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        if r.client == client {
            r.choice = choice;
        }
    }
}

pub fn route() -> Option<(u64, Option<WebTts>)> {
    ROUTE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|r| (r.client, r.choice.clone()))
}

fn region(id: &str) -> &'static str {
    match id.get(..1) {
        Some("b") => "UK",
        _ => "US",
    }
}

pub fn kokoro_voice_label(id: &str) -> String {
    let name = super::catalog::kokoro_name(id);
    match super::catalog::KOKORO_EN.iter().find(|v| v.0 == id) {
        Some((_, _, g, _)) => format!("{name} ({}, {g})", region(id)),
        None => name,
    }
}

pub fn grok_problem(mac: &VoiceCfg) -> Option<String> {
    let g = &mac.grok;
    if g.auth == "api_key" {
        return (super::grok_tts::key_saved() == Some(false))
            .then(|| "no xAI API key saved (Settings > Voice and Audio)".to_string());
    }
    let all = super::grok_tts::sources_now();
    match super::grok_tts::pick_source(&g.source, &all) {
        Some(s) if s.logged_in => None,
        Some(s) => Some(format!("{} is not logged in to grok", s.label)),
        None if g.source.is_empty() || g.source == "auto" => {
            Some("no grok account is logged in".into())
        }
        None => Some(format!("no grok login named {}", g.source)),
    }
}

pub struct Lists {
    pub kokoro: Vec<String>,
    pub say: Vec<String>,
    pub grok: Vec<String>,
}

impl Lists {
    pub fn now(mac: &VoiceCfg) -> Lists {
        let cached = crate::settings::AUDIO_LISTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let kokoro = cached
            .as_ref()
            .map(|l| l.kokoro_voices.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                super::tts::kokoro_voices(&crate::config::expand_tilde(&mac.kokoro_voices))
            });
        let say = cached
            .as_ref()
            .map(|l| l.say_voices.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                if cfg!(test) {
                    vec![]
                } else {
                    super::say_voices()
                }
            });
        let grok = cached.map(|l| l.grok_voices).unwrap_or_default();
        Lists { kokoro, say, grok }
    }
}

fn voice(id: &str, label: String) -> Value {
    json!({"id": id, "label": label})
}

fn engine(id: &str, label: &str, voices: Vec<Value>, problem: Option<String>) -> Value {
    json!({
        "engine": id,
        "label": label,
        "voices": voices,
        "available": problem.is_none(),
        "note": problem,
    })
}

pub fn list(mac: &VoiceCfg, lists: &Lists, choice: Option<&WebTts>) -> Value {
    let mut engines = vec![];
    let kokoro: Vec<Value> = super::catalog::kokoro_choices(&lists.kokoro)
        .iter()
        .map(|id| voice(id, kokoro_voice_label(id)))
        .collect();
    engines.push(engine(
        "kokoro",
        "Kokoro (this Mac, local)",
        kokoro,
        super::tts::kokoro_problem(mac),
    ));
    if super::local_tts::supported() {
        let mut cb = vec![voice("", "Default voice".into())];
        if !mac.local.chatterbox_ref.trim().is_empty() {
            cb.push(voice(
                "clone",
                format!(
                    "{} (as on the Mac)",
                    super::local_tts::voice_label("chatterbox", mac)
                ),
            ));
        }
        engines.push(engine(
            "chatterbox",
            "Chatterbox Turbo (this Mac, local)",
            cb,
            super::local_tts::problem("chatterbox", mac),
        ));
        let mut ming: Vec<Value> = super::local_tts::MING_VOICES
            .iter()
            .map(|id| {
                let (r, name) = id.split_once('_').unwrap_or(("US", id));
                let g = super::catalog::ming_label(id)
                    .and_then(|l| l.split(' ').nth(1))
                    .unwrap_or("");
                voice(id, format!("{name} ({r}, {g})"))
            })
            .collect();
        if !mac.local.ming_design.trim().is_empty() {
            ming.push(voice(
                "design",
                "Designed voice (as on the Mac)".to_string(),
            ));
        }
        engines.push(engine(
            "ming",
            "Ming-3.0 (this Mac, local)",
            ming,
            super::local_tts::problem("ming", mac),
        ));
    }
    let mut say = vec![voice("", "System voice".into())];
    say.extend(lists.say.iter().map(|n| voice(n, n.clone())));
    engines.push(engine(
        "say",
        "macOS say (this Mac, local)",
        say,
        (super::tts::which("say").is_none()).then(|| "say is not on this computer".to_string()),
    ));
    let names: Vec<String> = if lists.grok.is_empty() {
        super::grok_tts::KNOWN_VOICES
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        lists.grok.clone()
    };
    let grok: Vec<Value> = names
        .iter()
        .map(|id| voice(id, super::catalog::kokoro_name(id)))
        .collect();
    engines.push(engine(
        "grok",
        "Grok (xAI cloud: the spoken text is sent to xAI)",
        grok,
        grok_problem(mac),
    ));
    json!({
        "t": "voice",
        "web_tts_list": engines,
        "default": default_choice().json(),
        "mac": mac_choice(mac).json(),
        "choice": choice.map(WebTts::json),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_choice() {
        assert_eq!(
            parse(&json!({"engine": "kokoro", "voice": "bf_emma"})).unwrap(),
            WebTts::new("kokoro", "bf_emma")
        );
        assert_eq!(parse(&Value::Null).unwrap(), WebTts::new("mac", ""));
        assert_eq!(
            parse(&json!({"engine": "mac"})).unwrap(),
            WebTts::new("mac", "")
        );
        assert_eq!(
            parse(&json!({"engine": "grok", "voice": "ara"})).unwrap(),
            WebTts::new("grok", "ara")
        );
        assert_eq!(
            parse(&json!({"engine": "say", "voice": "Samantha"})).unwrap(),
            WebTts::new("say", "Samantha")
        );
        assert!(parse(&json!({"engine": "espeak", "voice": "x"}))
            .unwrap_err()
            .contains("unknown engine"));
        assert!(parse(&json!({"voice": "x"})).is_err());
        assert!(parse(&json!("kokoro")).is_err());
        assert!(parse(&json!({"engine": "kokoro", "voice": "Heart"})).is_err());
        assert!(parse(&json!({"engine": "ming", "voice": "nobody"})).is_err());
        assert!(parse(&json!({"engine": "say", "voice": "../x"})).is_err());
        assert!(parse(&json!({"engine": "kokoro", "voice": 3})).is_err());
    }

    #[test]
    fn browser_settings_replace_only_the_engine_and_voice() {
        let mac = VoiceCfg {
            tts_engine: "grok".into(),
            kokoro_voice: "bm_george".into(),
            tts_voice: Some("Daniel".into()),
            tts_speed: 1.3,
            ..Default::default()
        };

        let d = web_cfg(&mac, None);
        assert_eq!(
            (d.tts_engine.as_str(), d.kokoro_voice.as_str()),
            ("kokoro", "af_heart")
        );
        assert_eq!(d.tts_speed, 1.3);
        assert_eq!(d.tts_fallback, mac.tts_fallback);
        assert_eq!(d.grok, mac.grok);

        assert_eq!(web_cfg(&mac, Some(&WebTts::new("mac", ""))), mac);
        let k = web_cfg(&mac, Some(&WebTts::new("kokoro", "bf_emma")));
        assert_eq!(
            (k.tts_engine.as_str(), k.kokoro_voice.as_str()),
            ("kokoro", "bf_emma")
        );
        assert_eq!(
            web_cfg(&mac, Some(&WebTts::new("kokoro", ""))).kokoro_voice,
            "af_heart"
        );
        let g = web_cfg(&mac, Some(&WebTts::new("grok", "ara")));
        assert_eq!(
            (g.tts_engine.as_str(), g.grok.voice.as_str()),
            ("grok", "ara")
        );
        assert_eq!(g.kokoro_voice, "bm_george", "the rest stays the Mac's");
        assert_eq!(
            web_cfg(&mac, Some(&WebTts::new("grok", ""))).grok.voice,
            "eve"
        );
        let s = web_cfg(&mac, Some(&WebTts::new("say", "Samantha")));
        assert_eq!(
            (s.tts_engine.as_str(), s.tts_voice.as_deref()),
            ("say", Some("Samantha"))
        );
        assert_eq!(web_cfg(&mac, Some(&WebTts::new("say", ""))).tts_voice, None);
        let mut m2 = mac.clone();
        m2.local.chatterbox_ref = "/clips/me.wav".into();
        assert_eq!(
            web_cfg(&m2, Some(&WebTts::new("chatterbox", "")))
                .local
                .chatterbox_ref,
            ""
        );
        assert_eq!(
            web_cfg(&m2, Some(&WebTts::new("chatterbox", "clone")))
                .local
                .chatterbox_ref,
            "/clips/me.wav"
        );
        assert_eq!(
            web_cfg(&mac, Some(&WebTts::new("ming", "UK_George")))
                .local
                .ming_voice,
            "UK_George"
        );

        let gone = VoiceCfg {
            kokoro_model: "/nonexistent/kokoro.onnx".into(),
            tts_engine: "say".into(),
            ..Default::default()
        };
        assert_eq!(
            super::super::engines::chain(&web_cfg(&gone, None)),
            vec!["say"]
        );
        assert_eq!(mac_choice(&mac), WebTts::new("grok", "eve"));
        assert_eq!(
            mac_choice(&VoiceCfg::default()),
            WebTts::new("kokoro", "af_heart")
        );
    }

    #[test]
    fn the_list_has_every_engine_with_voices() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mac = VoiceCfg {
            kokoro_model: "/nonexistent/kokoro.onnx".into(),
            ..Default::default()
        };
        let lists = Lists {
            kokoro: vec![],
            say: vec!["Samantha".into()],
            grok: vec![],
        };
        let v = list(&mac, &lists, Some(&WebTts::new("say", "Samantha")));
        assert_eq!(v["t"], "voice");
        assert_eq!(
            v["default"],
            json!({"engine": "kokoro", "voice": "af_heart"})
        );
        assert_eq!(v["mac"], json!({"engine": "kokoro", "voice": "af_heart"}));
        assert_eq!(v["choice"], json!({"engine": "say", "voice": "Samantha"}));
        let e = v["web_tts_list"].as_array().unwrap();
        let ids: Vec<&str> = e.iter().map(|x| x["engine"].as_str().unwrap()).collect();
        assert_eq!(ids.first(), Some(&"kokoro"));
        for id in ["kokoro", "say", "grok"] {
            assert!(ids.contains(&id), "{ids:?}");
        }
        let k = &e[0];
        assert_eq!(k["label"], "Kokoro (this Mac, local)");
        assert_eq!(k["available"], false);
        assert!(k["note"].as_str().unwrap().contains("model missing"));
        assert_eq!(
            k["voices"][0],
            json!({"id": "af_heart", "label": "Heart (US, female)"})
        );
        assert_eq!(k["voices"].as_array().unwrap().len(), 28);
        let say = e.iter().find(|x| x["engine"] == "say").unwrap();
        assert_eq!(say["voices"][0]["id"], "");
        assert_eq!(
            say["voices"][1],
            json!({"id": "Samantha", "label": "Samantha"})
        );
        let grok = e.iter().find(|x| x["engine"] == "grok").unwrap();
        assert_eq!(grok["voices"][0], json!({"id": "eve", "label": "Eve"}));

        assert_eq!(grok["available"], false);
        assert!(grok["note"].as_str().unwrap().contains("logged in"));
        assert_eq!(kokoro_voice_label("bm_george"), "George (UK, male)");
        assert!(list(&mac, &lists, None)["choice"].is_null());
    }

    #[test]
    fn a_choice_applies_only_to_the_voice_browser() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        set_route(Some((3, None)));
        choice_changed(4, Some(WebTts::new("grok", "ara")));
        assert_eq!(route(), Some((3, None)));
        choice_changed(3, Some(WebTts::new("kokoro", "bf_emma")));
        assert_eq!(route(), Some((3, Some(WebTts::new("kokoro", "bf_emma")))));
        set_route(None);
        choice_changed(3, None);
        assert_eq!(route(), None);
    }
}
