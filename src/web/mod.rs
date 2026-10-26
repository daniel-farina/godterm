//! GodTerm Web: the whole interface in a browser, served by this process
//! (docs/WEB_APP.md has the plan and the protocol).
//!
//! Plain threads: one accepts connections on 127.0.0.1 (and one on the
//! tailnet IPv4, kept bound by another as the address comes and goes; see
//! tailnet.rs for signing in there), one per HTTP
//! request, and two per WebSocket (a reader and a writer with a bounded
//! queue). The UI thread pushes state from its loop (app_web.rs); tab
//! output goes straight from each PTY reader to the browsers (tap.rs).

pub mod app_web;
pub mod assets;
pub mod assist;
pub mod auth;
pub mod clientlog;
pub mod http;
pub mod pack;
pub mod push;
pub mod tailnet;
pub mod tap;
pub mod voice;
pub mod ws;

#[cfg(test)]
mod tests;

use crate::app::AppEvent;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// The protocol version in `hello`.
pub const PROTO: u32 = 1;

/// What a browser asks of the UI thread.
#[derive(Debug)]
pub enum ToApp {
    /// A new client: send it `hello`.
    Hello(u64),
    /// A user turn to the assistant, with images pasted into it (their
    /// bytes).
    Say {
        client: u64,
        text: String,
        images: Vec<Vec<u8>>,
    },
    /// An approval answered: its id, the choice.
    Answer {
        client: u64,
        id: String,
        choice: String,
    },
    /// Bytes were typed into a tab (they went to its PTY already).
    Typed(u64),
    /// Take (rows, cols) for a tab, or release it with None.
    Resize {
        client: u64,
        uid: u64,
        size: Option<(u16, u16)>,
        /// Who took it, in a word for the pane's header ("iPhone").
        by: Option<String>,
    },
    /// A client left: sizes it took go back to the panes.
    Gone(u64),
    /// Voice from a browser: `on` makes its mic and speakers GodTerm's
    /// (mode push or open) or gives them back; `talk` presses or releases
    /// push to talk.
    Voice {
        client: u64,
        on: Option<bool>,
        mode: String,
        talk: Option<bool>,
    },
    /// A `voice` frame for direct mode, brainstorm, stopping the speech or
    /// the page going to the background (voice.rs, web_voice_ctl).
    VoiceCtl { client: u64, v: serde_json::Value },
    /// A `git` frame from a tab's Git sheet (app_gitpanel::web_git).
    Git { client: u64, v: serde_json::Value },
    /// A `bview` frame from a tab's live browser view (browser_view.rs).
    BrowserView { client: u64, v: serde_json::Value },
    /// An `accounts` frame from the Accounts screen (app_accounts.rs).
    Accounts { client: u64, v: serde_json::Value },
}

/// One connected browser.
pub struct Client {
    pub id: u64,
    pub out: Arc<tap::Outbox>,
    /// Sent `hello`: state pushes go to it.
    pub ready: AtomicBool,
    /// Its mic, while it is GodTerm's voice input (binary kind 3 goes here).
    pub mic: Mutex<Option<Arc<crate::voice::audio::WebMic>>>,
    /// What it shows, as its last `view` frame said (device, panes, tabs,
    /// open sheet): the assistant's context for a turn typed there.
    pub view: Mutex<serde_json::Value>,
    /// The talk back voice it chose (`voice {web_tts}`); None: never sent,
    /// Kokoro af_heart (crate::voice::web_tts).
    pub tts: Mutex<Option<crate::voice::web_tts::WebTts>>,
}

/// The server's shared side.
pub struct Hub {
    pub port: u16,
    token: String,
    hosts: Vec<String>,
    tx: Sender<AppEvent>,
    next: AtomicU64,
    clients: Mutex<Vec<Arc<Client>>>,
    limiter: Mutex<auth::Limiter>,
    /// Stopped (the web app turned off or moved): the listener closes.
    stopped: AtomicBool,
    /// Tailnet sign in (`[web] tailnet_sign_in`); None: token only.
    tailnet: Option<Arc<tailnet::Tailnet>>,
    /// The app's files (embedded, or a folder reloaded when it changes).
    pub assets: Arc<assets::Assets>,
}

/// Which listener a connection came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// 127.0.0.1: local browsers and `tailscale serve`.
    Loopback,
    /// This Mac's tailnet address.
    Tailnet,
}

/// How to start the server.
#[derive(Default)]
pub struct Opts {
    /// `[web] hosts`.
    pub hosts: Vec<String>,
    pub tailnet: Option<Arc<tailnet::Tailnet>>,
    /// Listen on the tailnet IPv4 too, on this port.
    pub ip_port: Option<u16>,
    /// Serve the app from this folder, watched (`[web] assets_dir`).
    pub assets_dir: Option<std::path::PathBuf>,
}

impl Opts {
    /// From `[web]`, asking tailscale.
    pub fn from_config(cfg: &crate::config::WebCfg) -> Opts {
        let tailnet = cfg
            .tailnet_sign_in
            .then(|| Arc::new(tailnet::Tailnet::system(&cfg.tailnet_users)));
        // A demo instance never binds the tailnet address: it would race
        // the real GodTerm for its port.
        let ip = cfg.tailnet_ip && tailnet.is_some() && !crate::demo::active();
        Opts {
            hosts: cfg.hosts.clone(),
            ip_port: ip.then_some(cfg.ip_port),
            assets_dir: assets::dir_for(cfg),
            tailnet,
        }
    }
}

impl Hub {
    pub fn new(
        port: u16,
        token: String,
        hosts: Vec<String>,
        tailnet: Option<Arc<tailnet::Tailnet>>,
        tx: Sender<AppEvent>,
    ) -> Arc<Hub> {
        clientlog::set_secret(&token);
        Arc::new(Hub {
            port,
            token,
            hosts,
            tx,
            next: AtomicU64::new(1),
            clients: Mutex::new(vec![]),
            limiter: Mutex::new(auth::Limiter::default()),
            stopped: AtomicBool::new(false),
            tailnet,
            assets: assets::Assets::embedded(),
        })
    }

    pub fn tailnet(&self) -> Option<&Arc<tailnet::Tailnet>> {
        self.tailnet.as_ref()
    }

    /// A Host we answer to: loopback, *.ts.net, `[web] hosts`, or this
    /// Mac's tailnet addresses as tailscale gives them now.
    fn host_ok(&self, host: &str) -> bool {
        auth::host_ok(host, &self.hosts)
            || self.tailnet.as_ref().is_some_and(|t| {
                let ips: Vec<String> = t.own_ips().iter().map(|i| i.to_string()).collect();
                auth::host_ok(host, &ips)
            })
    }

    /// The tailnet identity of a request with no cookie.
    fn identity(&self, req: &http::Request, peer: SocketAddr, via: Via) -> tailnet::Who {
        use tailnet::Who;
        let Some(t) = &self.tailnet else {
            return Who::Unknown;
        };
        match via {
            Via::Tailnet => t.by_ip(peer.ip()),
            Via::Loopback => {
                let logins: Vec<&str> = req
                    .headers
                    .iter()
                    .filter(|(k, _)| k.eq_ignore_ascii_case("tailscale-user-login"))
                    .map(|(_, v)| v.as_str())
                    .collect();
                let [login] = logins[..] else {
                    return Who::Unknown;
                };
                // Serve's header, on the name serve answers for, from a
                // connection only tailscaled could have made.
                if !peer.ip().is_loopback() || !auth::ts_net_host(req.header("host").unwrap_or(""))
                {
                    crate::log::info(
                        "web: tailnet identity headers ignored (not via tailscale serve)",
                    );
                    return Who::Unknown;
                }
                if !auth::via_tailscale(peer) {
                    crate::log::info(
                        "web: tailnet identity headers from a local process were refused",
                    );
                    return Who::Unknown;
                }
                // Serve names the device in X-Forwarded-For. This Mac's own
                // address could be any local program, a tab's agent too,
                // so it needs the token like the IP listener does.
                let from = req
                    .header("x-forwarded-for")
                    .and_then(|v| v.split(',').next())
                    .and_then(|v| v.trim().parse::<std::net::IpAddr>().ok());
                match from {
                    Some(ip) if !t.own_ips().contains(&ip) => t.judge(login),
                    _ => {
                        crate::log::info(
                            "web: tailnet sign in from this Mac itself needs the token",
                        );
                        Who::Unknown
                    }
                }
            }
        }
    }

    /// Stop serving: every browser is disconnected and the port is freed
    /// (the accept loop is woken by one last connection).
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        for c in self.clients() {
            c.out.close();
        }
        let _ = TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], self.port)),
            std::time::Duration::from_millis(300),
        );
        crate::log::info(&format!("web: stopped serving port {}", self.port));
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub fn clients(&self) -> Vec<Arc<Client>> {
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn client(&self, id: u64) -> Option<Arc<Client>> {
        self.clients().into_iter().find(|c| c.id == id)
    }

    pub fn any_ready(&self) -> bool {
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|c| c.ready.load(Ordering::Relaxed))
    }

    /// A text frame to every client that has its hello.
    pub fn broadcast(&self, frame: &str) {
        for c in self.clients() {
            if c.ready.load(Ordering::Relaxed) {
                c.out.text(frame.to_string());
            }
        }
    }

    pub(crate) fn add(&self) -> Arc<Client> {
        let c = Arc::new(Client {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            out: tap::Outbox::new(),
            ready: AtomicBool::new(false),
            mic: Mutex::new(None),
            view: Mutex::new(serde_json::Value::Null),
            tts: Mutex::new(None),
        });
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&c));
        c
    }

    fn remove(&self, id: u64) {
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| c.id != id);
        let _ = self.tx.send(AppEvent::Web(ToApp::Gone(id)));
    }

    pub fn send(&self, ev: ToApp) -> bool {
        self.tx.send(AppEvent::Web(ev)).is_ok()
    }

    pub fn events(&self) -> &Sender<AppEvent> {
        &self.tx
    }

    /// The cookie check for a request.
    pub fn authed(&self, req: &http::Request) -> bool {
        let want = auth::cookie_value(&self.token);
        req.header("cookie")
            .and_then(|c| auth::cookie_in(c, auth::COOKIE))
            .is_some_and(|v| auth::same(v.as_bytes(), want.as_bytes()))
    }

    /// One HTTP connection: a login, a file, or the WebSocket.
    pub fn handle(self: &Arc<Self>, s: TcpStream, via: Via) {
        let Ok(peer) = s.peer_addr() else {
            return;
        };
        self.handle_from(s, peer, via);
    }

    /// `handle` with the peer address given (tests stand in a tailnet
    /// peer this way).
    pub fn handle_from(self: &Arc<Self>, mut s: TcpStream, peer: SocketAddr, via: Via) {
        let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(15)));
        let _ = s.set_nodelay(true);
        // The tailnet listener answers tailnet addresses only.
        if via == Via::Tailnet && !tailnet::in_tailnet(peer.ip()) {
            crate::log::info("web: refused a non-tailnet peer on the tailnet address");
            return;
        }
        let Some(req) = http::read_head(&mut s) else {
            let _ = http::text(&mut s, "400 Bad Request", "bad request");
            return;
        };
        let _ = self.route(s, req, peer, via);
    }

    /// The peer is on this machine: loopback, or this Mac's own tailnet
    /// address.
    fn local_peer(&self, ip: IpAddr) -> bool {
        ip.is_loopback()
            || self
                .tailnet
                .as_ref()
                .is_some_and(|t| t.own_ips().contains(&ip))
    }

    fn route(
        self: &Arc<Self>,
        mut s: TcpStream,
        req: http::Request,
        peer: SocketAddr,
        via: Via,
    ) -> std::io::Result<()> {
        // DNS rebinding: only our own names.
        let host = req.header("host").unwrap_or("").to_string();
        if !self.host_ok(&host) {
            return http::text(&mut s, "421 Misdirected Request", "unknown host");
        }
        // A photo shared to the installed app goes to its service worker;
        // one that reaches here (no worker yet) just opens the app.
        if req.method == "POST" && req.path == "/share" {
            return http::respond(
                &mut s,
                "303 See Other",
                &[("Location", "/".into())],
                &[],
                false,
            );
        }
        if req.method != "GET" && req.method != "HEAD" {
            return http::text(&mut s, "405 Method Not Allowed", "GET only");
        }
        // A login link: the token for a cookie, then the same page
        // without the token in the address bar (or history).
        // GodTerm's own tabs may not sign in or open the socket.
        let inside = || {
            let i = auth::from_inside(peer, self.local_peer(peer.ip()));
            if i {
                crate::log::info("web: refused a connection from a process GodTerm started");
            }
            i
        };
        if (req.param("t").is_some() || req.path == "/ws") && inside() {
            return http::text(
                &mut s,
                "403 Forbidden",
                "not from inside GodTerm: its tabs get the read-only control socket",
            );
        }
        if let Some(t) = req.param("t") {
            let now = std::time::Instant::now();
            let mut lim = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
            if lim.blocked(now) {
                crate::log::info("web: login refused: too many wrong tokens, waiting");
                return http::text(
                    &mut s,
                    "429 Too Many Requests",
                    "too many wrong tokens; wait a minute",
                );
            }
            if !auth::same(t.as_bytes(), self.token.as_bytes()) {
                lim.fail(now);
                drop(lim);
                crate::log::info("web: a login with a wrong token");
                return http::text(&mut s, "403 Forbidden", "wrong token");
            }
            drop(lim);
            let q = req.query_without("t");
            let to = if q.is_empty() {
                req.path.clone()
            } else {
                format!("{}?{q}", req.path)
            };
            crate::log::info("web: a browser logged in");
            return http::respond(
                &mut s,
                "303 See Other",
                &[
                    ("Location", to),
                    (
                        "Set-Cookie",
                        auth::set_cookie(&self.token, https(&req, via)),
                    ),
                    ("Cache-Control", "no-store".into()),
                ],
                &[],
                false,
            );
        }
        // What installing the app needs, without a cookie.
        if http::public(&req.path) && !self.authed(&req) {
            if http::serve_static(&mut s, &req, &self.assets.current(), &[])? {
                return Ok(());
            }
            return http::text(&mut s, "404 Not Found", "not found");
        }
        // No cookie: a tailnet user the owner allows signs in now (the
        // cookie is set on this answer, so the next requests are quick).
        let mut signed_in = None;
        if !self.authed(&req) {
            match self.identity(&req, peer, via) {
                tailnet::Who::Allowed(login) => {
                    crate::log::info(&format!("web: {login} signed in by tailnet identity"));
                    signed_in = Some(login);
                }
                tailnet::Who::Refused(login) => {
                    crate::log::info(&format!("web: refused tailnet user {login}"));
                    return http::text(&mut s, "403 Forbidden", &tailnet::refused_text(&login));
                }
                tailnet::Who::Unknown => {
                    // The page itself: a sign in form (a home screen app
                    // starts with no cookie and no address bar).
                    let page = req.path == "/" || req.path == "/index.html";
                    if page
                        && req
                            .header("accept")
                            .is_some_and(|a| a.contains("text/html"))
                        && http::serve_page(
                            &mut s,
                            &req,
                            &self.assets.current(),
                            "signin.html",
                            "401 Unauthorized",
                        )?
                    {
                        return Ok(());
                    }
                    return http::text(
                        &mut s,
                        "401 Unauthorized",
                        "Open the link `godterm web-url` prints to sign in.",
                    );
                }
            }
        }
        if req.path == "/ws" {
            if !req.wants_upgrade() {
                return http::text(&mut s, "426 Upgrade Required", "websocket only");
            }
            // Cross site WebSocket hijacking: the page must be ours.
            if !auth::origin_ok(req.header("origin"), &host, req.header("x-forwarded-host")) {
                crate::log::info("web: a WebSocket from another origin was refused");
                return http::text(&mut s, "403 Forbidden", "bad origin");
            }
            return ws::accept(self, s, &req);
        }
        let cookie: Vec<(&str, String)> = if signed_in.is_some() {
            vec![(
                "Set-Cookie",
                auth::set_cookie(&self.token, https(&req, via)),
            )]
        } else {
            vec![]
        };
        if http::serve_static(&mut s, &req, &self.assets.current(), &cookie)? {
            return Ok(());
        }
        http::text(&mut s, "404 Not Found", "not found")
    }
}

/// Over https: through `tailscale serve` (a *.ts.net Host or the proxy
/// says so). The tailnet IP listener is plain http.
fn https(req: &http::Request, via: Via) -> bool {
    via == Via::Loopback && req.https()
}

/// Listen on 127.0.0.1:`port` with `[web] hosts` only (token sign in);
/// connections are served on their own threads.
#[cfg(test)]
pub fn start(port: u16, hosts: Vec<String>, tx: Sender<AppEvent>) -> anyhow::Result<Arc<Hub>> {
    start_with(
        port,
        || Opts {
            hosts,
            ..Opts::default()
        },
        tx,
    )
}

/// Listen on 127.0.0.1:`port`, and on the tailnet IPv4 when `opts` says
/// (made once the port is ours: it may ask tailscale).
pub fn start_with(
    port: u16,
    opts: impl FnOnce() -> Opts,
    tx: Sender<AppEvent>,
) -> anyhow::Result<Arc<Hub>> {
    let token = auth::load_or_create()?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| anyhow::anyhow!("binding 127.0.0.1:{port}: {e}"))?;
    let port = listener.local_addr()?.port();
    let opts = opts();
    let mut hosts = opts.hosts;
    if opts.tailnet.is_none() {
        // The Mac's own tailnet addresses: a link by IP for browsers that
        // cannot resolve *.ts.net (a secure DNS setting bypasses MagicDNS).
        hosts.extend(tailscale_ips());
    }
    let mut hub = Hub::new(port, token, hosts, opts.tailnet, tx);
    if let Some(dir) = opts.assets_dir {
        if let Some(h) = Arc::get_mut(&mut hub) {
            h.assets = assets::Assets::from_dir(dir);
        }
        let w = Arc::downgrade(&hub);
        std::thread::Builder::new()
            .name("web-assets".into())
            .spawn(move || watch_assets(w, assets::WATCH_EVERY))?;
    }
    let h = Arc::clone(&hub);
    std::thread::Builder::new()
        .name("web".into())
        .spawn(move || accept_loop(h, listener, Via::Loopback, None))?;
    crate::log::info(&format!("web: serving http://127.0.0.1:{port}"));
    if let Some(ip_port) = opts.ip_port {
        let h = Arc::clone(&hub);
        std::thread::Builder::new()
            .name("web-ip".into())
            .spawn(move || keep_ip_listener(h, ip_port, IP_RETRY, IP_RECHECK))?;
    }
    Ok(hub)
}

/// Check the served folder every `every` while the server runs; a new
/// build goes to every browser at once (its "Update available" banner).
pub fn watch_assets(hub: std::sync::Weak<Hub>, every: std::time::Duration) {
    loop {
        std::thread::sleep(every);
        let Some(h) = hub.upgrade() else {
            return;
        };
        if h.is_stopped() {
            return;
        }
        if let Some(build) = h.assets.poll() {
            h.broadcast(&serde_json::json!({"t": "build", "build": build}).to_string());
        }
    }
}

fn accept_loop(h: Arc<Hub>, listener: TcpListener, via: Via, alive: Option<Arc<AtomicBool>>) {
    for conn in listener.incoming().flatten() {
        if h.is_stopped() || alive.as_ref().is_some_and(|a| !a.load(Ordering::SeqCst)) {
            break;
        }
        let h = Arc::clone(&h);
        let _ = std::thread::Builder::new()
            .name("web-conn".into())
            .spawn(move || h.handle(conn, via));
    }
}

/// A busy tailnet port is tried again this often.
pub const IP_RETRY: std::time::Duration = std::time::Duration::from_secs(5);
/// The tailnet address is checked again this often while bound.
pub const IP_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// Keep a listener on the tailnet IPv4:`port` until the hub stops: bound
/// when tailscale has an address and the port is free (tried again every
/// `retry`), moved when the address changes (checked every `recheck`).
pub fn keep_ip_listener(
    h: Arc<Hub>,
    port: u16,
    retry: std::time::Duration,
    recheck: std::time::Duration,
) {
    let Some(t) = h.tailnet().cloned() else {
        return;
    };
    let mut bound: Option<(SocketAddr, Arc<AtomicBool>)> = None;
    let mut said = false;
    let close = |b: (SocketAddr, Arc<AtomicBool>)| {
        b.1.store(false, Ordering::SeqCst);
        // Wake its accept so it sees it is done.
        let _ = TcpStream::connect_timeout(&b.0, std::time::Duration::from_millis(300));
    };
    let mut first = true;
    while !h.is_stopped() {
        if !first {
            t.refresh();
        }
        first = false;
        let want = t.ipv4();
        if bound.as_ref().map(|b| b.0.ip()) != want {
            if let Some(b) = bound.take() {
                crate::log::info(&format!("web: the tailnet address left {}", b.0));
                close(b);
            }
            if let Some(ip) = want {
                match TcpListener::bind((ip, port)) {
                    Ok(l) => {
                        let addr = l.local_addr().unwrap_or(SocketAddr::new(ip, port));
                        let alive = Arc::new(AtomicBool::new(true));
                        let (h2, a2) = (Arc::clone(&h), Arc::clone(&alive));
                        if std::thread::Builder::new()
                            .name("web-ip-accept".into())
                            .spawn(move || accept_loop(h2, l, Via::Tailnet, Some(a2)))
                            .is_ok()
                        {
                            crate::log::info(&format!("web: serving http://{addr} (tailnet)"));
                            bound = Some((addr, alive));
                            said = false;
                        }
                    }
                    Err(e) => {
                        if !said {
                            crate::log::error(&format!(
                                "web: binding {ip}:{port}: {e}; trying again every {} s",
                                retry.as_secs()
                            ));
                            said = true;
                        }
                    }
                }
            }
        }
        let wait = if bound.is_some() { recheck } else { retry };
        let until = std::time::Instant::now() + wait;
        while !h.is_stopped() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    if let Some(b) = bound.take() {
        close(b);
    }
}

/// The web app is on: `[web] enabled`, or `--web` at start.
pub fn wanted(cfg: &crate::config::Config) -> bool {
    cfg.web.enabled || std::env::args().any(|a| a == "--web")
}

/// The port: GODTERM_WEB_PORT, `--web-port N`, else `[web] port`.
pub fn port(cfg: &crate::config::Config) -> u16 {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--web-port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
        .or_else(|| crate::config::env_var("WEB_PORT").and_then(|p| p.parse().ok()))
        .unwrap_or(cfg.web.port)
}

/// `godterm web-url`: the links that sign a browser in.
pub fn url_command() -> anyhow::Result<()> {
    let cfg = crate::config::Config::load_or_init().unwrap_or_default();
    let port = port(&cfg);
    let token = auth::load_or_create()?;
    let name = tailscale_name();
    if cfg.web.tailnet_sign_in && name.is_some() {
        let name = name.clone().unwrap_or_default();
        let t = tailnet::Tailnet::system(&cfg.web.tailnet_users);
        let who = t.allowed();
        println!("tailscale: https://{name}:{port}/");
        if cfg.web.tailnet_ip && !crate::demo::active() {
            if let Some(ip) = t.ipv4() {
                println!("tailnet IP: http://{ip}:{}/", cfg.web.ip_port);
            }
        }
        println!(
            "  (no token: signs in {})",
            if who.is_empty() {
                "nobody yet (tailscale gave no owner; set [web] tailnet_users)".to_string()
            } else {
                who.join(", ")
            }
        );
        println!("local:     http://127.0.0.1:{port}/?t={token}");
        println!();
        println!(
            "(the https link needs: tailscale serve --bg --https={port} http://127.0.0.1:{port})"
        );
    } else {
        println!("local:     http://127.0.0.1:{port}/?t={token}");
        match name {
            Some(name) => {
                println!("tailscale: https://{name}:{port}/?t={token}");
                println!();
                println!("(the tailnet link needs: tailscale serve --bg --https={port} http://127.0.0.1:{port})");
            }
            None => println!("tailscale: not running (or not installed)"),
        }
    }
    if !cfg.web.enabled {
        println!();
        println!(
            "The web app is off: set [web] enabled = true in config.toml, or start godterm --web."
        );
    }
    Ok(())
}

/// This Mac's tailnet name (Self.DNSName), without the trailing dot.
fn tailscale_name() -> Option<String> {
    let v = tailscale_self()?;
    let name = v["DNSName"].as_str()?.trim_end_matches('.');
    (!name.is_empty()).then(|| name.to_string())
}

/// This Mac's tailnet addresses (Self.TailscaleIPs).
fn tailscale_ips() -> Vec<String> {
    tailscale_self()
        .and_then(|v| v["TailscaleIPs"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|ip| ip.as_str().map(str::to_string))
        .collect()
}

/// `tailscale status --json`'s Self, from the first tailscale that answers.
fn tailscale_self() -> Option<serde_json::Value> {
    let mut v = tailnet::cli(&["status", "--json"])?;
    Some(v["Self"].take())
}
