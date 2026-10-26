//! Web Push: the RFC 8291 example, a round trip, the VAPID JWT, the
//! subscription file, what the watcher pushes for, and a delivery to a
//! fake push service on loopback.
#![allow(deprecated)]

use super::*;
use crate::config::testing::LOCK;
use crate::pane::Activity;
use ring::signature::UnparsedPublicKey;
use std::io::{Read, Write};

fn b(s: &str) -> Vec<u8> {
    unb64u(&s.split_whitespace().collect::<String>()).unwrap()
}

// RFC 8291 section 5 and appendix A.
const PLAIN: &str = "When I grow up, I want to be a watermelon";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const UA_PRIV: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const UA_PUB: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AS_PRIV: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const AS_PUB: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
const ECDH: &str = "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs";
const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
   mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT
   pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

#[test]
fn push_base64url_round_trips() {
    for n in 0..40u8 {
        let v: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37).wrapping_add(n)).collect();
        let s = b64u(&v);
        assert!(!s.contains(['+', '/', '=']));
        assert_eq!(unb64u(&s).unwrap(), v);
    }
    assert_eq!(unb64u("a+b/").unwrap(), unb64u("a-b_").unwrap());
}

#[test]
fn push_rfc8291_intermediate_values() {
    let (cek, nonce) = derive(&b(ECDH), &b(AUTH), &b(UA_PUB), &b(AS_PUB), &b(SALT));
    assert_eq!(b64u(&cek), "oIhVW04MRdy2XN9CiKLxTg");
    assert_eq!(b64u(&nonce), "4h_95klXJ5E_qnoN");
    let salt: [u8; 16] = b(SALT).try_into().unwrap();
    let body = seal(
        &b(ECDH),
        &b(AUTH),
        &b(UA_PUB),
        &b(AS_PUB),
        &salt,
        PLAIN.as_bytes(),
    )
    .unwrap();
    assert_eq!(body, b(BODY));
    // (The RFC's example says Content-Length 145; its body is 144 bytes.)
    assert_eq!(body.len(), 144);
}

#[test]
fn push_rfc8291_example_with_the_senders_key() {
    // The ephemeral key and the salt come from a fixed "random" source:
    // the whole message must be the RFC's, byte for byte.
    let (k, s) = (b(AS_PRIV), b(SALT));
    let rng = ring::test::rand::FixedSliceSequenceRandom {
        bytes: &[&k, &s],
        current: core::cell::UnsafeCell::new(0),
    };
    let body = encrypt(&b(UA_PUB), &b(AUTH), PLAIN.as_bytes(), &rng).unwrap();
    assert_eq!(b64u(&body), b64u(&b(BODY)));
}

#[test]
fn push_encryption_round_trips_to_the_receiver() {
    let ua_priv = b(UA_PRIV);
    let me = agreement::EphemeralPrivateKey::generate(
        &agreement::ECDH_P256,
        &ring::test::rand::FixedSliceRandom { bytes: &ua_priv },
    )
    .unwrap();
    assert_eq!(me.compute_public_key().unwrap().as_ref(), &b(UA_PUB)[..]);
    let msg = br#"{"title":"GodTerm","body":"api: finished"}"#;
    let body = encrypt(&b(UA_PUB), &b(AUTH), msg, &SystemRandom::new()).unwrap();
    // Header: salt, rs 4096, a 65 byte key.
    assert_eq!(&body[16..20], &4096u32.to_be_bytes());
    assert_eq!(body[20], 65);
    assert_eq!(body.len(), 86 + msg.len() + 1 + 16);
    let as_pub = body[21..86].to_vec();
    let ecdh = agreement::agree_ephemeral(
        me,
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &as_pub),
        |s| s.to_vec(),
    )
    .unwrap();
    assert_eq!(open(&ecdh, &b(AUTH), &b(UA_PUB), &body).unwrap(), msg);
    // A wrong auth secret does not open it.
    assert!(open(&ecdh, &[7u8; 16], &b(UA_PUB), &body).is_none());
    // Two messages never share a salt or a key.
    let again = encrypt(&b(UA_PUB), &b(AUTH), msg, &SystemRandom::new()).unwrap();
    assert_ne!(again[..86], body[..86]);
    // A key that is not on the curve is refused.
    let mut bad = b(UA_PUB);
    bad[40] ^= 1;
    assert!(encrypt(&bad, &b(AUTH), msg, &SystemRandom::new()).is_err());
    // Too big for one record.
    assert!(encrypt(&b(UA_PUB), &b(AUTH), &[b'x'; 4080], &SystemRandom::new()).is_err());
}

fn new_key() -> EcdsaKeyPair {
    let rng = SystemRandom::new();
    let d = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, d.as_ref(), &rng).unwrap()
}

#[test]
fn push_vapid_jwt_shape_and_signature() {
    let k = new_key();
    assert_eq!(k.public_key().as_ref().len(), 65);
    assert_eq!(k.public_key().as_ref()[0], 4);
    let ep = "https://web.push.apple.com/QGuQyavXutnMH-secret?x=1";
    assert_eq!(origin(ep).unwrap(), "https://web.push.apple.com");
    assert_eq!(
        origin("https://Fcm.Googleapis.com:443/x").unwrap(),
        "https://fcm.googleapis.com:443"
    );
    assert!(origin("nope").is_none());
    let h = authorization(&k, ep, 1_800_000_000).unwrap();
    let (t, kpart) = h
        .strip_prefix("vapid t=")
        .unwrap()
        .split_once(", k=")
        .unwrap();
    assert_eq!(unb64u(kpart).unwrap(), k.public_key().as_ref());
    let parts: Vec<&str> = t.split('.').collect();
    assert_eq!(parts.len(), 3);
    let head: Value = serde_json::from_slice(&unb64u(parts[0]).unwrap()).unwrap();
    assert_eq!(head, json!({"typ": "JWT", "alg": "ES256"}));
    let claims: Value = serde_json::from_slice(&unb64u(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["aud"], "https://web.push.apple.com");
    assert_eq!(claims["exp"], 1_800_000_000u64 + JWT_LIFE_SECS);
    assert_eq!(claims["sub"], SUBJECT);
    assert!(claims["sub"].as_str().unwrap().starts_with("https://"));
    let sig = unb64u(parts[2]).unwrap();
    assert_eq!(sig.len(), 64);
    let input = format!("{}.{}", parts[0], parts[1]);
    UnparsedPublicKey::new(
        &ring::signature::ECDSA_P256_SHA256_FIXED,
        k.public_key().as_ref(),
    )
    .verify(input.as_bytes(), &sig)
    .unwrap();
    // No secret in the token: only its three claims.
    assert_eq!(claims.as_object().unwrap().len(), 3);
}

fn home(tag: &str) -> PathBuf {
    let h = std::env::temp_dir().join(format!("godterm-push-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    crate::config::testing::set_home(&h);
    h
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

// Windows has no Unix permission bits; the private mode is checked on Unix.
#[cfg(not(unix))]
fn mode(_p: &Path) -> u32 {
    0o600
}

#[test]
fn push_vapid_key_is_made_once_and_private() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("vapid");
    let p = vapid_path();
    assert!(p.starts_with(&h));
    let a = load_or_create_vapid(&p).unwrap();
    assert_eq!(mode(&p), 0o600);
    let b = load_or_create_vapid(&p).unwrap();
    assert_eq!(a.public_key().as_ref(), b.public_key().as_ref());
    assert_eq!(
        vapid().unwrap().public_key().as_ref(),
        a.public_key().as_ref()
    );
    // A broken file is replaced.
    std::fs::write(&p, b"junk").unwrap();
    let c = load_or_create_vapid(&p).unwrap();
    assert_ne!(c.public_key().as_ref(), a.public_key().as_ref());
    let _ = std::fs::remove_dir_all(&h);
}

fn browser_sub(endpoint: &str) -> Value {
    json!({"endpoint": endpoint, "keys": {"p256dh": UA_PUB, "auth": AUTH}})
}

#[test]
fn push_subscriptions_are_checked_kept_and_forgotten() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("subs");
    let ep = "https://web.push.apple.com/abc";
    assert!(subscribe(&browser_sub("http://web.push.apple.com/abc"), "x", None).is_err());
    let mut bad = browser_sub(ep);
    bad["keys"]["p256dh"] = json!("AAAA");
    assert!(subscribe(&bad, "x", None).is_err());
    let mut bad = browser_sub(ep);
    bad["keys"]["auth"] = json!("AAAA");
    assert!(subscribe(&bad, "x", None).is_err());

    let s = subscribe(&browser_sub(ep), "iPhone Safari standalone\n", None).unwrap();
    assert_eq!(s.device, "iPhone Safari standalone");
    assert_eq!(s.kinds, KINDS.to_vec());
    assert_eq!(s.id(), sub_id(ep));
    assert_eq!(s.id().len(), 16);
    assert_eq!(mode(&subs_path()), 0o600);
    // Kinds chosen; then a plain re-subscribe (a page load) keeps them.
    let s2 = subscribe(&browser_sub(ep), "iPhone", Some(vec!["approval".into()])).unwrap();
    assert_eq!(s2.kinds, vec!["approval"]);
    assert_eq!(s2.created, s.created);
    let s3 = subscribe(&browser_sub(ep), "iPhone", None).unwrap();
    assert_eq!(s3.kinds, vec!["approval"]);
    assert_eq!(subs().len(), 1);
    // On disk, read back fresh.
    let f: Value = serde_json::from_slice(&std::fs::read(subs_path()).unwrap()).unwrap();
    assert_eq!(f["subs"][0]["endpoint"], ep);
    assert_eq!(f["subs"][0]["kinds"], json!(["approval"]));
    assert_eq!(
        clean_kinds(&json!(["error", "bogus", "approval"])).unwrap(),
        vec!["approval", "error"]
    );
    assert!(unsubscribe(&s.id()));
    assert!(!unsubscribe(ep));
    assert!(subs().is_empty());
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn push_frames_answer_on_the_socket() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("frames");
    let out = Outbox::new();
    let take = |out: &Arc<Outbox>| -> Value {
        match out.pop(Duration::from_secs(5)) {
            Some(super::super::tap::Out::Text(s)) => serde_json::from_str(&s).unwrap(),
            _ => panic!("no reply"),
        }
    };
    on_frame(9, &out, &json!({"t": "push", "op": "key", "id": 1}));
    let r = take(&out);
    assert_eq!(
        (r["t"].as_str(), r["id"].as_u64(), r["ok"].as_bool()),
        (Some("push"), Some(1), Some(true))
    );
    assert_eq!(unb64u(r["key"].as_str().unwrap()).unwrap().len(), 65);
    let ep = "https://push.example.net/x";
    on_frame(
        9,
        &out,
        &json!({"op": "subscribe", "id": 2, "sub": browser_sub(ep), "device": "Mac", "kinds": ["finished"]}),
    );
    let r = take(&out);
    assert_eq!(r["sub"], sub_id(ep));
    assert_eq!(r["kinds"], json!(["finished"]));
    on_frame(9, &out, &json!({"op": "status", "id": 3, "endpoint": ep}));
    assert_eq!(take(&out)["subscribed"], true);
    on_frame(9, &out, &json!({"op": "test", "id": 4, "sub": "nope"}));
    let r = take(&out);
    assert_eq!(r["ok"], false);
    on_frame(
        9,
        &out,
        &json!({"op": "unsubscribe", "id": 5, "endpoint": ep}),
    );
    assert_eq!(take(&out)["removed"], true);
    on_frame(9, &out, &json!({"op": "status", "id": 6, "endpoint": ep}));
    assert_eq!(take(&out)["subscribed"], false);
    on_frame(9, &out, &json!({"op": "what", "id": 7}));
    assert_eq!(take(&out)["ok"], false);
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn push_visible_pages_are_skipped_while_fresh() {
    let now = Instant::now();
    set_visible(1001, "aaaa", true);
    set_visible(1002, "bbbb", false);
    set_visible(1003, "cccc", true);
    let l = looking(&[1001, 1002], now);
    assert!(l.contains("aaaa"));
    assert!(!l.contains("bbbb"));
    // 1003 is not connected: dropped.
    assert!(!l.contains("cccc"));
    assert!(!looking(&[1001, 1002, 1003], now).contains("cccc"));
    // A page that stopped saying so (iOS froze it) no longer counts.
    assert!(!looking(&[1001], now + VISIBLE_FOR + Duration::from_secs(1)).contains("aaaa"));
}

fn seen(a: Activity) -> Seen {
    Seen {
        activity: a,
        since: Instant::now(),
        bg_wait: false,
        finished: false,
        crashed: false,
    }
}

#[test]
fn push_watcher_notices_approvals_turns_and_errors() {
    let t0 = Instant::now();
    let mut w = Watch::default();
    let tab = |a: Activity| vec![(12u64, "api-server".to_string(), seen(a))];
    // The first look only records, even a tab that already waits.
    assert!(w.observe(&tab(Activity::Permission), false, t0).is_empty());
    assert!(w.observe(&tab(Activity::Working), false, t0).is_empty());
    // Asked for approval.
    let n = w.observe(
        &tab(Activity::Permission),
        false,
        t0 + Duration::from_secs(1),
    );
    assert_eq!(
        n,
        vec![Note {
            kind: "approval",
            tab: 12,
            name: "api-server".into()
        }]
    );
    // Again within 30 s: coalesced.
    w.observe(&tab(Activity::Working), false, t0 + Duration::from_secs(2));
    assert!(w
        .observe(
            &tab(Activity::Permission),
            false,
            t0 + Duration::from_secs(3)
        )
        .is_empty());
    // A real turn (working for 1.5 s or more) ends.
    w.observe(&tab(Activity::Working), false, t0 + Duration::from_secs(4));
    let n = w.observe(&tab(Activity::Ready), false, t0 + Duration::from_secs(8));
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].kind, "finished");
    // A flicker is not a turn.
    w.observe(&tab(Activity::Working), false, t0 + Duration::from_secs(60));
    assert!(w
        .observe(
            &tab(Activity::Ready),
            false,
            t0 + Duration::from_millis(60_500)
        )
        .is_empty());
    // Background work holds "finished" until it is done.
    w.observe(
        &tab(Activity::Working),
        false,
        t0 + Duration::from_secs(100),
    );
    let mut bg = seen(Activity::Ready);
    bg.bg_wait = true;
    let row = |s: Seen| vec![(12u64, "api-server".to_string(), s)];
    assert!(w
        .observe(&row(bg), false, t0 + Duration::from_secs(110))
        .is_empty());
    let n = w.observe(&tab(Activity::Ready), false, t0 + Duration::from_secs(120));
    assert_eq!(n[0].kind, "finished");
    // A crash.
    let mut c = seen(Activity::Exited);
    c.crashed = true;
    let n = w.observe(&row(c), false, t0 + Duration::from_secs(130));
    assert_eq!(n[0].kind, "error");
    // The assistant asks.
    let n = w.observe(&row(c), true, t0 + Duration::from_secs(131));
    assert_eq!(
        n,
        vec![Note {
            kind: "approval",
            tab: 0,
            name: String::new()
        }]
    );
    assert!(w
        .observe(&row(c), true, t0 + Duration::from_secs(200))
        .is_empty());
}

#[test]
fn push_payload_holds_only_the_name_and_kind() {
    let n = Note {
        kind: "approval",
        tab: 12,
        name: "api-server".into(),
    };
    let p = n.payload();
    assert_eq!(p["title"], "GodTerm");
    assert_eq!(p["body"], "api-server: waits for approval");
    assert_eq!(p["tag"], "approval-12");
    assert_eq!(p["url"], "./#tab=12&approvals");
    assert_eq!(n.urgency(), "high");
    let a = Note {
        kind: "approval",
        tab: 0,
        name: String::new(),
    };
    assert_eq!(a.payload()["body"], "Assistant: asks for a yes");
    assert_eq!(a.payload()["url"], "./#approvals");
    let f = Note {
        kind: "finished",
        tab: 3,
        name: "docs".into(),
    };
    assert_eq!(f.payload()["body"], "docs: finished");
    assert_eq!(f.payload()["url"], "./#tab=3");
    assert_eq!(f.urgency(), "normal");
    let e = Note {
        kind: "error",
        tab: 3,
        name: "docs".into(),
    };
    assert_eq!(e.payload()["body"], "docs: ended with an error");
}

/// A one request push service on loopback: the request's head and body,
/// answered with `code`.
fn fake_service(code: u16) -> (String, std::thread::JoinHandle<(String, Vec<u8>)>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut buf = vec![];
        let mut b = [0u8; 4096];
        let head_end = loop {
            let n = s.read(&mut b).unwrap();
            assert!(n > 0, "closed early");
            buf.extend_from_slice(&b[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let len: usize = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())?
            })
            .expect("a content-length");
        while buf.len() < head_end + len {
            let n = s.read(&mut b).unwrap();
            buf.extend_from_slice(&b[..n]);
        }
        let reason = if code == 201 { "Created" } else { "Gone" };
        write!(
            s,
            "HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        (head, buf[head_end..head_end + len].to_vec())
    });
    (format!("http://127.0.0.1:{port}/push/device-1"), t)
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case(name).then_some(v.trim())
    })
}

#[test]
fn push_delivery_end_to_end_decrypts_at_the_receiver() {
    // The receiver: the RFC's user agent key (fixed, so it can be used
    // after the message arrives).
    let ua_priv = b(UA_PRIV);
    let me = agreement::EphemeralPrivateKey::generate(
        &agreement::ECDH_P256,
        &ring::test::rand::FixedSliceRandom { bytes: &ua_priv },
    )
    .unwrap();
    let (ep, srv) = fake_service(201);
    let s = Sub {
        endpoint: ep,
        p256dh: UA_PUB.into(),
        auth: AUTH.into(),
        device: "test".into(),
        created: String::new(),
        kinds: KINDS.iter().map(|k| k.to_string()).collect(),
    };
    let k = new_key();
    let n = Note {
        kind: "approval",
        tab: 12,
        name: "api-server".into(),
    };
    assert_eq!(deliver(&k, &s, &n.payload(), "high").unwrap(), 201);
    let (head, body) = srv.join().unwrap();
    assert!(head.starts_with("POST /push/device-1 "));
    assert_eq!(header(&head, "ttl"), Some("3600"));
    assert_eq!(header(&head, "urgency"), Some("high"));
    assert_eq!(header(&head, "content-encoding"), Some("aes128gcm"));
    let authz = header(&head, "authorization").unwrap();
    assert!(authz.starts_with("vapid t="));
    assert!(authz.ends_with(&format!("k={}", b64u(k.public_key().as_ref()))));
    let claims: Value =
        serde_json::from_slice(&unb64u(authz[8..].split('.').nth(1).unwrap()).unwrap()).unwrap();
    assert_eq!(claims["aud"], s.endpoint.split("/push").next().unwrap());
    let ecdh = agreement::agree_ephemeral(
        me,
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &body[21..86]),
        |x| x.to_vec(),
    )
    .unwrap();
    let plain = open(&ecdh, &b(AUTH), &b(UA_PUB), &body).unwrap();
    let got: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(got, n.payload());
}

#[test]
fn push_a_gone_subscription_is_forgotten() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("gone");
    let (ep, srv) = fake_service(410);
    with_store(|subs| {
        subs.push(Sub {
            endpoint: ep.clone(),
            p256dh: UA_PUB.into(),
            auth: AUTH.into(),
            device: "old phone".into(),
            created: String::new(),
            kinds: vec!["finished".into()],
        });
        ((), true)
    });
    assert_eq!(subs().len(), 1);
    let s = subs()[0].clone();
    let r = deliver_logged(&s, &json!({"kind": "finished"}), "normal");
    assert!(r.unwrap_err().contains("410"));
    srv.join().unwrap();
    assert!(subs().is_empty());
    let _ = std::fs::remove_dir_all(&h);
}
