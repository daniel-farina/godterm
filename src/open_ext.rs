use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::app::App;
use crate::browser::{parse_local_state, ChromeProfile};
use crate::embedded::check_url;
use crate::tab_browser::{chrome_cmd, Cmd, Os};

const LINUX_CHROME: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Chrome,
    Safari,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "chrome" => Some(Kind::Chrome),
            "safari" => Some(Kind::Safari),
            _ => None,
        }
    }
}

pub struct Where {
    pub os: Os,
    pub app_dirs: Vec<PathBuf>,
    pub bin_dirs: Vec<PathBuf>,
    pub chrome_root: PathBuf,
}

impl Where {
    pub fn real() -> Where {
        let home = crate::config::home_dir();
        Where {
            os: Os::current(),
            app_dirs: vec![PathBuf::from("/Applications"), home.join("Applications")],
            bin_dirs: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            chrome_root: crate::browser::chrome_root(),
        }
    }
}

fn app_present(dirs: &[PathBuf], name: &str) -> bool {
    dirs.iter().any(|d| d.join(name).is_dir())
}

pub fn chrome_installed(w: &Where) -> bool {
    match w.os {
        Os::Mac => app_present(&w.app_dirs, "Google Chrome.app"),
        Os::Windows => w.chrome_root.join("Local State").is_file(),
        Os::Linux => {
            w.chrome_root.join("Local State").is_file()
                || LINUX_CHROME
                    .iter()
                    .any(|n| w.bin_dirs.iter().any(|d| d.join(n).is_file()))
        }
    }
}

pub fn safari_installed(w: &Where) -> bool {
    w.os == Os::Mac && app_present(&w.app_dirs, "Safari.app")
}

pub fn local_profiles(root: &Path) -> Vec<ChromeProfile> {
    parse_local_state(&std::fs::read_to_string(root.join("Local State")).unwrap_or_default())
}

pub fn listing(w: &Where, profiles: &[ChromeProfile]) -> Value {
    let chrome = chrome_installed(w);
    json!({
        "chrome": chrome,
        "safari": safari_installed(w),
        "profiles": if chrome {
            profiles.iter().map(|p| json!({
                "dir": p.dir,
                "name": if p.name.is_empty() { p.dir.clone() } else { p.name.clone() },
                "account": p.user_name,
            })).collect::<Vec<_>>()
        } else {
            vec![]
        },
    })
}

pub fn web_url(raw: &str) -> Result<String, String> {
    let u = check_url(raw)?;
    let low = u.to_ascii_lowercase();
    if low.starts_with("http://") || low.starts_with("https://") {
        Ok(u)
    } else {
        Err("only http and https pages can be opened in another browser".into())
    }
}

pub fn plan(
    os: Os,
    kind: Kind,
    profile: Option<&str>,
    raw_url: &str,
    known: &[ChromeProfile],
) -> Result<Cmd, String> {
    let url = web_url(raw_url)?;
    match kind {
        Kind::Safari => {
            if os != Os::Mac {
                return Err("Safari is only on a Mac".into());
            }
            Ok(Cmd {
                prog: "open".into(),
                args: vec!["-a".into(), "Safari".into(), url],
            })
        }
        Kind::Chrome => {
            if let Some(p) = profile {
                if !known.is_empty() && !known.iter().any(|k| k.dir == p) {
                    return Err("that Chrome profile was not found".into());
                }
            }
            chrome_cmd(os, &url, profile)
                .ok_or_else(|| "that address cannot be opened in Chrome".to_string())
        }
    }
}

pub fn open_with(
    w: &Where,
    kind: Kind,
    profile: Option<&str>,
    raw_url: &str,
    known: &[ChromeProfile],
    run: &dyn Fn(&Cmd) -> bool,
) -> Result<String, String> {
    let installed = match kind {
        Kind::Chrome => chrome_installed(w),
        Kind::Safari => safari_installed(w),
    };
    let word = match kind {
        Kind::Chrome => "Chrome",
        Kind::Safari => "Safari",
    };
    if !installed {
        return Err(format!("{word} was not found on this computer"));
    }
    let cmd = plan(w.os, kind, profile, raw_url, known)?;
    if !run(&cmd) {
        return Err(format!("Could not open {word}"));
    }
    let who = profile
        .and_then(|d| known.iter().find(|k| k.dir == d))
        .map(|k| {
            if k.name.is_empty() {
                k.dir.clone()
            } else {
                k.name.clone()
            }
        });
    Ok(match who {
        Some(n) => format!("Opened in Chrome, profile {n}"),
        None => format!("Opened in {word}"),
    })
}

pub fn launch(cmd: &Cmd) -> bool {
    if std::env::var_os("GODTERM_NO_OPEN").is_some() {
        return true;
    }
    let run = |prog: &str| {
        std::process::Command::new(prog)
            .args(&cmd.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|mut c| {
                let _ = std::thread::Builder::new()
                    .name("open-ext-wait".into())
                    .spawn(move || {
                        let _ = c.wait();
                    });
            })
            .is_ok()
    };
    if Os::current() == Os::Linux && cmd.prog == "google-chrome" {
        LINUX_CHROME.iter().any(|p| run(p))
    } else {
        run(&cmd.prog)
    }
}

impl App {
    pub fn web_open_ext(&mut self, client: u64, v: Value) {
        let Some(c) = self.web.as_ref().and_then(|w| w.hub.client(client)) else {
            return;
        };
        let out = Arc::clone(&c.out);
        let reply = |ok: bool, mut body: Value| {
            body["t"] = json!("oext");
            body["id"] = v["id"].clone();
            body["op"] = v["op"].clone();
            body["ok"] = json!(ok);
            out.text(body.to_string());
        };
        if !self.cfg.web.embedded_browser {
            return reply(false, json!({"error": "the embedded browser is off"}));
        }
        let w = Where::real();
        let mut known: Vec<ChromeProfile> = self
            .browser_profiles()
            .into_iter()
            .map(|p| p.profile)
            .collect();
        if known.is_empty() {
            known = local_profiles(&w.chrome_root);
        }
        match v["op"].as_str() {
            Some("list") => reply(true, listing(&w, &known)),
            Some("open") => {
                let Some(kind) = v["browser"].as_str().and_then(Kind::parse) else {
                    return reply(false, json!({"error": "unknown browser"}));
                };
                let url = v["url"].as_str().unwrap_or("");
                match open_with(&w, kind, v["profile"].as_str(), url, &known, &launch) {
                    Ok(m) => reply(true, json!({"message": m})),
                    Err(e) => reply(false, json!({"error": e})),
                }
            }
            _ => reply(false, json!({"error": "unknown request"})),
        }
    }
}
