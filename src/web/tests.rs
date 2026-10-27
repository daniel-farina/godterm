use super::auth::{self, Limiter};
use super::tap::{self, Out, Outbox};
use super::*;
use crate::config::testing::LOCK as ENV_LOCK;
use crate::pane::{LaunchKind, LaunchSpec, Pane};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn home(tag: &str) -> std::path::PathBuf {
    let h = std::env::temp_dir().join(format!("godterm-web-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    crate::config::testing::set_home(&h);
    h
}

#[test]
fn constant_time_compare_and_cookies() {
    assert!(auth::same(b"abc", b"abc"));
    assert!(!auth::same(b"abc", b"abd"));
    assert!(!auth::same(b"abc", b"abcd"));
    assert!(!auth::same(b"", b"a"));
    assert_eq!(
        auth::cookie_in("a=1; godterm_web=xyz; b=2", auth::COOKIE),
        Some("xyz")
    );
    assert_eq!(auth::cookie_in("xgodterm_web=1", auth::COOKIE), None);

    let v = auth::cookie_value("tok");
    assert_ne!(v, "tok");
    assert_eq!(v, auth::cookie_value("tok"));
    assert_ne!(v, auth::cookie_value("tok2"));
    let c = auth::set_cookie("tok", false);
    assert!(c.contains("HttpOnly") && c.contains("SameSite=Strict") && !c.contains("Secure"));
    assert!(auth::set_cookie("tok", true).contains("; Secure"));
}

#[test]
fn hosts_and_origins() {
    let none: Vec<String> = vec![];
    for h in [
        "127.0.0.1:7878",
        "localhost:7878",
        "[::1]:7878",
        "mac.tail1234.ts.net",
        "mac.tail1234.ts.net:7878",
    ] {
        assert!(auth::host_ok(h, &none), "{h}");
    }
    for h in [
        "evil.com",
        "127.0.0.1.evil.com:7878",
        "ts.net",
        "localhost.evil.com",
        "",
        "evil.ts.net.com",
    ] {
        assert!(!auth::host_ok(h, &none), "{h}");
    }
    assert!(auth::host_ok("mymac.lan:7878", &["mymac.lan".to_string()]));
    let ok = |o: Option<&str>, h: &str| auth::origin_ok(o, h, None);
    assert!(ok(Some("http://127.0.0.1:7878"), "127.0.0.1:7878"));
    assert!(ok(Some("https://mac.x.ts.net:7878"), "mac.x.ts.net:7878"));

    assert!(!ok(Some("http://127.0.0.1:8080"), "127.0.0.1:7878"));
    assert!(!ok(Some("http://localhost"), "localhost:7878"));
    assert!(!ok(Some("https://evil.com"), "127.0.0.1:7878"));
    assert!(!ok(Some("null"), "127.0.0.1:7878"));
    assert!(!ok(None, "127.0.0.1:7878"));

    assert!(auth::origin_ok(
        Some("https://mac.x.ts.net"),
        "127.0.0.1:7878",
        Some("mac.x.ts.net")
    ));
}

#[test]
fn wrong_tokens_are_rate_limited() {
    let mut l = Limiter::default();
    let t0 = Instant::now();
    for i in 0..auth::MAX_FAILS {
        assert!(!l.blocked(t0), "blocked after {i}");
        l.fail(t0);
    }
    assert!(l.blocked(t0));
    assert!(l.blocked(t0 + Duration::from_secs(30)));
    assert!(!l.blocked(t0 + auth::WINDOW));
}

#[test]
fn the_token_file_is_private_and_kept() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("token");
    let a = auth::load_or_create().unwrap();
    assert_eq!(a.len(), 64);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(auth::token_path(), h.join("web-token"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(auth::token_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(m & 0o777, 0o600);
    }
    assert_eq!(auth::load_or_create().unwrap(), a);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn parses_requests() {
    let r = http::parse("GET /a/b?t=x&v=2 HTTP/1.1\r\nHost: h\r\nCookie: c=1\r\n\r\n").unwrap();
    assert_eq!((r.method.as_str(), r.path.as_str()), ("GET", "/a/b"));
    assert_eq!(r.param("t"), Some("x"));
    assert_eq!(r.query_without("t"), "v=2");
    assert_eq!(r.header("COOKIE"), Some("c=1"));
    assert!(http::parse("GET a HTTP/1.1\r\n\r\n").is_none());
    assert!(http::parse("GET / SPDY\r\n\r\n").is_none());
}

fn test_hub(
    tag: &str,
) -> (
    Arc<Hub>,
    String,
    std::path::PathBuf,
    std::sync::mpsc::Receiver<AppEvent>,
) {
    let h = home(tag);
    let (tx, rx) = std::sync::mpsc::channel();
    let hub = start(0, vec![], tx).unwrap();
    let tok = auth::load_or_create().unwrap();
    (hub, tok, h, rx)
}

fn get(port: u16, head: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(closing(head).as_bytes()).unwrap();
    let mut out = vec![];
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn closing(head: &str) -> String {
    head.replacen("\r\n\r\n", "\r\nConnection: close\r\n\r\n", 1)
}

fn status(resp: &str) -> &str {
    resp.split(' ').nth(1).unwrap_or("")
}

#[test]
fn http_logins_cookies_and_refusals() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, _rx) = test_hub("http");
    let p = hub.port;
    let host = format!("127.0.0.1:{p}");

    let r = get(p, &format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"));
    assert_eq!(status(&r), "401", "{r}");
    assert!(!r.contains("<html"));

    let r = get(
        p,
        &format!("GET /?t={tok} HTTP/1.1\r\nHost: evil.com\r\n\r\n"),
    );
    assert_eq!(status(&r), "421");

    let r = get(
        p,
        &format!("GET /?t={tok}&x=1 HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert_eq!(status(&r), "303", "{r}");
    assert!(r.contains("Location: /?x=1\r\n"), "{r}");
    assert!(!r.to_lowercase().contains(&format!("location: /?t={tok}")));
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    assert!(r.contains(&format!(
        "Set-Cookie: {cookie}; Path=/; HttpOnly; SameSite=Strict"
    )));
    assert!(r.contains("Referrer-Policy: no-referrer"));

    let r = get(
        p,
        &format!("GET / HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n"),
    );
    assert_eq!(status(&r), "200", "{r}");
    assert!(r.contains("Cache-Control: no-cache"));
    assert!(r.contains("Content-Type: text/html"));

    let r = get(
        p,
        &format!(
            "GET / HTTP/1.1\r\nHost: {host}\r\nCookie: {}=00{}\r\n\r\n",
            auth::COOKIE,
            &auth::cookie_value(&tok)[2..]
        ),
    );
    assert_eq!(status(&r), "401");

    let r = get(
        p,
        &format!("GET /nope.js HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n"),
    );
    assert_eq!(status(&r), "404");
    let r = get(
        p,
        &format!("POST / HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n"),
    );
    assert_eq!(status(&r), "405");

    let ws = |origin: &str, cookie: &str| {
        get(
            p,
            &format!("GET /ws HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: {origin}\r\nCookie: {cookie}\r\n\r\n"),
        )
    };
    assert_eq!(status(&ws("https://evil.com", &cookie)), "403");
    assert_eq!(
        status(&ws(&format!("http://127.0.0.1:{}", p + 1), &cookie)),
        "403"
    );
    assert_eq!(status(&ws(&format!("http://{host}"), "x=1")), "401");

    for _ in 0..auth::MAX_FAILS {
        let r = get(
            p,
            &format!("GET /?t=00{} HTTP/1.1\r\nHost: {host}\r\n\r\n", &tok[2..]),
        );
        assert_eq!(status(&r), "403");
    }
    let r = get(
        p,
        &format!("GET /?t={tok} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert_eq!(status(&r), "429");
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn every_file_in_web_is_embedded() {
    let paths = assets::paths();
    assert!(paths.contains(&"index.html"), "{paths:?}");
    let a = assets::find("index.html").unwrap();
    assert!(a.mime.starts_with("text/html"));
    assert_eq!(assets::BUILD.len(), 12);

    for p in paths {
        let a = assets::find(p).unwrap();
        if !a.gz.is_empty() {
            assert!(a.gz.len() < a.bytes.len());
            assert_eq!(&a.gz[..2], &[0x1f, 0x8b]);
        }
    }
}

#[test]
fn binary_frames_round_trip() {
    let f = ws::frame(ws::KIND_OUTPUT, 70_000, b"hi");
    assert_eq!(f[0], 1);
    assert_eq!(ws::unframe(&f), Some((1, 70_000, &b"hi"[..])));
    assert_eq!(ws::unframe(&[1, 0, 0]), None);
    assert_eq!(ws::unframe(&[1, 0, 0, 0, 5]), Some((1, 5, &b""[..])));
}

#[test]
fn queries_are_kept_from_the_browser() {
    let s = |b: &[u8]| tap::strip_queries(b).into_owned();
    assert_eq!(s(b"a\x1b[6nb"), b"ab");
    assert_eq!(s(b"\x1b[c\x1b[>0c\x1b[>q\x1b[?2026$p\x1b[?u"), b"");
    assert_eq!(s(b"x\x1b]11;?\x07y\x1b]10;?\x1b\\z"), b"xyz");

    let keep: &[u8] = b"\x1b[31mred\x1b[0m\x1b[2;3H\x1b]0;title\x07\x1b[?25l\x1b[6";
    assert_eq!(s(keep), keep);
}

#[test]
fn a_slow_client_is_resynced_not_waited_for() {
    let out = Outbox::new();
    let chunk = vec![b'x'; 64 << 10];
    let t0 = Instant::now();

    for _ in 0..(tap::OUTPUT_LIMIT / chunk.len()) * 4 {
        out.output(7, &chunk);
    }
    assert!(t0.elapsed() < Duration::from_secs(1));
    let (bytes, frames) = out.queued();
    assert!(bytes <= tap::OUTPUT_LIMIT);

    out.output(8, b"y");
    let mut seen_resync = false;
    let mut after = 0;
    for _ in 0..frames + 2 {
        match out.pop(Duration::ZERO) {
            Some(Out::Resync(7)) => seen_resync = true,
            Some(Out::Output(7, _)) if seen_resync => after += 1,
            Some(_) => {}
            None => break,
        }
    }
    assert!(seen_resync);
    assert_eq!(
        after, 0,
        "output of a lagged tab is dropped until its snapshot"
    );

    out.snapshot(7, "{}".into());
    out.output(7, b"z");
    assert_eq!(out.pop(Duration::ZERO), Some(Out::Snapshot(7, "{}".into())));
    assert_eq!(out.pop(Duration::ZERO), Some(Out::Output(7, b"z".to_vec())));
}

#[test]
fn small_output_is_joined_and_text_is_bounded() {
    let out = Outbox::new();
    out.output(1, b"a");
    out.output(1, b"b");
    out.output(2, b"c");
    assert_eq!(
        out.pop(Duration::ZERO),
        Some(Out::Output(1, b"ab".to_vec()))
    );
    assert_eq!(out.pop(Duration::ZERO), Some(Out::Output(2, b"c".to_vec())));
    let big = "x".repeat(1 << 20);
    let mut n = 0;
    while out.text(big.clone()) {
        n += 1;
        assert!(n < 1000);
    }
    assert!(out.is_closed());
}

#[test]
fn only_what_installing_needs_is_public() {
    for p in [
        "/manifest.webmanifest",
        "/sw.js",
        "/icon-192.png",
        "/icon.svg",
        "/signin.html",
        "/signin.js",
        "/offline.html",
        "/shots/phone.png",
    ] {
        assert!(http::public(p), "{p}");
    }
    for p in [
        "/",
        "/index.html",
        "/app.css",
        "/js/main.js",
        "/ws",
        "/shots/../web-token",
        "/shots/a/b.png",
        "/icon-x/../y.png",
        "/vendor/xterm.js",
    ] {
        assert!(!http::public(p), "{p}");
    }
}

#[test]
fn snapshot_history_keeps_colors_without_cursor_moves() {
    let mut p = vt100::Parser::new(4, 20, 100);
    p.process(b"\x1b[31mred\x1b[m plain\r\n\x1b[1;44mbold on blue\x1b[m\r\nwide \xe4\xb8\xad!\r\n");
    for i in 0..6 {
        p.process(format!("line {i}\r\n").as_bytes());
    }
    let (data, rows, cols) = tap::snapshot_bytes(&mut p, 100);
    assert_eq!((rows, cols), (4, 20));
    let s = String::from_utf8(data).unwrap();
    let hist = &s[..s.find("line 3").unwrap()];
    assert!(
        hist.starts_with("\x1b[0;31mred\x1b[0m plain\r\n"),
        "{hist:?}"
    );
    assert!(
        hist.contains("\x1b[0;44;1mbold on blue\x1b[m\r\n"),
        "{hist:?}"
    );
    assert!(hist.contains("wide \u{4e2d}!\r\n"), "{hist:?}");

    assert!(
        !hist.contains("\x1b[A") && !hist.contains("\x1b[C"),
        "{hist:?}"
    );
}

fn stub_program(steps: &[&str]) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "godterm-web-stub-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    crate::test_stub::script(&d, steps)
        .to_string_lossy()
        .into_owned()
}

fn shell_tab(steps: &[&str], rows: u16, cols: u16) -> (Pane, std::sync::mpsc::Receiver<AppEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut p = Pane::new(std::env::temp_dir());
    p.resize(rows, cols);
    p.spawn(
        LaunchSpec {
            program: stub_program(steps),
            home_env: "",
            args: vec![],
            cwd: std::env::temp_dir(),
            config_dir: std::env::temp_dir(),
            kind: LaunchKind::Normal,
            pass_env: vec![],
            type_when_idle: None,
            extra_env: vec![],
            harness: crate::harness::Harness::Claude,
        },
        tx,
    )
    .unwrap();
    (p, rx)
}

fn replay(frames: &[Out], uid: u64) -> Option<vt100::Parser> {
    let mut t: Option<vt100::Parser> = None;
    for f in frames {
        match f {
            Out::Text(s) | Out::Snapshot(_, s) => {
                let v: serde_json::Value = serde_json::from_str(s).unwrap();
                if v["t"] == "snapshot" && v["tab"] == uid {
                    let mut p = vt100::Parser::new(
                        v["rows"].as_u64().unwrap() as u16,
                        v["cols"].as_u64().unwrap() as u16,
                        0,
                    );
                    p.process(v["data"].as_str().unwrap().as_bytes());
                    t = Some(p);
                } else if v["t"] == "resized" && v["tab"] == uid {
                    if let Some(p) = t.as_mut() {
                        p.screen_mut().set_size(
                            v["rows"].as_u64().unwrap() as u16,
                            v["cols"].as_u64().unwrap() as u16,
                        );
                    }
                }
            }
            Out::Output(u, b) if *u == uid => {
                if let Some(p) = t.as_mut() {
                    p.process(b);
                }
            }
            _ => {}
        }
    }
    t
}

#[test]
fn a_snapshot_then_output_has_no_gap_and_no_repeat() {
    let (p, _rx) = shell_tab(&["lines:3000:line {i}", "sleep:1"], 20, 60);
    let uid = p.uid;
    std::thread::sleep(Duration::from_millis(30));
    let out = Outbox::new();
    tap::get(uid).unwrap().subscribe(5, &out);
    let mut frames = vec![];
    let end = Instant::now() + Duration::from_secs(10);

    loop {
        while let Some(f) = out.pop(Duration::from_millis(20)) {
            frames.push(f);
        }
        let done = p.screen_text().contains("line 2999");
        if done || Instant::now() > end {
            std::thread::sleep(Duration::from_millis(100));
            while let Some(f) = out.pop(Duration::ZERO) {
                frames.push(f);
            }
            break;
        }
    }
    let shown = replay(&frames, uid).expect("a snapshot");
    assert_eq!(shown.screen().contents(), p.screen_text());

    let snaps = frames
        .iter()
        .filter(|f| matches!(f, Out::Snapshot(..)))
        .count();
    assert_eq!(snaps, 1);
    let first = replay(&frames[..1], uid).unwrap();
    let mut all = first.screen().contents();
    for f in &frames[1..] {
        if let Out::Output(_, b) = f {
            all.push_str(&String::from_utf8_lossy(b));
        }
    }
    let nums: Vec<u32> = all
        .split("line ")
        .skip(1)
        .filter_map(|s| s.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
        .collect();
    let first = nums[0];
    let want: Vec<u32> = (first..3000).collect();
    assert_eq!(nums, want, "a line was lost or repeated");
}

#[test]
fn a_resize_sends_its_size_and_a_respawn_a_new_snapshot() {
    let (mut p, _rx) = shell_tab(&["say:hello\\n", "sleep:5"], 10, 40);
    let uid = p.uid;
    std::thread::sleep(Duration::from_millis(200));
    let out = Outbox::new();
    tap::get(uid).unwrap().subscribe(1, &out);
    let mut frames = vec![];
    while let Some(f) = out.pop(Duration::ZERO) {
        frames.push(f);
    }
    p.resize(12, 50);

    let mut after = vec![];
    while let Some(f) = out.pop(Duration::ZERO) {
        assert!(
            !matches!(f, Out::Resync(_)),
            "a resize asked for a snapshot"
        );
        after.push(f);
    }
    let Some(Out::Text(r) | Out::Snapshot(_, r)) = after.first() else {
        panic!("no resized frame: {after:?}")
    };
    let v: serde_json::Value = serde_json::from_str(r).unwrap();
    assert_eq!(
        (v["t"].as_str(), v["rows"].as_u64(), v["cols"].as_u64()),
        (Some("resized"), Some(12), Some(50))
    );
    frames.extend(after);
    let shown = replay(&frames, uid).expect("a snapshot");
    assert_eq!(shown.screen().size(), (12, 50));
    assert!(shown.screen().contents().contains("hello"));

    let at = stream_pos(&frames, uid).unwrap();
    assert_eq!(at.sid, v["sid"].as_str().unwrap());
    let mut frames = vec![];

    p.kill();
    let (tx, _rx2) = std::sync::mpsc::channel();
    p.spawn(
        LaunchSpec {
            program: stub_program(&["say:second\\n", "sleep:5"]),
            home_env: "",
            args: vec![],
            cwd: std::env::temp_dir(),
            config_dir: std::env::temp_dir(),
            kind: LaunchKind::Normal,
            pass_env: vec![],
            type_when_idle: None,
            extra_env: vec![],
            harness: crate::harness::Harness::Claude,
        },
        tx,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    frames.clear();
    while let Some(f) = out.pop(Duration::ZERO) {
        if let Out::Resync(u) = f {
            tap::get(u).unwrap().resnapshot(1, &out);
        } else {
            frames.push(f);
        }
    }
    let shown = replay(&frames, uid).expect("a snapshot after the respawn");
    assert!(shown.screen().contents().contains("second"));
    assert!(!shown.screen().contents().contains("hello"));

    tap::get(uid).unwrap().unsubscribe(1);
    p.resize(10, 40);
    assert!(out.pop(Duration::ZERO).is_none());
}

#[test]
fn a_full_screen_apps_snapshot_is_on_the_alternate_screen() {
    let mut p = vt100::Parser::new(5, 20, 100);
    for i in 0..10 {
        p.process(format!("shell {i}\r\n").as_bytes());
    }
    p.process(b"\x1b[?1049h\x1b[?1003h\x1b[?1006hfull screen");
    let (data, _, _) = tap::snapshot_bytes(&mut p, 100);
    let s = String::from_utf8(data).unwrap();

    assert!(s.starts_with("\x1b[?1049h"), "{s:?}");
    assert!(!s.contains("shell"), "{s:?}");
    let mut b = vt100::Parser::new(5, 20, 0);
    b.process(s.as_bytes());
    assert!(b.screen().alternate_screen());
    assert!(b.screen().contents().contains("full screen"));
    assert_eq!(
        b.screen().mouse_protocol_encoding(),
        vt100::MouseProtocolEncoding::Sgr
    );
}

#[test]
fn a_wheel_from_a_browser_reaches_the_app_as_its_mouse_reports() {
    let (p, _rx) = shell_tab(
        &[
            "raw",
            "say:\\e[?1049h\\e[?1003h\\e[?1006hready\\r\\n",
            "hexdump:30",
            "sleep:5",
        ],
        10,
        80,
    );
    let t0 = Instant::now();
    while !p.screen_text().contains("ready") {
        assert!(t0.elapsed() < Duration::from_secs(5), "{}", p.screen_text());
        std::thread::sleep(Duration::from_millis(20));
    }
    let tap = tap::get(p.uid).unwrap();

    assert_eq!(tap.wheel(true, 2, 4, 2), Some("mouse"));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(tap.wheel(false, 1, 4, 2), Some("mouse"));
    let want = "1b 5b 3c 36 34 3b 35 3b 33 4d";
    while !p.screen_text().contains("4d") || p.screen_text().matches("1b").count() < 3 {
        assert!(t0.elapsed() < Duration::from_secs(5), "{}", p.screen_text());
        std::thread::sleep(Duration::from_millis(20));
    }
    let hex = p.screen_text().replace(['\n', '\r'], " ");
    let hex = hex.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        hex.contains(&format!("{want} {want} 1b 5b 3c 36 35")),
        "{hex}"
    );
}

fn stream_pos(frames: &[Out], uid: u64) -> Option<tap::Resume> {
    let mut r: Option<tap::Resume> = None;
    for f in frames {
        match f {
            Out::Text(s) | Out::Snapshot(_, s) => {
                let v: serde_json::Value = serde_json::from_str(s).unwrap();
                if (v["t"] == "snapshot" || v["t"] == "resume" || v["t"] == "resized")
                    && v["tab"] == uid
                {
                    r = Some(tap::Resume {
                        sid: v["sid"].as_str().unwrap().to_string(),
                        pos: v["pos"].as_u64().unwrap(),
                    });
                }
            }
            Out::Output(u, b) if *u == uid => {
                if let Some(r) = r.as_mut() {
                    r.pos += b.len() as u64;
                }
            }
            _ => {}
        }
    }
    r
}

fn drain(out: &std::sync::Arc<Outbox>, client: u64, frames: &mut Vec<Out>) {
    while let Some(f) = out.pop(Duration::from_millis(30)) {
        if let Out::Resync(u) = f {
            tap::get(u).unwrap().resnapshot(client, out);
        } else {
            frames.push(f);
        }
    }
}

#[test]
fn a_browser_that_comes_back_gets_only_what_it_missed() {
    let (mut p, _rx) = shell_tab(
        &[
            "say:first\\n",
            "read:x",
            "say:got {x}\\n",
            "read:y",
            "say:then {y}\\n",
            "sleep:5",
        ],
        10,
        40,
    );
    let uid = p.uid;
    std::thread::sleep(Duration::from_millis(300));
    let out = Outbox::new();
    tap::get(uid).unwrap().subscribe(7, &out);
    let mut frames = vec![];
    drain(&out, 7, &mut frames);
    let at = stream_pos(&frames, uid).expect("a snapshot");

    tap::get(uid).unwrap().unsubscribe(7);
    p.write(b"one\r");
    let t0 = Instant::now();
    while !p.screen_text().contains("got one") {
        assert!(t0.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));

    let out2 = Outbox::new();
    assert!(tap::get(uid).unwrap().subscribe_from(8, &out2, &at));
    let mut more = vec![];
    drain(&out2, 8, &mut more);
    let Some(Out::Text(first) | Out::Snapshot(_, first)) = more.first() else {
        panic!("no resume frame")
    };
    assert!(first.contains("\"resume\""), "{first}");
    assert!(!more
        .iter()
        .any(|f| matches!(f, Out::Text(s) | Out::Snapshot(_, s) if s.contains("snapshot"))));

    let mut all = frames;
    all.extend(more);
    let shown = replay(&all, uid).unwrap();
    assert_eq!(shown.screen().contents(), p.screen_text());
    let at2 = stream_pos(&all, uid).unwrap();

    tap::get(uid).unwrap().unsubscribe(8);
    let stale = tap::Resume {
        sid: "nope".into(),
        pos: at2.pos,
    };
    let out3 = Outbox::new();
    assert!(!tap::get(uid).unwrap().subscribe_from(9, &out3, &stale));
    tap::get(uid).unwrap().unsubscribe(9);
    p.resize(12, 50);
    let out4 = Outbox::new();
    assert!(!tap::get(uid).unwrap().subscribe_from(10, &out4, &at2));
    let mut snap = vec![];
    drain(&out4, 10, &mut snap);
    assert_eq!(replay(&snap, uid).unwrap().screen().size(), (12, 50));
}

#[test]
fn a_reader_is_never_held_up_by_a_client_that_reads_nothing() {
    let (p, _rx) = shell_tab(
        &["bytes:5999993:x", "say:\\n", "say:DONE\\n", "sleep:5"],
        24,
        80,
    );
    let out = Outbox::new();
    tap::get(p.uid).unwrap().subscribe(3, &out);
    let t0 = Instant::now();
    while !p.screen_text().contains("DONE") {
        assert!(t0.elapsed() < Duration::from_secs(15), "the reader stalled");
        std::thread::sleep(Duration::from_millis(50));
    }

    std::thread::sleep(Duration::from_millis(500));
    let (bytes, _) = out.queued();
    assert!(bytes <= tap::OUTPUT_LIMIT);

    let mut frames = vec![];
    while let Some(f) = out.pop(Duration::ZERO) {
        if let Out::Resync(u) = f {
            tap::get(u).unwrap().resnapshot(3, &out);
        } else {
            frames.push(f);
        }
    }
    let shown = replay(&frames, p.uid).unwrap();
    assert_eq!(shown.screen().contents(), p.screen_text());
}

fn ws_client(port: u16, cookie: &str) -> tungstenite::WebSocket<std::net::TcpStream> {
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("Cookie", cookie.parse().unwrap());
    req.headers_mut().insert(
        "Origin",
        format!("http://127.0.0.1:{port}").parse().unwrap(),
    );
    let s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tungstenite::client(req, s).unwrap().0
}

#[test]
fn websocket_streams_a_tab_and_takes_input() {
    use tungstenite::Message;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, rx) = test_hub("ws");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));

    let (p, _prx) = shell_tab(&["noecho", "echo_lines:got:"], 24, 80);
    let uid = p.uid;
    let mut ws = ws_client(hub.port, &cookie);

    match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        AppEvent::Web(ToApp::Hello(_)) => {}
        _ => panic!("not a hello"),
    }
    ws.send(Message::text(json_str(
        &serde_json::json!({"t": "subscribe", "tabs": [uid]}),
    )))
    .unwrap();
    let snap = loop {
        if let Message::Text(t) = ws.read().unwrap() {
            let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
            if v["t"] == "snapshot" {
                break v;
            }
        }
    };
    assert_eq!(snap["tab"], uid);
    assert_eq!(
        (snap["rows"].as_u64(), snap["cols"].as_u64()),
        (Some(24), Some(80))
    );

    let mut times = vec![];
    for i in 0..30 {
        let t0 = Instant::now();
        ws.send(Message::binary(ws::frame(
            ws::KIND_INPUT,
            uid,
            format!("k{i}\n").as_bytes(),
        )))
        .unwrap();
        let want = format!("got:k{i}");
        let mut seen = String::new();
        while !seen.contains(&want) {
            if let Message::Binary(b) = ws.read().unwrap() {
                let (k, u, d) = ws::unframe(&b).unwrap();
                assert_eq!((k, u), (ws::KIND_OUTPUT, uid));
                seen.push_str(&String::from_utf8_lossy(d));
            }
        }
        times.push(t0.elapsed());
    }
    times.sort();
    let median = times[times.len() / 2];
    eprintln!(
        "web: keystroke round trip median {median:?}, max {:?}",
        times.last().unwrap()
    );
    assert!(median < Duration::from_millis(50), "{median:?}");

    ws.send(Message::binary(ws::frame(ws::KIND_INPUT, 999_999, b"x")))
        .unwrap();
    loop {
        if let Message::Text(t) = ws.read().unwrap() {
            if t.as_str().contains("no such tab") {
                break;
            }
        }
    }

    ws.send(Message::text(r#"{"t":"ping","id":7}"#)).unwrap();
    loop {
        if let Message::Text(t) = ws.read().unwrap() {
            if t.as_str().contains("\"pong\"") {
                break;
            }
        }
    }
    drop(ws);
    drop(p);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn websocket_keeps_up_with_a_flood() {
    use tungstenite::Message;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, _rx) = test_hub("flood");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let (p, _prx) = shell_tab(
        &[
            "sleep:0.3",
            "yes:20000000",
            "say:\\n",
            "say:END-OF-FLOOD\\n",
            "sleep:10",
        ],
        50,
        200,
    );
    let uid = p.uid;
    let mut ws = ws_client(hub.port, &cookie);
    ws.send(Message::text(json_str(
        &serde_json::json!({"t": "subscribe", "tabs": [uid]}),
    )))
    .unwrap();
    let t0 = Instant::now();
    let mut bytes = 0usize;
    let mut snapshots = 0;
    let mut term: Option<vt100::Parser> = None;
    let mut tail = String::new();
    while t0.elapsed() < Duration::from_secs(30) {
        match ws.read().unwrap() {
            Message::Binary(b) => {
                let (_, _, d) = ws::unframe(&b).unwrap();
                bytes += d.len();
                if let Some(t) = term.as_mut() {
                    t.process(d);
                }
                tail.push_str(&String::from_utf8_lossy(&d[d.len().saturating_sub(64)..]));
                if tail.len() > 256 {
                    tail = tail[tail.len() - 128..].to_string();
                }
                if tail.contains("END-OF-FLOOD") {
                    break;
                }
            }
            Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                if v["t"] == "snapshot" {
                    snapshots += 1;
                    let mut p = vt100::Parser::new(50, 200, 0);
                    p.process(v["data"].as_str().unwrap().as_bytes());
                    if p.screen().contents().contains("END-OF-FLOOD") {
                        break;
                    }
                    term = Some(p);
                }
            }
            _ => {}
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    eprintln!(
        "web: flood: {:.1} MB in {secs:.1} s ({:.1} MB/s), {snapshots} snapshots",
        bytes as f64 / 1e6,
        bytes as f64 / 1e6 / secs
    );
    assert!(tail.contains("END-OF-FLOOD") || snapshots > 0);

    std::thread::sleep(Duration::from_millis(200));
    if let Some(t) = term {
        assert_eq!(t.screen().contents(), p.screen_text());
    }
    drop(p);
    let _ = std::fs::remove_dir_all(&h);
}

fn json_str(v: &serde_json::Value) -> String {
    v.to_string()
}

#[test]
fn a_process_godterm_started_cannot_sign_in() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, _rx) = test_hub("inside");

    let sink = h.join("curl-body.txt");
    let out = std::process::Command::new("curl")
        .args([
            "-s",
            "-o",
            sink.to_str().unwrap(),
            "-w",
            "%{http_code}",
            &format!("http://127.0.0.1:{}/?t={tok}", hub.port),
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "403");
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn a_browser_mic_keeps_only_fresh_audio_from_before_a_capture() {
    use crate::voice::audio::{Capture, WebMic, FRAME};
    let mic = WebMic::new();
    let pcm: Vec<u8> = (0..FRAME as i16 * 2)
        .flat_map(|s| s.to_le_bytes())
        .collect();

    mic.push(&pcm);
    let mut cap = Capture::web(&mic);
    assert!(cap.vp, "the browser cancels its own echo");
    assert_eq!(cap.queued(), FRAME * 2);
    let mut f = vec![];
    cap.frame(&mut f).unwrap();
    assert_eq!(f[..3], [0, 1, 2]);
    cap.frame(&mut f).unwrap();
    assert_eq!(f[0], FRAME as i16);

    let t0 = Instant::now();
    assert!(cap.frame(&mut f).is_some());
    assert!(t0.elapsed() >= Duration::from_millis(200), "it waited");
    assert!(f.iter().all(|s| *s == 0));

    drop(cap);
    mic.push(&pcm);
    std::thread::sleep(Duration::from_millis(1100));
    let mut cap = Capture::web(&mic);
    assert_eq!(cap.queued(), 0);
    cap.frame(&mut f).unwrap();
    assert!(f.iter().all(|s| *s == 0));

    let s = mic.take_stats();
    assert_eq!(s.frames, 2);
    assert_eq!(s.dropped, FRAME as u64 * 2);
    assert!(s.line().contains("dropped"), "{}", s.line());
    assert_eq!(mic.take_stats().frames, 0, "counts start again");
}

#[test]
fn a_browser_mic_knows_when_its_page_plays() {
    use crate::voice::audio::{Capture, WebMic};
    let mic = WebMic::new();
    let cap = Capture::web(&mic);
    assert!(!cap.web_playing());
    mic.set_playing(true);
    assert!(cap.web_playing());
    mic.set_playing(false);
    assert!(!cap.web_playing());
}

#[test]
fn the_voice_browser_gets_ducks_and_stops() {
    use crate::voice::{player, tts};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("web-voice-duck");
    let (tx, _rx) = std::sync::mpsc::channel();
    let hub = Hub::new(0, "x".repeat(64), vec![], None, tx);
    let a = hub.add();
    app.web = Some(super::app_web::WebSide::new(Arc::clone(&hub)));
    let got = |c: &Arc<Client>| -> Vec<String> {
        let mut out = vec![];
        while let Some(f) = c.out.pop(Duration::from_millis(20)) {
            if let Out::Text(t) | Out::Snapshot(_, t) = f {
                out.push(t);
            }
        }
        out
    };
    app.web_voice(a.id, Some(true), "open", None);
    assert_eq!(app.web_voice_client(), Some(a.id));
    let _ = got(&a);
    assert!(tts::web_chime(), "the browser takes the audio");
    assert!(player::web_end().is_some(), "a clip goes on its clock");
    tts::web_duck(0.4);
    tts::web_duck(0.4);
    tts::web_duck(1.0);
    let ducks: Vec<String> = got(&a)
        .into_iter()
        .filter(|t| t.contains("audio_duck"))
        .collect();
    assert_eq!(ducks.len(), 2, "{ducks:?}");
    assert!(ducks[0].contains(r#""level":0.4"#), "{ducks:?}");
    assert!(ducks[1].contains(r#""level":1.0"#), "{ducks:?}");
    let (v, _ctl) = crate::voice::Voice::fake(true);
    v.stop_speaking();
    assert!(got(&a).iter().any(|t| t.contains("audio_stop")));
    assert_eq!(player::web_end(), None, "nothing more expected to play");
    app.web_voice_off("test");

    tts::web_duck(0.4);
    assert!(!got(&a).iter().any(|t| t.contains("audio_duck")));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_browser_mic_counts_silence_and_level() {
    use crate::voice::audio::WebMic;
    let mic = WebMic::new();
    mic.push(&[0u8; 640]);
    let s = mic.take_stats();
    assert_eq!((s.frames, s.samples, s.nonzero), (1, 320, 0));
    assert!(s.db() <= -89.0);
    let loud: Vec<u8> = (0..320).flat_map(|_| 16384i16.to_le_bytes()).collect();
    mic.push(&loud);
    let s = mic.take_stats();
    assert_eq!(s.nonzero, 320);
    assert!((s.db() + 6.0).abs() < 0.5, "{}", s.db());
}

#[test]
fn speech_clips_are_wav() {
    let w = super::voice::wav(&[0.0, 0.5, -1.0], 24_000);
    assert_eq!(&w[..4], b"RIFF");
    assert_eq!(&w[8..16], b"WAVEfmt ");
    assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), 24_000);
    assert_eq!(w.len(), 44 + 6);
    assert_eq!(i16::from_le_bytes([w[46], w[47]]), 16383);
    assert_eq!(i16::from_le_bytes([w[48], w[49]]), -32767);
}

#[test]
fn websocket_voice_and_images_reach_the_app() {
    use tungstenite::Message;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, rx) = test_hub("ws-voice");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let mut ws = ws_client(hub.port, &cookie);
    let id = match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        AppEvent::Web(ToApp::Hello(id)) => id,
        _ => panic!("not a hello"),
    };
    let next = || loop {
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            AppEvent::Web(ToApp::Typed(_)) => continue,
            AppEvent::Web(t) => break t,
            _ => continue,
        }
    };
    ws.send(Message::text(r#"{"t":"voice","on":true,"mode":"open"}"#))
        .unwrap();
    match next() {
        ToApp::Voice {
            client, on, mode, ..
        } => assert_eq!((client, on, mode.as_str()), (id, Some(true), "open")),
        t => panic!("{t:?}"),
    }
    ws.send(Message::text(r#"{"t":"voice","talk":false}"#))
        .unwrap();
    match next() {
        ToApp::Voice { on, talk, .. } => assert_eq!((on, talk), (None, Some(false))),
        t => panic!("{t:?}"),
    }

    let mic = crate::voice::audio::WebMic::new();
    let mut cap = crate::voice::audio::Capture::web(&mic);
    *hub.client(id).unwrap().mic.lock().unwrap() = Some(Arc::clone(&mic));
    let pcm: Vec<u8> = (0..480i16).flat_map(|s| (s + 7).to_le_bytes()).collect();
    ws.send(Message::binary(ws::frame(ws::KIND_MIC, 0, &pcm)))
        .unwrap();
    let mut f = vec![];
    cap.frame(&mut f).unwrap();
    assert_eq!(f[..2], [7, 8], "the browser's samples");

    ws.send(Message::text(r#"{"t":"voice","direct":12}"#))
        .unwrap();
    match next() {
        ToApp::VoiceCtl { client, v } => {
            assert_eq!((client, &v["direct"]), (id, &serde_json::json!(12)))
        }
        t => panic!("{t:?}"),
    }

    let dir = std::env::temp_dir().join(format!("gt-ws-rec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let webm = dir.join("r.webm");
    let made = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args([
            "-i",
            "sine=frequency=300:duration=1:sample_rate=48000",
            "-c:a",
            "libopus",
        ])
        .arg(&webm)
        .status()
        .is_ok_and(|s| s.success());
    if made {
        cap.frame(&mut f).unwrap();
        let bytes = std::fs::read(&webm).unwrap();
        ws.send(Message::text(
            r#"{"t":"voice","rec":"audio/webm;codecs=opus"}"#,
        ))
        .unwrap();
        for c in bytes.chunks(2000) {
            ws.send(Message::binary(ws::frame(ws::KIND_MIC_REC, 0, c)))
                .unwrap();
        }
        ws.send(Message::text(r#"{"t":"voice","rec_end":true}"#))
            .unwrap();
        let mut loud = 0;
        for _ in 0..20 {
            cap.frame(&mut f).unwrap();
            loud += f.iter().filter(|s| s.abs() > 1000).count();
        }
        assert!(loud > 1000, "decoded samples: {loud}");
    }
    let _ = std::fs::remove_dir_all(&dir);

    let png = crate::assistant_images::testing::png(3, 2);
    let url = format!(
        "data:image/png;base64,{}",
        crate::assistant_images::base64(&png)
    );
    ws.send(Message::text(json_str(
        &serde_json::json!({"t": "say", "text": "look", "images": [url]}),
    )))
    .unwrap();
    match next() {
        ToApp::Say { text, images, .. } => {
            assert_eq!(text, "look");
            assert_eq!(images, vec![png]);
        }
        t => panic!("{t:?}"),
    }

    hub.client(id)
        .unwrap()
        .out
        .audio(super::voice::wav(&[0.1; 10], 24_000));
    loop {
        if let Message::Binary(b) = ws.read().unwrap() {
            let (k, _, d) = ws::unframe(&b).unwrap();
            assert_eq!(k, ws::KIND_AUDIO);
            assert_eq!(&d[..4], b"RIFF");
            break;
        }
    }
    drop(ws);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn the_chat_leaves_out_what_godterm_own_turns_carry() {
    use crate::app_assistant::{Entry, Who};
    let e = |who, t: &str| Entry::new(who, t);
    let log = [
        e(Who::User, "what is waiting?"),
        e(
            Who::Tool,
            r#"{"tool":"show","args":{"view":"approvals"},"status":"ok"}"#,
        ),
        e(Who::Reply, "Here are the approvals waiting for you."),
        e(Who::Note, "(events: t2 finished, t1 finished)"),
        e(
            Who::Tool,
            r#"{"tool":"show","args":{"view":"approvals"},"status":"refused: a report turn"}"#,
        ),
        e(Who::Preamble, "Checking."),
        e(Who::Note, "(t3 answered)"),
        e(Who::Reply, "t3 says the tests pass."),
        e(Who::Note, "(cut off; listening)"),
        e(Who::User, "thanks"),
        e(Who::Note, "Prompt to t3: delivered"),
    ];
    let mut sys = false;
    let shown: Vec<bool> = log
        .iter()
        .map(|x| !super::app_web::hidden(x, &mut sys))
        .collect();
    assert_eq!(
        shown,
        [true, true, true, false, false, false, false, true, true, true, true]
    );
}

#[test]
fn chat_messages_carry_when_they_were_written() {
    use crate::app_assistant::{now_ms, Entry, Who};
    let before = now_ms();
    let e = Entry::new(Who::Reply, "hi");
    assert!(e.at >= before && e.at <= now_ms(), "stamped now: {}", e.at);
    let m = super::app_web::message(3, &Entry::at(Who::User, "x", 1_700_000_000_000), false);
    assert_eq!(m["at"], 1_700_000_000_000i64);
    assert_eq!(m["id"], 3);
    assert_eq!(m["role"], "user");
    assert!(m.get("hidden").is_none());
}

#[test]
fn a_busy_port_is_tried_again_and_the_settings_start_stop_and_move_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("web-retry");
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    app.cfg.web.enabled = true;
    app.cfg.web.port = port;

    app.cfg.web.tailnet_sign_in = false;
    app.try_start_web(true);
    assert!(app.web.is_none(), "the port is taken");
    assert!(app.web_retry_at.is_some(), "it tries again");

    assert!(app.web_retry_tick().is_some());
    assert!(app.web.is_none());
    drop(busy);
    app.web_retry_at = Some(std::time::Instant::now());
    assert!(app.web_retry_tick().is_none());
    assert_eq!(
        app.web.as_ref().map(|w| w.hub.port),
        Some(port),
        "it got the port"
    );

    let other = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let old = app.cfg.web.clone();
    app.cfg.web.port = other;
    assert!(app.web_config_changed(&old).is_some());
    assert_eq!(app.web.as_ref().map(|w| w.hub.port), Some(other));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "the old port is free"
    );

    let old = app.cfg.web.clone();
    app.cfg.web.enabled = false;
    assert_eq!(app.web_config_changed(&old).as_deref(), Some("web app off"));
    assert!(app.web.is_none() && app.web_retry_at.is_none());
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", other)).is_ok(),
        "the port is free"
    );

    let old = app.cfg.web.clone();
    app.cfg.web.enabled = true;
    assert_eq!(app.web_config_changed(&old).as_deref(), Some("web app on"));
    assert!(app.web.is_some());
    app.stop_web();
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_web_turn_carries_its_view_and_web_layout_reaches_that_browser_only() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("web-assist");
    let (tx, _rx) = std::sync::mpsc::channel();
    let hub = Hub::new(0, "x".repeat(64), vec![], None, tx);
    let a = hub.add();
    let b = hub.add();
    for c in [&a, &b] {
        c.ready.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    *a.view.lock().unwrap() = serde_json::json!({"device": "desktop", "panes": 4, "tabs": [null, null, null, null], "focused": 0});
    app.web = Some(super::app_web::WebSide::new(Arc::clone(&hub)));

    assert!(!app.state_preamble().contains("came from the web app"));
    app.assistant.web_client = Some(a.id);
    let p = app.state_preamble();
    assert!(p.contains("came from the web app"), "{p}");
    assert!(p.contains("desktop, 4 panes: 1 empty"), "{p}");
    assert!(p.contains("web_layout"), "{p}");

    let tab = crate::control::tab_id(app.panes[0].tabs[0].uid);
    let v = app.control_call(
        "web_layout",
        &serde_json::json!({"panes": 2, "tabs": [tab]}),
    );
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    let got = |c: &Arc<Client>| -> Vec<String> {
        let mut out = vec![];
        while let Some(f) = c.out.pop(Duration::from_millis(20)) {
            if let Out::Text(t) | Out::Snapshot(_, t) = f {
                out.push(t);
            }
        }
        out
    };
    let fa = got(&a);
    assert!(
        fa.iter().any(|t| t.contains(r#""t":"ui""#)
            && t.contains(r#""action":"layout""#)
            && t.contains(r#""panes":2"#)),
        "{fa:?}"
    );
    assert!(
        got(&b).iter().all(|t| !t.contains(r#""t":"ui""#)),
        "the other browser is left alone"
    );

    let v = app.control_call(
        "web_view",
        &serde_json::json!({"view": "sessions", "client": "all"}),
    );
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert!(got(&a).iter().any(|t| t.contains("sessions")));
    assert!(got(&b).iter().any(|t| t.contains("sessions")));
    let _ = std::fs::remove_dir_all(home);
}

fn mock_tailnet(
    extra: &[&str],
    ips: Arc<Mutex<Vec<String>>>,
    peers: Vec<(&'static str, &'static str)>,
    calls: Arc<AtomicU64>,
) -> Arc<tailnet::Tailnet> {
    let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
    Arc::new(tailnet::Tailnet::new(
        &extra,
        Box::new(move || {
            let ips = ips.lock().unwrap().clone();
            Some(serde_json::json!({
                "Self": {"UserID": 7, "TailscaleIPs": ips, "DNSName": "mac.tail1.ts.net."},
                "User": {"7": {"ID": 7, "LoginName": "Me@Example.com"},
                         "8": {"ID": 8, "LoginName": "other@example.com"}},
            }))
        }),
        Box::new(move |ip| {
            calls.fetch_add(1, Ordering::SeqCst);
            let ip = ip.to_string();
            let (_, who) = peers.iter().find(|(p, _)| *p == ip)?;
            Some(match who.strip_prefix("tag:") {
                Some(name) => serde_json::json!({
                    "Node": {"Name": format!("{name}.tail1.ts.net."), "Tags": ["tag:server"]},
                    "UserProfile": {"LoginName": "tagged-devices"},
                }),
                None => serde_json::json!({
                    "Node": {"Name": "phone.tail1.ts.net.", "Tags": null},
                    "UserProfile": {"LoginName": who},
                }),
            })
        }),
    ))
}

fn own_ips() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(vec![
        "100.100.1.1".into(),
        "fd7a:115c:a1e0::1".into(),
    ]))
}

fn tailnet_hub(
    tag: &str,
    t: Arc<tailnet::Tailnet>,
) -> (
    Arc<Hub>,
    String,
    std::path::PathBuf,
    std::sync::mpsc::Receiver<AppEvent>,
) {
    let h = home(tag);
    let (tx, rx) = std::sync::mpsc::channel();
    let hub = start_with(
        0,
        || Opts {
            tailnet: Some(t),
            ..Opts::default()
        },
        tx,
    )
    .unwrap();
    let tok = auth::load_or_create().unwrap();
    (hub, tok, h, rx)
}

#[test]
fn tailnet_addresses_and_logins() {
    for ip in [
        "100.64.0.1",
        "100.127.255.254",
        "100.100.1.1",
        "fd7a:115c:a1e0::1:2",
    ] {
        assert!(tailnet::in_tailnet(ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "100.63.255.255",
        "100.128.0.1",
        "127.0.0.1",
        "8.8.8.8",
        "fd7a:115c:a1e1::1",
        "::1",
    ] {
        assert!(!tailnet::in_tailnet(ip.parse().unwrap()), "{ip}");
    }
    let calls = Arc::new(AtomicU64::new(0));
    let t = mock_tailnet(&["Friend@x.com"], own_ips(), vec![], calls);

    assert_eq!(t.owner().as_deref(), Some("me@example.com"));
    assert_eq!(
        t.judge("ME@example.com"),
        tailnet::Who::Allowed("me@example.com".into())
    );
    assert_eq!(
        t.judge("friend@X.com"),
        tailnet::Who::Allowed("friend@x.com".into())
    );
    assert_eq!(
        t.judge("other@example.com"),
        tailnet::Who::Refused("other@example.com".into())
    );
    assert_eq!(t.judge(""), tailnet::Who::Unknown);
    assert_eq!(t.ipv4(), Some("100.100.1.1".parse().unwrap()));
    assert!(auth::ts_net_host("mac.tail1.ts.net:7878"));
    assert!(!auth::ts_net_host("127.0.0.1:7878"));
    assert!(!auth::ts_net_host("evil.ts.net.com"));
    assert!(!auth::ts_net_host("ts.net"));
}

#[test]
fn serve_identity_headers_sign_in_only_allowed_logins_on_ts_net() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let calls = Arc::new(AtomicU64::new(0));
    let t = mock_tailnet(&["friend@x.com"], own_ips(), vec![], Arc::clone(&calls));
    let (hub, tok, h, _rx) = tailnet_hub("serve-id", t);
    let p = hub.port;
    let ts = format!("mac.tail1.ts.net:{p}");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let req = |host: &str, extra: &str| {
        get(
            p,
            &format!("GET / HTTP/1.1\r\nHost: {host}\r\n{extra}X-Forwarded-For: 100.100.2.2\r\nX-Forwarded-Proto: https\r\n\r\n"),
        )
    };

    let r = get(
        p,
        &format!("GET / HTTP/1.1\r\nHost: {ts}\r\nTailscale-User-Login: me@example.com\r\nX-Forwarded-For: 100.100.1.1\r\n\r\n"),
    );
    assert_eq!(status(&r), "401", "{r}");

    let r = req(&ts, "Tailscale-User-Login: me@example.com\r\n");
    assert_eq!(status(&r), "200", "{r}");
    assert!(r.contains("Content-Type: text/html"));
    assert!(
        r.contains(&format!(
            "Set-Cookie: {cookie}; Path=/; HttpOnly; SameSite=Strict"
        )),
        "{r}"
    );
    assert!(r.contains("; Secure"));

    assert_eq!(
        status(&req(&ts, "Tailscale-User-Login: Friend@x.com\r\n")),
        "200"
    );

    let r = req(&ts, "Tailscale-User-Login: other@example.com\r\n");
    assert_eq!(status(&r), "403", "{r}");
    assert!(r.contains("not open to other@example.com"), "{r}");
    assert!(!r.contains("Set-Cookie") && !r.contains("<html"));

    let r = req(
        &format!("127.0.0.1:{p}"),
        "Tailscale-User-Login: me@example.com\r\n",
    );
    assert_eq!(status(&r), "401", "{r}");
    let r = req(
        &format!("localhost:{p}"),
        "Tailscale-User-Login: me@example.com\r\n",
    );
    assert_eq!(status(&r), "401", "{r}");

    assert_eq!(
        status(&req("evil.com", "Tailscale-User-Login: me@example.com\r\n")),
        "421"
    );

    let r = req(
        &ts,
        "Tailscale-User-Login: other@example.com\r\nTailscale-User-Login: me@example.com\r\n",
    );
    assert_eq!(status(&r), "401", "{r}");

    assert_eq!(status(&req(&ts, "")), "401");

    let ws = |origin: &str| {
        get(
            p,
            &format!("GET /ws HTTP/1.1\r\nHost: {ts}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: {origin}\r\nTailscale-User-Login: me@example.com\r\nX-Forwarded-For: 100.100.2.2\r\n\r\n"),
        )
    };
    assert_eq!(status(&ws("https://evil.com")), "403");
    assert_eq!(status(&ws(&format!("http://127.0.0.1:{p}"))), "403");
    let mut c = std::net::TcpStream::connect(("127.0.0.1", p)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(format!("GET /ws HTTP/1.1\r\nHost: {ts}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: https://{ts}\r\nTailscale-User-Login: me@example.com\r\nX-Forwarded-For: 100.100.2.2\r\n\r\n").as_bytes()).unwrap();
    let mut b = [0u8; 12];
    c.read_exact(&mut b).unwrap();
    assert_eq!(&b, b"HTTP/1.1 101", "the socket opens by identity");
    drop(c);

    let sink = h.join("curl-body.txt");
    let out = std::process::Command::new("curl")
        .args([
            "-s",
            "-o",
            sink.to_str().unwrap(),
            "-w",
            "%{http_code}",
            "-H",
            &format!("Host: {ts}"),
            "-H",
            "Tailscale-User-Login: me@example.com",
            &format!("http://127.0.0.1:{p}/"),
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "401");

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    hub.stop();
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn identity_headers_are_ignored_with_tailnet_sign_in_off() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, _tok, h, _rx) = test_hub("serve-off");
    let r = get(
        hub.port,
        "GET / HTTP/1.1\r\nHost: mac.tail1.ts.net\r\nTailscale-User-Login: me@example.com\r\n\r\n",
    );
    assert_eq!(status(&r), "401", "{r}");
    hub.stop();
    let _ = std::fs::remove_dir_all(&h);
}

fn from_peer(hub: &Arc<Hub>, peer: &str, head: &str) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (srv, _) = l.accept().unwrap();
    let peer: std::net::SocketAddr = std::net::SocketAddr::new(peer.parse().unwrap(), 50000);
    let h = Arc::clone(hub);
    let t = std::thread::spawn(move || h.handle_from(srv, peer, Via::Tailnet));
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = c.write_all(closing(head).as_bytes());
    let mut out = vec![];
    let _ = c.read_to_end(&mut out);
    t.join().unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn the_tailnet_ip_signs_in_by_whois() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let calls = Arc::new(AtomicU64::new(0));
    let t = mock_tailnet(
        &[],
        own_ips(),
        vec![
            ("100.100.2.2", "me@example.com"),
            ("100.100.3.3", "other@example.com"),
            ("100.100.4.4", "tag:server"),
            ("fd7a:115c:a1e0::2", "ME@example.com"),
        ],
        Arc::clone(&calls),
    );
    let (hub, tok, h, _rx) = tailnet_hub("ip-id", t);
    let head = |extra: &str| format!("GET / HTTP/1.1\r\nHost: 100.100.1.1:7941\r\n{extra}\r\n");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));

    let r = from_peer(&hub, "100.100.2.2", &head(""));
    assert_eq!(status(&r), "200", "{r}");
    assert!(r.contains(&format!("Set-Cookie: {cookie};")), "{r}");
    assert!(!r.contains("; Secure"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    assert_eq!(status(&from_peer(&hub, "100.100.2.2", &head(""))), "200");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cached");

    assert_eq!(
        status(&from_peer(&hub, "fd7a:115c:a1e0::2", &head(""))),
        "200"
    );

    let r = from_peer(&hub, "100.100.3.3", &head(""));
    assert_eq!(status(&r), "403", "{r}");
    assert!(r.contains("not open to other@example.com"));
    let r = from_peer(&hub, "100.100.4.4", &head(""));
    assert_eq!(status(&r), "403", "{r}");
    assert!(r.contains("tagged node server.tail1.ts.net"), "{r}");

    let r = from_peer(
        &hub,
        "100.100.5.5",
        &head("Tailscale-User-Login: me@example.com\r\n"),
    );
    assert_eq!(status(&r), "401", "{r}");

    let n = calls.load(Ordering::SeqCst);
    assert_eq!(status(&from_peer(&hub, "100.100.1.1", &head(""))), "401");
    assert_eq!(calls.load(Ordering::SeqCst), n);
    let r = from_peer(
        &hub,
        "100.100.1.1",
        &format!("GET /?t={tok} HTTP/1.1\r\nHost: 100.100.1.1:7941\r\n\r\n"),
    );
    assert_eq!(status(&r), "303", "{r}");

    assert_eq!(from_peer(&hub, "8.8.8.8", &head("")), "");
    assert_eq!(from_peer(&hub, "192.168.1.5", &head("")), "");

    let r = from_peer(
        &hub,
        "100.100.2.2",
        "GET / HTTP/1.1\r\nHost: evil.com\r\n\r\n",
    );
    assert_eq!(status(&r), "421");
    let ws = |origin: &str| {
        from_peer(
            &hub,
            "100.100.2.2",
            &format!("GET /ws HTTP/1.1\r\nHost: 100.100.1.1:7941\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: {origin}\r\n\r\n"),
        )
    };
    assert_eq!(status(&ws("http://evil.com")), "403");
    assert_eq!(status(&ws("http://100.100.1.1:7880")), "403");
    hub.stop();
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn the_tailnet_listener_waits_for_its_port_and_follows_the_address() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let ips = Arc::new(Mutex::new(vec!["127.0.0.1".to_string()]));
    let t = mock_tailnet(&[], Arc::clone(&ips), vec![], Arc::new(AtomicU64::new(0)));
    let (hub, _tok, h, _rx) = tailnet_hub("ip-keep", t);
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let h2 = Arc::clone(&hub);
    let ms = Duration::from_millis(50);
    let th = std::thread::spawn(move || keep_ip_listener(h2, port, ms, ms));
    let free = || std::net::TcpListener::bind(("127.0.0.1", port)).is_ok();
    let wait = |want_free: bool| {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if free() == want_free {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    };
    std::thread::sleep(Duration::from_millis(200));

    drop(busy);
    assert!(wait(false), "it bound the port once it was free");

    ips.lock().unwrap().clear();
    assert!(wait(true), "it let the port go");

    ips.lock().unwrap().push("127.0.0.1".into());
    assert!(wait(false), "bound again");
    hub.stop();
    th.join().unwrap();
    assert!(wait(true), "stopped: the port is free");
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn a_press_makes_a_browser_the_voice_and_a_refusal_says_why() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("web-voice-press");
    let (tx, _rx) = std::sync::mpsc::channel();
    let hub = Hub::new(0, "x".repeat(64), vec![], None, tx);
    let a = hub.add();
    let b = hub.add();
    app.web = Some(super::app_web::WebSide::new(Arc::clone(&hub)));
    let got = |c: &Arc<Client>| -> Vec<String> {
        let mut out = vec![];
        while let Some(f) = c.out.pop(Duration::from_millis(20)) {
            if let Out::Text(t) | Out::Snapshot(_, t) = f {
                out.push(t);
            }
        }
        out
    };

    app.web_voice(a.id, None, "push", Some(false));
    assert_eq!(app.web_voice_client(), None);
    assert!(got(&a).is_empty());

    app.web_voice(a.id, None, "push", Some(true));
    assert_eq!(app.web_voice_client(), Some(a.id));
    assert!(
        a.mic.lock().unwrap().is_some(),
        "its audio goes to the engine"
    );
    let fa = got(&a);
    assert!(
        fa.iter()
            .any(|t| t.contains(r#""t":"voice""#) && t.contains(r#""ok":true"#)),
        "{fa:?}"
    );
    assert_eq!(app.web_state()["voice"]["web"], serde_json::json!(a.id));

    app.web_voice(b.id, None, "push", Some(true));
    assert_eq!(app.web_voice_client(), Some(b.id));
    assert!(a.mic.lock().unwrap().is_none());
    assert!(got(&a).iter().any(|t| t.contains(r#""moved":true"#)));

    got(&b);
    app.web_voice(b.id, Some(true), "open", None);
    assert_eq!(app.web_voice_client(), Some(b.id));
    let fb = got(&b);
    assert!(!fb.iter().any(|t| t.contains(r#""moved":true"#)), "{fb:?}");
    assert!(
        fb.iter()
            .any(|t| t.contains(r#""on":true"#) && t.contains(r#""mode":"open""#)),
        "{fb:?}"
    );
    assert!(
        b.mic.lock().unwrap().is_some(),
        "its mic is still the input"
    );

    app.web_voice_off("test");
    got(&b);
    app.voice.muted = true;
    app.web_voice(b.id, Some(true), "open", None);
    assert_eq!(app.web_voice_client(), None);
    let fb = got(&b);
    assert!(
        fb.iter().any(|t| t.contains(r#""ok":false"#)
            && t.contains(r#""code":"muted""#)
            && t.contains("muted in GodTerm")),
        "{fb:?}"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_muted_browser_mic_keeps_open_mic_awake() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("web-voice-muted");
    let (tx, _rx) = std::sync::mpsc::channel();
    let hub = Hub::new(0, "x".repeat(64), vec![], None, tx);
    let a = hub.add();
    let b = hub.add();
    app.web = Some(super::app_web::WebSide::new(Arc::clone(&hub)));
    app.cfg.voice.open_mic_sleep_min = 1;
    app.web_voice(a.id, Some(true), "open", None);
    assert_eq!(app.web_voice_client(), Some(a.id));
    assert!(app.voice.open_mic, "open mic on the browser's audio");

    app.web_voice_ctl(b.id, &serde_json::json!({"muted": true}));
    assert!(!app.web_mic_is_muted());
    app.web_voice_ctl(a.id, &serde_json::json!({"muted": true}));
    assert!(app.web_mic_is_muted());
    assert_eq!(
        app.web_state()["voice"]["web_muted"],
        serde_json::json!(true)
    );
    assert!(!app.voice.muted, "GodTerm's own mute is not touched");

    app.voice.last_heard = Instant::now() - Duration::from_secs(5 * 60);
    app.open_mic_tick();
    assert!(
        app.voice.open_mic,
        "no auto sleep while the browser is muted"
    );

    app.web_voice_ctl(a.id, &serde_json::json!({"muted": false}));
    assert!(!app.web_mic_is_muted());
    app.open_mic_tick();
    assert!(app.voice.open_mic, "the sleep count starts at the unmute");

    app.voice.last_heard = Instant::now() - Duration::from_secs(5 * 60);
    app.open_mic_tick();
    assert!(!app.voice.open_mic, "auto sleep again once unmuted");

    app.web_voice_ctl(a.id, &serde_json::json!({"muted": true}));
    app.web_voice_off("test");
    app.web_voice(a.id, Some(true), "open", None);
    assert!(!app.web_mic_is_muted());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_browser_chooses_its_own_talk_back_voice() {
    use crate::voice::web_tts::{self, WebTts};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("web-tts-choice");
    let (tx, _rx) = std::sync::mpsc::channel();
    let hub = Hub::new(0, "x".repeat(64), vec![], None, tx);
    let a = hub.add();
    let b = hub.add();
    app.web = Some(super::app_web::WebSide::new(Arc::clone(&hub)));
    let got = |c: &Arc<Client>, wait: Duration| -> Vec<serde_json::Value> {
        let mut out = vec![];
        while let Some(f) = c.out.pop(wait) {
            if let Out::Text(t) | Out::Snapshot(_, t) = f {
                out.push(serde_json::from_str(&t).unwrap());
            }
        }
        out
    };

    super::ws::web_tts(
        &a,
        &serde_json::json!({"engine": "kokoro", "voice": "bf_emma"}),
    );
    let r = got(&a, Duration::from_millis(20));
    assert_eq!(
        r.last().unwrap(),
        &serde_json::json!({"t": "voice", "web_tts": {"engine": "kokoro", "voice": "bf_emma"}, "ok": true})
    );
    assert_eq!(
        *a.tts.lock().unwrap(),
        Some(WebTts::new("kokoro", "bf_emma"))
    );

    super::ws::web_tts(&a, &serde_json::json!({"engine": "espeak", "voice": "x"}));
    let r = got(&a, Duration::from_millis(20));
    let r = r.last().unwrap();
    assert_eq!(
        (r["ok"].clone(), r["web_tts"]["engine"].clone()),
        (serde_json::json!(false), serde_json::json!("espeak"))
    );
    assert!(r["error"].as_str().unwrap().contains("unknown engine"));
    assert_eq!(a.tts.lock().unwrap().as_ref().unwrap().voice, "bf_emma");

    app.web_voice(a.id, None, "push", Some(true));
    assert_eq!(app.web_voice_client(), Some(a.id));
    assert_eq!(
        web_tts::route(),
        Some((a.id, Some(WebTts::new("kokoro", "bf_emma"))))
    );

    super::ws::web_tts(&b, &serde_json::json!({"engine": "grok", "voice": "ara"}));
    assert_eq!(web_tts::route().unwrap().1.unwrap().engine, "kokoro");
    super::ws::web_tts(&a, &serde_json::Value::Null);
    assert_eq!(web_tts::route(), Some((a.id, Some(WebTts::new("mac", "")))));

    app.web_voice(hub.add().id, None, "push", Some(true));
    let (_, none) = web_tts::route().unwrap();
    assert_eq!(none, None);
    let cfg = web_tts::web_cfg(&app.cfg.voice, none.as_ref());
    assert_eq!(
        (cfg.tts_engine.as_str(), cfg.kokoro_voice.as_str()),
        ("kokoro", "af_heart")
    );

    app.web_voice_off("test");
    assert_eq!(web_tts::route(), None);

    got(&a, Duration::from_millis(20));
    app.web_voice_ctl(a.id, &serde_json::json!({"web_tts_list": true}));
    let r = got(&a, Duration::from_secs(5));
    let l = r
        .iter()
        .find(|v| v["web_tts_list"].is_array())
        .expect("a list");
    assert_eq!(l["t"], "voice");
    assert_eq!(
        l["default"],
        serde_json::json!({"engine": "kokoro", "voice": "af_heart"})
    );
    assert_eq!(
        l["choice"],
        serde_json::json!({"engine": "mac", "voice": ""})
    );
    assert_eq!(
        l["mac"]["engine"],
        serde_json::json!(app.cfg.voice.tts_engine)
    );
    let e = l["web_tts_list"].as_array().unwrap();
    assert_eq!(e[0]["engine"], "kokoro");
    assert!(e
        .iter()
        .all(|x| x["voices"].is_array() && x["available"].is_boolean()));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_client_log_scrubs_secrets() {
    use super::clientlog::scrub;
    let tok = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    let s = scrub(&format!("loaded with {tok}"), tok);
    assert!(!s.contains(tok) && s.contains("[token]"), "{s}");
    let other = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    assert!(!scrub(other, tok).contains("fedcba98"));

    for u in [
        "GET http://127.0.0.1:7963/?t=sekrit123 failed",
        "open /x?a=1&t=sekrit123#h",
        "t=sekrit123",
    ] {
        let s = scrub(u, tok);
        assert!(
            !s.contains("sekrit123") && s.contains("t=[redacted]"),
            "{s}"
        );
    }

    assert_eq!(scrub("set=1 at=2", tok), "set=1 at=2");

    for h in [
        "Cookie: godterm_web=abc; other=1",
        "{\"cookie\":\"godterm_web=abc\"}",
        "Authorization: Bearer abc",
        "authorization=abc",
    ] {
        let s = scrub(h, tok);
        assert!(!s.contains("abc"), "{h} -> {s}");
        assert!(s.contains("[redacted]"), "{s}");
    }

    let b64 = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk";
    let s = scrub(b64, tok);
    assert!(!s.contains("iVBORw0KGgo") && s.contains("[blob"), "{s}");
    assert!(scrub("deadbeefdeadbeefdeadbeefdeadbeef", tok).contains("[hex]"));

    let plain = "conn: closed 1006 after 12.3 s, /js/term.js?v=0a1b2c3d4e5f:12:5";
    assert_eq!(scrub(plain, tok), plain);

    let s = scrub("a\x1b[31mb\nc\rd\u{7}", tok);
    assert!(!s.chars().any(|c| c.is_control()), "{s:?}");

    assert!(scrub(&"x ".repeat(5000), tok).chars().count() <= 2003);
}

#[test]
fn client_log_lines_hide_typed_text_and_are_rate_limited() {
    use super::clientlog::{lines, Rate};
    let tok = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let mut rate = Rate::default();
    let now = Instant::now();
    let v = serde_json::json!({"t": "log", "device": "iPhone Safari standalone 402x874@3",
    "lines": [
        {"at": 1, "cat": "conn", "text": "open"},
        {"cat": "typed", "text": "my password"},
        {"cat": "Clipboard", "text": "secret paste"},
        {"cat": "err\n", "text": format!("token {tok}")},
    ]});
    let out = lines(7, &v, tok, &mut rate, now);
    assert_eq!(out.len(), 4);
    let all = out.concat();
    assert!(
        all.contains(" c7 [iPhone Safari standalone 402x874@3] conn: open\n"),
        "{all}"
    );
    assert!(
        !all.contains("password") && all.contains("typed: [11 chars hidden]"),
        "{all}"
    );
    assert!(!all.contains("secret paste") && all.contains("clipboard: [12 chars hidden]"));
    assert!(
        !all.contains(tok) && all.contains("err: token [token]"),
        "{all}"
    );
    assert!(out
        .iter()
        .all(|l| l.ends_with('\n') && l.matches('\n').count() == 1));

    let many: Vec<_> = (0..200)
        .map(|i| serde_json::json!({"cat": "spam", "text": i}))
        .collect();
    let v = serde_json::json!({"t": "log", "device": "d", "lines": many});
    let mut kept = 0;
    for _ in 0..10 {
        kept += lines(7, &v, tok, &mut rate, now).len();
    }
    assert!((990..=1000).contains(&kept), "{kept}");
    let later = lines(7, &v, tok, &mut rate, now + Duration::from_secs(5));
    assert!(
        later[0].contains("lines dropped (rate limit)"),
        "{}",
        later[0]
    );
    assert!(later.len() <= 101, "{}", later.len());
}

#[test]
fn the_client_log_is_private_and_rotates() {
    use super::clientlog::LogFile;
    let dir = std::env::temp_dir().join(format!("godterm-weblog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let p = dir.join("web-client.log");
    let mut f = LogFile::new(p.clone(), 100);
    for i in 0..10 {
        f.write(&format!("line {i:02} {}\n", "x".repeat(20)))
            .unwrap();
    }
    let cur = std::fs::read_to_string(&p).unwrap();
    let old = std::fs::read_to_string(dir.join("web-client.log.1")).unwrap();
    assert!(
        cur.len() <= 100 && old.len() <= 100,
        "{} {}",
        cur.len(),
        old.len()
    );
    assert!(cur.contains("line 09") && !cur.contains("line 05"));
    assert!(old.contains("line 05") || old.contains("line 06"));

    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o600);
    }

    drop(f);
    let mut f = LogFile::new(p.clone(), 100);
    f.write("again\n").unwrap();
    assert!(std::fs::read_to_string(&p).unwrap().ends_with("again\n"));

    std::fs::remove_file(&p).unwrap();
    f.write("fresh\n").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "fresh\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn git_frames_round_trip() {
    use tungstenite::Message;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, rx) = test_hub("gitws");
    let fx = crate::git_ops::tests::fixture("gitws");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let mut ws = ws_client(hub.port, &cookie);
    match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        AppEvent::Web(ToApp::Hello(_)) => {}
        _ => panic!("not a hello"),
    }
    let env = crate::git_panel::Env {
        cfg: Default::default(),
        helper: None,
        gh: "/nonexistent/gh".into(),
        machine: "t".into(),
    };
    std::fs::write(fx.a.join("n.txt"), "n\n").unwrap();
    let mut ask = |frame: serde_json::Value| -> serde_json::Value {
        ws.send(Message::text(json_str(&frame))).unwrap();
        let (client, v) = match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            AppEvent::Web(ToApp::Git { client, v }) => (client, v),
            _ => panic!("not a git frame"),
        };
        assert_eq!(v["id"], frame["id"]);
        let c = hub.client(client).unwrap();
        crate::app_gitpanel::git_reply(&c.out, &v, Some(fx.a.clone()), &env, &[]);
        loop {
            if let Message::Text(t) = ws.read().unwrap() {
                let r: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                if r["t"] == "git" {
                    assert_eq!(r["id"], frame["id"]);
                    break r;
                }
            }
        }
    };
    let r = ask(serde_json::json!({"t": "git", "op": "view", "tab": "t1", "id": 1}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["view"]["changes"]["untracked"][0], "n.txt");
    let r = ask(
        serde_json::json!({"t": "git", "op": "prepare", "action": "commit", "message": "From the sheet", "tab": "t1", "id": 2}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let plan = r["plan_id"].as_str().unwrap().to_string();
    let r =
        ask(serde_json::json!({"t": "git", "op": "run", "plan_id": plan, "tab": "t1", "id": 3}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["said"].as_str().unwrap().starts_with("Committed"), "{r}");
    assert_eq!(r["view"]["commits"][0]["subject"], "From the sheet");
    drop(ws);
    let _ = std::fs::remove_dir_all(&fx.root);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn gitcfg_frames_round_trip() {
    use tungstenite::Message;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, ah) = crate::confirm_click_tests::test_app("gitcfgws");
    let (hub, tok, h, rx) = test_hub("gitcfgws");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let mut ws = ws_client(hub.port, &cookie);
    match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        AppEvent::Web(ToApp::Hello(_)) => {}
        _ => panic!("not a hello"),
    }
    let mut ask = |frame: serde_json::Value| -> serde_json::Value {
        ws.send(Message::text(json_str(&frame))).unwrap();
        let (client, v) = match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            AppEvent::Web(ToApp::Git { client, v }) => (client, v),
            _ => panic!("not a git frame"),
        };
        assert!(crate::app_gitcfg::is_cfg_frame(&v));
        let c = hub.client(client).unwrap();
        c.out.text(app.gitcfg_frame(&v).to_string());
        loop {
            if let Message::Text(t) = ws.read().unwrap() {
                let r: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                if r["t"] == "git" {
                    assert_eq!(r["id"], frame["id"]);
                    break r;
                }
            }
        }
    };
    let r = ask(serde_json::json!({"t": "git", "op": "cfg_view", "id": "g1"}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["view"]["empty"], true);
    assert_eq!(r["view"]["placeholders"]["rules"][0]["match"], "~/work/**");
    let r = ask(
        serde_json::json!({"t": "git", "op": "cfg_check", "draft": r["view"]["placeholders"], "id": "g2"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["plan_id"].as_str().is_some(), "{r}");
    assert!(r["diff"]
        .to_string()
        .contains("+ rule ~/work/** -> your-org"));
    let r = ask(serde_json::json!({"t": "git", "op": "cfg_nope", "id": "g3"}));
    assert_eq!(r["ok"], false);
    drop(ws);
    let _ = std::fs::remove_dir_all(&h);
    let _ = std::fs::remove_dir_all(&ah);
}

#[cfg(unix)]
#[test]
fn bview_frames_and_pictures_round_trip() {
    use tungstenite::Message;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, rx) = test_hub("bviewws");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let mut ws = ws_client(hub.port, &cookie);
    match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        AppEvent::Web(ToApp::Hello(_)) => {}
        _ => panic!("not a hello"),
    }
    let frame = serde_json::json!({"t": "bview", "id": "b1", "op": "open", "tab": 77});
    ws.send(Message::text(json_str(&frame))).unwrap();
    let (client, v) = match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        AppEvent::Web(ToApp::BrowserView { client, v }) => (client, v),
        _ => panic!("not a bview frame"),
    };
    assert_eq!(v["op"], "open");
    let d = crate::browser_view_tests::tmp("ws");
    let bin = crate::browser_view_tests::fake(
        &d,
        crate::browser_view_tests::BROWSERS,
        r#"[{"session_id":"s-ws","browser_instance_id":"aaa"}]"#,
    );
    let ctx = crate::browser_view_tests::ctx(&d, bin, 77);
    let c = hub.client(client).unwrap();
    crate::browser_view::handle(&ctx, client, &c.out, &v);
    let (mut live, mut pic) = (false, None);
    let end = Instant::now() + Duration::from_secs(8);
    while (!live || pic.is_none()) && Instant::now() < end {
        match ws.read().unwrap() {
            Message::Text(t) => {
                let r: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                if r["t"] == "bview" && r["op"] == "open" {
                    assert_eq!(
                        (r["id"].as_str(), r["state"].as_str()),
                        (Some("b1"), Some("live"))
                    );
                    live = true;
                }
            }
            Message::Binary(b) => {
                let (k, uid, p) = super::ws::unframe(&b).unwrap();
                if k == crate::browser_view::KIND_PICTURE {
                    pic = Some((uid, p.to_vec()));
                }
            }
            _ => {}
        }
    }
    assert!(live, "open answered");
    let (uid, p) = pic.expect("a picture");
    assert_eq!(uid, 77);
    assert_eq!(crate::browser_view::image_size(&p), Some((1280, 800)));

    drop(ws);
    crate::browser_view::unwatch(client, None);
    let end = Instant::now() + Duration::from_secs(6);
    while crate::browser_view::streaming("s-ws") && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!crate::browser_view::streaming("s-ws"));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn a_browser_refused_for_the_mac_mute_can_lift_it() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("web-unmute-mac");
    app.voice.muted = true;
    app.web_voice_ctl(1, &serde_json::json!({"unmute_mac": true}));
    assert!(!app.voice.muted, "unmute_mac lifts GodTerm's own mute");

    app.web_voice_ctl(1, &serde_json::json!({"unmute_mac": true}));
    assert!(!app.voice.muted);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_request_spoken_into_a_browser_is_that_browsers() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, home) = crate::confirm_click_tests::test_app("web-spoken-turn");
    let (tx, _rx) = std::sync::mpsc::channel();
    let hub = Hub::new(0, "x".repeat(64), vec![], None, tx);
    let a = hub.add();
    app.web = Some(super::app_web::WebSide::new(Arc::clone(&hub)));

    assert_eq!(app.turn_web_client(true), None);
    app.web_voice(a.id, Some(true), "open", None);
    assert_eq!(
        app.turn_web_client(true),
        Some(a.id),
        "spoken into browser a"
    );

    assert_eq!(app.turn_web_client(false), None);
    let _ = std::fs::remove_dir_all(home);
}

fn read_response(s: &mut std::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        head.push(b[0]);
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let len: usize = text
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).unwrap();
    text + &String::from_utf8_lossy(&body)
}

#[test]
fn http_keeps_a_connection_for_the_next_request() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, _rx) = test_hub("keep");
    let p = hub.port;
    let host = format!("127.0.0.1:{p}");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let mut s = std::net::TcpStream::connect(("127.0.0.1", p)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    s.write_all(
        format!("GET /app.css HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let r1 = read_response(&mut s);
    assert_eq!(status(&r1), "200", "{r1}");
    assert!(r1.contains("Connection: keep-alive"), "{r1}");
    s.write_all(format!("GET /js/main.js HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\nGET /nothing-here HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n").as_bytes()).unwrap();
    let r2 = read_response(&mut s);
    assert_eq!(status(&r2), "200", "{r2}");
    assert!(
        r2.contains("text/javascript") || r2.contains("application/javascript"),
        "{r2}"
    );
    let r3 = read_response(&mut s);
    assert_eq!(status(&r3), "404", "{r3}");
    assert!(r3.contains("Connection: keep-alive"), "{r3}");

    s.write_all(format!("GET /app.css HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
    let r4 = read_response(&mut s);
    assert!(r4.contains("Connection: close"), "{r4}");
    let mut rest = Vec::new();
    s.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty());
    let mut s = std::net::TcpStream::connect(("127.0.0.1", p)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        format!("GET /app.css HTTP/1.0\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let r5 = read_response(&mut s);
    assert!(r5.contains("Connection: close"), "{r5}");
    let mut rest = Vec::new();
    s.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty());
    hub.stop();
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn http_read_head_takes_the_head_only() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (mut srv, _) = l.accept().unwrap();
    srv.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(b"GET /a?x=1 HTTP/1.1\r\nHost: h\r").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    c.write_all(b"\n\r\nAFTER").unwrap();
    let req = http::read_head(&mut srv).unwrap();
    assert_eq!(
        (req.method.as_str(), req.path.as_str(), req.query.as_str()),
        ("GET", "/a", "x=1")
    );
    assert!(req.keep_alive);
    let mut after = [0u8; 5];
    srv.read_exact(&mut after).unwrap();
    assert_eq!(&after, b"AFTER");

    c.write_all(b"GET /b HTTP/1.1\r\nConnection: close\r\n\r\nGET /c HTTP/1.0\r\n\r\n")
        .unwrap();
    let b = http::read_head(&mut srv).unwrap();
    assert_eq!(b.path, "/b");
    assert!(!b.keep_alive);
    let c2 = http::read_head(&mut srv).unwrap();
    assert_eq!(c2.path, "/c");
    assert!(!c2.keep_alive, "HTTP/1.0 closes unless asked to keep");

    drop(c);
    assert!(http::read_head(&mut srv).is_none());
}

#[test]
fn websocket_sends_a_big_snapshot_gzipped_when_asked() {
    use tungstenite::Message;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, rx) = test_hub("wsgz");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));

    let (p, _prx) = shell_tab(
        &[
            "lines:2500:\\e[32mline {i5}\\e[0m the quick brown fox jumps over the lazy dog",
            "say:READY\\n",
            "cat",
        ],
        24,
        80,
    );
    let uid = p.uid;
    let mut ws = ws_client(hub.port, &cookie);
    match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        AppEvent::Web(ToApp::Hello(_)) => {}
        _ => panic!("not a hello"),
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    let plain = loop {
        ws.send(Message::text(json_str(
            &serde_json::json!({"t": "subscribe", "tabs": [uid]}),
        )))
        .unwrap();
        let snap = loop {
            if let Message::Text(t) = ws.read().unwrap() {
                let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                if v["t"] == "snapshot" {
                    break t.to_string();
                }
            }
        };
        ws.send(Message::text(json_str(
            &serde_json::json!({"t": "unsubscribe", "tabs": [uid]}),
        )))
        .unwrap();
        if snap.contains("READY") || Instant::now() > deadline {
            break snap;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        plain.len() > ws::GZ_MIN,
        "a big snapshot: {} bytes",
        plain.len()
    );

    ws.send(Message::text(json_str(
        &serde_json::json!({"t": "subscribe", "tabs": [uid], "gz": true}),
    )))
    .unwrap();
    let packed = loop {
        if let Message::Binary(b) = ws.read().unwrap() {
            let (k, u, d) = ws::unframe(&b).unwrap();
            if k == ws::KIND_SNAPSHOT_GZ {
                assert_eq!(u, uid);
                break d.to_vec();
            }
        }
    };
    assert!(
        packed.len() < plain.len() / 4,
        "gzipped {} of {}",
        packed.len(),
        plain.len()
    );
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&packed[..]), &mut text)
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["t"], "snapshot");
    assert_eq!(v["tab"], uid);
    assert!(v["data"].as_str().unwrap().contains("line 02499"));
    eprintln!(
        "web: snapshot {} bytes, gzipped {}",
        plain.len(),
        packed.len()
    );

    let sid = v["sid"].as_str().unwrap().to_string();
    let pos = v["pos"].as_u64().unwrap();
    ws.send(Message::text(json_str(
        &serde_json::json!({"t": "unsubscribe", "tabs": [uid]}),
    )))
    .unwrap();
    ws.send(Message::text(json_str(
        &serde_json::json!({"t": "subscribe", "tabs": [uid], "gz": true, "resume": {uid.to_string(): {"sid": sid, "pos": pos}}}),
    )))
    .unwrap();
    loop {
        if let Message::Text(t) = ws.read().unwrap() {
            let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
            if v["t"] == "resume" {
                break;
            }
        }
    }
    drop(ws);
    drop(p);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn netstat_lines_name_both_ends_of_a_connection() {
    let text = "\r\nActive Connections\r\n\r\n  Proto  Local Address          Foreign Address        State           PID\r\n  TCP    127.0.0.1:7878         127.0.0.1:50112        ESTABLISHED     100\r\n  TCP    127.0.0.1:50112        127.0.0.1:7878         ESTABLISHED     4242\r\n  TCP    127.0.0.1:50113        127.0.0.1:7878         ESTABLISHED     77\r\n  TCP    127.0.0.1:7878         0.0.0.0:0              LISTENING       100\r\n  TCP    [::1]:7878             [::1]:50200            ESTABLISHED     100\r\n  TCP    [::1]:50200            [::1]:7878             ESTABLISHED     55\r\n  TCP    127.0.0.1:50114        127.0.0.1:7878         TIME_WAIT       0\r\n";
    assert_eq!(
        auth::parse_netstat_conn(text, "127.0.0.1", 50112),
        vec![100, 4242]
    );
    assert_eq!(
        auth::parse_netstat_conn(text, "[::1]", 50200),
        vec![100, 55]
    );
    assert!(auth::parse_netstat_conn(text, "127.0.0.1", 50114).is_empty());
    assert!(auth::parse_netstat_conn(text, "127.0.0.1", 1).is_empty());
}

#[test]
fn websocket_file_upload_is_saved_under_uploads() {
    use tungstenite::Message;
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, h, rx) = test_hub("ws-file");
    let cookie = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let mut ws = ws_client(hub.port, &cookie);
    let _ = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut ask = |name: &str, data: &str| {
        ws.send(Message::text(json_str(
            &serde_json::json!({"t": "file", "id": "f1", "name": name, "data": data}),
        )))
        .unwrap();
        loop {
            if let Message::Text(t) = ws.read().unwrap() {
                let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                if v["id"] == "f1" {
                    break v;
                }
            }
        }
    };
    let ok = ask("../../evil name.txt", "data:text/plain;base64,aGVsbG8=");
    let path = std::path::PathBuf::from(ok["path"].as_str().unwrap());
    assert_eq!(ok["t"], "file");
    assert!(path.starts_with(crate::assistant_images::uploads_dir()));
    assert!(path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .ends_with("-evil_name.txt"));
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    let bad = ask("x", "not base64 !!");
    assert_eq!(bad["t"], "error");
    let _ = std::fs::remove_dir_all(&h);
}
