//! Live map (View ▾ > Live map, Ctrl-a G): the model behind the animated
//! diagram of every running session. A small read only snapshot of the app
//! is taken four times a second; token rates come from a background thread
//! that tails each tab's transcript by file offset; a particle sim moves
//! tokens along the links between frames. None of it runs while the view
//! is closed: the tailer thread stops itself when nobody asks for a while.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

use crate::app::{App, View};

/// `[viz]` in config.toml.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct VizCfg {
    /// Fewer frames (8 a second) and fewer particles in the live map.
    pub reduced_motion: bool,
}

/// Frames a second while the view is open, and with reduced motion.
pub const FPS: u64 = 30;
pub const FPS_REDUCED: u64 = 8;
/// A snapshot of the app this often (positions interpolate in between).
pub const SNAP_EVERY: Duration = Duration::from_millis(250);
/// The tailer thread ends when no snapshot asked for this long.
pub const TAILER_IDLE: Duration = Duration::from_secs(2);
/// Particles at most (and with reduced motion).
pub const MAX_PARTICLES: usize = 700;
pub const MAX_PARTICLES_REDUCED: usize = 140;
/// Seconds the token rate averages over (EMA time constant).
pub const RATE_TAU: f64 = 4.0;

// ---------------------------------------------------------------------
// Deterministic randomness
// ---------------------------------------------------------------------

/// SplitMix64: small, fast and reproducible from a seed.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn f(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.f()
    }
}

/// A stable hash of a number, for per node phases.
pub fn hash01(x: u64) -> f64 {
    Rng::new(x ^ 0x5151_7A7A).f()
}

// ---------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeId {
    Core,
    Brain,
    Account(usize),
    Tab(u64),
    Ghost(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabState {
    Working,
    Waiting,
    Starting,
    Background,
    Idle,
    Suspended,
    Exited,
}

impl TabState {
    pub fn label(self) -> &'static str {
        match self {
            TabState::Working => "working",
            TabState::Waiting => "waiting for approval",
            TabState::Starting => "starting",
            TabState::Background => "background",
            TabState::Idle => "idle",
            TabState::Suspended => "paused (zz)",
            TabState::Exited => "stopped",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabSnap {
    pub uid: u64,
    pub slot: usize,
    pub tab: usize,
    pub account: Option<usize>,
    pub name: String,
    pub folder: String,
    pub state: TabState,
    pub session: Option<String>,
    pub grok: bool,
    pub subagents: usize,
    pub shells: usize,
    pub loops: usize,
    /// Loop fires seen in its transcript so far.
    pub fires: usize,
    /// Session totals when the session list knows them (in, out).
    pub session_tokens: Option<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountSnap {
    pub label: String,
    pub color: (u8, u8, u8),
    pub grok: bool,
    pub logged_in: bool,
    pub five_left: Option<f64>,
    pub week_left: Option<f64>,
    pub eff_left: Option<f64>,
    pub pane: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GhostSnap {
    pub label: String,
    pub grok: bool,
    pub busy: bool,
    pub cwd: String,
    pub place: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub accounts: Vec<AccountSnap>,
    pub tabs: Vec<TabSnap>,
    pub ghosts: Vec<GhostSnap>,
    pub brain_on: bool,
    pub brain_busy: bool,
    pub brain_turns: u32,
    pub brain_cost: Option<f64>,
}

impl Snapshot {
    pub fn count(&self, s: TabState) -> usize {
        self.tabs.iter().filter(|t| t.state == s).count()
    }
}

fn rgb_of(c: ratatui::style::Color) -> (u8, u8, u8) {
    match c {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        _ => (160, 156, 144),
    }
}

// ---------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub x: f64,
    pub y: f64,
    pub r: f64,
    /// The account a tab orbits (its index in the snapshot), for links.
    pub parent: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Placed {
    pub nodes: Vec<Node>,
    pub core: (f64, f64),
    /// The account ring's radii.
    pub ring: (f64, f64),
    pub core_r: f64,
}

impl Placed {
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// How large a tab's satellite is: its context (log scale) and its rate.
pub fn tab_radius(context: u64, rate: f64) -> f64 {
    let c = ((1.0 + context as f64).log10() / 5.3).clamp(0.0, 1.0);
    let r = ((1.0 + rate).log10() / 3.0).clamp(0.0, 1.0);
    1.4 + 2.4 * c + 1.0 * r
}

/// Where everything goes on a canvas `w` by `h` dots (y down), at `zoom`
/// and sim time `t` (satellites orbit). `sizes` gives each tab's radius.
pub fn layout(
    snap: &Snapshot,
    w: f64,
    h: f64,
    zoom: f64,
    t: f64,
    sizes: &HashMap<u64, f64>,
) -> Placed {
    let (cx, cy) = (w / 2.0, h / 2.0);
    let n = snap.accounts.len();
    let zoom = zoom.clamp(0.4, 3.0);
    let (rx, ry) = (w * 0.34 * zoom, h * 0.33 * zoom);
    let core_r = (rx.min(ry) * 0.17).clamp(5.0, 18.0);
    let margin = 3.0;
    let clamp = |x: f64, y: f64| {
        (
            x.clamp(margin, (w - margin).max(margin)),
            y.clamp(margin, (h - margin).max(margin)),
        )
    };
    let mut nodes = vec![Node {
        id: NodeId::Core,
        x: cx,
        y: cy,
        r: core_r,
        parent: None,
    }];
    // The account ring.
    let mut acc_pos = vec![];
    for i in 0..n {
        let (a, k) = if n == 1 {
            (0.0, 0.62)
        } else {
            (
                -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * i as f64 / n as f64,
                1.0,
            )
        };
        let (x, y) = clamp(cx + rx * k * a.cos(), cy + ry * k * a.sin());
        acc_pos.push((x, y, a));
        nodes.push(Node {
            id: NodeId::Account(i),
            x,
            y,
            r: (4.5 * zoom.sqrt()).clamp(3.0, 7.0),
            parent: None,
        });
    }
    // Satellites: room between neighbours on the ring decides the orbit.
    let gap = if n <= 1 {
        rx.min(ry) * 0.9
    } else {
        std::f64::consts::PI * (rx + ry) / n as f64
    };
    let orbit0 = (gap * 0.30).min(ry * 0.42).max(7.0);
    let mut by_acc: Vec<Vec<&TabSnap>> = vec![vec![]; n + 1];
    for tb in &snap.tabs {
        let i = tb.account.filter(|&a| a < n).unwrap_or(n);
        by_acc[i].push(tb);
    }
    for (i, tabs) in by_acc.iter().enumerate() {
        let (ax, ay) = if i < n {
            (acc_pos[i].0, acc_pos[i].1)
        } else {
            (cx, cy)
        };
        let base = if i < n { orbit0 } else { core_r + orbit0 * 0.8 };
        // Rings fill from the inside; each holds what fits at ~6.5 dots
        // apart.
        let mut placed = 0;
        let mut ring = 0;
        while placed < tabs.len() {
            let rr = base * (1.0 + 0.48 * ring as f64);
            let cap = ((std::f64::consts::TAU * rr / 6.5) as usize).max(3);
            let take = cap.min(tabs.len() - placed);
            let dir = if ring % 2 == 0 { 1.0 } else { -1.0 };
            let phase = hash01(i as u64 * 31 + ring as u64) * std::f64::consts::TAU;
            for j in 0..take {
                let tb = tabs[placed + j];
                let a = phase
                    + std::f64::consts::TAU * j as f64 / take as f64
                    + dir * t * 0.14 / (1.0 + ring as f64 * 0.5);
                let (x, y) = clamp(ax + rr * a.cos(), ay + rr * a.sin());
                nodes.push(Node {
                    id: NodeId::Tab(tb.uid),
                    x,
                    y,
                    r: sizes.get(&tb.uid).copied().unwrap_or(2.0),
                    parent: if i < n { Some(i) } else { None },
                });
            }
            placed += take;
            ring += 1;
        }
    }
    // The assistant circles the core.
    if snap.brain_on {
        let br = core_r + (rx.min(ry) * 0.16).max(6.0);
        let a = t * 0.22 + 0.8;
        let (x, y) = clamp(cx + br * a.cos(), cy + br * 0.8 * a.sin());
        nodes.push(Node {
            id: NodeId::Brain,
            x,
            y,
            r: 3.0,
            parent: None,
        });
    }
    // Sessions in other terminals sit outside the ring.
    let g = snap.ghosts.len();
    for k in 0..g {
        let off = if n > 0 {
            std::f64::consts::PI / n as f64
        } else {
            0.0
        };
        let a = -std::f64::consts::FRAC_PI_2 + off + std::f64::consts::TAU * k as f64 / g as f64;
        let (x, y) = clamp(cx + rx * 1.36 * a.cos(), cy + ry * 1.36 * a.sin());
        nodes.push(Node {
            id: NodeId::Ghost(k),
            x,
            y,
            r: 2.5,
            parent: None,
        });
    }
    Placed {
        nodes,
        core: (cx, cy),
        ring: (rx, ry),
        core_r,
    }
}

/// The node under a point (dots): the closest one whose edge is within a
/// small reach, satellites before the larger nodes they sit near.
pub fn hit(placed: &Placed, x: f64, y: f64) -> Option<NodeId> {
    let mut best: Option<(f64, NodeId)> = None;
    for n in &placed.nodes {
        let d = ((n.x - x).powi(2) + (n.y - y).powi(2)).sqrt();
        let reach = n.r + 3.0;
        if d > reach {
            continue;
        }
        // Small things win ties with big things they overlap.
        let score = d - n.r * 0.5
            + match n.id {
                NodeId::Core => 2.0,
                NodeId::Account(_) => 0.5,
                _ => 0.0,
            };
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, n.id));
        }
    }
    best.map(|b| b.1)
}

/// A terminal cell to canvas dots (its center), for an area starting at
/// (ax, ay).
pub fn cell_to_dots(col: u16, row: u16, ax: u16, ay: u16) -> (f64, f64) {
    (
        (col.saturating_sub(ax)) as f64 * 2.0 + 1.0,
        (row.saturating_sub(ay)) as f64 * 4.0 + 2.0,
    )
}

// ---------------------------------------------------------------------
// Particles
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Link {
    Tab(u64),
    Brain,
    Ghost(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Particle {
    pub link: Link,
    /// Output (tab to core) rather than input (core to tab).
    pub out: bool,
    /// Progress along the link, 0..1.
    pub t: f64,
    pub speed: f64,
    /// Sideways offset (dots) so streams look like streams.
    pub wob: f64,
    /// 0..1, picks a shade.
    pub shade: f64,
    pub burst: bool,
}

/// How much flows along one link right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub link: Link,
    /// Tokens a second.
    pub rate_in: f64,
    pub rate_out: f64,
    /// Particles a second regardless of tokens (a working tab looks alive).
    pub base: f64,
}

/// Particles a second for a token rate.
pub fn spawn_rate(rate: f64, base: f64) -> f64 {
    (base + 0.8 * rate.max(0.0).sqrt()).min(26.0)
}

/// Link progress a second for a token rate (a link takes 1 to 3 seconds).
pub fn speed_for(rate: f64) -> f64 {
    (0.34 + 0.16 * (1.0 + rate.max(0.0)).log10()).min(1.0)
}

#[derive(Debug, Clone)]
pub struct Sim {
    pub particles: Vec<Particle>,
    pub cap: usize,
    rng: Rng,
    acc: HashMap<(Link, bool), f64>,
}

impl Sim {
    pub fn new(seed: u64) -> Sim {
        Sim {
            particles: vec![],
            cap: MAX_PARTICLES,
            rng: Rng::new(seed),
            acc: HashMap::new(),
        }
    }

    fn spawn(&mut self, link: Link, out: bool, speed: f64, burst: bool) {
        if self.particles.len() >= self.cap {
            return;
        }
        let jitter = if burst { 0.45 } else { 0.18 };
        let p = Particle {
            link,
            out,
            t: self.rng.range(0.0, 0.04),
            speed: speed * self.rng.range(1.0 - jitter, 1.0 + jitter),
            wob: self.rng.range(-1.6, 1.6),
            shade: self.rng.f(),
            burst,
        };
        self.particles.push(p);
    }

    /// Move everything `dt` seconds and spawn what the flows ask for.
    pub fn step(&mut self, dt: f64, flows: &[Flow]) {
        let dt = dt.clamp(0.0, 0.25);
        for p in &mut self.particles {
            p.t += p.speed * dt;
        }
        self.particles.retain(|p| p.t < 1.0);
        let live: HashSet<(Link, bool)> = flows
            .iter()
            .flat_map(|f| [(f.link, false), (f.link, true)])
            .collect();
        self.acc.retain(|k, _| live.contains(k));
        for f in flows {
            for (out, rate) in [(false, f.rate_in), (true, f.rate_out)] {
                let lambda = spawn_rate(rate, f.base);
                let a = self.acc.entry((f.link, out)).or_insert(0.0);
                *a += lambda * dt;
                let n = *a as usize;
                *a -= n as f64;
                for _ in 0..n {
                    self.spawn(f.link, out, speed_for(rate), false);
                }
            }
        }
    }

    /// A turn finished or tokens arrived: `n` fast particles at once.
    pub fn burst(&mut self, link: Link, out: bool, n: usize) {
        for _ in 0..n {
            let s = self.rng.range(0.7, 1.3);
            self.spawn(link, out, s, true);
        }
    }

    /// Drop the particles of links that are gone.
    pub fn keep_links(&mut self, live: &HashSet<Link>) {
        self.particles.retain(|p| live.contains(&p.link));
    }
}

// ---------------------------------------------------------------------
// Token rates from transcripts
// ---------------------------------------------------------------------

/// One assistant message's usage: new input (with cache writes), output,
/// and the context it had (cache reads included, for size only).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub context: u64,
}

/// The usage of one transcript line, with its message id.
pub fn usage_of_line(line: &str) -> Option<(Option<String>, Usage)> {
    if !line.contains("\"usage\"") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return None;
    }
    let m = v.get("message")?;
    let u = m.get("usage")?;
    let g = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let id = m.get("id").and_then(|x| x.as_str()).map(str::to_string);
    let fresh = g("input_tokens") + g("cache_creation_input_tokens");
    Some((
        id,
        Usage {
            // Never cache reads in the headline: they repeat the context.
            input: fresh,
            output: g("output_tokens"),
            context: fresh + g("cache_read_input_tokens"),
        },
    ))
}

/// Where one transcript was read up to, and what was counted per message
/// id (streaming repeats a message's usage on every content block).
#[derive(Debug, Clone, Default)]
pub struct Cursor {
    pub offset: u64,
    partial: String,
    seen: VecDeque<(String, Usage)>,
    pub primed: bool,
    /// grok: last session totals seen.
    grok_last: Option<(u64, u64)>,
}

/// History read when a transcript is first seen (sizes only, no rate).
pub const PRIME_BYTES: u64 = 256 * 1024;

impl Cursor {
    /// New lines in: the tokens they add (only what was not counted yet)
    /// and the latest context.
    pub fn feed(&mut self, text: &str) -> (Usage, usize) {
        let mut all = std::mem::take(&mut self.partial);
        all.push_str(text);
        let mut add = Usage::default();
        let mut msgs = 0;
        let ends_whole = all.ends_with('\n');
        let mut lines: Vec<&str> = all.split('\n').collect();
        if !ends_whole {
            self.partial = lines.pop().unwrap_or("").to_string();
        }
        for l in lines {
            let Some((id, u)) = usage_of_line(l) else {
                continue;
            };
            add.context = u.context;
            let prev = id
                .as_ref()
                .and_then(|i| self.seen.iter_mut().find(|(s, _)| s == i));
            match prev {
                Some((_, p)) => {
                    add.input += u.input.saturating_sub(p.input);
                    add.output += u.output.saturating_sub(p.output);
                    p.input = p.input.max(u.input);
                    p.output = p.output.max(u.output);
                }
                None => {
                    add.input += u.input;
                    add.output += u.output;
                    msgs += 1;
                    if let Some(i) = id {
                        self.seen.push_back((i, u));
                        if self.seen.len() > 32 {
                            self.seen.pop_front();
                        }
                    }
                }
            }
        }
        (add, msgs)
    }
}

/// Read what a transcript gained since the cursor. The first read only
/// primes (the last PRIME_BYTES give the context size, no rate).
pub fn tail_file(path: &Path, cur: &mut Cursor) -> Option<(Usage, usize)> {
    let len = std::fs::metadata(path).ok()?.len();
    if len < cur.offset {
        // Rewritten: start over.
        *cur = Cursor::default();
    }
    if cur.primed && len == cur.offset {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let start = if cur.primed {
        cur.offset
    } else {
        len.saturating_sub(PRIME_BYTES)
    };
    f.seek(SeekFrom::Start(start)).ok()?;
    // At most 4 MB per read, so one huge write cannot stall the thread.
    let mut buf = Vec::new();
    f.take((len - start).min(4 << 20))
        .read_to_end(&mut buf)
        .ok()?;
    cur.offset = start + buf.len() as u64;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if !cur.primed && start > 0 {
        // Skip the cut line.
        text = text
            .split_once('\n')
            .map(|x| x.1.to_string())
            .unwrap_or_default();
    }
    let (u, n) = cur.feed(&text);
    if !cur.primed {
        cur.primed = true;
        return Some((
            Usage {
                input: 0,
                output: 0,
                context: u.context,
            },
            0,
        ));
    }
    Some((u, n))
}

/// grok keeps running totals in `<session>/usage.json`: the change since
/// the last read.
pub fn tail_grok(dir: &Path, cur: &mut Cursor) -> Option<(Usage, usize)> {
    let t = std::fs::read_to_string(dir.join("usage.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&t).ok()?;
    let g = |k: &str| {
        v.pointer(&format!("/session/{k}"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    };
    let tin = g("inputTokens") + g("cacheCreationTokens");
    let tout = g("outputTokens");
    let out = match cur.grok_last {
        Some((a, b)) if cur.primed => Usage {
            input: tin.saturating_sub(a),
            output: tout.saturating_sub(b),
            context: 0,
        },
        _ => Usage::default(),
    };
    cur.grok_last = Some((tin, tout));
    cur.primed = true;
    Some((out, usize::from(out.output > 0)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct TailJob {
    pub uid: u64,
    pub path: PathBuf,
    pub grok: bool,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TailMsg {
    Tokens {
        uid: u64,
        usage: Usage,
        turns: usize,
    },
    /// Running subagents, shells and other tasks under a tab.
    Background {
        uid: u64,
        subs: usize,
        shells: usize,
    },
    Ghosts(Vec<GhostSnap>),
}

pub struct TailReq {
    pub jobs: Vec<TailJob>,
    pub homes: Vec<(crate::harness::Harness, PathBuf)>,
    pub ours: HashSet<u32>,
}

pub struct Tailer {
    pub tx: Sender<TailReq>,
    pub rx: Receiver<TailMsg>,
}

/// The background worker: tails transcripts, counts what runs under each
/// tab, finds sessions in other terminals. It ends on its own once no
/// request came for TAILER_IDLE (the view closed).
pub fn spawn_tailer() -> Option<Tailer> {
    let (tx, req_rx) = mpsc::channel::<TailReq>();
    let (msg_tx, rx) = mpsc::channel::<TailMsg>();
    std::thread::Builder::new()
        .name("godterm-livemap".into())
        .spawn(move || tailer_loop(req_rx, msg_tx))
        .ok()?;
    Some(Tailer { tx, rx })
}

fn tailer_loop(req_rx: Receiver<TailReq>, tx: Sender<TailMsg>) {
    let mut req: Option<TailReq> = None;
    let mut last_req = Instant::now();
    let mut cursors: HashMap<(u64, PathBuf), Cursor> = HashMap::new();
    let mut last_bg: Option<Instant> = None;
    let mut last_ghosts: Option<Instant> = None;
    loop {
        match req_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(r) => {
                req = Some(r);
                last_req = Instant::now();
                // Only the newest request matters.
                while let Ok(r) = req_rx.try_recv() {
                    req = Some(r);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if last_req.elapsed() > TAILER_IDLE {
            return;
        }
        let Some(r) = req.as_ref() else { continue };
        let live: HashSet<(u64, PathBuf)> =
            r.jobs.iter().map(|j| (j.uid, j.path.clone())).collect();
        cursors.retain(|k, _| live.contains(k));
        for j in &r.jobs {
            let cur = cursors.entry((j.uid, j.path.clone())).or_default();
            let got = if j.grok {
                tail_grok(&j.path, cur)
            } else {
                tail_file(&j.path, cur)
            };
            if let Some((usage, turns)) = got {
                if tx
                    .send(TailMsg::Tokens {
                        uid: j.uid,
                        usage,
                        turns,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
        if last_bg.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            last_bg = Some(Instant::now());
            let table = if cfg!(test) {
                vec![]
            } else {
                crate::activity::process_table()
            };
            let now = SystemTime::now();
            for j in &r.jobs {
                let subs = if j.grok {
                    0
                } else {
                    crate::activity::running_subagents(&j.path, now)
                };
                let children = j
                    .pid
                    .map(|p| {
                        crate::activity::descendants(&table, p)
                            .into_iter()
                            .map(|c| (c.pid, c.cmd.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let a = crate::activity::classify(false, false, None, 0, children, 0);
                let _ = tx.send(TailMsg::Background {
                    uid: j.uid,
                    subs,
                    shells: a.background_shells + a.background_tasks,
                });
            }
        }
        if last_ghosts.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            last_ghosts = Some(Instant::now());
            let found = if cfg!(test) {
                vec![]
            } else {
                crate::takeover::find(&r.homes, &r.ours)
            };
            let ghosts = found
                .into_iter()
                .take(12)
                .map(|l| GhostSnap {
                    label: l
                        .name
                        .clone()
                        .or_else(|| l.cwd.file_name().map(|f| f.to_string_lossy().into_owned()))
                        .unwrap_or_else(|| l.harness.name().to_string()),
                    grok: l.harness == crate::harness::Harness::Grok,
                    busy: l.status == "busy",
                    cwd: crate::config::tilde(&l.cwd),
                    place: l.describe(),
                })
                .collect();
            let _ = tx.send(TailMsg::Ghosts(ghosts));
        }
    }
}

// ---------------------------------------------------------------------
// The view's state
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rate {
    pub ema_in: f64,
    pub ema_out: f64,
    pend_in: f64,
    pend_out: f64,
    pub context: u64,
    /// Since the view opened.
    pub seen_in: u64,
    pub seen_out: u64,
    /// Sim time of the last tokens.
    pub last: f64,
}

impl Rate {
    /// Fold what arrived over the last `dt` seconds into the averages.
    pub fn update(&mut self, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        let a = 1.0 - (-dt / RATE_TAU).exp();
        self.ema_in += a * (self.pend_in / dt - self.ema_in);
        self.ema_out += a * (self.pend_out / dt - self.ema_out);
        self.pend_in = 0.0;
        self.pend_out = 0.0;
        if self.ema_in < 0.05 {
            self.ema_in = 0.0;
        }
        if self.ema_out < 0.05 {
            self.ema_out = 0.0;
        }
    }
    pub fn add(&mut self, u: Usage) {
        self.pend_in += u.input as f64;
        self.pend_out += u.output as f64;
        self.seen_in += u.input;
        self.seen_out += u.output;
        if u.context > 0 {
            self.context = u.context;
        }
    }
    pub fn total(&self) -> f64 {
        self.ema_in + self.ema_out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TickKind {
    Opened,
    Closed,
    Turn,
    Approval,
    Loop,
    Brain,
    Tokens,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub at: f64,
    pub kind: TickKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ripple {
    pub node: NodeId,
    pub born: f64,
    pub color: (u8, u8, u8),
    pub reach: f64,
}

pub struct LiveMap {
    pub paused: bool,
    pub labels: bool,
    pub zoom: f64,
    /// Sim time (stops while paused).
    pub t: f64,
    last_frame: Option<Instant>,
    snap_at: Option<Instant>,
    pub snap: Snapshot,
    pub sim: Sim,
    pub ripples: Vec<Ripple>,
    pub ticker: VecDeque<Tick>,
    pub rates: HashMap<u64, Rate>,
    /// Each tab's drawn size, eased toward its target.
    pub sizes: HashMap<u64, f64>,
    /// Tokens a second, the last 60 seconds: (in, out).
    pub hist: VecDeque<(f64, f64)>,
    bucket: (f64, f64, f64),
    paths: HashMap<u64, (Option<String>, Option<PathBuf>, Instant)>,
    background: HashMap<u64, (usize, usize)>,
    ghosts: Vec<GhostSnap>,
    tailer: Option<Tailer>,
    /// Where the last frame put things (for the mouse).
    pub placed: Placed,
    pub canvas: ratatui::layout::Rect,
    /// Milliseconds a frame takes to build (EMA), and frames drawn.
    pub render_ms: f64,
    pub frames: u64,
    prev: HashMap<u64, (TabState, usize, String)>,
    prev_brain: Option<u32>,
    primed: bool,
}

impl Default for LiveMap {
    fn default() -> LiveMap {
        LiveMap {
            paused: false,
            labels: true,
            zoom: 1.0,
            t: 0.0,
            last_frame: None,
            snap_at: None,
            snap: Snapshot::default(),
            sim: Sim::new(0x60D7_E2A1),
            ripples: vec![],
            ticker: VecDeque::new(),
            rates: HashMap::new(),
            sizes: HashMap::new(),
            hist: VecDeque::new(),
            bucket: (0.0, 0.0, 0.0),
            paths: HashMap::new(),
            background: HashMap::new(),
            ghosts: vec![],
            tailer: None,
            placed: Placed::default(),
            canvas: Default::default(),
            render_ms: 0.0,
            frames: 0,
            prev: HashMap::new(),
            prev_brain: None,
            primed: false,
        }
    }
}

impl LiveMap {
    /// The tailer thread is running (for tests: closed costs nothing).
    #[cfg(test)]
    pub fn tailer_alive(&self) -> bool {
        self.tailer
            .as_ref()
            .is_some_and(|t| !matches!(t.rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)))
    }

    pub fn push_tick(&mut self, kind: TickKind, text: String) {
        self.ticker.push_back(Tick {
            at: self.t,
            kind,
            text,
        });
        while self.ticker.len() > 40 {
            self.ticker.pop_front();
        }
    }

    fn ripple(&mut self, node: NodeId, color: (u8, u8, u8), reach: f64) {
        if self.ripples.len() < 48 {
            self.ripples.push(Ripple {
                node,
                born: self.t,
                color,
                reach,
            });
        }
    }

    /// Fold a fresh snapshot in: events for the ticker, bursts and ripples
    /// for what changed.
    pub fn apply(&mut self, snap: Snapshot) {
        let first = !self.primed;
        self.primed = true;
        let mut now_prev = HashMap::new();
        for tb in &snap.tabs {
            now_prev.insert(tb.uid, (tb.state, tb.fires, tb.name.clone()));
            let Some((ps, pf, _)) = self.prev.get(&tb.uid).cloned() else {
                if !first {
                    self.push_tick(TickKind::Opened, format!("tab opened: {}", tb.name));
                    self.ripple(NodeId::Tab(tb.uid), (120, 255, 170), 9.0);
                }
                continue;
            };
            if ps == TabState::Working && tb.state != TabState::Working {
                let r = self.rates.get(&tb.uid).cloned().unwrap_or_default();
                self.push_tick(
                    TickKind::Turn,
                    if r.seen_out > 0 {
                        format!(
                            "turn finished: {} ({} out)",
                            tb.name,
                            human(r.seen_out as f64)
                        )
                    } else {
                        format!("turn finished: {}", tb.name)
                    },
                );
                self.sim.burst(Link::Tab(tb.uid), true, 14);
                self.ripple(NodeId::Tab(tb.uid), (255, 200, 60), 12.0);
                self.ripple(NodeId::Core, (255, 170, 60), 16.0);
            }
            if tb.state == TabState::Waiting && ps != TabState::Waiting {
                self.push_tick(TickKind::Approval, format!("approval needed: {}", tb.name));
                self.ripple(NodeId::Tab(tb.uid), (255, 60, 200), 14.0);
            }
            if tb.fires > pf {
                self.push_tick(TickKind::Loop, format!("loop fired: {}", tb.name));
                self.ripple(NodeId::Tab(tb.uid), (120, 200, 255), 12.0);
            }
            if tb.state == TabState::Working && ps != TabState::Working {
                self.sim.burst(Link::Tab(tb.uid), false, 8);
            }
        }
        if !first {
            let gone: Vec<String> = self
                .prev
                .iter()
                .filter(|(uid, _)| !now_prev.contains_key(uid))
                .map(|(_, (_, _, name))| name.clone())
                .collect();
            for n in gone {
                self.push_tick(TickKind::Closed, format!("tab closed: {n}"));
            }
        }
        if let Some(p) = self.prev_brain {
            if snap.brain_turns > p {
                self.push_tick(TickKind::Brain, "assistant answered".into());
                self.sim.burst(Link::Brain, true, 10);
                self.ripple(NodeId::Brain, (190, 130, 255), 10.0);
            }
        }
        self.prev_brain = Some(snap.brain_turns);
        self.prev = now_prev;
        let live: HashSet<u64> = snap.tabs.iter().map(|t| t.uid).collect();
        self.rates.retain(|k, _| live.contains(k));
        self.sizes.retain(|k, _| live.contains(k));
        let mut links: HashSet<Link> = live.iter().map(|u| Link::Tab(*u)).collect();
        links.insert(Link::Brain);
        links.extend((0..snap.ghosts.len()).map(Link::Ghost));
        self.sim.keep_links(&links);
        self.snap = snap;
    }

    /// Messages from the tailer.
    pub fn drain(&mut self) {
        let Some(tl) = &self.tailer else { return };
        let msgs: Vec<TailMsg> = tl.rx.try_iter().collect();
        for m in msgs {
            self.on_msg(m);
        }
    }

    pub fn on_msg(&mut self, m: TailMsg) {
        match m {
            TailMsg::Tokens { uid, usage, turns } => {
                let t = self.t;
                let r = self.rates.entry(uid).or_default();
                r.add(usage);
                if usage.input + usage.output > 0 {
                    r.last = t;
                }
                self.bucket.1 += usage.input as f64;
                self.bucket.2 += usage.output as f64;
                if usage.output > 0 || usage.input > 0 {
                    let n_in = ((usage.input as f64).max(1.0).log2() - 7.0).clamp(0.0, 10.0);
                    let n_out = ((usage.output as f64).max(1.0).log2() - 5.0).clamp(0.0, 12.0);
                    self.sim.burst(Link::Tab(uid), false, n_in as usize);
                    self.sim.burst(Link::Tab(uid), true, n_out as usize);
                }
                if turns > 0 && usage.output >= 2000 {
                    let name = self
                        .snap
                        .tabs
                        .iter()
                        .find(|x| x.uid == uid)
                        .map(|x| x.name.clone())
                        .unwrap_or_default();
                    self.push_tick(
                        TickKind::Tokens,
                        format!("{name}: +{} out", human(usage.output as f64)),
                    );
                }
            }
            TailMsg::Background { uid, subs, shells } => {
                self.background.insert(uid, (subs, shells));
            }
            TailMsg::Ghosts(g) => self.ghosts = g,
        }
    }

    /// Tokens a second over all tabs: (in, out).
    pub fn totals(&self) -> (f64, f64) {
        self.rates
            .values()
            .fold((0.0, 0.0), |a, r| (a.0 + r.ema_in, a.1 + r.ema_out))
    }

    /// What flows where right now, for the sim.
    pub fn flows(&self) -> Vec<Flow> {
        let mut v = vec![];
        for tb in &self.snap.tabs {
            let r = self.rates.get(&tb.uid).cloned().unwrap_or_default();
            let base = match tb.state {
                TabState::Working => 2.2,
                TabState::Waiting => 0.25,
                TabState::Background => 0.5,
                TabState::Starting => 0.6,
                _ => 0.0,
            };
            if base > 0.0 || r.total() > 0.0 {
                v.push(Flow {
                    link: Link::Tab(tb.uid),
                    rate_in: r.ema_in,
                    rate_out: r.ema_out,
                    base,
                });
            }
        }
        if self.snap.brain_on && self.snap.brain_busy {
            v.push(Flow {
                link: Link::Brain,
                rate_in: 0.0,
                rate_out: 0.0,
                base: 4.0,
            });
        }
        for (i, g) in self.snap.ghosts.iter().enumerate() {
            if g.busy {
                v.push(Flow {
                    link: Link::Ghost(i),
                    rate_in: 0.0,
                    rate_out: 0.0,
                    base: 0.7,
                });
            }
        }
        v
    }

    /// Advance to `now`: snapshot when due, rates, sizes, particles.
    /// `reduced` lowers the particle budget.
    pub fn advance(&mut self, now: Instant, reduced: bool) -> bool {
        let dt = self
            .last_frame
            .map(|l| now.saturating_duration_since(l).as_secs_f64())
            .unwrap_or(0.0)
            .min(0.25);
        self.last_frame = Some(now);
        self.sim.cap = if reduced {
            MAX_PARTICLES_REDUCED
        } else {
            MAX_PARTICLES
        };
        if self.sim.particles.len() > self.sim.cap {
            self.sim.particles.truncate(self.sim.cap);
        }
        let sdt = if self.paused { 0.0 } else { dt };
        self.t += sdt;
        // Per second token buckets for the sparklines.
        self.bucket.0 += dt;
        if self.bucket.0 >= 1.0 {
            self.hist.push_back((self.bucket.1, self.bucket.2));
            while self.hist.len() > 60 {
                self.hist.pop_front();
            }
            self.bucket = (0.0, 0.0, 0.0);
        }
        // Sizes ease toward their targets.
        let k = 1.0 - (-sdt * 3.0).exp();
        for tb in &self.snap.tabs {
            let r = self.rates.get(&tb.uid).cloned().unwrap_or_default();
            let target = tab_radius(r.context, r.total());
            let s = self.sizes.entry(tb.uid).or_insert(target);
            *s += (target - *s) * k;
        }
        self.ripples.retain(|r| self.t - r.born < 1.6);
        let flows = self.flows();
        self.sim.step(sdt, &flows);
        let due = self
            .snap_at
            .is_none_or(|t| now.duration_since(t) >= SNAP_EVERY);
        if due {
            let dts = self
                .snap_at
                .map(|t| now.duration_since(t).as_secs_f64())
                .unwrap_or(0.0);
            self.snap_at = Some(now);
            for r in self.rates.values_mut() {
                r.update(dts);
            }
        }
        due
    }
}

/// 1234 -> "1.2k".
pub fn human(n: f64) -> String {
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e4 {
        format!("{:.0}k", n / 1e3)
    } else if n >= 1e3 {
        format!("{:.1}k", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

// ---------------------------------------------------------------------
// The app side
// ---------------------------------------------------------------------

impl App {
    /// Reduced motion: the setting, or the memory saver.
    pub fn livemap_reduced(&self) -> bool {
        self.cfg.viz.reduced_motion || self.memory_saver_on()
    }

    /// How often the screen must redraw for an animation, if any is on
    /// screen: the live map (30 fps, 8 with reduced motion, fewer if
    /// frames are slow) or the voice meter.
    pub fn anim_interval(&self) -> Option<Duration> {
        if self.view == View::LiveMap {
            let fps = if self.livemap_reduced() {
                FPS_REDUCED
            } else if self.livemap.render_ms > 20.0 {
                10
            } else if self.livemap.render_ms > 10.0 {
                20
            } else {
                FPS
            };
            return Some(Duration::from_millis(1000 / fps));
        }
        if self.meter_live() {
            return Some(Duration::from_millis(40));
        }
        // A spinner on screen (a visible tab working, waiting or running
        // background jobs): 8 frames a second; nothing when all is idle.
        self.spinner_on_screen()
            .then_some(Duration::from_millis(125))
    }

    /// Some pane on screen has a working, waiting or background tab.
    pub fn spinner_on_screen(&self) -> bool {
        if !matches!(self.view, View::Grid | View::Overview) {
            return false;
        }
        let bg = self.bg_cache.borrow();
        self.panes.iter().enumerate().any(|(i, p)| {
            self.pane_rects.get(i).is_some_and(|r| r.width > 0)
                && p.tabs.iter().any(|t| {
                    matches!(
                        t.activity,
                        crate::pane::Activity::Working | crate::pane::Activity::Permission
                    ) || bg.get(&t.uid).is_some_and(|(_, v)| v.is_some())
                })
        })
    }

    pub fn open_livemap(&mut self) {
        if self.view == View::LiveMap {
            self.go_home();
            return;
        }
        self.modal = crate::app::Modal::None;
        self.view = View::LiveMap;
        self.livemap.paused = false;
        self.livemap.last_frame = None;
        self.livemap.snap_at = None;
    }

    pub(crate) fn on_livemap_key(&mut self, k: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let lm = &mut self.livemap;
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.go_home(),
            KeyCode::Char(' ') => {
                lm.paused = !lm.paused;
                let p = lm.paused;
                self.flash(if p {
                    "Live map paused (space resumes)"
                } else {
                    "Live map running"
                });
            }
            KeyCode::Char('l') => lm.labels = !lm.labels,
            KeyCode::Char('+') | KeyCode::Char('=') => lm.zoom = (lm.zoom * 1.12).min(2.6),
            KeyCode::Char('-') | KeyCode::Char('_') => lm.zoom = (lm.zoom / 1.12).max(0.5),
            KeyCode::Char('0') => lm.zoom = 1.0,
            _ => {}
        }
    }

    /// Mouse on the live map: hover is read at draw time; a click on a
    /// tab opens it, on an account focuses its pane; the wheel zooms.
    /// Returns true when it took the event.
    pub fn livemap_mouse(&mut self, m: &crossterm::event::MouseEvent) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        if self.view != View::LiveMap || self.modal != crate::app::Modal::None {
            return false;
        }
        let c = self.livemap.canvas;
        if !crate::hits::contains(c, m.column, m.row) {
            return false;
        }
        match m.kind {
            MouseEventKind::ScrollUp => {
                self.livemap.zoom = (self.livemap.zoom * 1.08).min(2.6);
                true
            }
            MouseEventKind::ScrollDown => {
                self.livemap.zoom = (self.livemap.zoom / 1.08).max(0.5);
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let (x, y) = cell_to_dots(m.column, m.row, c.x, c.y);
                match hit(&self.livemap.placed, x, y) {
                    Some(NodeId::Tab(uid)) => {
                        if let Some((s, t)) = self.livemap_find(uid) {
                            self.jump_to(s, t);
                        }
                    }
                    Some(NodeId::Account(a)) => {
                        match self.panes.iter().position(|p| p.account == Some(a)) {
                            Some(p) => {
                                self.panes[p].hidden = false;
                                self.focus = p;
                                self.go_home();
                                self.focus = p;
                            }
                            None => self.flash("No pane shows this account (Ctrl-a a)"),
                        }
                    }
                    Some(NodeId::Brain) => self.toggle_assistant_panel(),
                    _ => {}
                }
                true
            }
            MouseEventKind::Moved => true,
            _ => false,
        }
    }

    /// (pane, tab) of a tab id.
    pub fn livemap_find(&self, uid: u64) -> Option<(usize, usize)> {
        self.panes
            .iter()
            .enumerate()
            .find_map(|(s, p)| p.tabs.iter().position(|t| t.uid == uid).map(|t| (s, t)))
    }

    /// Where a tab's transcript is (claude: the jsonl; grok: its folder).
    fn livemap_path(&self, s: usize, t: usize) -> Option<PathBuf> {
        let a = self.panes[s].account?;
        let acc = self.cfg.accounts.get(a)?;
        if acc.harness() == crate::harness::Harness::Grok {
            let id = self.panes[s].tabs[t].session_id.clone()?;
            return std::fs::read_dir(acc.config_dir().join("sessions"))
                .ok()?
                .flatten()
                .map(|p| p.path().join(&id))
                .find(|p| p.join("summary.json").is_file() || p.join("usage.json").is_file());
        }
        self.transcript_path(s, t)
    }

    /// The read only picture of the app the map draws.
    pub fn livemap_snapshot(&mut self) -> Snapshot {
        let now = Instant::now();
        let mut fires: HashMap<u64, (usize, usize)> = HashMap::new();
        for r in &self.loops {
            let e = fires.entry(r.uid).or_default();
            e.0 += 1;
            e.1 += r.lp.fires;
        }
        let mut tabs = vec![];
        let mut jobs = vec![];
        for (s, slot) in self.panes.iter().enumerate() {
            let labels = crate::slot::tab_labels(slot);
            let grok = slot
                .account
                .and_then(|a| self.cfg.accounts.get(a))
                .is_some_and(|a| a.harness() == crate::harness::Harness::Grok);
            for (t, tab) in slot.tabs.iter().enumerate() {
                let (subs, shells) = self
                    .livemap
                    .background
                    .get(&tab.uid)
                    .copied()
                    .unwrap_or((0, 0));
                let recent = self
                    .livemap
                    .rates
                    .get(&tab.uid)
                    .is_some_and(|r| r.last > 0.0 && self.livemap.t - r.last < 3.0);
                use crate::pane::{Activity as A, PaneState as P};
                let state = if tab.suspended {
                    TabState::Suspended
                } else {
                    match &tab.state {
                        P::Running => match tab.activity {
                            A::Working => TabState::Working,
                            A::Permission => TabState::Waiting,
                            A::Starting => TabState::Starting,
                            _ if recent => TabState::Working,
                            _ if subs + shells > 0 => TabState::Background,
                            _ => TabState::Idle,
                        },
                        P::Idle => TabState::Suspended,
                        _ => TabState::Exited,
                    }
                };
                let (loops, fired) = fires.get(&tab.uid).copied().unwrap_or((0, 0));
                let session_tokens = tab.session_id.as_ref().and_then(|id| {
                    let a = slot.account?;
                    self.accounts
                        .get(a)?
                        .sessions
                        .iter()
                        .find(|x| &x.id == id)
                        .map(|x| (x.tokens.input + x.tokens.cache_creation, x.tokens.output))
                });
                tabs.push(TabSnap {
                    uid: tab.uid,
                    slot: s,
                    tab: t,
                    account: slot.account,
                    name: labels.get(t).cloned().unwrap_or_else(|| tab.name()),
                    folder: crate::config::tilde(tab.live_cwd.as_ref().unwrap_or(&tab.cwd)),
                    state,
                    session: tab.session_id.clone(),
                    grok,
                    subagents: subs,
                    shells,
                    loops,
                    fires: fired,
                    session_tokens,
                });
                // Transcript paths are looked up every few seconds, not
                // every snapshot.
                let stale = match self.livemap.paths.get(&tab.uid) {
                    Some((sid, p, at)) => {
                        *sid != tab.session_id
                            || (p.is_none() && at.elapsed() > Duration::from_secs(3))
                    }
                    None => true,
                };
                if stale {
                    let p = self.livemap_path(s, t);
                    self.livemap
                        .paths
                        .insert(tab.uid, (tab.session_id.clone(), p, now));
                }
                if let Some((_, Some(p), _)) = self.livemap.paths.get(&tab.uid) {
                    if !tab.suspended && tab.is_running() {
                        jobs.push(TailJob {
                            uid: tab.uid,
                            path: p.clone(),
                            grok,
                            pid: tab.pid(),
                        });
                    }
                }
            }
        }
        let live: HashSet<u64> = tabs.iter().map(|t| t.uid).collect();
        self.livemap.paths.retain(|k, _| live.contains(k));
        self.livemap.background.retain(|k, _| live.contains(k));
        let accounts = self
            .cfg
            .accounts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let st = self.accounts.get(i);
                let u = st.and_then(|s| s.usage.as_ref());
                let mut week = u.and_then(|u| u.get("seven_day")).map(|w| w.left_now());
                if week.is_none() {
                    week = st.and_then(|s| s.grok_screen.as_ref()).map(|g| g.1);
                }
                AccountSnap {
                    label: if a.label.trim().is_empty() {
                        a.name.clone()
                    } else {
                        a.label.clone()
                    },
                    color: rgb_of(crate::theme::parse_color(&a.color)),
                    grok: a.harness() == crate::harness::Harness::Grok,
                    logged_in: st.is_some_and(|s| s.login.logged_in()),
                    five_left: st.and_then(|s| s.five_hour_left()),
                    week_left: week,
                    eff_left: st.and_then(|s| s.effective_left()).or(week),
                    pane: self.panes.iter().position(|p| p.account == Some(i)),
                }
            })
            .collect();
        // Ask the tailer for the next round (and start it if needed).
        let req = TailReq {
            jobs,
            homes: self.agent_homes(),
            ours: self.own_pids(),
        };
        let sent = self
            .livemap
            .tailer
            .as_ref()
            .is_some_and(|t| t.tx.send(req).is_ok());
        if !sent {
            self.livemap.tailer = spawn_tailer();
            if let Some(t) = &self.livemap.tailer {
                let req = TailReq {
                    jobs: self
                        .livemap
                        .paths
                        .iter()
                        .filter_map(|(uid, (_, p, _))| {
                            let (s, t) = self.livemap_find(*uid)?;
                            let tab = &self.panes[s].tabs[t];
                            (tab.is_running() && !tab.suspended).then_some(TailJob {
                                uid: *uid,
                                path: p.clone()?,
                                grok: tabs.iter().any(|x| x.uid == *uid && x.grok),
                                pid: tab.pid(),
                            })
                        })
                        .collect(),
                    homes: self.agent_homes(),
                    ours: self.own_pids(),
                };
                let _ = t.tx.send(req);
            }
        }
        let a = &self.assistant;
        Snapshot {
            accounts,
            tabs,
            ghosts: self.livemap.ghosts.clone(),
            brain_on: a.brain.is_some() || a.busy,
            brain_busy: a.busy,
            brain_turns: a.turns_in_brain,
            brain_cost: a.last_cost,
        }
    }

    /// One frame's model work: drain the tailer, snapshot when due, move
    /// the sim. Called by the view's draw only.
    pub fn livemap_frame(&mut self) {
        let reduced = self.livemap_reduced();
        self.livemap.drain();
        if self.livemap.advance(Instant::now(), reduced) {
            let s = self.livemap_snapshot();
            self.livemap.apply(s);
        }
        // A tailer whose thread ended (idle) is dropped so the next
        // snapshot starts a fresh one.
        if self
            .livemap
            .tailer
            .as_ref()
            .is_some_and(|t| matches!(t.rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)))
        {
            self.livemap.tailer = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(accounts: usize, tabs: usize) -> Snapshot {
        let mut s = Snapshot {
            brain_on: true,
            ..Default::default()
        };
        for i in 0..accounts {
            s.accounts.push(AccountSnap {
                label: format!("acct{i}"),
                color: (138, 160, 128),
                grok: i % 3 == 2,
                logged_in: true,
                five_left: Some(70.0),
                week_left: Some(40.0),
                eff_left: Some(40.0),
                pane: Some(i),
            });
        }
        for j in 0..tabs {
            s.tabs.push(TabSnap {
                uid: 100 + j as u64,
                slot: j % accounts.max(1),
                tab: j / accounts.max(1),
                account: (accounts > 0).then_some(j % accounts.max(1)),
                name: format!("tab{j}"),
                folder: "~/x".into(),
                state: TabState::Idle,
                session: None,
                grok: false,
                subagents: 0,
                shells: 0,
                loops: 0,
                fires: 0,
                session_tokens: None,
            });
        }
        s
    }

    #[test]
    fn particle_sim_is_deterministic_with_a_seed() {
        let flows = vec![
            Flow {
                link: Link::Tab(1),
                rate_in: 400.0,
                rate_out: 90.0,
                base: 2.0,
            },
            Flow {
                link: Link::Brain,
                rate_in: 0.0,
                rate_out: 0.0,
                base: 4.0,
            },
        ];
        let run = || {
            let mut s = Sim::new(42);
            for i in 0..200 {
                s.step(1.0 / 30.0, &flows);
                if i == 50 {
                    s.burst(Link::Tab(1), true, 12);
                }
            }
            s.particles
        };
        let a = run();
        assert_eq!(a, run());
        assert!(!a.is_empty());
        assert!(a.iter().all(|p| (0.0..1.0).contains(&p.t)));
        assert!(a.iter().any(|p| p.out) && a.iter().any(|p| !p.out));
        // A different seed gives a different picture.
        let mut b = Sim::new(43);
        for _ in 0..200 {
            b.step(1.0 / 30.0, &flows);
        }
        assert_ne!(a, b.particles);
    }

    #[test]
    fn particles_follow_the_rate_and_respect_the_cap() {
        let mk = |rate: f64| {
            let mut s = Sim::new(7);
            let f = vec![Flow {
                link: Link::Tab(1),
                rate_in: rate,
                rate_out: 0.0,
                base: 0.0,
            }];
            let mut spawned = 0usize;
            let mut last = 0;
            for _ in 0..300 {
                s.step(0.01, &f);
                spawned += s.particles.len().saturating_sub(last);
                last = s.particles.len();
            }
            spawned
        };
        assert_eq!(mk(0.0), 0, "no tokens, no base: nothing flows");
        assert!(mk(2000.0) > mk(50.0) * 2, "more tokens, denser stream");
        assert!(speed_for(5000.0) > speed_for(10.0));
        let mut s = Sim::new(1);
        s.cap = 50;
        let f: Vec<Flow> = (0..40)
            .map(|i| Flow {
                link: Link::Tab(i),
                rate_in: 1e6,
                rate_out: 1e6,
                base: 5.0,
            })
            .collect();
        for _ in 0..100 {
            s.step(0.05, &f);
            s.burst(Link::Tab(0), true, 30);
        }
        assert!(s.particles.len() <= 50);
        // Links that went away lose their particles.
        s.keep_links(&HashSet::from([Link::Tab(0)]));
        assert!(s.particles.iter().all(|p| p.link == Link::Tab(0)));
    }

    #[test]
    fn layout_fits_1_to_12_accounts_and_40_tabs() {
        for (w, h) in [(400.0, 180.0), (160.0, 80.0), (240.0, 100.0)] {
            for n in 1..=12 {
                for tabs in [0, 1, 5, 17, 40] {
                    let s = snap(n, tabs);
                    for t in [0.0, 13.7] {
                        let p = layout(&s, w, h, 1.0, t, &HashMap::new());
                        // Core, accounts, every tab, the brain.
                        assert_eq!(p.nodes.len(), 1 + n + tabs + 1, "{n} accounts {tabs} tabs");
                        for nd in &p.nodes {
                            assert!(
                                nd.x >= 0.0 && nd.x <= w && nd.y >= 0.0 && nd.y <= h,
                                "{nd:?} outside {w}x{h}"
                            );
                        }
                        // Accounts keep apart from each other and the core.
                        let acc: Vec<&Node> = p
                            .nodes
                            .iter()
                            .filter(|x| matches!(x.id, NodeId::Account(_)))
                            .collect();
                        for (i, a) in acc.iter().enumerate() {
                            let dc = ((a.x - p.core.0).powi(2) + (a.y - p.core.1).powi(2)).sqrt();
                            assert!(dc > p.core_r + a.r, "account on the core");
                            for b in &acc[i + 1..] {
                                let d = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                                assert!(d > a.r + b.r, "{n} accounts overlap at {w}x{h}");
                            }
                        }
                        // Each tab sits nearer its own account than the core does.
                        for nd in &p.nodes {
                            if let (NodeId::Tab(_), Some(a)) = (nd.id, nd.parent) {
                                let an = p.get(NodeId::Account(a)).unwrap();
                                let d = ((nd.x - an.x).powi(2) + (nd.y - an.y).powi(2)).sqrt();
                                assert!(
                                    d < p.ring.0.max(p.ring.1) * 1.2,
                                    "tab far from its account"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hit_testing_finds_the_node_under_the_mouse() {
        let s = snap(4, 12);
        let p = layout(&s, 400.0, 180.0, 1.0, 0.0, &HashMap::new());
        for nd in &p.nodes {
            assert_eq!(hit(&p, nd.x, nd.y), Some(nd.id), "center of {:?}", nd.id);
        }
        // Empty space: nothing.
        assert_eq!(hit(&p, 1.0, 1.0), None);
        // A point just off a tab still finds it.
        let tab = p
            .nodes
            .iter()
            .find(|n| matches!(n.id, NodeId::Tab(_)))
            .unwrap();
        assert_eq!(hit(&p, tab.x + tab.r + 1.0, tab.y), Some(tab.id));
        // Cells map to dot centers.
        assert_eq!(cell_to_dots(12, 7, 2, 1), (21.0, 26.0));
    }

    fn line(id: &str, inp: u64, cw: u64, cr: u64, out: u64) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"id\":\"{id}\",\"content\":[],\"usage\":{{\"input_tokens\":{inp},\"cache_creation_input_tokens\":{cw},\"cache_read_input_tokens\":{cr},\"output_tokens\":{out}}}}}}}\n"
        )
    }

    #[test]
    fn token_rates_come_from_transcripts_incrementally() {
        let d = std::env::temp_dir().join(format!("godterm-livemap-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("s.jsonl");
        // History before the view opened: sizes only.
        std::fs::write(&f, line("m0", 10, 500, 90_000, 40)).unwrap();
        let mut c = Cursor::default();
        let (u, n) = tail_file(&f, &mut c).unwrap();
        assert_eq!((u.input, u.output, n), (0, 0, 0));
        assert_eq!(u.context, 90_510, "context counts cache reads");
        assert!(tail_file(&f, &mut c).is_none(), "nothing new, no read");
        // New turns: streaming repeats an id; cache reads stay out.
        let mut more = String::new();
        more.push_str(&line("m1", 5, 1000, 100_000, 10));
        more.push_str(&line("m1", 5, 1000, 100_000, 300));
        more.push_str("{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n");
        more.push_str(&line("m2", 3, 0, 101_000, 50));
        let mut fh = std::fs::OpenOptions::new().append(true).open(&f).unwrap();
        use std::io::Write;
        // Half a line first: nothing is counted until it ends.
        let (a, b) = more.split_at(more.len() - 20);
        fh.write_all(a.as_bytes()).unwrap();
        let (u1, n1) = tail_file(&f, &mut c).unwrap();
        fh.write_all(b.as_bytes()).unwrap();
        let (u2, n2) = tail_file(&f, &mut c).unwrap();
        assert_eq!(u1.input + u2.input, 1005 + 3);
        assert_eq!(u1.output + u2.output, 300 + 50);
        assert_eq!(n1 + n2, 2);
        assert_eq!(u2.context, 101_003);
        // The rate: an EMA of what arrived.
        let mut r = Rate::default();
        r.add(Usage {
            input: 1008,
            output: 350,
            context: 0,
        });
        r.update(1.0);
        assert!(r.ema_in > 0.0 && r.ema_in < 1008.0);
        let first = r.ema_in;
        r.update(1.0);
        assert!(r.ema_in < first, "decays when nothing arrives");
        // grok: deltas of its running totals.
        let gd = d.join("grok");
        std::fs::create_dir_all(&gd).unwrap();
        let w = |i: u64, o: u64| {
            std::fs::write(
                gd.join("usage.json"),
                format!("{{\"session\":{{\"inputTokens\":{i},\"outputTokens\":{o},\"cachedReadTokens\":99999}}}}"),
            )
            .unwrap()
        };
        w(100, 10);
        let mut gc = Cursor::default();
        assert_eq!(tail_grok(&gd, &mut gc).unwrap().0.input, 0);
        w(400, 70);
        let (gu, _) = tail_grok(&gd, &mut gc).unwrap();
        assert_eq!((gu.input, gu.output), (300, 60));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn events_fill_the_ticker() {
        let mut lm = LiveMap::default();
        let mut s = snap(2, 3);
        lm.apply(s.clone());
        assert!(lm.ticker.is_empty(), "the first snapshot is not news");
        s.tabs[0].state = TabState::Working;
        lm.apply(s.clone());
        s.tabs[0].state = TabState::Idle;
        s.tabs[1].state = TabState::Waiting;
        s.tabs[2].fires = 1;
        s.tabs.push(TabSnap {
            uid: 999,
            name: "new".into(),
            ..s.tabs[0].clone()
        });
        lm.apply(s.clone());
        assert!(!lm.sim.particles.is_empty(), "a finished turn bursts");
        s.tabs.remove(0);
        lm.apply(s);
        let text: Vec<String> = lm.ticker.iter().map(|t| t.text.clone()).collect();
        for want in [
            "turn finished: tab0",
            "approval needed: tab1",
            "loop fired: tab2",
            "tab opened: new",
            "tab closed: tab0",
        ] {
            assert!(
                text.iter().any(|t| t.starts_with(want)),
                "{want} in {text:?}"
            );
        }
        assert!(
            lm.sim.particles.iter().all(|p| p.link != Link::Tab(100)),
            "a closed tab's particles go"
        );
    }

    #[test]
    fn the_view_opens_draws_clicks_through_and_costs_nothing_closed() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("godterm-lm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        crate::config::testing::set_home(&home);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(crate::config::Config::default(), tx);
        app.cfg.claude_bin = Some("/usr/bin/true".into());
        for i in 0..3 {
            let d = home.join(format!("proj{i}"));
            std::fs::create_dir_all(&d).unwrap();
            app.panes[i % 2].add_tab(d);
        }
        let draw = |app: &mut crate::app::App, w: u16, h: u16| {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| crate::ui::draw(f, app)).unwrap();
            let b = term.backend().buffer().clone();
            let mut s = String::new();
            for y in 0..b.area.height {
                for x in 0..b.area.width {
                    s.push_str(b[(x, y)].symbol());
                }
                s.push('\n');
            }
            s
        };
        // Closed: no frames, no thread, no animation timer.
        draw(&mut app, 200, 50);
        assert_eq!(app.livemap.frames, 0);
        assert!(!app.livemap.tailer_alive());
        assert_eq!(app.anim_interval(), None);
        // Open: 30 fps, a snapshot, every tab placed.
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        ));
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('G'),
            KeyModifiers::SHIFT,
        ));
        assert_eq!(app.view, View::LiveMap);
        assert_eq!(app.anim_interval(), Some(Duration::from_millis(1000 / FPS)));
        let text = draw(&mut app, 200, 50);
        assert!(
            text.contains("LIVE MAP") && text.contains("EVENTS"),
            "{text}"
        );
        assert!(text.contains("GODTERM"));
        assert!(app.livemap.frames > 0 && app.livemap.tailer_alive());
        let n_tabs: usize = app.panes.iter().map(|p| p.tabs.len()).sum();
        assert_eq!(app.livemap.snap.tabs.len(), n_tabs);
        // Reduced motion: 8 fps.
        app.cfg.viz.reduced_motion = true;
        assert_eq!(
            app.anim_interval(),
            Some(Duration::from_millis(1000 / FPS_REDUCED))
        );
        app.cfg.viz.reduced_motion = false;
        // Hover shows a tooltip; a click on a tab opens it.
        let uid = app.panes[1].tabs.last().unwrap().uid;
        let n = app.livemap.placed.get(NodeId::Tab(uid)).unwrap().clone();
        let c = app.livemap.canvas;
        let (col, row) = (c.x + (n.x / 2.0) as u16, c.y + (n.y / 4.0) as u16);
        let ev = |kind| MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse_event(ev(MouseEventKind::Moved));
        let text = draw(&mut app, 200, 50);
        assert!(text.contains("click to open this tab"), "{text}");
        // The draw moved the satellite a little: aim again.
        let n = app.livemap.placed.get(NodeId::Tab(uid)).unwrap().clone();
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: c.x + (n.x / 2.0) as u16,
            row: c.y + (n.y / 4.0) as u16,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse_event(ev);
        assert_eq!(app.view, View::Grid);
        assert_eq!(app.focus, 1);
        // Closed again: the timer stops now, the thread on its own.
        assert_eq!(app.anim_interval(), None);
        let frames = app.livemap.frames;
        draw(&mut app, 200, 50);
        assert_eq!(app.livemap.frames, frames);
        let t0 = Instant::now();
        while app.livemap.tailer_alive() && t0.elapsed() < Duration::from_secs(6) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!app.livemap.tailer_alive(), "the tailer thread ended");
        // Small terminals get the radial list.
        app.open_livemap();
        let text = draw(&mut app, 70, 20);
        assert!(text.contains("LIVE MAP") && text.contains("└─"), "{text}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
