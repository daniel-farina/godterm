//! Terminal output off the UI thread.
//!
//! The UI thread never writes to the terminal itself. Each frame is rendered
//! into memory, as one self contained chunk (synchronized update begin, the
//! cell diff, the cursor, synchronized update end), and handed to a writer
//! thread that does the blocking writes. A terminal that stops reading (a
//! full tty queue) then stalls only that thread: the event loop, voice,
//! ticks and state saving go on, and frames wait until the writer is free.
//!
//! Chunks are never dropped once produced: ratatui diffs each frame against
//! the previous one, so a lost chunk would leave the screen wrong.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crossterm::{cursor, queue, terminal};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use ratatui::{Frame, Terminal};

/// How long the writer may sit on one chunk before it counts as stalled.
pub const STALL: Duration = Duration::from_secs(2);

#[derive(Default)]
struct St {
    queue: VecDeque<Vec<u8>>,
    /// When the writer took the chunk it is writing now.
    inflight_since: Option<Instant>,
    closed: bool,
}

struct Inner {
    st: Mutex<St>,
    cv: Condvar,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A queue of byte chunks and the thread that writes them, in order.
#[derive(Clone)]
pub struct Out {
    inner: Arc<Inner>,
}

impl Out {
    /// Start the writer thread on `sink`.
    pub fn spawn<W: Write + Send + 'static>(name: &str, mut sink: W) -> io::Result<Out> {
        let inner = Arc::new(Inner {
            st: Mutex::new(St::default()),
            cv: Condvar::new(),
        });
        let t = Arc::clone(&inner);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || loop {
                let chunk = {
                    let mut st = t.lock();
                    loop {
                        if let Some(c) = st.queue.pop_front() {
                            st.inflight_since = Some(Instant::now());
                            break c;
                        }
                        if st.closed {
                            return;
                        }
                        st = t.cv.wait(st).unwrap_or_else(|e| e.into_inner());
                    }
                };
                // write_all retries EINTR. Any other error (the terminal
                // went away) loses only this chunk; the next one tries again.
                let _ = sink.write_all(&chunk).and_then(|_| sink.flush());
                let mut st = t.lock();
                st.inflight_since = None;
                t.cv.notify_all();
            })?;
        Ok(Out { inner })
    }

    /// Queue bytes for the terminal. Never blocks, never drops.
    pub fn send(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let mut st = self.inner.lock();
        st.queue.push_back(bytes);
        self.inner.cv.notify_all();
    }

    /// Nothing queued and nothing being written.
    pub fn idle(&self) -> bool {
        let st = self.inner.lock();
        st.queue.is_empty() && st.inflight_since.is_none()
    }

    /// How long the writer has been on its current chunk (zero when free).
    pub fn stalled_for(&self) -> Duration {
        self.inner
            .lock()
            .inflight_since
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    /// Wait at most `bound` for everything queued to be written.
    pub fn wait_idle(&self, bound: Duration) -> bool {
        let end = Instant::now() + bound;
        let mut st = self.inner.lock();
        while !(st.queue.is_empty() && st.inflight_since.is_none()) {
            let now = Instant::now();
            if now >= end {
                return false;
            }
            st = self
                .inner
                .cv
                .wait_timeout(st, end - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// The last bytes (terminal restore): queued, then at most `bound` to
    /// get them out. True when they were written. The writer thread is told
    /// to end once its queue is empty; a stuck one is left behind.
    pub fn finish(&self, bytes: Vec<u8>, bound: Duration) -> bool {
        if self.inner.lock().closed {
            // Finished before (a panic after the normal restore).
            return self.idle();
        }
        self.send(bytes);
        let done = self.wait_idle(bound);
        self.inner.lock().closed = true;
        self.inner.cv.notify_all();
        done
    }
}

static GLOBAL: OnceLock<Out> = OnceLock::new();

/// Make `out` the terminal's writer for code without a handle to it.
pub fn install(out: Out) {
    let _ = GLOBAL.set(out);
}

pub fn global() -> Option<&'static Out> {
    GLOBAL.get()
}

/// Bytes for the terminal from anywhere (OSC 52 and the like): through the
/// writer when the TUI runs, else straight to stdout.
pub fn write_raw(bytes: Vec<u8>) {
    match global() {
        Some(o) => o.send(bytes),
        None => {
            let mut out = io::stdout();
            let _ = out.write_all(&bytes);
            let _ = out.flush();
        }
    }
}

/// A writer into a shared in-memory buffer.
#[derive(Clone, Default)]
pub struct FrameBuf(Arc<Mutex<Vec<u8>>>);

impl FrameBuf {
    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Write for FrameBuf {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The crossterm backend over a [`FrameBuf`], except that it never asks the
/// terminal where the cursor is. Crossterm's query writes to stdout itself
/// and waits up to 2 s for the answer, and ratatui asks on every clear and
/// resize; the cursor is wherever the frame last put it.
pub struct BufBackend {
    inner: CrosstermBackend<FrameBuf>,
    cursor: Position,
}

impl BufBackend {
    pub fn new(buf: FrameBuf) -> Self {
        BufBackend {
            inner: CrosstermBackend::new(buf),
            cursor: Position::ORIGIN,
        }
    }
}

impl Backend for BufBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.cursor)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let p = position.into();
        self.cursor = p;
        self.inner.set_cursor_position(p)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A change in the writer's health, for the caller to log.
#[derive(Debug, PartialEq, Eq)]
pub enum Health {
    Stalled,
    Recovered(Duration),
}

/// The TUI's screen: a ratatui terminal drawing into memory, frames going
/// out through an [`Out`].
pub struct Screen<B: Backend = BufBackend> {
    pub term: Terminal<B>,
    buf: FrameBuf,
    out: Out,
    need_clear: bool,
    stalled: Option<Instant>,
    stall_after: Duration,
}

impl Screen<BufBackend> {
    pub fn new(out: Out) -> io::Result<Self> {
        let buf = FrameBuf::default();
        let term = Terminal::new(BufBackend::new(buf.clone()))?;
        Ok(Screen::with_terminal(term, buf, out))
    }
}

impl<B: Backend> Screen<B> {
    /// A screen over any backend that writes into `buf`.
    pub fn with_terminal(term: Terminal<B>, buf: FrameBuf, out: Out) -> Self {
        Screen {
            term,
            buf,
            out,
            need_clear: true,
            stalled: None,
            stall_after: STALL,
        }
    }

    #[cfg(test)]
    pub fn set_stall_after(&mut self, d: Duration) {
        self.stall_after = d;
    }

    /// The writer is free: a new frame may be drawn.
    pub fn ready(&self) -> bool {
        self.out.idle()
    }

    /// The next frame starts from a cleared screen and redraws every cell.
    pub fn clear_next(&mut self) {
        self.need_clear = true;
    }

    /// Queue a crossterm command (mouse capture and the like).
    pub fn command(&self, c: impl crossterm::Command) {
        let mut v = Vec::new();
        let _ = queue!(v, c);
        self.out.send(v);
    }

    /// Notice the writer stalling or coming back. A writer back from a stall
    /// gets a full redraw: what the terminal shows is uncertain by then.
    pub fn check(&mut self) -> Option<Health> {
        match self.stalled {
            None if self.out.stalled_for() >= self.stall_after => {
                self.stalled = Some(Instant::now());
                Some(Health::Stalled)
            }
            Some(at) if self.out.idle() => {
                self.stalled = None;
                self.need_clear = true;
                Some(Health::Recovered(at.elapsed() + self.stall_after))
            }
            _ => None,
        }
    }

    pub fn stalled(&self) -> bool {
        self.stalled.is_some()
    }

    /// Draw one frame and queue it as one chunk. The caller holds frames
    /// back while the writer is busy ([`Screen::ready`]); a frame drawn
    /// anyway is queued behind, never dropped. `done` sees the drawn buffer.
    pub fn draw<B2>(
        &mut self,
        render: impl FnOnce(&mut Frame),
        done: impl FnOnce(&Buffer) -> B2,
    ) -> io::Result<B2>
    where
        B::Error: Into<io::Error>,
    {
        // One frame at once (terminals that support it), with the cursor
        // hidden while cells are written: no cursor flashing across the
        // screen. The frame shows it again at the input.
        let mut head = Vec::new();
        let _ = queue!(head, terminal::BeginSynchronizedUpdate, cursor::Hide);
        let _ = self.buf.take();
        let mut drawn = Ok(());
        if self.need_clear {
            drawn = self.term.clear().map_err(Into::into);
        }
        let r = match drawn {
            Ok(()) => self
                .term
                .draw(render)
                .map(|f| done(f.buffer))
                .map_err(Into::into),
            Err(e) => Err(e),
        };
        let body = self.buf.take();
        match r {
            Ok(v) => {
                self.need_clear = false;
                let mut chunk = head;
                chunk.extend_from_slice(&body);
                let _ = queue!(chunk, terminal::EndSynchronizedUpdate);
                self.out.send(chunk);
                Ok(v)
            }
            Err(e) => {
                // Nothing of a failed frame goes out, and the next one
                // redraws everything, so the diff never builds on it.
                self.need_clear = true;
                Err(e)
            }
        }
    }
}

/// A draw error the loop rides out (try again next frame).
pub fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;

    const BEGIN: &[u8] = b"\x1b[?2026h";
    const END: &[u8] = b"\x1b[?2026l";

    /// Open or not, and the chunks written.
    type GateState = Mutex<(bool, Vec<Vec<u8>>)>;

    /// A sink that parks every write until opened, recording each chunk.
    #[derive(Clone, Default)]
    struct Gate {
        st: Arc<(GateState, Condvar)>,
    }

    impl Gate {
        fn open(&self) {
            self.st.0.lock().unwrap().0 = true;
            self.st.1.notify_all();
        }
        fn chunks(&self) -> Vec<Vec<u8>> {
            self.st.0.lock().unwrap().1.clone()
        }
    }

    impl Write for Gate {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let mut g = self.st.0.lock().unwrap();
            while !g.0 {
                g = self.st.1.wait(g).unwrap();
            }
            g.1.push(b.to_vec());
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    /// A screen over a test backend whose output lands in a FrameBuf, as
    /// the real one does (the crossterm backend needs a tty for its size).
    struct TeeBackend {
        test: TestBackend,
        inner: CrosstermBackend<FrameBuf>,
    }

    impl Backend for TeeBackend {
        type Error = io::Error;
        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            let cells: Vec<_> = content.collect();
            self.test
                .draw(cells.iter().copied())
                .map_err(io::Error::other)?;
            self.inner.draw(cells.into_iter())
        }
        fn hide_cursor(&mut self) -> io::Result<()> {
            self.inner.hide_cursor()
        }
        fn show_cursor(&mut self) -> io::Result<()> {
            self.inner.show_cursor()
        }
        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Ok(Position::ORIGIN)
        }
        fn set_cursor_position<P: Into<Position>>(&mut self, p: P) -> io::Result<()> {
            self.inner.set_cursor_position(p)
        }
        fn clear(&mut self) -> io::Result<()> {
            self.inner.clear()
        }
        fn clear_region(&mut self, t: ClearType) -> io::Result<()> {
            self.inner.clear_region(t)
        }
        fn size(&self) -> io::Result<Size> {
            Ok(self.test.size().unwrap())
        }
        fn window_size(&mut self) -> io::Result<WindowSize> {
            Ok(self.test.window_size().unwrap())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn screen(out: Out, w: u16, h: u16) -> Screen<TeeBackend> {
        let buf = FrameBuf::default();
        let be = TeeBackend {
            test: TestBackend::new(w, h),
            inner: CrosstermBackend::new(buf.clone()),
        };
        Screen::with_terminal(Terminal::new(be).unwrap(), buf, out)
    }

    fn text(n: usize) -> impl FnOnce(&mut Frame) {
        move |f: &mut Frame| {
            let s = (n % 10).to_string().repeat(4000);
            f.render_widget(
                Paragraph::new(s).wrap(ratatui::widgets::Wrap { trim: false }),
                f.area(),
            );
        }
    }

    /// The event loop's draw scheduling, as main runs it, for `run` long.
    /// Returns (iterations, frames drawn, longest iteration).
    fn spin<B: Backend>(s: &mut Screen<B>, run: Duration) -> (u32, usize, Duration)
    where
        B::Error: Into<io::Error>,
    {
        let start = Instant::now();
        let (mut iters, mut frames, mut worst) = (0u32, 0usize, Duration::ZERO);
        while start.elapsed() < run {
            let t = Instant::now();
            iters += 1;
            // Every turn an event arrives and the screen is dirty; it stays
            // dirty until the writer is free.
            let _ = s.check();
            if s.ready() {
                s.draw(text(frames), |_| ()).unwrap();
                frames += 1;
            }
            worst = worst.max(t.elapsed());
            std::thread::sleep(Duration::from_millis(2));
        }
        (iters, frames, worst)
    }

    #[test]
    fn a_stuck_writer_never_blocks_the_loop_and_frames_wait() {
        let gate = Gate::default();
        let out = Out::spawn("test-out", gate.clone()).unwrap();
        let mut s = screen(out.clone(), 80, 24);
        s.set_stall_after(Duration::from_millis(100));
        let (iters, frames, worst) = spin(&mut s, Duration::from_millis(400));
        // The loop kept turning; one frame went to the writer and the rest
        // waited (kept dirty) instead of piling up or being dropped.
        // Loose bounds for slow CI runners (a 2 ms sleep can take 10 ms
        // there); a loop blocked on the writer would turn once in 400 ms.
        assert!(iters > 10, "loop stalled: {iters} iterations");
        assert!(
            worst < Duration::from_millis(200),
            "an iteration took {worst:?}"
        );
        assert_eq!(frames, 1);
        assert!(!out.idle());
        assert!(s.stalled(), "the stall is noticed");
        assert!(out.stalled_for() >= Duration::from_millis(100));

        // The reader resumes: the frame lands, a full redraw follows.
        gate.open();
        assert!(out.wait_idle(Duration::from_secs(2)));
        assert!(matches!(s.check(), Some(Health::Recovered(_))));
        let (_, more, _) = spin(&mut s, Duration::from_millis(100));
        assert!(more > 1, "drawing resumed");
        assert!(out.wait_idle(Duration::from_secs(2)));
        let chunks = gate.chunks();
        assert_eq!(chunks.len(), 1 + more, "every produced frame was written");
        for c in &chunks {
            assert!(c.starts_with(BEGIN), "chunk starts the synchronized update");
            assert!(c.ends_with(END), "chunk ends it");
            assert_eq!(count(c, BEGIN), 1);
            assert_eq!(count(c, END), 1);
        }
        // The frame after recovery cleared the screen (full redraw).
        assert!(count(&chunks[1], b"\x1b[2J") == 1);
    }

    #[test]
    fn frames_queued_behind_a_stuck_writer_keep_their_order() {
        let gate = Gate::default();
        let out = Out::spawn("test-out", gate.clone()).unwrap();
        let mut s = screen(out.clone(), 40, 10);
        for n in 0..5 {
            // Drawn regardless of ready(): queued, never dropped.
            s.draw(text(n), |_| ()).unwrap();
        }
        gate.open();
        assert!(out.wait_idle(Duration::from_secs(2)));
        let chunks = gate.chunks();
        assert_eq!(chunks.len(), 5);
        for (n, c) in chunks.iter().enumerate() {
            assert!(c.starts_with(BEGIN) && c.ends_with(END));
            let s = String::from_utf8_lossy(c);
            assert!(
                s.contains(&n.to_string().repeat(20)),
                "chunk {n} out of order: {s:?}"
            );
        }
    }

    #[test]
    fn a_pipe_nobody_reads_stalls_only_the_writer_then_recovers() {
        let (mut rd, wr) = std::io::pipe().unwrap();
        let out = Out::spawn("test-out", wr).unwrap();
        let mut s = screen(out.clone(), 200, 60);
        s.set_stall_after(Duration::from_millis(100));
        // Far more than a pipe holds: the writer blocks in write().
        let start = Instant::now();
        let mut produced = 0;
        while out.stalled_for() < Duration::from_millis(150) {
            assert!(start.elapsed() < Duration::from_secs(5), "never filled");
            if s.ready() {
                s.draw(text(produced), |_| ()).unwrap();
                produced += 1;
            } else {
                // A huge raw chunk too, so the pipe surely fills.
                out.send(vec![b'x'; 256 * 1024]);
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = s.check();
        }
        assert!(s.stalled());
        let t = Instant::now();
        s.draw(text(produced), |_| ()).unwrap();
        produced += 1;
        assert!(t.elapsed() < Duration::from_millis(50), "draw blocked");

        // The reader resumes.
        let reader = std::thread::spawn(move || {
            let mut all = Vec::new();
            let _ = std::io::Read::read_to_end(&mut rd, &mut all);
            all
        });
        assert!(out.wait_idle(Duration::from_secs(5)), "writer recovered");
        assert!(matches!(s.check(), Some(Health::Recovered(_))));
        // Close the write end so the reader sees EOF.
        assert!(out.finish(Vec::new(), Duration::from_secs(1)));
        drop(s);
        drop(out);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !reader.is_finished() {
            assert!(Instant::now() < deadline, "reader never saw EOF");
            std::thread::sleep(Duration::from_millis(10));
        }
        let all = reader.join().unwrap();
        assert_eq!(count(&all, BEGIN), produced);
        assert_eq!(count(&all, END), produced);
        // Never interleaved: every begin is closed before the next one.
        let mut open = false;
        for i in 0..all.len() {
            if all[i..].starts_with(BEGIN) {
                assert!(!open, "frames interleaved");
                open = true;
            } else if all[i..].starts_with(END) {
                assert!(open);
                open = false;
            }
        }
        assert!(!open);
    }

    #[test]
    fn shutdown_returns_within_the_bound_while_the_writer_is_stuck() {
        let gate = Gate::default();
        let out = Out::spawn("test-out", gate.clone()).unwrap();
        let mut s = screen(out.clone(), 40, 10);
        s.draw(text(0), |_| ()).unwrap();
        let t = Instant::now();
        let done = out.finish(b"restore".to_vec(), Duration::from_millis(300));
        let took = t.elapsed();
        assert!(!done);
        assert!(took >= Duration::from_millis(290), "{took:?}");
        assert!(
            took < Duration::from_millis(1000),
            "shutdown hung: {took:?}"
        );
        gate.open();
        // Once free, the writer still gets the restore out, then ends.
        assert!(out.wait_idle(Duration::from_secs(2)));
        assert_eq!(gate.chunks().last().unwrap(), b"restore");
    }

    #[test]
    fn shutdown_with_a_healthy_writer_gets_the_restore_out() {
        let gate = Gate::default();
        gate.open();
        let out = Out::spawn("test-out", gate.clone()).unwrap();
        out.send(b"hello".to_vec());
        assert!(out.finish(b"bye".to_vec(), Duration::from_secs(1)));
        assert_eq!(gate.chunks(), vec![b"hello".to_vec(), b"bye".to_vec()]);
        // A second finish (the panic hook after the restore) is immediate.
        let t = Instant::now();
        assert!(out.finish(b"again".to_vec(), Duration::from_secs(1)));
        assert!(t.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn clear_next_redraws_in_full_inside_one_chunk() {
        let gate = Gate::default();
        gate.open();
        let out = Out::spawn("test-out", gate.clone()).unwrap();
        let mut s = screen(out.clone(), 20, 5);
        s.draw(text(0), |_| ()).unwrap();
        assert!(out.wait_idle(Duration::from_secs(1)));
        assert!(!s.need_clear);
        let before = gate.chunks().len();
        s.clear_next();
        s.draw(text(1), |_| ()).unwrap();
        assert!(out.wait_idle(Duration::from_secs(1)));
        let chunks = gate.chunks();
        assert_eq!(chunks.len(), before + 1);
        let last = chunks.last().unwrap();
        assert_eq!(count(last, b"\x1b[2J"), 1);
        assert!(last.starts_with(BEGIN) && last.ends_with(END));
    }

    #[test]
    fn transient_errors_are_ridden_out() {
        assert!(transient(&io::Error::from(io::ErrorKind::WouldBlock)));
        assert!(transient(&io::Error::from(io::ErrorKind::Interrupted)));
        assert!(!transient(&io::Error::other("gone")));
    }

    #[test]
    fn the_buf_backend_never_queries_the_terminal_for_the_cursor() {
        let buf = FrameBuf::default();
        let mut be = BufBackend::new(buf.clone());
        assert_eq!(be.get_cursor_position().unwrap(), Position::ORIGIN);
        be.set_cursor_position(Position::new(3, 4)).unwrap();
        assert_eq!(be.get_cursor_position().unwrap(), Position::new(3, 4));
        let bytes = buf.take();
        assert!(!bytes.windows(4).any(|w| w == b"\x1b[6n"));
    }
}
