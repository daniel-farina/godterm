//! Setup that works out of the box: what is missing (Settings > Setup,
//! shown on first launch), one step installs after a yes (the exact
//! commands and sizes shown first), run in the background with spoken
//! progress, picked up live. Steps that need sudo run in a visible tab
//! where the user types their own password; nothing is ever sudo'd
//! silently and no shell rc file is touched.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::App;
use crate::deps::{self, Dep, Group, Step};

#[derive(Default)]
pub struct SetupState {
    cache: RefCell<Option<(Instant, Vec<Dep>)>>,
    /// The whisper model to offer ("large-v3-turbo" or "small.en").
    pub whisper_choice: Option<String>,
    /// An Install click waiting for its second click (the confirmation).
    pub confirm: Option<(String, Instant)>,
    /// Installs running now (ids).
    pub running: Vec<String>,
    /// Tests: programs resolve here, downloads may use 127.0.0.1.
    pub bin_dir: Option<PathBuf>,
    pub local_ok: bool,
    pub url_base: Option<String>,
    /// Tests: the commands sudo tabs would have run.
    pub tab_runs: Vec<String>,
    /// Tests: the platform the plans are for, and the cache folder.
    pub os: Option<deps::Os>,
    pub cache_dir: Option<PathBuf>,
}

/// `name` (or `name.exe`) in `dir`.
fn in_dir(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    [dir.join(name), dir.join(format!("{name}.exe"))]
        .into_iter()
        .find(|p| p.is_file())
}

impl App {
    fn setup_which(&self) -> impl Fn(&str) -> Option<PathBuf> + '_ {
        move |b: &str| {
            if let Some(d) = &self.setup.bin_dir {
                return in_dir(d, b);
            }
            deps::which(b)
        }
    }

    /// Every dependency with its status (read again at most every 3 s).
    pub fn deps(&self) -> Vec<Dep> {
        if let Some((t, v)) = self.setup.cache.borrow().as_ref() {
            if t.elapsed() < Duration::from_secs(3) {
                return v.clone();
            }
        }
        let w = self.setup_which();
        let mut probe = deps::Probe::host(&w);
        if let Some(os) = self.setup.os {
            probe.os = os;
        }
        if let Some(c) = &self.setup.cache_dir {
            probe.cache = c.clone();
        }
        let mut v = deps::all(&self.cfg, self.setup.whisper_choice.as_deref(), &probe);
        if let Some(base) = &self.setup.url_base {
            for d in &mut v {
                for s in &mut d.steps {
                    if let Step::Download(x) = s {
                        let file = x.url.rsplit('/').next().unwrap_or("").to_string();
                        x.url = format!("{base}/{file}");
                    }
                }
            }
        }
        *self.setup.cache.borrow_mut() = Some((Instant::now(), v.clone()));
        v
    }

    pub fn deps_changed(&self) {
        *self.setup.cache.borrow_mut() = None;
    }

    /// The program a tab of `account` runs is not installed.
    pub fn agent_missing(&self, account: Option<usize>) -> Option<&'static str> {
        let a = account?;
        let id = match self.cfg.accounts.get(a)?.harness() {
            crate::harness::Harness::Claude => "claude",
            crate::harness::Harness::Grok => "grok",
        };
        self.deps()
            .iter()
            .find(|d| d.id == id && !d.status.ok())
            .map(|d| d.id)
    }

    /// The state block's line (only when something is missing).
    pub fn setup_state_line(&self) -> String {
        let miss: Vec<String> = self
            .deps()
            .iter()
            .filter(|d| !d.status.ok())
            .map(|d| {
                format!(
                    "{}{}",
                    d.id,
                    if d.group == Group::Required {
                        " (required)"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        if miss.is_empty() {
            String::new()
        } else {
            format!(
                "missing: {} (install_dependency; setup_status for the plan)\n",
                miss.join(", ")
            )
        }
    }

    /// The required pieces that are missing.
    pub fn required_missing(&self) -> Vec<&'static str> {
        self.deps()
            .iter()
            .filter(|d| d.group == Group::Required && !d.status.ok())
            .map(|d| d.name)
            .collect()
    }

    pub fn setup_status_json(&self) -> Value {
        let ds = self.deps();
        json!({
            "items": ds.iter().map(|d| json!({
                "id": d.id, "name": d.name, "group": d.group, "status": d.status.word(), "detail": d.status.detail(),
                "why": d.why, "plan": d.steps.iter().map(Step::shown).collect::<Vec<_>>(),
                "download": (d.download_bytes() > 0).then(|| deps::size(d.download_bytes())),
                "needs_sudo": d.steps.iter().any(Step::needs_sudo),
            })).collect::<Vec<_>>(),
            "whisper_models": deps::WHISPER_MODELS.iter().map(|m| json!({"id": m.0, "size": deps::size(m.3), "note": m.4})).collect::<Vec<_>>(),
            "no_install_paths": if cfg!(target_os = "macos") { "Apple speech recognition and say need no install; Grok voice needs only a Grok login or an xAI key" } else { "Grok voice needs only a Grok login or an xAI key" },
        })
    }

    /// What an install of `ids` runs, as one confirmation.
    pub fn install_question(&self, ids: &[&str]) -> Result<String, String> {
        let ds = self.deps();
        let mut parts = vec![];
        let mut bytes = 0;
        let mut sudo = false;
        let mut manual = vec![];
        for id in ids {
            let d = ds
                .iter()
                .find(|d| d.id == *id)
                .ok_or_else(|| format!("no item {id}"))?;
            if d.status.ok() {
                continue;
            }
            if !d.installable() {
                manual.push(format!(
                    "{}: {}",
                    d.name,
                    d.steps
                        .iter()
                        .map(Step::shown)
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
                continue;
            }
            bytes += d.download_bytes();
            sudo |= d.steps.iter().any(Step::needs_sudo);
            parts.push(format!(
                "{}: {}",
                d.name,
                d.steps
                    .iter()
                    .map(Step::shown)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if parts.is_empty() {
            return Err(if manual.is_empty() {
                "everything asked for is already installed".into()
            } else {
                format!(
                    "GodTerm cannot install this here; do it by hand: {}",
                    manual.join(" | ")
                )
            });
        }
        let mut q = format!("Install {}?", parts.join(" | "));
        if bytes > 0 {
            q.push_str(&format!(" Downloads {} in all.", deps::size(bytes)));
        }
        if sudo {
            q.push_str(" Steps with sudo run in a new tab, where you type your password.");
        }
        if !manual.is_empty() {
            q.push_str(&format!(
                " Not installable here (by hand): {}.",
                manual.join(" | ")
            ));
        }
        Ok(q)
    }

    /// Install `ids` in the background (sudo steps in a visible tab).
    pub fn run_install(&mut self, ids: &[String]) -> String {
        let ds = self.deps();
        let mut plan: Vec<(String, String, Vec<Step>)> = vec![];
        for id in ids {
            if let Some(d) = ds
                .iter()
                .find(|d| d.id == id && !d.status.ok() && d.installable())
            {
                plan.push((d.id.to_string(), d.name.to_string(), d.steps.clone()));
            }
        }
        if plan.is_empty() {
            return "Nothing to install.".into();
        }
        // sudo steps: in a tab the user watches and types into.
        let mut background = vec![];
        for (id, name, steps) in plan {
            if steps.iter().any(Step::needs_sudo) {
                let line = steps
                    .iter()
                    .map(Step::shown)
                    .collect::<Vec<_>>()
                    .join(" && ");
                crate::log::info(&format!("setup: {name} in a tab (sudo): {line}"));
                self.admin_record(&format!("install {name} in a tab: {line}"));
                let p = self.focus;
                let shell = if cfg!(windows) { "cmd" } else { "sh" };
                let flag = if cfg!(windows) { "/C" } else { "-c" };
                let args = vec![
                    flag.to_string(),
                    format!("{line}; echo; echo 'Done. You can close this tab.'"),
                ];
                // Tests never run sudo: the tab's command is only recorded.
                if cfg!(test) {
                    self.setup
                        .tab_runs
                        .push(format!("{shell} {}", args.join(" ")));
                } else {
                    self.run_in_new_tab(p, shell, args);
                }
                self.view = crate::app::View::Grid;
                self.announce_progress(&format!(
                    "Installing {name} in a new tab: type your password there."
                ));
                continue;
            }
            background.push((id, name, steps));
        }
        if background.is_empty() {
            return "The install runs in a new tab; type your password there.".into();
        }
        let names: Vec<String> = background.iter().map(|b| b.1.clone()).collect();
        self.admin_record(&format!("install {}", names.join(", ")));
        self.setup.running = background.iter().map(|b| b.0.clone()).collect();
        let tx = self.admin_event_sender();
        let local_ok = self.setup.local_ok;
        let bin_dir = self.setup.bin_dir.clone();
        self.admin.jobs += 1;
        std::thread::spawn(move || {
            let mut failed = vec![];
            let mut done = vec![];
            let mut voice = false;
            for (id, name, steps) in background {
                let _ = tx.send(crate::app_admin::AdminEvent::Progress(format!(
                    "Installing {name}."
                )));
                let mut ok = true;
                for s in &steps {
                    let r: Result<(), String> = match s {
                        Step::Download(d) => {
                            let last = std::cell::Cell::new(0u64);
                            let tx2 = tx.clone();
                            let n2 = name.clone();
                            deps::download(d, local_ok, &|got, total| {
                                let pct = (got * 100).checked_div(total).unwrap_or(0);
                                if pct >= last.get() + 25 {
                                    last.set(pct);
                                    let _ = tx2.send(crate::app_admin::AdminEvent::Progress(
                                        format!("{n2}: {pct}%"),
                                    ));
                                }
                            })
                            .map_err(|e| format!("{e:#}"))
                        }
                        Step::Run { program, args, .. } => {
                            let prog = bin_dir
                                .as_ref()
                                .and_then(|d| in_dir(d, program))
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| program.clone());
                            run_cmd(&prog, args)
                        }
                        Step::Script { shell, line } => {
                            let sh = bin_dir
                                .as_ref()
                                .and_then(|d| in_dir(d, shell))
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| shell.clone());
                            let flag = if shell == "powershell" {
                                "-Command"
                            } else {
                                "-c"
                            };
                            run_cmd(&sh, &[flag.to_string(), line.clone()])
                        }
                        Step::Manual(_) => Ok(()),
                    };
                    crate::log::info(&format!(
                        "setup: {} -> {}",
                        s.shown(),
                        match &r {
                            Ok(()) => "ok".to_string(),
                            Err(e) => e.clone(),
                        }
                    ));
                    if let Err(e) = r {
                        failed.push(format!("{name}: {}", crate::sessions::snippet(&e, 120)));
                        ok = false;
                        break;
                    }
                }
                if ok {
                    voice |= !matches!(id.as_str(), "claude" | "grok");
                    done.push(name);
                }
            }
            let text = match (done.is_empty(), failed.is_empty()) {
                (false, true) => format!("Installed {}.", done.join(", ")),
                (true, false) => format!("Install failed: {}.", failed.join("; ")),
                _ => format!(
                    "Installed {}; failed: {}.",
                    done.join(", "),
                    failed.join("; ")
                ),
            };
            let _ = tx.send(crate::app_admin::AdminEvent::SetupDone {
                ok: failed.is_empty(),
                text,
                voice,
            });
        });
        format!("Installing {} now; I'll say each step.", names.join(", "))
    }

    /// After an install: read the state again; new tabs find the program,
    /// and the voice engine restarts on the new pieces.
    pub fn after_setup(&mut self, voice: bool) {
        self.setup.running.clear();
        self.deps_changed();
        // The installers put claude and grok in ~/.local/bin or ~/.grok/bin,
        // which this process's PATH may not have: point GodTerm at them
        // (config.toml only; shell rc files are never touched).
        if !cfg!(test) {
            for (id, key, cands) in [
                (
                    "claude",
                    "claude_bin",
                    vec![".local/bin/claude", ".claude/local/claude"],
                ),
                (
                    "grok",
                    "grok_bin",
                    vec![".grok/bin/grok", ".local/bin/grok"],
                ),
            ] {
                if self.deps().iter().any(|d| d.id == id && !d.status.ok()) {
                    if let Some(p) = cands
                        .iter()
                        .map(|c| crate::config::home_dir().join(c))
                        .find(|p| p.is_file())
                    {
                        let v = p.display().to_string();
                        let _ = crate::settings::write(
                            &crate::config::Config::path(),
                            &crate::settings::Key::Global(key),
                            Some(toml_edit::value(v.clone())),
                        );
                        if id == "claude" {
                            self.cfg.claude_bin = Some(v.clone());
                        } else {
                            self.cfg.grok_bin = Some(v.clone());
                        }
                        self.config_mtime = crate::app::config_mtime();
                        self.admin_record(&format!(
                            "set {key} to {v} (not on this terminal's PATH)"
                        ));
                        self.deps_changed();
                    }
                }
            }
        }
        if voice && self.voice.engine.is_some() {
            let wake = self.voice.always_on;
            crate::log::info("setup: restarting the voice engine on the new pieces");
            self.stop_voice();
            self.start_voice(wake);
        }
    }

    /// Settings > Setup's Install button: the first click shows what it
    /// runs, the second (within 15 s) runs it.
    pub fn setup_click(&mut self, id: &str) {
        let ids: Vec<&str> = if id == "voice" {
            self.deps()
                .iter()
                .filter(|d| d.group == Group::Voice && !d.status.ok())
                .map(|d| d.id)
                .collect()
        } else {
            vec![]
        };
        let ids: Vec<String> = if ids.is_empty() {
            vec![id.to_string()]
        } else {
            ids.iter().map(|s| s.to_string()).collect()
        };
        let again = self
            .setup
            .confirm
            .as_ref()
            .is_some_and(|(k, t)| k == id && t.elapsed() < Duration::from_secs(15));
        if !again {
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            match self.install_question(&refs) {
                Ok(q) => {
                    self.setup.confirm = Some((id.to_string(), Instant::now()));
                    self.flash(format!("{q} Click Install again to go ahead."));
                }
                Err(e) => self.flash(e),
            }
            return;
        }
        self.setup.confirm = None;
        let say = self.run_install(&ids);
        self.flash(say);
    }

    /// Open Settings > Setup.
    pub fn open_setup(&mut self) {
        self.view = crate::app::View::Settings;
        self.settings_section = crate::settings::SECTIONS
            .iter()
            .position(|(s, _)| *s == crate::settings::Section::Setup)
            .unwrap_or(0);
        self.settings_sel = 0;
        self.deps_changed();
    }

    /// At start: the setup screen when something required is missing (or
    /// on the very first run), unless turned off.
    pub fn maybe_show_setup(&mut self, first_run: bool) {
        if self.cfg.setup_dont_show || cfg!(test) {
            return;
        }
        let any = self.deps().iter().any(|d| !d.status.ok());
        if !self.required_missing().is_empty() || (first_run && any) {
            self.open_setup();
        }
    }
}

fn run_cmd(program: &str, args: &[String]) -> Result<(), String> {
    let mut c = std::process::Command::new(program);
    c.args(args);
    match crate::procs::output_timeout(&mut c, Duration::from_secs(1800)) {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr);
            let out = String::from_utf8_lossy(&o.stdout);
            Err(e
                .lines()
                .chain(out.lines())
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("it failed")
                .trim()
                .to_string())
        }
        Err(e) => Err(e),
    }
}
