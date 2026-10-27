use serde_json::Value;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const MAX_BYTES: u64 = 4 << 20;

const MAX_LINES: usize = 200;
const MAX_TEXT: usize = 2000;
const MAX_DEVICE: usize = 80;

const BURST_LINES: f64 = 1000.0;
const RATE_LINES: f64 = 20.0;
const BURST_BYTES: f64 = 256.0 * 1024.0;
const RATE_BYTES: f64 = 16.0 * 1024.0;

const HIDDEN: &[&str] = &["typed", "input", "paste", "clipboard", "audio", "mic"];

pub fn path() -> PathBuf {
    crate::config::app_home().join("web-client.log")
}

fn rotated(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

static SECRET: Mutex<String> = Mutex::new(String::new());

pub fn set_secret(token: &str) {
    *SECRET.lock().unwrap_or_else(|e| e.into_inner()) = token.to_string();
}

pub fn scrub(s: &str, secret: &str) -> String {
    let mut t: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if secret.len() >= 16 {
        t = t.replace(secret, "[token]");
    }
    let t = blobs(&query_t(&headers(&t)));
    cap(&t, MAX_TEXT)
}

fn cap(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_string(),
    }
}

fn headers(s: &str) -> String {
    let low = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let b = s.as_bytes();
    'outer: while i < s.len() {
        for key in ["cookie", "authorization", "godterm_web", "bearer"] {
            if !low[i..].starts_with(key) {
                continue;
            }
            let mut j = i + key.len();
            while j < b.len() && matches!(b[j], b'"' | b'\'' | b' ') {
                j += 1;
            }
            let sep =
                key == "bearer" && j > i + key.len() || j < b.len() && matches!(b[j], b':' | b'=');
            if !sep {
                continue;
            }
            if key != "bearer" {
                j += 1;
            }
            let end = s[j..]
                .find(['"', '\'', ',', '}'])
                .map_or(s.len(), |k| j + k);
            out.push_str(&s[i..j]);
            out.push_str(" [redacted]");
            i = end;
            continue 'outer;
        }
        let c = s[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn query_t(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(k) = rest.find("t=") {
        let before = if k > 0 {
            rest[..k].chars().next_back()
        } else {
            out.chars().next_back()
        };
        out.push_str(&rest[..k + 2]);
        rest = &rest[k + 2..];
        if matches!(before, None | Some('?' | '&' | ';' | ' ' | '"' | '\'')) {
            let end = rest
                .find(|c: char| c == '&' || c == '#' || c == '"' || c == '\'' || c.is_whitespace())
                .unwrap_or(rest.len());
            if end > 0 {
                out.push_str("[redacted]");
            }
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out
}

fn blobs(s: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-');
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        let hex = run.len() >= 32 && run.chars().all(|c| c.is_ascii_hexdigit());
        let b64 = run.len() >= 40
            && run.chars().any(|c| c.is_ascii_digit())
            && run.chars().any(|c| c.is_ascii_uppercase())
            && run.chars().any(|c| c.is_ascii_lowercase());
        if hex {
            out.push_str("[hex]");
        } else if b64 {
            out.push_str(&format!("[blob {} chars]", run.len()));
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in s.chars() {
        if word(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

pub struct Rate {
    lines: f64,
    bytes: f64,
    at: Instant,
    dropped: u64,
}

impl Default for Rate {
    fn default() -> Self {
        Rate {
            lines: BURST_LINES,
            bytes: BURST_BYTES,
            at: Instant::now(),
            dropped: 0,
        }
    }
}

impl Rate {
    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.at = now;
        self.lines = (self.lines + dt * RATE_LINES).min(BURST_LINES);
        self.bytes = (self.bytes + dt * RATE_BYTES).min(BURST_BYTES);
    }

    fn take(&mut self, len: usize) -> bool {
        if self.lines >= 1.0 && self.bytes >= len as f64 {
            self.lines -= 1.0;
            self.bytes -= len as f64;
            true
        } else {
            self.dropped += 1;
            false
        }
    }
}

pub fn lines(client: u64, v: &Value, secret: &str, rate: &mut Rate, now: Instant) -> Vec<String> {
    rate.refill(now);
    let mut device = scrub(v["device"].as_str().unwrap_or("?"), secret);
    device = cap(&device.replace(['[', ']'], ""), MAX_DEVICE);
    let wall = chrono::Local::now();
    let stamp = |at: Option<i64>| {
        use chrono::TimeZone;
        let t = at
            .and_then(|ms| chrono::Local.timestamp_millis_opt(ms).single())
            .filter(|t| (*t - wall).num_hours().abs() < 48)
            .unwrap_or(wall);
        t.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
    };
    let mut out = vec![];
    if rate.dropped > 0 && rate.lines >= 1.0 {
        out.push(format!(
            "{} c{client} [{device}] log: {} lines dropped (rate limit)\n",
            stamp(None),
            rate.dropped
        ));
        rate.dropped = 0;
    }
    for l in v["lines"].as_array().into_iter().flatten().take(MAX_LINES) {
        let cat: String = l["cat"]
            .as_str()
            .unwrap_or("log")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .take(24)
            .collect::<String>()
            .to_ascii_lowercase();
        let cat = if cat.is_empty() { "log".into() } else { cat };
        let raw = match &l["text"] {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            x => x.to_string(),
        };
        let text = if HIDDEN.contains(&cat.as_str()) {
            format!("[{} chars hidden]", raw.chars().count())
        } else {
            scrub(&raw, secret)
        };
        let line = format!(
            "{} c{client} [{device}] {cat}: {text}\n",
            stamp(l["at"].as_i64())
        );
        if rate.take(line.len()) {
            out.push(line);
        }
    }
    out
}

pub struct LogFile {
    path: PathBuf,
    max: u64,
    file: Option<std::fs::File>,
    size: u64,
}

impl LogFile {
    pub fn new(path: PathBuf, max: u64) -> LogFile {
        LogFile {
            path,
            max,
            file: None,
            size: 0,
        }
    }

    fn open(&mut self) -> std::io::Result<&mut std::fs::File> {
        if self.file.is_none() {
            use crate::platform::OpenOptionsExt;
            if let Some(d) = self.path.parent() {
                std::fs::create_dir_all(d)?;
            }
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&self.path)?;
            self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
            self.file = Some(f);
        }
        Ok(self.file.as_mut().expect("opened"))
    }

    pub fn write(&mut self, text: &str) -> std::io::Result<()> {
        if self.file.is_some() && !self.path.exists() {
            self.file = None;
        }
        self.open()?;
        if self.size > 0 && self.size + text.len() as u64 > self.max {
            self.file = None;
            std::fs::rename(&self.path, rotated(&self.path))?;
            self.open()?;
        }
        let f = self.open()?;
        f.write_all(text.as_bytes())?;
        self.size += text.len() as u64;
        Ok(())
    }
}

static TX: OnceLock<SyncSender<String>> = OnceLock::new();
static BEHIND: AtomicU64 = AtomicU64::new(0);

pub fn submit(lines: Vec<String>) {
    if lines.is_empty() {
        return;
    }
    let tx = TX.get_or_init(|| {
        let (tx, rx) = sync_channel::<String>(4096);
        let p = path();
        let _ = std::thread::Builder::new()
            .name("web-log".into())
            .spawn(move || {
                let mut f = LogFile::new(p, MAX_BYTES);
                while let Ok(l) = rx.recv() {
                    let n = BEHIND.swap(0, Ordering::Relaxed);
                    if n > 0 {
                        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                        let _ = f.write(&format!(
                            "{ts} - [server] log: {n} lines dropped (writer behind)\n"
                        ));
                    }
                    if let Err(e) = f.write(&l) {
                        crate::log::error(&format!("web: client log: {e}"));
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            });
        tx
    });
    for l in lines {
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = tx.try_send(l) {
            BEHIND.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub fn take(client: u64, v: &Value, rate: &mut Rate) {
    let secret = SECRET.lock().unwrap_or_else(|e| e.into_inner()).clone();
    submit(lines(client, v, &secret, rate, Instant::now()));
}

fn client_of(line: &str) -> Option<u64> {
    line.split_whitespace()
        .nth(2)
        .and_then(|c| c.strip_prefix('c'))
        .and_then(|n| n.parse().ok())
}

pub fn command(args: &[String]) -> anyhow::Result<()> {
    let val = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    let follow = args.iter().any(|a| a == "--follow" || a == "-f");
    let client = val("--client");
    let last = val("--lines").map(|n| n as usize);
    let p = path();
    let keep = |l: &str| client.is_none() || client_of(l) == client;
    let mut all: Vec<String> = vec![];
    for f in [rotated(&p), p.clone()] {
        if let Ok(s) = std::fs::read_to_string(&f) {
            all.extend(s.lines().filter(|l| keep(l)).map(str::to_string));
        }
    }
    if all.is_empty() && !p.exists() && !follow {
        println!(
            "no web client log yet at {} (a browser writes it once it connects, unless its Debug log is off; --demo reads the demo's)",
            p.display()
        );
        return Ok(());
    }
    let skip = last.map_or(0, |n| all.len().saturating_sub(n));
    let mut out = std::io::stdout().lock();
    for l in &all[skip..] {
        writeln!(out, "{l}")?;
    }
    out.flush()?;
    if !follow {
        return Ok(());
    }

    let mut pos = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    let mut part = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if len < pos {
            pos = 0;
            part.clear();
        }
        if len == pos {
            continue;
        }
        let Ok(mut f) = std::fs::File::open(&p) else {
            continue;
        };
        f.seek(SeekFrom::Start(pos))?;
        let mut buf = vec![];
        Read::by_ref(&mut f).take(len - pos).read_to_end(&mut buf)?;
        pos += buf.len() as u64;
        let mut r = std::io::BufReader::new(&buf[..]);
        loop {
            let mut l = String::new();
            if r.read_line(&mut l)? == 0 {
                break;
            }
            if !l.ends_with('\n') {
                part.push_str(&l);
                break;
            }
            let full = std::mem::take(&mut part) + l.trim_end_matches('\n');
            if keep(&full) {
                writeln!(out, "{full}")?;
            }
        }
        out.flush()?;
    }
}
