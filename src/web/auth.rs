//! Who may use the web app: whoever holds the token in
//! `<app home>/web-token` (made on first start, owner only). A visit with
//! `?t=<token>` trades it for a session cookie; every other request needs
//! that cookie. Wrong tokens are rate limited, and the WebSocket must come
//! from a page of the same origin.

use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const COOKIE: &str = "godterm_web";

pub fn token_path() -> PathBuf {
    crate::config::app_home().join("web-token")
}

/// The token, made (64 hex chars, mode 0600) when there is none.
pub fn load_or_create() -> std::io::Result<String> {
    use crate::platform::OpenOptionsExt;
    use std::io::Write;
    let p = token_path();
    if let Ok(t) = std::fs::read_to_string(&p) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut b = [0u8; 32];
    crate::platform::fill_random(&mut b);
    let tok: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let tmp = p.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(tok.as_bytes())?;
    }
    std::fs::rename(&tmp, &p)?;
    Ok(tok)
}

/// Equal, in time that depends only on the lengths.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The cookie value for a token: derived, so the token itself is never
/// stored in the browser's cookie jar.
pub fn cookie_value(token: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("godterm-web-session:{token}").as_bytes());
    h.iter().map(|x| format!("{x:02x}")).collect()
}

/// The value of cookie `name` in a Cookie header.
pub fn cookie_in<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

/// Set-Cookie for a good login; Secure when the page came over https
/// (Tailscale serve), which a plain http://127.0.0.1 visit cannot use.
pub fn set_cookie(token: &str, https: bool) -> String {
    format!(
        "{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000{}",
        cookie_value(token),
        if https { "; Secure" } else { "" }
    )
}

/// Failed logins: at most `MAX_FAILS` in a `WINDOW`; after that every
/// login is refused until the window passes (even a right token, so
/// guessing gains nothing from going on).
#[derive(Default)]
pub struct Limiter {
    fails: std::collections::VecDeque<Instant>,
}

pub const MAX_FAILS: usize = 10;
pub const WINDOW: Duration = Duration::from_secs(60);

impl Limiter {
    fn prune(&mut self, now: Instant) {
        while self
            .fails
            .front()
            .is_some_and(|t| now.duration_since(*t) >= WINDOW)
        {
            self.fails.pop_front();
        }
    }

    pub fn blocked(&mut self, now: Instant) -> bool {
        self.prune(now);
        self.fails.len() >= MAX_FAILS
    }

    pub fn fail(&mut self, now: Instant) {
        self.prune(now);
        self.fails.push_back(now);
    }
}

/// The host part of a Host header or an origin ("http://h:1/" -> "h:1").
fn authority(s: &str) -> &str {
    let s = s.trim();
    let s = s.split_once("://").map_or(s, |(_, r)| r);
    s.split('/').next().unwrap_or("").trim_end_matches('.')
}

/// The name without a port ("[::1]:7878" -> "[::1]").
fn hostname(a: &str) -> &str {
    if a.starts_with('[') {
        return a
            .split_once(']')
            .map_or(a, |(h, _)| h)
            .trim_start_matches('[');
    }
    a.rsplit_once(':').map_or(a, |(h, p)| {
        if p.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            a
        }
    })
}

/// A Host this server answers to: loopback names, the Mac's tailnet name
/// (*.ts.net, through `tailscale serve`), or one set in `[web] hosts`.
/// Anything else is a DNS rebinding attempt or a mistake.
pub fn host_ok(host: &str, extra: &[String]) -> bool {
    let h = hostname(authority(host)).to_ascii_lowercase();
    matches!(h.as_str(), "127.0.0.1" | "localhost" | "::1")
        || (h.ends_with(".ts.net") && h.len() > 7)
        || extra.iter().any(|e| e.eq_ignore_ascii_case(&h))
}

/// A Host on the Mac's tailnet name (*.ts.net): what `tailscale serve`
/// answers for.
pub fn ts_net_host(host: &str) -> bool {
    let h = hostname(authority(host)).to_ascii_lowercase();
    h.ends_with(".ts.net") && h.len() > 7
}

/// The WebSocket's Origin is this server's own (as the browser reached
/// it: the Host, or the name a proxy forwarded).
pub fn origin_ok(origin: Option<&str>, host: &str, forwarded: Option<&str>) -> bool {
    let Some(o) = origin else {
        return false;
    };
    let o = authority(o).to_ascii_lowercase();
    if o.is_empty() {
        return false;
    }
    // Exactly, port included: cookies are per site, not per port, so a
    // page on another local port must not get in with ours.
    let same = |h: &str| authority(h).to_ascii_lowercase() == o;
    same(host) || forwarded.is_some_and(same)
}

/// The other processes on a TCP connection whose client side is
/// `ip`:`port` (lsof). None when that cannot be told: lsof must list this
/// process (the server end), or it saw nothing at all.
fn conn_peers(ip: std::net::IpAddr, port: u16) -> Option<Vec<u32>> {
    let addr = match ip {
        std::net::IpAddr::V4(v) => v.to_string(),
        std::net::IpAddr::V6(v) => format!("[{v}]"),
    };
    let out = std::process::Command::new(if cfg!(target_os = "macos") {
        "/usr/sbin/lsof"
    } else {
        "lsof"
    })
    .args(["-nP", "-t", &format!("-iTCP@{addr}:{port}")])
    .stdin(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .output()
    .ok()?;
    let me = std::process::id();
    let pids: Vec<u32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    pids.contains(&me)
        .then(|| pids.into_iter().filter(|p| *p != me).collect())
}

/// The connection comes from inside GodTerm: a tab's agent (or anything
/// else it started) holding the token, being the same user that can read
/// the token file. Those get the read-only control socket, never the
/// user's own access. A browser is never GodTerm's child; a process that
/// detached itself is not caught (this is a second wall, not the first).
/// `local`: the peer is on this machine (loopback, or this Mac's own
/// tailnet address).
pub fn from_inside(peer: std::net::SocketAddr, local: bool) -> bool {
    if !local || !cfg!(unix) {
        return false;
    }
    let me = std::process::id();
    conn_peers(peer.ip(), peer.port())
        .unwrap_or_default()
        .into_iter()
        .any(|p| crate::peer::descends_from(p, &[me], crate::peer::parent_of))
}

/// The executable a process runs (the kernel's answer, not its argv[0]).
#[cfg(target_os = "macos")]
fn exe_of(pid: u32) -> Option<std::path::PathBuf> {
    let mut buf = vec![0u8; 4096];
    // SAFETY: the buffer is valid for its length; proc_pidpath writes at
    // most that many bytes and returns how many.
    let n = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(std::path::PathBuf::from(String::from_utf8(buf).ok()?))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn exe_of(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Tailscale's own program: owned by root (so not something this user
/// made) and named for it.
#[cfg(not(unix))]
fn is_tailscale(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn is_tailscale(pid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(exe) = exe_of(pid) else {
        return false;
    };
    let named = exe
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().contains("tailscale"));
    named && std::fs::metadata(&exe).is_ok_and(|m| m.uid() == 0)
}

/// A loopback connection whose client end only Tailscale could hold: no
/// process this user can see holds it, except Tailscale's own (tailscaled
/// runs as root, so lsof shows nothing for it). A local process that
/// sends serve's identity headers itself is seen here and refused. False
/// when it cannot be told.
pub fn via_tailscale(peer: std::net::SocketAddr) -> bool {
    if !peer.ip().is_loopback() || !cfg!(unix) {
        return false;
    }
    match conn_peers(peer.ip(), peer.port()) {
        Some(pids) => pids.into_iter().all(is_tailscale),
        None => false,
    }
}
