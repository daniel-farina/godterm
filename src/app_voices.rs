use crate::app::{App, Modal};
use crate::settings::Key;
use crate::voice::{catalog, local_tts};

#[derive(Debug, Clone, PartialEq)]
pub enum VoicePick {
    Engine(&'static str),

    Kokoro(&'static str),

    Ming(&'static str),
}

fn engine_word(w: &str) -> Option<&'static str> {
    Some(match w {
        "kokoro" | "cocoro" | "kokora" | "kokoru" | "coco row" | "koko row" => "kokoro",
        "chatterbox" | "chatter box" | "chatterbox turbo" | "chatter box turbo" => "chatterbox",
        "ming" | "ming 3" | "ming three" => "ming",
        "grok" | "grock" | "groq" | "grokk" => "grok",
        "mac" | "macos" | "mac os" | "say" | "system" | "apple" => "say",
        "local" => "kokoro",
        _ => return None,
    })
}

pub fn parse_voice_phrase(raw: &str, engine: &str) -> Option<VoicePick> {
    let n = crate::voice::grammar::normalize(&raw.replace(['\u{2019}', '\u{2018}'], "'"));
    let w = crate::app_direct::core_words(&n);
    if w.is_empty() || w.len() > 8 {
        return None;
    }
    let s = w.join(" ");
    let rest = [
        "use the ",
        "use ",
        "switch to the ",
        "switch to ",
        "change to the ",
        "change to ",
        "try the ",
        "go back to the ",
        "go back to ",
        "talk with the ",
        "speak with the ",
    ]
    .iter()
    .find_map(|p| s.strip_prefix(p))?;
    let (body, said_voice) = match rest
        .strip_suffix(" voices")
        .or_else(|| rest.strip_suffix(" voice"))
    {
        Some(b) => (b.trim(), true),
        None => (rest.trim(), false),
    };
    let body = body.strip_prefix("the ").unwrap_or(body);

    if let Some(e) = engine_word(body) {
        if e == "grok" && !said_voice {
            return None;
        }
        if body == "local" && !said_voice {
            return None;
        }
        if matches!(
            body,
            "mac" | "macos" | "mac os" | "say" | "system" | "apple"
        ) && !said_voice
        {
            return None;
        }
        return Some(VoicePick::Engine(e));
    }
    if !said_voice {
        return None;
    }

    let key: String = body.replace([' ', '_'], "");
    let ming = local_tts::MING_VOICES
        .iter()
        .find(|v| {
            let full = v.to_lowercase().replace('_', "");
            let short = v.split('_').nth(1).unwrap_or("").to_lowercase();
            key == full || key == short
        })
        .copied();
    let kokoro = catalog::KOKORO_EN.iter().map(|v| v.0).find(|id| {
        let full = id.replace('_', "");
        let short = id.split('_').nth(1).unwrap_or("");
        key == full || key == short
    });
    match (kokoro, ming) {
        (Some(_), Some(m)) if engine == "ming" => Some(VoicePick::Ming(m)),
        (Some(k), _) => Some(VoicePick::Kokoro(k)),
        (None, Some(m)) => Some(VoicePick::Ming(m)),
        (None, None) => None,
    }
}

pub fn confirm_lines(id: &str, remove: bool) -> Vec<String> {
    let Some(i) = local_tts::info(id) else {
        return vec![];
    };
    if !remove {
        return local_tts::consent_lines(id);
    }
    vec![
        format!("Remove {}?", i.name),
        format!(
            "Deletes its model (about {:.1} GB, in {}),",
            i.disk_gb,
            crate::config::tilde(&local_tts::hub_cache())
        ),
        "and the helper environment in ~/.godterm/voices when no other".into(),
        "local voice uses it. Kokoro speaks instead; install again any time.".into(),
    ]
}

static PICKED: std::sync::Mutex<Option<Option<String>>> = std::sync::Mutex::new(None);

impl App {
    pub fn voice_choice_intercept(&mut self, command: &str) -> bool {
        let Some(p) = parse_voice_phrase(command, &self.cfg.voice.tts_engine) else {
            return false;
        };
        crate::log::info(&format!("voice: choice {p:?}: {command}"));
        let say = self.apply_voice_pick(&p);
        self.voice.action = Some(say.clone());
        self.flash(say.clone());
        self.speak(&say);
        true
    }

    fn write_voice(&mut self, key: &'static str, v: &str) -> Result<(), String> {
        crate::settings::write(
            &crate::config::Config::path(),
            &Key::Global(key),
            Some(toml_edit::value(v)),
        )
        .map_err(|e| format!("{e:#}"))
    }

    pub fn apply_voice_pick(&mut self, p: &VoicePick) -> String {
        let (engine, extra): (&'static str, Option<(&'static str, String)>) = match p {
            VoicePick::Engine(e) => (e, None),
            VoicePick::Kokoro(v) => ("kokoro", Some(("voice.kokoro_voice", v.to_string()))),
            VoicePick::Ming(v) => ("ming", Some(("voice.local.ming_voice", v.to_string()))),
        };
        if matches!(engine, "chatterbox" | "ming") {
            if let Some(why) = local_tts::problem(engine, &self.cfg.voice) {
                let name = local_tts::info(engine).map(|i| i.name).unwrap_or(engine);
                return if why.starts_with("not installed") {
                    format!("{name} is not installed yet. Settings, Voice and Audio, has Install; it asks first.")
                } else {
                    format!("{name} is unavailable: {why}.")
                };
            }
        }
        if let Some((k, v)) = &extra {
            if let Err(e) = self.write_voice(k, v) {
                return format!("Not saved: {e}");
            }
        }
        if self.cfg.voice.tts_engine != engine {
            if let Err(e) = self.write_voice("voice.tts_engine", engine) {
                return format!("Not saved: {e}");
            }
        }
        self.state_dirty = true;
        self.reload_config_now();
        let vc = &self.cfg.voice;
        match p {
            VoicePick::Kokoro(v) => format!("Okay, this is {}.", catalog::kokoro_name(v)),
            VoicePick::Ming(v) => {
                format!("Okay, this is Ming's {}.", v.split('_').nth(1).unwrap_or(v))
            }
            VoicePick::Engine("kokoro") => format!(
                "Okay, Kokoro, the {} voice.",
                catalog::kokoro_name(&vc.kokoro_voice)
            ),
            VoicePick::Engine("chatterbox") => {
                "Okay, Chatterbox Turbo. It takes a few seconds to load the first time.".into()
            }
            VoicePick::Engine("ming") => {
                "Okay, Ming. It takes a moment before each sentence.".into()
            }
            VoicePick::Engine("grok") => "Okay, the Grok voice.".into(),
            VoicePick::Engine(_) => "Okay, the Mac voice.".into(),
        }
    }

    pub fn ask_local_voice(&mut self, id: &'static str) {
        if !local_tts::supported() {
            return self.flash("Local voices need a Mac with Apple Silicon.");
        }
        if local_tts::installing() {
            return self.flash("An install is already running; its progress shows on its row.");
        }
        let installed = local_tts::installed(id, &self.cfg.voice.local);
        self.modal = Modal::ConfirmLocalVoice(id, installed);
    }

    pub fn local_voice_yes(&mut self, id: &'static str, remove: bool) {
        let name = local_tts::info(id).map(|i| i.name).unwrap_or(id);
        if remove {
            if self.cfg.voice.tts_engine == id {
                let _ = self.write_voice("voice.tts_engine", "kokoro");
                self.reload_config_now();
            }
            match local_tts::remove(id, &self.cfg.voice.local) {
                Ok(m) => self.flash(m),
                Err(e) => self.flash(format!("{name}: {e}")),
            }
        } else {
            local_tts::start_install(id, &self.cfg.voice);
            self.flash(format!("Installing {name}; progress shows on its row."));
        }
    }

    pub fn local_voices_tick(&mut self) {
        for n in local_tts::take_notes() {
            self.flash(n);
        }
        match local_tts::take_finished() {
            Some(local_tts::Install::Done { msg, .. }) => self.flash(msg),
            Some(local_tts::Install::Failed { id, msg }) => {
                let name = local_tts::info(&id).map(|i| i.name).unwrap_or("");
                self.flash(format!("{name} install failed: {msg}"))
            }
            _ => {}
        }
        let picked = PICKED.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(Some(path)) = picked {
            match self.write_voice("voice.local.chatterbox_ref", &path) {
                Ok(()) => {
                    self.reload_config_now();
                    self.flash(format!("Chatterbox clones {path}"));
                }
                Err(e) => self.flash(format!("Not saved: {e}")),
            }
        }
    }

    pub fn pick_voice_clip(&mut self) {
        if cfg!(test) || !cfg!(target_os = "macos") {
            return self.flash("Type the clip's path in the row (Enter).");
        }
        std::thread::spawn(|| {
            let out = std::process::Command::new("osascript")
                .args([
                    "-e",
                    "POSIX path of (choose file with prompt \"A clip of the voice to clone (WAV, over 5 seconds)\" of type {\"public.audio\"})",
                ])
                .output();
            let path = out
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|p| !p.is_empty());
            *PICKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_phrases() {
        let e = |s: &str| parse_voice_phrase(s, "kokoro");
        assert_eq!(e("use the local voice"), Some(VoicePick::Engine("kokoro")));
        assert_eq!(e("Use Chatterbox."), Some(VoicePick::Engine("chatterbox")));
        assert_eq!(
            e("switch to chatter box"),
            Some(VoicePick::Engine("chatterbox"))
        );
        assert_eq!(e("use Kokoro please"), Some(VoicePick::Engine("kokoro")));
        assert_eq!(e("use the Grok voice"), Some(VoicePick::Engine("grok")));
        assert_eq!(e("use grok voice"), Some(VoicePick::Engine("grok")));
        assert_eq!(e("use the ming voice"), Some(VoicePick::Engine("ming")));
        assert_eq!(e("use the mac voice"), Some(VoicePick::Engine("say")));
        assert_eq!(
            e("use the Bella voice"),
            Some(VoicePick::Kokoro("af_bella"))
        );
        assert_eq!(
            e("use the af heart voice"),
            Some(VoicePick::Kokoro("af_heart"))
        );
        assert_eq!(
            e("okay use the Michael voice"),
            Some(VoicePick::Kokoro("am_michael"))
        );

        assert_eq!(
            e("use the George voice"),
            Some(VoicePick::Kokoro("bm_george"))
        );
        assert_eq!(
            parse_voice_phrase("use the George voice", "ming"),
            Some(VoicePick::Ming("UK_George"))
        );
        assert_eq!(
            e("use the Stella voice"),
            Some(VoicePick::Ming("US_Stella"))
        );

        for s in [
            "use grok",
            "use the tests",
            "use the local copy",
            "use the bella",
            "use the nobody voice",
            "please use the Bella voice for the narration in the video we are making today",
            "chatterbox",
        ] {
            assert_eq!(e(s), None, "{s}");
        }
    }

    #[test]
    fn remove_asks_with_what_goes() {
        let l = confirm_lines("chatterbox", true).join(" ");
        assert!(
            l.contains("Remove Chatterbox Turbo") && l.contains("2.9 GB"),
            "{l}"
        );
        assert!(confirm_lines("ming", false)[0].contains("Install Ming-3.0"));
        assert!(confirm_lines("bogus", false).is_empty());
    }
}
