//! Signing in by tailnet identity, with no token:
//!
//! - through `tailscale serve` (https://<mac>.ts.net:<port>/): serve
//!   proxies to 127.0.0.1 and adds `Tailscale-User-Login` for requests
//!   from a tailnet user (never for tagged nodes; a copy the client sent
//!   is stripped). The header is trusted only on loopback, with a
//!   `*.ts.net` Host, when no other process of this user holds the client
//!   end of the connection (tailscaled runs as root; a local process
//!   forging the header is seen with lsof and refused);
//! - on this Mac's tailnet IPv4 (`[web] tailnet_ip`, port `[web] ip_port`):
//!   the peer address is asked of `tailscale whois` (cached a few minutes).
//!
//! Either way the login must be the owner of this Mac's tailnet node or in
//! `[web] tailnet_users`; another tailnet user (a device shared in from
//! another account) is refused with a page that says so.

use serde_json::Value;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a whois answer is kept (a failed one for less).
pub const WHOIS_TTL: Duration = Duration::from_secs(180);
const WHOIS_FAIL_TTL: Duration = Duration::from_secs(10);

type StatusFn = Box<dyn Fn() -> Option<Value> + Send + Sync>;
type WhoisFn = Box<dyn Fn(IpAddr) -> Option<Value> + Send + Sync>;

/// Who a request is, by its tailnet identity.
#[derive(Debug, PartialEq, Eq)]
pub enum Who {
    /// No identity to go by: the token or cookie decides.
    Unknown,
    /// An allowed login.
    Allowed(String),
    /// A tailnet user that is not allowed (or a tagged node).
    Refused(String),
}

pub struct Tailnet {
    /// `[web] tailnet_users` (lower case).
    extra: Vec<String>,
    /// `tailscale status --json` (the whole answer).
    status: StatusFn,
    /// `tailscale whois --json <ip>`.
    whois: WhoisFn,
    /// From the last status: the node owner's login and this node's
    /// addresses.
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

    /// With the real `tailscale` CLI.
    pub fn system(extra: &[String]) -> Tailnet {
        Tailnet::new(
            extra,
            Box::new(|| cli(&["status", "--json"])),
            Box::new(|ip| cli(&["whois", "--json", &ip.to_string()])),
        )
    }

    /// Ask tailscale again (the owner and this node's addresses). Kept as
    /// they were when it does not answer.
    pub fn refresh(&self) {
        let Some(v) = (self.status)() else {
            return;
        };
        let me = &v["Self"];
        let ips: Vec<IpAddr> = me["TailscaleIPs"]
            .as_array()
            .map(|a| a.iter().filter_map(|i| i.as_str()?.parse().ok()).collect())
            .unwrap_or_default();
        // A tagged node has no owner to go by: only tailnet_users.
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

    /// This node's tailnet addresses.
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

    /// The logins that sign in: the node's owner and `tailnet_users`.
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

    /// A connection to the tailnet IP listener, by its peer address.
    /// This Mac's own addresses are never signed in this way (a local
    /// process could reach them); they need the token.
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

/// A Tailscale address: 100.64.0.0/10 or fd7a:115c:a1e0::/48.
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

/// The tailscale CLIs to try, in order.
pub const BINS: [&str; 4] = [
    "tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
];

/// The JSON a tailscale subcommand prints, from the first CLI that
/// answers.
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

/// The page another tailnet user gets.
pub fn refused_text(login: &str) -> String {
    format!(
        "This GodTerm is not open to {login}.\n\n\
         Tailscale says this request comes from {login}, which is not the owner \
         of this Mac's tailnet node and not in [web] tailnet_users. The owner can \
         add it there (config.toml), or sign this browser in with the token link \
         `godterm web-url` prints.\n"
    )
}
