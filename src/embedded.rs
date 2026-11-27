use std::collections::{HashMap, VecDeque};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::assistant_images::{base64, unbase64};
use crate::web::tap::Outbox;

pub const KIND_FRAME: u8 = 7;

pub const MAX_PAGES: usize = 6;
pub const MAX_TOTAL: usize = 40;
pub const SLEEP_MINUTES: u64 = 30;

static PAGE_CAP: AtomicUsize = AtomicUsize::new(MAX_TOTAL);
static SLEEP_MS: AtomicU64 = AtomicU64::new(SLEEP_MINUTES * 60_000);
static LINGER_MS: AtomicU64 = AtomicU64::new(180_000);
static NEXT_PAGE: AtomicU32 = AtomicU32::new(0);

pub fn set_page_cap(n: usize) {
    PAGE_CAP.store(n.clamp(MAX_PAGES, 200), Ordering::Relaxed);
}

pub fn page_cap() -> usize {
    PAGE_CAP.load(Ordering::Relaxed)
}

pub fn set_sleep_after(d: Duration) {
    SLEEP_MS.store(d.as_millis() as u64, Ordering::Relaxed);
}

pub fn sleep_after() -> Option<Duration> {
    match SLEEP_MS.load(Ordering::Relaxed) {
        0 => None,
        ms => Some(Duration::from_millis(ms)),
    }
}

#[cfg(test)]
pub fn set_linger(d: Duration) {
    LINGER_MS.store(d.as_millis() as u64, Ordering::Relaxed);
}

fn linger() -> Duration {
    Duration::from_millis(LINGER_MS.load(Ordering::Relaxed))
}

fn now_ms() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

const VIEWER_STALE: Duration = Duration::from_secs(90);
const CALL_TIMEOUT: Duration = Duration::from_secs(20);
const START_TIMEOUT: Duration = Duration::from_secs(20);
const SEND_EVERY: Duration = Duration::from_millis(70);
const READ_TICK: Duration = Duration::from_millis(25);
const LIMBO_MAX: Duration = Duration::from_secs(5);
const WAKE_RETRY: Duration = Duration::from_secs(10);
const RING: usize = 300;
const MAX_TEXT: usize = 15000;
const MAX_URL: usize = 2048;
const KEEP_SHOTS: usize = 12;
const START_W: u32 = 1100;
const START_H: u32 = 760;
const NOT_OWNED: &str = "that page is not one of this tab's pages";
const CRASH_NOTE: &str = "the embedded browser had stopped unexpectedly and was started again: this tab's pages were reloaded, so form input and script state are gone";
const WOKE_NOTE: &str = "this page had been put to sleep to save memory and was reloaded: form input and script state are gone";

fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone)]
pub struct Ctx {
    pub uid: u64,
    pub bin: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    seq: u64,
    kind: String,
    text: String,
    url: String,
    status: i64,
}

struct Viewer {
    client: u64,
    out: Arc<Outbox>,
    keep: Instant,
}

#[derive(Debug, Clone, Default)]
struct Page {
    id: u32,
    target: String,
    sid: String,
    url: String,
    title: String,
    loading: bool,
    back: bool,
    fwd: bool,
    console: VecDeque<Entry>,
    net: VecDeque<Entry>,
    seq: u64,
    loads: u64,
    asleep: bool,
    scroll: (f64, f64),
    used: u64,
    woke: bool,
}

#[derive(Debug, Default)]
struct Removal {
    was_active: bool,
    empty: bool,
}

#[derive(Debug, Default)]
struct Tabs {
    pages: Vec<Page>,
    active: u32,
    joining: Vec<String>,
}

impl Tabs {
    fn can_add(&self) -> bool {
        self.pages.len() + self.joining.len() < MAX_PAGES
    }

    fn known(&self, target: &str) -> bool {
        self.joining.iter().any(|t| t == target) || self.by_target_ref(target).is_some()
    }

    fn begin(&mut self, target: &str) -> bool {
        if self.known(target) || !self.can_add() {
            return false;
        }
        self.joining.push(target.to_string());
        true
    }

    fn abandon(&mut self, target: &str) {
        self.joining.retain(|t| t != target);
    }

    fn push(&mut self, page: Page) -> u32 {
        let id = NEXT_PAGE.fetch_add(1, Ordering::Relaxed) + 1;
        self.pages.push(Page {
            id,
            used: now_ms(),
            ..page
        });
        id
    }

    fn add(&mut self, target: &str, sid: &str, url: &str) -> u32 {
        self.abandon(target);
        self.push(Page {
            target: target.to_string(),
            sid: sid.to_string(),
            url: url.to_string(),
            ..Page::default()
        })
    }

    fn add_asleep(&mut self, url: &str) -> u32 {
        self.push(Page {
            url: url.to_string(),
            asleep: true,
            ..Page::default()
        })
    }

    fn revive(&mut self, id: u32, target: &str, sid: &str) -> bool {
        match self.get_mut(id) {
            Some(p) if p.asleep => {
                p.target = target.to_string();
                p.sid = sid.to_string();
                p.asleep = false;
                p.loading = false;
                p.used = now_ms();
                true
            }
            _ => false,
        }
    }

    fn rest(&mut self, id: u32) -> Option<(String, String)> {
        let p = self.get_mut(id).filter(|p| !p.asleep)?;
        p.asleep = true;
        p.loading = false;
        p.back = false;
        p.fwd = false;
        Some((std::mem::take(&mut p.target), std::mem::take(&mut p.sid)))
    }

    fn get(&self, id: u32) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut Page> {
        self.pages.iter_mut().find(|p| p.id == id)
    }

    fn cur(&self) -> Option<&Page> {
        self.get(self.active)
    }

    fn cur_mut(&mut self) -> Option<&mut Page> {
        self.get_mut(self.active)
    }

    fn by_sid(&mut self, sid: &str) -> Option<&mut Page> {
        self.pages
            .iter_mut()
            .find(|p| !sid.is_empty() && p.sid == sid)
    }

    fn by_target_ref(&self, target: &str) -> Option<&Page> {
        self.pages
            .iter()
            .find(|p| !target.is_empty() && p.target == target)
    }

    fn by_target(&mut self, target: &str) -> Option<&mut Page> {
        self.pages
            .iter_mut()
            .find(|p| !target.is_empty() && p.target == target)
    }

    fn remove_at(&mut self, i: usize) -> Removal {
        let gone = self.pages.remove(i);
        let was_active = gone.id == self.active;
        if was_active {
            let near = if i > 0 { i - 1 } else { 0 };
            self.active = self.pages.get(near).map_or(0, |p| p.id);
        }
        Removal {
            was_active,
            empty: self.pages.is_empty(),
        }
    }

    fn remove_target(&mut self, target: &str) -> Option<Removal> {
        let i = self
            .pages
            .iter()
            .position(|p| !target.is_empty() && p.target == target)?;
        Some(self.remove_at(i))
    }

    fn remove_id(&mut self, id: u32) -> Option<Removal> {
        let i = self.pages.iter().position(|p| p.id == id)?;
        Some(self.remove_at(i))
    }

    fn asleep(&self) -> usize {
        self.pages.iter().filter(|p| p.asleep).count()
    }

    fn list(&self) -> Value {
        Value::Array(
            self.pages
                .iter()
                .map(|p| {
                    let mut v = json!({"id": p.id, "url": p.url, "title": p.title, "active": p.id == self.active});
                    if p.asleep {
                        v["asleep"] = json!(true);
                    }
                    v
                })
                .collect(),
        )
    }
}

struct State {
    tabs: Tabs,
    viewers: Vec<Viewer>,
    frame: Option<Arc<Vec<u8>>>,
    frame_dims: (f64, f64),
    dirty: bool,
    last_push: Instant,
    vp: (u32, u32),
    meta_sent: Option<Value>,
    meta_dirty: bool,
    shots: u64,
    cast_sid: String,
    note: Option<String>,
    wake_failed: Option<Instant>,
}

enum Cmd {
    Send(String),
    Stop,
}

pub struct Session {
    uid: u64,
    bin: Mutex<String>,
    st: Mutex<State>,
    cv: Condvar,
    alive: AtomicBool,
    act: Mutex<()>,
    casting: Mutex<()>,
    waking: Mutex<()>,
}

struct Tag {
    uid: u64,
    sid: String,
}

struct Claim {
    uid: u64,
    page: u32,
}

struct Limbo {
    sid: String,
    at: Instant,
}

enum Key<'a> {
    Sid(&'a str),
    Target(&'a str),
}

#[derive(Default)]
struct Book {
    tags: HashMap<String, Tag>,
    sids: HashMap<String, String>,
    claims: HashMap<String, Claim>,
    limbo: HashMap<String, Limbo>,
    reserved: usize,
}

impl Book {
    fn live(&self) -> usize {
        self.tags.len() + self.reserved
    }

    fn tag(&mut self, target: &str, sid: &str, uid: u64) {
        self.sids.insert(sid.to_string(), target.to_string());
        self.tags.insert(
            target.to_string(),
            Tag {
                uid,
                sid: sid.to_string(),
            },
        );
    }

    fn forget(&mut self, target: &str) {
        if let Some(t) = self.tags.remove(target) {
            self.sids.remove(&t.sid);
        }
        self.claims.remove(target);
        self.limbo.remove(target);
    }

    fn owner_of_target(&self, target: &str) -> Option<u64> {
        self.tags.get(target).map(|t| t.uid)
    }

    fn by_sid(&self, sid: &str) -> Option<(u64, String)> {
        let target = self.sids.get(sid)?;
        Some((self.tags.get(target)?.uid, target.clone()))
    }

    fn owns(&self, uid: u64, key: &Key) -> Result<(), String> {
        let who = match key {
            Key::Sid(s) => self.by_sid(s).map(|x| x.0),
            Key::Target(t) => self.owner_of_target(t),
        };
        if who == Some(uid) {
            Ok(())
        } else {
            Err(NOT_OWNED.into())
        }
    }

    #[cfg(test)]
    fn owned(&self, uid: u64) -> usize {
        self.tags.values().filter(|t| t.uid == uid).count()
    }
}

struct Browser {
    dir: PathBuf,
    pid: u32,
    child: Mutex<Option<Child>>,
    tx: Mutex<mpsc::Sender<Cmd>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
    next_id: AtomicU64,
    alive: AtomicBool,
    book: Mutex<Book>,
    idle: Mutex<Option<Instant>>,
}

#[derive(Default)]
struct Hub {
    owners: HashMap<u64, Arc<Session>>,
    browser: Option<Arc<Browser>>,
}

fn hub() -> &'static Mutex<Hub> {
    static H: std::sync::OnceLock<Mutex<Hub>> = std::sync::OnceLock::new();
    H.get_or_init(|| Mutex::new(Hub::default()))
}

fn owner_lock() -> &'static Mutex<()> {
    static L: Mutex<()> = Mutex::new(());
    &L
}

fn start_lock() -> &'static Mutex<()> {
    static L: Mutex<()> = Mutex::new(());
    &L
}

fn browser() -> Option<Arc<Browser>> {
    lk(hub())
        .browser
        .clone()
        .filter(|b| b.alive.load(Ordering::Relaxed))
}

fn owners() -> Vec<Arc<Session>> {
    lk(hub()).owners.values().cloned().collect()
}

pub fn get(uid: u64) -> Option<Arc<Session>> {
    lk(hub())
        .owners
        .get(&uid)
        .filter(|s| s.alive.load(Ordering::Relaxed))
        .cloned()
}

pub fn live_uids() -> Vec<u64> {
    let mut v: Vec<u64> = lk(hub())
        .owners
        .iter()
        .filter(|(_, s)| s.alive.load(Ordering::Relaxed))
        .map(|(u, _)| *u)
        .collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
pub fn browser_pid() -> Option<u32> {
    browser().map(|b| b.pid)
}

#[cfg(test)]
pub fn live_pages() -> usize {
    browser().map_or(0, |b| lk(&b.book).live())
}

pub fn root_dir() -> PathBuf {
    std::env::temp_dir().join("godterm-embedded")
}

fn is_ip(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>().is_ok()
}

pub fn is_local_host(host: &str) -> bool {
    let h = host.trim_matches(|c| c == '[' || c == ']').to_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h == "::1"
        || h == "0.0.0.0"
        || h.starts_with("127.")
        || (is_ip(&h) && h.starts_with("192.168."))
        || h.ends_with(".local")
}

fn split_scheme(s: &str) -> Option<(&str, &str)> {
    let i = s.find(':')?;
    let (sch, rest) = (&s[..i], &s[i + 1..]);
    if sch.is_empty()
        || !sch.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        || !sch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    {
        return None;
    }
    let port_like = !rest.is_empty()
        && rest
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if port_like {
        return None;
    }
    Some((sch, rest))
}

fn host_of(rest: &str) -> String {
    let after = rest.strip_prefix("//").unwrap_or(rest);
    let auth = after.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or(auth);
    if let Some(r) = auth.strip_prefix('[') {
        return r.split(']').next().unwrap_or("").to_string();
    }
    auth.split(':').next().unwrap_or("").to_string()
}

pub fn check_url(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("no address".into());
    }
    if s.len() > MAX_URL {
        return Err("that address is too long".into());
    }
    if s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("an address has no spaces or control characters".into());
    }
    if s.eq_ignore_ascii_case("about:blank") {
        return Ok("about:blank".into());
    }
    match split_scheme(s) {
        Some((sch, rest)) => {
            let sch = sch.to_lowercase();
            match sch.as_str() {
                "http" | "https" => {
                    if !rest.starts_with("//") || host_of(rest).is_empty() {
                        return Err("that address has no host".into());
                    }
                    Ok(format!("{sch}:{rest}"))
                }
                "file" => Err("file: pages are not opened here: serve the folder (for example `python3 -m http.server` or the project's dev server) and open http://localhost:PORT".into()),
                "chrome" | "chrome-extension" | "chrome-untrusted" | "devtools" | "view-source" | "edge" | "brave" | "about" => Err(format!("{sch}: pages are not opened in the embedded browser")),
                other => Err(format!("{other}: addresses are not opened in the embedded browser (only http and https)")),
            }
        }
        None => {
            let host = host_of(s);
            if host.is_empty() {
                return Err("that address has no host".into());
            }
            let local = is_local_host(&host) || is_ip(&host);
            Ok(format!("{}://{s}", if local { "http" } else { "https" }))
        }
    }
}

pub fn find_chrome(over: &str) -> Option<PathBuf> {
    let over = over.trim();
    if !over.is_empty() {
        let p = PathBuf::from(over);
        if p.is_file() {
            return Some(p);
        }
        return crate::voice::tts::which(over);
    }
    let mut fixed: Vec<PathBuf> = vec![];
    if cfg!(target_os = "macos") {
        for app in [
            "Google Chrome",
            "Chromium",
            "Brave Browser",
            "Microsoft Edge",
            "Google Chrome for Testing",
        ] {
            fixed.push(PathBuf::from(format!(
                "/Applications/{app}.app/Contents/MacOS/{app}"
            )));
            if let Some(h) = dirs::home_dir() {
                fixed.push(h.join(format!("Applications/{app}.app/Contents/MacOS/{app}")));
            }
        }
    }
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(b) = std::env::var_os(var) {
                let b = PathBuf::from(b);
                fixed.push(b.join("Google/Chrome/Application/chrome.exe"));
                fixed.push(b.join("Microsoft/Edge/Application/msedge.exe"));
                fixed.push(b.join("Chromium/Application/chrome.exe"));
            }
        }
    }
    if let Some(p) = fixed.into_iter().find(|p| p.is_file()) {
        return Some(p);
    }
    for n in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "brave-browser",
    ] {
        if let Some(p) = crate::voice::tts::which(n) {
            return Some(p);
        }
    }
    None
}

fn http_get(port: u16, path: &str) -> Result<String, String> {
    use std::io::{Read, Write};
    let mut s = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(2),
    )
    .map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    s.set_write_timeout(Some(Duration::from_secs(3))).ok();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let split = buf.windows(4).position(|w| w == b"\r\n\r\n");
        if let Some(i) = split {
            let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
            let want = head.lines().find_map(|l| {
                l.strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse::<usize>().ok())
            });
            if let Some(n) = want {
                if buf.len() >= i + 4 + n {
                    return Ok(String::from_utf8_lossy(&buf[i + 4..i + 4 + n]).into_owned());
                }
            }
        }
        match s.read(&mut chunk) {
            Ok(0) => {
                return split
                    .map(|i| String::from_utf8_lossy(&buf[i + 4..]).into_owned())
                    .ok_or_else(|| "no answer".to_string())
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(e.to_string()),
        }
        if buf.len() > 4 << 20 {
            return Err("too much".into());
        }
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe {
        libc::kill(pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    unsafe {
        libc::killpg(pid as i32, libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

fn cmd_of(pid: u32) -> String {
    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

pub fn sweep_stale() {
    let Ok(rd) = std::fs::read_dir(root_dir()) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(owner) = std::fs::read_to_string(p.join("owner")) else {
            continue;
        };
        let mut it = owner
            .split_whitespace()
            .filter_map(|x| x.parse::<u32>().ok());
        let (Some(gt), Some(ch)) = (it.next(), it.next()) else {
            continue;
        };
        if gt == std::process::id() || pid_alive(gt) {
            continue;
        }
        if ch != 0 && pid_alive(ch) && cmd_of(ch).contains(&*p.to_string_lossy()) {
            kill_group(ch);
        }
        let _ = std::fs::remove_dir_all(&p);
    }
}

fn spawn_chrome(bin: &Path, dir: &Path) -> Result<(Child, u16), String> {
    let prof = dir.join("profile");
    std::fs::create_dir_all(&prof).map_err(|e| e.to_string())?;
    let mut c = Command::new(bin);
    c.arg("--headless=new")
        .arg("--remote-debugging-port=0")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--user-data-dir={}", prof.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        .arg("--disable-sync")
        .arg("--disable-background-networking")
        .arg("--disable-component-update")
        .arg("--disable-default-apps")
        .arg("--disable-breakpad")
        .arg("--disable-features=Translate,MediaRouter,OptimizationHints")
        .arg("--password-store=basic")
        .arg("--use-mock-keychain")
        .arg("--mute-audio")
        .arg("--hide-scrollbars")
        .arg(format!("--window-size={START_W},{START_H}"))
        .arg("--no-startup-window")
        .arg("--disable-component-extensions-with-background-pages")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut child = c
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
    let file = prof.join("DevToolsActivePort");
    let t0 = Instant::now();
    loop {
        if let Some(port) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| s.lines().next().and_then(|l| l.trim().parse::<u16>().ok()))
        {
            return Ok((child, port));
        }
        if let Ok(Some(st)) = child.try_wait() {
            return Err(format!("the browser exited at start ({st})"));
        }
        if t0.elapsed() > START_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the browser did not open its debugging port in time".into());
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

fn browser_ws_url(port: u16) -> Result<String, String> {
    let t0 = Instant::now();
    loop {
        if let Ok(body) = http_get(port, "/json/version") {
            if let Some(u) = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["webSocketDebuggerUrl"].as_str().map(str::to_string))
            {
                return Ok(u);
            }
        }
        if t0.elapsed() > Duration::from_secs(8) {
            return Err("the browser did not offer a debugging socket".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn url_allowed(url: &str) -> bool {
    url.is_empty()
        || url == "about:blank"
        || url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("chrome-error://")
}

fn connect_ws(url: &str, port: u16) -> Result<tungstenite::WebSocket<TcpStream>, String> {
    use tungstenite::client::IntoClientRequest;
    let req = url.into_client_request().map_err(|e| e.to_string())?;
    let s = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(3),
    )
    .map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    s.set_write_timeout(Some(Duration::from_secs(10))).ok();
    s.set_nodelay(true).ok();
    let (ws, _) = tungstenite::client(req, s).map_err(|e| e.to_string())?;
    ws.get_ref().set_read_timeout(Some(READ_TICK)).ok();
    Ok(ws)
}

enum How {
    Wake(u64, u32),
    Popup(u64),
    Full(u64),
    Park,
}

impl Browser {
    fn send_raw(&self, v: String) -> bool {
        lk(&self.tx).send(Cmd::Send(v)).is_ok()
    }

    fn start_call(
        &self,
        sid: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<(u64, mpsc::Receiver<Value>), String> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err("the embedded browser is closed".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lk(&self.pending).insert(id, tx);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(sid) = sid {
            msg["sessionId"] = json!(sid);
        }
        if !self.send_raw(msg.to_string()) {
            lk(&self.pending).remove(&id);
            return Err("the embedded browser is closed".into());
        }
        Ok((id, rx))
    }

    fn finish_call(
        &self,
        id: u64,
        rx: mpsc::Receiver<Value>,
        method: &str,
        wait: Duration,
    ) -> Result<Value, String> {
        match rx.recv_timeout(wait) {
            Ok(v) => match v.get("error") {
                Some(e) => Err(e["message"]
                    .as_str()
                    .unwrap_or("the browser refused")
                    .to_string()),
                None => Ok(v["result"].clone()),
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("the embedded browser stopped".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lk(&self.pending).remove(&id);
                Err(format!("{method} timed out"))
            }
        }
    }

    fn call(
        &self,
        sid: Option<&str>,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, String> {
        let (id, rx) = self.start_call(sid, method, params)?;
        self.finish_call(id, rx, method, wait)
    }

    fn fire(&self, sid: Option<&str>, method: &str, params: Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(sid) = sid {
            msg["sessionId"] = json!(sid);
        }
        self.send_raw(msg.to_string());
    }

    fn owns(&self, uid: u64, key: &Key) -> Result<(), String> {
        lk(&self.book).owns(uid, key)
    }

    fn call_for(
        &self,
        uid: u64,
        sid: &str,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, String> {
        self.owns(uid, &Key::Sid(sid))?;
        self.call(Some(sid), method, params, wait)
    }

    fn target_for(&self, uid: u64, method: &str, target: &str) -> Result<Value, String> {
        self.owns(uid, &Key::Target(target))?;
        self.call(None, method, json!({"targetId": target}), CALL_TIMEOUT)
    }

    fn close_for(&self, uid: u64, target: &str) -> bool {
        if self.owns(uid, &Key::Target(target)).is_err() {
            return false;
        }
        self.fire(None, "Target.closeTarget", json!({"targetId": target}));
        lk(&self.book).forget(target);
        true
    }

    fn refuse(&self, target: &str, sid: &str) {
        self.fire(Some(sid), "Runtime.runIfWaitingForDebugger", json!({}));
        self.fire(None, "Target.closeTarget", json!({"targetId": target}));
        lk(&self.book).forget(target);
    }

    fn context_for(&self, _uid: u64) -> Option<String> {
        None
    }

    fn create_for(self: &Arc<Self>, uid: u64, page: u32) -> Result<(), String> {
        {
            let mut k = lk(&self.book);
            if k.live() >= page_cap() {
                return Err(format!(
                    "the embedded browser already has {} pages awake across all tabs, the limit in GodTerm's settings (Web app, Embedded browser pages); close a page tab, or wait for idle ones to go to sleep",
                    k.live()
                ));
            }
            k.reserved += 1;
        }
        let mut params = json!({"url": "about:blank", "newWindow": true});
        if let Some(c) = self.context_for(uid) {
            params["browserContextId"] = json!(c);
        }
        let r = self.call(None, "Target.createTarget", params, CALL_TIMEOUT);
        let mut k = lk(&self.book);
        k.reserved = k.reserved.saturating_sub(1);
        let target = r?["targetId"]
            .as_str()
            .ok_or("the browser did not open a page")?
            .to_string();
        match k.limbo.remove(&target) {
            Some(l) => {
                k.tag(&target, &l.sid, uid);
                drop(k);
                adopt(self, How::Wake(uid, page), target, l.sid, String::new());
            }
            None => {
                k.claims.insert(target, Claim { uid, page });
            }
        }
        Ok(())
    }

    fn exited(&self) -> bool {
        match lk(&self.child).as_mut() {
            Some(c) => !matches!(c.try_wait(), Ok(None)),
            None => true,
        }
    }

    fn stop(&self, crashed: bool) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = lk(&self.tx).send(Cmd::Stop);
        if let Some(mut c) = lk(&self.child).take() {
            kill_group(self.pid);
            let _ = c.kill();
            let _ = c.wait();
        }
        lk(&self.pending).clear();
        {
            let mut h = lk(hub());
            if h.browser.as_ref().is_some_and(|b| b.pid == self.pid) {
                h.browser = None;
            }
        }
        {
            let mut k = lk(&self.book);
            *k = Book::default();
        }
        clean_dir(self.dir.clone());
        for s in owners() {
            s.browser_gone(crashed);
        }
        crate::log::info(if crashed {
            "embedded: the browser stopped unexpectedly"
        } else {
            "embedded: the browser was closed"
        });
    }
}

fn clean_dir(dir: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("embedded-clean".into())
        .spawn(move || {
            for _ in 0..5 {
                if std::fs::remove_dir_all(&dir).is_ok() || !dir.exists() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
}

fn shots_root() -> PathBuf {
    root_dir().join(format!("p{}-shots", std::process::id()))
}

fn shots_dir(uid: u64) -> PathBuf {
    shots_root().join(format!("t{uid}"))
}

impl Session {
    fn push(&self, sid: &str, kind: &str, text: String, url: String, status: i64, net: bool) {
        let mut g = lk(&self.st);
        let t = &mut g.tabs;
        let page = if sid.is_empty() {
            t.cur_mut()
        } else {
            t.by_sid(sid)
        };
        let Some(p) = page else {
            return;
        };
        log_to(p, kind, text, url, status, net);
    }

    fn active_sid(&self) -> Result<String, String> {
        match lk(&self.st).tabs.cur() {
            Some(p) if !p.asleep => Ok(p.sid.clone()),
            Some(_) => Err("this page is asleep: any action on it wakes it".into()),
            None => Err("the embedded browser has no page".into()),
        }
    }

    pub fn call(&self, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
        let sid = self.active_sid()?;
        self.call_on(&sid, method, params, wait)
    }

    fn call_on(
        &self,
        sid: &str,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, String> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err("the embedded browser is closed".into());
        }
        let b = browser().ok_or("the embedded browser is not running")?;
        b.call_for(self.uid, sid, method, params, wait)
    }

    fn target_op(&self, method: &str, target: &str) -> Result<Value, String> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err("the embedded browser is closed".into());
        }
        let b = browser().ok_or("the embedded browser is not running")?;
        b.target_for(self.uid, method, target)
    }

    fn touch(&self) {
        let mut g = lk(&self.st);
        self.touch_locked(&mut g);
    }

    fn touch_locked(&self, g: &mut State) {
        if let Some(p) = g.tabs.cur_mut() {
            p.used = now_ms();
        }
    }

    fn loads(&self) -> u64 {
        lk(&self.st).tabs.cur().map_or(0, |p| p.loads)
    }

    fn is_loading(&self) -> bool {
        lk(&self.st).tabs.cur().is_some_and(|p| p.loading)
    }

    pub fn eval(&self, expr: &str) -> Result<Value, String> {
        let r = self.call(
            "Runtime.evaluate",
            json!({"expression": expr, "returnByValue": true, "awaitPromise": true, "userGesture": true}),
            CALL_TIMEOUT,
        )?;
        if let Some(d) = r.get("exceptionDetails") {
            let t = d["exception"]["description"]
                .as_str()
                .or_else(|| d["text"].as_str())
                .unwrap_or("the script failed");
            return Err(t.lines().next().unwrap_or(t).to_string());
        }
        Ok(r["result"]["value"].clone())
    }

    fn emulate(&self, sid: &str, w: u32, h: u32) -> Result<(), String> {
        let dsf = if w < 600 { 2 } else { 1 };
        self.call_on(
            sid,
            "Emulation.setDeviceMetricsOverride",
            json!({"width": w, "height": h, "deviceScaleFactor": dsf, "mobile": false}),
            CALL_TIMEOUT,
        )?;
        Ok(())
    }

    fn cast(&self, force: bool) {
        let _c = lk(&self.casting);
        let (want, have, (w, h)) = {
            let g = lk(&self.st);
            let want = if g.viewers.is_empty() {
                String::new()
            } else {
                g.tabs
                    .cur()
                    .filter(|p| !p.asleep)
                    .map(|p| p.sid.clone())
                    .unwrap_or_default()
            };
            (want, g.cast_sid.clone(), g.vp)
        };
        if want == have && (!force || want.is_empty()) {
            return;
        }
        if !have.is_empty() {
            let _ = self.call_on(&have, "Page.stopScreencast", json!({}), CALL_TIMEOUT);
        }
        let mut now = String::new();
        if !want.is_empty() {
            let dsf = if w < 600 { 2 } else { 1 };
            let ok = self
                .call_on(
                    &want,
                    "Page.startScreencast",
                    json!({"format": "jpeg", "quality": 72, "maxWidth": (w * dsf).min(2000), "maxHeight": (h * dsf).min(2000), "everyNthFrame": 1}),
                    CALL_TIMEOUT,
                )
                .is_ok();
            if ok {
                now = want;
            }
        }
        lk(&self.st).cast_sid = now;
    }

    fn set_viewport(&self, w: u32, h: u32) -> Result<(), String> {
        let w = w.clamp(320, 2400);
        let h = h.clamp(240, 2400);
        let active = self.active_sid()?;
        self.emulate(&active, w, h)?;
        let others: Vec<String> = {
            let mut g = lk(&self.st);
            g.vp = (w, h);
            g.tabs
                .pages
                .iter()
                .filter(|p| !p.asleep && p.sid != active)
                .map(|p| p.sid.clone())
                .collect()
        };
        for sid in others {
            let _ = self.emulate(&sid, w, h);
        }
        self.cast(true);
        Ok(())
    }

    pub fn viewport(&self) -> (u32, u32) {
        lk(&self.st).vp
    }

    pub fn navigate(&self, url: &str) -> Result<Value, String> {
        let url = check_url(url)?;
        let before = self.loads();
        let r = self.call("Page.navigate", json!({"url": url}), CALL_TIMEOUT)?;
        if let Some(e) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            self.mark_meta();
            return Err(format!("{url} did not load: {e}"));
        }
        let loaded = self.wait_load(before, Duration::from_secs(15));
        self.mark_meta();
        let (u, t) = self.refresh_meta();
        Ok(json!({"url": u, "title": t, "loaded": loaded}))
    }

    fn wait_load(&self, before: u64, max: Duration) -> bool {
        let id = lk(&self.st).tabs.active;
        self.wait_page(id, before, max)
    }

    fn wait_page(&self, id: u32, before: u64, max: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < max {
            {
                let g = lk(&self.st);
                if g.tabs
                    .get(id)
                    .is_some_and(|p| p.loads > before && !p.loading)
                {
                    return true;
                }
            }
            if !self.alive.load(Ordering::Relaxed) || browser().is_none() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        false
    }

    fn settle(&self, max: Duration) {
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(180));
        while t0.elapsed() < max && self.is_loading() {
            std::thread::sleep(Duration::from_millis(60));
        }
        std::thread::sleep(Duration::from_millis(60));
    }

    fn mark_meta(&self) {
        lk(&self.st).meta_dirty = true;
        self.cv.notify_all();
    }

    fn refresh_meta(&self) -> (String, String) {
        let stored = |s: &Session| {
            let g = lk(&s.st);
            g.tabs.cur().map_or((String::new(), String::new()), |p| {
                (p.url.clone(), p.title.clone())
            })
        };
        let Ok(sid) = self.active_sid() else {
            return stored(self);
        };
        if let Ok(h) = self.call_on(
            &sid,
            "Page.getNavigationHistory",
            json!({}),
            Duration::from_secs(4),
        ) {
            let idx = h["currentIndex"].as_i64().unwrap_or(0);
            let n = h["entries"].as_array().map_or(0, |a| a.len()) as i64;
            let cur = h["entries"]
                .as_array()
                .and_then(|a| a.get(idx.max(0) as usize))
                .cloned()
                .unwrap_or(Value::Null);
            let live_title = self
                .call_on(
                    &sid,
                    "Runtime.evaluate",
                    json!({"expression": "document.title", "returnByValue": true}),
                    Duration::from_secs(2),
                )
                .ok()
                .and_then(|r| r["result"]["value"].as_str().map(str::to_string));
            let mut g = lk(&self.st);
            if let Some(p) = g.tabs.by_sid(&sid) {
                p.url = cur["url"].as_str().unwrap_or("").to_string();
                p.title = live_title
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| cur["title"].as_str().unwrap_or("").to_string());
                p.back = idx > 0;
                p.fwd = idx + 1 < n;
            }
        }
        stored(self)
    }

    fn meta_json(&self) -> Value {
        let g = lk(&self.st);
        let (url, title, loading, back, fwd) = g
            .tabs
            .cur()
            .map_or((String::new(), String::new(), false, false, false), |p| {
                (p.url.clone(), p.title.clone(), p.loading, p.back, p.fwd)
            });
        json!({"t": "ebv", "op": "meta", "url": url, "title": title, "loading": loading, "back": back, "fwd": fwd, "w": g.vp.0, "h": g.vp.1, "tabs": g.tabs.list(), "active": g.tabs.active})
    }

    fn tell_viewers(&self, v: &Value) {
        let g = lk(&self.st);
        for w in &g.viewers {
            let mut m = v.clone();
            m["tab"] = json!(self.uid);
            w.out.text(m.to_string());
        }
    }

    fn flush_frame(&self, force: bool) {
        let mut g = lk(&self.st);
        if !g.dirty || g.viewers.is_empty() {
            return;
        }
        if !force && g.last_push.elapsed() < SEND_EVERY {
            return;
        }
        let Some(f) = g.frame.clone() else {
            return;
        };
        g.viewers.retain(|v| !v.out.is_closed());
        let mut all = true;
        for v in &g.viewers {
            if !v
                .out
                .picture(crate::web::ws::frame(KIND_FRAME, self.uid, &f))
            {
                all = false;
            }
        }
        g.last_push = Instant::now();
        if all {
            g.dirty = false;
        }
    }

    fn browser_gone(&self, crashed: bool) {
        let mut g = lk(&self.st);
        let mut lost = 0;
        for p in g.tabs.pages.iter_mut().filter(|p| !p.asleep) {
            p.asleep = true;
            p.loading = false;
            p.back = false;
            p.fwd = false;
            p.target.clear();
            p.sid.clear();
            lost += 1;
            if crashed {
                log_to(
                    p,
                    "browser",
                    "the embedded browser stopped unexpectedly: this page is reloaded on its next use"
                        .into(),
                    String::new(),
                    0,
                    false,
                );
            }
        }
        g.tabs.joining.clear();
        g.cast_sid.clear();
        g.frame = None;
        g.dirty = false;
        g.meta_sent = None;
        g.meta_dirty = true;
        if crashed && lost > 0 {
            g.note = Some(CRASH_NOTE.into());
        }
        drop(g);
        self.cv.notify_all();
    }

    fn end(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        let targets: Vec<String> = {
            let mut g = lk(&self.st);
            let uid = self.uid;
            for w in g.viewers.drain(..) {
                w.out
                    .text(json!({"t": "ebv", "tab": uid, "op": "ended"}).to_string());
            }
            g.tabs.joining.clear();
            g.tabs
                .pages
                .drain(..)
                .filter(|p| !p.asleep)
                .map(|p| p.target)
                .collect()
        };
        if let Some(b) = browser() {
            for t in &targets {
                b.close_for(self.uid, t);
            }
            let mut k = lk(&b.book);
            let uid = self.uid;
            k.claims.retain(|_, c| c.uid != uid);
        }
        self.cv.notify_all();
        clean_dir(shots_dir(self.uid));
    }

    fn sleep_page(&self, id: u32) -> bool {
        let Some(b) = browser() else {
            return false;
        };
        let Some(sid) = lk(&self.st)
            .tabs
            .get(id)
            .filter(|p| !p.asleep)
            .map(|p| p.sid.clone())
        else {
            return false;
        };
        let facts = self
            .call_on(
                &sid,
                "Runtime.evaluate",
                json!({"expression": "({x: scrollX, y: scrollY, u: location.href, t: document.title})", "returnByValue": true}),
                Duration::from_secs(3),
            )
            .ok()
            .map(|r| r["result"]["value"].clone());
        let target = {
            let mut g = lk(&self.st);
            let active = g.tabs.active == id;
            let Some((target, was)) = g.tabs.rest(id) else {
                return false;
            };
            if let (Some(v), Some(p)) = (facts, g.tabs.get_mut(id)) {
                p.scroll = (
                    v["x"].as_f64().unwrap_or(0.0),
                    v["y"].as_f64().unwrap_or(0.0),
                );
                if let Some(u) = v["u"]
                    .as_str()
                    .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                {
                    p.url = u.to_string();
                    if let Some(t) = v["t"].as_str().filter(|t| !t.is_empty()) {
                        p.title = t.to_string();
                    }
                }
            }
            if g.cast_sid == was {
                g.cast_sid.clear();
            }
            if active {
                g.frame = None;
                g.dirty = false;
            }
            target
        };
        b.close_for(self.uid, &target);
        self.mark_meta();
        crate::log::info(&format!(
            "embedded: page {id} of tab {} went to sleep",
            self.uid
        ));
        true
    }

    fn sleep_due(&self, window: u64) {
        let now = now_ms();
        let (others, active) = {
            let mut g = lk(&self.st);
            g.viewers
                .retain(|v| v.keep.elapsed() < VIEWER_STALE && !v.out.is_closed());
            let viewing = !g.viewers.is_empty();
            let a = g.tabs.active;
            let due = |p: &Page| !p.asleep && !p.loading && now.saturating_sub(p.used) > window;
            let others: Vec<u32> = g
                .tabs
                .pages
                .iter()
                .filter(|p| p.id != a && due(p))
                .map(|p| p.id)
                .collect();
            let awake = g
                .tabs
                .pages
                .iter()
                .filter(|p| p.id != a && !p.asleep)
                .count();
            let active = g
                .tabs
                .cur()
                .filter(|p| due(p) && !viewing && awake == others.len())
                .map(|p| p.id);
            (others, active)
        };
        for id in others {
            self.sleep_page(id);
        }
        if let Some(id) = active {
            if let Ok(_busy) = self.act.try_lock() {
                self.sleep_page(id);
            }
        }
    }

    fn wake(&self, id: u32) -> Result<bool, String> {
        let _w = lk(&self.waking);
        let (url, scroll) = {
            let g = lk(&self.st);
            let p = g.tabs.get(id).ok_or("there is no tab with that id")?;
            if !p.asleep {
                return Ok(false);
            }
            (p.url.clone(), p.scroll)
        };
        if !self.alive.load(Ordering::Relaxed) {
            return Err("the embedded browser is closed".into());
        }
        let bin = lk(&self.bin).clone();
        let mut made = Err("the embedded browser did not start".to_string());
        for _ in 0..2 {
            let b = ensure_browser(&bin)?;
            made = b.create_for(self.uid, id);
            if made.is_ok() || b.alive.load(Ordering::Relaxed) {
                break;
            }
        }
        made?;
        let t0 = Instant::now();
        let sid = loop {
            let ready = lk(&self.st)
                .tabs
                .get(id)
                .filter(|p| !p.asleep)
                .map(|p| p.sid.clone());
            if let Some(sid) = ready {
                break sid;
            }
            if t0.elapsed() > Duration::from_secs(10) || !self.alive.load(Ordering::Relaxed) {
                return Err("the page did not become ready".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let real = url.starts_with("http://") || url.starts_with("https://");
        if real {
            let before = lk(&self.st).tabs.get(id).map_or(0, |p| p.loads);
            let _ = self.call_on(&sid, "Page.navigate", json!({"url": url}), CALL_TIMEOUT);
            self.wait_page(id, before, Duration::from_secs(15));
            if scroll != (0.0, 0.0) {
                let _ = self.call_on(
                    &sid,
                    "Runtime.evaluate",
                    json!({"expression": format!("window.scrollTo({}, {})", scroll.0, scroll.1)}),
                    Duration::from_secs(3),
                );
            }
            crate::log::info(&format!("embedded: page {id} of tab {} woke", self.uid));
        }
        if let Some(p) = lk(&self.st).tabs.get_mut(id) {
            p.used = now_ms();
            p.woke = real;
        }
        self.mark_meta();
        Ok(true)
    }

    fn show(&self, id: u32) -> Result<(), String> {
        self.wake(id)?;
        let (sid, target) = {
            let g = lk(&self.st);
            let p = g.tabs.get(id).ok_or("there is no tab with that id")?;
            (p.sid.clone(), p.target.clone())
        };
        let _ = self.target_op("Target.activateTarget", &target);
        let (w, h) = self.viewport();
        let _ = self.emulate(&sid, w, h);
        self.cast(false);
        self.mark_meta();
        Ok(())
    }

    fn ready(&self) -> Result<(), String> {
        let (id, asleep) = lk(&self.st)
            .tabs
            .cur()
            .map(|p| (p.id, p.asleep))
            .ok_or("the embedded browser has no page")?;
        if asleep {
            self.show(id)?;
        }
        Ok(())
    }

    pub fn activate(&self, id: u32) -> Result<(), String> {
        {
            let mut g = lk(&self.st);
            if g.tabs.get(id).is_none() {
                return Err("there is no tab with that id".into());
            }
            let now = now_ms();
            if let Some(p) = g.tabs.cur_mut() {
                p.used = now;
            }
            g.tabs.active = id;
            if let Some(p) = g.tabs.cur_mut() {
                p.used = now;
            }
            g.frame = None;
            g.dirty = false;
            g.meta_sent = None;
        }
        self.show(id)
    }

    pub fn tabs_json(&self) -> Value {
        lk(&self.st).tabs.list()
    }

    fn settle_active(&self) {
        let (id, asleep, blank, viewing) = {
            let g = lk(&self.st);
            let Some(p) = g.tabs.cur() else {
                return;
            };
            (
                p.id,
                p.asleep,
                p.url.is_empty() || p.url == "about:blank",
                !g.viewers.is_empty(),
            )
        };
        if !asleep || viewing || (blank && browser().is_some()) {
            let _ = self.show(id);
        } else {
            self.mark_meta();
        }
    }
}

fn log_to(p: &mut Page, kind: &str, text: String, url: String, status: i64, net: bool) {
    p.seq += 1;
    let e = Entry {
        seq: p.seq,
        kind: kind.to_string(),
        text,
        url,
        status,
    };
    let ring = if net { &mut p.net } else { &mut p.console };
    ring.push_back(e);
    while ring.len() > RING {
        ring.pop_front();
    }
}

fn reply_to(
    b: &Browser,
    ws: &mut tungstenite::WebSocket<TcpStream>,
    sid: &str,
    method: &str,
    params: Value,
) {
    let id = b.next_id.fetch_add(1, Ordering::Relaxed);
    let mut c = json!({"id": id, "method": method, "params": params});
    if !sid.is_empty() {
        c["sessionId"] = json!(sid);
    }
    let _ = ws.send(tungstenite::Message::text(c.to_string()));
}

fn adopt(b: &Arc<Browser>, how: How, target: String, sid: String, url: String) {
    if matches!(how, How::Park) {
        return;
    }
    let b = Arc::clone(b);
    let _ = std::thread::Builder::new()
        .name("embedded-join".into())
        .spawn(move || match how {
            How::Park => {}
            How::Wake(uid, page) => match get(uid) {
                Some(s) => {
                    let _ = join_page(&s, &b, &target, &sid, &url, Some(page));
                }
                None => b.refuse(&target, &sid),
            },
            How::Popup(uid) => {
                let Some(s) = get(uid) else {
                    return b.refuse(&target, &sid);
                };
                if lk(&s.st).tabs.begin(&target) {
                    let _ = join_page(&s, &b, &target, &sid, &url, None);
                } else {
                    b.refuse(&target, &sid);
                    s.push(
                        "",
                        "blocked",
                        format!("a new tab was refused: at most {MAX_PAGES} tabs"),
                        String::new(),
                        0,
                        false,
                    );
                }
            }
            How::Full(uid) => {
                b.refuse(&target, &sid);
                if let Some(s) = get(uid) {
                    s.push(
                        "",
                        "blocked",
                        format!(
                            "a new tab was refused: the embedded browser already has {} pages awake across all tabs",
                            page_cap()
                        ),
                        String::new(),
                        0,
                        false,
                    );
                }
            }
        });
}

fn target_event(b: &Arc<Browser>, m: &str, p: &Value) {
    match m {
        "Target.attachedToTarget" => {
            let info = &p["targetInfo"];
            if info["type"] != "page" {
                return;
            }
            let (Some(sid), Some(target)) = (p["sessionId"].as_str(), info["targetId"].as_str())
            else {
                return;
            };
            let opener = info["openerId"].as_str().unwrap_or("");
            let how = {
                let mut k = lk(&b.book);
                if k.tags.contains_key(target) {
                    return;
                }
                if let Some(c) = k.claims.remove(target) {
                    k.tag(target, sid, c.uid);
                    How::Wake(c.uid, c.page)
                } else if let Some(uid) = k.owner_of_target(opener) {
                    if k.live() < page_cap() {
                        k.tag(target, sid, uid);
                        How::Popup(uid)
                    } else {
                        How::Full(uid)
                    }
                } else {
                    k.limbo.insert(
                        target.to_string(),
                        Limbo {
                            sid: sid.to_string(),
                            at: Instant::now(),
                        },
                    );
                    How::Park
                }
            };
            adopt(
                b,
                how,
                target.to_string(),
                sid.to_string(),
                info["url"].as_str().unwrap_or("").to_string(),
            );
        }
        "Target.detachedFromTarget" => {
            let sid = p["sessionId"].as_str().unwrap_or("");
            let hit = {
                let mut k = lk(&b.book);
                let hit = k.by_sid(sid);
                if let Some((_, t)) = &hit {
                    k.forget(t);
                }
                hit
            };
            if let Some((uid, t)) = hit {
                if let Some(s) = get(uid) {
                    page_gone(&s, &t);
                }
            }
        }
        "Target.targetDestroyed" => {
            let Some(t) = p["targetId"].as_str() else {
                return;
            };
            let uid = {
                let mut k = lk(&b.book);
                let uid = k.owner_of_target(t);
                k.forget(t);
                uid
            };
            if let Some(s) = uid.and_then(get) {
                page_gone(&s, t);
            }
        }
        "Target.targetInfoChanged" => {
            let info = &p["targetInfo"];
            let Some(t) = info["targetId"].as_str() else {
                return;
            };
            let Some(s) = lk(&b.book).owner_of_target(t).and_then(get) else {
                return;
            };
            let mut g = lk(&s.st);
            let active = g.tabs.active;
            if let Some(pg) = g.tabs.by_target(t) {
                if pg.id != active {
                    pg.url = info["url"].as_str().unwrap_or("").to_string();
                    pg.title = info["title"].as_str().unwrap_or("").to_string();
                }
            }
            drop(g);
            s.mark_meta();
        }
        _ => {}
    }
}

fn handle_event(b: &Arc<Browser>, ws: &mut tungstenite::WebSocket<TcpStream>, v: &Value) {
    let Some(m) = v["method"].as_str() else {
        return;
    };
    let p = &v["params"];
    if m.starts_with("Target.") {
        return target_event(b, m, p);
    }
    let sid = v["sessionId"].as_str().unwrap_or("");
    if sid.is_empty() {
        return;
    }
    let Some(s) = lk(&b.book).by_sid(sid).and_then(|(uid, _)| get(uid)) else {
        if m == "Page.screencastFrame" {
            let fid = p["sessionId"].as_i64().unwrap_or(0);
            reply_to(
                b,
                ws,
                sid,
                "Page.screencastFrameAck",
                json!({"sessionId": fid}),
            );
        }
        return;
    };
    let s = &s;
    match m {
        "Page.screencastFrame" => {
            let fid = p["sessionId"].as_i64().unwrap_or(0);
            reply_to(
                b,
                ws,
                sid,
                "Page.screencastFrameAck",
                json!({"sessionId": fid}),
            );
            let is_active = lk(&s.st).tabs.cur().is_some_and(|x| x.sid == sid);
            if !is_active {
                return;
            }
            if let Some(bytes) = p["data"].as_str().and_then(unbase64) {
                let mut g = lk(&s.st);
                g.frame = Some(Arc::new(bytes));
                g.dirty = true;
                let dw = p["metadata"]["deviceWidth"].as_f64().unwrap_or(0.0);
                let dh = p["metadata"]["deviceHeight"].as_f64().unwrap_or(0.0);
                if dw > 0.0 && dh > 0.0 {
                    g.frame_dims = (dw, dh);
                }
            }
        }
        "Runtime.consoleAPICalled" => {
            let level = p["type"].as_str().unwrap_or("log");
            let text: Vec<String> = p["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| {
                            x["value"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| x["value"].as_f64().map(|n| n.to_string()))
                                .or_else(|| x["description"].as_str().map(str::to_string))
                                .unwrap_or_else(|| x["type"].as_str().unwrap_or("").to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            let url = p["stackTrace"]["callFrames"][0]["url"]
                .as_str()
                .unwrap_or("")
                .to_string();
            s.push(sid, level, text.join(" "), url, 0, false);
        }
        "Runtime.exceptionThrown" => {
            let d = &p["exceptionDetails"];
            let t = d["exception"]["description"]
                .as_str()
                .or_else(|| d["text"].as_str())
                .unwrap_or("uncaught exception");
            s.push(
                sid,
                "exception",
                t.lines().next().unwrap_or(t).to_string(),
                d["url"].as_str().unwrap_or("").to_string(),
                0,
                false,
            );
        }
        "Log.entryAdded" => {
            let e = &p["entry"];
            s.push(
                sid,
                e["level"].as_str().unwrap_or("info"),
                e["text"].as_str().unwrap_or("").to_string(),
                e["url"].as_str().unwrap_or("").to_string(),
                0,
                false,
            );
        }
        "Network.responseReceived" => {
            let r = &p["response"];
            s.push(
                sid,
                p["type"].as_str().unwrap_or(""),
                String::new(),
                r["url"].as_str().unwrap_or("").to_string(),
                r["status"].as_i64().unwrap_or(0),
                true,
            );
        }
        "Network.loadingFailed" => {
            if p["canceled"].as_bool() != Some(true) {
                s.push(
                    sid,
                    p["type"].as_str().unwrap_or(""),
                    p["errorText"].as_str().unwrap_or("failed").to_string(),
                    String::new(),
                    -1,
                    true,
                );
            }
        }
        "Page.frameStartedLoading" => {
            if let Some(pg) = lk(&s.st).tabs.by_sid(sid) {
                pg.loading = true;
            }
            s.mark_meta();
        }
        "Page.frameStoppedLoading" => {
            if let Some(pg) = lk(&s.st).tabs.by_sid(sid) {
                pg.loading = false;
            }
            s.mark_meta();
        }
        "Page.loadEventFired" => {
            if let Some(pg) = lk(&s.st).tabs.by_sid(sid) {
                pg.loads += 1;
                pg.loading = false;
            }
            s.mark_meta();
        }
        "Page.navigatedWithinDocument" => s.mark_meta(),
        "Page.frameNavigated" => {
            let f = &p["frame"];
            if f.get("parentId").is_none() {
                let url = f["url"].as_str().unwrap_or("");
                if !url_allowed(url) {
                    s.push(
                        sid,
                        "blocked",
                        format!(
                            "navigation to {} was blocked",
                            url.chars().take(120).collect::<String>()
                        ),
                        url.to_string(),
                        0,
                        false,
                    );
                    reply_to(b, ws, sid, "Page.navigate", json!({"url": "about:blank"}));
                }
                s.mark_meta();
            }
        }
        "Page.javascriptDialogOpening" => {
            let ty = p["type"].as_str().unwrap_or("alert");
            s.push(
                sid,
                "dialog",
                format!("{ty}: {}", p["message"].as_str().unwrap_or("")),
                String::new(),
                0,
                false,
            );
            let accept = matches!(ty, "alert" | "beforeunload");
            reply_to(
                b,
                ws,
                sid,
                "Page.handleJavaScriptDialog",
                json!({"accept": accept}),
            );
        }
        _ => {}
    }
}

fn join_page(
    s: &Arc<Session>,
    b: &Arc<Browser>,
    target: &str,
    sid: &str,
    url: &str,
    wake: Option<u32>,
) -> Result<u32, String> {
    let setup = (|| -> Result<(), String> {
        b.owns(s.uid, &Key::Sid(sid))?;
        if !s.alive.load(Ordering::Relaxed) {
            return Err("the embedded browser is closed".into());
        }
        let (w, h) = lk(&s.st).vp;
        let dsf = if w < 600 { 2 } else { 1 };
        let mut sent = vec![];
        for (m, p) in [
            ("Page.enable", json!({})),
            ("Runtime.enable", json!({})),
            ("Network.enable", json!({})),
            ("Log.enable", json!({})),
            ("Page.setDownloadBehavior", json!({"behavior": "deny"})),
            (
                "Emulation.setDeviceMetricsOverride",
                json!({"width": w, "height": h, "deviceScaleFactor": dsf, "mobile": false}),
            ),
            (
                "Emulation.setFocusEmulationEnabled",
                json!({"enabled": true}),
            ),
            ("Runtime.runIfWaitingForDebugger", json!({})),
        ] {
            let (id, rx) = b.start_call(Some(sid), m, p)?;
            sent.push((m, id, rx));
        }
        let mut first = Ok(());
        for (m, id, rx) in sent {
            let r = b.finish_call(id, rx, m, CALL_TIMEOUT);
            if first.is_ok()
                && !matches!(
                    m,
                    "Page.setDownloadBehavior"
                        | "Emulation.setFocusEmulationEnabled"
                        | "Runtime.runIfWaitingForDebugger"
                )
            {
                first = r.map(|_| ());
            }
        }
        first
    })();
    let joined = setup.and_then(|_| match wake {
        Some(id) => {
            if lk(&s.st).tabs.revive(id, target, sid) {
                Ok(id)
            } else {
                Err("that tab is gone".to_string())
            }
        }
        None => Ok(lk(&s.st).tabs.add(target, sid, url)),
    });
    match joined {
        Ok(id) => {
            if wake.is_none() {
                let _ = s.activate(id);
            }
            Ok(id)
        }
        Err(e) => {
            lk(&s.st).tabs.abandon(target);
            b.fire(None, "Target.closeTarget", json!({"targetId": target}));
            lk(&b.book).forget(target);
            Err(e)
        }
    }
}

fn page_gone(s: &Arc<Session>, target: &str) {
    let r = {
        let mut g = lk(&s.st);
        let r = g.tabs.remove_target(target);
        if r.as_ref().is_some_and(|r| r.empty) {
            let id = g.tabs.add_asleep("about:blank");
            g.tabs.active = id;
        }
        r
    };
    s.mark_meta();
    let Some(r) = r else {
        return;
    };
    if !s.alive.load(Ordering::Relaxed) || !(r.was_active || r.empty) {
        return;
    }
    let s2 = Arc::clone(s);
    let _ = std::thread::Builder::new()
        .name("embedded-tabs".into())
        .spawn(move || s2.settle_active());
}

pub fn open_tab(s: &Arc<Session>, url: Option<&str>) -> Result<u32, String> {
    let url = match url.filter(|u| !u.trim().is_empty()) {
        Some(u) => Some(check_url(u)?),
        None => None,
    };
    let (id, before) = {
        let mut g = lk(&s.st);
        if !g.tabs.can_add() {
            return Err(format!(
                "this browser already has {MAX_PAGES} tabs: close one first"
            ));
        }
        let before = g.tabs.active;
        (g.tabs.add_asleep("about:blank"), before)
    };
    if let Err(e) = s.activate(id) {
        let mut g = lk(&s.st);
        g.tabs.remove_id(id);
        if g.tabs.get(before).is_some() {
            g.tabs.active = before;
        }
        drop(g);
        s.mark_meta();
        return Err(e);
    }
    if let Some(u) = url {
        s.navigate(&u)?;
    }
    Ok(id)
}

pub fn close_tab(s: &Arc<Session>, id: u32) -> Result<(), String> {
    let (target, r) = {
        let mut g = lk(&s.st);
        let target = g
            .tabs
            .get(id)
            .ok_or("there is no tab with that id")?
            .target
            .clone();
        if g.tabs.pages.len() == 1 {
            g.tabs.add_asleep("about:blank");
        }
        let r = g.tabs.remove_id(id).unwrap_or_default();
        (target, r)
    };
    if !target.is_empty() {
        if let Some(b) = browser() {
            b.close_for(s.uid, &target);
        }
    }
    if r.was_active {
        s.settle_active();
    } else {
        s.mark_meta();
    }
    Ok(())
}

fn io_loop(b: Arc<Browser>, mut ws: tungstenite::WebSocket<TcpStream>, rx: mpsc::Receiver<Cmd>) {
    'run: loop {
        if !b.alive.load(Ordering::Relaxed) {
            break;
        }
        while let Ok(c) = rx.try_recv() {
            match c {
                Cmd::Send(t) => {
                    if ws.send(tungstenite::Message::text(t)).is_err() {
                        break 'run;
                    }
                }
                Cmd::Stop => break 'run,
            }
        }
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => {
                let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else {
                    continue;
                };
                if let Some(id) = v["id"].as_u64() {
                    if let Some(tx) = lk(&b.pending).remove(&id) {
                        let _ = tx.send(v);
                    }
                } else {
                    handle_event(&b, &mut ws, &v);
                }
            }
            Ok(tungstenite::Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
        for s in owners() {
            s.flush_frame(false);
        }
    }
    let _ = ws.get_ref().shutdown(Shutdown::Both);
    b.stop(true);
}

fn meta_loop(s: Arc<Session>) {
    let mut last = Instant::now() - Duration::from_secs(10);
    while s.alive.load(Ordering::Relaxed) {
        {
            let g = lk(&s.st);
            let _ =
                s.cv.wait_timeout(g, Duration::from_millis(600))
                    .unwrap_or_else(|e| e.into_inner());
        }
        let (dirty, viewing, wake) = {
            let mut g = lk(&s.st);
            let d = g.meta_dirty;
            g.meta_dirty = false;
            g.viewers
                .retain(|v| v.keep.elapsed() < VIEWER_STALE && !v.out.is_closed());
            let viewing = !g.viewers.is_empty();
            let wake = g
                .tabs
                .cur()
                .filter(|p| {
                    viewing && p.asleep && g.wake_failed.is_none_or(|t| t.elapsed() > WAKE_RETRY)
                })
                .map(|p| p.id);
            (d, viewing, wake)
        };
        if let Some(id) = wake {
            let r = s.show(id);
            lk(&s.st).wake_failed = r.is_err().then(Instant::now);
        }
        s.cast(false);
        if dirty || (viewing && last.elapsed() > Duration::from_millis(1500)) {
            last = Instant::now();
            s.refresh_meta();
            let m = s.meta_json();
            let changed = lk(&s.st).meta_sent.as_ref() != Some(&m);
            if changed {
                lk(&s.st).meta_sent = Some(m.clone());
                s.tell_viewers(&m);
            }
        }
    }
}

fn new_state() -> State {
    State {
        tabs: Tabs::default(),
        viewers: vec![],
        frame: None,
        frame_dims: (0.0, 0.0),
        dirty: false,
        last_push: Instant::now() - Duration::from_secs(1),
        vp: (START_W, START_H),
        meta_sent: None,
        meta_dirty: true,
        shots: 0,
        cast_sid: String::new(),
        note: None,
        wake_failed: None,
    }
}

fn start_browser(bin: &str) -> Result<Arc<Browser>, String> {
    let bin = find_chrome(bin).ok_or(
        "no Chrome, Chromium, Edge or Brave was found for the embedded browser (set web.embedded_browser_bin)",
    )?;
    sweep_stale();
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = root_dir().join(format!(
        "p{}-b{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (child, port) = match spawn_chrome(&bin, &dir) {
        Ok(x) => x,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    let pid = child.id();
    let _ = std::fs::write(
        dir.join("owner"),
        format!("{} {}\n", std::process::id(), pid),
    );
    let (tx, rx) = mpsc::channel();
    let b = Arc::new(Browser {
        dir: dir.clone(),
        pid,
        child: Mutex::new(Some(child)),
        tx: Mutex::new(tx),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        alive: AtomicBool::new(true),
        book: Mutex::new(Book::default()),
        idle: Mutex::new(None),
    });
    let ws = match browser_ws_url(port).and_then(|u| connect_ws(&u, port)) {
        Ok(w) => w,
        Err(e) => {
            b.stop(false);
            return Err(e);
        }
    };
    let b2 = Arc::clone(&b);
    if let Err(e) = std::thread::Builder::new()
        .name("embedded-io".into())
        .spawn(move || io_loop(b2, ws, rx))
    {
        b.stop(false);
        return Err(e.to_string());
    }
    let setup = (|| -> Result<(), String> {
        b.call(
            None,
            "Target.setDiscoverTargets",
            json!({"discover": true}),
            CALL_TIMEOUT,
        )?;
        b.call(
            None,
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true, "filter": [{"type": "page"}, {"exclude": true}]}),
            CALL_TIMEOUT,
        )?;
        let t = b.call(None, "Target.getTargets", json!({}), CALL_TIMEOUT)?;
        for x in t["targetInfos"].as_array().into_iter().flatten() {
            if let (true, Some(id)) = (x["type"] == "page", x["targetId"].as_str()) {
                b.fire(None, "Target.closeTarget", json!({"targetId": id}));
            }
        }
        Ok(())
    })();
    if let Err(e) = setup {
        b.stop(false);
        return Err(e);
    }
    crate::log::info("embedded: started the shared browser");
    Ok(b)
}

fn ensure_browser(bin: &str) -> Result<Arc<Browser>, String> {
    if let Some(b) = browser() {
        return Ok(b);
    }
    let _g = lk(start_lock());
    if let Some(b) = browser() {
        return Ok(b);
    }
    let b = start_browser(bin)?;
    lk(hub()).browser = Some(Arc::clone(&b));
    keeper();
    Ok(b)
}

fn keeper() {
    static ON: AtomicBool = AtomicBool::new(false);
    if cfg!(test) || ON.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("embedded-keeper".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(2));
            housekeep();
        });
}

pub fn housekeep() {
    let Some(b) = lk(hub()).browser.clone() else {
        return;
    };
    if b.exited() {
        b.stop(true);
        return;
    }
    if let Some(w) = sleep_after() {
        for s in owners() {
            s.sleep_due(w.as_millis() as u64);
        }
    }
    let stale: Vec<(String, String)> = lk(&b.book)
        .limbo
        .iter()
        .filter(|(_, l)| l.at.elapsed() > LIMBO_MAX)
        .map(|(t, l)| (t.clone(), l.sid.clone()))
        .collect();
    for (t, sid) in stale {
        b.refuse(&t, &sid);
    }
    let quiet = {
        let k = lk(&b.book);
        k.live() == 0 && k.claims.is_empty() && k.limbo.is_empty()
    };
    let done = {
        let mut i = lk(&b.idle);
        if quiet {
            i.get_or_insert_with(Instant::now).elapsed() >= linger()
        } else {
            *i = None;
            false
        }
    };
    if done {
        b.stop(false);
    }
}

pub fn ensure(ctx: &Ctx) -> Result<Arc<Session>, String> {
    if let Some(s) = get(ctx.uid) {
        *lk(&s.bin) = ctx.bin.clone();
        s.touch();
        return Ok(s);
    }
    let _g = lk(owner_lock());
    if let Some(s) = get(ctx.uid) {
        return Ok(s);
    }
    let s = Arc::new(Session {
        uid: ctx.uid,
        bin: Mutex::new(ctx.bin.clone()),
        st: Mutex::new(new_state()),
        cv: Condvar::new(),
        alive: AtomicBool::new(true),
        act: Mutex::new(()),
        casting: Mutex::new(()),
        waking: Mutex::new(()),
    });
    let id = {
        let mut g = lk(&s.st);
        let id = g.tabs.add_asleep("about:blank");
        g.tabs.active = id;
        id
    };
    lk(hub()).owners.insert(ctx.uid, Arc::clone(&s));
    if let Err(e) = s.show(id) {
        close(ctx.uid);
        return Err(e);
    }
    let s2 = Arc::clone(&s);
    if let Err(e) = std::thread::Builder::new()
        .name("embedded-meta".into())
        .spawn(move || meta_loop(s2))
    {
        close(ctx.uid);
        return Err(e.to_string());
    }
    crate::log::info(&format!("embedded: tab {} has pages now", ctx.uid));
    Ok(s)
}

pub fn close(uid: u64) -> bool {
    let s = lk(hub()).owners.remove(&uid);
    match s {
        Some(s) => {
            s.end();
            true
        }
        None => false,
    }
}

pub fn shutdown_all() {
    let (all, b) = {
        let mut h = lk(hub());
        let all: Vec<Arc<Session>> = h.owners.drain().map(|(_, s)| s).collect();
        (all, h.browser.take())
    };
    for s in all {
        s.end();
    }
    if let Some(b) = b {
        b.stop(false);
    }
    let _ = std::fs::remove_dir_all(shots_root());
}

pub fn reap(live_tabs: &[u64]) {
    for uid in live_uids() {
        if !live_tabs.contains(&uid) {
            crate::log::info(&format!("embedded: closing the pages of tab {uid}"));
            close(uid);
        }
    }
}

pub fn drop_client(client: u64) {
    for s in owners() {
        lk(&s.st).viewers.retain(|v| v.client != client);
    }
}

fn vk(key: &str) -> i64 {
    match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Escape" => 27,
        " " => 32,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Insert" => 45,
        "Delete" => 46,
        k if k.len() == 1 => {
            let c = k.chars().next().unwrap_or(' ');
            if c.is_ascii_alphabetic() {
                c.to_ascii_uppercase() as i64
            } else if c.is_ascii_digit() {
                c as i64
            } else {
                0
            }
        }
        k if k.starts_with('F') && k[1..].parse::<i64>().is_ok_and(|n| (1..=12).contains(&n)) => {
            111 + k[1..].parse::<i64>().unwrap_or(1)
        }
        _ => 0,
    }
}

pub fn norm_key(k: &str) -> String {
    match k.to_lowercase().as_str() {
        "enter" | "return" => "Enter".into(),
        "esc" | "escape" => "Escape".into(),
        "tab" => "Tab".into(),
        "space" | "spacebar" => " ".into(),
        "backspace" => "Backspace".into(),
        "delete" | "del" => "Delete".into(),
        "up" | "arrowup" => "ArrowUp".into(),
        "down" | "arrowdown" => "ArrowDown".into(),
        "left" | "arrowleft" => "ArrowLeft".into(),
        "right" | "arrowright" => "ArrowRight".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        "insert" => "Insert".into(),
        l if l.len() > 1 && l.starts_with('f') && l[1..].parse::<u32>().is_ok() => {
            format!("F{}", &l[1..])
        }
        _ => k.to_string(),
    }
}

pub fn parse_combo(spec: &str) -> (String, i64) {
    let parts: Vec<&str> = if spec == "+" {
        vec!["+"]
    } else {
        spec.split('+').collect()
    };
    let mut mods = 0;
    let mut key = String::new();
    for (i, p) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        match p.to_lowercase().as_str() {
            "alt" | "option" if !last => mods |= 1,
            "ctrl" | "control" if !last => mods |= 2,
            "meta" | "cmd" | "command" | "super" if !last => mods |= 4,
            "shift" if !last => mods |= 8,
            _ => key = norm_key(p),
        }
    }
    (key, mods)
}

pub fn key_events(key: &str, code: &str, mods: i64, text: Option<&str>) -> Vec<Value> {
    let vkc = vk(key);
    let printable = key.chars().count() == 1 && (mods & 6) == 0;
    let mut down = json!({"type": "rawKeyDown", "key": key, "code": code, "modifiers": mods, "windowsVirtualKeyCode": vkc, "nativeVirtualKeyCode": vkc});
    if printable {
        let t = text.unwrap_or(key);
        down["type"] = json!("keyDown");
        down["text"] = json!(t);
        down["unmodifiedText"] = json!(t);
    } else if key == "Enter" {
        down["type"] = json!("keyDown");
        down["text"] = json!("\r");
        down["unmodifiedText"] = json!("\r");
    } else if (mods & 6) != 0 && key.chars().count() == 1 {
        let cmd = match key.to_lowercase().as_str() {
            "a" => Some("selectAll"),
            "c" => Some("copy"),
            "x" => Some("cut"),
            "v" => Some("paste"),
            "z" if mods & 8 != 0 => Some("redo"),
            "z" => Some("undo"),
            "y" => Some("redo"),
            _ => None,
        };
        if let Some(c) = cmd {
            down["commands"] = json!([c]);
        }
    }
    let up = json!({"type": "keyUp", "key": key, "code": code, "modifiers": mods, "windowsVirtualKeyCode": vkc, "nativeVirtualKeyCode": vkc});
    vec![down, up]
}

impl Session {
    pub fn press(
        &self,
        key: &str,
        code: &str,
        mods: i64,
        text: Option<&str>,
    ) -> Result<(), String> {
        for e in key_events(key, code, mods, text) {
            self.call("Input.dispatchKeyEvent", e, CALL_TIMEOUT)?;
        }
        Ok(())
    }

    pub fn mouse(
        &self,
        kind: &str,
        x: f64,
        y: f64,
        button: &str,
        cc: i64,
        dx: f64,
        dy: f64,
    ) -> Result<(), String> {
        let ty = match kind {
            "down" => "mousePressed",
            "up" => "mouseReleased",
            "wheel" => "mouseWheel",
            _ => "mouseMoved",
        };
        let b = match button {
            "right" => "right",
            "middle" => "middle",
            "none" => "none",
            _ => "left",
        };
        let held = match b {
            "left" => 1,
            "right" => 2,
            "middle" => 4,
            _ => 0,
        };
        let buttons = if ty == "mouseReleased" || b == "none" {
            0
        } else {
            held
        };
        let mut p =
            json!({"type": ty, "x": x, "y": y, "button": b, "buttons": buttons, "clickCount": cc});
        if ty == "mouseWheel" {
            p["deltaX"] = json!(dx);
            p["deltaY"] = json!(dy);
        }
        self.call("Input.dispatchMouseEvent", p, CALL_TIMEOUT)?;
        Ok(())
    }

    pub fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        self.mouse("move", x, y, "none", 0, 0.0, 0.0)?;
        self.mouse("down", x, y, "left", 1, 0.0, 0.0)?;
        self.mouse("up", x, y, "left", 1, 0.0, 0.0)
    }

    pub fn insert_text(&self, t: &str) -> Result<(), String> {
        self.call("Input.insertText", json!({"text": t}), CALL_TIMEOUT)?;
        Ok(())
    }

    fn history(&self, delta: i64) -> Result<(), String> {
        let h = self.call("Page.getNavigationHistory", json!({}), CALL_TIMEOUT)?;
        let idx = h["currentIndex"].as_i64().unwrap_or(0) + delta;
        let id = h["entries"]
            .as_array()
            .and_then(|a| a.get(idx.max(0) as usize))
            .and_then(|e| e["id"].as_i64())
            .filter(|_| idx >= 0)
            .ok_or("there is nothing in that direction")?;
        self.call(
            "Page.navigateToHistoryEntry",
            json!({"entryId": id}),
            CALL_TIMEOUT,
        )?;
        self.mark_meta();
        Ok(())
    }

    fn reload(&self) -> Result<(), String> {
        self.call("Page.reload", json!({}), CALL_TIMEOUT)?;
        self.mark_meta();
        Ok(())
    }
}

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

const SNAPSHOT_JS: &str = r#"(() => {
  const sel = 'a[href],button,input,select,textarea,summary,[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=switch],[onclick],[contenteditable=""],[contenteditable=true]';
  const els = [], out = [];
  for (const el of document.querySelectorAll(sel)) {
    const r = el.getBoundingClientRect(), cs = getComputedStyle(el);
    if (r.width < 1 || r.height < 1 || cs.visibility === 'hidden' || cs.display === 'none') continue;
    if (el.type === 'hidden') continue;
    const label = (el.getAttribute('aria-label') || el.innerText || el.value || el.placeholder || el.alt || el.title || el.name || '').trim().replace(/\s+/g, ' ').slice(0, 80);
    els.push(el);
    out.push({ ref: els.length, tag: el.tagName.toLowerCase(), type: el.type || el.getAttribute('role') || '', label, href: el.href || undefined, disabled: !!el.disabled || undefined, x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2), visible: r.bottom > 0 && r.top < innerHeight && r.right > 0 && r.left < innerWidth });
    if (out.length >= 150) break;
  }
  window.__gtRefs = els;
  return out;
})()"#;

fn find_js(args: &Value) -> Result<String, String> {
    if let Some(n) = args["ref"].as_u64() {
        return Ok(format!(
            "(window.__gtRefs && window.__gtRefs[{}])",
            n.saturating_sub(1)
        ));
    }
    if let Some(s) = args["selector"].as_str().filter(|s| !s.trim().is_empty()) {
        return Ok(format!("document.querySelector({})", js_str(s)));
    }
    if let Some(t) = args["text"].as_str().filter(|_| args["action"] == "click") {
        return Ok(format!(
            "(()=>{{const t={}.toLowerCase();const c=[...document.querySelectorAll('a,button,[role=button],[role=link],[role=tab],[role=menuitem],input[type=submit],input[type=button],summary,label')].filter(e=>{{const r=e.getBoundingClientRect();return r.width>0&&r.height>0&&((e.innerText||e.value||e.getAttribute('aria-label')||'').trim().toLowerCase().includes(t))}});c.sort((a,b)=>(a.innerText||a.value||'').length-(b.innerText||b.value||'').length);return c[0]||null}})()",
            js_str(t)
        ));
    }
    Err("name what to use: ref (from snapshot), selector or text".into())
}

impl Session {
    fn locate(&self, args: &Value) -> Result<(f64, f64, String), String> {
        let f = find_js(args)?;
        let js = format!(
            "(()=>{{const el={f};if(!el)return null;el.scrollIntoView({{block:'center',inline:'center'}});const r=el.getBoundingClientRect();const x=r.left+r.width/2,y=r.top+r.height/2;const top=document.elementFromPoint(x,y);const covered=top&&top!==el&&!el.contains(top)&&!top.contains(el)?(top.tagName.toLowerCase()+(top.id?'#'+top.id:'')):null;return {{x,y,w:r.width,h:r.height,covered,tag:el.tagName.toLowerCase()}}}})()"
        );
        let v = self.eval(&js)?;
        if v.is_null() {
            return Err(
                "nothing on the page matches that (take a snapshot to see what is there)".into(),
            );
        }
        let x = v["x"].as_f64().unwrap_or(0.0);
        let y = v["y"].as_f64().unwrap_or(0.0);
        let cover = v["covered"].as_str().unwrap_or("").to_string();
        Ok((x, y, cover))
    }

    pub fn screenshot(&self, full: bool, png: bool) -> Result<(PathBuf, Vec<u8>, Value), String> {
        let mut p = json!({"format": if png { "png" } else { "jpeg" }});
        if !png {
            p["quality"] = json!(72);
        }
        if full {
            let m = self.call("Page.getLayoutMetrics", json!({}), CALL_TIMEOUT)?;
            let c = &m["cssContentSize"];
            let w = c["width"].as_f64().unwrap_or(1000.0);
            let h = c["height"].as_f64().unwrap_or(800.0).min(8000.0);
            p["captureBeyondViewport"] = json!(true);
            p["clip"] = json!({"x": 0, "y": 0, "width": w, "height": h, "scale": 1});
        }
        let r = self.call("Page.captureScreenshot", p, Duration::from_secs(30))?;
        let bytes = r["data"]
            .as_str()
            .and_then(unbase64)
            .ok_or("the browser sent no picture")?;
        let n = {
            let mut g = lk(&self.st);
            g.shots += 1;
            g.shots
        };
        let d = shots_dir(self.uid);
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        let mark = shots_root().join("owner");
        if !mark.exists() {
            let _ = std::fs::write(mark, format!("{} 0\n", std::process::id()));
        }
        let path = d.join(format!("shot-{n}.{}", if png { "png" } else { "jpg" }));
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        if n as usize > KEEP_SHOTS {
            for ext in ["png", "jpg"] {
                let _ =
                    std::fs::remove_file(d.join(format!("shot-{}.{ext}", n as usize - KEEP_SHOTS)));
            }
        }
        let (w, h) = crate::browser_view::image_size(&bytes).unwrap_or((0, 0));
        Ok((path, bytes, json!({"width": w, "height": h})))
    }

    fn page_info(&self) -> Value {
        let (u, t) = self.refresh_meta();
        let g = lk(&self.st);
        let (loading, back, fwd) = g
            .tabs
            .cur()
            .map_or((false, false, false), |p| (p.loading, p.back, p.fwd));
        let mut v = json!({"url": u, "title": t, "tab": g.tabs.active, "tab_count": g.tabs.pages.len(), "owned_pages": g.tabs.pages.len(), "asleep_pages": g.tabs.asleep(), "loading": loading, "can_back": back, "can_forward": fwd, "viewport": {"width": g.vp.0, "height": g.vp.1}});
        if g.tabs.cur().is_some_and(|p| p.asleep) {
            v["asleep"] = json!(true);
        }
        v
    }
}

fn entries(
    ring: &VecDeque<Entry>,
    since: u64,
    limit: usize,
    keep: impl Fn(&Entry) -> bool,
) -> (Vec<Value>, u64) {
    let mut out = vec![];
    let mut last = since;
    let mut cut = false;
    for e in ring.iter().filter(|e| e.seq > since && keep(e)) {
        if out.len() >= limit {
            cut = true;
            break;
        }
        last = e.seq;
        out.push(json!({"seq": e.seq, "kind": e.kind, "text": e.text.chars().take(800).collect::<String>(), "url": e.url, "status": if e.status != 0 { json!(e.status) } else { Value::Null }}));
    }
    if !cut {
        last = ring.back().map_or(last, |e| e.seq.max(last));
    }
    (out, last)
}

fn trunc(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    (s.chars().take(max).collect(), true)
}

pub fn run(ctx: &Ctx, args: &Value) -> Value {
    match run_inner(ctx, args) {
        Ok(v) => v,
        Err(e) => json!({"ok": false, "error": e}),
    }
}

fn run_inner(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let action = args["action"]
        .as_str()
        .unwrap_or("status")
        .trim()
        .to_lowercase();
    match action.as_str() {
        "status" => {
            return Ok(match get(ctx.uid) {
                Some(s) => {
                    let mut v = s.page_info();
                    v["ok"] = json!(true);
                    v["open"] = json!(true);
                    v
                }
                None => {
                    json!({"ok": true, "open": false, "owned_pages": 0, "say": "no embedded browser is open for this tab; navigate opens one"})
                }
            });
        }
        "tabs" => {
            return Ok(match get(ctx.uid) {
                Some(s) => {
                    let g = lk(&s.st);
                    json!({"ok": true, "open": true, "tabs": g.tabs.list(), "active": g.tabs.active})
                }
                None => {
                    json!({"ok": true, "open": false, "tabs": [], "say": "no embedded browser is open for this tab; navigate opens one"})
                }
            });
        }
        "close" => {
            let was = close(ctx.uid);
            return Ok(json!({"ok": true, "closed": was}));
        }
        _ => {}
    }
    if action == "new_tab" {
        if let Some(u) = args["url"].as_str().filter(|u| !u.trim().is_empty()) {
            check_url(u)?;
        }
    }
    if matches!(action.as_str(), "open" | "navigate" | "goto") {
        if let Some(u) = args["url"].as_str().filter(|u| !u.trim().is_empty()) {
            check_url(u)?;
        }
    }
    let s = match action.as_str() {
        "open" | "navigate" | "goto" | "new_tab" => ensure(ctx)?,
        _ => get(ctx.uid)
            .ok_or("no embedded browser is open for this tab: use action open or navigate first")?,
    };
    let _g = lk(&s.act);
    s.touch();
    if !matches!(
        action.as_str(),
        "new_tab" | "switch_tab" | "close_tab" | "console" | "network"
    ) {
        s.ready()?;
    }
    let timeout = Duration::from_millis(
        args["timeout_ms"]
            .as_u64()
            .unwrap_or(10000)
            .clamp(200, 60000),
    );
    let mut out = match action.as_str() {
        "open" | "navigate" | "goto" => {
            let w = args["width"].as_u64();
            let h = args["height"].as_u64();
            if let Some(dev) = args["device"].as_str() {
                let (w, h) = match dev {
                    "phone" | "mobile" => (390, 844),
                    "tablet" => (820, 1180),
                    _ => (START_W, START_H),
                };
                s.set_viewport(w, h)?;
            } else if let (Some(w), Some(h)) = (w, h) {
                s.set_viewport(w as u32, h as u32)?;
            }
            match args["url"].as_str().filter(|u| !u.trim().is_empty()) {
                Some(u) => s.navigate(u)?,
                None if action == "open" => json!({"say": "opened a blank page"}),
                None => return Err("navigate needs a url".into()),
            }
        }
        "new_tab" => {
            let id = open_tab(&s, args["url"].as_str())?;
            json!({"opened_tab": id, "tabs": s.tabs_json()})
        }
        "switch_tab" => {
            let id = tab_arg(args)?;
            s.activate(id)?;
            json!({"tabs": s.tabs_json()})
        }
        "close_tab" => {
            let id = tab_arg(args)?;
            close_tab(&s, id)?;
            json!({"closed_tab": id, "tabs": s.tabs_json()})
        }
        "back" => {
            let before = s.loads();
            s.history(-1)?;
            s.wait_load(before, Duration::from_secs(8));
            json!({})
        }
        "forward" => {
            let before = s.loads();
            s.history(1)?;
            s.wait_load(before, Duration::from_secs(8));
            json!({})
        }
        "reload" => {
            let before = s.loads();
            s.reload()?;
            json!({"loaded": s.wait_load(before, Duration::from_secs(15))})
        }
        "screenshot" => {
            let full = args["full_page"].as_bool().unwrap_or(false);
            let png = args["format"].as_str() == Some("png");
            let (path, bytes, dims) = s.screenshot(full, png)?;
            json!({"path": path, "bytes": bytes.len(), "size": dims, "image_b64": base64(&bytes), "image_type": if png { "image/png" } else { "image/jpeg" }})
        }
        "snapshot" => {
            let v = s.eval(SNAPSHOT_JS)?;
            json!({"elements": v, "say": "click or type with ref N; refs last until the next snapshot or navigation"})
        }
        "text" | "read" => {
            let max = args["max_chars"].as_u64().unwrap_or(MAX_TEXT as u64).clamp(200, 60000) as usize;
            let js = match args["selector"].as_str().filter(|x| !x.is_empty()) {
                Some(sel) => format!("(()=>{{const e=document.querySelector({});return e?e.innerText:null}})()", js_str(sel)),
                None => "document.body ? document.body.innerText : ''".to_string(),
            };
            let v = s.eval(&js)?;
            let t = v.as_str().ok_or("nothing matches that selector")?;
            let (t, cut) = trunc(t, max);
            json!({"text": t, "truncated": cut})
        }
        "html" => {
            let max = args["max_chars"].as_u64().unwrap_or(MAX_TEXT as u64).clamp(200, 120000) as usize;
            let js = match args["selector"].as_str().filter(|x| !x.is_empty()) {
                Some(sel) => format!("(()=>{{const e=document.querySelector({});return e?e.outerHTML:null}})()", js_str(sel)),
                None => "document.documentElement.outerHTML".to_string(),
            };
            let v = s.eval(&js)?;
            let t = v.as_str().ok_or("nothing matches that selector")?;
            let (t, cut) = trunc(t, max);
            json!({"html": t, "truncated": cut})
        }
        "eval" | "evaluate" => {
            let e = args["expression"].as_str().ok_or("eval needs an expression")?;
            let v = s.eval(e)?;
            let t = serde_json::to_string(&v).unwrap_or_default();
            let (t, cut) = trunc(&t, 10000);
            json!({"result": if cut { Value::String(t) } else { v }, "truncated": cut})
        }
        "click" => {
            let before = s.loads();
            let (x, y, covered) = if args["x"].is_number() && args["y"].is_number() {
                (args["x"].as_f64().unwrap_or(0.0), args["y"].as_f64().unwrap_or(0.0), String::new())
            } else {
                s.locate(args)?
            };
            s.click_at(x, y)?;
            s.settle(Duration::from_secs(4));
            let navigated = s.loads() > before;
            let mut v = json!({"clicked": {"x": x.round(), "y": y.round()}, "navigated": navigated});
            if !covered.is_empty() {
                v["warning"] = json!(format!("another element ({covered}) was on top at that point, so the click may have hit it"));
            }
            v
        }
        "type" | "fill" => {
            let t = args["text"].as_str().ok_or("type needs text")?;
            let target = args["ref"].is_number() || args["selector"].as_str().is_some_and(|x| !x.is_empty());
            if target {
                let f = find_js(args)?;
                let clear = args["clear"].as_bool().unwrap_or(true);
                let js = format!(
                    "(()=>{{const el={f};if(!el)return false;el.scrollIntoView({{block:'center'}});el.focus();if({clear}){{if(el.select)el.select();else document.execCommand('selectAll')}}return true}})()"
                );
                if s.eval(&js)? != json!(true) {
                    return Err("nothing on the page matches that".into());
                }
                if clear {
                    s.press("Backspace", "Backspace", 0, None)?;
                }
            }
            s.insert_text(t)?;
            if args["submit"].as_bool().unwrap_or(false) {
                let before = s.loads();
                s.press("Enter", "Enter", 0, None)?;
                s.settle(Duration::from_secs(4));
                return Ok(finish(&s, &action, json!({"typed": t.chars().count(), "submitted": true, "navigated": s.loads() > before})));
            }
            json!({"typed": t.chars().count()})
        }
        "press" | "key" => {
            let spec = args["key"].as_str().ok_or("press needs a key such as Enter or Control+A")?;
            let (k, mods) = parse_combo(spec);
            if k.is_empty() {
                return Err("that is not a key".into());
            }
            let before = s.loads();
            s.press(&k, &k, mods, None)?;
            s.settle(Duration::from_secs(3));
            json!({"pressed": spec, "navigated": s.loads() > before})
        }
        "scroll" => {
            if args["ref"].is_number() || args["selector"].as_str().is_some_and(|x| !x.is_empty()) {
                let f = find_js(args)?;
                let ok = s.eval(&format!("(()=>{{const el={f};if(!el)return false;el.scrollIntoView({{block:'center'}});return true}})()"))?;
                if ok != json!(true) {
                    return Err("nothing on the page matches that".into());
                }
            } else {
                let (w, h) = s.viewport();
                let dy = args["dy"].as_f64().unwrap_or(h as f64 * 0.8);
                let dx = args["dx"].as_f64().unwrap_or(0.0);
                s.mouse("wheel", w as f64 / 2.0, h as f64 / 2.0, "none", 0, dx, dy)?;
            }
            std::thread::sleep(Duration::from_millis(250));
            let pos = s.eval("({x:Math.round(scrollX),y:Math.round(scrollY),height:document.documentElement.scrollHeight,view:innerHeight})")?;
            json!({"scroll": pos})
        }
        "hover" => {
            let (x, y, _) = if args["x"].is_number() && args["y"].is_number() {
                (args["x"].as_f64().unwrap_or(0.0), args["y"].as_f64().unwrap_or(0.0), String::new())
            } else {
                s.locate(args)?
            };
            s.mouse("move", x, y, "none", 0, 0.0, 0.0)?;
            json!({"hovered": {"x": x.round(), "y": y.round()}})
        }
        "wait" => {
            let t0 = Instant::now();
            if let Some(ms) = args["ms"].as_u64() {
                std::thread::sleep(Duration::from_millis(ms.min(30000)));
                json!({"waited_ms": ms.min(30000)})
            } else {
                let gone = args["gone"].as_bool().unwrap_or(false);
                let cond = if let Some(sel) = args["selector"].as_str().filter(|x| !x.is_empty()) {
                    format!("!!document.querySelector({})", js_str(sel))
                } else if let Some(t) = args["text"].as_str().filter(|x| !x.is_empty()) {
                    format!("(document.body?document.body.innerText:'').includes({})", js_str(t))
                } else {
                    "document.readyState==='complete'".to_string()
                };
                let want = !gone;
                let mut met = false;
                while t0.elapsed() < timeout {
                    if s.eval(&cond).ok().and_then(|v| v.as_bool()) == Some(want) {
                        met = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                if !met {
                    return Err(format!("still not there after {} ms", timeout.as_millis()));
                }
                json!({"waited_ms": t0.elapsed().as_millis() as u64})
            }
        }
        "console" => {
            let since = args["since"].as_u64().unwrap_or(0);
            let limit = args["limit"].as_u64().unwrap_or(100).clamp(1, 300) as usize;
            let errors = args["level"].as_str() == Some("error");
            let g = lk(&s.st);
            let empty = VecDeque::new();
            let ring = g.tabs.cur().map_or(&empty, |p| &p.console);
            let (list, cursor) = entries(ring, since, limit, |e| {
                !errors || matches!(e.kind.as_str(), "error" | "exception" | "assert" | "blocked")
            });
            json!({"entries": list, "cursor": cursor})
        }
        "network" => {
            let since = args["since"].as_u64().unwrap_or(0);
            let limit = args["limit"].as_u64().unwrap_or(100).clamp(1, 300) as usize;
            let all = args["all"].as_bool().unwrap_or(false);
            let g = lk(&s.st);
            let empty = VecDeque::new();
            let ring = g.tabs.cur().map_or(&empty, |p| &p.net);
            let (list, cursor) = entries(ring, since, limit, |e| all || e.status >= 400 || e.status < 0);
            json!({"entries": list, "cursor": cursor, "only_failures": !all})
        }
        "resize" | "emulate" => {
            let (w, h) = match args["device"].as_str() {
                Some("phone") | Some("mobile") => (390, 844),
                Some("tablet") => (820, 1180),
                Some("desktop") => (START_W, START_H),
                _ => (
                    args["width"].as_u64().ok_or("resize needs device or width and height")? as u32,
                    args["height"].as_u64().ok_or("resize needs device or width and height")? as u32,
                ),
            };
            s.set_viewport(w, h)?;
            json!({})
        }
        other => return Err(format!("unknown action \"{other}\": open, navigate, tabs, new_tab, switch_tab, close_tab, back, forward, reload, screenshot, snapshot, click, type, press, scroll, hover, text, html, eval, wait, console, network, resize, status, close")),
    };
    out = finish(&s, &action, out);
    Ok(out)
}

fn tab_arg(args: &Value) -> Result<u32, String> {
    args["id"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| "name the tab with id (see action tabs)".to_string())
}

fn finish(s: &Arc<Session>, action: &str, mut v: Value) -> Value {
    if !v.is_object() {
        v = json!({});
    }
    if !matches!(
        action,
        "console" | "network" | "text" | "html" | "eval" | "evaluate" | "snapshot"
    ) {
        let info = s.page_info();
        for k in ["url", "title"] {
            if v.get(k).is_none() {
                v[k] = info[k].clone();
            }
        }
    }
    let (woke, note) = {
        let mut g = lk(&s.st);
        let woke = g
            .tabs
            .cur_mut()
            .is_some_and(|p| std::mem::take(&mut p.woke));
        (woke, g.note.take())
    };
    if woke {
        v["woke"] = json!(true);
    }
    if let Some(n) = note.or_else(|| woke.then(|| WOKE_NOTE.to_string())) {
        v["note"] = json!(n);
    }
    v["ok"] = json!(true);
    v
}

pub fn handle_web(ctx: &Ctx, client: u64, out: &Arc<Outbox>, v: &Value) {
    let op = v["op"].as_str().unwrap_or("");
    let reply = |mut r: Value| {
        if v["id"].is_string() || v["id"].is_number() {
            r["t"] = json!("ebv");
            r["id"] = v["id"].clone();
            r["tab"] = json!(ctx.uid);
            r["op"] = json!(op);
            out.text(r.to_string());
        }
    };
    let num = |k: &str| v[k].as_f64().unwrap_or(0.0);
    match op {
        "open" => match ensure(ctx) {
            Ok(s) => {
                let (w, h) = (v["w"].as_u64(), v["h"].as_u64());
                {
                    let mut g = lk(&s.st);
                    g.viewers.retain(|x| x.client != client);
                    g.viewers.push(Viewer {
                        client,
                        out: Arc::clone(out),
                        keep: Instant::now(),
                    });
                    g.dirty = true;
                    g.meta_sent = None;
                    g.wake_failed = None;
                    s.touch_locked(&mut g);
                }
                if let Err(e) = s.ready() {
                    lk(&s.st).viewers.retain(|x| x.client != client);
                    return reply(json!({"ok": false, "error": e}));
                }
                if let (Some(w), Some(h)) = (w, h) {
                    if (w as u32, h as u32) != s.viewport() {
                        let _ = s.set_viewport(w as u32, h as u32);
                    }
                }
                s.cast(false);
                s.mark_meta();
                s.flush_frame(true);
                let mut m = s.meta_json();
                m["ok"] = json!(true);
                reply(m);
            }
            Err(e) => reply(json!({"ok": false, "error": e})),
        },
        "close" => {
            if let Some(s) = get(ctx.uid) {
                lk(&s.st).viewers.retain(|x| x.client != client);
                s.cast(false);
            }
            reply(json!({"ok": true}));
        }
        "kill" => {
            close(ctx.uid);
            reply(json!({"ok": true}));
        }
        "tab_new" | "tab_switch" | "tab_close" => {
            let Some(s) = get(ctx.uid) else {
                return reply(json!({"ok": false, "error": "The embedded browser is not open"}));
            };
            s.touch();
            let id = v["page"].as_u64().and_then(|n| u32::try_from(n).ok());
            let r = match (op, id) {
                ("tab_new", _) => open_tab(&s, v["url"].as_str()).map(|_| ()),
                ("tab_switch", Some(i)) => s.activate(i),
                ("tab_close", Some(i)) => close_tab(&s, i),
                _ => Err("name the tab".to_string()),
            };
            match r {
                Ok(()) => {
                    let mut m = s.meta_json();
                    m["ok"] = json!(true);
                    s.flush_frame(true);
                    reply(m);
                }
                Err(e) => reply(json!({"ok": false, "error": e})),
            }
        }
        "keep" => {
            let live = get(ctx.uid).is_some_and(|s| {
                let mut g = lk(&s.st);
                g.viewers
                    .iter_mut()
                    .filter(|x| x.client == client)
                    .for_each(|x| x.keep = Instant::now());
                s.touch_locked(&mut g);
                g.viewers.iter().any(|x| x.client == client)
            });
            reply(json!({"ok": live}));
        }
        _ => {
            let Some(s) = get(ctx.uid) else {
                return reply(json!({"ok": false, "error": "The embedded browser is not open"}));
            };
            s.touch();
            if let Err(e) = s.ready() {
                return reply(json!({"ok": false, "error": e}));
            }
            let r: Result<Value, String> = match op {
                "go" => match s.navigate(v["url"].as_str().unwrap_or("")) {
                    Ok(x) => Ok(x),
                    Err(e) => {
                        s.push("", "blocked", e.clone(), String::new(), 0, false);
                        Err(e)
                    }
                },
                "back" => s.history(-1).map(|_| json!({})),
                "forward" => s.history(1).map(|_| json!({})),
                "reload" => s.reload().map(|_| json!({})),
                "stop" => s
                    .call("Page.stopLoading", json!({}), CALL_TIMEOUT)
                    .map(|_| json!({})),
                "resize" => s
                    .set_viewport(num("w") as u32, num("h") as u32)
                    .map(|_| json!({})),
                "mouse" => {
                    let (dw, dh) = {
                        let g = lk(&s.st);
                        (g.vp.0 as f64, g.vp.1 as f64)
                    };
                    let x = (num("fx") * dw).clamp(0.0, dw);
                    let y = (num("fy") * dh).clamp(0.0, dh);
                    s.mouse(
                        v["kind"].as_str().unwrap_or("move"),
                        x,
                        y,
                        v["button"].as_str().unwrap_or("left"),
                        v["cc"].as_i64().unwrap_or(1),
                        num("dx"),
                        num("dy"),
                    )
                    .map(|_| json!({}))
                }
                "hit" | "edit" => {
                    let (dw, dh) = {
                        let g = lk(&s.st);
                        (g.vp.0 as f64, g.vp.1 as f64)
                    };
                    let x = (num("fx") * dw).clamp(0.0, dw);
                    let y = (num("fy") * dh).clamp(0.0, dh);
                    if op == "hit" {
                        s.hit_test(x, y)
                    } else {
                        s.edit_at(x, y, v["what"].as_str().unwrap_or(""), v["text"].as_str())
                            .map(|_| json!({}))
                    }
                }
                "source" => s.page_source(),
                "key" => {
                    let key = v["key"].as_str().unwrap_or("");
                    if key.is_empty() || key.chars().count() > 20 {
                        Err("no key".into())
                    } else {
                        s.press(
                            key,
                            v["code"].as_str().unwrap_or(""),
                            v["mods"].as_i64().unwrap_or(0) & 15,
                            v["text"].as_str(),
                        )
                        .map(|_| json!({}))
                    }
                }
                "text" => {
                    let t = v["text"].as_str().unwrap_or("");
                    if t.is_empty() || t.chars().count() > 20000 {
                        Err("no text".into())
                    } else {
                        s.insert_text(t).map(|_| json!({}))
                    }
                }
                other => Err(format!("unknown op {other:?}")),
            };
            match r {
                Ok(mut x) => {
                    x["ok"] = json!(true);
                    reply(x);
                }
                Err(e) => reply(json!({"ok": false, "error": e})),
            }
        }
    }
}

pub fn tick(live_tabs: &[u64], enabled: bool) -> bool {
    static LAST: Mutex<Option<(Instant, Vec<u64>)>> = Mutex::new(None);
    let mut g = lk(&LAST);
    let (at, seen) = g.get_or_insert_with(|| (Instant::now() - Duration::from_secs(60), vec![]));
    if at.elapsed() < Duration::from_secs(2) {
        return false;
    }
    *at = Instant::now();
    if !enabled {
        let any = {
            let h = lk(hub());
            !h.owners.is_empty() || h.browser.is_some()
        };
        if any {
            shutdown_all();
        }
    } else {
        reap(live_tabs);
    }
    let now = live_uids();
    let changed = *seen != now;
    *seen = now;
    changed
}

#[path = "embedded_hit.rs"]
pub mod hit;

#[cfg(test)]
#[path = "embedded_tabs_tests.rs"]
mod tabs_tests;

#[cfg(test)]
#[path = "embedded_hit_tests.rs"]
mod hit_tests;

#[cfg(test)]
#[path = "embedded_shared_tests.rs"]
mod shared_tests;
