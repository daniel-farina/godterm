use std::cell::Cell;
use std::io::{Read, Write};
use std::net::TcpStream;

const MAX_HEAD: usize = 16 << 10;

#[derive(Debug, Default, Clone)]
pub struct Request {
    pub method: String,

    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,

    pub keep_alive: bool,
}

thread_local! {

    static KEEP: Cell<bool> = const { Cell::new(false) };
}

pub fn set_keep(keep: bool) {
    KEEP.with(|k| k.set(keep));
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k == name).then_some(v)
        })
    }

    pub fn query_without(&self, name: &str) -> String {
        self.query
            .split('&')
            .filter(|kv| !kv.is_empty() && kv.split('=').next() != Some(name))
            .collect::<Vec<_>>()
            .join("&")
    }

    pub fn wants_upgrade(&self) -> bool {
        self.header("upgrade")
            .is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
    }

    pub fn accepts_gzip(&self) -> bool {
        self.header("accept-encoding")
            .is_some_and(|a| a.split(',').any(|e| e.trim().starts_with("gzip")))
    }

    pub fn https(&self) -> bool {
        self.header("x-forwarded-proto")
            .is_some_and(|p| p.eq_ignore_ascii_case("https"))
            || self
                .header("host")
                .is_some_and(|h| h.split(':').next().unwrap_or("").ends_with(".ts.net"))
    }
}

pub fn parse(head: &str) -> Option<Request> {
    let mut lines = head.split("\r\n");
    let first = lines.next()?;
    let mut parts = first.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if !path.starts_with('/') {
        return None;
    }
    let mut headers = vec![];
    for l in lines {
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let mut req = Request {
        method,
        path: path.to_string(),
        query: query.to_string(),
        headers,
        keep_alive: version != "HTTP/1.0",
    };
    if let Some(c) = req.header("connection") {
        let says = |w: &str| c.split(',').any(|t| t.trim().eq_ignore_ascii_case(w));
        if says("close") {
            req.keep_alive = false;
        } else if says("keep-alive") {
            req.keep_alive = true;
        }
    }
    Some(req)
}

pub fn read_head(s: &mut TcpStream) -> Option<Request> {
    let mut head: Vec<u8> = Vec::with_capacity(1024);
    let mut peek = [0u8; 4096];
    loop {
        let n = s.peek(&mut peek).ok()?;
        if n == 0 {
            return None;
        }

        let from = head.len().saturating_sub(3);
        let mut window = head[from..].to_vec();
        window.extend_from_slice(&peek[..n]);
        if let Some(i) = window.windows(4).position(|w| w == b"\r\n\r\n") {
            let take = from + i + 4 - head.len();
            let mut rest = vec![0u8; take];
            s.read_exact(&mut rest).ok()?;
            head.extend_from_slice(&rest);
            return parse(std::str::from_utf8(&head).ok()?);
        }
        if head.len() + n > MAX_HEAD {
            return None;
        }

        let mut got = vec![0u8; n];
        s.read_exact(&mut got).ok()?;
        head.extend_from_slice(&got);
    }
}

const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

const SECURITY: &str = "X-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nCross-Origin-Opener-Policy: same-origin\r\n";

pub fn respond(
    s: &mut impl Write,
    status: &str,
    headers: &[(&str, String)],
    body: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    let mut h = format!("HTTP/1.1 {status}\r\n{SECURITY}");
    for (k, v) in headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str(&format!(
        "Content-Length: {}\r\nConnection: {}\r\n\r\n",
        body.len(),
        if KEEP.with(Cell::get) {
            "keep-alive"
        } else {
            "close"
        }
    ));
    s.write_all(h.as_bytes())?;
    if !head_only {
        s.write_all(body)?;
    }
    s.flush()
}

pub fn text(s: &mut impl Write, status: &str, msg: &str) -> std::io::Result<()> {
    respond(
        s,
        status,
        &[
            ("Content-Type", "text/plain; charset=utf-8".into()),
            ("Cache-Control", "no-store".into()),
        ],
        msg.as_bytes(),
        false,
    )
}

fn serve_dev(
    s: &mut impl Write,
    req: &Request,
    dir: &str,
    path: &str,
    extra: &[(&str, String)],
) -> std::io::Result<bool> {
    if path.split('/').any(|c| c == ".." || c.starts_with('.')) {
        return Ok(false);
    }
    let Ok(body) = std::fs::read(std::path::Path::new(dir).join(path)) else {
        return Ok(false);
    };
    let mut headers = vec![
        ("Content-Type", super::assets::mime(path).to_string()),
        ("Cache-Control", "no-store".to_string()),
    ];
    headers.extend(extra.iter().cloned());
    if path == "index.html" {
        headers.push(("Content-Security-Policy", CSP.to_string()));
    }
    respond(s, "200 OK", &headers, &body, req.method == "HEAD")?;
    Ok(true)
}

pub fn public(path: &str) -> bool {
    let p = path.trim_start_matches('/');
    matches!(
        p,
        "manifest.webmanifest"
            | "sw.js"
            | "signin.html"
            | "signin.js"
            | "offline.html"
            | "icon.svg"
    ) || (p.starts_with("icon-") && p.ends_with(".png") && !p.contains('/'))
        || (p.starts_with("shots/") && p.ends_with(".png") && !p[6..].contains('/'))
}

pub fn serve_page(
    s: &mut impl Write,
    req: &Request,
    assets: &super::assets::Table,
    path: &str,
    status: &str,
) -> std::io::Result<bool> {
    let body: Vec<u8> = if let Some(dir) = crate::config::env_var("WEB_DIR") {
        match std::fs::read(std::path::Path::new(&dir).join(path)) {
            Ok(b) => b,
            Err(_) => return Ok(false),
        }
    } else {
        match assets.find(path) {
            Some(a) => a.bytes.to_vec(),
            None => return Ok(false),
        }
    };
    let headers = [
        ("Content-Type", super::assets::mime(path).to_string()),
        ("Cache-Control", "no-store".to_string()),
        ("Content-Security-Policy", CSP.to_string()),
    ];
    respond(s, status, &headers, &body, req.method == "HEAD")?;
    Ok(true)
}

pub fn serve_static(
    s: &mut impl Write,
    req: &Request,
    assets: &super::assets::Table,
    extra: &[(&str, String)],
) -> std::io::Result<bool> {
    let path = if req.path == "/" {
        "index.html"
    } else {
        req.path.trim_start_matches('/')
    };
    if let Some(dir) = crate::config::env_var("WEB_DIR") {
        return serve_dev(s, req, &dir, path, extra);
    }
    let Some(a) = assets.find(path) else {
        return Ok(false);
    };
    let etag = format!("\"{}\"", assets.build);
    if req.header("if-none-match") == Some(etag.as_str()) {
        let mut headers = vec![("ETag", etag)];
        headers.extend(extra.iter().cloned());
        respond(s, "304 Not Modified", &headers, &[], req.method == "HEAD")?;
        return Ok(true);
    }

    let cache = if path == "index.html" || req.param("v") != Some(assets.build.as_str()) {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    let mut headers = vec![
        ("Content-Type", a.mime.to_string()),
        ("Cache-Control", cache.to_string()),
        ("ETag", etag),
        ("Vary", "Accept-Encoding".to_string()),
    ];
    headers.extend(extra.iter().cloned());
    if path == "index.html" {
        headers.push(("Content-Security-Policy", CSP.to_string()));
    }
    let body = if !a.gz.is_empty() && req.accepts_gzip() {
        headers.push(("Content-Encoding", "gzip".to_string()));
        a.gz
    } else {
        a.bytes
    };
    respond(s, "200 OK", &headers, body, req.method == "HEAD")?;
    Ok(true)
}
