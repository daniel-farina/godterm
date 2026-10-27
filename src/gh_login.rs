use serde::Serialize;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const ARGS: [&str; 7] = [
    "auth",
    "login",
    "--hostname",
    "github.com",
    "--git-protocol",
    "https",
    "--web",
];

const DEVICE_URL: &str = "https://github.com/login/device";
const GIVE_UP: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Info {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Info {
    pub fn running(&self) -> bool {
        self.state == "starting" || self.state == "code"
    }
}

pub fn parse_code(line: &str) -> Option<String> {
    let i = line.find("one-time code")?;
    let rest = line[i..].split_once(':')?.1;
    let code: String = rest
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let ok = (4..=16).contains(&code.len())
        && code.contains('-')
        && code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-');
    ok.then_some(code)
}

pub fn parse_url(line: &str) -> Option<String> {
    let i = line.find("https://github.com/")?;
    let u: String = line[i..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    let u = u.trim_end_matches(['.', ',', ')']).to_string();
    u.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/:.-_?=&%".contains(c))
        .then_some(u)
}

pub fn parse_account(line: &str) -> Option<String> {
    let i = line.find("Logged in as ")?;
    let name: String = line[i + 13..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    crate::git_map::valid_owner(&name).then_some(name)
}

fn take_line(info: &mut Info, last: &mut String, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if let Some(c) = parse_code(line) {
        info.code = Some(c);
        if info.state == "starting" {
            info.state = "code".into();
        }
        info.url.get_or_insert_with(|| DEVICE_URL.into());
        return;
    }
    if let Some(u) = parse_url(line) {
        info.url = Some(u);
        return;
    }
    if let Some(a) = parse_account(line) {
        info.account = Some(a);
        return;
    }

    if !line.contains("gho_") && !line.contains("ghp_") {
        *last = line.chars().take(300).collect();
    }
}

pub struct Login {
    info: Arc<Mutex<Info>>,
    child: Arc<Mutex<Option<Child>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Login {
    pub fn start(gh: &Path, env: &[(&str, &str)]) -> Login {
        let info = Arc::new(Mutex::new(Info {
            state: "starting".into(),
            ..Default::default()
        }));
        let child = Arc::new(Mutex::new(None));
        let me = Login {
            info: Arc::clone(&info),
            child: Arc::clone(&child),
        };
        if let Err(e) = crate::git_cred::gh_args_ok(&ARGS) {
            *lock(&info) = Info {
                state: "failed".into(),
                error: Some(e),
                ..Default::default()
            };
            return me;
        }
        let mut c = Command::new(gh);
        c.args(ARGS)
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_ENTERPRISE_TOKEN")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            .env("GH_SPINNER_DISABLED", "1")
            .env("BROWSER", "true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut ch = match c.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                *lock(&info) = Info {
                    state: "failed".into(),
                    error: Some(format!("could not run gh: {e}")),
                    ..Default::default()
                };
                return me;
            }
        };
        let last = Arc::new(Mutex::new(String::new()));
        let mut readers = vec![];
        let outs: Vec<Box<dyn Read + Send>> = [
            ch.stdout
                .take()
                .map(|o| Box::new(o) as Box<dyn Read + Send>),
            ch.stderr
                .take()
                .map(|o| Box::new(o) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        .collect();
        for o in outs {
            let (info, last) = (Arc::clone(&info), Arc::clone(&last));
            readers.push(std::thread::spawn(move || {
                for l in BufReader::new(o).lines().map_while(Result::ok) {
                    take_line(&mut lock(&info), &mut lock(&last), &l);
                }
            }));
        }
        *lock(&child) = Some(ch);
        let (info2, child2) = (Arc::clone(&info), Arc::clone(&child));
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let status = loop {
                std::thread::sleep(Duration::from_millis(150));
                let mut g = lock(&child2);
                let Some(ch) = g.as_mut() else {
                    break None;
                };
                match ch.try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) if t0.elapsed() > GIVE_UP => {
                        let _ = ch.kill();
                        let _ = ch.wait();
                        let mut i = lock(&info2);
                        i.state = "failed".into();
                        i.error = Some("gave up after 15 minutes".into());
                        *g = None;
                        return;
                    }
                    Ok(None) => {}
                    Err(_) => break None,
                }
            };
            *lock(&child2) = None;
            for r in readers {
                let _ = r.join();
            }
            let mut i = lock(&info2);
            if i.state == "cancelled" {
                return;
            }
            match status {
                Some(s) if s.success() => i.state = "done".into(),
                _ => {
                    i.state = "failed".into();
                    let l = lock(&last).clone();
                    i.error = Some(if l.is_empty() {
                        "gh auth login did not finish".into()
                    } else {
                        l
                    });
                }
            }
        });
        me
    }

    pub fn info(&self) -> Info {
        lock(&self.info).clone()
    }

    pub fn cancel(&self) {
        let mut g = lock(&self.child);
        if let Some(ch) = g.as_mut() {
            lock(&self.info).state = "cancelled".into();
            let _ = ch.kill();
            let _ = ch.wait();
        }
        *g = None;
    }
}

impl Drop for Login {
    fn drop(&mut self) {
        self.cancel();
    }
}

static CURRENT: Mutex<Option<Login>> = Mutex::new(None);

pub fn start(gh: &Path) -> Info {
    let mut g = lock(&CURRENT);
    if let Some(l) = g.as_ref().filter(|l| l.info().running()) {
        return l.info();
    }
    let l = Login::start(gh, &[]);
    let i = l.info();
    *g = Some(l);
    i
}

pub fn poll() -> Info {
    lock(&CURRENT).as_ref().map(Login::info).unwrap_or_default()
}

pub fn cancel() -> Info {
    let g = lock(&CURRENT);
    match g.as_ref() {
        Some(l) => {
            l.cancel();
            l.info()
        }
        None => Info::default(),
    }
}

#[cfg(all(test, unix))]
#[path = "gh_login_tests.rs"]
mod tests;
