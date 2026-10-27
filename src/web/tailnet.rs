use serde_json::Value;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const WHOIS_TTL: Duration = Duration::from_secs(180);
const WHOIS_FAIL_TTL: Duration = Duration::from_secs(10);

type StatusFn = Box<dyn Fn() -> Option<Value> + Send + Sync>;
type WhoisFn = Box<dyn Fn(IpAddr) -> Option<Value> + Send + Sync>;

#[derive(Debug, PartialEq, Eq)]
pub enum Who {
    Unknown,

    Allowed(String),

    Refused(String),
}

pub struct Tailnet {
    extra: Vec<String>,

    status: StatusFn,

    whois: WhoisFn,

    known: Mutex<Known>,
    cache: Mutex<HashMap<IpAddr, (Instant, Who)>>,
}

#[derive(Default, Clone)]
struct Known {
    owner: Option<String>,
    ips: Vec<IpAddr>,
}

impl Tailnet {
    pub fn new(extra: &[String], status: StatusFn, whois: WhoisFn) -> Tailnet {
        let t = Tailnet {
            extra: extra
                .iter()
                .map(|u| u.trim().to_ascii_lowercase())
                .filter(|u| !u.is_empty())
                .collect(),
            status,
            whois,
            known: Mutex::new(Known::default()),
            cache: Mutex::new(HashMap::new()),
        };
        t.refresh();
        t
    }

    pub fn system(extra: &[String]) -> Tailnet {
        Tailnet::new(
            extra,
            Box::new(|| cli(&["status", "--json"])),
            Box::new(|ip| cli(&["whois", "--json", &ip.to_string()])),
        )
    }

    pub fn refresh(&self) {
        let Some(v) = (self.status)() else {
            return;
        };
        let me = &v["Self"];
        let ips: Vec<IpAddr> = me["TailscaleIPs"]
            .as_array()
            .map(|a| a.iter().filter_map(|i| i.as_str()?.parse().ok()).collect())
            .unwrap_or_default();

        let tagged = me["Tags"].as_array().is_some_and(|t| !t.is_empty());
        let owner = me["UserID"]
            .as_u64()
            .map(|id| id.to_string())
            .filter(|_| !tagged)
            .and_then(|id| v["User"][&id]["LoginName"].as_str().map(str::to_string))
            .map(|l| l.to_ascii_lowercase());
        let mut k = self.known.lock().unwrap_or_else(|e| e.into_inner());
        k.ips = ips;
        k.owner = owner;
    }

    pub fn owner(&self) -> Option<String> {
        self.known
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .owner
            .clone()
    }

    pub fn own_ips(&self) -> Vec<IpAddr> {
        self.known
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ips
            .clone()
    }

    pub fn ipv4(&self) -> Option<IpAddr> {
        self.own_ips().into_iter().find(IpAddr::is_ipv4)
    }

    pub fn allowed(&self) -> Vec<String> {
        let mut v: Vec<String> = self.owner().into_iter().collect();
        v.extend(self.extra.iter().cloned());
        v
    }

    pub fn judge(&self, login: &str) -> Who {
        let l = login.trim().to_ascii_lowercase();
        if l.is_empty() {
            return Who::Unknown;
        }
        if self.allowed().contains(&l) {
            Who::Allowed(l)
        } else {
            Who::Refused(l)
        }
    }

    pub fn by_ip(&self, peer: IpAddr) -> Who {
        if !in_tailnet(peer) || self.own_ips().contains(&peer) {
            return Who::Unknown;
        }
        let now = Instant::now();
        {
            let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, who)) = c.get(&peer) {
                let ttl = if *who == Who::Unknown {
                    WHOIS_FAIL_TTL
                } else {
                    WHOIS_TTL
                };
                if now.duration_since(*at) < ttl {
                    return clone_who(who);
                }
            }
        }
        let who = match (self.whois)(peer) {
            Some(v) => {
                let tags = v["Node"]["Tags"].as_array().is_some_and(|t| !t.is_empty());
                let login = v["UserProfile"]["LoginName"].as_str().unwrap_or("");
                if tags {
                    Who::Refused(format!("tagged node {}", node_name(&v)))
                } else {
                    self.judge(login)
                }
            }
            None => Who::Unknown,
        };
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.retain(|_, (at, _)| now.duration_since(*at) < WHOIS_TTL);
        c.insert(peer, (now, clone_who(&who)));
        who
    }
}

fn clone_who(w: &Who) -> Who {
    match w {
        Who::Unknown => Who::Unknown,
        Who::Allowed(l) => Who::Allowed(l.clone()),
        Who::Refused(l) => Who::Refused(l.clone()),
    }
}

fn node_name(v: &Value) -> String {
    v["Node"]["Name"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string()
}

pub fn in_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

pub const BINS: [&str; 4] = [
    "tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
];

pub fn cli(args: &[&str]) -> Option<Value> {
    for b in BINS {
        let Ok(out) = std::process::Command::new(b)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        return serde_json::from_slice(&out.stdout).ok();
    }
    None
}

pub fn refused_text(login: &str) -> String {
    format!(
        "This GodTerm is not open to {login}.\n\n\
         Tailscale says this request comes from {login}, which is not the owner \
         of this Mac's tailnet node and not in [web] tailnet_users. The owner can \
         add it there (config.toml), or sign this browser in with the token link \
         `godterm web-url` prints.\n"
    )
}
