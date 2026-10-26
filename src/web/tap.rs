//! A tab's output for web clients: each subscribed browser gets the bytes
//! the PTY reader fed the parser, in order, from a snapshot taken under
//! the same parser lock (so nothing is missed or repeated).
//!
//! Locks are always taken in this order: a tab's parser, its `Tap`, a
//! client's `Outbox`. The reader thread never waits on a browser: a
//! client that falls behind has its queued output dropped and gets a
//! fresh snapshot instead (`Out::Resync`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex};

/// Bytes of terminal output queued for one client before it is treated
/// as slow (its tabs are re-sent as snapshots once it catches up).
pub const OUTPUT_LIMIT: usize = 4 << 20;
/// Queued text frames past this mean the browser is gone: drop it.
pub const HARD_LIMIT: usize = 64 << 20;
/// History lines sent above the screen in a snapshot.
pub const SNAPSHOT_HISTORY: usize = 3000;
/// The recent output a tab keeps for browsers that come back (a phone
/// back from another app reconnects and asks for what it missed, instead
/// of a whole snapshot).
pub const RESUME_BYTES: usize = 256 << 10;
/// How long that output is kept after the last browser left the tab.
pub const RESUME_KEEP: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// What a client's writer thread sends next.
#[derive(Debug, PartialEq)]
pub enum Out {
    /// A JSON frame.
    Text(String),
    /// A tab's `snapshot` (or `resume`) frame: JSON, sent gzipped as
    /// binary kind 6 when the browser asked for that and it is big.
    Snapshot(u64, String),
    /// Terminal output of a tab (binary kind 1 once framed).
    Output(u64, Vec<u8>),
    /// Take a new snapshot of the tab and send it.
    Resync(u64),
    /// Assistant speech for this browser (binary kind 2 once framed).
    Audio(Vec<u8>),
    /// A browser view picture, framed already (binary kind 5).
    Picture(Vec<u8>),
    /// Send a pong with this payload.
    Pong(Vec<u8>),
    /// Close the connection.
    Close,
}

#[derive(Default)]
struct Queue {
    frames: VecDeque<Out>,
    /// Bytes of `Output` frames queued.
    output: usize,
    /// Bytes of everything queued.
    total: usize,
    /// Tabs whose output is dropped until their next snapshot.
    lagged: HashSet<u64>,
    /// `Picture` frames queued.
    pictures: usize,
    closed: bool,
}

/// One client's send queue: bounded, never blocks the producer.
#[derive(Default)]
pub struct Outbox {
    q: Mutex<Queue>,
    cv: Condvar,
}

impl Outbox {
    pub fn new() -> Arc<Outbox> {
        Arc::new(Outbox::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A JSON frame; false when the client is gone or hopelessly behind
    /// (it is closed then).
    pub fn text(&self, s: String) -> bool {
        let mut q = self.lock();
        if q.closed {
            return false;
        }
        if q.total + s.len() > HARD_LIMIT {
            q.closed = true;
            q.frames.clear();
            q.frames.push_back(Out::Close);
            self.cv.notify_one();
            return false;
        }
        q.total += s.len();
        q.frames.push_back(Out::Text(s));
        self.cv.notify_one();
        true
    }

    /// Speech audio (a WAV clip); dropped when the browser is far behind.
    pub fn audio(&self, clip: Vec<u8>) {
        let mut q = self.lock();
        if q.closed || q.total + clip.len() > OUTPUT_LIMIT {
            return;
        }
        q.total += clip.len();
        q.frames.push_back(Out::Audio(clip));
        self.cv.notify_one();
    }

    /// A browser view picture (framed): dropped while one is still queued
    /// (a slow browser gets fewer pictures, never a backlog). False when
    /// it was dropped.
    pub fn picture(&self, framed: Vec<u8>) -> bool {
        let mut q = self.lock();
        if q.closed || q.pictures > 0 || q.total + framed.len() > OUTPUT_LIMIT {
            return false;
        }
        q.total += framed.len();
        q.pictures += 1;
        q.frames.push_back(Out::Picture(framed));
        self.cv.notify_one();
        true
    }

    /// A pong (answers come before queued output would be nicer, but
    /// browsers do not ping; this is for tests and proxies).
    pub fn pong(&self, data: Vec<u8>) {
        let mut q = self.lock();
        if !q.closed {
            q.frames.push_back(Out::Pong(data));
            self.cv.notify_one();
        }
    }

    /// Output of tab `uid`. Over the limit, the tab's output is dropped
    /// until a new snapshot (queued here) replaces it.
    pub fn output(&self, uid: u64, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut q = self.lock();
        if q.closed || q.lagged.contains(&uid) {
            return;
        }
        if q.output + bytes.len() > OUTPUT_LIMIT {
            q.lagged.insert(uid);
            q.frames.push_back(Out::Resync(uid));
            self.cv.notify_one();
            return;
        }
        q.output += bytes.len();
        q.total += bytes.len();
        // Joined onto the last frame when it is the same tab's (fewer,
        // larger WebSocket messages under a flood).
        if let Some(Out::Output(u, b)) = q.frames.back_mut() {
            if *u == uid && b.len() < 256 << 10 {
                b.extend_from_slice(bytes);
                return;
            }
        }
        q.frames.push_back(Out::Output(uid, bytes.to_vec()));
        self.cv.notify_one();
    }

    /// The tab changed in a way its byte stream does not carry (a resize,
    /// a new process, a rebuilt parser): send a new snapshot.
    pub fn resync(&self, uid: u64) {
        let mut q = self.lock();
        if q.closed || !q.lagged.insert(uid) {
            return;
        }
        q.frames.push_back(Out::Resync(uid));
        self.cv.notify_one();
    }

    /// Tab `uid` was resized (a `resized` frame, in order with its
    /// output): the browser resizes its terminal and output goes on, with
    /// no new snapshot. Skipped while the tab's output is dropped (its
    /// snapshot, already due, has the new size).
    pub fn resized(&self, uid: u64, frame: String) {
        let mut q = self.lock();
        if q.closed || q.lagged.contains(&uid) {
            return;
        }
        q.total += frame.len();
        q.frames.push_back(Out::Text(frame));
        self.cv.notify_one();
    }

    /// The snapshot for `uid` (taken under its parser lock): output flows
    /// again after it.
    pub fn snapshot(&self, uid: u64, frame: String) {
        let mut q = self.lock();
        if q.closed {
            return;
        }
        q.lagged.remove(&uid);
        q.total += frame.len();
        q.frames.push_back(Out::Snapshot(uid, frame));
        self.cv.notify_one();
    }

    pub fn close(&self) {
        let mut q = self.lock();
        if !q.closed {
            q.closed = true;
            q.frames.push_back(Out::Close);
            self.cv.notify_one();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// The next frame, waiting up to `wait` (None on timeout).
    pub fn pop(&self, wait: std::time::Duration) -> Option<Out> {
        let mut q = self.lock();
        if q.frames.is_empty() {
            q = self
                .cv
                .wait_timeout_while(q, wait, |q| q.frames.is_empty())
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let f = q.frames.pop_front()?;
        let n = match &f {
            Out::Output(_, b) => {
                q.output -= b.len();
                b.len()
            }
            Out::Text(s) | Out::Snapshot(_, s) => s.len(),
            Out::Audio(b) => b.len(),
            Out::Picture(b) => {
                q.pictures -= 1;
                b.len()
            }
            _ => 0,
        };
        q.total -= n;
        Some(f)
    }

    /// Bytes queued (tests).
    #[cfg(test)]
    pub fn queued(&self) -> (usize, usize) {
        let q = self.lock();
        (q.output, q.frames.len())
    }
}

/// One subscribed client.
struct Sub {
    client: u64,
    out: Arc<Outbox>,
}

struct Inner {
    /// The generation whose reader may publish (older readers are still
    /// draining a killed process).
    gen: u64,
    parser: Arc<Mutex<vt100::Parser>>,
    input: Option<SyncSender<Vec<u8>>>,
    subs: Vec<Sub>,
    /// The output stream browsers get: bumped whenever it breaks (a new
    /// process, a resize, a rebuilt screen), so a resume from before it
    /// gets a snapshot instead.
    epoch: u64,
    /// Bytes published in this stream so far.
    pos: u64,
    /// The last RESUME_BYTES of them.
    ring: VecDeque<u8>,
    /// No browser left: the ring is dropped after this.
    keep_until: Option<std::time::Instant>,
}

impl Inner {
    /// The stream broke: nothing from before can be resumed.
    fn new_stream(&mut self) {
        self.epoch += 1;
        self.ring.clear();
    }
}

/// Where a browser's copy of a tab's output ends: the stream id from its
/// last `snapshot` or `resume` and the bytes it got since then.
#[derive(Debug, Clone, PartialEq)]
pub struct Resume {
    pub sid: String,
    pub pos: u64,
}

/// A tab's web side: its parser and input as of its current process, and
/// the clients subscribed to it.
pub struct Tap {
    pub uid: u64,
    /// This tap's own number in this process (part of its stream ids).
    serial: u64,
    inner: Mutex<Inner>,
    /// Someone is subscribed (read by the reader thread without the lock,
    /// while it holds the parser lock that subscribing also takes).
    any: AtomicBool,
}

impl Tap {
    /// A tab's tap, registered under its uid until dropped (`forget`).
    pub fn new(uid: u64, parser: Arc<Mutex<vt100::Parser>>) -> Arc<Tap> {
        static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let t = Arc::new(Tap {
            uid,
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            inner: Mutex::new(Inner {
                gen: 0,
                parser,
                input: None,
                subs: vec![],
                epoch: 0,
                pos: 0,
                ring: VecDeque::new(),
                keep_until: None,
            }),
            any: AtomicBool::new(false),
        });
        registry().insert(uid, Arc::clone(&t));
        t
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The id of the current output stream: unique across processes
    /// (GodTerm's start), taps and breaks in the stream.
    fn sid(&self, i: &Inner) -> String {
        format!("{:x}.{}.{}", process_nonce(), self.serial, i.epoch)
    }

    /// A new process (generation `gen`) with a fresh parser: clients see
    /// its screen from a new snapshot.
    pub fn respawn(
        &self,
        gen: u64,
        parser: Arc<Mutex<vt100::Parser>>,
        input: Option<SyncSender<Vec<u8>>>,
    ) {
        let mut i = self.lock();
        i.gen = gen;
        i.parser = parser;
        i.input = input;
        i.new_stream();
        for s in &i.subs {
            s.out.resync(self.uid);
        }
    }

    /// The process ended: input goes nowhere (and its writer thread may
    /// end, holding no sender here).
    pub fn stop_input(&self) {
        self.lock().input = None;
    }

    /// Whether any client is subscribed. Called with the parser locked.
    pub fn active(&self) -> bool {
        self.any.load(Ordering::Acquire)
    }

    /// Bytes the reader of generation `gen` just fed the parser (called
    /// with the parser still locked).
    pub fn publish(&self, gen: u64, bytes: &[u8]) {
        let mut i = self.lock();
        if i.gen != gen {
            return;
        }
        // Nobody came back in time: let the kept output go.
        if i.subs.is_empty() && i.keep_until.is_none_or(|t| std::time::Instant::now() >= t) {
            i.keep_until = None;
            i.ring = VecDeque::new();
            i.epoch += 1;
            self.any.store(false, Ordering::Release);
            return;
        }
        let shown = strip_queries(bytes);
        i.pos += shown.len() as u64;
        i.ring.extend(shown.iter());
        let over = i.ring.len().saturating_sub(RESUME_BYTES);
        if over > 0 {
            i.ring.drain(..over);
        }
        for s in &i.subs {
            s.out.output(self.uid, &shown);
        }
    }

    /// Every client takes a new snapshot (called with the parser locked).
    pub fn resync_all(&self) {
        let mut i = self.lock();
        i.new_stream();
        for s in &i.subs {
            s.out.resync(self.uid);
        }
    }

    /// The screen was resized to rows x cols (called with the parser
    /// locked, after its new size is set): browsers get a `resized` frame
    /// and output flows on, instead of a whole snapshot each time (a phone
    /// zooming, a pane dragged wider). Their terminal rewraps as a real
    /// one does and the app redraws. The stream id changes (a resume from
    /// before the resize gets a snapshot) but not the position.
    pub fn resized(&self, rows: u16, cols: u16) {
        let mut i = self.lock();
        i.new_stream();
        let frame = serde_json::json!({
            "t": "resized",
            "tab": self.uid,
            "rows": rows,
            "cols": cols,
            "sid": self.sid(&i),
            "pos": i.pos,
        })
        .to_string();
        for s in &i.subs {
            s.out.resized(self.uid, frame.clone());
        }
    }

    /// `n` wheel notches over screen cell (col, row) from a browser, as
    /// the terminal UI's wheel sends them (mouse reports in the app's own
    /// encoding, or arrow keys on the alternate screen). Returns what went
    /// to the PTY ("mouse", "keys"), or None when the app takes no wheel
    /// or the tab is not running.
    pub fn wheel(&self, up: bool, n: u16, col: u16, row: u16) -> Option<&'static str> {
        let parser = Arc::clone(&self.lock().parser);
        let (bytes, how) = {
            let p = parser.lock().unwrap_or_else(|e| e.into_inner());
            let s = p.screen();
            let how = if s.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
                "mouse"
            } else {
                "keys"
            };
            (crate::app_mouse::wheel_input(s, up, n, col, row)?, how)
        };
        self.write(&bytes).then_some(how)
    }

    /// Write browser input to the tab's PTY; false when it has no process
    /// or its queue is full.
    pub fn write(&self, bytes: &[u8]) -> bool {
        let tx = self.lock().input.clone();
        tx.is_some_and(|tx| tx.try_send(bytes.to_vec()).is_ok())
    }

    /// Subscribe `client` (or, when it already is, just re-snapshot): the
    /// snapshot goes into its outbox under the parser lock, then output.
    pub fn subscribe(&self, client: u64, out: &Arc<Outbox>) {
        self.snapshot_for(client, out, true, None);
    }

    /// Subscribe a browser that already has the tab's output up to
    /// `from`: when the stream is unbroken and its ring still holds the
    /// rest, a `resume` frame and only the missing bytes go out, else a
    /// snapshot as `subscribe` sends. True when it resumed.
    pub fn subscribe_from(&self, client: u64, out: &Arc<Outbox>, from: &Resume) -> bool {
        self.snapshot_for(client, out, true, Some(from))
    }

    /// A new snapshot for a client still subscribed (after `Out::Resync`).
    pub fn resnapshot(&self, client: u64, out: &Arc<Outbox>) {
        self.snapshot_for(client, out, false, None);
    }

    fn snapshot_for(
        &self,
        client: u64,
        out: &Arc<Outbox>,
        add: bool,
        from: Option<&Resume>,
    ) -> bool {
        loop {
            let parser = Arc::clone(&self.lock().parser);
            let mut p = parser.lock().unwrap_or_else(|e| e.into_inner());
            let mut i = self.lock();
            // Respawned between the two locks: take the new parser.
            if !Arc::ptr_eq(&i.parser, &parser) {
                continue;
            }
            let known = i.subs.iter().any(|s| s.client == client);
            if !known && !add {
                return false;
            }
            if !known {
                i.subs.push(Sub {
                    client,
                    out: Arc::clone(out),
                });
                i.keep_until = None;
                self.any.store(true, Ordering::Release);
            }
            let sid = self.sid(&i);
            let (rows, cols) = p.screen().size();
            if let Some(r) = from {
                let missed = i.pos.checked_sub(r.pos);
                if let Some(n) = missed.filter(|&n| r.sid == sid && n <= i.ring.len() as u64) {
                    let n = n as usize;
                    let tail: Vec<u8> = i.ring.range(i.ring.len() - n..).copied().collect();
                    let frame = serde_json::json!({
                        "t": "resume",
                        "tab": self.uid,
                        "sid": sid,
                        "pos": r.pos,
                        "rows": rows,
                        "cols": cols,
                    })
                    .to_string();
                    out.snapshot(self.uid, frame);
                    out.output(self.uid, &tail);
                    return true;
                }
            }
            let frame = snapshot_frame(self.uid, &mut p, &sid, i.pos);
            out.snapshot(self.uid, frame);
            return false;
        }
    }

    /// The client is gone or looks elsewhere. The recent output stays for
    /// RESUME_KEEP after the last one, for a browser that comes back.
    pub fn unsubscribe(&self, client: u64) {
        let mut i = self.lock();
        let before = i.subs.len();
        i.subs.retain(|s| s.client != client);
        if before > 0 && i.subs.is_empty() {
            i.keep_until = Some(std::time::Instant::now() + RESUME_KEEP);
        }
    }

    /// The screen size (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        let parser = Arc::clone(&self.lock().parser);
        let p = parser.lock().unwrap_or_else(|e| e.into_inner());
        p.screen().size()
    }

    /// The tab is gone: out of the registry.
    pub fn forget(&self) {
        let mut reg = registry();
        if reg.get(&self.uid).is_some_and(|t| std::ptr::eq(&**t, self)) {
            reg.remove(&self.uid);
        }
    }
}

/// A number for this GodTerm process (its start time), so stream ids from
/// before a restart never match.
fn process_nonce() -> u64 {
    static N: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64 ^ u64::from(std::process::id()))
    })
}

/// Every tab's tap by uid.
fn registry() -> std::sync::MutexGuard<'static, HashMap<u64, Arc<Tap>>> {
    static REG: std::sync::OnceLock<Mutex<HashMap<u64, Arc<Tap>>>> = std::sync::OnceLock::new();
    REG.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

pub fn get(uid: u64) -> Option<Arc<Tap>> {
    registry().get(&uid).cloned()
}

/// Remove `client` from every tab it subscribed to.
pub fn drop_client(client: u64, uids: &[u64]) {
    for u in uids {
        if let Some(t) = get(*u) {
            t.unsubscribe(client);
        }
    }
}

/// The `snapshot` frame for a tab: its history as text, then its screen
/// with colors, its input modes and the cursor. The parser's own scroll
/// position (the terminal UI's) is left as it was.
/// `sid` and `pos` say where the output that follows starts (for a later
/// resume).
pub fn snapshot_frame(uid: u64, p: &mut vt100::Parser, sid: &str, pos: u64) -> String {
    let (data, rows, cols) = snapshot_bytes(p, SNAPSHOT_HISTORY);
    serde_json::json!({
        "t": "snapshot",
        "tab": uid,
        "rows": rows,
        "cols": cols,
        "sid": sid,
        "pos": pos,
        "data": String::from_utf8_lossy(&data),
    })
    .to_string()
}

/// (bytes, rows, cols) that redraw `p` on a blank terminal of its size.
pub fn snapshot_bytes(p: &mut vt100::Parser, history: usize) -> (Vec<u8>, u16, u16) {
    let (rows, cols) = p.screen().size();
    let was = p.screen().scrollback();
    let mut out = Vec::new();
    // History lives on the main screen only; the alternate screen (a full
    // screen app) has none to show.
    if !p.screen().alternate_screen() && history > 0 {
        p.screen_mut().set_scrollback(usize::MAX);
        let keep = p.screen().scrollback().min(history);
        for k in (1..=keep).rev() {
            p.screen_mut().set_scrollback(k);
            history_row(p.screen(), cols, &mut out);
            out.extend_from_slice(b"\r\n");
        }
        p.screen_mut().set_scrollback(0);
        if keep > 0 {
            // The screen's rows as text push the history up into the
            // terminal's scrollback; the screen is then drawn in full.
            let lines: Vec<String> = p.screen().rows(0, cols).collect();
            let n = lines.len();
            for (i, l) in lines.iter().enumerate() {
                out.extend_from_slice(l.as_bytes());
                if i + 1 < n {
                    out.extend_from_slice(b"\r\n");
                }
            }
        }
    }
    p.screen_mut().set_scrollback(0);
    let s = p.screen();
    // A full screen app's screen goes on the browser's alternate screen
    // too, as its output put it there: drawn on the main one, the browser
    // took it for a screen with history to scroll (Claude Code's fullscreen
    // view never got the wheel), and the app's redraws piled up in it.
    if s.alternate_screen() {
        out.extend_from_slice(b"\x1b[?1049h\x1b[H\x1b[2J");
    }
    out.extend_from_slice(&s.contents_formatted());
    out.extend_from_slice(&s.input_mode_formatted());
    let (r, c) = s.cursor_position();
    out.extend_from_slice(format!("\x1b[{};{}H", r + 1, c + 1).as_bytes());
    if s.hide_cursor() {
        out.extend_from_slice(b"\x1b[?25l");
    }
    p.screen_mut().set_scrollback(was);
    (out, rows, cols)
}

/// The top visible row of `s` (a history line while scrolled back) with
/// its colors and attributes, as plain SGR runs: no cursor moves, trailing
/// blanks dropped, attributes reset at the end.
fn history_row(s: &vt100::Screen, cols: u16, out: &mut Vec<u8>) {
    type Style = (vt100::Color, vt100::Color, bool, bool, bool, bool, bool);
    let style = |c: &vt100::Cell| -> Style {
        (
            c.fgcolor(),
            c.bgcolor(),
            c.bold(),
            c.dim(),
            c.italic(),
            c.underline(),
            c.inverse(),
        )
    };
    let plain: Style = (
        vt100::Color::Default,
        vt100::Color::Default,
        false,
        false,
        false,
        false,
        false,
    );
    // The last column that shows anything (text, or a colored background).
    let mut end = 0;
    for col in 0..cols {
        if let Some(c) = s.cell(0, col) {
            if c.has_contents() || c.bgcolor() != vt100::Color::Default || c.inverse() {
                end = col + 1;
            }
        }
    }
    let mut cur = plain;
    for col in 0..end {
        let Some(c) = s.cell(0, col) else { break };
        if c.is_wide_continuation() {
            continue;
        }
        let st = style(c);
        if st != cur {
            sgr(&st, out);
            cur = st;
        }
        if c.has_contents() {
            out.extend_from_slice(c.contents().as_bytes());
        } else {
            out.push(b' ');
        }
    }
    if cur != plain {
        out.extend_from_slice(b"\x1b[m");
    }

    fn sgr(st: &Style, out: &mut Vec<u8>) {
        let mut v = String::from("\x1b[0");
        let color = |c: vt100::Color, base: u8, v: &mut String| match c {
            vt100::Color::Default => {}
            vt100::Color::Idx(i) if i < 8 => v.push_str(&format!(";{}", base + i)),
            vt100::Color::Idx(i) if i < 16 => v.push_str(&format!(";{}", base + 60 + i - 8)),
            vt100::Color::Idx(i) => v.push_str(&format!(";{};5;{i}", base + 8)),
            vt100::Color::Rgb(r, g, b) => v.push_str(&format!(";{};2;{r};{g};{b}", base + 8)),
        };
        color(st.0, 30, &mut v);
        color(st.1, 40, &mut v);
        for (on, code) in [
            (st.2, ";1"),
            (st.3, ";2"),
            (st.4, ";3"),
            (st.5, ";4"),
            (st.6, ";7"),
        ] {
            if on {
                v.push_str(code);
            }
        }
        v.push('m');
        out.extend_from_slice(v.as_bytes());
    }
}

/// Output without the terminal queries GodTerm answers itself (cursor
/// position, device attributes, colors, version, modes): a browser's
/// terminal would answer them too, and its replies would reach the
/// program as typed input. Only whole sequences within one chunk.
pub fn strip_queries(b: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if !b.contains(&0x1b) {
        return std::borrow::Cow::Borrowed(b);
    }
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut changed = false;
    while i < b.len() {
        if b[i] == 0x1b {
            if let Some(n) = query_len(&b[i..]) {
                i += n;
                changed = true;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    if changed {
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(b)
    }
}

/// The length of the query sequence at the start of `b`, if it is one.
fn query_len(b: &[u8]) -> Option<usize> {
    match b.get(1)? {
        b'[' => {
            // CSI: parameters and intermediates, then the final byte.
            let mut j = 2;
            while j < b.len() && (0x20..0x40).contains(&b[j]) {
                j += 1;
            }
            let fin = *b.get(j)?;
            let params = &b[2..j];
            let query = match fin {
                // DSR (5n, 6n, ?6n), DA (c, 0c, >c, =c), XTVERSION (>q),
                // DECRQM (...$p), kitty keyboard (?u), XTWINOPS reports.
                b'n' => matches!(params, b"5" | b"6" | b"?6"),
                b'c' => matches!(params, b"" | b"0" | b">" | b">0" | b"=" | b"=0"),
                b'q' => matches!(params, b">" | b">0"),
                b'p' => params.ends_with(b"$"),
                b'u' => params == b"?",
                b't' => matches!(params, b"14" | b"16" | b"18" | b"19"),
                _ => false,
            };
            query.then_some(j + 1)
        }
        b']' => {
            // OSC 4/10/11/12 color queries: "...;?" then BEL or ST.
            let end = b
                .iter()
                .position(|&x| x == 0x07)
                .map(|p| (p, p + 1))
                .or_else(|| {
                    b.windows(2)
                        .position(|w| w == b"\x1b\\")
                        .filter(|&p| p > 1)
                        .map(|p| (p, p + 2))
                })?;
            let body = &b[2..end.0];
            let color = body.starts_with(b"10;")
                || body.starts_with(b"11;")
                || body.starts_with(b"12;")
                || body.starts_with(b"4;");
            (color && body.ends_with(b";?")).then_some(end.1)
        }
        _ => None,
    }
}
