use super::login::{self, Login};
use super::webauthn::soft::Authenticator;
use super::webauthn::{b64_decode, b64_encode};
use super::*;
use crate::config::testing::LOCK as ENV_LOCK;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::time::Duration;

const ORIGIN: &str = "https://mac.example.ts.net";
const RP: &str = "mac.example.ts.net";
const T0: i64 = 1_800_000_000;

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("godterm-login-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn enroll(l: &Login, a: &Authenticator, label: &str, now: i64) -> Result<Value, String> {
    let o = l.register_begin(now)?;
    let ch = b64_decode(o["publicKey"]["challenge"].as_str().unwrap()).unwrap();
    assert_eq!(o["publicKey"]["rp"]["id"], RP);
    assert_eq!(
        o["publicKey"]["authenticatorSelection"]["userVerification"],
        "required"
    );
    assert_eq!(o["publicKey"]["pubKeyCredParams"][0]["alg"], -7);
    let client = Authenticator::client_json("webauthn.create", &ch, ORIGIN);
    let (cd, att) = a.create(RP, &client);
    l.register_finish(
        o["cid"].as_str().unwrap(),
        label,
        &b64_encode(&cd),
        &b64_encode(&att),
        now,
    )
}

struct Asserted {
    cid: String,
    id: String,
    cd: String,
    ad: String,
    sig: String,
}

fn assert_with(l: &Login, a: &mut Authenticator, now: i64, origin: &str) -> Asserted {
    let o = l.login_begin(now).unwrap();
    assert_eq!(o["publicKey"]["rpId"], RP);
    assert_eq!(o["publicKey"]["userVerification"], "required");
    let ch = b64_decode(o["publicKey"]["challenge"].as_str().unwrap()).unwrap();
    let client = Authenticator::client_json("webauthn.get", &ch, origin);
    let (cd, ad, sig) = a.get(RP, &client);
    Asserted {
        cid: o["cid"].as_str().unwrap().to_string(),
        id: b64_encode(&a.id),
        cd: b64_encode(&cd),
        ad: b64_encode(&ad),
        sig: b64_encode(&sig),
    }
}

fn finish(l: &Login, x: &Asserted, now: i64) -> Result<String, ()> {
    l.login_finish(&x.cid, &x.id, &x.cd, &x.ad, &x.sig, "Test device", now)
}

fn ready(tag: &str) -> (Login, Authenticator, std::path::PathBuf) {
    let d = dir(tag);
    let l = Login::open(d.clone());
    l.set_remote_url(ORIGIN).unwrap();
    let a = Authenticator::new(b"credential-one");
    enroll(&l, &a, "iPhone", T0).unwrap();
    (l, a, d)
}

#[test]
fn addresses_must_be_https_hostnames() {
    assert_eq!(
        login::parse_origin("https://Mac.Example.ts.net/").unwrap(),
        (ORIGIN.to_string(), RP.to_string())
    );
    assert_eq!(
        login::parse_origin("https://mac.example.ts.net:443")
            .unwrap()
            .0,
        ORIGIN
    );
    assert_eq!(
        login::parse_origin("https://mac.example.ts.net:8443")
            .unwrap()
            .0,
        "https://mac.example.ts.net:8443"
    );
    assert!(login::parse_origin("http://localhost:7878").is_ok());
    for bad in [
        "http://mac.example.ts.net",
        "https://127.0.0.1",
        "https://[::1]",
        "https://localhost",
        "https://a.b/path",
        "https://u@a.b",
        "mac.example.ts.net",
        "https://a.b:0",
        "https://a.b:99999",
        "https://",
        "https://-a.b",
    ] {
        assert!(login::parse_origin(bad).is_err(), "{bad}");
    }
}

#[test]
fn enabling_needs_an_address_and_a_passkey_and_shows_the_recovery_code_once() {
    let d = dir("enable");
    let l = Login::open(d.clone());
    assert!(l.register_begin(T0).is_err());
    l.set_remote_url(ORIGIN).unwrap();
    assert!(l.enable().unwrap_err().contains("passkey"));
    let a = Authenticator::new(b"c1");
    enroll(&l, &a, "iPhone", T0).unwrap();
    assert!(!l.enabled());
    let code = l.enable().unwrap();
    assert!(l.enabled());
    assert_eq!(code.len(), 4 * 5 + 4);
    assert!(l.enable().is_err());
    let files = std::fs::read_to_string(d.join(login::PASSKEYS_FILE)).unwrap();
    assert!(
        !files.contains(&code.replace('-', "")),
        "recovery code stored in the clear"
    );
    assert!(!files.contains(&code));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(d.join(login::PASSKEYS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(m & 0o777, 0o600);
    }
    let l2 = Login::open(d.clone());
    assert!(l2.enabled());
    assert_eq!(l2.passkeys().len(), 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_passkey_signs_in_and_the_challenge_is_single_use() {
    let (l, mut a, d) = ready("signin");
    l.enable().unwrap();
    let x = assert_with(&l, &mut a, T0 + 10, ORIGIN);
    let cookie = finish(&l, &x, T0 + 11).unwrap();
    assert_eq!(cookie.len(), 64);
    assert!(l.check_session(&cookie, T0 + 12).is_some());
    assert!(finish(&l, &x, T0 + 12).is_err(), "replayed challenge");
    let sessions = std::fs::read_to_string(d.join(login::SESSIONS_FILE)).unwrap();
    assert!(
        !sessions.contains(&cookie),
        "session id stored in the clear"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn expired_challenges_wrong_origins_and_bad_signatures_are_refused() {
    let (l, mut a, d) = ready("refuse");
    l.enable().unwrap();

    let x = assert_with(&l, &mut a, T0 + 10, ORIGIN);
    assert!(finish(&l, &x, T0 + 10 + login::CHALLENGE_TTL + 1).is_err());

    let x = assert_with(&l, &mut a, T0 + 10, "https://evil.example.com");
    assert!(finish(&l, &x, T0 + 11).is_err());

    let mut x = assert_with(&l, &mut a, T0 + 10, ORIGIN);
    let mut sig = b64_decode(&x.sig).unwrap();
    let last = sig.len() - 1;
    sig[last] ^= 0x01;
    x.sig = b64_encode(&sig);
    assert!(finish(&l, &x, T0 + 11).is_err());

    let mut x = assert_with(&l, &mut a, T0 + 10, ORIGIN);
    x.id = b64_encode(b"unknown credential");
    assert!(finish(&l, &x, T0 + 11).is_err());

    let x = assert_with(&l, &mut a, T0 + 10, ORIGIN);
    let mut y = assert_with(&l, &mut a, T0 + 10, ORIGIN);
    y.cid = x.cid.clone();
    assert!(
        finish(&l, &y, T0 + 11).is_err(),
        "signature over another challenge"
    );

    let x = assert_with(&l, &mut a, T0 + 10, ORIGIN);
    let o = l.register_begin(T0 + 10).unwrap();
    let mut z = assert_with(&l, &mut a, T0 + 10, ORIGIN);
    z.cid = o["cid"].as_str().unwrap().to_string();
    assert!(
        finish(&l, &z, T0 + 11).is_err(),
        "a registration challenge cannot log in"
    );
    assert!(finish(&l, &x, T0 + 12).is_ok());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_counter_that_goes_back_is_refused() {
    let (l, mut a, d) = ready("counter");
    l.enable().unwrap();
    a.counter = 10;
    let x = assert_with(&l, &mut a, T0 + 1, ORIGIN);
    assert!(finish(&l, &x, T0 + 2).is_ok());
    a.counter = 4;
    let x = assert_with(&l, &mut a, T0 + 3, ORIGIN);
    assert!(finish(&l, &x, T0 + 4).is_err());
    a.counter = 11;
    let x = assert_with(&l, &mut a, T0 + 5, ORIGIN);
    assert!(finish(&l, &x, T0 + 6).is_ok());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn login_is_closed_while_off() {
    let (l, mut a, d) = ready("off");
    assert!(l.login_begin(T0).is_err());
    l.enable().unwrap();
    let x = assert_with(&l, &mut a, T0, ORIGIN);
    l.disable(T0);
    assert!(finish(&l, &x, T0 + 1).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn sessions_expire_renew_and_are_revoked() {
    let (l, _a, d) = ready("sessions");
    let c = l.issue_session("Mac, Safari", T0);
    let sid = l.check_session(&c, T0 + 5).unwrap();
    assert!(l.check_session("x", T0).is_none());
    assert!(l.check_session(&"0".repeat(64), T0).is_none());
    assert!(l.check_session(&c, T0 + login::SESSION_TTL - 1).is_some());
    assert!(
        l.check_session(&c, T0 + login::SESSION_TTL + 100).is_some(),
        "renewed by use"
    );
    let late = T0 + login::SESSION_TTL + 100 + login::SESSION_TTL + 1;
    assert!(l.check_session(&c, late).is_none(), "expired");
    l.revoke_all(None, late);

    let c1 = l.issue_session("iPhone, Safari", T0 + 1000);
    let c2 = l.issue_session("iPad, Safari", T0 + 1000);
    let list = l.sessions(T0 + 1001);
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|s| s.get("hash").is_none()));
    let s1 = l.check_session(&c1, T0 + 1002).unwrap();
    assert!(l.revoke_session(&s1, T0 + 1003));
    assert!(l.check_session(&c1, T0 + 1004).is_none());
    assert!(l.check_session(&c2, T0 + 1004).is_some());
    assert!(!l.session_live(&sid, late));
    assert_eq!(l.revoke_all(None, T0 + 1005), 1);
    assert!(l.check_session(&c2, T0 + 1006).is_none());

    let l2 = Login::open(d.clone());
    let c3 = l2.issue_session("x", T0 + 2000);
    assert!(
        l.check_session(&c3, T0 + 2001).is_some(),
        "sessions are shared through the file"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_recovery_code_works_once_and_a_reset_from_the_machine_turns_login_off() {
    let (l, _a, d) = ready("recovery");
    let code = l.enable().unwrap();
    assert!(l
        .use_recovery("AAAA-AAAA-AAAA-AAAA-AAAA", "x", T0)
        .is_none());
    let lower = code.to_lowercase().replace('-', " ");
    let c = l.use_recovery(&lower, "Mac, Safari", T0).unwrap();
    assert!(l.check_session(&c, T0 + 1).is_some());
    assert!(l.use_recovery(&code, "x", T0 + 2).is_none(), "used up");
    let fresh = l.new_recovery();
    assert_ne!(fresh, code);

    assert!(Login::reset(&d, T0 + 3));
    assert!(!l.enabled(), "a running GodTerm sees the reset");
    assert!(l.check_session(&c, T0 + 4).is_none());
    assert!(l.use_recovery(&fresh, "x", T0 + 5).is_none());
    assert_eq!(l.passkeys().len(), 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn passkeys_can_be_renamed_and_removed_but_not_the_last_one_while_login_is_on() {
    let (l, _a, d) = ready("manage");
    let b = Authenticator::new(b"credential-two");
    assert!(enroll(&l, &b, "Key", T0 + 1).is_ok());
    assert!(
        enroll(&l, &b, "Key again", T0 + 2).is_err(),
        "the same credential twice"
    );
    let list = l.passkeys();
    assert_eq!(list.len(), 2);
    let pid = list[0]["pid"].as_str().unwrap().to_string();
    assert!(l.rename_passkey(&pid, "  Work phone  "));
    assert_eq!(l.passkeys()[0]["label"], "Work phone");
    assert!(!l.rename_passkey(&pid, "   "));
    l.enable().unwrap();
    assert!(l.remove_passkey(&pid).is_ok());
    let last = l.passkeys()[0]["pid"].as_str().unwrap().to_string();
    assert!(l.remove_passkey(&last).is_err());
    l.disable(T0);
    assert!(l.remove_passkey(&last).is_ok());
    assert!(l.passkeys().is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_address_cannot_change_while_login_is_on() {
    let (l, _a, d) = ready("addr");
    l.enable().unwrap();
    assert!(l.set_remote_url("https://other.example.ts.net").is_err());
    assert!(l.set_remote_url(ORIGIN).is_ok());
    assert!(l.clear_remote_url().is_err());
    l.disable(T0);
    assert!(l.set_remote_url("https://other.example.ts.net").is_ok());
    assert!(
        l.passkeys()[0]["current"] == false,
        "a passkey for the old address is flagged"
    );
    assert!(l.enable().is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn pairing_links_work_once_and_expire() {
    let (l, _a, d) = ready("pair");
    let c = l.new_pair(T0);
    assert!(!l.use_pair("nope", T0));
    assert!(l.use_pair(&c, T0 + 5));
    assert!(!l.use_pair(&c, T0 + 6));
    let c = l.new_pair(T0);
    assert!(!l.use_pair(&c, T0 + login::PAIR_TTL + 1));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn too_many_failures_block_one_client_not_everyone() {
    let (l, _a, d) = ready("limit");
    for _ in 0..auth::MAX_FAILS {
        assert!(!l.blocked("1.2.3.4"));
        l.fail("1.2.3.4");
    }
    assert!(l.blocked("1.2.3.4"));
    assert!(!l.blocked("5.6.7.8"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn only_a_read_only_login_tool_exists_for_the_assistant_and_tabs() {
    let names: Vec<&str> = crate::control::TOOLS
        .iter()
        .map(|t| t.name)
        .filter(|n| n.contains("passkey") || n.contains("web_login") || n.contains("remote_access"))
        .collect();
    assert!(names.is_empty(), "{names:?}");
    for n in ["login_status"] {
        assert!(crate::control::TOOLS.iter().any(|t| t.name == n));
    }
    for n in crate::control::TOOLS.iter().map(|t| t.name) {
        assert!(
            !n.contains("enable_login") && !n.contains("disable_login"),
            "{n}"
        );
    }
    assert!(crate::control::READ_ONLY.contains(&"login_status"));
}

fn hub_with_login(tag: &str) -> (Arc<Hub>, String, Authenticator, std::path::PathBuf) {
    let h = std::env::temp_dir().join(format!("godterm-weblogin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    crate::config::testing::set_home(&h);
    let (tx, rx) = std::sync::mpsc::channel();
    std::mem::forget(rx);
    let hub = start(0, vec![], tx).unwrap();
    let tok = auth::load_or_create().unwrap();
    hub.login.set_remote_url(ORIGIN).unwrap();
    let a = Authenticator::new(b"http-credential");
    enroll(&hub.login, &a, "Phone", T0).unwrap();
    (hub, tok, a, h)
}

fn raw(port: u16, req: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        req.replacen("\r\n\r\n", "\r\nConnection: close\r\n\r\n", 1)
            .as_bytes(),
    )
    .unwrap();
    let mut out = vec![];
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn post(port: u16, path: &str, host: &str, extra: &str, body: &Value) -> String {
    let b = body.to_string();
    raw(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\n\r\n{b}",
            b.len()
        ),
    )
}

fn json_body(resp: &str) -> Value {
    serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap_or("{}")).unwrap_or(json!({}))
}

fn status_of(resp: &str) -> &str {
    resp.split(' ').nth(1).unwrap_or("")
}

fn set_cookie(resp: &str) -> Option<String> {
    resp.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .map(|l| {
            l.split_once(':')
                .unwrap()
                .1
                .trim()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
}

fn ws_status(port: u16, host: &str, extra: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        format!("GET /ws HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: https://{host}\r\n{extra}\r\n").as_bytes(),
    )
    .unwrap();
    let mut b = [0u8; 12];
    let _ = s.read_exact(&mut b);
    String::from_utf8_lossy(&b).into_owned()
}

#[test]
fn with_login_on_remote_requests_need_a_session_and_loopback_keeps_its_token() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, mut a, h) = hub_with_login("gate");
    let p = hub.port;
    let local = format!("127.0.0.1:{p}");
    let remote = RP.to_string();
    let legacy = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let html = "Accept: text/html\r\n";

    let r = raw(
        p,
        &format!("GET / HTTP/1.1\r\nHost: {remote}\r\nCookie: {legacy}\r\n\r\n"),
    );
    assert_eq!(
        status_of(&r),
        "200",
        "login is off: the token cookie still works remotely"
    );

    hub.login.enable().unwrap();

    let r = raw(
        p,
        &format!("GET / HTTP/1.1\r\nHost: {local}\r\nCookie: {legacy}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "200", "loopback keeps the token cookie");
    let r = raw(
        p,
        &format!("GET /?t={tok} HTTP/1.1\r\nHost: {local}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "303", "loopback token link");

    let r = raw(
        p,
        &format!("GET / HTTP/1.1\r\nHost: {remote}\r\nCookie: {legacy}\r\n{html}\r\n"),
    );
    assert_eq!(status_of(&r), "303", "{r}");
    assert!(r.contains("Location: /login\r\n"));
    let r = raw(
        p,
        &format!("GET /api HTTP/1.1\r\nHost: {remote}\r\nCookie: {legacy}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "401");
    let r = raw(
        p,
        &format!("GET /?t={tok} HTTP/1.1\r\nHost: {remote}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "403");
    assert!(set_cookie(&r).is_none());
    assert!(!r.contains("wrong token"));

    for fwd in [
        "X-Forwarded-For: 203.0.113.9",
        "Tailscale-User-Login: a@b.c",
        "X-Forwarded-Proto: https",
    ] {
        let r = raw(
            p,
            &format!("GET / HTTP/1.1\r\nHost: {local}\r\nCookie: {legacy}\r\n{fwd}\r\n\r\n"),
        );
        assert_eq!(
            status_of(&r),
            "401",
            "{fwd}: a proxied request is remote even with a loopback host: {r}"
        );
    }

    let r = raw(p, &format!("GET /login HTTP/1.1\r\nHost: {remote}\r\n\r\n"));
    assert_eq!(status_of(&r), "200");
    assert!(r.contains("text/html"));
    let r = raw(p, &format!("GET /sw.js HTTP/1.1\r\nHost: {remote}\r\n\r\n"));
    assert_eq!(status_of(&r), "200");

    assert!(!ws_status(p, &remote, &format!("Cookie: {legacy}\r\n")).contains("101"));
    assert!(!ws_status(p, &remote, "").contains("101"));

    let st = raw(
        p,
        &format!("GET /auth/state HTTP/1.1\r\nHost: {remote}\r\n\r\n"),
    );
    assert_eq!(json_body(&st)["enabled"], true);
    assert!(!st.contains("passkey"));

    let origin_hdr = format!("Origin: {ORIGIN}\r\n");
    let r = post(p, "/auth/login/begin", &remote, "", &json!({}));
    assert_eq!(status_of(&r), "403", "no origin");
    let begin = post(p, "/auth/login/begin", &remote, &origin_hdr, &json!({}));
    assert_eq!(status_of(&begin), "200", "{begin}");
    let o = json_body(&begin);
    let ch = b64_decode(o["publicKey"]["challenge"].as_str().unwrap()).unwrap();
    let client = Authenticator::client_json("webauthn.get", &ch, ORIGIN);
    let (cd, ad, sig) = a.get(RP, &client);
    let fin = json!({"cid": o["cid"], "id": b64_encode(&a.id), "clientDataJSON": b64_encode(&cd),
        "authenticatorData": b64_encode(&ad), "signature": b64_encode(&sig)});
    let wrong = post(
        p,
        "/auth/login/finish",
        &remote,
        "Origin: https://evil.example\r\n",
        &fin,
    );
    assert_eq!(status_of(&wrong), "403");
    let r = post(p, "/auth/login/finish", &remote, &origin_hdr, &fin);
    assert_eq!(status_of(&r), "200", "{r}");
    let cookie = set_cookie(&r).unwrap();
    assert!(cookie.starts_with(&format!("{}=", login::SESSION_COOKIE)));
    let sc = r.lines().find(|l| l.starts_with("Set-Cookie")).unwrap();
    assert!(
        sc.contains("HttpOnly") && sc.contains("Secure") && sc.contains("SameSite=Strict"),
        "{sc}"
    );
    let replay = post(p, "/auth/login/finish", &remote, &origin_hdr, &fin);
    assert_eq!(status_of(&replay), "401");

    let r = raw(
        p,
        &format!("GET / HTTP/1.1\r\nHost: {remote}\r\nCookie: {cookie}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "200");
    assert!(ws_status(p, &remote, &format!("Cookie: {cookie}\r\n")).contains("101"));

    let r = raw(
        p,
        &format!("GET /auth/status HTTP/1.1\r\nHost: {remote}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "401");
    let r = raw(
        p,
        &format!("GET /auth/status HTTP/1.1\r\nHost: {remote}\r\nCookie: {legacy}\r\n\r\n"),
    );
    assert_eq!(
        status_of(&r),
        "401",
        "the token cookie is not a session for remote requests"
    );
    let r = raw(
        p,
        &format!("GET /auth/status HTTP/1.1\r\nHost: {remote}\r\nCookie: {cookie}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "200", "{r}");
    let s = json_body(&r);
    assert_eq!(s["sessions"][0]["current"], true);
    assert!(
        !r.contains("\"key\"") && !r.contains(&b64_encode(&a.public())),
        "no key material over http"
    );

    let sid = s["sessions"][0]["sid"].as_str().unwrap().to_string();
    let other = format!(
        "{}={}",
        login::SESSION_COOKIE,
        hub.login.issue_session("Other", login::now())
    );
    let r = post(
        p,
        "/auth/session/revoke",
        &remote,
        &format!("Origin: {ORIGIN}\r\nCookie: {other}\r\n"),
        &json!({"sid": sid}),
    );
    assert_eq!(status_of(&r), "200");
    let r = raw(
        p,
        &format!("GET / HTTP/1.1\r\nHost: {remote}\r\nCookie: {cookie}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "401", "a revoked session is refused at once");

    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn enrolling_and_switching_login_need_a_trusted_session_and_the_right_origin() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, _a, h) = hub_with_login("enroll");
    let p = hub.port;
    let local = format!("127.0.0.1:{p}");
    let remote = RP.to_string();
    let legacy = format!("{}={}", auth::COOKIE, auth::cookie_value(&tok));
    let same_origin = format!("Origin: http://{local}\r\n");
    let remote_origin = format!("Origin: {ORIGIN}\r\n");

    for path in [
        "register/begin",
        "enable",
        "disable",
        "pair",
        "passkey/remove",
        "session/revoke_all",
        "recovery/new",
        "remote",
    ] {
        let r = post(
            p,
            &format!("/auth/{path}"),
            &remote,
            &remote_origin,
            &json!({}),
        );
        assert_eq!(status_of(&r), "401", "{path} unauthenticated remote: {r}");
        let r = post(
            p,
            &format!("/auth/{path}"),
            &local,
            &same_origin,
            &json!({}),
        );
        assert_eq!(status_of(&r), "401", "{path} unauthenticated local");
        let r = post(
            p,
            &format!("/auth/{path}"),
            &local,
            &format!("Origin: https://evil.example\r\nCookie: {legacy}\r\n"),
            &json!({}),
        );
        assert_eq!(status_of(&r), "403", "{path} from another origin");
    }
    let r = raw(p, &format!("POST /auth/enable HTTP/1.1\r\nHost: {local}\r\nOrigin: http://{local}\r\nCookie: {legacy}\r\nContent-Length: 2\r\n\r\n{{}}"));
    assert_eq!(status_of(&r), "415", "a form post is refused");

    let r = post(
        p,
        "/auth/register/begin",
        &local,
        &format!("{same_origin}Cookie: {legacy}\r\n"),
        &json!({}),
    );
    assert_eq!(status_of(&r), "200", "{r}");
    assert_eq!(json_body(&r)["publicKey"]["rp"]["id"], RP);

    let r = post(
        p,
        "/auth/enable",
        &local,
        &format!("{same_origin}Cookie: {legacy}\r\n"),
        &json!({}),
    );
    assert_eq!(status_of(&r), "200", "{r}");
    let code = json_body(&r)["recovery"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 24);
    assert!(hub.login.enabled());

    let r = post(
        p,
        "/auth/register/begin",
        &remote,
        &format!("{remote_origin}Cookie: {legacy}\r\n"),
        &json!({}),
    );
    assert_eq!(
        status_of(&r),
        "401",
        "with login on, the token cookie cannot enroll from remote"
    );

    let r = post(
        p,
        "/auth/pair",
        &local,
        &format!("{same_origin}Cookie: {legacy}\r\n"),
        &json!({}),
    );
    assert_eq!(status_of(&r), "200");
    let url = json_body(&r)["url"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!("{ORIGIN}/auth/pair?c=")));
    let path = url.strip_prefix(ORIGIN).unwrap();
    let r = raw(p, &format!("GET {path} HTTP/1.1\r\nHost: {remote}\r\n\r\n"));
    assert_eq!(status_of(&r), "303", "{r}");
    let cookie = set_cookie(&r).unwrap();
    let r = raw(p, &format!("GET {path} HTTP/1.1\r\nHost: {remote}\r\n\r\n"));
    assert_eq!(status_of(&r), "403", "a pairing link works once");
    let r = raw(
        p,
        &format!("GET /auth/status HTTP/1.1\r\nHost: {remote}\r\nCookie: {cookie}\r\n\r\n"),
    );
    assert_eq!(status_of(&r), "200");

    let r = post(
        p,
        "/auth/remote",
        &remote,
        &format!("{remote_origin}Cookie: {cookie}\r\n"),
        &json!({"action": "off"}),
    );
    assert_eq!(status_of(&r), "400", "the address stays while login is on");

    let r = post(
        p,
        "/auth/disable",
        &remote,
        &format!("{remote_origin}Cookie: {cookie}\r\n"),
        &json!({}),
    );
    assert_eq!(status_of(&r), "200", "{r}");
    assert!(!hub.login.enabled());
    let r = raw(
        p,
        &format!("GET /auth/status HTTP/1.1\r\nHost: {remote}\r\nCookie: {cookie}\r\n\r\n"),
    );
    assert_eq!(
        status_of(&r),
        "401",
        "turning login off signs every session out"
    );
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn funnel_needs_login_and_the_remote_actions_drive_tailscale() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (hub, tok, _a, h) = hub_with_login("funnel");
    let ts = Arc::new(remote::fake::Ts::new(hub.port));
    {
        let t = Arc::clone(&ts);
        *hub.ts_run.lock().unwrap() = Arc::new(move |a: &[String]| t.run(a));
    }
    let p = hub.port;
    let local = format!("127.0.0.1:{p}");
    let cookie = format!("Cookie: {}={}\r\n", auth::COOKIE, auth::cookie_value(&tok));
    let hdr = format!("Origin: http://{local}\r\n{cookie}");
    let r = post(
        p,
        "/auth/remote",
        &local,
        &hdr,
        &json!({"action": "funnel"}),
    );
    assert_eq!(status_of(&r), "400");
    assert!(json_body(&r)["error"]
        .as_str()
        .unwrap()
        .contains("login on first"));
    let r = post(p, "/auth/remote", &local, &hdr, &json!({"action": "serve"}));
    assert_eq!(status_of(&r), "200", "{r}");
    assert_eq!(json_body(&r)["remote"]["url"], ORIGIN);
    assert_eq!(json_body(&r)["remote"]["public"], false);
    hub.login.enable().unwrap();
    let r = post(
        p,
        "/auth/remote",
        &local,
        &hdr,
        &json!({"action": "funnel"}),
    );
    assert_eq!(status_of(&r), "200", "{r}");
    assert_eq!(json_body(&r)["remote"]["public"], true);
    let r = post(p, "/auth/disable", &local, &hdr, &json!({}));
    assert_eq!(status_of(&r), "200");
    let r = raw(
        p,
        &format!("GET /auth/remote HTTP/1.1\r\nHost: {local}\r\n{cookie}\r\n"),
    );
    assert_eq!(
        json_body(&r)["remote"]["public"],
        false,
        "disabling login closed Funnel first"
    );
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
#[ignore]
fn serve_for_the_browser_test() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let port: u16 = std::env::var("GT_E2E_PORT").unwrap().parse().unwrap();
    let h = std::path::PathBuf::from(std::env::var("GT_E2E_HOME").unwrap());
    std::fs::create_dir_all(&h).unwrap();
    crate::config::testing::set_home(&h);
    let (tx, rx) = std::sync::mpsc::channel();
    std::mem::forget(rx);
    let hub = start(port, vec![], tx).unwrap();
    hub.login
        .set_remote_url(&format!("http://localhost:{port}"))
        .unwrap();
    while !h.join("stop").exists() {
        std::thread::sleep(Duration::from_millis(200));
    }
}
