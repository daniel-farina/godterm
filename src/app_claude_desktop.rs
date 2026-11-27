use crate::app::App;
use crate::harness::Harness;
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const APP: &str = "/Applications/Claude.app";

pub fn installed() -> bool {
    cfg!(target_os = "macos") && Path::new(APP).exists()
}

pub fn tab_args(args: &Value) -> Option<Value> {
    let t = args
        .get("tab")
        .or_else(|| args.get("id"))
        .filter(|v| !v.is_null())?;
    let mut o = args.clone();
    o["tab"] = t.clone();
    Some(o)
}

fn session_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !id.starts_with('-')
}

fn resume_command(
    bin: &str,
    cwd: &Path,
    config_dir: &Path,
    session: &str,
    pass: &[String],
) -> Command {
    let mut c = Command::new(bin);
    c.args(["--desktop", "--resume", session])
        .current_dir(cwd)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, _) in std::env::vars() {
        if crate::pane::should_scrub(&k, pass) {
            c.env_remove(&k);
        }
    }
    c
}

fn run_resume(mut c: Command) -> Result<(), String> {
    let mut child = c
        .spawn()
        .map_err(|e| format!("could not run claude: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => return Ok(()),
            Ok(Some(_)) => {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = e.read_to_string(&mut err);
                }
                let line = err.trim().lines().last().unwrap_or("").trim().to_string();
                return Err(if line.is_empty() {
                    "claude --desktop did not open the conversation".into()
                } else {
                    format!("claude --desktop said: {line}")
                });
            }
            Ok(None) if start.elapsed() > Duration::from_secs(20) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("claude --desktop took too long".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(e) => return Err(format!("claude --desktop failed: {e}")),
        }
    }
}

impl App {
    pub(crate) fn open_in_claude_desktop_tool(&mut self, args: &Value) -> Result<Value, String> {
        if !self.web_caller {
            return Err("open_in_claude_desktop is the user's own button in the web app".into());
        }
        if !cfg!(target_os = "macos") {
            return Err(
                "Claude Desktop is opened on the Mac that runs GodTerm, and this is not a Mac"
                    .into(),
            );
        }
        if !installed() {
            return Err(
                "Claude Desktop is not installed on this Mac (no Claude.app in Applications)"
                    .into(),
            );
        }
        let args = tab_args(args).ok_or("which tab? give its id")?;
        let (s, t) = self.one_tab(&args)?;
        let Some(a) = self.panes[s].account else {
            return Err("this tab has no account".into());
        };
        let acct = &self.cfg.accounts[a];
        if acct.harness() != Harness::Claude {
            return Err("only Claude tabs can open in Claude Desktop".into());
        }
        let tab = &self.panes[s].tabs[t];
        let name = tab.name();
        match tab.session_id.as_deref() {
            Some(id) if session_ok(id) => {
                let c = resume_command(
                    &self.cfg.claude_bin(),
                    &tab.cwd,
                    &acct.config_dir(),
                    id,
                    &self.cfg.pass_env,
                );
                run_resume(c)?;
                Ok(
                    json!({"ok": true, "say": format!("Opened {name} in Claude Desktop"), "resumed": true}),
                )
            }
            Some(_) => Err(
                "this tab's session id looks wrong, so it was not sent to Claude Desktop".into(),
            ),
            None => {
                let st = Command::new("open")
                    .args(["-a", "Claude"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map_err(|e| format!("open failed: {e}"))?;
                if !st.success() {
                    return Err("could not open Claude Desktop".into());
                }
                Ok(
                    json!({"ok": true, "say": format!("{name} has no conversation yet, so Claude Desktop is only brought to the front"), "resumed": false}),
                )
            }
        }
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    fn args_of(c: &Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn session_ids_that_could_pass_as_flags_are_refused() {
        assert!(session_ok("8f1c2d3e-aaaa-bbbb-cccc-0123456789ab"));
        assert!(!session_ok("--help"));
        assert!(!session_ok("a b"));
        assert!(!session_ok(""));
    }

    #[test]
    fn the_resume_command_is_claude_desktop_resume_in_the_tabs_folder() {
        let c = resume_command(
            "claude",
            Path::new("/tmp"),
            Path::new("/tmp/cfg"),
            "abc-1",
            &[],
        );
        assert_eq!(args_of(&c), ["--desktop", "--resume", "abc-1"]);
        assert_eq!(c.get_current_dir(), Some(Path::new("/tmp")));
    }

    #[test]
    fn a_tab_id_can_come_as_tab_or_id() {
        assert_eq!(tab_args(&json!({"id": "t1"})).unwrap()["tab"], "t1");
        assert_eq!(tab_args(&json!({"tab": "t2"})).unwrap()["tab"], "t2");
        assert!(tab_args(&json!({})).is_none());
    }
}
