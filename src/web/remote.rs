use serde_json::{json, Value};

pub type Run<'a> = &'a dyn Fn(&[String]) -> Option<(bool, String)>;

pub fn system_run(args: &[String]) -> Option<(bool, String)> {
    for b in super::tailnet::BINS {
        let mut c = std::process::Command::new(b);
        c.args(args);
        match crate::procs::output_timeout(&mut c, std::time::Duration::from_secs(25)) {
            Ok(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&o.stderr));
                return Some((o.status.success(), text));
            }
            Err(e) if e.contains("timed out") => {
                return Some((
                    false,
                    "tailscale did not answer in time; it may be waiting for an approval in the admin console".into(),
                ))
            }
            Err(_) => continue,
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    pub installed: bool,
    pub running: bool,
    pub host: String,
    pub url: String,
    pub public: bool,
    pub https_port: u16,
}

impl Remote {
    pub fn json(&self) -> Value {
        json!({"installed": self.installed, "running": self.running, "host": self.host,
            "url": self.url, "public": self.public, "https_port": self.https_port})
    }
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn json_of(run: Run, a: &[&str]) -> Option<Value> {
    let (ok, text) = run(&args(a))?;
    if !ok {
        return None;
    }
    serde_json::from_str(text.trim()).ok()
}

fn proxies_to(v: &Value, web_port: u16) -> bool {
    let want = [format!(":{web_port}"), format!(":{web_port}/")];
    v["Handlers"].as_object().is_some_and(|h| {
        h.values().any(|x| {
            x["Proxy"].as_str().is_some_and(|p| {
                (p.contains("127.0.0.1") || p.contains("localhost"))
                    && want.iter().any(|w| p.ends_with(w.as_str()))
            })
        })
    })
}

fn split_hostport(hp: &str) -> (String, u16) {
    match hp.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(443)),
        None => (hp.to_string(), 443),
    }
}

fn used_ports(serve: &Value) -> Vec<u16> {
    let mut v: Vec<u16> = serve["TCP"]
        .as_object()
        .map(|t| t.keys().filter_map(|k| k.parse().ok()).collect())
        .unwrap_or_default();
    if let Some(w) = serve["Web"].as_object() {
        v.extend(w.keys().map(|k| split_hostport(k).1));
    }
    v
}

pub fn status(run: Run, web_port: u16) -> Remote {
    let mut r = Remote {
        installed: false,
        running: false,
        host: String::new(),
        url: String::new(),
        public: false,
        https_port: 0,
    };
    let Some((_, _)) = run(&args(&["version"])) else {
        return r;
    };
    r.installed = true;
    let Some(st) = json_of(run, &["status", "--json"]) else {
        return r;
    };
    r.running = st["BackendState"].as_str() == Some("Running");
    r.host = st["Self"]["DNSName"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string();
    if !r.running {
        return r;
    }
    let Some(serve) = json_of(run, &["serve", "status", "--json"]) else {
        return r;
    };
    if let Some(web) = serve["Web"].as_object() {
        for (hp, cfg) in web {
            if proxies_to(cfg, web_port) {
                let (host, port) = split_hostport(hp);
                r.https_port = port;
                r.url = if port == 443 {
                    format!("https://{host}")
                } else {
                    format!("https://{host}:{port}")
                };
                r.public = serve["AllowFunnel"][hp.as_str()].as_bool() == Some(true);
                break;
            }
        }
    }
    r
}

fn short(text: &str) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    t.chars().take(400).collect()
}

fn pick_port(run: Run, web_port: u16, current: &Remote) -> Result<u16, String> {
    if current.https_port != 0 {
        return Ok(current.https_port);
    }
    let serve = json_of(run, &["serve", "status", "--json"]).unwrap_or(json!({}));
    let used = used_ports(&serve);
    [443u16, 8443, 10000]
        .into_iter()
        .find(|p| !used.contains(p))
        .ok_or_else(|| {
            format!("Tailscale ports 443, 8443 and 10000 are all in use; free one of them (port {web_port} is GodTerm's)")
        })
}

pub fn create(run: Run, web_port: u16, public: bool) -> Result<Remote, String> {
    let cur = status(run, web_port);
    if !cur.installed {
        return Err("Tailscale is not installed on this Mac".into());
    }
    if !cur.running {
        return Err("Tailscale is not running or not signed in".into());
    }
    let port = pick_port(run, web_port, &cur)?;
    if cur.public && !public {
        let (ok, text) = run(&args(&["serve", &format!("--https={port}"), "off"]))
            .ok_or("Tailscale is not installed on this Mac")?;
        if !ok {
            return Err(short(&text));
        }
    }
    let verb = if public { "funnel" } else { "serve" };
    let (ok, text) = run(&args(&[
        verb,
        "--bg",
        &format!("--https={port}"),
        &format!("http://127.0.0.1:{web_port}"),
    ]))
    .ok_or("Tailscale is not installed on this Mac")?;
    if !ok {
        return Err(short(&text));
    }
    let now = status(run, web_port);
    if now.url.is_empty() {
        return Err(format!(
            "Tailscale did not publish the address: {}",
            short(&text)
        ));
    }
    if public && !now.public {
        return Err(format!(
            "Tailscale did not turn Funnel on: {}",
            short(&text)
        ));
    }
    Ok(now)
}

pub fn make_private(run: Run, web_port: u16) -> Result<Remote, String> {
    let cur = status(run, web_port);
    if cur.url.is_empty() || !cur.public {
        return Ok(cur);
    }
    create(run, web_port, false)
}

pub fn remove(run: Run, web_port: u16) -> Result<(), String> {
    let cur = status(run, web_port);
    if cur.url.is_empty() {
        return Ok(());
    }
    let (ok, text) = run(&args(&[
        "serve",
        &format!("--https={}", cur.https_port),
        "off",
    ]))
    .ok_or("Tailscale is not installed on this Mac")?;
    if ok {
        Ok(())
    } else {
        Err(short(&text))
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    pub struct Ts {
        pub state: Mutex<(Option<(u16, bool)>, Vec<Vec<String>>)>,
        pub web_port: u16,
        pub running: bool,
        pub busy_443: bool,
    }

    impl Ts {
        pub fn new(web_port: u16) -> Ts {
            Ts {
                state: Mutex::new((None, vec![])),
                web_port,
                running: true,
                busy_443: false,
            }
        }

        pub fn run(&self, a: &[String]) -> Option<(bool, String)> {
            let mut g = self.state.lock().unwrap();
            g.1.push(a.to_vec());
            let w: Vec<&str> = a.iter().map(String::as_str).collect();
            match w.as_slice() {
                ["version"] => Some((true, "1.80.0\n".into())),
                ["status", "--json"] => Some((
                    true,
                    json!({"BackendState": if self.running {"Running"} else {"Stopped"},
                        "Self": {"DNSName": "mac.example.ts.net."}})
                    .to_string(),
                )),
                ["serve", "status", "--json"] => {
                    let mut v = json!({});
                    let mut tcp = serde_json::Map::new();
                    if self.busy_443 {
                        tcp.insert("443".into(), json!({"HTTPS": true}));
                        v["Web"] = json!({"mac.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:9999"}}}});
                    }
                    if let Some((p, public)) = g.0 {
                        tcp.insert(p.to_string(), json!({"HTTPS": true}));
                        let key = format!("mac.example.ts.net:{p}");
                        let mut web = v["Web"].as_object().cloned().unwrap_or_default();
                        web.insert(key.clone(), json!({"Handlers": {"/": {"Proxy": format!("http://127.0.0.1:{}", self.web_port)}}}));
                        v["Web"] = Value::Object(web);
                        if public {
                            v["AllowFunnel"] = json!({ key: true });
                        }
                    }
                    v["TCP"] = Value::Object(tcp);
                    Some((true, v.to_string()))
                }
                [verb @ ("serve" | "funnel"), "--bg", https, _] => {
                    let p: u16 = https.trim_start_matches("--https=").parse().unwrap();
                    g.0 = Some((p, *verb == "funnel"));
                    Some((true, String::new()))
                }
                ["serve", _, "off"] => {
                    g.0 = None;
                    Some((true, String::new()))
                }
                _ => Some((false, "unknown".into())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Ts;
    use super::*;

    #[test]
    fn creates_a_private_address_then_a_public_one_and_removes_it() {
        let ts = Ts::new(7878);
        let run = |a: &[String]| ts.run(a);
        let s = status(&run, 7878);
        assert!(s.installed && s.running && s.url.is_empty());
        let r = create(&run, 7878, false).unwrap();
        assert_eq!(r.url, "https://mac.example.ts.net");
        assert!(!r.public);
        let r = create(&run, 7878, true).unwrap();
        assert!(r.public);
        let r = make_private(&run, 7878).unwrap();
        assert!(!r.public && r.url == "https://mac.example.ts.net");
        remove(&run, 7878).unwrap();
        assert!(status(&run, 7878).url.is_empty());
    }

    #[test]
    fn another_service_on_443_moves_us_to_8443_and_never_shell_strings() {
        let mut ts = Ts::new(7878);
        ts.busy_443 = true;
        let run = |a: &[String]| ts.run(a);
        let r = create(&run, 7878, false).unwrap();
        assert_eq!(r.url, "https://mac.example.ts.net:8443");
        let g = ts.state.lock().unwrap();
        assert!(g
            .1
            .iter()
            .any(|c| c == &args(&["serve", "--bg", "--https=8443", "http://127.0.0.1:7878"])));
    }

    #[test]
    fn stopped_or_missing_tailscale_is_reported() {
        let mut ts = Ts::new(7878);
        ts.running = false;
        let run = |a: &[String]| ts.run(a);
        assert!(create(&run, 7878, false)
            .unwrap_err()
            .contains("not running"));
        let none = |_: &[String]| None;
        let s = status(&none, 7878);
        assert!(!s.installed);
        assert!(create(&none, 7878, false)
            .unwrap_err()
            .contains("not installed"));
    }
}
