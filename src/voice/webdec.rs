//! Compressed mic audio from a browser: the web app's last fallback when
//! Web Audio gives it no samples (seen on iPhone home screen apps). The
//! page records with MediaRecorder (AAC in MP4 on iOS, Opus in WebM
//! elsewhere) in small pieces, and ffmpeg decodes the stream as it comes
//! into 16 kHz mono PCM16, which goes to the voice engine exactly as the
//! PCM the page sends itself.
//!
//! A recording that ffmpeg cannot read as a stream (an MP4 whose index is
//! written at the end) is decoded again as a whole when it ends.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// ffmpeg's path, set by the app (voice.ffmpeg).
static FFMPEG: Mutex<String> = Mutex::new(String::new());

pub fn set_ffmpeg(path: &str) {
    *FFMPEG.lock().unwrap_or_else(|e| e.into_inner()) = path.to_string();
}

fn ffmpeg() -> String {
    let p = FFMPEG.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if p.is_empty() {
        "ffmpeg".into()
    } else {
        p
    }
}

/// ffmpeg's input format for a MediaRecorder type.
pub fn input_format(mime: &str) -> Option<&'static str> {
    let m = mime.to_ascii_lowercase();
    if m.contains("mp4") || m.contains("aac") || m.contains("m4a") {
        Some("mov")
    } else if m.contains("webm") || m.contains("matroska") {
        Some("matroska")
    } else if m.contains("ogg") {
        Some("ogg")
    } else {
        None
    }
}

/// Where the decoded PCM goes.
pub type PcmSink = Box<dyn FnMut(&[u8]) + Send>;
type Sink = Arc<Mutex<PcmSink>>;

/// One recording being decoded.
pub struct Decoder {
    pub mime: String,
    tx: Option<mpsc::Sender<Vec<u8>>>,
    child: Arc<Mutex<Child>>,
    /// PCM bytes decoded so far.
    out_bytes: Arc<Mutex<u64>>,
    /// The reader finished (ffmpeg's output ended).
    done: Arc<(Mutex<bool>, std::sync::Condvar)>,
    /// Everything that came, for a decode of the whole when the stream
    /// gave nothing.
    all: Vec<u8>,
    sink: Sink,
    pub in_bytes: u64,
    pub chunks: u64,
    pub started: Instant,
    err: Arc<Mutex<String>>,
}

fn spawn(mime: &str, input: Option<&std::path::Path>) -> std::io::Result<Child> {
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    match input {
        Some(p) => {
            cmd.arg("-nostdin").arg("-i").arg(p);
        }
        None => {
            cmd.args(["-fflags", "+nobuffer", "-probesize", "32768"]);
            if let Some(f) = input_format(mime) {
                cmd.args(["-f", f]);
            }
            cmd.args(["-i", "pipe:0"]);
        }
    }
    cmd.args([
        "-vn",
        "-ac",
        "1",
        "-ar",
        "16000",
        "-f",
        "s16le",
        "-flush_packets",
        "1",
        "pipe:1",
    ]);
    let child = cmd
        .stdin(if input.is_some() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    super::track(child.id());
    Ok(child)
}

/// Read ffmpeg's PCM into the sink (whole samples), and its errors.
fn pipe_out(
    child: &mut Child,
    sink: Sink,
    out_bytes: Arc<Mutex<u64>>,
    done: Arc<(Mutex<bool>, std::sync::Condvar)>,
    err: Arc<Mutex<String>>,
) {
    if let Some(mut so) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut odd: Option<u8> = None;
            loop {
                let n = match so.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let mut v = Vec::with_capacity(n + 1);
                v.extend(odd.take());
                v.extend_from_slice(&buf[..n]);
                if v.len() % 2 == 1 {
                    odd = v.pop();
                }
                *out_bytes.lock().unwrap_or_else(|e| e.into_inner()) += v.len() as u64;
                (sink.lock().unwrap_or_else(|e| e.into_inner()))(&v);
            }
            let (m, cv) = &*done;
            *m.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        });
    }
    if let Some(mut se) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = se.read_to_string(&mut s);
            let s = s.trim();
            if !s.is_empty() {
                let last: Vec<&str> = s.lines().rev().take(2).collect();
                *err.lock().unwrap_or_else(|e| e.into_inner()) = last.join(" / ");
            }
        });
    }
}

impl Decoder {
    /// Start decoding a recording of type `mime`; PCM16 (16 kHz mono,
    /// little endian, whole samples) goes to `sink` as it is decoded.
    pub fn start(mime: &str, sink: PcmSink) -> std::io::Result<Decoder> {
        let mut child = spawn(mime, None)?;
        let sink: Sink = Arc::new(Mutex::new(sink));
        let out_bytes = Arc::new(Mutex::new(0));
        let done = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let err = Arc::new(Mutex::new(String::new()));
        pipe_out(
            &mut child,
            Arc::clone(&sink),
            Arc::clone(&out_bytes),
            Arc::clone(&done),
            Arc::clone(&err),
        );
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        if let Some(mut si) = child.stdin.take() {
            // Writes on their own thread: a slow ffmpeg never holds up the
            // connection's reader.
            std::thread::spawn(move || {
                while let Ok(b) = rx.recv() {
                    if si.write_all(&b).is_err() {
                        break;
                    }
                    let _ = si.flush();
                }
            });
        }
        Ok(Decoder {
            mime: mime.to_string(),
            tx: Some(tx),
            child: Arc::new(Mutex::new(child)),
            out_bytes,
            done,
            all: vec![],
            sink,
            in_bytes: 0,
            chunks: 0,
            started: Instant::now(),
            err,
        })
    }

    /// A piece of the recording.
    pub fn push(&mut self, bytes: &[u8]) {
        self.in_bytes += bytes.len() as u64;
        self.chunks += 1;
        // (At most 20 MB kept for the decode of the whole.)
        if self.all.len() < 20 << 20 {
            self.all.extend_from_slice(bytes);
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(bytes.to_vec());
        }
    }

    /// PCM bytes decoded so far.
    pub fn decoded(&self) -> u64 {
        *self.out_bytes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The recording ended: decode what is left (waits up to `wait`). When
    /// the stream gave nothing, the whole recording is decoded from a file.
    /// Returns a line for the log.
    pub fn finish(&mut self, wait: Duration) -> String {
        self.tx = None;
        let until = Instant::now() + wait;
        {
            let (m, cv) = &*self.done;
            let mut d = m.lock().unwrap_or_else(|e| e.into_inner());
            while !*d {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                d = cv
                    .wait_timeout(d, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        let mut how = "streamed";
        if self.decoded() == 0 && !self.all.is_empty() {
            how = "decoded as a whole";
            self.whole(until);
        }
        let e = self.err.lock().unwrap_or_else(|e| e.into_inner()).clone();
        format!(
            "{} recording, {} pieces, {} KB in, {} ms of audio out ({how}){}",
            self.mime,
            self.chunks,
            self.in_bytes / 1024,
            self.decoded() / 32,
            if e.is_empty() {
                String::new()
            } else {
                format!(", ffmpeg: {e}")
            }
        )
    }

    fn whole(&mut self, until: Instant) {
        let ext = match input_format(&self.mime) {
            Some("mov") => "mp4",
            Some("ogg") => "ogg",
            _ => "webm",
        };
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "godterm-webrec-{}-{}.{ext}",
            std::process::id(),
            self.started.elapsed().as_nanos()
        ));
        if std::fs::write(&path, &self.all).is_err() {
            return;
        }
        if let Ok(mut c) = spawn(&self.mime, Some(&path)) {
            let done = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
            pipe_out(
                &mut c,
                Arc::clone(&self.sink),
                Arc::clone(&self.out_bytes),
                Arc::clone(&done),
                Arc::clone(&self.err),
            );
            let (m, cv) = &*done;
            let mut d = m.lock().unwrap_or_else(|e| e.into_inner());
            while !*d {
                let left = until
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(500));
                let (g, t) = cv.wait_timeout(d, left).unwrap_or_else(|e| e.into_inner());
                d = g;
                if t.timed_out() {
                    break;
                }
            }
            drop(d);
            let _ = c.kill();
            let _ = c.wait();
            super::untrack(c.id());
        }
        let _ = std::fs::remove_file(&path);
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.tx = None;
        let mut c = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = c.kill();
        let _ = c.wait();
        super::untrack(c.id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_for_media_recorder_types() {
        assert_eq!(input_format("audio/mp4"), Some("mov"));
        assert_eq!(input_format("audio/mp4;codecs=mp4a.40.2"), Some("mov"));
        assert_eq!(input_format("audio/webm;codecs=opus"), Some("matroska"));
        assert_eq!(input_format("audio/ogg;codecs=opus"), Some("ogg"));
        assert_eq!(input_format("audio/x-unknown"), None);
    }

    /// A real recording (a tone in AAC/MP4 and Opus/WebM made by ffmpeg),
    /// fed in small pieces: PCM comes out, about as long as it went in.
    #[test]
    fn decodes_a_recording_fed_in_pieces() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("gt-webdec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (mime, file, extra) in [
            (
                "audio/mp4",
                "t.mp4",
                &["-c:a", "aac", "-movflags", "frag_keyframe+empty_moov"][..],
            ),
            ("audio/mp4", "t2.mp4", &["-c:a", "aac"][..]),
            ("audio/webm;codecs=opus", "t.webm", &["-c:a", "libopus"][..]),
        ] {
            let p = dir.join(file);
            let ok = Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
                .args(["-i", "sine=frequency=440:duration=2:sample_rate=48000"])
                .args(extra)
                .arg(&p)
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                continue;
            }
            let bytes = std::fs::read(&p).unwrap();
            let got = Arc::new(Mutex::new(0usize));
            let g = Arc::clone(&got);
            let mut d =
                Decoder::start(mime, Box::new(move |b| *g.lock().unwrap() += b.len())).unwrap();
            for c in bytes.chunks(3000) {
                d.push(c);
            }
            let line = d.finish(Duration::from_secs(10));
            let ms = *got.lock().unwrap() / 32;
            assert!((1700..=2300).contains(&ms), "{file}: {ms} ms ({line})");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
