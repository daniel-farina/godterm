use crate::routes::{RouteCfg, RoutesCfg};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const INSTALL_HINT: &str = "tailscaled is not installed. Install the open source Tailscale (for example: brew install tailscale), or set its path under Manage routes.";

#[derive(Debug, Clone, PartialEq)]
pub enum TsState {
    Stopped,
    Missing(String),
    Starting,
    Login(String),
    ChooseNode,
    Unreachable(String),
    Ready,
    Error(String),
}

impl TsState {
    pub fn word(&self) -> &'static str {
        match self {
            TsState::Stopped => "stopped",
            TsState::Missing(_) => "missing",
            TsState::Starting => "starting",
            TsState::Login(_) => "login",
            TsState::ChooseNode => "choose",
            TsState::Unreachable(_) => "unreachable",
            TsState::Ready => "ready",
            TsState::Error(_) => "error",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            TsState::Stopped => "Starts when a tab uses it".into(),
            TsState::Missing(m) | TsState::Unreachable(m) | TsState::Error(m) => m.clone(),
            TsState::Starting => "Starting".into(),
            TsState::Login(_) => "Login needed: open the login link once".into(),
            TsState::ChooseNode => "Choose an exit node".into(),
            TsState::Ready => "Ready".into(),
        }
    }

    pub fn waits(&self) -> bool {
        matches!(
            self,
            TsState::Starting | TsState::Login(_) | TsState::Stopped
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExitNode {
    pub name: String,
    pub ip: String,
    pub online: bool,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct Bins {
    pub daemon: PathBuf,
    pub cli: PathBuf,
}

const SEARCH: [&str; 6] = [
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/usr/bin",
    "/usr/sbin",
];

fn is_exec(p: &Path) -> bool {
    p.is_absolute() && p.is_file()
}

pub fn find_bin(name: &str, configured: &str, also: Option<&Path>) -> Option<PathBuf> {
    let c = configured.trim();
    if !c.is_empty() {
        let p = PathBuf::from(c);
        return is_exec(&p).then_some(p);
    }
    if cfg!(test) {
        return None;
    }
    also.into_iter()
        .map(|d| d.join(name))
        .chain(SEARCH.iter().map(|d| Path::new(d).join(name)))
        .find(|p| is_exec(p))
}

pub fn resolve(cfg: &RoutesCfg) -> Result<Bins, String> {
    let daemon = find_bin("tailscaled", &cfg.tailscaled_bin, None).ok_or_else(|| {
        if cfg.tailscaled_bin.trim().is_empty() {
            INSTALL_HINT.to_string()
        } else {
            format!(
                "tailscaled was not found at the configured path. {}",
                INSTALL_HINT
            )
        }
    })?;
    let cli = find_bin("tailscale", &cfg.tailscale_bin, daemon.parent()).ok_or(
        "the tailscale command was not found next to tailscaled; set its path under Manage routes",
    )?;
    Ok(Bins { daemon, cli })
}

pub fn run(
    bin: &Path,
    args: &[String],
    out: &Path,
    timeout: Duration,
) -> Result<(bool, String), String> {
    let f = std::fs::File::create(out)
        .map_err(|e| format!("could not write {}: {e}", out.display()))?;
    let f2 = f.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(f)
        .stderr(f2)
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", bin.display()))?;
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the tailscale command did not answer in time".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let text = std::fs::read_to_string(out).unwrap_or_default();
    Ok((status.success(), text))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub backend: String,
    pub auth_url: String,
    pub nodes: Vec<ExitNode>,
    pub exit_online: bool,
    pub active_names: Vec<String>,
}

fn peer_names(p: &Value) -> Vec<String> {
    let mut out = vec![];
    if let Some(h) = p["HostName"].as_str() {
        out.push(h.to_string());
    }
    if let Some(d) = p["DNSName"].as_str() {
        let d = d.trim_end_matches('.');
        out.push(d.to_string());
        if let Some(first) = d.split('.').next() {
            out.push(first.to_string());
        }
    }
    for ip in p["TailscaleIPs"].as_array().into_iter().flatten() {
        if let Some(ip) = ip.as_str() {
            out.push(ip.split('/').next().unwrap_or(ip).to_string());
        }
    }
    out.retain(|n| !n.is_empty());
    out
}

pub fn parse_status(v: &Value) -> Status {
    let mut st = Status {
        backend: v["BackendState"].as_str().unwrap_or("").to_string(),
        auth_url: v["AuthURL"].as_str().unwrap_or("").to_string(),
        exit_online: v["ExitNodeStatus"]["Online"].as_bool().unwrap_or(false),
        ..Default::default()
    };
    for p in v["Peer"].as_object().into_iter().flat_map(|m| m.values()) {
        let active = p["ExitNode"].as_bool().unwrap_or(false);
        if active {
            st.active_names = peer_names(p);
        }
        if !active && !p["ExitNodeOption"].as_bool().unwrap_or(false) {
            continue;
        }
        let dns = p["DNSName"].as_str().unwrap_or("").trim_end_matches('.');
        let name = dns
            .split('.')
            .next()
            .filter(|n| !n.is_empty())
            .or(p["HostName"].as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        st.nodes.push(ExitNode {
            name,
            ip: peer_names(p)
                .into_iter()
                .find(|n| n.parse::<std::net::Ipv4Addr>().is_ok())
                .unwrap_or_default(),
            online: p["Online"].as_bool().unwrap_or(false),
            active,
        });
    }
    st.nodes.sort_by(|a, b| a.name.cmp(&b.name));
    st
}

pub fn parse_exit_list(text: &str) -> Vec<String> {
    let mut out = vec![];
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() < 2 || w[0].parse::<std::net::IpAddr>().is_err() {
            continue;
        }
        let name = w[1].split('.').next().unwrap_or("");
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

pub fn system_exit_nodes(cfg: &RoutesCfg, scratch: &Path) -> Result<Vec<String>, String> {
    let cli = match resolve(cfg) {
        Ok(b) => b.cli,
        Err(_) => find_bin("tailscale", &cfg.tailscale_bin, None)
            .ok_or("the tailscale command was not found")?,
    };
    let _ = std::fs::create_dir_all(scratch);
    let out = scratch.join("exit-nodes.out");
    let r = run(
        &cli,
        &["exit-node".into(), "list".into()],
        &out,
        Duration::from_secs(4),
    );
    let _ = std::fs::remove_file(&out);
    let (ok, text) = r?;
    if !ok {
        return Err("the system Tailscale did not list exit nodes".into());
    }
    Ok(parse_exit_list(&text))
}

pub fn hostname(route: &str) -> String {
    format!("godterm-{}", route.replace('_', "-"))
}

pub struct Instance {
    pub name: String,
    pub dir: PathBuf,
    pub sock: PathBuf,
    pub port: u16,
    route: RouteCfg,
    bins: RoutesCfg,
    daemon: Option<Child>,
    pub proxy: bool,
    up: Option<Child>,
    up_at: Option<Instant>,
    cli_ok: Option<PathBuf>,
    node_set: bool,
    set_at: Option<Instant>,
    started: Instant,
    pub state: TsState,
    pub nodes: Vec<ExitNode>,
}

fn term(child: &mut Child) {
    let pid = child.id();
    crate::platform::kill(pid as i32, crate::platform::SIGTERM);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Instance {
    pub fn new(home: &Path, route: RouteCfg, bins: RoutesCfg) -> Instance {
        let dir = home.join("tailscale").join(&route.name);
        Instance {
            name: route.name.clone(),
            sock: dir.join("tailscaled.sock"),
            dir,
            port: 0,
            route,
            bins,
            daemon: None,
            proxy: false,
            up: None,
            up_at: None,
            cli_ok: None,
            node_set: false,
            set_at: None,
            started: Instant::now(),
            state: TsState::Starting,
            nodes: vec![],
        }
    }

    pub fn set_cfg(&mut self, route: &RouteCfg, bins: &RoutesCfg) {
        if route.exit_node != self.route.exit_node || route.allow_lan != self.route.allow_lan {
            self.node_set = false;
            self.set_at = None;
        }
        self.route = route.clone();
        if *bins != self.bins {
            self.bins = bins.clone();
            self.cli_ok = None;
        }
    }

    pub fn daemon_pid(&self) -> Option<u32> {
        self.daemon.as_ref().map(|c| c.id())
    }

    fn pidfile(&self) -> PathBuf {
        self.dir.join("tailscaled.pid")
    }

    fn sock_arg(&self) -> String {
        format!("--socket={}", self.sock.display())
    }

    fn cli(&self, bins: &Bins, args: &[&str], timeout: Duration) -> Result<(bool, String), String> {
        let mut a = vec![self.sock_arg()];
        a.extend(args.iter().map(|s| s.to_string()));
        run(&bins.cli, &a, &self.dir.join("cli.out"), timeout)
    }

    fn cli_honours_socket(&mut self, bins: &Bins) -> bool {
        if self.cli_ok.as_deref() == Some(bins.cli.as_path()) {
            return true;
        }
        let absent = format!("--socket={}", self.dir.join("absent.sock").display());
        let r = run(
            &bins.cli,
            &[absent, "status".into(), "--json".into()],
            &self.dir.join("cli.out"),
            Duration::from_secs(4),
        );
        let ignores = matches!(&r, Ok((true, _)));
        if !ignores {
            self.cli_ok = Some(bins.cli.clone());
        }
        !ignores
    }

    fn reap_orphan(&self) {
        let Some(pid) = std::fs::read_to_string(self.pidfile())
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
        else {
            return;
        };
        let _ = std::fs::remove_file(self.pidfile());
        if !crate::platform::alive(pid) {
            return;
        }
        let ps = Path::new("/bin/ps");
        let Ok(out) = Command::new(ps)
            .args(["-o", "command=", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .output()
        else {
            return;
        };
        let cmd = String::from_utf8_lossy(&out.stdout);
        if !cmd.contains(&format!("--statedir={}", self.dir.display())) {
            return;
        }
        crate::log::info(&format!(
            "routes: {}: stopping the tailscaled an earlier GodTerm left running",
            self.name
        ));
        crate::platform::kill(pid as i32, crate::platform::SIGTERM);
        let t0 = Instant::now();
        while crate::platform::alive(pid) && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn daemon_args(&self, proxy: bool) -> Vec<String> {
        let mut a = vec![
            "--tun=userspace-networking".to_string(),
            format!("--statedir={}", self.dir.display()),
            self.sock_arg(),
        ];
        if proxy {
            a.push(format!("--socks5-server=127.0.0.1:{}", self.port));
            a.push(format!(
                "--outbound-http-proxy-listen=127.0.0.1:{}",
                self.port
            ));
        }
        a
    }

    fn start_daemon(&mut self, bins: &Bins, proxy: bool) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("could not create the route folder: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        self.reap_orphan();
        let log =
            std::fs::File::create(self.dir.join("tailscaled.log")).map_err(|e| e.to_string())?;
        let log2 = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(&bins.daemon)
            .args(self.daemon_args(proxy))
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log2)
            .spawn()
            .map_err(|e| format!("could not start tailscaled: {e}"))?;
        crate::log::info(&format!(
            "routes: {}: started tailscaled pid {} ({})",
            self.name,
            child.id(),
            if proxy {
                "proxy on"
            } else {
                "setup, proxy off"
            }
        ));
        let _ = std::fs::write(self.pidfile(), child.id().to_string());
        self.daemon = Some(child);
        self.proxy = proxy;
        self.started = Instant::now();
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut u) = self.up.take() {
            let _ = u.kill();
            let _ = u.wait();
        }
        if let Some(mut d) = self.daemon.take() {
            crate::log::info(&format!(
                "routes: {}: stopping tailscaled pid {}",
                self.name,
                d.id()
            ));
            term(&mut d);
        }
        let _ = std::fs::remove_file(self.pidfile());
        self.proxy = false;
        self.state = TsState::Stopped;
    }

    fn restart(&mut self, bins: &Bins, proxy: bool) {
        if let Some(mut d) = self.daemon.take() {
            term(&mut d);
        }
        self.proxy = false;
        if proxy {
            match crate::routes::free_port() {
                Some(p) => self.port = p,
                None => {
                    self.state = TsState::Error("no free local port for the route".into());
                    return;
                }
            }
        }
        self.state = match self.start_daemon(bins, proxy) {
            Ok(()) => TsState::Starting,
            Err(e) => TsState::Error(e),
        };
    }

    fn drop_proxy(&mut self, bins: &Bins) {
        if self.proxy {
            crate::log::info(&format!(
                "routes: {}: the exit node is not active, closing the proxy",
                self.name
            ));
            self.restart(bins, false);
        }
    }

    fn login(&mut self, bins: &Bins, st: &Status) {
        if let Some(u) = &mut self.up {
            if matches!(u.try_wait(), Ok(Some(_))) {
                self.up = None;
            }
        }
        let due = self
            .up_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(5));
        if self.up.is_none() && due {
            let out = std::fs::File::create(self.dir.join("up.out"));
            if let Ok(out) = out {
                let err = out.try_clone().ok();
                let host = format!("--hostname={}", hostname(&self.name));
                let mut c = Command::new(&bins.cli);
                c.args([
                    self.sock_arg().as_str(),
                    "up",
                    "--reset",
                    host.as_str(),
                    "--json",
                ])
                .stdin(Stdio::null())
                .stdout(out);
                match err {
                    Some(e) => c.stderr(e),
                    None => c.stderr(Stdio::null()),
                };
                crate::log::info(&format!("routes: {}: tailscale up (login)", self.name));
                self.up = c.spawn().ok();
                self.up_at = Some(Instant::now());
            }
        }
        let mut url = st.auth_url.clone();
        if url.is_empty() {
            let text = std::fs::read_to_string(self.dir.join("up.out")).unwrap_or_default();
            if let Some(i) = text.find("https://") {
                url = text[i..]
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '"')
                    .collect();
            }
        }
        self.state = if url.starts_with("https://") {
            TsState::Login(url)
        } else {
            TsState::Starting
        };
    }

    pub fn step(&mut self) {
        let bins = match resolve(&self.bins) {
            Ok(b) => b,
            Err(e) => {
                self.stop();
                self.state = TsState::Missing(e);
                return;
            }
        };
        if self.sock.as_os_str().len() > 100 {
            self.state = TsState::Error(
                "the GodTerm data folder path is too long for the tailscaled socket".into(),
            );
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            self.state = TsState::Error(format!("could not create the route folder: {e}"));
            return;
        }
        if !self.cli_honours_socket(&bins) {
            self.stop();
            self.state = TsState::Missing("this tailscale command ignores --socket (it belongs to the Tailscale app), so it is not used. Install the open source Tailscale (for example: brew install tailscale).".into());
            return;
        }
        let dead = match &mut self.daemon {
            Some(d) => matches!(d.try_wait(), Ok(Some(_)) | Err(_)),
            None => true,
        };
        if dead {
            self.daemon = None;
            self.state = match self.start_daemon(&bins, false) {
                Ok(()) => TsState::Starting,
                Err(e) => TsState::Error(e),
            };
            return;
        }
        let st = match self.cli(&bins, &["status", "--json"], Duration::from_secs(4)) {
            Ok((_, text)) => match text
                .find('{')
                .and_then(|i| serde_json::from_str::<Value>(&text[i..]).ok())
            {
                Some(v) => parse_status(&v),
                None => {
                    self.state = if self.started.elapsed() > Duration::from_secs(30) {
                        TsState::Error("tailscaled is running but does not answer; see tailscaled.log in the route folder".into())
                    } else {
                        TsState::Starting
                    };
                    return;
                }
            },
            Err(e) => {
                self.state = TsState::Error(e);
                return;
            }
        };
        self.nodes = st.nodes.clone();
        match st.backend.as_str() {
            "Running" => {}
            "NeedsLogin" | "NoState" | "Stopped" => {
                self.node_set = false;
                self.drop_proxy(&bins);
                self.login(&bins, &st);
                return;
            }
            "NeedsMachineAuth" => {
                self.drop_proxy(&bins);
                self.state = TsState::Unreachable(
                    "waiting for a tailnet admin to approve this device".into(),
                );
                return;
            }
            _ => {
                self.state = TsState::Starting;
                return;
            }
        }
        let want = self.route.exit_node.trim().to_string();
        if want.is_empty() {
            self.drop_proxy(&bins);
            self.state = TsState::ChooseNode;
            return;
        }
        if !self.node_set {
            self.drop_proxy(&bins);
            if self
                .set_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
            {
                return;
            }
            self.set_at = Some(Instant::now());
            let node = format!("--exit-node={want}");
            let lan = format!("--exit-node-allow-lan-access={}", self.route.allow_lan);
            crate::log::info(&format!("routes: {}: tailscale set exit node", self.name));
            match self.cli(
                &bins,
                &["set", node.as_str(), lan.as_str()],
                Duration::from_secs(8),
            ) {
                Ok((true, _)) => {
                    self.node_set = true;
                    self.state = TsState::Starting;
                }
                Ok((false, text)) => {
                    let line = text
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .trim();
                    self.state = TsState::Unreachable(format!(
                        "could not use exit node {want}: {}",
                        line.chars().take(160).collect::<String>()
                    ));
                }
                Err(e) => self.state = TsState::Error(e),
            }
            return;
        }
        let matches = want == "auto:any" && !st.active_names.is_empty()
            || st
                .active_names
                .iter()
                .any(|n| n.eq_ignore_ascii_case(&want));
        if !matches {
            self.node_set = false;
            self.drop_proxy(&bins);
            if !matches!(self.state, TsState::Unreachable(_)) {
                self.state = TsState::Starting;
            }
            return;
        }
        if !st.exit_online {
            self.drop_proxy(&bins);
            self.state = TsState::Unreachable(format!("exit node {want} is offline"));
            return;
        }
        if !self.proxy {
            self.restart(&bins, true);
            return;
        }
        self.state = TsState::Ready;
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.daemon.is_some() || self.up.is_some() {
            self.stop();
        }
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: TsState,
    pub nodes: Vec<ExitNode>,
    pub port: u16,
    pub proxy: bool,
    pub pid: Option<u32>,
}

pub struct Handle {
    pub snap: Arc<Mutex<Snapshot>>,
    cfg: Arc<Mutex<(RouteCfg, RoutesCfg)>>,
    stop: Arc<AtomicBool>,
    pub hold_until: Option<Instant>,
}

impl Handle {
    pub fn spawn(home: &Path, route: RouteCfg, bins: RoutesCfg) -> Result<Handle, String> {
        let mut inst = Instance::new(home, route.clone(), bins.clone());
        let snap = Arc::new(Mutex::new(Snapshot {
            state: TsState::Starting,
            nodes: vec![],
            port: 0,
            proxy: false,
            pid: None,
        }));
        let cfg = Arc::new(Mutex::new((route, bins)));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, c2, f2) = (Arc::clone(&snap), Arc::clone(&cfg), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("route-tailscale".into())
            .spawn(move || {
                while !f2.load(Ordering::SeqCst) {
                    let (r, b) = c2.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    inst.set_cfg(&r, &b);
                    inst.step();
                    {
                        let mut s = s2.lock().unwrap_or_else(|e| e.into_inner());
                        s.state = inst.state.clone();
                        s.nodes = inst.nodes.clone();
                        s.proxy = inst.proxy;
                        s.port = inst.port;
                        s.pid = inst.daemon_pid();
                    }
                    let pause = if inst.state == TsState::Ready {
                        3000
                    } else {
                        1200
                    };
                    let t0 = Instant::now();
                    while t0.elapsed() < Duration::from_millis(pause) && !f2.load(Ordering::SeqCst)
                    {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                inst.stop();
                let mut s = s2.lock().unwrap_or_else(|e| e.into_inner());
                s.state = TsState::Stopped;
                s.proxy = false;
                s.pid = None;
            })
            .map_err(|e| e.to_string())?;
        Ok(Handle {
            snap,
            cfg,
            stop,
            hold_until: None,
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snap.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_cfg(&self, route: &RouteCfg, bins: &RoutesCfg) {
        let mut c = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
        if c.0 != *route || c.1 != *bins {
            *c = (route.clone(), bins.clone());
        }
    }

    pub fn stop(&self, now: bool) {
        self.stop.store(true, Ordering::SeqCst);
        if now {
            if let Some(pid) = self.snapshot().pid {
                crate::platform::kill(pid as i32, crate::platform::SIGTERM);
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop(false);
    }
}
