use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const LIMITS: &str = "Sets proxy variables for the tab's agent. Tools that ignore them (ssh, raw sockets, some CLIs) still connect directly: this is not a kill switch.";
pub const TEST_HOST: &str = "api.anthropic.com";
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
const IO_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct RouteCfg {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub exit_node: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub allow_lan: bool,
}

impl std::fmt::Debug for RouteCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteCfg")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("url", &mask_url(&self.url))
            .field("exit_node", &self.exit_node)
            .field("allow_lan", &self.allow_lan)
            .finish()
    }
}

impl RouteCfg {
    pub fn is_tailscale(&self) -> bool {
        self.kind == "tailscale"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct RoutesCfg {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tailscaled_bin: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tailscale_bin: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<RouteCfg>,
}

impl RoutesCfg {
    pub fn get(&self, name: &str) -> Option<&RouteCfg> {
        self.list.iter().find(|r| r.name == name)
    }
}

pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 24
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !n.starts_with('-')
        && n != "direct"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
    Socks5,
    Socks5h,
}

impl Scheme {
    pub fn word(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
            Scheme::Socks5 => "socks5",
            Scheme::Socks5h => "socks5h",
        }
    }

    pub fn is_socks(self) -> bool {
        matches!(self, Scheme::Socks5 | Scheme::Socks5h)
    }
}

#[derive(Clone, PartialEq)]
pub struct ProxyUrl {
    pub scheme: Scheme,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub host: String,
    pub port: u16,
}

impl std::fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.masked())
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|x| u8::from_str_radix(x, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl ProxyUrl {
    pub fn parse(text: &str) -> Result<ProxyUrl, String> {
        let t = text.trim();
        if t.is_empty() {
            return Err("the proxy URL is empty".into());
        }
        if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("the proxy URL has spaces or control characters".into());
        }
        let (scheme, rest) = t
            .split_once("://")
            .ok_or("the proxy URL needs a scheme: http://, https://, socks5:// or socks5h://")?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            "socks5" => Scheme::Socks5,
            "socks5h" => Scheme::Socks5h,
            _ => return Err("only http://, https://, socks5:// and socks5h:// proxies".into()),
        };
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (userinfo, hostport) = match rest.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, rest),
        };
        if hostport.contains(['/', '?', '#']) {
            return Err("the proxy URL must not have a path".into());
        }
        let (host, port) = if let Some(r) = hostport.strip_prefix('[') {
            let (h, p) = r
                .split_once(']')
                .ok_or("bad IPv6 address in the proxy URL")?;
            (h.to_string(), p.strip_prefix(':'))
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (hostport.to_string(), None),
            }
        };
        if host.is_empty() {
            return Err("the proxy URL has no host".into());
        }
        let port = match port {
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or("the proxy URL has a bad port")?,
            None => match scheme {
                Scheme::Http => 80,
                Scheme::Https => 443,
                _ => 1080,
            },
        };
        let (user, pass) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((a, b)) => (Some(pct_decode(a)), Some(pct_decode(b))),
                None => (Some(pct_decode(u)), None),
            },
            None => (None, None),
        };
        Ok(ProxyUrl {
            scheme,
            user,
            pass,
            host,
            port,
        })
    }

    pub fn hostport(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn masked(&self) -> String {
        format!(
            "{}://{}{}",
            self.scheme.word(),
            if self.user.is_some() { "***@" } else { "" },
            self.hostport()
        )
    }

    pub fn has_secret(&self) -> bool {
        self.user.is_some()
    }
}

pub fn mask_url(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    match ProxyUrl::parse(text) {
        Ok(u) => u.masked(),
        Err(_) => "(not a valid proxy URL)".into(),
    }
}

pub fn no_proxy(existing: Option<&str>) -> String {
    let mut out: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    for part in existing
        .unwrap_or("")
        .split([',', ' '])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(part)) {
            out.push(part.to_string());
        }
    }
    out.join(",")
}

pub fn existing_no_proxy() -> Option<String> {
    std::env::var("NO_PROXY")
        .ok()
        .or_else(|| std::env::var("no_proxy").ok())
}

#[cfg(test)]
pub const PROXY_VARS: [&str; 8] = [
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
    "no_proxy",
];

pub fn proxy_env(http: &str, all: &str, existing: Option<&str>) -> Vec<(String, String)> {
    let np = no_proxy(existing);
    let mut out = vec![];
    for (k, v) in [
        ("HTTPS_PROXY", http),
        ("HTTP_PROXY", http),
        ("ALL_PROXY", all),
        ("NO_PROXY", np.as_str()),
    ] {
        out.push((k.to_string(), v.to_string()));
        out.push((k.to_ascii_lowercase(), v.to_string()));
    }
    out
}

fn dial(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {host}: {e}"))?;
    let mut last = format!("{host} has no address");
    for a in addrs.take(4) {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(IO_TIMEOUT));
                let _ = s.set_write_timeout(Some(IO_TIMEOUT));
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = format!("could not connect to {host}:{port}: {e}"),
        }
    }
    Err(last)
}

pub fn reachable(host: &str, port: u16, timeout: Duration) -> Result<(), String> {
    dial(host, port, timeout).map(|_| ())
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn read_head<S: Read>(s: &mut S, max: usize) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(512);
    let mut one = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if buf.len() >= max {
            return Err("the reply headers are too long".into());
        }
        match s.read(&mut one) {
            Ok(0) => return Err("the connection closed early".into()),
            Ok(_) => buf.push(one[0]),
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    Ok(buf)
}

fn http_connect<S: Read + Write>(
    s: &mut S,
    u: &ProxyUrl,
    host: &str,
    port: u16,
) -> Result<(), String> {
    let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if let Some(user) = &u.user {
        let cred = format!("{user}:{}", u.pass.as_deref().unwrap_or(""));
        req.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            b64(cred.as_bytes())
        ));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())
        .and_then(|_| s.flush())
        .map_err(|e| format!("the proxy closed the connection: {e}"))?;
    let head = read_head(s, 16 * 1024)?;
    let line = String::from_utf8_lossy(&head);
    let status = line.lines().next().unwrap_or("").to_string();
    let code = status.split_whitespace().nth(1).unwrap_or("");
    match code {
        "200" => Ok(()),
        "407" => Err("the proxy asked for a user name and password (407)".into()),
        _ => Err(format!(
            "the proxy refused the connection ({})",
            status.chars().take(60).collect::<String>()
        )),
    }
}

fn socks5_connect<S: Read + Write>(
    s: &mut S,
    u: &ProxyUrl,
    host: &str,
    port: u16,
) -> Result<(), String> {
    let io = |e: std::io::Error| format!("the SOCKS proxy did not answer: {e}");
    let hello: &[u8] = if u.user.is_some() {
        &[5, 2, 0, 2]
    } else {
        &[5, 1, 0]
    };
    s.write_all(hello).map_err(io)?;
    let mut r = [0u8; 2];
    s.read_exact(&mut r).map_err(io)?;
    if r[0] != 5 {
        return Err("that is not a SOCKS5 proxy".into());
    }
    match r[1] {
        0 => {}
        2 => {
            let user = u.user.clone().unwrap_or_default();
            let pass = u.pass.clone().unwrap_or_default();
            if user.len() > 255 || pass.len() > 255 {
                return Err("the SOCKS user name or password is too long".into());
            }
            let mut m = vec![1, user.len() as u8];
            m.extend(user.as_bytes());
            m.push(pass.len() as u8);
            m.extend(pass.as_bytes());
            s.write_all(&m).map_err(io)?;
            s.read_exact(&mut r).map_err(io)?;
            if r[1] != 0 {
                return Err("the SOCKS proxy refused the user name and password".into());
            }
        }
        _ => return Err("the SOCKS proxy wants a login this route does not have".into()),
    }
    let mut m = vec![5, 1, 0];
    let ip = host.parse::<std::net::IpAddr>().ok().or_else(|| {
        (u.scheme == Scheme::Socks5)
            .then(|| (host, port).to_socket_addrs().ok()?.next().map(|a| a.ip()))
            .flatten()
    });
    match ip {
        Some(std::net::IpAddr::V4(a)) => {
            m.push(1);
            m.extend(a.octets());
        }
        Some(std::net::IpAddr::V6(a)) => {
            m.push(4);
            m.extend(a.octets());
        }
        None => {
            if host.len() > 255 {
                return Err("the host name is too long".into());
            }
            m.push(3);
            m.push(host.len() as u8);
            m.extend(host.as_bytes());
        }
    }
    m.extend(port.to_be_bytes());
    s.write_all(&m).map_err(io)?;
    let mut h = [0u8; 4];
    s.read_exact(&mut h).map_err(io)?;
    if h[1] != 0 {
        return Err(format!(
            "the SOCKS proxy could not reach {host} (code {})",
            h[1]
        ));
    }
    let n = match h[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).map_err(io)?;
            l[0] as usize
        }
        _ => return Err("the SOCKS proxy sent a bad reply".into()),
    };
    let mut skip = vec![0u8; n + 2];
    s.read_exact(&mut skip).map_err(io)?;
    Ok(())
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

fn tls_over<S: Read + Write>(
    stream: S,
    host: &str,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, S>, String> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| format!("{host} is not a valid TLS name"))?;
    let conn = rustls::ClientConnection::new(tls_config(), name)
        .map_err(|e| format!("TLS setup failed: {e}"))?;
    let mut tls = rustls::StreamOwned::new(conn, stream);
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|e| format!("the TLS handshake with {host} failed: {e}"))?;
    }
    Ok(tls)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub ok: bool,
    pub detail: String,
}

#[cfg(test)]
pub fn tunnel_only(u: &ProxyUrl, host: &str, port: u16) -> Result<(), String> {
    let mut s = dial(&u.host, u.port, CONNECT_TIMEOUT)?;
    match u.scheme {
        Scheme::Http => http_connect(&mut s, u, host, port),
        Scheme::Https => {
            let mut t = tls_over(s, &u.host)?;
            http_connect(&mut t, u, host, port)
        }
        _ => socks5_connect(&mut s, u, host, port),
    }
}

pub fn probe(u: &ProxyUrl, host: &str) -> Probe {
    let t0 = std::time::Instant::now();
    let r = (|| -> Result<(), String> {
        let mut s = dial(&u.host, u.port, CONNECT_TIMEOUT)?;
        match u.scheme {
            Scheme::Http => {
                http_connect(&mut s, u, host, 443)?;
                tls_over(s, host).map(|_| ())
            }
            Scheme::Https => {
                let mut t = tls_over(s, &u.host)?;
                http_connect(&mut t, u, host, 443)?;
                tls_over(t, host).map(|_| ())
            }
            _ => {
                socks5_connect(&mut s, u, host, 443)?;
                tls_over(s, host).map(|_| ())
            }
        }
    })();
    match r {
        Ok(()) => Probe {
            ok: true,
            detail: format!(
                "Reached {host} through the route (TLS handshake only, {} ms)",
                t0.elapsed().as_millis()
            ),
        },
        Err(e) => Probe {
            ok: false,
            detail: e,
        },
    }
}

pub fn free_port() -> Option<u16> {
    TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

pub struct Bridge {
    pub port: u16,
    stop: Arc<AtomicBool>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn relay(a: TcpStream, b: TcpStream) {
    let _ = a.set_read_timeout(None);
    let _ = b.set_read_timeout(None);
    let (Ok(mut a2), Ok(mut b2)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let (mut a1, mut b1) = (a, b);
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a2, &mut b1);
        let _ = b1.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut b2, &mut a1);
    let _ = a1.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
}

fn target_of(head: &str) -> Option<(bool, String, u16)> {
    let line = head.lines().next()?;
    let mut w = line.split_whitespace();
    let (method, target) = (w.next()?, w.next()?);
    let split = |hp: &str, default: u16| -> Option<(String, u16)> {
        if let Some(r) = hp.strip_prefix('[') {
            let (h, p) = r.split_once(']')?;
            let port = match p.strip_prefix(':') {
                Some(p) => p.parse().ok()?,
                None => default,
            };
            return Some((h.to_string(), port));
        }
        match hp.rsplit_once(':') {
            Some((h, p)) => Some((h.to_string(), p.parse().ok()?)),
            None => Some((hp.to_string(), default)),
        }
    };
    if method.eq_ignore_ascii_case("CONNECT") {
        let (h, p) = split(target, 443)?;
        return Some((true, h, p));
    }
    let rest = target.strip_prefix("http://")?;
    let hp = rest.split(['/', '?']).next()?;
    let (h, p) = split(hp, 80)?;
    Some((false, h, p))
}

fn bridge_conn(mut client: TcpStream, up: &ProxyUrl) {
    let _ = client.set_read_timeout(Some(IO_TIMEOUT));
    let Ok(head) = read_head(&mut client, 64 * 1024) else {
        return;
    };
    let text = String::from_utf8_lossy(&head).into_owned();
    let Some((connect, host, port)) = target_of(&text) else {
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let upstream = dial(&up.host, up.port, CONNECT_TIMEOUT).and_then(|mut s| {
        socks5_connect(&mut s, up, &host, port)?;
        Ok(s)
    });
    let Ok(mut upstream) = upstream else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let sent = if connect {
        client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
    } else {
        upstream.write_all(&head)
    };
    if sent.is_ok() {
        relay(client, upstream);
    }
}

impl Bridge {
    pub fn start(up: ProxyUrl) -> Result<Bridge, String> {
        let l =
            TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("could not listen: {e}"))?;
        let port = l.local_addr().map_err(|e| e.to_string())?.port();
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("route-bridge".into())
            .spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    match l.accept() {
                        Ok((c, _)) => {
                            let _ = c.set_nonblocking(false);
                            let up = up.clone();
                            let _ = std::thread::Builder::new()
                                .name("route-bridge-conn".into())
                                .spawn(move || bridge_conn(c, &up));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(40)),
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Bridge { port, stop })
    }
}
