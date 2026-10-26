//! Web Push (docs/WEB_APP.md, "Push notifications"): a phone with the app
//! on its Home Screen gets a notification when a tab waits for approval,
//! finishes a turn or ends with an error, also while the page is closed.
//!
//! - VAPID (RFC 8292): one P-256 key pair, made once and kept in
//!   `<app home>/web-push-vapid.pk8` (0600); every push carries an ES256
//!   JWT for the push service's origin.
//! - Encryption (RFC 8291): aes128gcm, one record of 4096, an ephemeral
//!   ECDH key and a random salt per message.
//! - Subscriptions: `<app home>/web-push-subs.json` (0600), one per device.
//!   A push service answering 404 or 410 has dropped it: so do we.
//! - The UI thread only notices what happened (`Watch`, cheap, no I/O);
//!   a thread of its own reads the files, encrypts and posts.
//!
//! Only the tab's name and the kind ever leave this Mac.

use super::tap::Outbox;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use ring::{aead, agreement, hmac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What a device can opt into.
pub const KINDS: [&str; 3] = ["approval", "finished", "error"];
/// At most one push per tab per kind this often.
pub const COALESCE: Duration = Duration::from_secs(30);
/// How long a push service keeps a message for a device that is offline.
pub const TTL_SECS: u32 = 3600;
/// The record size in the aes128gcm header.
pub const RECORD_SIZE: u32 = 4096;
/// A page that said "visible" counts as looking for this long (it says so
/// again every 15 s while it is).
pub const VISIBLE_FOR: Duration = Duration::from_secs(40);
/// The JWT's lifetime (RFC 8292 allows 24 h; kept short).
pub const JWT_LIFE_SECS: u64 = 3600;
const _: () = assert!(JWT_LIFE_SECS <= 12 * 3600);
/// The JWT's `sub`: the project's page, neutral and the same for everyone.
pub const SUBJECT: &str = "https://godterm.com";
/// Devices kept (the oldest goes first).
const MAX_SUBS: usize = 20;
/// Payloads stay well inside one record.
const MAX_PAYLOAD: usize = 3000;

// ---------- base64url ----------

/// base64url without padding (what Web Push uses everywhere).
pub fn b64u(b: &[u8]) -> String {
    crate::assistant_images::base64(b)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

/// base64url or standard base64, padding optional.
pub fn unb64u(s: &str) -> Option<Vec<u8>> {
    crate::assistant_images::unbase64(&s.replace('-', "+").replace('_', "/"))
}

// ---------- VAPID ----------

pub fn vapid_path() -> PathBuf {
    crate::config::app_home().join("web-push-vapid.pk8")
}

pub fn subs_path() -> PathBuf {
    crate::config::app_home().join("web-push-subs.json")
}

/// The VAPID key at `path`, made (PKCS#8, 0600) when there is none or it
/// does not parse.
pub fn load_or_create_vapid(path: &Path) -> anyhow::Result<EcdsaKeyPair> {
    let rng = SystemRandom::new();
    if let Ok(b) = std::fs::read(path) {
        if let Ok(k) = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &b, &rng) {
            return Ok(k);
        }
        crate::log::error("web push: the VAPID key did not parse; making a new one");
    }
    let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(|_| anyhow::anyhow!("making the VAPID key failed"))?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::config::write_private(path, doc.as_ref())?;
    crate::log::info("web push: made a VAPID key");
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, doc.as_ref(), &rng)
        .map_err(|e| anyhow::anyhow!("the new VAPID key: {e}"))
}

/// This home's VAPID key (kept in memory once read).
pub fn vapid() -> anyhow::Result<Arc<EcdsaKeyPair>> {
    static K: Mutex<Option<(PathBuf, Arc<EcdsaKeyPair>)>> = Mutex::new(None);
    let p = vapid_path();
    let mut g = K.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((q, k)) = g.as_ref() {
        if *q == p {
            return Ok(Arc::clone(k));
        }
    }
    let k = Arc::new(load_or_create_vapid(&p)?);
    *g = Some((p, Arc::clone(&k)));
    Ok(k)
}

/// The scheme, host and port of a URL ("https://web.push.apple.com").
pub fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() || host.contains('@') {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

/// An ES256 JWT for `aud`, valid until `exp` (unix seconds).
pub fn jwt(key: &EcdsaKeyPair, aud: &str, exp: u64, sub: &str) -> anyhow::Result<String> {
    let head = b64u(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = b64u(
        json!({"aud": aud, "exp": exp, "sub": sub})
            .to_string()
            .as_bytes(),
    );
    let input = format!("{head}.{claims}");
    let sig = key
        .sign(&SystemRandom::new(), input.as_bytes())
        .map_err(|_| anyhow::anyhow!("signing the JWT failed"))?;
    Ok(format!("{input}.{}", b64u(sig.as_ref())))
}

/// The Authorization header for a push to `endpoint` at `now`.
pub fn authorization(key: &EcdsaKeyPair, endpoint: &str, now: u64) -> anyhow::Result<String> {
    let aud = origin(endpoint).ok_or_else(|| anyhow::anyhow!("a bad push endpoint"))?;
    let t = jwt(key, &aud, now + JWT_LIFE_SECS, SUBJECT)?;
    Ok(format!(
        "vapid t={t}, k={}",
        b64u(key.public_key().as_ref())
    ))
}

// ---------- RFC 8291 ----------

fn hmac256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut c = hmac::Context::with_key(&k);
    for p in parts {
        c.update(p);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(c.sign().as_ref());
    out
}

/// The content key and nonce from the ECDH secret (RFC 8291 section 3.4,
/// RFC 8188 section 2.2; each HKDF expand is one block).
pub fn derive(
    ecdh: &[u8],
    auth: &[u8],
    ua_pub: &[u8],
    as_pub: &[u8],
    salt: &[u8],
) -> ([u8; 16], [u8; 12]) {
    let prk_key = hmac256(auth, &[ecdh]);
    let ikm = hmac256(&prk_key, &[b"WebPush: info\0", ua_pub, as_pub, &[1]]);
    let prk = hmac256(salt, &[&ikm]);
    let cek = hmac256(&prk, &[b"Content-Encoding: aes128gcm\0", &[1]]);
    let nonce = hmac256(&prk, &[b"Content-Encoding: nonce\0", &[1]]);
    let mut k = [0u8; 16];
    k.copy_from_slice(&cek[..16]);
    let mut n = [0u8; 12];
    n.copy_from_slice(&nonce[..12]);
    (k, n)
}

/// The message body: the aes128gcm header (salt, record size, the
/// sender's public key) and one record (plaintext, 0x02, tag).
pub fn seal(
    ecdh: &[u8],
    auth: &[u8],
    ua_pub: &[u8],
    as_pub: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> anyhow::Result<Vec<u8>> {
    if plaintext.len() + 17 > RECORD_SIZE as usize {
        anyhow::bail!("the payload is too big for one record");
    }
    let (cek, nonce) = derive(ecdh, auth, ua_pub, as_pub, salt);
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, &cek).map_err(|_| anyhow::anyhow!("aes key"))?,
    );
    let mut rec = Vec::with_capacity(plaintext.len() + 17);
    rec.extend_from_slice(plaintext);
    rec.push(2);
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut rec,
    )
    .map_err(|_| anyhow::anyhow!("encrypting failed"))?;
    let mut out = Vec::with_capacity(86 + rec.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(as_pub.len() as u8);
    out.extend_from_slice(as_pub);
    out.extend_from_slice(&rec);
    Ok(out)
}

/// Encrypt `plaintext` for a subscription's keys, with an ephemeral key
/// and a salt from `rng` (the system's; tests pass fixed ones).
pub fn encrypt(
    ua_pub: &[u8],
    auth: &[u8],
    plaintext: &[u8],
    rng: &dyn SecureRandom,
) -> anyhow::Result<Vec<u8>> {
    let me = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, rng)
        .map_err(|_| anyhow::anyhow!("making the ephemeral key failed"))?;
    let as_pub = me
        .compute_public_key()
        .map_err(|_| anyhow::anyhow!("the ephemeral public key"))?;
    let as_pub = as_pub.as_ref().to_vec();
    let mut salt = [0u8; 16];
    rng.fill(&mut salt)
        .map_err(|_| anyhow::anyhow!("no random salt"))?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_pub);
    let ecdh = agreement::agree_ephemeral(me, &peer, |s| s.to_vec())
        .map_err(|_| anyhow::anyhow!("the subscription's key is not a P-256 point"))?;
    seal(&ecdh, auth, ua_pub, &as_pub, &salt, plaintext)
}

/// Decrypt a body given the ECDH secret (the receiver's side; tests).
#[cfg(test)]
pub fn open(ecdh: &[u8], auth: &[u8], ua_pub: &[u8], body: &[u8]) -> Option<Vec<u8>> {
    let salt = body.get(..16)?;
    let idlen = *body.get(20)? as usize;
    let as_pub = body.get(21..21 + idlen)?;
    let (cek, nonce) = derive(ecdh, auth, ua_pub, as_pub, salt);
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).ok()?);
    let mut rec = body.get(21 + idlen..)?.to_vec();
    let plain = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut rec,
        )
        .ok()?;
    let end = plain.iter().rposition(|b| *b != 0)?;
    (plain[end] == 2).then(|| plain[..end].to_vec())
}

// ---------- subscriptions ----------

/// One device's subscription.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sub {
    pub endpoint: String,
    /// The browser's P-256 public key (65 bytes, base64url).
    pub p256dh: String,
    /// Its auth secret (16 bytes, base64url).
    pub auth: String,
    /// "iPhone Safari standalone 402x874@3".
    #[serde(default)]
    pub device: String,
    /// When it was first stored (RFC 3339).
    #[serde(default)]
    pub created: String,
    /// What it opted into (of KINDS).
    #[serde(default)]
    pub kinds: Vec<String>,
}

impl Sub {
    /// A short id for the page (the endpoint itself is a secret).
    pub fn id(&self) -> String {
        sub_id(&self.endpoint)
    }
}

pub fn sub_id(endpoint: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(endpoint.as_bytes());
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    subs: Vec<Sub>,
}

/// The subscriptions in memory, for the file they came from.
static STORE: Mutex<Option<(PathBuf, Vec<Sub>)>> = Mutex::new(None);

fn with_store<R>(f: impl FnOnce(&mut Vec<Sub>) -> (R, bool)) -> R {
    let p = subs_path();
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|(q, _)| *q != p) {
        let subs = std::fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice::<File>(&b).ok())
            .map(|f| f.subs)
            .unwrap_or_default();
        *g = Some((p.clone(), subs));
    }
    let Some((_, subs)) = g.as_mut() else {
        unreachable!()
    };
    let (r, dirty) = f(subs);
    if dirty {
        let body = serde_json::to_vec_pretty(&File { subs: subs.clone() }).unwrap_or_default();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("tmp");
        let ok = crate::config::write_private(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &p));
        if let Err(e) = ok {
            crate::log::error(&format!("web push: saving the subscriptions: {e}"));
        }
    }
    r
}

pub fn subs() -> Vec<Sub> {
    with_store(|s| (s.clone(), false))
}

fn clean_kinds(v: &Value) -> Option<Vec<String>> {
    let a = v.as_array()?;
    Some(
        KINDS
            .iter()
            .filter(|k| a.iter().any(|x| x.as_str() == Some(k)))
            .map(|k| k.to_string())
            .collect(),
    )
}

/// Check and store a subscription (the browser's PushSubscription JSON):
/// a known endpoint keeps its created time and kinds unless new kinds
/// are given.
pub fn subscribe(sub: &Value, device: &str, kinds: Option<Vec<String>>) -> Result<Sub, String> {
    let endpoint = sub["endpoint"].as_str().unwrap_or("").trim().to_string();
    if !endpoint.starts_with("https://") || endpoint.len() > 2048 || origin(&endpoint).is_none() {
        return Err("the push endpoint must be an https URL".into());
    }
    let p256dh = sub["keys"]["p256dh"].as_str().unwrap_or("");
    let auth = sub["keys"]["auth"].as_str().unwrap_or("");
    let ua = unb64u(p256dh).unwrap_or_default();
    if ua.len() != 65 || ua[0] != 4 {
        return Err("p256dh must be a P-256 public key (65 bytes)".into());
    }
    if unb64u(auth).is_none_or(|a| a.len() != 16) {
        return Err("auth must be 16 bytes".into());
    }
    let given = kinds.is_some();
    let device: String = device
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let new = Sub {
        endpoint,
        p256dh: b64u(&ua),
        auth: b64u(&unb64u(auth).unwrap_or_default()),
        device,
        created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        kinds: kinds.unwrap_or_else(|| KINDS.iter().map(|k| k.to_string()).collect()),
    };
    Ok(with_store(|subs| {
        let mut out = new.clone();
        if let Some(old) = subs.iter_mut().find(|s| s.endpoint == new.endpoint) {
            out.created = old.created.clone();
            if !given {
                out.kinds = old.kinds.clone();
            }
            let dirty = *old != out;
            *old = out.clone();
            return (out, dirty);
        }
        subs.push(out.clone());
        while subs.len() > MAX_SUBS {
            subs.remove(0);
        }
        crate::log::info(&format!("web push: a device subscribed ({})", out.device));
        (out, true)
    }))
}

/// Forget a subscription by its endpoint or id. Whether one went.
pub fn unsubscribe(key: &str) -> bool {
    with_store(|subs| {
        let n = subs.len();
        subs.retain(|s| s.endpoint != key && s.id() != key);
        let gone = subs.len() != n;
        if gone {
            crate::log::info("web push: a device unsubscribed");
        }
        (gone, gone)
    })
}

// ---------- visibility ----------

/// What each connected page last said: client -> (its subscription id,
/// visible, when).
type Looks = HashMap<u64, (String, bool, Instant)>;
static VISIBLE: Mutex<Option<Looks>> = Mutex::new(None);

pub fn set_visible(client: u64, sub: &str, on: bool) {
    let mut g = VISIBLE.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .insert(client, (sub.to_string(), on, Instant::now()));
}

/// The subscriptions a connected page is looking at now (those devices
/// are not sent a push). Entries of clients gone are dropped.
pub fn looking(connected: &[u64], now: Instant) -> HashSet<String> {
    let mut g = VISIBLE.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    m.retain(|c, _| connected.contains(c));
    m.values()
        .filter(|(s, on, at)| *on && !s.is_empty() && now.duration_since(*at) < VISIBLE_FOR)
        .map(|(s, _, _)| s.clone())
        .collect()
}

// ---------- what happened ----------

/// A tab as the watcher last saw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    pub activity: crate::pane::Activity,
    /// When `activity` began (as the watcher saw it).
    pub since: Instant,
    pub bg_wait: bool,
    pub finished: bool,
    pub crashed: bool,
}

/// The kinds a tab's change from `prev` to `now` is worth a push for.
pub fn changes(prev: &Seen, now: &Seen, at: Instant) -> Vec<&'static str> {
    use crate::pane::Activity::*;
    let mut v = vec![];
    if now.activity == Permission && prev.activity != Permission {
        v.push("approval");
    }
    let turn_ended = prev.activity == Working
        && now.activity == Ready
        && at.duration_since(prev.since) >= crate::app::FINISHED_MIN_WORK
        && !now.bg_wait;
    let bg_done = prev.bg_wait && !now.bg_wait && now.activity == Ready;
    if turn_ended || bg_done || (now.finished && !prev.finished) {
        v.push("finished");
    }
    if now.crashed && !prev.crashed {
        v.push("error");
    }
    v
}

/// A push to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub kind: &'static str,
    /// The tab's uid (0: the assistant).
    pub tab: u64,
    pub name: String,
}

impl Note {
    pub fn payload(&self) -> Value {
        let who = if self.tab == 0 {
            "Assistant".to_string()
        } else {
            self.name.chars().take(60).collect()
        };
        let what = match self.kind {
            "approval" if self.tab == 0 => "asks for a yes",
            "approval" => "waits for approval",
            "finished" => "finished",
            "error" => "ended with an error",
            _ => "test notification",
        };
        let url = match (self.kind, self.tab) {
            ("approval", 0) => "./#approvals".to_string(),
            ("approval", t) => format!("./#tab={t}&approvals"),
            ("test", _) => "./".to_string(),
            (_, t) => format!("./#tab={t}"),
        };
        let tag = if self.tab == 0 {
            format!("{}-assistant", self.kind)
        } else {
            format!("{}-{}", self.kind, self.tab)
        };
        json!({
            "title": "GodTerm", "body": format!("{who}: {what}"), "tag": tag,
            "url": url, "kind": self.kind, "tab": self.tab,
        })
    }

    fn urgency(&self) -> &'static str {
        if self.kind == "approval" {
            "high"
        } else {
            "normal"
        }
    }
}

/// The UI thread's watcher: what each tab was, and when each push last
/// went (for coalescing).
#[derive(Default)]
pub struct Watch {
    seen: Option<HashMap<u64, Seen>>,
    asking: bool,
    last: HashMap<(u64, &'static str), Instant>,
}

impl Watch {
    /// Compare with the last look. The first look only records (no push
    /// for what already waited when GodTerm started).
    pub fn observe(
        &mut self,
        tabs: &[(u64, String, Seen)],
        asking: bool,
        at: Instant,
    ) -> Vec<Note> {
        let mut out = vec![];
        let first = self.seen.is_none();
        let prev = self.seen.take().unwrap_or_default();
        let mut next = HashMap::new();
        for (uid, name, now) in tabs {
            let mut now = *now;
            match prev.get(uid) {
                Some(p) => {
                    if p.activity == now.activity {
                        now.since = p.since;
                    } else {
                        now.since = at;
                    }
                    for k in changes(p, &now, at) {
                        out.push(Note {
                            kind: k,
                            tab: *uid,
                            name: name.clone(),
                        });
                    }
                }
                None => now.since = at,
            }
            next.insert(*uid, now);
        }
        if asking && !self.asking && !first {
            out.push(Note {
                kind: "approval",
                tab: 0,
                name: String::new(),
            });
        }
        self.asking = asking;
        self.seen = Some(next);
        if first {
            return vec![];
        }
        self.last.retain(|_, t| at.duration_since(*t) < COALESCE);
        out.retain(|n| {
            let key = (n.tab, n.kind);
            if self.last.contains_key(&key) {
                return false;
            }
            self.last.insert(key, at);
            true
        });
        out
    }
}

impl crate::app::App {
    /// From web_pump: notice approvals, finished turns and errors, and
    /// hand any push to the sender thread.
    pub fn web_push_tick(&mut self) {
        let mut tabs = vec![];
        let at = Instant::now();
        for (s, t) in self.all_tabs() {
            let tab = &self.panes[s].tabs[t];
            tabs.push((
                tab.uid,
                tab.name(),
                Seen {
                    activity: tab.activity,
                    since: at,
                    bg_wait: tab.bg_wait,
                    finished: tab.finished,
                    crashed: tab.activity == crate::pane::Activity::Exited && tab.crashed(),
                },
            ));
        }
        let asking = self.pending_confirms.iter().any(|p| p.answerable())
            || self.assistant.selfswitch.asked.is_some();
        let Some(w) = self.web.as_mut() else {
            return;
        };
        let notes = w.push.observe(&tabs, asking, at);
        if notes.is_empty() {
            return;
        }
        let connected: Vec<u64> = w.hub.clients().iter().map(|c| c.id).collect();
        let skip = looking(&connected, at);
        for n in notes {
            submit(Job::Note(n, skip.clone()));
        }
    }
}

// ---------- sending ----------

pub enum Job {
    /// To every device that opted into its kind, but the ones looking.
    Note(Note, HashSet<String>),
    /// A test to one device, answered on its socket.
    Test {
        sub: String,
        out: Arc<Outbox>,
        id: Value,
    },
}

fn sender() -> &'static Mutex<Sender<Job>> {
    static TX: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("web-push".into())
            .spawn(move || send_loop(rx));
        Mutex::new(tx)
    })
}

pub fn submit(j: Job) {
    let _ = sender().lock().unwrap_or_else(|e| e.into_inner()).send(j);
}

fn send_loop(rx: Receiver<Job>) {
    for j in rx {
        match j {
            Job::Note(n, skip) => {
                let all = subs();
                if all.is_empty() {
                    continue;
                }
                let payload = n.payload();
                for s in all.iter().filter(|s| s.kinds.iter().any(|k| k == n.kind)) {
                    if skip.contains(&s.id()) {
                        crate::log::info(&format!(
                            "web push: {} not sent to {} (the app is open there)",
                            n.kind, s.device
                        ));
                        continue;
                    }
                    let _ = deliver_logged(s, &payload, n.urgency());
                }
            }
            Job::Test { sub, out, id } => {
                let found = subs().into_iter().find(|s| s.id() == sub);
                let reply = match found {
                    None => {
                        json!({"t": "push", "id": id, "ok": false, "error": "this device is not subscribed"})
                    }
                    Some(s) => {
                        let n = Note {
                            kind: "test",
                            tab: 0,
                            name: String::new(),
                        };
                        let mut p = n.payload();
                        p["body"] = json!("Notifications work on this device");
                        match deliver_logged(&s, &p, "normal") {
                            Ok(code) => json!({"t": "push", "id": id, "ok": true, "status": code}),
                            Err(e) => json!({"t": "push", "id": id, "ok": false, "error": e}),
                        }
                    }
                };
                out.text(reply.to_string());
            }
        }
    }
}

/// Send to one device, logging what happened; a 404 or 410 forgets it.
fn deliver_logged(s: &Sub, payload: &Value, urgency: &str) -> Result<u16, String> {
    let host = origin(&s.endpoint).unwrap_or_default();
    let key = vapid().map_err(|e| format!("{e:#}"))?;
    match deliver(&key, s, payload, urgency) {
        Ok(code) if (200..300).contains(&code) => {
            crate::log::info(&format!(
                "web push: sent {} to {} ({code})",
                payload["kind"].as_str().unwrap_or(""),
                s.device
            ));
            Ok(code)
        }
        Ok(code @ (404 | 410)) => {
            crate::log::info(&format!(
                "web push: {host} dropped the subscription of {} ({code}); forgotten",
                s.device
            ));
            unsubscribe(&s.endpoint);
            Err(format!(
                "the push service no longer knows this device ({code}); turn notifications on again"
            ))
        }
        Ok(code) => {
            crate::log::error(&format!(
                "web push: {host} answered {code} for {}",
                s.device
            ));
            Err(format!("the push service answered {code}"))
        }
        Err(e) => {
            crate::log::error(&format!("web push: {host}: {e:#}"));
            Err(format!("{e:#}"))
        }
    }
}

/// Encrypt and POST one push; the status code.
pub fn deliver(key: &EcdsaKeyPair, s: &Sub, payload: &Value, urgency: &str) -> anyhow::Result<u16> {
    let body = payload.to_string();
    if body.len() > MAX_PAYLOAD {
        anyhow::bail!("the payload is too big");
    }
    let ua = unb64u(&s.p256dh).ok_or_else(|| anyhow::anyhow!("bad p256dh"))?;
    let auth = unb64u(&s.auth).ok_or_else(|| anyhow::anyhow!("bad auth"))?;
    let msg = encrypt(&ua, &auth, body.as_bytes(), &SystemRandom::new())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let authz = authorization(key, &s.endpoint, now)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .max_redirects(0)
        .user_agent(format!("godterm/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let resp = agent
        .post(&s.endpoint)
        .header("TTL", TTL_SECS.to_string())
        .header("Urgency", urgency)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("Authorization", authz)
        .send(&msg[..])
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(resp.status().as_u16())
}

// ---------- the page's frames ----------

/// A `push` frame from a page (ws.rs): key, status, subscribe,
/// unsubscribe, test, visible.
pub fn on_frame(client: u64, out: &Arc<Outbox>, v: &Value) {
    let id = v["id"].clone();
    let reply = |r: Value| {
        let mut r = r;
        r["t"] = json!("push");
        r["id"] = id.clone();
        out.text(r.to_string());
    };
    match v["op"].as_str().unwrap_or("") {
        "key" => match vapid() {
            Ok(k) => reply(json!({"ok": true, "key": b64u(k.public_key().as_ref())})),
            Err(e) => reply(json!({"ok": false, "error": format!("{e:#}")})),
        },
        "status" => {
            let ep = v["endpoint"].as_str().unwrap_or("");
            let found = subs()
                .into_iter()
                .find(|s| !ep.is_empty() && s.endpoint == ep);
            reply(match found {
                Some(s) => json!({"ok": true, "subscribed": true, "sub": s.id(), "kinds": s.kinds}),
                None => json!({"ok": true, "subscribed": false}),
            });
        }
        "subscribe" => {
            let kinds = clean_kinds(&v["kinds"]);
            let r = subscribe(&v["sub"], v["device"].as_str().unwrap_or(""), kinds);
            reply(match r {
                Ok(s) => json!({"ok": true, "subscribed": true, "sub": s.id(), "kinds": s.kinds}),
                Err(e) => json!({"ok": false, "error": e}),
            });
        }
        "unsubscribe" => {
            let key = v["endpoint"].as_str().or(v["sub"].as_str()).unwrap_or("");
            let gone = !key.is_empty() && unsubscribe(key);
            reply(json!({"ok": true, "removed": gone}));
        }
        "test" => submit(Job::Test {
            sub: v["sub"].as_str().unwrap_or("").to_string(),
            out: Arc::clone(out),
            id: id.clone(),
        }),
        "visible" => set_visible(
            client,
            v["sub"].as_str().unwrap_or(""),
            v["on"].as_bool().unwrap_or(false),
        ),
        other => reply(json!({"ok": false, "error": format!("unknown push op {other:?}")})),
    }
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
