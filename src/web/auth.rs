use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const COOKIE: &str = "godterm_web";

pub fn token_path() -> PathBuf {
    crate::config::app_home().join("web-token")
}

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

pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn cookie_value(token: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("godterm-web-session:{token}").as_bytes());
    h.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn cookie_in<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

pub fn set_cookie(token: &str, https: bool) -> String {
    format!(
        "{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000{}",
        cookie_value(token),
        if https { "; Secure" } else { "" }
    )
}

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

fn authority(s: &str) -> &str {
    let s = s.trim();
    let s = s.split_once("://").map_or(s, |(_, r)| r);
    s.split('/').next().unwrap_or("").trim_end_matches('.')
}

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

pub fn host_ok(host: &str, extra: &[String]) -> bool {
    let h = hostname(authority(host)).to_ascii_lowercase();
    matches!(h.as_str(), "127.0.0.1" | "localhost" | "::1")
        || (h.ends_with(".ts.net") && h.len() > 7)
        || extra.iter().any(|e| e.eq_ignore_ascii_case(&h))
}

pub fn ts_net_host(host: &str) -> bool {
    let h = hostname(authority(host)).to_ascii_lowercase();
    h.ends_with(".ts.net") && h.len() > 7
}

pub fn origin_ok(origin: Option<&str>, host: &str, forwarded: Option<&str>) -> bool {
    let Some(o) = origin else {
        return false;
    };
    let o = authority(o).to_ascii_lowercase();
    if o.is_empty() {
        return false;
    }

    let same = |h: &str| authority(h).to_ascii_lowercase() == o;
    same(host) || forwarded.is_some_and(same)
}

fn conn_peers(ip: std::net::IpAddr, port: u16) -> Option<Vec<u32>> {
    let addr = match ip {
        std::net::IpAddr::V4(v) => v.to_string(),
        std::net::IpAddr::V6(v) => format!("[{v}]"),
    };
    let pids: Vec<u32> = if cfg!(windows) {
        let out = crate::procs::output_timeout(
            std::process::Command::new("netstat").args(["-ano", "-p", "TCP"]),
            std::time::Duration::from_secs(3),
        )
        .ok()?;
        parse_netstat_conn(&String::from_utf8_lossy(&out.stdout), &addr, port)
    } else {
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
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    };
    let me = std::process::id();
    pids.contains(&me)
        .then(|| pids.into_iter().filter(|p| *p != me).collect())
}

pub(crate) fn parse_netstat_conn(text: &str, addr: &str, port: u16) -> Vec<u32> {
    let end = format!("{addr}:{port}");
    let mut out = vec![];
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();

        if w.len() < 5 || !w[0].eq_ignore_ascii_case("tcp") {
            continue;
        }
        if !w[3].eq_ignore_ascii_case("established") || (w[1] != end && w[2] != end) {
            continue;
        }
        if let Ok(pid) = w[4].parse::<u32>() {
            if !out.contains(&pid) {
                out.push(pid);
            }
        }
    }
    out
}

pub fn from_inside(peer: std::net::SocketAddr, local: bool) -> bool {
    if !local {
        return false;
    }
    let me = std::process::id();
    let peers = conn_peers(peer.ip(), peer.port()).unwrap_or_default();
    if peers.is_empty() {
        return false;
    }
    let parent = crate::peer::parents();
    peers
        .into_iter()
        .any(|p| crate::peer::descends_from(p, &[me], &parent))
}

#[cfg(target_os = "macos")]
fn exe_of(pid: u32) -> Option<std::path::PathBuf> {
    let mut buf = vec![0u8; 4096];

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

#[cfg(windows)]
fn is_tailscale(pid: u32) -> bool {
    let Some(exe) = crate::procs::process(pid).and_then(|p| p.exe) else {
        return false;
    };
    let named = exe
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().starts_with("tailscale"));
    let dirs: Vec<std::path::PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|k| std::env::var_os(k))
        .map(std::path::PathBuf::from)
        .collect();
    named && dirs.iter().any(|d| exe.starts_with(d))
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

pub fn via_tailscale(peer: std::net::SocketAddr) -> bool {
    if !peer.ip().is_loopback() {
        return false;
    }
    match conn_peers(peer.ip(), peer.port()) {
        Some(pids) => pids.into_iter().all(is_tailscale),
        None => false,
    }
}
