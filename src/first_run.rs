//! The first run as one flow inside the app, with no questions on the
//! plain terminal first:
//!
//! 1. the welcome: turn voice on or skip it (onboarding.rs),
//! 2. the first account picker: all six agents, which CLIs are installed,
//!    i installs a missing one, or a line says where to get it
//!    (add_account.rs with `first`),
//! 3. its login in the new pane, with a one line banner for that agent,
//!    then that tab is the first tab, and the short tour.
//!
//! `godterm setup` still asks on the plain terminal. A home that already
//! has accounts never sees any of this.

use crate::add_account::AddForm;
use crate::app::{App, Modal, Onboard, View};
use crate::config::Config;
use crate::deps::{Dep, Step};
use crate::harness::Harness;

/// The three steps, as the welcome and the picker name them.
pub const STEPS: [&str; 3] = ["voice", "your first account", "log in"];

/// "step 2 of 3: your first account".
pub fn step_label(n: usize) -> String {
    let n = n.clamp(1, STEPS.len());
    format!("step {n} of {}: {}", STEPS.len(), STEPS[n - 1])
}

/// The welcome's line of what comes.
pub fn roadmap() -> String {
    STEPS
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{} {s}", i + 1))
        .collect::<Vec<_>>()
        .join("   ")
}

/// Esc on the first account picker.
pub const SKIPPED_FIRST_ACCOUNT: &str =
    "No account yet. Ctrl-a A adds one when you are ready (or click Add account).";

/// A home with no config yet, or with no accounts, counts as new.
pub fn fresh_home(first_run: bool, cfg: &Config) -> bool {
    first_run || cfg.accounts.is_empty()
}

/// The config of a brand new home on a terminal: every default but the
/// placeholder accounts (the first account picker adds a real one).
pub fn empty_config() -> Config {
    Config {
        accounts: vec![],
        ..Config::default()
    }
}

/// `empty_config`, saved (unless a claudego install still waits to be
/// migrated: then the usual path, which leaves the file alone).
pub fn new_home_config() -> anyhow::Result<Config> {
    if crate::migrate::pending() {
        return Config::load_or_init();
    }
    let cfg = empty_config();
    cfg.save()?;
    Ok(cfg)
}

/// How to get a missing CLI, in one short line: the command GodTerm
/// would run, or where to download it.
pub fn how_to_install(d: &Dep) -> String {
    let text = d
        .steps
        .iter()
        .map(Step::shown)
        .collect::<Vec<_>>()
        .join(" && ");
    if d.installable() || text.ends_with('.') {
        text
    } else {
        format!("{text}.")
    }
}

/// A pane whose agent is missing: what to do next.
pub fn missing_pane_line(d: &Dep) -> String {
    if d.installable() {
        format!(
            "{} isn't installed. Press I to install it ({}).",
            d.name,
            how_to_install(d)
        )
    } else {
        format!("{} isn't installed. {}", d.name, how_to_install(d))
    }
}

/// How the login goes for this agent, in one line.
pub fn login_hint(h: Harness) -> String {
    match h {
        Harness::Claude => "Pick \"Claude account with subscription\", finish in the browser, paste the code in the pane below.".into(),
        Harness::Grok => "Follow Grok Build's login in the pane below: it opens the browser, then comes back here.".into(),
        _ => format!(
            "{} keeps its own login: follow its steps in the pane below.",
            h.label()
        ),
    }
}

/// The flash once the first account is ready.
pub fn ready_line(label: &str, logged_in: bool) -> String {
    if logged_in {
        format!("{label} is ready. Type in the pane to start. Ctrl-a t opens another tab, Ctrl-a : finds any action.")
    } else {
        format!("{label} is not logged in yet: Ctrl-a I logs it in. Ctrl-a : finds any action.")
    }
}

/// Word wrap `s` into at most `lines` lines of `width`; the last one ends
/// in … when cut.
pub fn wrap(s: &str, width: usize, lines: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    for w in s.split_whitespace() {
        let need = cur.chars().count() + usize::from(!cur.is_empty()) + w.chars().count();
        if need > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.len() > lines {
        out.truncate(lines);
        if let Some(l) = out.last_mut() {
            let keep: String = l.chars().take(width.saturating_sub(2)).collect();
            *l = format!("{keep} …");
        }
    }
    for l in &mut out {
        if l.chars().count() > width {
            *l = crate::sessions::snippet(l, width);
        }
    }
    out
}

impl App {
    /// The first account picker (the first run, or no accounts at start).
    pub fn open_first_account(&mut self) {
        let mut f = AddForm::new(self);
        f.first = true;
        f.label = "Work".into();
        if self.cfg.accounts.iter().any(|a| a.display() == "Work") {
            f.label = format!("Account {}", self.cfg.accounts.len() + 1);
        }
        if !f.ok[f.agent] {
            f.error = Some(f.missing_line());
        }
        self.view = View::Grid;
        self.modal = Modal::AddAccount(Box::new(f));
        crate::log::info("first run: the first account picker");
    }

    /// The Add account form, if open, reads what is installed again.
    pub fn refresh_add_form(&mut self) {
        if !matches!(self.modal, Modal::AddAccount(_)) {
            return;
        }
        self.deps_changed();
        let deps = self.deps();
        if let Modal::AddAccount(f) = &mut self.modal {
            let was = f.ok[f.agent];
            f.refresh(&deps);
            if !was && f.ok[f.agent] {
                f.error = Some(format!(
                    "{} is installed. Enter adds the account.",
                    crate::add_account::AGENTS[f.agent].1
                ));
            }
        }
    }

    /// The first account was added and its login started: watch for the
    /// login (Claude and Grok tell), then its tab and the tour.
    pub fn first_account_added(&mut self, idx: usize) {
        let h = self.cfg.accounts[idx].harness();
        if h.integrated() {
            self.onboard = Some(Onboard {
                queue: vec![idx],
                pos: 0,
                skipped: vec![],
                first: true,
            });
        } else {
            // Its CLI keeps the login: nothing here can tell when it is
            // done, so the tour is one key away instead of in the way.
            self.flash(format!(
                "{} starts in this pane: follow its login steps. Ctrl-a T is the 1 minute tour.",
                h.label()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::{Group, Status};

    fn dep(id: &'static str, steps: Vec<Step>) -> Dep {
        Dep {
            id,
            name: "Thing",
            group: Group::Optional,
            status: Status::Missing("not installed".into()),
            steps,
            why: "",
        }
    }

    #[test]
    fn steps_and_roadmap() {
        assert_eq!(step_label(2), "step 2 of 3: your first account");
        assert_eq!(roadmap(), "1 voice   2 your first account   3 log in");
    }

    #[test]
    fn fresh_home_means_no_config_or_no_accounts() {
        let none = Config {
            accounts: vec![],
            ..Config::default()
        };
        assert!(fresh_home(true, &none));
        assert!(fresh_home(false, &none));
        let mut some = none.clone();
        some.accounts.push(crate::config::AccountCfg {
            name: "w".into(),
            label: "W".into(),
            color: "sage".into(),
            cwd: None,
            args: vec![],
            permission_mode: None,
            auto_trust: None,
            show_email: None,
            new_tab_base: None,
            scrollback_lines: None,
            config_dir: None,
            harness: "claude".into(),
        });
        assert!(!fresh_home(false, &some), "existing users are unaffected");
    }

    #[test]
    fn install_lines() {
        let script = dep(
            "claude",
            vec![Step::Script {
                shell: "sh".into(),
                line: "curl -fsSL https://claude.ai/install.sh | bash".into(),
            }],
        );
        assert!(how_to_install(&script).contains("install.sh"));
        assert!(missing_pane_line(&script).starts_with("Thing isn't installed. Press I to"));
        let manual = dep(
            "codex",
            vec![Step::Manual("Install Codex CLI from https://x".into())],
        );
        assert_eq!(how_to_install(&manual), "Install Codex CLI from https://x.");
        assert!(!missing_pane_line(&manual).contains("Press I"));
    }

    #[test]
    fn login_hints_name_the_agent() {
        assert!(login_hint(Harness::Claude).contains("subscription"));
        assert!(login_hint(Harness::Grok).contains("Grok"));
        assert!(login_hint(Harness::Codex).contains("Codex"));
        for h in crate::harness::ALL {
            assert!(!login_hint(*h).contains('\u{2014}'), "no em dashes");
        }
    }

    use crate::config::testing::LOCK as ENV_LOCK;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// An app on a brand new home: no accounts, and nothing of the host
    /// counts (programs resolve in an empty folder of the test home).
    fn new_home(tag: &str) -> (App, std::path::PathBuf) {
        let home =
            std::env::temp_dir().join(format!("godterm-firstrun-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        crate::config::testing::set_home(&home);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(empty_config(), tx);
        app.setup.bin_dir = Some(home.join("empty-bin"));
        app.setup.cache_dir = Some(home.join("cache"));
        app.setup.os = Some(crate::deps::Os::Mac);
        app.cfg.claude_bin = Some("claude".into());
        app.cfg.grok_bin = Some("grok".into());
        app.cfg.codex_bin = Some("codex".into());
        app.cfg.cursor_bin = Some("cursor-agent".into());
        app.cfg.antigravity_bin = Some("antigravity".into());
        app.cfg.opencode_bin = Some("opencode".into());
        (app, home)
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn screen(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(160, 44)).unwrap();
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        let b = term.backend().buffer();
        let mut t = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                t.push_str(b[(x, y)].symbol());
            }
            t.push('\n');
        }
        t
    }

    fn form(app: &App) -> &AddForm {
        match &app.modal {
            Modal::AddAccount(f) => f,
            m => panic!("no Add account form: {m:?}"),
        }
    }

    #[test]
    fn first_run_picker_login_then_tab_and_tour() {
        use crate::creds::CredSource;
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = new_home("flow");
        // Claude Code is "installed": a program that exits at once.
        app.cfg.claude_bin = Some(crate::test_stub::true_bin());
        app.autostart();
        app.open_first_account();
        let f = form(&app);
        assert!(f.first && f.ok[0] && f.agent == 0 && f.label == "Work");
        let t = screen(&mut app);
        assert!(t.contains("Welcome: add your first account"), "{t}");
        assert!(t.contains("step 2 of 3: your first account"), "{t}");
        assert!(t.contains("✓"), "installed agents are marked: {t}");
        // Enter adds it and starts its login, with the first run banner.
        app.on_add_account_key(key(KeyCode::Enter));
        assert_eq!(app.cfg.accounts.len(), 1);
        assert_eq!(app.modal, Modal::None);
        let ob = app.onboard.clone().expect("the login is watched");
        assert!(ob.first && ob.queue == vec![0]);
        let t = screen(&mut app);
        assert!(t.contains("step 3 of 3: log in Work"), "{t}");
        assert!(t.contains("Claude account with subscription"), "{t}");
        // Logged in: its tab stays in front, the tour opens, one line says
        // what to do next.
        app.accounts[0].login.source = Some(CredSource::File);
        app.tick();
        assert!(app.onboard.is_none());
        assert_eq!(app.view, View::Grid);
        assert_eq!(app.modal, Modal::Tour(0));
        let fl = app.flash.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        assert!(
            fl.contains("Work is ready") && fl.contains("Ctrl-a t"),
            "{fl}"
        );
        app.shutdown();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_missing_agent_says_how_and_waits() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = new_home("missing");
        app.open_first_account();
        let f = form(&app);
        assert!(
            f.ok.iter().all(|ok| !ok),
            "nothing is installed here: {:?} {:?}",
            f.ok,
            app.deps()
                .iter()
                .map(|d| (d.id, d.status.clone()))
                .collect::<Vec<_>>()
        );
        assert!(f
            .error
            .as_deref()
            .unwrap()
            .contains("Press i to install it"));
        let t = screen(&mut app);
        assert!(
            t.contains("missing") && t.contains("press i to install it"),
            "{t}"
        );
        // Enter does not add an account whose CLI is missing.
        app.on_add_account_key(key(KeyCode::Enter));
        assert!(app.cfg.accounts.is_empty());
        // A CLI GodTerm cannot install: where to get it.
        app.on_add_account_key(key(KeyCode::Char('3')));
        let e = form(&app).error.clone().unwrap();
        assert!(
            e.contains("Codex CLI is not installed") && e.contains("https://"),
            "{e}"
        );
        app.on_add_account_key(key(KeyCode::Char('i')));
        assert!(!form(&app).error.clone().unwrap().contains("again"));
        // One it can: the first i says what runs, a second would run it.
        app.on_add_account_key(key(KeyCode::Char('1')));
        app.on_add_account_key(key(KeyCode::Char('i')));
        let e = form(&app).error.clone().unwrap();
        assert!(
            e.contains("install.sh") && e.contains("Press i again"),
            "{e}"
        );
        assert!(
            app.setup.running.is_empty(),
            "nothing runs on the first press"
        );
        // It appears (installed elsewhere): the card turns ready.
        let bin = home.join("empty-bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::copy(crate::test_stub::true_bin(), bin.join("claude")).unwrap();
        app.refresh_add_form();
        let f = form(&app);
        assert!(f.ok[0]);
        assert!(
            f.error.as_deref().unwrap().contains("Enter adds"),
            "{:?}",
            f.error
        );
        // Esc: no account yet, and one line says how to add one later.
        app.on_add_account_key(key(KeyCode::Esc));
        assert_eq!(app.modal, Modal::None);
        let fl = app.flash.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        assert_eq!(fl, SKIPPED_FIRST_ACCOUNT);
        app.flash = None;
        let t = screen(&mut app);
        assert!(t.contains("C-a A add an account"), "the hint row: {t}");
        app.shutdown();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_welcome_leads_to_the_picker() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, home) = new_home("welcome");
        app.start_voice_onboarding(true);
        let t = screen(&mut app);
        assert!(t.contains("step 1 of 3: voice"), "{t}");
        assert!(
            t.contains("1 voice   2 your first account   3 log in"),
            "{t}"
        );
        assert!(t.contains("Esc  Skip voice"), "{t}");
        app.end_voice_onboarding(crate::onboarding::Outcome::Skipped);
        assert!(
            form(&app).first,
            "skipping voice opens the first account picker"
        );
        app.shutdown();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn empty_config_has_no_placeholder_accounts() {
        let c = empty_config();
        assert!(c.accounts.is_empty());
        assert!(
            !Config::default().accounts.is_empty(),
            "the old default had some"
        );
        assert!(c.validate().is_ok());
    }

    #[test]
    fn wrap_keeps_two_lines() {
        let w = wrap("one two three four five six seven", 10, 2);
        assert_eq!(w.len(), 2);
        assert!(w[1].ends_with('…'), "{w:?}");
        assert_eq!(wrap("short", 10, 2), vec!["short".to_string()]);
    }
}
