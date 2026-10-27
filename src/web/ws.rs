use super::tap::{self, Out};
use super::{Client, Hub, ToApp};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tungstenite::protocol::{Role, WebSocketConfig};
use tungstenite::{Message, WebSocket};

pub const KIND_OUTPUT: u8 = 1;
pub const KIND_INPUT: u8 = 1;

pub const KIND_AUDIO: u8 = 2;

pub const KIND_MIC: u8 = 3;

pub const KIND_MIC_REC: u8 = 4;

pub const KIND_SNAPSHOT_GZ: u8 = 6;
pub const GZ_MIN: usize = 16 << 10;

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut z = flate2::write::GzEncoder::new(
        Vec::with_capacity(bytes.len() / 4),
        flate2::Compression::default(),
    );
    let _ = z.write_all(bytes);
    z.finish().unwrap_or_default()
}

const MAX_IMAGES: usize = 6;
const MAX_IMAGE_BYTES: usize = 20 << 20;

const PING_EVERY: Duration = Duration::from_secs(25);
const READ_IDLE: Duration = Duration::from_secs(90);

struct ReadSide(TcpStream);

impl Read for ReadSide {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for ReadSide {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct WriteSide(TcpStream);

impl Read for WriteSide {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}

impl Write for WriteSide {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

pub fn frame(kind: u8, uid: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + payload.len());
    v.push(kind);
    v.extend_from_slice(&(uid as u32).to_be_bytes());
    v.extend_from_slice(payload);
    v
}

pub fn unframe(b: &[u8]) -> Option<(u8, u64, &[u8])> {
    if b.len() < 5 {
        return None;
    }
    let uid = u32::from_be_bytes([b[1], b[2], b[3], b[4]]) as u64;
    Some((b[0], uid, &b[5..]))
}

pub fn accept(hub: &Arc<Hub>, mut s: TcpStream, req: &super::http::Request) -> std::io::Result<()> {
    let key = req.header("sec-websocket-key").unwrap_or("");
    if key.is_empty() || req.header("sec-websocket-version") != Some("13") {
        return super::http::text(&mut s, "400 Bad Request", "bad websocket request");
    }
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    s.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .as_bytes(),
    )?;
    s.set_read_timeout(Some(READ_IDLE))?;
    let rd = s.try_clone()?;
    let shut = s.try_clone()?;
    let client = hub.add();
    let mut cfg = WebSocketConfig::default();
    cfg.max_message_size = Some(32 << 20);
    cfg.max_frame_size = Some(32 << 20);
    let reader = WebSocket::from_raw_socket(ReadSide(rd), Role::Server, Some(cfg));
    let writer = WebSocket::from_raw_socket(WriteSide(s), Role::Server, None);
    let c = Arc::clone(&client);
    let wshut = shut.try_clone()?;
    std::thread::Builder::new()
        .name(format!("web-out-{}", client.id))
        .spawn(move || {
            write_loop(writer, &c);
            c.out.close();
            let _ = wshut.shutdown(std::net::Shutdown::Both);
        })?;
    hub.send(ToApp::Hello(client.id));
    crate::log::info(&format!("web: client {} connected", client.id));
    let mut st = ReadState::default();
    read_loop(hub, reader, &client, &mut st);
    client.out.close();
    let subs: Vec<u64> = st.subs.iter().copied().collect();
    tap::drop_client(client.id, &subs);
    hub.remove(client.id);
    let _ = shut.shutdown(std::net::Shutdown::Both);
    crate::log::info(&format!("web: client {} left", client.id));
    Ok(())
}

fn write_loop<S: Read + Write>(mut ws: WebSocket<S>, c: &Client) {
    let mut last_ping = Instant::now();
    let mut pending = false;
    loop {
        let due = PING_EVERY.saturating_sub(last_ping.elapsed());
        if due.is_zero() {
            if ws.send(Message::Ping(Vec::new().into())).is_err() {
                return;
            }
            last_ping = Instant::now();
            pending = false;
            continue;
        }
        let Some(f) = c.out.pop(if pending { Duration::ZERO } else { due }) else {
            if pending {
                if ws.flush().is_err() {
                    return;
                }
                pending = false;
            }
            continue;
        };
        let msg = match f {
            Out::Text(s) => Message::text(s),
            Out::Snapshot(uid, s) => {
                if s.len() >= GZ_MIN && c.gz.load(std::sync::atomic::Ordering::Relaxed) {
                    Message::binary(frame(KIND_SNAPSHOT_GZ, uid, &gzip(s.as_bytes())))
                } else {
                    Message::text(s)
                }
            }
            Out::Output(uid, b) => Message::binary(frame(KIND_OUTPUT, uid, &b)),
            Out::Audio(b) => Message::binary(frame(KIND_AUDIO, 0, &b)),
            Out::Picture(b) => Message::binary(b),
            Out::Resync(uid) => {
                if let Some(t) = tap::get(uid) {
                    t.resnapshot(c.id, &c.out);
                }
                continue;
            }
            Out::Pong(d) => Message::Pong(d.into()),
            Out::Close => {
                let _ = ws.close(None);
                let _ = ws.flush();
                return;
            }
        };

        if ws.write(msg).is_err() {
            return;
        }
        pending = true;
    }
}

#[derive(Default)]
struct ReadState {
    subs: HashSet<u64>,

    typed: HashMap<u64, Instant>,

    mic_log: Option<Instant>,
    mic_stray: u32,

    log: super::clientlog::Rate,

    rec: Option<crate::voice::webdec::Decoder>,
}

fn rec_start(c: &Arc<Client>, st: &mut ReadState, mime: &str) {
    if let Some(mut old) = st.rec.take() {
        let line = old.finish(Duration::from_millis(300));
        crate::log::info(&format!("web: client {} mic {line} (replaced)", c.id));
    }
    let cc = Arc::clone(c);
    let mut lvl = (Instant::now(), 0f64, 0u64, 0u64);
    let sink = move |b: &[u8]| {
        let mic = cc.mic.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(m) = mic {
            m.push(b);
        }
        for s in b.chunks_exact(2) {
            let v = f64::from(i16::from_le_bytes([s[0], s[1]]));
            lvl.1 += v * v;
            lvl.2 += 1;
        }
        lvl.3 += b.len() as u64;
        if lvl.0.elapsed() >= Duration::from_millis(150) && lvl.2 > 0 {
            let db = 20.0 * ((lvl.1 / lvl.2 as f64).sqrt() / 32768.0 + 1e-9).log10();
            cc.out.text(
                json!({"t": "voice_level", "db": (db * 10.0).round() / 10.0, "ms": lvl.3 / 32})
                    .to_string(),
            );
            lvl = (Instant::now(), 0.0, 0, lvl.3);
        }
    };
    match crate::voice::webdec::Decoder::start(mime, Box::new(sink)) {
        Ok(d) => {
            crate::log::info(&format!(
                "web: client {} mic as a recording ({mime}), decoded with ffmpeg",
                c.id
            ));
            st.rec = Some(d);
        }
        Err(e) => {
            crate::log::info(&format!(
                "web: client {} mic recording: ffmpeg did not start: {e}",
                c.id
            ));
            c.out.text(
                json!({"t": "voice", "ok": false, "code": "decoder", "error": format!("GodTerm could not decode the recording (ffmpeg: {e})")})
                    .to_string(),
            );
        }
    }
}

pub fn web_tts(c: &Client, t: &Value) {
    let r = crate::voice::web_tts::parse(t);
    let reply = match &r {
        Ok(choice) => {
            let mut held = c.tts.lock().unwrap_or_else(|e| e.into_inner());
            *held = Some(choice.clone());

            crate::voice::web_tts::choice_changed(c.id, Some(choice.clone()));
            crate::log::info(&format!(
                "web: client {} chose talk back {} {}",
                c.id,
                choice.engine,
                if choice.voice.is_empty() {
                    "(default voice)"
                } else {
                    choice.voice.as_str()
                }
            ));
            json!({"t": "voice", "web_tts": choice.json(), "ok": true})
        }
        Err(e) => {
            crate::log::info(&format!(
                "web: client {} talk back choice refused: {e}",
                c.id
            ));
            json!({"t": "voice", "web_tts": t, "ok": false, "error": e})
        }
    };
    c.out.text(reply.to_string());
}

fn rec_end(c: &Client, st: &mut ReadState) {
    if let Some(mut d) = st.rec.take() {
        let line = d.finish(Duration::from_secs(3));
        crate::log::info(&format!("web: client {} mic {line}", c.id));
    }
}

fn on_mic(c: &Client, st: &mut ReadState, data: &[u8]) {
    let mic = c.mic.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(m) = mic else {
        st.mic_stray += 1;
        if st.mic_stray == 50 {
            crate::log::info(&format!(
                "web: client {} sends mic audio but is not the voice; dropped",
                c.id
            ));
        }
        return;
    };
    st.mic_stray = 0;
    m.push(data);
    if !crate::voice::audio::voice_debug() {
        return;
    }
    let now = Instant::now();
    let Some(t) = st.mic_log else {
        st.mic_log = Some(now);
        return;
    };
    if now.duration_since(t) >= Duration::from_secs(1) {
        st.mic_log = Some(now);
        let (s, open) = m.take_second();
        crate::log::info(&format!(
            "voice debug: client {} mic: {} in the last second, {}",
            c.id,
            s.line(),
            if open {
                "the engine reads it"
            } else {
                "no capture open"
            }
        ));
    }
}

fn read_loop<S: Read + Write>(
    hub: &Arc<Hub>,
    mut ws: WebSocket<S>,
    c: &Arc<Client>,
    st: &mut ReadState,
) {
    loop {
        let msg = match ws.read() {
            Ok(m) => m,
            Err(_) => return,
        };
        match msg {
            Message::Text(t) => {
                let v: Value = match serde_json::from_str(t.as_str()) {
                    Ok(v) => v,
                    Err(e) => {
                        c.out.text(error_frame(&format!("bad frame: {e}"), None));
                        continue;
                    }
                };
                on_text(hub, c, st, &v);
            }
            Message::Binary(b) => {
                let Some((kind, uid, data)) = unframe(&b) else {
                    continue;
                };
                if kind == KIND_INPUT {
                    on_input(hub, c, st, uid, data);
                } else if kind == KIND_MIC {
                    on_mic(c, st, data);
                } else if kind == KIND_MIC_REC {
                    match st.rec.as_mut() {
                        Some(d) => d.push(data),
                        None => {
                            st.mic_stray += 1;
                            if st.mic_stray == 5 {
                                crate::log::info(&format!(
                                    "web: client {} sends a mic recording with no voice {{rec}} first; dropped",
                                    c.id
                                ));
                            }
                        }
                    }
                }
            }
            Message::Ping(d) => c.out.pong(d.to_vec()),
            Message::Close(_) => return,
            _ => {}
        }
        if c.out.is_closed() {
            return;
        }
    }
}

fn error_frame(msg: &str, id: Option<&Value>) -> String {
    let mut v = json!({"t": "error", "error": msg});
    if let Some(id) = id {
        v["id"] = id.clone();
    }
    v.to_string()
}

fn uids(v: &Value) -> Vec<u64> {
    let one = |x: &Value| {
        x.as_u64().or_else(|| {
            x.as_str()
                .and_then(|s| s.trim_start_matches('t').parse().ok())
        })
    };
    match v {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        x => one(x).into_iter().collect(),
    }
}

fn on_input(hub: &Arc<Hub>, c: &Arc<Client>, st: &mut ReadState, uid: u64, data: &[u8]) {
    let Some(t) = tap::get(uid) else {
        c.out.text(error_frame("no such tab", Some(&json!(uid))));
        return;
    };
    if !t.write(data) {
        c.out
            .text(json!({"t": "error", "error": "the tab is not running", "tab": uid}).to_string());
        return;
    }

    let now = Instant::now();
    if st
        .typed
        .get(&uid)
        .is_none_or(|t| now.duration_since(*t) >= Duration::from_millis(200))
    {
        st.typed.insert(uid, now);
        hub.send(ToApp::Typed(uid));
    }
}

fn on_text(hub: &Arc<Hub>, c: &Arc<Client>, st: &mut ReadState, v: &Value) {
    let t = v["t"].as_str().unwrap_or("");
    match t {
        "subscribe" => {
            if v["gz"].as_bool() == Some(true) {
                c.gz.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            for uid in uids(&v["tabs"]) {
                match tap::get(uid) {
                    Some(tap) => {
                        let r = &v["resume"][uid.to_string()];
                        match (r["sid"].as_str(), r["pos"].as_u64()) {
                            (Some(sid), Some(pos)) => {
                                let from = tap::Resume {
                                    sid: sid.to_string(),
                                    pos,
                                };
                                tap.subscribe_from(c.id, &c.out, &from);
                            }
                            _ => tap.subscribe(c.id, &c.out),
                        }
                        st.subs.insert(uid);
                    }
                    None => {
                        c.out.text(
                            json!({"t": "error", "error": "no such tab", "tab": uid}).to_string(),
                        );
                    }
                }
            }
        }

        "view" => {
            let mut keep = json!({});
            for k in ["device", "panes", "tabs", "focused", "open"] {
                if !v[k].is_null() {
                    keep[k] = v[k].clone();
                }
            }
            *c.view.lock().unwrap_or_else(|e| e.into_inner()) = keep;
        }
        "unsubscribe" => {
            for uid in uids(&v["tabs"]) {
                if let Some(tap) = tap::get(uid) {
                    tap.unsubscribe(c.id);
                }
                st.subs.remove(&uid);
            }
        }
        "input" => {
            if let (Some(uid), Some(d)) = (uids(&v["tab"]).first(), v["data"].as_str()) {
                on_input(hub, c, st, *uid, d.as_bytes());
            }
        }
        "wheel" => {
            let Some(&uid) = uids(&v["tab"]).first() else {
                return;
            };
            let Some(t) = tap::get(uid) else {
                c.out.text(error_frame("no such tab", Some(&json!(uid))));
                return;
            };
            let up = v["up"].as_bool().unwrap_or(true);
            let n = v["n"].as_u64().unwrap_or(1).clamp(1, 64) as u16;
            let at = |k: &str| v[k].as_u64().unwrap_or(0).min(1000) as u16;
            if t.wheel(up, n, at("col"), at("row")).is_none() {
                c.out
                    .text(json!({"t": "wheel", "tab": uid, "taken": false}).to_string());
            }
        }
        "resize" => {
            let Some(&uid) = uids(&v["tab"]).first() else {
                return;
            };
            let (rows, cols) = (v["rows"].as_u64(), v["cols"].as_u64());
            let size = match (rows, cols) {
                _ if v["release"].as_bool() == Some(true) => None,
                (Some(r), Some(k)) if r > 0 && k > 0 => {
                    Some((r.min(500) as u16, k.min(1000) as u16))
                }
                _ => None,
            };

            let by = v["by"].as_str().map(|s| {
                s.chars()
                    .filter(|c| c.is_alphanumeric() || *c == ' ')
                    .take(24)
                    .collect::<String>()
            });
            hub.send(ToApp::Resize {
                client: c.id,
                uid,
                size,
                by: by.filter(|s| !s.trim().is_empty()),
            });
        }
        "say" => {
            let text = v["text"].as_str().unwrap_or("").trim().to_string();
            let mut images = vec![];
            for i in v["images"]
                .as_array()
                .into_iter()
                .flatten()
                .take(MAX_IMAGES)
            {
                let s = i.as_str().unwrap_or("");
                let b64 = s.split_once("base64,").map_or(s, |(_, b)| b);
                match crate::assistant_images::unbase64(b64) {
                    Some(b) if !b.is_empty() && b.len() <= MAX_IMAGE_BYTES => images.push(b),
                    _ => {
                        c.out.text(error_frame("an image could not be read", None));
                    }
                }
            }
            if !text.is_empty() || !images.is_empty() {
                hub.send(ToApp::Say {
                    client: c.id,
                    text,
                    images,
                });
            }
        }
        "image" => {
            let s = v["data"].as_str().unwrap_or("");
            let b64 = s.split_once("base64,").map_or(s, |(_, b)| b);
            let saved = match crate::assistant_images::unbase64(b64) {
                Some(b) if !b.is_empty() && b.len() <= MAX_IMAGE_BYTES => {
                    crate::assistant_images::save_bytes(&b).map_err(|e| format!("{e:#}"))
                }
                Some(b) if b.len() > MAX_IMAGE_BYTES => Err("the image is over 20 MB".into()),
                _ => Err("the image could not be read".into()),
            };
            c.out.text(match saved {
                Ok((path, w, h)) => {
                    json!({"t": "image", "id": v["id"], "path": path, "w": w, "h": h}).to_string()
                }
                Err(e) => error_frame(&e, Some(&v["id"])),
            });
        }
        "file" => {
            let s = v["data"].as_str().unwrap_or("");
            let b64 = s.split_once("base64,").map_or(s, |(_, b)| b);
            let name = v["name"].as_str().unwrap_or("file");
            let saved = match crate::assistant_images::unbase64(b64) {
                Some(b) if b.len() > MAX_IMAGE_BYTES => Err("the file is over 20 MB".into()),
                Some(b) => {
                    crate::assistant_images::save_upload(name, &b).map_err(|e| format!("{e:#}"))
                }
                None => Err("the file could not be read".into()),
            };
            c.out.text(match saved {
                Ok(path) => json!({"t": "file", "id": v["id"], "path": path}).to_string(),
                Err(e) => error_frame(&e, Some(&v["id"])),
            });
        }
        "answer" => {
            let id = v["id"].as_str().unwrap_or("").to_string();
            let choice = v["choice"].as_str().unwrap_or("").to_string();
            hub.send(ToApp::Answer {
                client: c.id,
                id,
                choice,
            });
        }
        "voice" => {
            if let Some(d) = v["debug"].as_bool() {
                crate::voice::audio::VOICE_DEBUG.store(d, std::sync::atomic::Ordering::Relaxed);
                crate::log::info(&format!(
                    "web: client {} turned voice debugging {}",
                    c.id,
                    if d { "on" } else { "off" }
                ));
            }
            if let Some(l) = v["log"].as_str() {
                let l: String = l.chars().take(400).collect();
                crate::log::info(&format!("web: client {} voice: {l}", c.id));
            }
            if let Some(m) = v["rec"].as_str() {
                rec_start(c, st, m);
            }
            if v["rec_end"].as_bool() == Some(true) {
                rec_end(c, st);
            }
            if let Some(t) = v.get("web_tts") {
                web_tts(c, t);
            }

            if let Some(p) = v["playing"].as_bool() {
                if let Some(m) = c.mic.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    m.set_playing(p);
                }
            }

            if [
                "direct",
                "brainstorm",
                "hush",
                "away",
                "muted",
                "unmute_mac",
                "set",
                "web_tts_list",
            ]
            .iter()
            .any(|k| !v[*k].is_null())
            {
                hub.send(ToApp::VoiceCtl {
                    client: c.id,
                    v: v.clone(),
                });
            }
            if v["on"].is_null() && v["talk"].is_null() {
                return;
            }
            hub.send(ToApp::Voice {
                client: c.id,
                on: v["on"].as_bool(),
                mode: v["mode"].as_str().unwrap_or("push").to_string(),
                talk: v["talk"].as_bool(),
            });
        }
        "tool" => tool_call(hub, c, v),

        "git" => {
            hub.send(ToApp::Git {
                client: c.id,
                v: v.clone(),
            });
        }

        "bview" => {
            hub.send(ToApp::BrowserView {
                client: c.id,
                v: v.clone(),
            });
        }

        "accounts" => {
            hub.send(ToApp::Accounts {
                client: c.id,
                v: v.clone(),
            });
        }
        "push" => super::push::on_frame(c.id, &c.out, v),

        "log" => super::clientlog::take(c.id, v, &mut st.log),
        "ping" => {
            c.out.text(json!({"t": "pong", "id": v["id"]}).to_string());
        }
        other => {
            c.out
                .text(error_frame(&format!("unknown frame type {other:?}"), None));
        }
    }
}

fn tool_call(hub: &Arc<Hub>, c: &Arc<Client>, v: &Value) {
    let id = v["id"].clone();
    let Some(name) = v["name"].as_str().filter(|n| !n.is_empty()) else {
        c.out.text(error_frame("tool needs a name", Some(&id)));
        return;
    };
    let mut args = match &v["args"] {
        Value::Object(_) => v["args"].clone(),
        Value::Null => json!({}),
        _ => {
            c.out.text(error_frame("args must be an object", Some(&id)));
            return;
        }
    };
    args["_client"] = json!("web");
    if let Some(u) = v["user_says"].as_str() {
        crate::log::info(&format!("web: the user answered: {u}"));
        let _ = hub
            .events()
            .send(crate::app::AppEvent::UserTurn(u.to_string()));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    if hub
        .events()
        .send(crate::app::AppEvent::Control(name.to_string(), args, tx))
        .is_err()
    {
        return;
    }
    let out = Arc::clone(&c.out);
    let wait = crate::control::reply_wait(name);
    let _ = std::thread::Builder::new()
        .name("web-tool".into())
        .spawn(move || {
            let r = rx.recv_timeout(wait).unwrap_or_else(
                |_| json!({"ok": false, "error": "godterm did not answer in time"}),
            );
            out.text(json!({"t": "result", "id": id, "result": r}).to_string());
        });
}
